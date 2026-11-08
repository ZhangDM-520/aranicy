//! The two-axis class boundary (candidate 7b, `docs/reviews/2026-11-08-wave2-design.md` §7.4/§7.5).
//!
//! `score = s0²` is dominated by the *interaction rate*
//! (`ilog2(wait_ft·wake_ft)`), so a compiler that never sleeps and an idle
//! audio daemon that rarely wakes look alike (`docs/adr/0001-classifier-boundary.md`).
//! This module separates the two axes again, absolutely:
//!
//! ```text
//! lean = log2(max(burst, BURST_FLOOR_NS) / stage_min_ns)
//!      − log2(max(hop,   hop_min_hz)    / hop_min_hz)
//! ```
//!
//! * `burst` — mean nanoseconds per schedule (long bursts = throughput work);
//! * `hop` — sleeps per second (frequent handoffs = latency work).
//!
//! Both axes are absolute, so no population statistic enters the decision and
//! no host calibration is needed: `64 µs` is sched_ext cake's "long burst"
//! seat threshold and `100 Hz` a hop floor below which the rate axis carries
//! no signal. `lean` is weight-free for the same reason `sample_score` is —
//! every input is independent of the nice we apply.
//!
//! Deliberately a **free function of numbers**: offline replay over recorded
//! rows and the live shadow classifier call the same code, so there is exactly
//! one definition of the boundary and no threshold arithmetic inside the state
//! machine.

use aranicy_wire::Sample;

/// cake's `SEAT_BURST_MIN_NS`: above this a burst is "long" (bulk).
pub const STAGE_MIN_NS: u64 = 64_000;

/// Hop floor: below this many sleeps/second the rate axis is 0.
pub const HOP_MIN_HZ: u64 = 100;

/// cake's handoff floor: never divide by ~0 (a 1.4 µs burst is not evidence
/// of anything, but `log2(0)` would turn it into −∞).
pub const BURST_FLOOR_NS: u64 = 1_464;

/// The class boundary in absolute units. `deadband_pct` is the same
/// multiplicative band the score classifier uses, applied in log space:
/// `lean` inside `±log2(1 + deadband/100)` is `None` (no verdict).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Boundary {
    pub stage_min_ns: u64,
    pub hop_min_hz: u64,
    pub deadband_pct: u64,
}

/// The two classes the lean axis can express. Mirrors the actuated
/// [`crate::bias_state::BiasClass`] without importing it: this module knows
/// nothing about the state machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lean {
    Bulk,
    Responsive,
}

impl Lean {
    /// The class a [`lean`] value implies (positive = bulk). Kept here so the
    /// sign convention has one definition — no caller maps numbers to classes
    /// itself.
    fn of(value: f64) -> Lean {
        if value > 0.0 {
            Lean::Bulk
        } else {
            Lean::Responsive
        }
    }
}

impl Boundary {
    /// Half-width of the deadband in `lean` units: `log2(1 + pct/100)`.
    fn deadband(&self) -> f64 {
        (1.0 + self.deadband_pct as f64 / 100.0).log2()
    }
}

/// Weight-free, absolute lean of one window: positive = bulk (long bursts),
/// negative = responsive (short, frequent bursts). `None` = no evidence
/// (the task never ran, so the burst axis is 0) or inside the deadband.
pub fn lean(burst_ns: u64, hop_hz: u64, b: &Boundary) -> Option<f64> {
    if burst_ns == 0 {
        return None;
    }
    let stage = b.stage_min_ns.max(1) as f64;
    let hop_floor = b.hop_min_hz.max(1) as f64;
    let burst = (burst_ns.max(BURST_FLOOR_NS) as f64) / stage;
    let rate = (hop_hz.max(b.hop_min_hz) as f64) / hop_floor;
    let value = burst.log2() - rate.log2();
    (value.abs() > b.deadband()).then_some(value)
}

/// [`lean`] collapsed to the class it implies. `None` = no evidence or the
/// deadband (the caller holds its current class, exactly like the score
/// classifier's deadband).
pub fn classify(burst_ns: u64, hop_hz: u64, b: &Boundary) -> Option<Lean> {
    lean(burst_ns, hop_hz, b).map(Lean::of)
}

/// Convenience for replay and tests over one raw row: the wire's
/// `avg_runtime_wall` is the burst axis and `wait_freq` the hop axis.
///
/// Unused by the live path on purpose: the engine aggregates window MEANS
/// (several rows pooled under one history key) and classifies those, so a
/// single row only makes sense offline.
#[allow(dead_code)]
pub fn lean_of_sample(s: &Sample, b: &Boundary) -> Option<f64> {
    lean(s.avg_runtime_wall, s.wait_freq, b)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn boundary() -> Boundary {
        Boundary {
            stage_min_ns: STAGE_MIN_NS,
            hop_min_hz: HOP_MIN_HZ,
            deadband_pct: 15,
        }
    }

    fn row(comm: &str, wait: u64, rt: u64, runs: u32, sleeps: u32) -> Sample {
        Sample {
            pid: 1,
            comm: comm.to_string(),
            wait_freq: wait,
            avg_runtime_wall: rt,
            runs,
            sleeps,
            ..Sample::default()
        }
    }

    #[test]
    fn compiler_shape_is_bulk() {
        // 5 ms per schedule, never sleeps: lean = log2(78.125) ≈ +6.29.
        let b = boundary();
        let l = lean(5_000_000, 0, &b).expect("evidence");
        assert!((l - (5_000_000f64 / 64_000.0).log2()).abs() < 1e-9);
        assert!(l > 0.0);
        assert_eq!(classify(5_000_000, 0, &b), Some(Lean::Bulk));
    }

    #[test]
    fn interactive_shape_is_responsive() {
        // 200 µs bursts at 500 Hz: log2(3.125) − log2(5) ≈ −0.68.
        let b = boundary();
        assert_eq!(classify(200_000, 500, &b), Some(Lean::Responsive));
    }

    #[test]
    fn inside_the_deadband_there_is_no_verdict() {
        let b = boundary();
        // deadband == log2(1.15) ≈ 0.2016; log2(74/64) ≈ 0.2095 is outside,
        // log2(70/64) ≈ 0.129 is inside.
        assert_eq!(classify(74_000, 100, &b), Some(Lean::Bulk));
        assert_eq!(lean(64_000, 100, &b), None, "exactly on the anchor");
        assert_eq!(lean(70_000, 100, &b), None);
        assert_eq!(classify(70_000, 110, &b), None);
        // Symmetric on the responsive side: log2(0.9) ≈ −0.152 is inside;
        // log2(0.85) ≈ −0.234 is outside. (Both need the burst above the
        // handoff floor, or the floor itself pins the value far negative.)
        assert_eq!(lean(57_600, 100, &b), None);
        assert_eq!(classify(54_400, 100, &b), Some(Lean::Responsive));
    }

    #[test]
    fn a_burst_of_zero_is_no_evidence() {
        // avg_runtime_wall == 0 means the task never ran in the window.
        let b = boundary();
        assert_eq!(lean(0, 10_000, &b), None);
        assert_eq!(classify(0, 0, &b), None);
    }

    #[test]
    fn counts_are_not_required_for_a_verdict() {
        // A row from an old collector carries runs = sleeps = 0; the burst
        // axis alone still classifies (counts are offline evidence).
        let b = boundary();
        let s = row("burner", 0, 5_000_000, 0, 0);
        assert_eq!(lean_of_sample(&s, &b), lean(5_000_000, 0, &b));
        assert_eq!(classify(s.avg_runtime_wall, s.wait_freq, &b), Some(Lean::Bulk));
    }

    #[test]
    fn boundary_is_monotone_in_both_axes() {
        // More burst ⇒ leaner bulk; more hops ⇒ leaner responsive. This is
        // the property a scalar `score` cannot express.
        let b = boundary();
        let slow = lean(1_000_000, 100, &b).expect("evidence");
        let long = lean(10_000_000, 100, &b).expect("evidence");
        assert!(long > slow);
        let chatty = lean(1_000_000, 2_000, &b).expect("evidence");
        assert!(chatty < slow);
    }

    #[test]
    fn sample_convenience_matches_the_numbers() {
        let b = boundary();
        let s = row("pipewire", 500, 200_000, 0, 0);
        let l = lean_of_sample(&s, &b).expect("evidence");
        assert_eq!(Some(l), lean(s.avg_runtime_wall, s.wait_freq, &b));
        assert!(l < 0.0, "frequent short bursts are responsive");
        assert_eq!(classify(s.avg_runtime_wall, s.wait_freq, &b), Some(Lean::Responsive));
    }
}

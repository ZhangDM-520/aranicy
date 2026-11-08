//! Window aggregation: monotonic per-task counters -> bias-engine samples.
//!
//! The BPF side only accumulates (`run_ns`, `runnable_ns`, `runs`, `sleeps`,
//! `wakes_issued`). Everything the bias engine scores is derived here from
//! two snapshots, so the arithmetic is deterministic and unit-testable with
//! an injected clock.
//!
//! ## Unit contract
//!
//! The sample fields, their units and their scales are defined once, on
//! [`aranicy_wire::Sample`]; this module only produces that type. In short:
//! `wait_freq`/`wake_freq` are events per second, `avg_runtime_wall` is
//! nanoseconds per schedule, `task_util_est` is DEMAND (running + runnable
//! duty) and `cpu_util_wall` is DELIVERED (running duty), both on the same
//! 0..[`MAX_UTIL`] scale so starvation is `delivered < ratio * demand`, and
//! `static_prio` is nice + [`NICE_OFFSET`].
//!
//! `runs`/`sleeps` are the raw deltas of the same counters the rates are
//! smoothed from: per-window COUNTS (`u32`, saturating), so a consumer can
//! tell "never slept" (`sleeps == 0` with `runs > 0`) from "not measured"
//! (a payload without the fields, which defaults to 0).
//!
//! `task_util_est` is named after the lavd field the bias engine reads; lavd
//! itself only accumulates running time, which cannot express demand because
//! a task that never sleeps would read as 0 wait. lavd also emits demand in
//! 0..1024 and delivered in 0..100 (percent), and the starved gate compares
//! them raw, which makes the comparison meaningless. Both are normalized to
//! 0..[`MAX_UTIL`] here so `delivered < ratio * demand` is a real statement
//! about share starvation.
//!
//! PELT is deliberately not used: `se.avg.*` is a FAIR-class path and stays
//! zero under sched_ext schedulers (measured on the development host).

use std::collections::BTreeMap;

pub use aranicy_wire::{MAX_UTIL, Sample};

pub const TASK_COMM_LEN: usize = 16;

const NANOS_PER_SEC: u64 = 1_000_000_000;

/// Mirrors `struct task_acc` in `bpf/collector.bpf.c` byte for byte.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RawAcc {
    pub comm: [u8; TASK_COMM_LEN],
    /// nice + 20 (the BPF side subtracts MAX_RT_PRIO).
    pub static_prio: u32,
    pub pad: u32,
    pub run_ns: u64,
    pub runnable_ns: u64,
    pub runs: u64,
    pub sleeps: u64,
    pub wakes_issued: u64,
    pub last_run_start: u64,
    pub last_enqueue: u64,
    pub last_seen: u64,
    /// Time of the last `sched_stat_runtime` tick (0 = none this run).
    pub last_tick: u64,
}

impl RawAcc {
    /// comm is NUL-padded; stop at the first NUL and replace invalid UTF-8.
    pub fn comm_str(&self) -> String {
        let end = self
            .comm
            .iter()
            .position(|b| *b == 0)
            .unwrap_or(TASK_COMM_LEN);
        String::from_utf8_lossy(&self.comm[..end]).into_owned()
    }

    fn parse(bytes: &[u8]) -> Option<Self> {
        if bytes.len() != size_of::<RawAcc>() {
            return None;
        }
        let u32_at =
            |o: usize| -> u32 { u32::from_ne_bytes(bytes[o..o + 4].try_into().expect("4 bytes")) };
        let u64_at =
            |o: usize| -> u64 { u64::from_ne_bytes(bytes[o..o + 8].try_into().expect("8 bytes")) };
        let mut comm = [0u8; TASK_COMM_LEN];
        comm.copy_from_slice(&bytes[0..TASK_COMM_LEN]);
        Some(RawAcc {
            comm,
            static_prio: u32_at(16),
            pad: u32_at(20),
            run_ns: u64_at(24),
            runnable_ns: u64_at(32),
            runs: u64_at(40),
            sleeps: u64_at(48),
            wakes_issued: u64_at(56),
            last_run_start: u64_at(64),
            last_enqueue: u64_at(72),
            last_seen: u64_at(80),
            last_tick: u64_at(88),
        })
    }
}

/// pid -> accumulator, as drained from the BPF map.
pub type Snapshot = BTreeMap<u32, RawAcc>;

/// Time constant of the rate smoothing. lavd keeps EWMA counts per event, so
/// a task that slept 2 s ago still reports a small non-zero rate; an
/// instantaneous window rate would read 0 for a busy compiler and the bias
/// engine skips such samples entirely (score needs all three signals > 0).
const RATE_TC_NS: u64 = 30_000_000_000;

#[derive(Debug, Default, Clone, Copy)]
struct Ema {
    wait: f64,
    wake: f64,
    runtime: f64,
    /// `runtime` holds a real observation (not the zero default).
    rt_init: bool,
}

/// Converts successive snapshots into per-task window samples.
#[derive(Debug, Default)]
pub struct Aggregator {
    prev: Snapshot,
    prev_at: Option<u64>,
    ema: BTreeMap<u32, Ema>,
}

impl Aggregator {
    pub fn new() -> Self {
        Self::default()
    }

    /// Ingest a snapshot taken at `now_ns` (CLOCK_MONOTONIC). The first call
    /// only establishes the baseline; later calls return one row per task
    /// that showed activity during the window.
    pub fn ingest(&mut self, cur: Snapshot, now_ns: u64) -> Vec<Sample> {
        let Some(prev_at) = self.prev_at else {
            self.prev = cur;
            self.prev_at = Some(now_ns);
            return Vec::new();
        };
        let dt = now_ns.saturating_sub(prev_at);
        if dt == 0 {
            // Same instant: nothing can have elapsed. Keep the older
            // baseline so the next drain still measures a real window.
            return Vec::new();
        }

        let mut out = Vec::new();
        for (pid, c) in &cur {
            // pid 0 is the per-CPU idle task: it has no meaningful comm to
            // bias and it wakes everyone (`swapper/N` dominated the probe).
            if *pid == 0 {
                continue;
            }
            // A pid absent from the baseline was created after it: its map
            // entry starts at zero (fork/exec reset it), so cumulative
            // counters ARE this window's activity. Treat it as a zero
            // baseline instead of discarding the first window — otherwise a
            // build helper that lives under one drain is never sampled.
            let zero = RawAcc::default();
            let (p, first_sighting) = match self.prev.get(pid) {
                Some(p) => (p, false),
                None => (&zero, true),
            };
            // pid reuse: a different task must not inherit the counters or
            // the smoothing state of the previous occupant.
            if !first_sighting && p.comm != c.comm {
                self.ema.remove(pid);
                continue;
            }

            let d_run = c.run_ns.saturating_sub(p.run_ns);
            let d_runnable = c.runnable_ns.saturating_sub(p.runnable_ns);
            let d_runs = c.runs.saturating_sub(p.runs);
            let d_sleeps = c.sleeps.saturating_sub(p.sleeps);
            let d_wakes = c.wakes_issued.saturating_sub(p.wakes_issued);
            // Exponential smoothing with a wall-clock-derived alpha keeps the
            // signal alive across windows where the task did not sleep/wake.
            let alpha = 1.0 - (-(dt as f64) / RATE_TC_NS as f64).exp();
            let seen_before = self.ema.contains_key(pid);
            if d_run == 0 && d_runnable == 0 && d_runs == 0 && d_sleeps == 0 && d_wakes == 0 {
                // Nothing happened, so no row is emitted — but presence in the
                // map must not freeze the last rate forever: a task idle for
                // minutes would otherwise report its pre-idle rate as soon as
                // it moved again.
                if seen_before {
                    let e = self.ema.get_mut(pid).expect("checked by contains_key");
                    e.wait += (0.0 - e.wait) * alpha;
                    e.wake += (0.0 - e.wake) * alpha;
                }
                continue;
            }

            let per_sec = |delta: u64| -> f64 {
                (u128::from(delta) * u128::from(NANOS_PER_SEC)) as f64 / dt as f64
            };
            let util = |nanos: u64| -> u64 {
                (((u128::from(nanos) * u128::from(MAX_UTIL)) / u128::from(dt)) as u64).min(MAX_UTIL)
            };

            // A task that ran through the window without switching out has
            // d_run > 0 but d_runs == 0; dividing by zero would report
            // avg_runtime_wall = 0, and the bias engine discards such samples
            // (that is exactly what a CPU-bound compiler looks like). Use the
            // in-flight run length in that case.
            let instant_rt = if d_runs > 0 {
                d_run as f64 / d_runs as f64
            } else if d_run > 0 {
                let start = if c.last_run_start > 0 {
                    c.last_run_start
                } else {
                    c.last_tick
                };
                if start > 0 && now_ns > start {
                    now_ns.saturating_sub(start) as f64
                } else {
                    d_run as f64
                }
            } else {
                0.0
            };
            let inst_wait = per_sec(d_sleeps);
            let inst_wake = per_sec(d_wakes);
            let e = self.ema.entry(*pid).or_default();
            if seen_before {
                e.wait += (inst_wait - e.wait) * alpha;
                e.wake += (inst_wake - e.wake) * alpha;
            } else {
                // Seed with the first real observation: smoothing from zero
                // would report a dead signal for a task that has been alive
                // for exactly one window.
                e.wait = inst_wait;
                e.wake = inst_wake;
            }
            // Runtime is seeded on the first window that HAS runtime, not the
            // first window a pid is seen: a rate-only window would otherwise
            // pin it to zero for good.
            if instant_rt > 0.0 {
                if e.rt_init && seen_before {
                    e.runtime += (instant_rt - e.runtime) * alpha;
                } else {
                    e.runtime = instant_rt;
                }
                e.rt_init = true;
            }

            out.push(Sample {
                pid: i64::from(*pid),
                comm: c.comm_str(),
                static_prio: u64::from(c.static_prio),
                wait_freq: e.wait.round().max(0.0) as u64,
                wake_freq: e.wake.round().max(0.0) as u64,
                avg_runtime_wall: e.runtime.round().max(0.0) as u64,
                task_util_est: util(d_run.saturating_add(d_runnable)),
                cpu_util_wall: util(d_run),
                // Raw per-window counts: the exact evidence the smoothed
                // means lose (a window with runs but no sleeps is a real
                // "never slept", not a missing measurement).
                runs: u32::try_from(d_runs).unwrap_or(u32::MAX),
                sleeps: u32::try_from(d_sleeps).unwrap_or(u32::MAX),
            });
        }

        self.ema.retain(|pid, _| cur.contains_key(pid));
        self.prev = cur;
        self.prev_at = Some(now_ns);
        out
    }

    /// Order rows for delivery and cut them to `cap`, returning how many rows
    /// the cut dropped (the observation budget's cost, reported on the wire).
    ///
    /// Sort by DEMAND (`task_util_est`) first: `task_util_est >=
    /// cpu_util_wall` by construction (running + runnable vs running), so
    /// demand ordering still puts hot tasks first while retaining a starved
    /// RESPONSIVE task — high demand, low delivered — that delivered-first
    /// ordering would cut first. That task is exactly what the starved gate
    /// exists for, and a name that never appears never learns.
    /// `cpu_util_wall`, `comm`, `pid` are deterministic tie-breaks.
    pub fn order(rows: &mut Vec<Sample>, cap: usize) -> u32 {
        let dropped = rows.len().saturating_sub(cap);
        rows.sort_by(|a, b| {
            b.task_util_est
                .cmp(&a.task_util_est)
                .then_with(|| b.cpu_util_wall.cmp(&a.cpu_util_wall))
                .then_with(|| a.comm.cmp(&b.comm))
                .then_with(|| a.pid.cmp(&b.pid))
        });
        rows.truncate(cap);
        u32::try_from(dropped).unwrap_or(u32::MAX)
    }
}

/// Parse one raw map value (kept public for the loader and its tests).
pub fn parse_raw(bytes: &[u8]) -> Option<RawAcc> {
    RawAcc::parse(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    const T0: u64 = 1_000 * NANOS_PER_SEC;
    const ONE_SEC: u64 = NANOS_PER_SEC;

    fn acc(comm: &str) -> RawAcc {
        let mut a = RawAcc::default();
        let b = comm.as_bytes();
        a.comm[..b.len()].copy_from_slice(b);
        a.static_prio = 20;
        a
    }

    fn snap(pid: u32, a: RawAcc) -> Snapshot {
        let mut m = Snapshot::new();
        m.insert(pid, a);
        m
    }

    #[test]
    fn first_snapshot_is_baseline() {
        let mut agg = Aggregator::new();
        assert!(agg.ingest(snap(7, acc("app")), T0).is_empty());
    }

    #[test]
    fn window_math_is_exact() {
        let mut agg = Aggregator::new();
        agg.ingest(snap(7, acc("app")), T0);

        let mut c = acc("app");
        c.run_ns = 500_000_000; // 50% of the window running
        c.runnable_ns = 250_000_000; // 25% runnable
        c.runs = 100;
        c.sleeps = 50;
        c.wakes_issued = 20;
        let rows = agg.ingest(snap(7, c), T0 + ONE_SEC);

        assert_eq!(rows.len(), 1);
        let s = &rows[0];
        assert_eq!(s.pid, 7);
        assert_eq!(s.comm, "app");
        assert_eq!(s.wait_freq, 50, "sleeps per second");
        assert_eq!(s.wake_freq, 20, "wakes issued per second");
        assert_eq!(s.avg_runtime_wall, 5_000_000, "500ms / 100 runs");
        assert_eq!(s.task_util_est, 768, "running + runnable = 75% of 1024");
        assert_eq!(s.cpu_util_wall, 512, "50% of 1024");
        assert_eq!(s.static_prio, 20);
        assert_eq!(s.runs, 100, "per-window schedule count, not a rate");
        assert_eq!(s.sleeps, 50, "per-window sleep count, not a rate");
    }

    /// A task that runs through the whole window without switching out has
    /// d_runs == 0: it must still report a runtime, or the bias engine drops
    /// exactly the CPU-bound samples it needs.
    #[test]
    fn uninterrupted_run_reports_runtime() {
        let mut agg = Aggregator::new();
        agg.ingest(snap(7, acc("burner")), T0);

        let mut c = acc("burner");
        c.run_ns = 480_000_000; // ran 96% of the window, never switched out
        c.last_run_start = T0;
        let rows = agg.ingest(snap(7, c), T0 + 500_000_000);
        assert_eq!(rows.len(), 1);
        assert!(
            rows[0].avg_runtime_wall > 0,
            "uninterrupted run reported no runtime"
        );
        assert_eq!(rows[0].cpu_util_wall, 983);
    }

    /// A pid first seen after the baseline was created in-window with zeroed
    /// counters: its cumulative values ARE the window's activity, so the row
    /// must not be discarded (a build helper can live under one drain).
    #[test]
    fn mid_window_task_is_not_discarded() {
        let mut agg = Aggregator::new();
        agg.ingest(snap(7, acc("app")), T0);

        let mut fresh = acc("cc1plus");
        fresh.run_ns = 250_000_000;
        fresh.runs = 1;
        fresh.last_run_start = T0 + 100_000_000;
        let rows = agg.ingest(snap(9, fresh), T0 + ONE_SEC);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].pid, 9);
        assert_eq!(rows[0].cpu_util_wall, 256);
        assert_eq!(rows[0].avg_runtime_wall, 250_000_000);
        assert_eq!(rows[0].runs, 1, "cumulative counters ARE the window");
        assert_eq!(rows[0].sleeps, 0, "never slept: a real zero, not absent");
    }

    #[test]
    fn utilization_is_clamped_to_one_cpu() {
        let mut agg = Aggregator::new();
        agg.ingest(snap(7, acc("busy")), T0);
        let mut c = acc("busy");
        c.run_ns = 2 * ONE_SEC; // counters that outran the window
        c.runnable_ns = 2 * ONE_SEC;
        let rows = agg.ingest(snap(7, c), T0 + ONE_SEC);
        assert_eq!(rows[0].cpu_util_wall, MAX_UTIL);
        assert_eq!(rows[0].task_util_est, MAX_UTIL);
    }

    #[test]
    fn idle_task_is_never_reported() {
        let mut agg = Aggregator::new();
        let mut idle = acc("swapper/9");
        idle.run_ns = 0;
        agg.ingest(snap(0, idle), T0);
        idle.wakes_issued = 4_000;
        idle.runnable_ns = ONE_SEC / 2;
        assert!(agg.ingest(snap(0, idle), T0 + ONE_SEC).is_empty());
    }

    #[test]
    fn inactive_tasks_are_skipped() {
        let mut agg = Aggregator::new();
        let mut idle = acc("idle");
        idle.sleeps = 3;
        agg.ingest(snap(7, idle), T0);
        assert!(agg.ingest(snap(7, idle), T0 + ONE_SEC).is_empty());
    }

    /// A task that is present in the map but does nothing must have its
    /// rates relaxed toward zero: otherwise a task idle for minutes reports
    /// its pre-idle rate in the first window it moves again.
    #[test]
    fn idle_windows_decay_the_rates() {
        let mut agg = Aggregator::new();
        agg.ingest(snap(7, acc("app")), T0);

        let mut busy = acc("app");
        busy.runs = 10;
        busy.run_ns = ONE_SEC / 10;
        busy.sleeps = 100;
        busy.wakes_issued = 100;
        let rows = agg.ingest(snap(7, busy), T0 + ONE_SEC);
        assert_eq!(rows[0].wait_freq, 100);

        // 120 s of nothing: still in the map, no counters moved.
        assert!(agg.ingest(snap(7, busy), T0 + 121 * ONE_SEC).is_empty());

        // One event in the next window must not resurrect the stale 100/s.
        let mut again = busy;
        again.sleeps += 1;
        let rows = agg.ingest(snap(7, again), T0 + 122 * ONE_SEC);
        assert_eq!(rows.len(), 1);
        assert!(
            rows[0].wait_freq <= 3,
            "stale rate survived idleness: {}",
            rows[0].wait_freq
        );
    }

    #[test]
    fn pid_reuse_does_not_inherit_counters() {
        let mut agg = Aggregator::new();
        agg.ingest(snap(7, acc("old")), T0);
        let mut c = acc("new");
        c.run_ns = ONE_SEC;
        assert!(agg.ingest(snap(7, c), T0 + ONE_SEC).is_empty());
    }

    #[test]
    fn exited_pids_are_not_emitted() {
        let mut agg = Aggregator::new();
        agg.ingest(snap(7, acc("gone")), T0);
        assert!(agg.ingest(Snapshot::new(), T0 + ONE_SEC).is_empty());
    }

    #[test]
    fn same_instant_keeps_the_baseline() {
        let mut agg = Aggregator::new();
        agg.ingest(snap(7, acc("app")), T0);
        let mut c = acc("app");
        c.run_ns = ONE_SEC;
        assert!(agg.ingest(snap(7, c), T0).is_empty());
        // the real window still measures from the original baseline
        let rows = agg.ingest(snap(7, c), T0 + ONE_SEC);
        assert_eq!(rows[0].cpu_util_wall, MAX_UTIL);
    }

    #[test]
    fn comm_is_nul_trimmed_and_utf8_lossy() {
        let mut a = acc("abc");
        a.comm[3] = 0xff;
        assert_eq!(a.comm_str(), "abc\u{fffd}");
    }

    #[test]
    fn raw_layout_round_trips() {
        let mut a = acc("layout");
        a.run_ns = 11;
        a.runnable_ns = 22;
        a.runs = 33;
        a.sleeps = 44;
        a.wakes_issued = 55;
        a.last_run_start = 66;
        a.last_enqueue = 77;
        a.last_seen = 88;
        a.last_tick = 99;
        let mut bytes = Vec::with_capacity(size_of::<RawAcc>());
        bytes.extend_from_slice(&a.comm);
        bytes.extend_from_slice(&a.static_prio.to_ne_bytes());
        bytes.extend_from_slice(&a.pad.to_ne_bytes());
        for f in [
            a.run_ns,
            a.runnable_ns,
            a.runs,
            a.sleeps,
            a.wakes_issued,
            a.last_run_start,
            a.last_enqueue,
            a.last_seen,
            a.last_tick,
        ] {
            bytes.extend_from_slice(&f.to_ne_bytes());
        }
        assert_eq!(parse_raw(&bytes), Some(a));
        assert_eq!(size_of::<RawAcc>(), 96, "must match struct task_acc");
    }

    #[test]
    fn order_is_hot_first_and_capped() {
        let mk = |comm: &str, cpu, demand| Sample {
            comm: comm.to_string(),
            cpu_util_wall: cpu,
            task_util_est: demand,
            ..Sample::default()
        };
        let mut rows = vec![mk("cold", 10, 10), mk("hot", 900, 900), mk("warm", 100, 900)];
        let dropped = Aggregator::order(&mut rows, 2);
        assert_eq!(dropped, 1);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].comm, "hot");
        assert_eq!(rows[1].comm, "warm");
    }

    /// A starved RESPONSIVE task (high demand, low delivered) must survive a
    /// cut that delivered-first ordering would have dropped. This is the
    /// observation-budget contract: a name that never appears never learns.
    #[test]
    fn demand_ordering_retains_a_starved_task() {
        let mk = |comm: &str, cpu, demand| Sample {
            comm: comm.to_string(),
            cpu_util_wall: cpu,
            task_util_est: demand,
            ..Sample::default()
        };
        // Delivered-first would keep `hot` and `steady`, cutting `starved`.
        let mut rows = vec![
            mk("hot", 1024, 1024),
            mk("starved", 120, 950),
            mk("steady", 800, 800),
        ];
        let dropped = Aggregator::order(&mut rows, 2);
        assert_eq!(dropped, 1);
        let kept: Vec<&str> = rows.iter().map(|s| s.comm.as_str()).collect();
        assert_eq!(kept, vec!["hot", "starved"]);
    }

    #[test]
    fn order_reports_zero_when_nothing_is_dropped() {
        let mk = |comm: &str| Sample {
            comm: comm.to_string(),
            cpu_util_wall: 10,
            task_util_est: 10,
            ..Sample::default()
        };
        let mut rows = vec![mk("a"), mk("b")];
        assert_eq!(Aggregator::order(&mut rows, 2), 0);
        assert_eq!(Aggregator::order(&mut rows, 1), 1);
    }
}

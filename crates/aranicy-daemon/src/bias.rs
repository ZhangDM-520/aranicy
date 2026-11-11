//! Bias estimator + controller (see plan.md 4.2).
//!
//! Control law (dynamic nice bias in [min, max] around the rule's base nice):
//!
//! * **Weight-free estimator (loop breaker):** per sample
//!   `S0 = ilog2(wait_ft·wake_ft) + ilog2(runtime_ft)` with `weight_ft := 1`,
//!   `score = S0²`. lavd's own `lat_cri` embeds the task weight (= f(our
//!   nice)), so classifying on it would let our bias justify itself. All
//!   inputs here are weight-independent (see `sampler.rs`), so the same
//!   behavior classifies identically at any applied nice — asserted in tests.
//! * **Threshold:** T_us = userspace EWMA (60 s time constant) of the
//!   population mean score from the same batches (self-consistent scale; the
//!   lavd-side `avg_lat_cri` scale is deliberately never mixed in).
//! * **Classification:** hysteresis per `comm`: BULK below `deadband_pct`
//!   under T for 2 consecutive classifying windows, RESPONSIVE above for 2,
//!   deadband holds the class. Windows are `window_s` and need `min_samples`
//!   scored samples; absence is never evidence (streaks/untouched confidence)
//!   and presence is never counter-evidence (sparse-but-active windows hold
//!   the confidence; only absence decays it — see the history bullet).
//!   Decisive evidence (beyond 2× the deadband — past the actuator-swing
//!   envelope the deadband was sized for) classifies on ONE window at
//!   `min_samples / 4` (≥ 4) samples, so short-lived phases (builds) are not
//!   starved by the two-window confirmation.
//! * **Controller:** BULK → `max_bias`, RESPONSIVE + share-starved →
//!   `min_bias`, RESPONSIVE → 0, deadband/unknown → hold then relax toward 0
//!   at the release rate. Attack (away from 0 toward target) ≤ 1 step /
//!   `attack_step_s` from want-onset; release (toward 0) ≤ 1 step /
//!   `release_step_s`; one bias step globally per `global_step_s` (shared
//!   budget, deterministic BTreeMap order).
//! * **History keys (fuzzy):** a `bias_group.<tag> = <regex>` family pools
//!   every matching comm into one entry named `<tag>` (a build's
//!   cc1plus→as→ld→lto1 handoff learns as one `build` stream); otherwise
//!   [`normalize_name`] collapses per-instance (`name:7`) and version
//!   (`clang-19`) suffixes. Semantic families are never guessed.
//! * **History never permanently fixes bias:** confidence decays with
//!   wall-time since last activity (half-life `half_life_s`) and each
//!   `RELAX_STEP` of decay releases one step toward 0; zero confidence forces
//!   bias = 0. Entry cap with LRU eviction by `last_seen`.
//!
//! All time enters through `now: SystemTime` parameters (never
//! `Instant::now()`), so behavior is deterministic given (samples, timestamps).
#![allow(dead_code)] // public surface is wired into daemon.rs by the caller

use crate::bias_state::{BiasClass, BiasSnapshot, Durable, EntrySnapshot, BIAS_STATE_VERSION};
use crate::classify::{self, Boundary, Lean};
use aranicy_wire::Sample;
use std::collections::btree_map::Entry as MapEntry;
use std::collections::BTreeMap;
use std::time::SystemTime;

/// Confidence assigned when a closed-loop mismatch forces a re-baseline.
pub const LOW_CONFIDENCE: f64 = 0.25;

/// Confidence at or below this flushes to zero (zero confidence ⇒ zero bias).
const CONFIDENCE_EPS: f64 = 1e-9;

/// Each `RELAX_STEP` of confidence decay earns one release step toward 0.
const RELAX_STEP: f64 = 0.25;

/// Windows of agreeing evidence needed to move the class (hysteresis
/// confirmation depth). Shared by the classifier's streak rule and the
/// attack-want grace period, which mirrors it.
const HYSTERESIS_STREAK: u32 = 2;

/// EWMA time constant (seconds) of the population threshold T_us.
const THRESHOLD_TC_S: f64 = 60.0;

/// Shadow logging cadence: at most one `bias shadow` line per name and one
/// `bias shadow summary` line per engine, per interval. Volume is the whole
/// reason shadow mode is behind a flag; the counters are unbounded, the log
/// is not.
const SHADOW_LOG_S: u64 = 600;

fn unix_secs(t: SystemTime) -> u64 {
    t.duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// Weight-free per-sample score (the loop breaker): lavd's lat_cri formula
/// shape with `weight_ft := 1`, so our applied nice can never feed back into
/// the signal that justifies the bias.
///
/// `wait_freq == 0` and `wake_freq == 0` are MEANINGFUL zeros, not missing
/// fields: a compiler burning a core has no wait events and does not wake
/// anyone (its children are forked, not woken), and "long runtime, no
/// sleeps" is the canonical BULK case. Skipping such samples would blind the
/// classifier to exactly the workloads the bias exists for (measured:
/// cc1plus never sleeps and wakes nobody while compiling). Only a zero
/// `avg_runtime_wall` means "no evidence": the task never ran in the window.
pub fn sample_score(s: &Sample) -> Option<u64> {
    if s.avg_runtime_wall == 0 {
        return None;
    }
    let wait_ft = s.wait_freq.min(100_000) + 1;
    let wake_ft = s.wake_freq.min(100_000) + 1;
    let runtime_ft = (1_000_000_000u64 / s.avg_runtime_wall.max(1)).max(1);
    let s0 = u64::from((wait_ft * wake_ft).ilog2() + runtime_ft.ilog2());
    Some(s0 * s0)
}

/// A declared fuzzy family: every matching comm shares one learned entry.
pub type BiasGroup = (String, regex::Regex);

/// Collapse suffix families onto one history key: everything after the last
/// `:`/`#` is a per-instance suffix (`kworker/u97:10`, `ghostty:gdrv0`,
/// `WRRende~ckend#0`) and one trailing separator+digits run is a version
/// (`clang-19`, `python3.12`). Deliberately conservative: `cc1plus`, `lto1`
/// and `x265` are NOT folded — semantic families belong in `bias_group`.
pub fn normalize_name(comm: &str) -> std::borrow::Cow<'_, str> {
    let mut s: &str = comm;
    if let Some(i) = s.rfind([':', '#']) {
        if i > 0 {
            s = &s[..i];
        }
    }
    let b = s.as_bytes();
    let mut end = b.len();
    while end > 0 && b[end - 1].is_ascii_digit() {
        end -= 1;
    }
    if end > 0 && end < b.len() && matches!(b[end - 1], b'-' | b'_' | b'.') {
        end -= 1;
        s = &s[..end];
    } else {
        s = &s[..b.len()]; // no separator+digits: keep whole
    }
    if s.is_empty() {
        s = comm;
    }
    if s.len() == comm.len() {
        std::borrow::Cow::Borrowed(comm)
    } else {
        std::borrow::Cow::Owned(s.to_string())
    }
}

/// History key for a sampled comm: a declared `bias_group` pattern wins
/// (fuzzy family pooling), else the normalized name. Borrowed when nothing
/// changes — the exec path stays allocation-free.
fn entry_key<'a>(comm: &'a str, groups: &'a [BiasGroup]) -> std::borrow::Cow<'a, str> {
    for (tag, re) in groups {
        if re.is_match(comm) {
            return std::borrow::Cow::Borrowed(tag.as_str());
        }
    }
    normalize_name(comm)
}

/// Engine knobs (plan.md config keys: `bias_*`).
#[derive(Debug, Clone, PartialEq)]
pub struct BiasConfig {
    /// Classification window, seconds.
    pub window_s: u64,
    /// Deadband half-width around T, percent (0.85 / 1.15 at 15).
    pub deadband_pct: u64,
    /// Scored samples per name-window required to classify.
    pub min_samples: u64,
    /// Confidence half-life while unseen, seconds.
    pub half_life_s: u64,
    /// Per-name entry cap (LRU by `last_seen`).
    pub max_entries: usize,
    /// Lower clamp for bias (also the starved-RESPONSIVE target).
    pub min_bias: i8,
    /// Upper clamp for bias (also the BULK target).
    pub max_bias: i8,
    /// Minimum spacing of attack steps (toward target, away from 0).
    pub attack_step_s: u64,
    /// Minimum spacing of release steps (toward 0).
    pub release_step_s: u64,
    /// Global minimum spacing of any bias step across all names.
    pub global_step_s: u64,
    /// Starved when delivered < ratio × demand.
    pub starved_ratio: f64,
    /// Demand noise floor: below this the starved gate stays shut.
    pub starved_min_demand: u64,
    /// Aggregate CPU DEMAND a name must carry before a POSITIVE (bulk) step
    /// may be taken, on the `MAX_UTIL` scale where 1024 is one core. It is the
    /// window's total across the name's rows (instances × duty), so a parallel
    /// build of many short-lived compilers passes while a shell one-liner that
    /// ran for 2 ms reads ~12 however interactive its score looks.
    pub attack_min_util: u64,
    /// Run the absolute two-axis lean classifier alongside the actuated one
    /// and log only disagreements (observation only; never actuates).
    pub shadow: bool,
    /// Lean axis: the "long burst" threshold in nanoseconds (cake's seat
    /// threshold). Tunable so the flip gate can move it without a rebuild.
    pub lean_stage_min_ns: u64,
    /// Lean axis: the hop floor in Hz; below it the rate axis reads as 0.
    pub lean_hop_hz: u64,
}

impl Default for BiasConfig {
    fn default() -> Self {
        BiasConfig {
            window_s: 60,
            deadband_pct: 15,
            min_samples: 30,
            half_life_s: 1800,
            max_entries: 4096,
            min_bias: -1,
            max_bias: 3,
            // Responsive ramp: a 60 s step outlives short build phases
            // (measured: cc1plus reached +3 only after the compile ended).
            attack_step_s: 30,
            release_step_s: 300,
            global_step_s: 5,
            starved_ratio: 0.5,
            starved_min_demand: 64,
            attack_min_util: 512,
            shadow: false,
            lean_stage_min_ns: classify::STAGE_MIN_NS,
            lean_hop_hz: classify::HOP_MIN_HZ,
        }
    }
}

impl BiasConfig {
    /// The lean boundary this configuration expresses. One place that maps
    /// knobs onto [`Boundary`], so the shadow path and any future actuator
    /// read the same three numbers.
    fn boundary(&self) -> Boundary {
        Boundary {
            stage_min_ns: self.lean_stage_min_ns,
            hop_min_hz: self.lean_hop_hz,
            deadband_pct: self.deadband_pct,
        }
    }
}

/// One reported bias move, returned as data by [`BiasEngine::ingest`] and
/// [`BiasEngine::tick`] (there is no drain call).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Change {
    pub name: String,
    /// Bias before the move.
    pub prev: i8,
    pub bias: i8,
    pub class: BiasClass,
    pub cause: Cause,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cause {
    /// A window closed with classifiable evidence.
    Evidence,
    /// A non-evidence move toward the neutral: aging/deadband relax, or a
    /// reload's clamp/merge.
    Relax,
    /// Zero confidence flushed the bias.
    Flush,
}

/// One shadow comparison of a closed window: what the population boundary
/// was about to say (`pop`, the candidate before hysteresis) and what the
/// absolute lean axis says about the same window (`lean`). Observation only —
/// it is reported and counted, never read by any control law.
#[derive(Debug, Clone, Copy)]
struct ShadowVerdict {
    pop: Option<BiasClass>,
    lean: Option<Lean>,
    /// The raw `lean` value behind `lean` (`None` = inside the deadband or no
    /// evidence); kept for the log line, which wants the number, not the sign.
    lean_value: Option<f64>,
    /// Window means the verdict was computed from.
    burst_ns: u64,
    hop_hz: u64,
}

/// The lean class a score class stands for, for comparison purposes only.
fn class_to_lean(c: BiasClass) -> Option<Lean> {
    match c {
        BiasClass::Bulk => Some(Lean::Bulk),
        BiasClass::Responsive => Some(Lean::Responsive),
        BiasClass::Unknown => None,
    }
}

#[derive(Debug)]
struct Entry {
    durable: Durable,
    live: Live,
}

/// Everything a restart cannot keep: the closed-loop claim (no actuation
/// survives a restart, so neither can the expectation), the controller's
/// target and rate stamps, and the window accumulators. Deliberately has no
/// `Deserialize` — transient state loaded from disk is a compile error.
#[derive(Debug, Default)]
struct Live {
    /// Last nice value this daemon actually applied for the name (the
    /// closed-loop expectation; `None` = never actuated by us).
    applied_nice: Option<i8>,
    /// `note_applied` has been called since creation/restore, so
    /// `applied_nice` is an authoritative record (even an explicit `None`
    /// = the apply did not take). Distinguishes a released claim from the
    /// total absence of a record (a restart) — see `verify_row`.
    claim_recorded: bool,
    target: i8,
    /// Timestamp the stored confidence is valid at (aging anchor).
    aged_at: u64,
    /// Confidence watermark since the last relax step / evidence gain.
    relax_watermark: f64,
    cpu_util_ewma: f64,
    demand_ewma: f64,
    /// Aggregate demand of the name over a window: the SUM of its rows'
    /// `task_util_est`, i.e. instances × duty on the 1024-per-core scale.
    /// Mean demand cannot see a parallel build (each short-lived `cc1plus`
    /// row looks idle); the sum can.
    agg_demand_ewma: f64,
    util_init: bool,
    /// `score_ema` holds a real observation (a genuine 0.0 score must not be
    /// re-used as the "uninitialized" sentinel).
    score_init: bool,
    starved: bool,
    win_score_sum: u64,
    win_score_n: u64,
    win_n: u64,
    win_cpu_sum: u64,
    win_util_sum: u64,
    win_util_n: u64,
    /// Lean axis (shadow only): summed burst (ns per schedule) and hop rate
    /// (sleeps/s) over the rows that carry runtime evidence, and how many
    /// such rows the window had. Transient by construction — the persisted
    /// entry does not grow (this wave is observation only).
    win_burst_sum_ns: u64,
    win_hop_sum: u64,
    win_lean_n: u64,
    /// Last time this name's disagreement was logged (`SHADOW_LOG_S`
    /// throttle). Transient: a restart may log once more, never less.
    shadow_logged_at: Option<u64>,
    pending: Option<BiasClass>,
    streak: u32,
    want_since_attack: Option<u64>,
    /// When the attack want went false (grace timer: a short deadband
    /// excursion must not clear the attack desire — see `slew_pass`).
    attack_want_idle_since: Option<u64>,
    want_since_release: Option<u64>,
    last_step_attack: Option<u64>,
    last_step_release: Option<u64>,
    /// Bias before the pending, not-yet-reported move, and what moved it.
    changed: Option<(i8, Cause)>,
}

impl Live {
    fn restore(d: &Durable, cfg: &BiasConfig) -> Self {
        Live {
            // The claim is not persisted: a restart cannot verify anything
            // against it, and a frozen entry must re-baseline on a fresh
            // sample instead of being pinned by a phantom expectation.
            applied_nice: None,
            claim_recorded: false,
            // Starved status is rebuilt from fresh samples; restored
            // RESPONSIVE entries target 0 until evidence says otherwise.
            target: target_for(cfg, d.class, false),
            aged_at: d.last_seen, // aging resumes from the last sighting
            relax_watermark: d.confidence,
            score_init: true, // a persisted score is always real evidence
            ..Self::default()
        }
    }
}

fn target_for(cfg: &BiasConfig, class: BiasClass, starved: bool) -> i8 {
    match class {
        BiasClass::Bulk => cfg.max_bias,
        BiasClass::Responsive => {
            if starved {
                cfg.min_bias
            } else {
                0
            }
        }
        BiasClass::Unknown => 0,
    }
}

/// Merge `other` into `dst` when two entries collapse onto one history key
/// after a `reconfigure` re-key (a new `bias_group` family adopting both, or a
/// family removed so its tag becomes an ordinary name).
///
/// The higher-confidence side survives (its score_ema, class, sample count and
/// closed-loop claim are the better evidence), but:
/// * the larger `|bias|` wins, so a merge never weakens an existing actuator
///   state (the slew law then reconciles bias with the surviving class's
///   target at the normal rate — merging must not release a learned ramp);
/// * `last_seen` takes the later stamp so LRU cannot evict a pair that was
///   live on either side.
fn merge_entries(dst: &mut Entry, mut other: Entry) {
    if other.durable.confidence > dst.durable.confidence {
        std::mem::swap(dst, &mut other);
    }
    // `dst` now holds the higher-confidence entry; `other` is the loser.
    let survivor_bias = dst.durable.bias;
    if other.durable.bias.abs() > dst.durable.bias.abs() {
        dst.durable.bias = other.durable.bias;
    }
    dst.durable.last_seen = dst.durable.last_seen.max(other.durable.last_seen);
    if dst.durable.bias != survivor_bias {
        dst.live.changed = Some((survivor_bias, Cause::Relax));
    }
}

fn want_stamp(current: Option<u64>, want: bool, now: u64) -> Option<u64> {
    if want {
        Some(current.unwrap_or(now))
    } else {
        None
    }
}

/// (attack_want, release_want, delta) for one entry. Away from 0 toward the
/// target is attack; toward 0 (target 0, overshoot, or relax credit) release.
fn direction_wants(e: &Entry) -> (bool, bool, i16) {
    let relax_pending =
        e.durable.bias != 0 && e.live.relax_watermark - e.durable.confidence >= RELAX_STEP;
    let delta = i16::from(e.live.target) - i16::from(e.durable.bias);
    let (mut attack_want, mut release_want) = (false, relax_pending);
    if delta != 0 {
        let next = e.durable.bias + delta.signum() as i8;
        if next.abs() > e.durable.bias.abs() {
            attack_want = true;
        } else {
            release_want = true;
        }
    }
    (attack_want, release_want, delta)
}

impl Entry {
    fn new(now: u64) -> Self {
        Entry {
            durable: Durable {
                last_seen: now,
                ..Durable::default()
            },
            live: Live {
                aged_at: now,
                ..Live::default()
            },
        }
    }

    fn reset_window(&mut self) {
        self.live.win_score_sum = 0;
        self.live.win_score_n = 0;
        self.live.win_n = 0;
        self.live.win_cpu_sum = 0;
        self.live.win_util_sum = 0;
        self.live.win_util_n = 0;
        self.live.win_burst_sum_ns = 0;
        self.live.win_hop_sum = 0;
        self.live.win_lean_n = 0;
    }

    /// Closed-loop re-baseline after a frozen mismatch: unfreeze and drop
    /// confidence to low; the recorded bias and class stay until evidence or
    /// aging moves them. The WANT stamps survive: the controller's desire
    /// (target vs bias) did not change, and resetting the want onset would
    /// re-arm the 30 s attack timer and push the entry to the back of the
    /// global-step FIFO on every re-baseline — under churn comms (a build's
    /// short-lived cc1plus/ld) that starved the ramp at its first step.
    fn rebaseline(&mut self, now: u64) {
        self.durable.frozen = false;
        self.durable.confidence = LOW_CONFIDENCE;
        self.live.relax_watermark = LOW_CONFIDENCE;
        self.live.pending = None;
        self.live.streak = 0;
        self.reset_window();
        self.live.aged_at = now;
    }

    /// Returns the shadow comparison for this window when shadow mode is on,
    /// the window carries lean evidence AND the population boundary produced
    /// a verdict to compare against (no scored rows or no `T` yet ⇒ `None`).
    /// The caller (which owns the counters, the log throttle and the engine's
    /// clock) decides what to do with it; nothing here can influence the
    /// actuated class, target or slew state.
    fn close_window(
        &mut self,
        cfg: &BiasConfig,
        threshold: Option<f64>,
        alpha: f64,
        close_t: u64,
    ) -> Option<ShadowVerdict> {
        if self.live.win_n == 0 {
            // Absent this window: silence is never evidence. Streaks,
            // confidence and target all hold; wall-time aging handles decay.
            return None;
        }
        self.live.aged_at = close_t;

        if self.live.win_util_n > 0 {
            let cpu = self.live.win_cpu_sum as f64 / self.live.win_util_n as f64;
            let dem = self.live.win_util_sum as f64 / self.live.win_util_n as f64;
            let agg = self.live.win_util_sum as f64;
            if self.live.util_init {
                self.live.cpu_util_ewma += (cpu - self.live.cpu_util_ewma) * alpha;
                self.live.demand_ewma += (dem - self.live.demand_ewma) * alpha;
                self.live.agg_demand_ewma += (agg - self.live.agg_demand_ewma) * alpha;
            } else {
                self.live.cpu_util_ewma = cpu;
                self.live.demand_ewma = dem;
                self.live.agg_demand_ewma = agg;
                self.live.util_init = true;
            }
        }
        self.live.starved = self.live.demand_ewma > cfg.starved_min_demand as f64
            && self.live.cpu_util_ewma < cfg.starved_ratio * self.live.demand_ewma;

        if self.durable.sample_count_ewma == 0.0 {
            self.durable.sample_count_ewma = self.live.win_n as f64;
        } else {
            self.durable.sample_count_ewma +=
                (self.live.win_n as f64 - self.durable.sample_count_ewma) * alpha;
        }
        let score_mean = if self.live.win_score_n > 0 {
            let mean = self.live.win_score_sum as f64 / self.live.win_score_n as f64;
            if self.live.score_init {
                self.durable.score_ema += (mean - self.durable.score_ema) * alpha;
            } else {
                self.durable.score_ema = mean;
                self.live.score_init = true;
            }
            Some(mean)
        } else {
            None
        };

        // Shadow mode divides the window's mean burst and mean hop rate
        // here — only when enabled, so the off path costs the (integer)
        // accumulation in `absorb` and nothing else.
        let lean_window = if cfg.shadow && self.live.win_lean_n > 0 {
            let n = self.live.win_lean_n;
            let window = (self.live.win_burst_sum_ns / n, self.live.win_hop_sum / n);
            // Persisted so the verdict can be replayed offline (ADR 0003);
            // the recorded population is the only evidence the gate accepts.
            self.durable.burst_ns = window.0;
            self.durable.hop_hz = window.1;
            self.durable.lean_at = close_t;
            Some(window)
        } else {
            None
        };
        let mut shadow = None;

        if let (Some(mean), Some(th)) = (score_mean, threshold) {
            let d = cfg.deadband_pct as f64 / 100.0;
            let low = th * (1.0 - d);
            let high = th * (1.0 + d);
            let cand = if mean < low {
                Some(BiasClass::Bulk)
            } else if mean > high {
                Some(BiasClass::Responsive)
            } else {
                None
            };
            // The comparison is against `cand` — the verdict this window is
            // handing to the hysteresis rule — not against the class that
            // survives it: shadow measures the decision, not the streak.
            if let Some((burst_ns, hop_hz)) = lean_window {
                let boundary = cfg.boundary();
                let value = classify::lean(burst_ns, hop_hz, &boundary);
                shadow = Some(ShadowVerdict {
                    pop: cand,
                    lean: classify::classify(burst_ns, hop_hz, &boundary),
                    lean_value: value,
                    burst_ns,
                    hop_hz,
                });
            }
            // Decisive evidence sits outside the actuator-swing envelope
            // (2x the deadband, well past the ~11% self-induced swing the
            // deadband was sized for): classify on ONE window and a quarter
            // of the samples, so short-lived bursts (build phases) are not
            // starved by the two-window confirmation.
            let decisive = mean < th * (1.0 - 2.0 * d) || mean > th * (1.0 + 2.0 * d);
            let need = if decisive {
                (cfg.min_samples / 4).max(4)
            } else {
                cfg.min_samples
            };
            let need_streak = if decisive { 1 } else { HYSTERESIS_STREAK };
            if self.live.win_score_n >= need {
                match cand {
                    Some(c) => {
                        if self.live.pending == Some(c) {
                            self.live.streak += 1;
                        } else {
                            self.live.pending = Some(c);
                            self.live.streak = 1;
                        }
                        if self.live.streak >= need_streak {
                            self.durable.class = c;
                            self.live.pending = None;
                            self.live.streak = 0;
                        }
                        self.durable.confidence += (1.0 - self.durable.confidence) * 0.5;
                        self.live.relax_watermark =
                            self.live.relax_watermark.max(self.durable.confidence);
                    }
                    None => {
                        // Deadband: class holds, but the bias relaxes toward 0
                        // through the release rule (target 0).
                        self.live.pending = None;
                        self.live.streak = 0;
                    }
                }
                self.live.target = if cand.is_some() {
                    target_for(cfg, self.durable.class, self.live.starved)
                } else {
                    0
                };
            } else {
                // Observed but too sparse to classify (samples present, too
                // few scored): presence is never counter-evidence — hold the
                // class AND the confidence. Only absence decays confidence
                // (wall-time aging below). Churny comms (a build's
                // cc1plus→lto1 handoff) never accumulate a full window and
                // used to spiral confidence to zero mid-activity, flushing a
                // correct bias (measured: 0.488 → 0.007 over 6 min of
                // active building).
                self.live.pending = None;
                self.live.streak = 0;
            }
        } else if self.live.win_score_n < cfg.min_samples {
            // Observed but unscored (or no population threshold yet):
            // presence, not evidence against — hold as above.
            self.live.pending = None;
            self.live.streak = 0;
        }
        self.reset_window();
        shadow
    }

    fn release_ready(&self, cfg: &BiasConfig, now: u64) -> bool {
        self.live.want_since_release.is_some_and(|w| {
            now >= w.saturating_add(cfg.release_step_s)
                && self.live.last_step_release.is_none_or(|l| {
                    now >= l.saturating_add(cfg.release_step_s)
                })
        })
    }

    fn attack_ready(&self, cfg: &BiasConfig, now: u64) -> bool {
        // No evidence, no attack. A confidence-0 entry (aged out, or restored
        // from a zero-confidence state) has nothing backing a ramp, and the
        // aging pass would flush the step in the same iteration — it only
        // spends the shared global step and re-arms the FIFO (AGENTS.md, "Open
        // finding — no-op attack cycle").
        if self.durable.confidence <= 0.0 {
            return false;
        }
        // Sustained load, not a rank on the score axis: the bias deprioritizes
        // throughput work, and a name that used a few ms of CPU in the whole
        // window is not throughput work whatever its score says. Measured live
        // 2026-11-11: `ls`, `date`, `cut`, `chown`, `env` and `gettext` ramped
        // to +1 and burned the shared step budget while `rustc` sat at 0.
        //
        // Only steps that LOWER a name's priority are gated. `target < 0` is
        // the starved-RESPONSIVE move (raise priority for a task that is being
        // starved of CPU), which must never be blocked: it is a correction in
        // the harmless direction, and gating it would silently disable
        // `bias_starved_min_demand` for every demand below this floor.
        //
        // The measure is the name's AGGREGATE demand across the window, not
        // the per-row mean: a `make -j24` of 80 ms `cc1plus` instances leaves
        // each row at a low duty cycle (they are short-lived, not idle), so a
        // mean-based floor would declare the canonical bulk workload
        // permanently unattackable.
        if self.live.target > 0 && self.live.agg_demand_ewma < cfg.attack_min_util as f64 {
            return false;
        }
        self.live.want_since_attack.is_some_and(|w| {
            now >= w.saturating_add(cfg.attack_step_s)
                && self.live.last_step_attack.is_none_or(|l| {
                    now >= l.saturating_add(cfg.attack_step_s)
                })
        })
    }
}

/// Per-name estimator + controller with injectable time.
#[derive(Debug)]
pub struct BiasEngine {
    cfg: BiasConfig,
    /// Fuzzy families (matched first; members pool into one entry).
    groups: Vec<BiasGroup>,
    entries: BTreeMap<String, Entry>,
    /// Population threshold T_us (EWMA of mean score), if known.
    threshold: Option<f64>,
    window_end: Option<u64>,
    pop_score_sum: u64,
    pop_score_n: u64,
    /// Names with samples in the current window (fast empty-window close).
    win_names: u64,
    last_global_step: Option<u64>,
    /// Whether the last global step went to a release (fairness cap: never
    /// two release wins in a row while an attack waits — see `slew_pass`).
    last_step_was_release: bool,
    last_now: u64,
    /// Shadow-mode counters (cumulative since construction/restore):
    /// name-windows with a lean verdict, both sides disagreeing, exactly one
    /// side having a verdict. Never persisted, never read by the controller.
    shadow_windows: u64,
    shadow_disagreements: u64,
    shadow_unilateral: u64,
    /// When the cumulative summary was last emitted (`SHADOW_LOG_S` throttle).
    shadow_summary_at: Option<u64>,
    /// Boundary of the most recent capture window (shadow mode). Kept even
    /// after `bias_shadow` is switched off, so a capture taken before the
    /// switch is still replayable; a boundary change clears it together with
    /// the entries' inputs (they measured a different classifier).
    capture: Option<crate::bias_state::LeanCapture>,
}

impl BiasEngine {
    pub fn new(cfg: BiasConfig) -> Self {
        Self::new_with_groups(cfg, Vec::new())
    }

    /// `groups` are fuzzy families (see [`entry_key`]): their members pool
    /// samples into one entry and share its learned bias.
    pub fn new_with_groups(mut cfg: BiasConfig, groups: Vec<BiasGroup>) -> Self {
        // Library-level invariant: i8::clamp panics on an inverted range.
        // Config::parse already warns and resets; normalize here too so any
        // other caller is safe.
        if cfg.min_bias > cfg.max_bias {
            std::mem::swap(&mut cfg.min_bias, &mut cfg.max_bias);
        }
        BiasEngine {
            cfg,
            groups,
            entries: BTreeMap::new(),
            threshold: None,
            window_end: None,
            pop_score_sum: 0,
            pop_score_n: 0,
            win_names: 0,
            last_global_step: None,
            last_step_was_release: false,
            last_now: 0,
            shadow_windows: 0,
            shadow_disagreements: 0,
            shadow_unilateral: 0,
            shadow_summary_at: None,
            capture: None,
        }
    }

    /// One delivered window. Verifies the closed loop against every row
    /// first — absorption skips frozen entries, so a row that re-baselines one
    /// counts as evidence in *this* batch — then absorbs it and lets the
    /// controller step. Wall-clock aging is [`Self::tick`]'s job; the loop
    /// owes one of those per iteration, batch or not (see `bias_pass`).
    pub fn ingest(&mut self, batch: &[Sample], now: SystemTime) -> Vec<Change> {
        let t = unix_secs(now);
        for s in batch {
            self.verify_row(&s.comm, s.static_prio, t);
        }
        self.absorb(batch, t);
        self.enforce_cap();
        self.slew_pass(t, true);
        self.drain_changes()
    }

    /// Wall-clock aging since the last pass: confidence decays, bias relaxes
    /// toward 0, nothing attacks. Runs whether or not a batch arrived — a
    /// batch cadence (500 ms) is shorter than the loop's idle timeout (1 s),
    /// so a healthy stream would otherwise never age (measured live: a
    /// vanished name held `confidence 0.994599` unchanged across 145 s).
    pub fn tick(&mut self, now: SystemTime) -> Vec<Change> {
        let t = unix_secs(now);
        self.last_now = self.last_now.max(t);
        self.age_entries(t);
        self.slew_pass(t, false);
        self.enforce_cap();
        self.drain_changes()
    }

    /// Wall-time decay for every entry, seen or not: confidence halves every
    /// `half_life_s` since its anchor, and zero confidence flushes the bias.
    fn age_entries(&mut self, t: u64) {
        let hl = self.cfg.half_life_s.max(1) as f64;
        for e in self.entries.values_mut() {
            let base = e.live.aged_at.max(e.durable.last_seen);
            let dt = t.saturating_sub(base);
            if dt > 0 {
                e.durable.confidence *= 0.5f64.powf(dt as f64 / hl);
                e.live.aged_at = t;
            }
            if e.durable.confidence < CONFIDENCE_EPS {
                e.durable.confidence = 0.0;
            }
            if e.durable.confidence == 0.0 && e.durable.bias != 0 {
                // History must never permanently fix the bias.
                let prev = e.durable.bias;
                e.durable.bias = 0;
                e.live.relax_watermark = 0.0;
                e.live.want_since_attack = None;
                e.live.attack_want_idle_since = None;
                e.live.want_since_release = None;
                e.live.changed = Some((prev, Cause::Flush));
            }
        }
    }

    /// Advance the window clock to `t`, close every window that has ended, and
    /// accumulate this batch into the window still open.
    fn absorb(&mut self, batch: &[Sample], t: u64) {
        self.last_now = self.last_now.max(t);
        if self.window_end.is_none() {
            self.window_end = Some(t.saturating_add(self.cfg.window_s.max(1)));
        }
        let mut end = self.window_end.expect("initialized above");
        while t >= end {
            self.close_window(end);
            end = end.saturating_add(self.cfg.window_s.max(1));
        }
        self.window_end = Some(end);

        for s in batch {
            // Kernel threads are out of scope for the whole engine: nice on a
            // kthread is meaningless and, because the step budget is global,
            // every kthread entry that wants a ramp delays the workloads the
            // bias exists for (measured live 2026-11-11: `kthreadd` and
            // `ksoftirqd/12` held +3 while `cargo` sat at 0). Skipped before
            // the key is derived, so they also stay out of the population
            // mean `T` that classifies everything else.
            if s.is_kernel_thread() {
                continue;
            }
            // Fuzzy family key: suffix families pool (kworker/u97:*), and
            // declared bias_group patterns pool semantic families (cc1*/lto1).
            let key = entry_key(&s.comm, &self.groups).into_owned();
            let e = self.entries.entry(key).or_insert_with(|| Entry::new(t));
            // Frozen by closed-loop mismatch: no evidence updates until a
            // matching row re-baselines it (`verify_row`, before this call);
            // aging still relaxes the stored bias toward 0 meanwhile.
            if e.durable.frozen {
                continue;
            }
            if e.live.win_n == 0 {
                self.win_names += 1;
            }
            e.durable.last_seen = t;
            e.live.win_n += 1;
            e.live.win_cpu_sum = e.live.win_cpu_sum.saturating_add(s.cpu_util_wall);
            e.live.win_util_sum = e.live.win_util_sum.saturating_add(s.task_util_est);
            e.live.win_util_n += 1;
            // Lean axes: same evidence rule as `sample_score` — a run of zero
            // length says nothing, so a row that never ran is not a row that
            // ran for 0 ns.
            if s.avg_runtime_wall > 0 {
                e.live.win_burst_sum_ns =
                    e.live.win_burst_sum_ns.saturating_add(s.avg_runtime_wall);
                e.live.win_hop_sum = e.live.win_hop_sum.saturating_add(s.wait_freq);
                e.live.win_lean_n += 1;
            }
            if let Some(score) = sample_score(s) {
                e.live.win_score_sum = e.live.win_score_sum.saturating_add(score);
                e.live.win_score_n += 1;
                self.pop_score_sum = self.pop_score_sum.saturating_add(score);
                self.pop_score_n += 1;
            }
        }
    }

    /// Applied nice = `clamp(base_nice + bias, -20, 19)`, recomputed from the
    /// base every call (idempotent — never increments on itself).
    pub fn target_nice(&self, name: &str, base_nice: i8) -> i8 {
        let key = entry_key(name, &self.groups);
        let bias = self
            .entries
            .get(key.as_ref())
            .map_or(0, |e| e.durable.bias);
        (i16::from(base_nice) + i16::from(bias)).clamp(-20, 19) as i8
    }

    /// Closed-loop check for one row against the nice this daemon last applied
    /// for `name`. Observed nice = `observed_static_prio - 20` (the sampler's
    /// field is nice + 20). Semantics:
    /// - names we never applied are not verified (their ambient nice is not
    ///   ours to compare against), and unknown names are ignored;
    /// - `static_prio == 0` (nice −20) is indistinguishable from the lenient
    ///   default of an absent field — skip rather than fabricate a mismatch;
    /// - mismatch freezes the entry (no evidence updates); a later matching
    ///   row re-baselines it (unfreeze, confidence to low). A persistent
    ///   mismatch therefore cannot pin the bias: aging still flushes it.
    fn verify_row(&mut self, name: &str, observed_static_prio: u64, now: u64) {
        if observed_static_prio == 0 {
            return;
        }
        let Self { entries, groups, .. } = self;
        let key = entry_key(name, groups);
        let Some(e) = entries.get_mut(key.as_ref()) else {
            return;
        };
        let Some(expected_nice) = e.live.applied_nice else {
            // No actuation on record at all (a restart: the expectation is
            // not persisted). No observation can ever "match", so the
            // freeze has nothing left to protect — a fresh sample
            // re-baselines the entry instead of bricking it forever. A
            // RELEASED claim (explicit `None`: the apply did not take) is
            // an authoritative record and must not unwind the freeze.
            if e.durable.frozen && !e.live.claim_recorded {
                e.rebaseline(now);
            }
            return;
        };
        let observed_nice = observed_static_prio as i64 - 20;
        if observed_nice == i64::from(expected_nice) {
            if e.durable.frozen {
                e.rebaseline(now);
            }
        } else if !e.durable.frozen {
            e.durable.frozen = true;
        }
    }

    /// Record the nice this daemon applied for `name` (`None` when the apply
    /// did not take, e.g. Denied/Failed — then verification is skipped).
    pub fn note_applied(&mut self, name: &str, nice: Option<i8>) {
        let Self {
            entries, groups, ..
        } = self;
        let key = entry_key(name, groups);
        if let Some(e) = entries.get_mut(key.as_ref()) {
            e.live.applied_nice = nice;
            e.live.claim_recorded = true;
        }
    }

    /// The nice this daemon last applied for `name`, if any.
    pub fn applied_nice(&self, name: &str) -> Option<i8> {
        let key = entry_key(name, &self.groups);
        self.entries
            .get(key.as_ref())
            .and_then(|e| e.live.applied_nice)
    }

    /// Serializable state for persistence (see `bias_state::save`).
    pub fn snapshot(&self) -> BiasSnapshot {
        // The boundary the capture was taken with, not the one currently
        // configured: `bias_lean_*` are live keys, and turning the shadow off
        // must not discard a capture that already exists. Cleared when no
        // entry carries inputs any more (or when the boundary changed).
        let lean_capture = self
            .capture
            .filter(|_| self.entries.values().any(|e| e.durable.lean_at > 0));
        BiasSnapshot {
            version: BIAS_STATE_VERSION,
            threshold_ema: self.threshold,
            lean_capture,
            entries: self
                .entries
                .iter()
                .map(|(name, e)| EntrySnapshot {
                    name: name.clone(),
                    durable: e.durable.clone(),
                })
                .collect(),
        }
    }

    /// Restore persisted state (startup). Everything transient is rebuilt by
    /// [`Live::restore`]; aging resumes from each entry's `last_seen`, and
    /// frozen entries stay frozen until a matching row re-baselines them.
    pub fn restore(&mut self, snapshot: BiasSnapshot) {
        self.threshold = snapshot.threshold_ema;
        for es in snapshot.entries {
            let durable = Durable {
                confidence: es.durable.confidence.clamp(0.0, 1.0),
                bias: es.durable.bias.clamp(self.cfg.min_bias, self.cfg.max_bias),
                ..es.durable
            };
            let live = Live::restore(&durable, &self.cfg);
            self.entries.insert(es.name, Entry { durable, live });
        }
        self.last_now = self
            .entries
            .values()
            .map(|e| e.durable.last_seen)
            .max()
            .unwrap_or(0);
        self.enforce_cap();
    }

    /// The effective knobs: defaults at construction, then whatever the last
    /// [`Self::reconfigure`] installed.
    pub fn config(&self) -> &BiasConfig {
        &self.cfg
    }

    /// Adopt new configuration without dropping learned history (`--reload`).
    ///
    /// Preserved: `threshold`, every entry's `score_ema`/`sample_count_ewma`/
    /// class/confidence/frozen/applied claim, the in-flight `window_end` and
    /// window accumulators, the want stamps, the global-step budget and the
    /// release/attack rate laws. These are evidence and rate state, not
    /// configuration; resetting them would replay the restart pathologies the
    /// slew law was measured against.
    ///
    /// Recomputed:
    /// * `bias` is clamped into the new `[min_bias, max_bias]` (a shrink of
    ///   `bias_max` must take effect now, not at the next step);
    /// * `target` is re-derived from the entry's class and its current
    ///   `starved` flag, so a new clamp range reaches the controller;
    /// * entries are re-keyed through [`entry_key`] under the new `groups`
    ///   (a changed `bias_group.<tag>` changes history keys): collisions merge
    ///   per [`merge_entries`], and the entry cap is re-enforced.
    pub fn reconfigure(&mut self, mut cfg: BiasConfig, groups: Vec<BiasGroup>) {
        // Same library-level invariant as `new_with_groups`: i8::clamp panics
        // on an inverted range, and a reload is not allowed to take the bias
        // thread down.
        if cfg.min_bias > cfg.max_bias {
            std::mem::swap(&mut cfg.min_bias, &mut cfg.max_bias);
        }
        self.groups = groups;
        // A different boundary makes every recorded lean input describe a
        // classifier this daemon no longer runs; the capture restarts.
        let boundary_changed = self.cfg.boundary() != cfg.boundary();
        if boundary_changed {
            self.capture = None;
        }

        let mut rekeyed: BTreeMap<String, Entry> = BTreeMap::new();
        for (name, mut e) in std::mem::take(&mut self.entries) {
            if boundary_changed {
                e.durable.burst_ns = 0;
                e.durable.hop_hz = 0;
                e.durable.lean_at = 0;
            }
            let old_bias = e.durable.bias;
            e.durable.bias = e.durable.bias.clamp(cfg.min_bias, cfg.max_bias);
            e.live.target = target_for(&cfg, e.durable.class, e.live.starved);
            if e.durable.bias != old_bias {
                // Report a clamp through the normal "bias changed" path. A
                // reload is not evidence, so it reports as a relax move.
                e.live.changed = Some((old_bias, Cause::Relax));
            }
            let key = entry_key(&name, &self.groups).into_owned();
            match rekeyed.entry(key) {
                MapEntry::Vacant(slot) => {
                    slot.insert(e);
                }
                MapEntry::Occupied(mut slot) => merge_entries(slot.get_mut(), e),
            }
        }
        self.entries = rekeyed;
        self.cfg = cfg;
        self.enforce_cap();
    }

    /// Moves since the last call, each reported exactly once. Marks survive
    /// until reported, so a move that happened while the stream was down is
    /// logged at the next pass rather than lost.
    fn drain_changes(&mut self) -> Vec<Change> {
        let mut out = Vec::new();
        for (name, e) in self.entries.iter_mut() {
            let Some((prev, cause)) = e.live.changed.take() else {
                continue;
            };
            out.push(Change {
                name: name.clone(),
                prev,
                bias: e.durable.bias,
                class: e.durable.class,
                cause,
            });
        }
        out
    }

    fn close_window(&mut self, close_t: u64) {
        let alpha = 1.0 - (-(self.cfg.window_s.max(1) as f64) / THRESHOLD_TC_S).exp();
        let (mut windows, mut disagreements, mut unilateral) = (0u64, 0u64, 0u64);
        if self.pop_score_n > 0 || self.win_names > 0 {
            if self.pop_score_n > 0 {
                let mean = self.pop_score_sum as f64 / self.pop_score_n as f64;
                self.threshold = Some(match self.threshold {
                    Some(th) => th + (mean - th) * alpha,
                    None => mean,
                });
            }
            let threshold = self.threshold;
            for (name, e) in self.entries.iter_mut() {
                let Some(v) = e.close_window(&self.cfg, threshold, alpha, close_t) else {
                    continue;
                };
                // The shadow verdict exists only when `bias_shadow` is on
                // and the window carries lean evidence, so nothing below
                // runs (or allocates, or logs) on the default path.
                let pop = v.pop.and_then(class_to_lean);
                if v.lean.is_some() {
                    windows += 1;
                }
                let both = pop.zip(v.lean);
                if both.is_some_and(|(a, b)| a != b) {
                    disagreements += 1;
                } else if both.is_none() && (pop.is_some() || v.lean.is_some()) {
                    unilateral += 1;
                }
                if pop == v.lean {
                    continue;
                }
                // One line per name per SHADOW_LOG_S: a disagreement is a
                // permanent property of a workload on this host, so repeating
                // it every window would drown the journal.
                let throttled = e
                    .live
                    .shadow_logged_at
                    .is_some_and(|t| close_t < t.saturating_add(SHADOW_LOG_S));
                if !throttled {
                    e.live.shadow_logged_at = Some(close_t);
                    tracing::info!(
                        name = %name,
                        old = ?v.pop,
                        new = ?v.lean,
                        lean = ?v.lean_value,
                        burst_ns = v.burst_ns,
                        hop_hz = v.hop_hz,
                        "bias shadow"
                    );
                }
            }
        }
        if self.cfg.shadow {
            let b = self.cfg.boundary();
            self.capture = Some(crate::bias_state::LeanCapture {
                stage_min_ns: b.stage_min_ns,
                hop_min_hz: b.hop_min_hz,
                deadband_pct: b.deadband_pct,
                saved_at: close_t,
            });
        }
        self.pop_score_sum = 0;
        self.pop_score_n = 0;
        self.win_names = 0;
        self.shadow_windows += windows;
        self.shadow_disagreements += disagreements;
        self.shadow_unilateral += unilateral;
        let summary_due = self
            .shadow_summary_at
            .is_none_or(|t| close_t >= t.saturating_add(SHADOW_LOG_S));
        if self.cfg.shadow && self.shadow_windows > 0 && summary_due {
            self.shadow_summary_at = Some(close_t);
            tracing::info!(
                windows = self.shadow_windows,
                disagreements = self.shadow_disagreements,
                unilateral = self.shadow_unilateral,
                "bias shadow summary"
            );
        }
    }

    /// Cumulative shadow-mode counters: name-windows with a lean verdict,
    /// both-sides-different verdicts, and exactly-one-side verdicts. All zero
    /// when shadow mode was never enabled. Observation only.
    pub fn shadow_stats(&self) -> (u64, u64, u64) {
        (
            self.shadow_windows,
            self.shadow_disagreements,
            self.shadow_unilateral,
        )
    }

    /// Apply slew limits. `allow_attack = false` (aging) only releases.
    /// The shared global budget buys at most one step per pass; the winner
    /// is chosen FIFO by want-onset within each direction (release outranks
    /// attack, name breaks ties) and re-arms its want after stepping, so a
    /// tight lexicographic order can never starve later names. Fairness
    /// cap: never two release wins in a row while an attack waits — the
    /// restored release cohort (every want stamps at restart and matures
    /// together at +release_step_s) would otherwise monopolize the single
    /// global step and starve every ramp. Deterministic given (samples,
    /// timestamps).
    fn slew_pass(&mut self, now: u64, allow_attack: bool) {
        for e in self.entries.values_mut() {
            if e.durable.bias == 0 && e.live.relax_watermark - e.durable.confidence >= RELAX_STEP {
                e.live.relax_watermark = e.durable.confidence; // nothing left to relax
            }
            let (attack_want, release_want, _) = direction_wants(e);
            e.live.want_since_release = want_stamp(e.live.want_since_release, release_want, now);
            // The attack desire survives short deadband excursions: a
            // deadband window flips target to 0 but is not counter-evidence
            // (hysteresis holds the class), so clearing the want onset on
            // every flip would re-arm the 30 s attack timer and reset FIFO
            // priority each window — the measured ramp starvation (bias
            // frozen at +1 through a whole build). A want that stays false
            // for a full hysteresis confirmation (2 windows) is abandoned.
            if attack_want {
                e.live.want_since_attack = want_stamp(e.live.want_since_attack, true, now);
                e.live.attack_want_idle_since = None;
            } else if e.live.want_since_attack.is_some() {
                let idle_since = *e.live.attack_want_idle_since.get_or_insert(now);
                if now.saturating_sub(idle_since)
                    >= u64::from(HYSTERESIS_STREAK) * self.cfg.window_s.max(1)
                {
                    e.live.want_since_attack = None;
                    e.live.attack_want_idle_since = None;
                }
            } else {
                e.live.attack_want_idle_since = None;
            }
        }

        let budget_used = self
            .last_global_step
            .is_some_and(|g| now < g.saturating_add(self.cfg.global_step_s));
        if budget_used {
            return;
        }

        // (want-onset, name): FIFO within each direction.
        let mut best_release: Option<(u64, String)> = None;
        let mut best_attack: Option<(u64, String)> = None;
        for (name, e) in &self.entries {
            let (attack_want, release_want, _) = direction_wants(e);
            if release_want && e.release_ready(&self.cfg, now) {
                let key = (e.live.want_since_release.unwrap_or(now), name.clone());
                if best_release.as_ref().is_none_or(|b| key < *b) {
                    best_release = Some(key);
                }
            }
            if allow_attack && attack_want && e.attack_ready(&self.cfg, now) {
                let key = (e.live.want_since_attack.unwrap_or(now), name.clone());
                if best_attack.as_ref().is_none_or(|b| key < *b) {
                    best_attack = Some(key);
                }
            }
        }

        let attack_wins =
            best_attack.is_some() && (best_release.is_none() || self.last_step_was_release);
        let winner = if attack_wins {
            best_attack.map(|(_, n)| (n, false))
        } else {
            best_release
                .map(|(_, n)| (n, true))
                .or_else(|| best_attack.map(|(_, n)| (n, false)))
        };

        if let Some((name, is_release)) = winner {
            let e = self.entries.get_mut(&name).expect("selected entry exists");
            let prev = e.durable.bias;
            if is_release {
                e.durable.bias -= e.durable.bias.signum();
                e.live.last_step_release = Some(now);
                e.live.want_since_release = Some(now); // re-arm: back of the FIFO
                e.live.relax_watermark = e.durable.confidence;
            } else {
                let delta = i16::from(e.live.target) - i16::from(e.durable.bias);
                e.durable.bias = (e.durable.bias + delta.signum() as i8)
                    .clamp(self.cfg.min_bias, self.cfg.max_bias);
                e.live.last_step_attack = Some(now);
                e.live.want_since_attack = Some(now); // re-arm: back of the FIFO
            }
            let cause = if is_release {
                Cause::Relax
            } else {
                Cause::Evidence
            };
            e.live.changed = Some((prev, cause));
            self.last_global_step = Some(now);
            self.last_step_was_release = is_release;
        }
    }

    fn enforce_cap(&mut self) {
        let cap = self.cfg.max_entries;
        if self.entries.len() <= cap {
            return;
        }
        let mut by_age: Vec<(u64, String)> = self
            .entries
            .iter()
            .map(|(name, e)| (e.durable.last_seen, name.clone()))
            .collect();
        by_age.sort(); // (last_seen, name): deterministic LRU order
        let drop = self.entries.len() - cap;
        for (_, name) in by_age.into_iter().take(drop) {
            self.entries.remove(&name);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sampler::Batch;
    use std::time::Duration;

    /// Arbitrary unix-seconds base; every timestamp below is an offset from it.
    const T0: u64 = 1_000_000;

    fn ts(t: u64) -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(T0 + t)
    }

    fn sample(comm: &str, wait: u64, wake: u64, rt: u64, util: u64, cpu: u64) -> Sample {
        Sample {
            pid: 1,
            comm: comm.to_string(),
            wait_freq: wait,
            wake_freq: wake,
            avg_runtime_wall: rt,
            task_util_est: util,
            cpu_util_wall: cpu,
            static_prio: 20,
            // The score path ignores the raw counts; the shadow path keeps
            // using the per-sample means (see classify::lean_of_sample).
            ..Sample::default()
        }
    }

    // Hand-computed score fixtures (see score_fixtures_and_skip). The
    // classification envelope around the 400-score population mean T is
    //   0.85..1.15 × T  deadband — holds the class
    //   beyond 0.70..1.30 × T     — decisive: one window, min_samples/4 (≥ 4)
    // so the bulk/resp fixtures sit deliberately INSIDE the 1×..2× deadband
    // band (non-decisive: the two-window confirmation path is what they
    // exercise) while the `decisive_*` fixtures sit far outside it:
    //   400 = 20²  : ilog2(1024·1024) = 20, runtime_ft = 1   (the population)
    //   289 = 17²  : 10 + ilog2(200) = 17                    (bulk, ~0.80 × T)
    //   324 = 18²  : 10 + ilog2(500) = 18                    (deadband low
    //                edge — borderline just INSIDE 0.85 × T, flips cand)
    //   576 = 24²  : 20 + ilog2(25) = 24                     (resp, ~1.25 × T)
    //   100 = 10²  : ilog2(32·32) = 10, runtime_ft = 1       (decisive bulk)
    //   784 = 28²  : 20 + ilog2(256) = 28                    (decisive resp)
    fn noise_batch(n: usize) -> Batch {
        (0..n)
            .map(|_| sample("noise", 1023, 1023, 1_000_000_000, 500, 500))
            .collect()
    }
    fn bulk_batch(name: &str, n: usize) -> Batch {
        (0..n)
            .map(|_| sample(name, 31, 31, 5_000_000, 500, 500))
            .collect()
    }
    fn resp_batch(name: &str, n: usize) -> Batch {
        (0..n)
            .map(|_| sample(name, 1023, 1023, 40_000_000, 500, 500))
            .collect()
    }
    fn decisive_bulk_batch(name: &str, n: usize) -> Batch {
        (0..n)
            .map(|_| sample(name, 31, 31, 1_000_000_000, 500, 500))
            .collect()
    }
    fn starved_batch(name: &str, n: usize) -> Batch {
        (0..n)
            .map(|_| sample(name, 1023, 1023, 3_906_250, 1000, 100))
            .collect()
    }
    fn tiny_demand_batch(name: &str, n: usize) -> Batch {
        (0..n)
            .map(|_| sample(name, 1023, 1023, 3_906_250, 10, 0))
            .collect()
    }
    fn dead_batch(name: &str, n: usize) -> Batch {
        (0..n)
            .map(|_| sample(name, 1023, 1023, 1_000_000_000, 500, 500))
            .collect()
    }
    fn borderline_batch(name: &str, n: usize) -> Batch {
        (0..n)
            .map(|_| sample(name, 31, 31, 2_000_000, 500, 500))
            .collect()
    }

    // --- shadow fixtures ---------------------------------------------------
    // A stable background is mandatory (AGENTS.md: a threshold that is the
    // mean of the population being classified deadbands whenever one name
    // dominates). `shadow_noise_batch` scores 841 = 29² and leans Bulk
    // (+0.61), and it keeps T at 790 for every window.
    fn shadow_noise_batch(n: usize) -> Batch {
        (0..n)
            .map(|_| sample("noise", 1023, 1023, 1_000_000, 500, 500))
            .collect()
    }

    /// Score 484 = 22² — 0.61 × T, so the population boundary calls it BULK
    /// (decisively), while 100 µs bursts at 1 kHz lean −2.68: RESPONSIVE.
    fn short_hop_batch(name: &str, n: usize) -> Batch {
        (0..n)
            .map(|_| sample(name, 1_000, 0, 100_000, 500, 500))
            .collect()
    }

    /// One scripted window: a stable population plus the disagreeing target.
    /// Population mean = (60·841 + 10·484) / 70 = 790, so T is 790 and the
    /// background stays inside its deadband (841 < 1.15 × 790 = 908.5).
    fn shadow_window() -> Batch {
        let mut w = shadow_noise_batch(60);
        w.extend(short_hop_batch("app", 10));
        w
    }

    /// The persisted half of one entry (what a restart keeps).
    fn snap_entry(eng: &BiasEngine, name: &str) -> Durable {
        eng.snapshot()
            .entries
            .into_iter()
            .find(|e| e.name == name)
            .expect("entry present")
            .durable
    }

    /// One row for `comm` carrying an explicit closed-loop observation.
    fn at_prio(comm: &str, static_prio: u64) -> Sample {
        Sample {
            static_prio,
            ..sample(comm, 31, 31, 5_000_000, 500, 500)
        }
    }

    fn change(name: &str, prev: i8, bias: i8, class: BiasClass, cause: Cause) -> Change {
        Change {
            name: name.to_string(),
            prev,
            bias,
            class,
            cause,
        }
    }

    /// Two classifying windows of `mk` samples against a 400-score population
    /// baseline. Closes at t = 61 (streak 1) and t = 121 (class settles);
    /// the slew pass of the t = 121 call arms the wants.
    fn drive_class(eng: &mut BiasEngine, names: &[&str], mk: fn(&str, usize) -> Batch) {
        let mut w1 = noise_batch(60);
        let mut w2 = noise_batch(60);
        for n in names {
            w1.extend(mk(n, 32));
            w2.extend(mk(n, 32));
        }
        eng.ingest(&w1, ts(1));
        eng.ingest(&w2, ts(61));
        eng.ingest(&[], ts(121));
    }

    fn drive_bulk(eng: &mut BiasEngine, names: &[&str]) {
        drive_class(eng, names, bulk_batch);
    }

    /// Three attack steps at want+60/120/180: bias 0 → 3.
    fn ramp_attack(eng: &mut BiasEngine) {
        eng.ingest(&[], ts(181));
        eng.ingest(&[], ts(241));
        eng.ingest(&[], ts(301));
    }

    #[test]
    fn score_fixtures_and_skip() {
        assert_eq!(
            sample_score(&sample("x", 1023, 1023, 1_000_000_000, 0, 0)),
            Some(400)
        );
        assert_eq!(
            sample_score(&sample("x", 31, 31, 1_000_000_000, 0, 0)),
            Some(100)
        );
        assert_eq!(
            sample_score(&sample("x", 1023, 1023, 3_906_250, 0, 0)),
            Some(784)
        );
        // freqs clamp at 100_000: ilog2(100001²) = 33, runtime_ft = 1 → 33²
        assert_eq!(
            sample_score(&sample("x", 200_000, 200_000, 1_000_000_000, 0, 0)),
            Some(1089)
        );
        // A never-sleeping task that wakes nobody is still evidence: both
        // frequencies are meaningful zeros, so the score is the runtime term
        // alone. wait_ft = 1, wake_ft = 1, runtime_ft = 1 -> log2 sum 0.
        assert_eq!(
            sample_score(&sample("x", 0, 0, 1_000_000_000, 0, 0)),
            Some(0)
        );
        // wait_ft = 1, wake_ft = 101 -> ilog2 = 6, runtime_ft = 1
        assert_eq!(
            sample_score(&sample("x", 0, 100, 1_000_000_000, 0, 0)),
            Some(36)
        );
        assert_eq!(
            sample_score(&sample("x", 100, 0, 1_000_000_000, 0, 0)),
            Some(36)
        );
        // Never running in the window = no evidence at all.
        assert_eq!(sample_score(&sample("x", 100, 100, 0, 0, 0)), None);
    }

    /// A kernel thread is out of scope for the whole engine: it must not get
    /// an entry, must not contribute to the population mean `T`, and must not
    /// claim the shared step budget. Live 2026-11-11: `kthreadd` and
    /// `ksoftirqd/12` held +3 while `cargo` sat at 0.
    #[test]
    fn kernel_threads_never_enter_the_engine() {
        let mut eng = BiasEngine::new(BiasConfig::default());
        let kt = |name: &str| {
            let mut s = sample(name, 0, 0, 3_000_000, 900, 800);
            s.kthread = 1;
            s
        };
        // The proven two-window bulk drive (with the background population the
        // threshold needs), plus kernel-thread rows carrying plenty of
        // evidence: they must change nothing, including `T`.
        let mut w1 = noise_batch(60);
        let mut w2 = noise_batch(60);
        w1.extend(bulk_batch("cargo", 32));
        w2.extend(bulk_batch("cargo", 32));
        for w in [&mut w1, &mut w2] {
            for name in ["kworker/3", "ksoftirqd/12", "kworker/u97"] {
                w.extend(std::iter::repeat_n(kt(name), 32));
            }
        }
        eng.ingest(&w1, ts(1));
        eng.ingest(&w2, ts(61));
        eng.ingest(&[], ts(121));
        ramp_attack(&mut eng);
        let snap = eng.snapshot();
        let names: Vec<&str> = snap.entries.iter().map(|e| e.name.as_str()).collect();
        assert!(
            !names.iter().any(|n| n.starts_with("kworker") || n.starts_with("ksoftirqd")),
            "kernel threads entered the engine: {names:?}"
        );
        // The real workload still learned: the skip must not have poisoned T.
        let cargo = snap
            .entries
            .iter()
            .find(|e| e.name == "cargo")
            .expect("cargo learned");
        assert!(
            cargo.durable.bias > 0,
            "cargo bias {} after the drive",
            cargo.durable.bias
        );
    }

    /// A name can rank BULK on the score axis and still not deserve a ramp:
    /// the bias exists to deprioritize throughput work, and a shell one-liner
    /// that used a few milliseconds of CPU in the window is not throughput
    /// work. Both names below carry identical (bulk) scores; only the one
    /// with sustained demand may attack.
    ///
    /// Measured live 2026-11-11 (before this gate): `ls`, `date`, `cut`,
    /// `chown`, `env` and `gettext` ramped to +1 and burned the shared step
    /// budget while `rustc` waited.
    #[test]
    fn a_shell_never_attacks_on_a_cpu_spike() {
        let mut eng = BiasEngine::new(BiasConfig::default());
        let mut w1 = noise_batch(60);
        let mut w2 = noise_batch(60);
        for w in [&mut w1, &mut w2] {
            // Same rates, therefore the same score; only the duty cycle differs.
            w.extend(bulk_batch("rustc", 32));
            w.extend(
                (0..32).map(|_| sample("ls", 31, 31, 5_000_000, 4, 2)),
            );
        }
        eng.ingest(&w1, ts(1));
        eng.ingest(&w2, ts(61));
        eng.ingest(&[], ts(121));

        let snap = eng.snapshot();
        for name in ["rustc", "ls"] {
            let e = snap.entries.iter().find(|e| e.name == name).expect(name);
            assert_eq!(e.durable.class, BiasClass::Bulk, "{name} class");
        }

        ramp_attack(&mut eng);
        let snap = eng.snapshot();
        let rustc = snap.entries.iter().find(|e| e.name == "rustc").expect("rustc");
        let ls = snap.entries.iter().find(|e| e.name == "ls").expect("ls");
        assert!(
            rustc.durable.bias > 0,
            "the real load must still learn (bias {})",
            rustc.durable.bias
        );
        assert_eq!(
            ls.durable.bias, 0,
            "a CPU spike spent the step budget on a shell one-liner"
        );
    }

    /// The demand gate must never block the STARVED move. A starved
    /// RESPONSIVE name gets target −1, which raises its priority — a
    /// correction in the harmless direction. Gating it would silently disable
    /// `bias_starved_min_demand` for every demand below the floor.
    #[test]
    fn the_demand_gate_never_blocks_a_starved_step() {
        // A floor nobody can reach: bulk ramps are impossible, starved steps
        // are not.
        let mut eng = BiasEngine::new(BiasConfig {
            attack_min_util: 1_000_000,
            ..BiasConfig::default()
        });
        drive_class(&mut eng, &["starved"], starved_batch);
        assert_eq!(snap_entry(&eng, "starved").class, BiasClass::Responsive);
        ramp_attack(&mut eng);
        assert_eq!(
            snap_entry(&eng, "starved").bias,
            -1,
            "the starved ramp was blocked by the demand gate"
        );
    }

    /// The gate measures AGGREGATE demand, so a parallel build of short-lived
    /// compilers still learns: each `cc1plus` row is a low duty cycle over the
    /// drain (it is short-lived, not idle), and a per-row floor would call the
    /// canonical bulk workload unattackable while passing a single long-lived
    /// `rustc`.
    #[test]
    fn a_parallel_build_of_short_instances_can_still_learn() {
        let mut eng = BiasEngine::new(BiasConfig::default());
        let mut w1 = noise_batch(60);
        let mut w2 = noise_batch(60);
        for w in [&mut w1, &mut w2] {
            // 32 instances at 14% duty each: ~4.5 cores of aggregate demand,
            // every single row far below any per-row floor.
            w.extend((0..32).map(|_| sample("cc1plus", 31, 31, 5_000_000, 143, 143)));
        }
        eng.ingest(&w1, ts(1));
        eng.ingest(&w2, ts(61));
        eng.ingest(&[], ts(121));
        let cls = snap_entry(&eng, "cc1plus").class;
        ramp_attack(&mut eng);

        assert!(
            cls == BiasClass::Bulk,
            "fixture did not classify bulk: {cls:?}"
        );
        assert!(
            snap_entry(&eng, "cc1plus").bias > 0,
            "a parallel build was starved by the demand gate (bias {})",
            snap_entry(&eng, "cc1plus").bias
        );
    }

    /// The classifier's ordering, on the contract level: a shell one-liner
    /// (short bursts, many hops) must outrank a compiler (long bursts, few
    /// hops), and each axis must carry the direction on its own.
    ///
    /// The producer, not this formula, is what inverted in v0.3.0 — the
    /// red-first regression for that lives in the collector
    /// (`agg::tests::rate_is_per_active_second_not_per_window_second`). This
    /// test pins the formula so a future change cannot quietly flip an axis
    /// and re-create the 2026-11-11 mis-targeting (`bash`/`cat`/`grep` at +3,
    /// `cargo` at 0).
    #[test]
    fn a_shell_outranks_a_compiler_on_both_axes() {
        // 50 us bursts, ~2000 sleeps/s of active time, 100 wakes/s.
        let shell = sample("ls", 0, 0, 50_000, 2_000, 100);
        // 5 ms bursts, ~20 sleeps/s, 2 wakes/s.
        let compiler = sample("cargo", 0, 0, 5_000_000, 20, 2);

        let (shell_score, compiler_score) = (
            sample_score(&shell).expect("shell ran"),
            sample_score(&compiler).expect("compiler ran"),
        );
        assert!(
            shell_score > compiler_score,
            "interactive work must score above throughput work: shell {shell_score} vs compiler {compiler_score}"
        );

        // Burst axis alone: same hop rate, the shorter burst wins.
        let mut shell_burst = compiler.clone();
        shell_burst.avg_runtime_wall = 50_000;
        assert!(sample_score(&shell_burst) > sample_score(&compiler));

        // Hop axis alone: same burst, the higher hop rate wins.
        let mut shell_hop = compiler.clone();
        shell_hop.wait_freq = 2_000;
        shell_hop.wake_freq = 100;
        assert!(sample_score(&shell_hop) > sample_score(&compiler));
    }

    #[test]
    fn actuator_independence() {
        // Same behavior at nice 0 and nice +3 must score identically: the
        // score ignores static_prio, so our own bias cannot feed the signal
        // that justifies the bias.
        let mut at_base = sample("x", 1023, 1023, 3_906_250, 500, 500);
        at_base.static_prio = 20; // nice 0
        let mut at_plus3 = at_base.clone();
        at_plus3.static_prio = 23; // nice +3
        assert_eq!(sample_score(&at_base), sample_score(&at_plus3));
        assert_eq!(sample_score(&at_base), Some(784));

        // Engine-level: identical behavior classifies identically at both
        // prios (loop breaker holds end to end).
        let mut classes = Vec::new();
        for prio in [20u64, 23] {
            let mut eng = BiasEngine::new(BiasConfig::default());
            let mut w1 = noise_batch(60);
            let mut w2 = noise_batch(60);
            let (mut a1, mut a2) = (bulk_batch("app", 32), bulk_batch("app", 32));
            for s in a1.iter_mut().chain(a2.iter_mut()) {
                s.static_prio = prio;
            }
            w1.extend(a1);
            w2.extend(a2);
            eng.ingest(&w1, ts(1));
            eng.ingest(&w2, ts(61));
            eng.ingest(&[], ts(121));
            classes.push(snap_entry(&eng, "app").class);
        }
        assert_eq!(classes[0], BiasClass::Bulk);
        assert_eq!(classes[0], classes[1]);
    }

    #[test]
    fn hysteresis_bulk_needs_two_windows_and_deadband_holds() {
        let mut eng = BiasEngine::new(BiasConfig::default());
        let mut w1 = noise_batch(60);
        w1.extend(bulk_batch("app", 32));
        let mut w2 = noise_batch(60);
        w2.extend(bulk_batch("app", 32));

        eng.ingest(&w1, ts(1));
        eng.ingest(&w2, ts(61)); // closes w1: 1 window below threshold
        assert_eq!(snap_entry(&eng, "app").class, BiasClass::Unknown);

        eng.ingest(&[], ts(121)); // closes w2: 2nd window → BULK
        assert_eq!(snap_entry(&eng, "app").class, BiasClass::Bulk);

        // crossing into the deadband holds the class
        let mut w3 = noise_batch(60);
        w3.extend(dead_batch("app", 32));
        eng.ingest(&w3, ts(181));
        eng.ingest(&[], ts(241)); // closes w3: deadband
        assert_eq!(snap_entry(&eng, "app").class, BiasClass::Bulk);
    }

    #[test]
    fn hysteresis_responsive_symmetric() {
        let mut eng = BiasEngine::new(BiasConfig::default());
        let mut w1 = noise_batch(60);
        w1.extend(resp_batch("app", 32));
        let mut w2 = noise_batch(60);
        w2.extend(resp_batch("app", 32));

        eng.ingest(&w1, ts(1));
        eng.ingest(&w2, ts(61));
        assert_eq!(snap_entry(&eng, "app").class, BiasClass::Unknown);
        eng.ingest(&[], ts(121));
        assert_eq!(snap_entry(&eng, "app").class, BiasClass::Responsive);

        let mut w3 = noise_batch(60);
        w3.extend(dead_batch("app", 32));
        eng.ingest(&w3, ts(181));
        eng.ingest(&[], ts(241));
        assert_eq!(snap_entry(&eng, "app").class, BiasClass::Responsive);
    }

    #[test]
    fn slew_attack_and_release() {
        // Step spacing pinned explicitly: the tick arithmetic below is
        // 60-based. The production default is pinned separately in
        // `attack_step_default_is_thirty_seconds`.
        let mut eng = BiasEngine::new(BiasConfig {
            attack_step_s: 60,
            ..BiasConfig::default()
        });
        drive_bulk(&mut eng, &["app"]); // class Bulk at t=121, wants armed

        // attack: at most 1 step / 60 s, first step 60 s after want-onset
        assert_eq!(snap_entry(&eng, "app").bias, 0);
        eng.ingest(&[], ts(180));
        assert_eq!(snap_entry(&eng, "app").bias, 0);
        eng.ingest(&[], ts(181));
        assert_eq!(snap_entry(&eng, "app").bias, 1);
        eng.ingest(&[], ts(240));
        assert_eq!(snap_entry(&eng, "app").bias, 1);
        eng.ingest(&[], ts(241));
        assert_eq!(snap_entry(&eng, "app").bias, 2);
        eng.ingest(&[], ts(300));
        assert_eq!(snap_entry(&eng, "app").bias, 2);
        eng.ingest(&[], ts(301));
        assert_eq!(snap_entry(&eng, "app").bias, 3); // 3 steps × 60 s
        eng.ingest(&[], ts(400));
        assert_eq!(snap_entry(&eng, "app").bias, 3); // clamped at max

        // deadband window → target 0 → release at most 1 step / 300 s
        let mut w = noise_batch(60);
        w.extend(dead_batch("app", 32));
        eng.ingest(&w, ts(420));
        eng.ingest(&[], ts(480)); // closes deadband window (want at 480)
        eng.ingest(&[], ts(700));
        assert_eq!(snap_entry(&eng, "app").bias, 3);
        eng.ingest(&[], ts(780));
        assert_eq!(snap_entry(&eng, "app").bias, 2);
        eng.ingest(&[], ts(1080));
        assert_eq!(snap_entry(&eng, "app").bias, 1);
        eng.ingest(&[], ts(1380));
        assert_eq!(snap_entry(&eng, "app").bias, 0); // 3 steps × 300 s: slower
    }

    #[test]
    fn global_step_budget_one_per_five_seconds() {
        let mut eng = BiasEngine::new(BiasConfig::default());
        drive_bulk(&mut eng, &["aaa", "bbb"]); // both Bulk, wants armed at 121

        eng.ingest(&[], ts(181));
        assert_eq!(snap_entry(&eng, "aaa").bias, 1); // sorted order spends budget
        assert_eq!(snap_entry(&eng, "bbb").bias, 0);
        eng.ingest(&[], ts(182));
        assert_eq!(snap_entry(&eng, "aaa").bias, 1);
        assert_eq!(snap_entry(&eng, "bbb").bias, 0); // within the 5 s cap
        eng.ingest(&[], ts(186));
        assert_eq!(snap_entry(&eng, "bbb").bias, 1); // budget free again
        assert_eq!(snap_entry(&eng, "aaa").bias, 1);
    }

    /// The restored release cohort must not monopolize the shared global
    /// step. At restart every persisted entry's want stamps at the first
    /// slew pass and the whole cohort matures together at
    /// want+release_step_s; since releases outrank attacks, that flood
    /// used to budget-starve every attack forever (live: an endless 5 s
    /// `bias changed … bias=0` rotation while the build ramp never ran).
    /// Fairness cap: never two release wins in a row while an attack waits.
    #[test]
    fn release_flood_cannot_starve_attacks() {
        // Fairness of the shared step, not the demand gate (see
        // `a_shell_never_attacks_on_a_cpu_spike`).
        let mut eng = BiasEngine::new(BiasConfig {
            attack_min_util: 0,
            ..BiasConfig::default()
        });
        let mut entries = vec![EntrySnapshot {
            name: "app".into(),
            durable: Durable {
                class: BiasClass::Bulk,
                confidence: 0.9,
                ..Durable::default()
            },
        }];
        for i in 0..100 {
            entries.push(EntrySnapshot {
                name: format!("z-flood-{i:03}"),
                durable: Durable {
                    bias: 2,
                    confidence: 0.5,
                    ..Durable::default()
                },
            });
        }
        eng.restore(BiasSnapshot {
            version: BIAS_STATE_VERSION,
            threshold_ema: None,
            entries,
            ..Default::default()
        });
        eng.ingest(&[], ts(1)); // wants stamp here: flood releases mature at 301

        for t in (301..=371).step_by(5) {
            eng.ingest(&[], ts(t));
        }
        // the flood drains a little...
        let drained = eng
            .snapshot()
            .entries
            .iter()
            .filter(|e| e.name.starts_with("z-flood") && e.durable.bias < 2)
            .count();
        assert!(drained >= 3, "flood should drain, got {drained}");
        // ...and the attack still ramped 0 -> 3 at its own 30 s cadence
        assert_eq!(
            snap_entry(&eng, "app").bias,
            3,
            "the release flood starved the attack"
        );
    }

    #[test]
    fn starved_gate_only_low_minus_one() {
        // RESPONSIVE + starved (delivered 100 « 50% of demand 1000) → −1
        let mut eng = BiasEngine::new(BiasConfig::default());
        drive_class(&mut eng, &["starved"], starved_batch);
        assert_eq!(snap_entry(&eng, "starved").class, BiasClass::Responsive);
        ramp_attack(&mut eng);
        assert_eq!(snap_entry(&eng, "starved").bias, -1);
        eng.ingest(&[], ts(400));
        assert_eq!(snap_entry(&eng, "starved").bias, -1); // clamped at min

        // RESPONSIVE but delivered == demand → target 0, no −1
        let mut eng2 = BiasEngine::new(BiasConfig::default());
        drive_class(&mut eng2, &["ok"], resp_batch);
        ramp_attack(&mut eng2);
        eng2.ingest(&[], ts(400));
        assert_eq!(snap_entry(&eng2, "ok").bias, 0);

        // bad ratio but demand below the noise floor → gate stays shut
        let mut eng3 = BiasEngine::new(BiasConfig::default());
        drive_class(&mut eng3, &["tiny"], tiny_demand_batch);
        ramp_attack(&mut eng3);
        eng3.ingest(&[], ts(400));
        assert_eq!(snap_entry(&eng3, "tiny").bias, 0);
    }

    #[test]
    fn aging_halves_confidence_at_half_life_and_relaxes_to_zero() {
        let mut eng = BiasEngine::new(BiasConfig::default());
        drive_bulk(&mut eng, &["app"]); // confidence 0.75 after two windows
        ramp_attack(&mut eng); // bias 3 at t = 301
        let e = snap_entry(&eng, "app");
        assert_eq!(e.bias, 3);
        assert!((e.confidence - 0.75).abs() < 1e-12);

        // one half-life of absence halves confidence exactly (aging anchor is
        // the last window close at t = 121)
        eng.tick(ts(121 + 1800));
        let e = snap_entry(&eng, "app");
        assert!((e.confidence - 0.375).abs() < 1e-12);
        assert_eq!(e.bias, 3); // release is rate-limited: still parked

        // continued absence: confidence keeps decaying, bias relaxes toward 0
        // through release steps and finally the zero-confidence reset — the
        // stream being absent never freezes history.
        let mut t = 121 + 1800;
        while t < 121 + 200_000 {
            t += 100;
            eng.tick(ts(t));
        }
        let e = snap_entry(&eng, "app");
        assert_eq!(e.confidence, 0.0);
        assert_eq!(e.bias, 0);
    }

    #[test]
    fn zero_confidence_forces_zero_bias() {
        let mut eng = BiasEngine::new(BiasConfig::default());
        drive_bulk(&mut eng, &["app"]);
        ramp_attack(&mut eng);
        assert_eq!(snap_entry(&eng, "app").bias, 3);

        eng.tick(ts(121 + 1800 * 80)); // decay underflows the epsilon in one call
        let e = snap_entry(&eng, "app");
        assert_eq!(e.confidence, 0.0);
        assert_eq!(e.bias, 0);
    }

    #[test]
    fn zero_confidence_entries_do_not_spend_the_global_step() {
        // Isolates the confidence rule: the sustained-demand gate has its own
        // test (`a_shell_never_attacks_on_a_cpu_spike`), and these restored
        // entries carry no measured demand.
        let mut eng = BiasEngine::new(BiasConfig {
            attack_min_util: 0,
            ..BiasConfig::default()
        });
        let entry = |name: &str, confidence: f64| EntrySnapshot {
            name: name.into(),
            durable: Durable {
                class: BiasClass::Bulk,
                bias: 0,
                confidence,
                last_seen: 1,
                ..Durable::default()
            },
        };
        eng.restore(BiasSnapshot {
            version: BIAS_STATE_VERSION,
            threshold_ema: None,
            entries: vec![entry("aaa_zombie", 0.0), entry("zzz_live", 0.5)],
            ..Default::default()
        });

        // The first pass stamps both attack wants; the second is past
        // attack_step_s, so the FIFO would hand the single global step to
        // "aaa_zombie" — which has no evidence to back a ramp, and whose step
        // the aging pass would flush anyway.
        eng.ingest(&[], ts(0));
        let changes = eng.ingest(&[], ts(31));
        assert_eq!(
            changes,
            vec![change("zzz_live", 0, 1, BiasClass::Bulk, Cause::Evidence)],
            "the step must go to the entry with evidence"
        );
        assert_eq!(snap_entry(&eng, "aaa_zombie").bias, 0);
        assert_eq!(snap_entry(&eng, "zzz_live").bias, 1);
    }

    #[test]
    fn entry_cap_evicts_least_recently_seen() {
        let cfg = BiasConfig {
            max_entries: 4,
            ..BiasConfig::default()
        };
        let mut eng = BiasEngine::new(cfg);
        for (i, name) in ["a", "b", "c", "d", "e"].iter().enumerate() {
            eng.ingest(&bulk_batch(name, 1), ts(1 + i as u64));
        }
        eng.tick(ts(100));
        let mut names: Vec<String> = eng
            .snapshot()
            .entries
            .into_iter()
            .map(|e| e.name)
            .collect();
        names.sort();
        assert_eq!(names, vec!["b", "c", "d", "e"]);
    }

    #[test]
    fn verify_skips_names_never_applied_and_unknown_static_prio() {
        let mut eng = BiasEngine::new(BiasConfig::default());
        drive_bulk(&mut eng, &["app"]);
        ramp_attack(&mut eng); // bias 3
        // Never actuated by us: ambient nice is not ours to verify, so a
        // mismatching observation must not freeze anything.
        eng.ingest(&[at_prio("app", 20)], ts(360)); // nice 0 ≠ 3 would mismatch
        assert!(!snap_entry(&eng, "app").frozen);
        // Actuated now — the remembered value is the expectation.
        eng.note_applied("app", Some(3));
        assert_eq!(eng.applied_nice("app"), Some(3));
        // static_prio 0 (nice −20 / absent field default) → skip verification.
        eng.ingest(&[at_prio("app", 0)], ts(361));
        assert!(!snap_entry(&eng, "app").frozen);
        // Releasing the claim stops verification again.
        eng.note_applied("app", None);
        assert_eq!(eng.applied_nice("app"), None);
        eng.ingest(&[at_prio("app", 20)], ts(362));
        assert!(!snap_entry(&eng, "app").frozen);
    }

    /// The row check is private and `ingest` creates entries for the rows it
    /// sees; an unknown name must still be a no-op (nothing to verify).
    #[test]
    fn verify_row_ignores_unknown_names() {
        let mut eng = BiasEngine::new(BiasConfig::default());
        eng.verify_row("ghost", 23, unix_secs(ts(1)));
        assert!(eng.snapshot().entries.is_empty());
    }

    #[test]
    fn verify_mismatch_freezes_until_a_matching_sample_rebaselines() {
        let mut eng = BiasEngine::new(BiasConfig::default());
        drive_bulk(&mut eng, &["app"]);
        ramp_attack(&mut eng); // bias 3
        eng.note_applied("app", Some(3));
        assert_eq!(eng.target_nice("app", 0), 3);

        // observed nice 0 (static_prio 20) ≠ applied 3 → freeze
        eng.ingest(&[at_prio("app", 20)], ts(360));
        let e = snap_entry(&eng, "app");
        assert!(e.frozen);
        assert_eq!(e.bias, 3);
        assert_eq!(e.confidence, 0.75);

        // samples for other names leave the frozen entry untouched
        eng.ingest(&bulk_batch("other", 5), ts(361));
        let e = snap_entry(&eng, "app");
        assert!(e.frozen);
        assert_eq!(e.confidence, 0.75);
        assert_eq!(e.bias, 3);

        // a persistent mismatch never re-baselines: samples are dropped and
        // confidence must not churn at LOW on every batch
        eng.ingest(&bulk_batch("app", 5), ts(362));
        let e = snap_entry(&eng, "app");
        assert!(e.frozen);
        assert_eq!(e.confidence, 0.75);

        // a matching observation re-baselines: unfreeze + low confidence,
        // bias kept (evidence/aging will move it from here)
        eng.ingest(&[at_prio("app", 23)], ts(363)); // nice 23 − 20 == 3
        let e = snap_entry(&eng, "app");
        assert!(!e.frozen);
        assert_eq!(e.confidence, LOW_CONFIDENCE);
        assert_eq!(e.bias, 3);
    }

    /// Verification must precede absorption: a row that re-baselines a frozen
    /// entry is evidence in the *same* batch. Absorbing first drops the whole
    /// window (frozen entries take no samples), so the classification lands
    /// one window late.
    #[test]
    fn ingest_rebaselines_and_counts_the_same_batch() {
        let mut eng = BiasEngine::new(BiasConfig::default());
        drive_bulk(&mut eng, &["app"]); // class Bulk, confidence 0.75
        ramp_attack(&mut eng); // bias 3
        eng.note_applied("app", Some(3));
        eng.ingest(&[at_prio("app", 20)], ts(300)); // nice 0 ≠ 3 → frozen
        assert!(snap_entry(&eng, "app").frozen);

        // The batch reports the applied nice, so its re-baseline and the
        // window it fills have to happen in the same call. The noise rows keep
        // the population threshold from drifting onto `app` alone.
        let mut batch = noise_batch(60);
        batch.extend((0..32).map(|_| at_prio("app", 23)));
        eng.ingest(&batch, ts(361));
        let e = snap_entry(&eng, "app");
        assert!(!e.frozen, "a matching row must re-baseline the entry");
        assert_eq!(e.confidence, LOW_CONFIDENCE, "no window has closed yet");

        eng.ingest(&[], ts(421)); // closes the window the batch fed
        let e = snap_entry(&eng, "app");
        assert!(
            e.confidence > LOW_CONFIDENCE,
            "the re-baselined batch must count as evidence, confidence {}",
            e.confidence
        );
    }

    /// A relax step taken while the sample stream is down used to be deferred
    /// to the next batch or lost outright (the timeout arm never drained).
    /// `tick` reports its own moves, with the cause that made them.
    #[test]
    fn tick_reports_relax_moves() {
        let mut eng = BiasEngine::new(BiasConfig::default());
        drive_bulk(&mut eng, &["app"]);
        ramp_attack(&mut eng); // bias 3, confidence 0.75

        // A deadband window turns the target to 0; the release only fires
        // release_step_s later, with no batch anywhere in between.
        let mut w = noise_batch(60);
        w.extend(dead_batch("app", 32));
        eng.ingest(&w, ts(420));
        eng.ingest(&[], ts(480)); // closes it: the release want stamps at 480
        assert_eq!(
            eng.tick(ts(780)),
            vec![change("app", 3, 2, BiasClass::Bulk, Cause::Relax)],
            "the no-evidence pass must report its relax step"
        );

        // Zero confidence flushes the rest, also without a batch.
        let mut flushed = BiasEngine::new(BiasConfig::default());
        drive_bulk(&mut flushed, &["app"]);
        ramp_attack(&mut flushed); // bias 3
        assert_eq!(
            flushed.tick(ts(121 + 1800 * 80)),
            vec![change("app", 3, 0, BiasClass::Bulk, Cause::Flush)]
        );
    }

    #[test]
    fn persistent_mismatch_cannot_pin_the_bias() {
        let mut eng = BiasEngine::new(BiasConfig::default());
        drive_bulk(&mut eng, &["app"]);
        ramp_attack(&mut eng); // bias 3
        eng.note_applied("app", Some(3));
        eng.ingest(&[at_prio("app", 20)], ts(360)); // mismatch → freeze
        // Samples keep flowing while the loop keeps lying (they carry
        // static_prio 20 = nice 0): the entry stays frozen and un-updated...
        for i in 0..5u64 {
            eng.ingest(&bulk_batch("app", 5), ts(400 + i * 60));
            assert!(snap_entry(&eng, "app").frozen);
        }
        // ...but aging still flushes the bias: old learnings never pin it.
        let mut t = 400 + 5 * 60;
        while t < 200_000 {
            t += 100;
            eng.tick(ts(t));
        }
        assert_eq!(snap_entry(&eng, "app").bias, 0);
    }

    /// The freeze must never outlive its own evidence. After a restart the
    /// closed-loop expectation (`applied_nice`) is gone — it is not
    /// persisted — so no observation can ever "match" again and a frozen
    /// entry used to brick forever (live: `build` stuck frozen after the
    /// restart, class stale, learning dead — the mid-compile ramp could
    /// never start). With nothing applied on record the loop can verify
    /// nothing, so a fresh sample re-baselines the entry.
    #[test]
    fn frozen_with_no_actuation_on_record_rebaselines_on_a_fresh_sample() {
        let mut eng = BiasEngine::new(BiasConfig::default());
        drive_bulk(&mut eng, &["app"]);
        ramp_attack(&mut eng); // bias 3
        eng.note_applied("app", Some(3));
        eng.ingest(&[at_prio("app", 20)], ts(360)); // mismatch -> frozen

        // restart: the state round-trips, the actuation on record does not
        let snap = eng.snapshot();
        let mut eng2 = BiasEngine::new(BiasConfig::default());
        eng2.restore(snap);
        assert!(snap_entry(&eng2, "app").frozen);

        // a fresh sample must re-baseline: bias kept, confidence low
        eng2.ingest(&[at_prio("app", 20)], ts(361));
        let e = snap_entry(&eng2, "app");
        assert!(!e.frozen);
        assert_eq!(e.confidence, LOW_CONFIDENCE);
        assert_eq!(e.bias, 3); // evidence/aging move it from here
    }

    /// Only the absence of ANY actuation on record (a restart) may unwind
    /// the freeze. Releasing the claim — `note_applied(name, None)` when an
    /// apply did not take (Denied/Failed/Gone) — must not: the pinned law
    /// stands, a mismatch freezes the entry and only a matching sample
    /// re-baselines it (aging flushing the bias is the documented escape).
    #[test]
    fn frozen_entry_with_released_claim_stays_frozen() {
        let mut eng = BiasEngine::new(BiasConfig::default());
        drive_bulk(&mut eng, &["app"]);
        ramp_attack(&mut eng); // bias 3
        eng.note_applied("app", Some(3));
        eng.ingest(&[at_prio("app", 20)], ts(360)); // mismatch -> frozen

        eng.note_applied("app", None); // apply did not take: claim released
        eng.ingest(&[at_prio("app", 23)], ts(361)); // would "match" — but nothing is claimed
        assert!(
            snap_entry(&eng, "app").frozen,
            "a released claim must not unwind the freeze"
        );
    }

    #[test]
    fn inverted_clamps_are_normalized_not_panicking() {
        // i8::clamp panics on min > max; BiasEngine::new must not.
        let cfg = BiasConfig {
            min_bias: 5,
            max_bias: 2,
            ..BiasConfig::default()
        };
        let mut eng = BiasEngine::new(cfg);
        let snap = crate::bias_state::BiasSnapshot {
            version: crate::bias_state::BIAS_STATE_VERSION,
            threshold_ema: None,
            lean_capture: None,
            entries: vec![crate::bias_state::EntrySnapshot {
                name: "app".into(),
                durable: crate::bias_state::Durable {
                    bias: 4,
                    ..crate::bias_state::Durable::default()
                },
            }],
        };
        eng.restore(snap);
        // normalized to [2, 5]; the restored bias clamps into that range
        assert!((2..=5).contains(&snap_entry(&eng, "app").bias));
    }

    #[test]
    fn target_nice_is_idempotent_and_clamped() {
        let mut eng = BiasEngine::new(BiasConfig::default());
        drive_bulk(&mut eng, &["app"]);
        ramp_attack(&mut eng);
        assert_eq!(eng.target_nice("app", 0), 3);
        assert_eq!(eng.target_nice("app", 0), 3); // recomputed, never accumulates
        assert_eq!(eng.target_nice("app", 18), 19); // clamped at 19
        assert_eq!(eng.target_nice("app", -5), -2);
        assert_eq!(eng.target_nice("ghost", 7), 7); // unknown names: base only
    }

    #[test]
    fn ingest_reports_each_move_exactly_once() {
        let mut eng = BiasEngine::new(BiasConfig::default());
        drive_bulk(&mut eng, &["app"]);
        let changes = eng.ingest(&[], ts(181)); // app bias 0 → 1 at T0+181
        assert_eq!(
            changes,
            vec![change("app", 0, 1, BiasClass::Bulk, Cause::Evidence)]
        );
        // the move was reported, not queued: a pass with nothing to do is silent
        assert!(eng.ingest(&[], ts(186)).is_empty());
        // the next step is a fresh report carrying its own `prev`
        let changes = eng.ingest(&[], ts(211)); // app bias 1 → 2
        assert_eq!(
            changes,
            vec![change("app", 1, 2, BiasClass::Bulk, Cause::Evidence)]
        );
        assert!(eng.ingest(&[], ts(216)).is_empty());
    }

    #[test]
    fn normalize_name_collapses_instance_and_version_suffixes_only() {
        use std::borrow::Cow;
        // per-instance suffix after the last ':'/'#'
        assert_eq!(normalize_name("kworker/u97:10"), "kworker/u97");
        assert_eq!(normalize_name("ghostty:gdrv0"), "ghostty");
        assert_eq!(normalize_name("WRRende~ckend#0"), "WRRende~ckend");
        // one trailing separator+digits run is a version
        assert_eq!(normalize_name("clang-19"), "clang");
        assert_eq!(normalize_name("python3.12"), "python3");
        assert_eq!(normalize_name("python3.12.3"), "python3.12"); // one run only
        assert_eq!(normalize_name("v1.2"), "v1");
        // semantic families are deliberately NOT folded here (bias_group's job)
        for keep in ["cc1plus", "lto1", "x265", "sleep", "python3"] {
            assert_eq!(normalize_name(keep), keep);
            assert!(matches!(normalize_name(keep), Cow::Borrowed(_)));
        }
        // changed names are Owned, unchanged ones Borrowed (the exec path
        // stays allocation-free)
        assert!(matches!(normalize_name("kworker/u97:10"), Cow::Owned(_)));
        // stripping must never produce an empty key
        assert_eq!(normalize_name("-1"), "-1");
    }

    #[test]
    fn bias_group_pools_members_into_one_entry() {
        let re = regex::Regex::new("^(cc1.*|lto1|collect2|as|ld.*)$").unwrap();
        let mut eng =
            BiasEngine::new_with_groups(BiasConfig::default(), vec![("build".to_string(), re)]);
        // 4 samples per phase name: alone each is under the decisive floor
        // (7), pooled the family classifies on the first window — the exact
        // cc1plus→as→ld→lto1 handoff pattern a build produces.
        let mut w1 = noise_batch(60);
        w1.extend(decisive_bulk_batch("cc1plus", 4));
        w1.extend(decisive_bulk_batch("lto1", 4));
        eng.ingest(&w1, ts(1));
        eng.ingest(&[], ts(61)); // closes w1

        let snap = eng.snapshot();
        let names: Vec<&str> = snap.entries.iter().map(|e| e.name.as_str()).collect();
        // cc1plus/lto1 pooled into "build"; only the noise population beside it
        assert_eq!(names, vec!["build", "noise"]);
        assert_eq!(
            snap.entries
                .iter()
                .find(|e| e.name == "build")
                .unwrap()
                .durable
                .class,
            BiasClass::Bulk
        );

        // ramp the pooled bias; every member name sees it
        eng.ingest(&[], ts(91));
        eng.ingest(&[], ts(121));
        eng.ingest(&[], ts(151));
        assert_eq!(eng.target_nice("lto1", 0), 3);
        assert_eq!(eng.target_nice("cc1plus", 0), 3);

        // actuation + closed-loop verification also key through members
        eng.note_applied("lto1", Some(3));
        assert_eq!(eng.applied_nice("cc1plus"), Some(3));
        eng.ingest(&[at_prio("ld", 20)], ts(160)); // nice 0 ≠ 3 → freeze the entry
        assert!(snap_entry(&eng, "build").frozen);

        // non-members keep their own entries
        eng.ingest(&decisive_bulk_batch("x265", 4), ts(200));
        assert!(eng.snapshot().entries.iter().any(|e| e.name == "x265"));
    }

    #[test]
    fn decisive_evidence_classifies_on_one_window() {
        // Default min_samples = 30 → decisive floor = max(30 / 4, 4) = 7.
        let mut eng = BiasEngine::new(BiasConfig::default());
        let mut w1 = noise_batch(60);
        w1.extend(decisive_bulk_batch("app", 7));
        eng.ingest(&w1, ts(1));
        eng.ingest(&[], ts(61)); // ONE window suffices
        assert_eq!(snap_entry(&eng, "app").class, BiasClass::Bulk);

        // one sample short of the floor: still just sparse evidence
        let mut eng2 = BiasEngine::new(BiasConfig::default());
        let mut w = noise_batch(60);
        w.extend(decisive_bulk_batch("app", 6));
        eng2.ingest(&w, ts(1));
        eng2.ingest(&[], ts(61));
        assert_eq!(snap_entry(&eng2, "app").class, BiasClass::Unknown);
    }

    #[test]
    fn non_decisive_needs_two_windows_and_min_samples() {
        // non-decisive evidence is still bound by min_samples: 29 < 30 keeps
        // the class Unknown no matter how many windows accumulate
        let mut eng = BiasEngine::new(BiasConfig::default());
        let mut w1 = noise_batch(60);
        w1.extend(bulk_batch("app", 29));
        let mut w2 = noise_batch(60);
        w2.extend(bulk_batch("app", 29));
        eng.ingest(&w1, ts(1));
        eng.ingest(&w2, ts(61));
        eng.ingest(&[], ts(121));
        assert_eq!(snap_entry(&eng, "app").class, BiasClass::Unknown);

        // …and one full window never suffices for non-decisive evidence
        let mut eng2 = BiasEngine::new(BiasConfig::default());
        let mut w = noise_batch(60);
        w.extend(bulk_batch("app", 32));
        eng2.ingest(&w, ts(1));
        eng2.ingest(&[], ts(61));
        assert_eq!(snap_entry(&eng2, "app").class, BiasClass::Unknown);
    }

    #[test]
    fn attack_step_default_is_thirty_seconds() {
        // Locked default: a build-phase ramp must reach target in 3×30 s —
        // the old 60 s step outlived short compile phases entirely.
        assert_eq!(BiasConfig::default().attack_step_s, 30);
    }

    /// A closed-loop mismatch re-baseline must not stall the ramp: the want
    /// onset survives the reset, so the attack step still fires at
    /// want+attack_step_s (and keeps its FIFO priority) even when freeze /
    /// re-baseline churns around it — the measured build-ramp starvation.
    #[test]
    fn rebaseline_preserves_want_onset_for_the_ramp() {
        let mut eng = BiasEngine::new(BiasConfig::default()); // attack_step 30
        drive_bulk(&mut eng, &["app"]); // class Bulk at t=121, want armed at 121
        eng.note_applied("app", Some(3));
        eng.ingest(&[at_prio("app", 20)], ts(140)); // mismatch -> frozen
        eng.ingest(&[at_prio("app", 23)], ts(141)); // match -> re-baseline (conf -> LOW)
        assert_eq!(snap_entry(&eng, "app").confidence, LOW_CONFIDENCE);

        eng.ingest(&[], ts(151)); // want(121) + 30 -> the step must still happen
        assert_eq!(snap_entry(&eng, "app").bias, 1);
    }

    /// Borderline windows that flip `cand` Bulk↔None every window churn the
    /// target 3↔0 and must not starve the ramp. The live build hit exactly
    /// this (window mean 1066 vs deadband low edge 1119): every flip
    /// re-stamped the want onsets, so neither the 30 s attack nor the 300 s
    /// release timer ever matured and the bias froze at +1 for the whole
    /// compile (zero `bias changed name="build"` lines). Sustained
    /// alternating evidence classifies BULK throughout and must still ramp
    /// to the target at the attack cadence.
    #[test]
    fn borderline_flips_do_not_starve_the_ramp() {
        let mut eng = BiasEngine::new(BiasConfig::default()); // attack_step 30
        drive_bulk(&mut eng, &["app"]); // class Bulk at t=121, want armed at 121

        // Take exactly one step — the live run's frozen starting point.
        eng.ingest(&borderline_batch("app", 32), ts(151));
        assert_eq!(snap_entry(&eng, "app").bias, 1);

        // Alternate decisive BULK windows (score 100) with borderline
        // deadband windows (score 324): `cand` flips Bulk↔None and the
        // target 3↔0 every window while the class holds BULK — the flip
        // pattern of the live build. Windows close at 181, 241, …;
        // each ingest closes the previous window first, then slew-passes.
        for i in 0..6u64 {
            let mut bulk = noise_batch(60);
            bulk.extend(decisive_bulk_batch("app", 32));
            let mut dead = noise_batch(60);
            dead.extend(borderline_batch("app", 32));
            eng.ingest(&bulk, ts(181 + 120 * i)); // closes a dead window
            eng.ingest(&dead, ts(241 + 120 * i)); // closes a bulk window
        }
        eng.ingest(&[], ts(901)); // close the trailing dead window

        let e = snap_entry(&eng, "app");
        assert_eq!(e.class, BiasClass::Bulk, "alternation must hold the class");
        assert_eq!(e.bias, 3, "borderline cand flips froze the ramp");
    }

    /// Sparse-but-active windows must hold the confidence: churn comms never
    /// accumulate a full window and used to halve it into the ground
    /// mid-activity (0.488 → 0.007 over 6 min of live building), letting the
    /// relax rule dip and eventually flush a correct bias.
    #[test]
    fn sparse_active_windows_hold_confidence() {
        let mut eng = BiasEngine::new(BiasConfig::default());
        drive_bulk(&mut eng, &["app"]); // class Bulk, confidence 0.75
        assert_eq!(snap_entry(&eng, "app").class, BiasClass::Bulk);
        let conf = snap_entry(&eng, "app").confidence;
        assert!((conf - 0.75).abs() < 1e-12);

        // two windows of 5 scored samples each (below min_samples = 30)
        for (open, close) in [(181, 241), (241, 301)] {
            eng.ingest(&bulk_batch("app", 5), ts(open));
            eng.ingest(&[], ts(close));
            let e = snap_entry(&eng, "app");
            assert!(
                (e.confidence - conf).abs() < 1e-12,
                "sparse window moved confidence: {} -> {}",
                conf,
                e.confidence
            );
            assert_eq!(e.class, BiasClass::Bulk);
        }
    }

    /// Present but entirely unscored samples (zero freq/runtime fields) are
    /// still presence: the class and confidence hold.
    #[test]
    fn unscored_active_windows_hold_confidence() {
        let mut eng = BiasEngine::new(BiasConfig::default());
        drive_bulk(&mut eng, &["app"]); // class Bulk, confidence 0.75
        let conf = snap_entry(&eng, "app").confidence;

        // >= min_samples so the window reaches the scoring gate rather than
        // being held as sparse.
        let unscored: Batch = (0..32)
            .map(|_| sample("app", 0, 31, 0, 500, 500))
            .collect();
        eng.ingest(&unscored, ts(181));
        eng.ingest(&[], ts(241));
        let e = snap_entry(&eng, "app");
        assert_eq!(e.class, BiasClass::Bulk);
        assert!(
            (e.confidence - conf).abs() < 1e-12,
            "unscored window moved confidence: {} -> {}",
            conf,
            e.confidence
        );
    }

    /// A CPU-bound compiler has `wait_freq == 0` AND `wake_freq == 0`: a
    /// genuine zero score (not a missing field) must classify BULK, and the
    /// next non-zero window must EWMA from that real zero instead of treating
    /// it as the "uninitialized" sentinel.
    #[test]
    fn zero_score_compiler_batch_is_bulk() {
        fn compiler_batch(name: &str, n: usize) -> Batch {
            (0..n)
                .map(|_| sample(name, 0, 0, 900_000_000, 1000, 1000))
                .collect()
        }

        let mut eng = BiasEngine::new(BiasConfig::default());
        drive_class(&mut eng, &["cc1plus"], compiler_batch);
        assert_eq!(snap_entry(&eng, "cc1plus").class, BiasClass::Bulk);
        assert_eq!(
            snap_entry(&eng, "cc1plus").score_ema,
            0.0,
            "a zero score is a real observation"
        );

        let mut w = noise_batch(60);
        w.extend((0..32).map(|_| sample("cc1plus", 31, 31, 1_000_000_000, 500, 500)));
        eng.ingest(&w, ts(181));
        eng.ingest(&[], ts(241));
        // The 32 compiler samples score 100; smoothing from the real zero
        // gives 100 * (1 - e^-1) ~= 63.2, while treating 0.0 as
        // "uninitialized" would snap the EMA to 100.
        let e = snap_entry(&eng, "cc1plus");
        assert!(
            e.score_ema > 0.0 && e.score_ema < 99.0,
            "score_ema must smooth from the real zero: {}",
            e.score_ema
        );
    }

    #[test]
    fn starved_gate_honours_custom_thresholds() {
        // A floor above any possible demand keeps the gate shut.
        let cfg = BiasConfig {
            starved_min_demand: 1500,
            ..BiasConfig::default()
        };
        let mut eng = BiasEngine::new(cfg);
        drive_class(&mut eng, &["starved"], starved_batch);
        ramp_attack(&mut eng);
        eng.ingest(&[], ts(400));
        assert_eq!(
            snap_entry(&eng, "starved").bias,
            0,
            "demand 1000 < starved_min_demand 1500"
        );

        // A permissive ratio lets a moderately shared task through.
        let cfg = BiasConfig {
            starved_ratio: 0.99,
            starved_min_demand: 10,
            ..BiasConfig::default()
        };
        let mut eng = BiasEngine::new(cfg);
        drive_class(&mut eng, &["starved"], starved_batch);
        ramp_attack(&mut eng);
        assert_eq!(snap_entry(&eng, "starved").bias, -1);
    }

    #[test]
    fn snapshot_restore_round_trip_keeps_bias_but_not_live_state() {
        let mut eng = BiasEngine::new(BiasConfig::default());
        drive_bulk(&mut eng, &["app"]);
        ramp_attack(&mut eng);
        // Mid-ramp: a claim on record and a window open with samples in it.
        eng.note_applied("app", Some(3));
        let mid: Batch = (0..5).map(|_| at_prio("app", 23)).collect();
        eng.ingest(&mid, ts(305));
        let snap = eng.snapshot();

        let mut eng2 = BiasEngine::new(BiasConfig::default());
        eng2.restore(snap.clone());
        assert_eq!(eng2.snapshot(), snap);
        assert_eq!(eng2.target_nice("app", 0), 3);
        assert_eq!(snap_entry(&eng2, "app").bias, 3, "durable bias round-trips");

        // The transient half is rebuilt, never persisted: no claim (the
        // closed loop cannot verify against a restart), no want stamps, no
        // window accumulators.
        let e = &eng2.entries["app"];
        assert_eq!(eng2.applied_nice("app"), None);
        assert!(!e.live.claim_recorded);
        assert!(e.live.want_since_attack.is_none());
        assert!(e.live.want_since_release.is_none());
        assert_eq!((e.live.win_n, e.live.win_score_n), (0, 0));
        // aging resumes from the last sighting
        assert_eq!(e.live.aged_at, snap_entry(&eng2, "app").last_seen);

        let before = snap_entry(&eng2, "app").confidence;
        eng2.tick(ts(305 + 1800));
        let after = snap_entry(&eng2, "app").confidence;
        assert!((after - before * 0.5).abs() < 1e-12);
    }

    /// A reload that changes `bias_window_s` must change when windows close —
    /// the next windows are `window_s` wide, not the startup width.
    #[test]
    fn reconfigure_applies_window_width_to_later_windows() {
        let mut eng = BiasEngine::new(BiasConfig::default());
        eng.reconfigure(
            BiasConfig {
                window_s: 10,
                ..BiasConfig::default()
            },
            Vec::new(),
        );
        assert_eq!(eng.config().window_s, 10);

        let mut w = noise_batch(60);
        w.extend(decisive_bulk_batch("app", 7));
        eng.ingest(&w, ts(1)); // window 1..11
        eng.ingest(&[], ts(11)); // closes it: 10 s, not 60 s
        assert_eq!(snap_entry(&eng, "app").class, BiasClass::Bulk);

        // Control: the same fixture under the default 60 s window is still
        // open at t = 11, so this assertion is about the new width.
        let mut wide = BiasEngine::new(BiasConfig::default());
        wide.ingest(&w, ts(1));
        wide.ingest(&[], ts(11));
        assert_eq!(snap_entry(&wide, "app").class, BiasClass::Unknown);
    }

    /// `bias_deadband_pct` is a live knob too: a sample 7.8 % above T is
    /// inside the default 15 % deadband and outside a reconfigured 3 % one.
    #[test]
    fn reconfigure_applies_deadband_to_later_windows() {
        // score 441 = 21² (wait 1023/wake 1023 → 20, runtime 400 ms → +1);
        // 441 = 1.10 × the 400-score population.
        let near = |name: &str, n: usize| -> Batch {
            (0..n)
                .map(|_| sample(name, 1023, 1023, 400_000_000, 500, 500))
                .collect()
        };
        assert_eq!(sample_score(&sample("x", 1023, 1023, 400_000_000, 500, 500)), Some(441));

        let mut narrow = BiasEngine::new(BiasConfig::default());
        narrow.ingest(&noise_batch(60), ts(1));
        narrow.ingest(&[], ts(61)); // population only: T = 400
        narrow.reconfigure(
            BiasConfig {
                deadband_pct: 3,
                ..BiasConfig::default()
            },
            Vec::new(),
        );
        let mut w = noise_batch(60);
        w.extend(near("app", 32));
        narrow.ingest(&w, ts(120));
        narrow.ingest(&[], ts(181));
        assert_eq!(snap_entry(&narrow, "app").class, BiasClass::Responsive);

        // Control: identical evidence under the default 15 % deadband holds
        // the class (no reconfigure ⇒ the key would have been ignored).
        let mut wide = BiasEngine::new(BiasConfig::default());
        wide.ingest(&noise_batch(60), ts(1));
        wide.ingest(&[], ts(61));
        wide.ingest(&w, ts(120));
        wide.ingest(&[], ts(181));
        assert_eq!(snap_entry(&wide, "app").class, BiasClass::Unknown);
    }

    /// Shrinking `bias_max` must take effect on the spot (not at the next
    /// slew step) and pull the target along with it.
    #[test]
    fn reconfigure_clamps_bias_and_rederives_the_target() {
        let mut eng = BiasEngine::new(BiasConfig::default());
        drive_bulk(&mut eng, &["app"]);
        ramp_attack(&mut eng);
        let before = snap_entry(&eng, "app");
        assert_eq!((before.bias, eng.entries["app"].live.target), (3, 3));
        let threshold_before = eng.snapshot().threshold_ema;

        eng.reconfigure(
            BiasConfig {
                max_bias: 1,
                ..BiasConfig::default()
            },
            Vec::new(),
        );

        let after = snap_entry(&eng, "app");
        assert_eq!(after.bias, 1, "clamped immediately");
        assert_eq!(
            eng.entries["app"].live.target,
            1,
            "target follows the class and the new clamp"
        );
        assert_eq!(eng.target_nice("app", 0), 1);
        // Historical evidence is not configuration: it must survive.
        assert_eq!(after.score_ema, before.score_ema);
        assert_eq!(after.confidence, before.confidence);
        assert_eq!(after.class, before.class);
        assert_eq!(eng.snapshot().threshold_ema, threshold_before);
        // The clamp surfaces on the normal "bias changed" path at the next
        // pass, marked as a non-evidence move.
        assert_eq!(
            eng.ingest(&[], ts(305)),
            vec![change("app", 3, 1, BiasClass::Bulk, Cause::Relax)]
        );
    }

    /// Changing `bias_group.<tag>` changes history keys: the members are
    /// re-keyed, merging on collision without losing confidence or bias.
    #[test]
    fn reconfigure_rekeys_groups_without_losing_confidence() {
        let mut eng = BiasEngine::new(BiasConfig::default());
        drive_bulk(&mut eng, &["cc1plus"]);
        ramp_attack(&mut eng);
        let strong = snap_entry(&eng, "cc1plus");
        assert_eq!((strong.bias, strong.class), (3, BiasClass::Bulk));

        // A second family member with weaker evidence (one decisive window).
        let mut w = noise_batch(60);
        w.extend(decisive_bulk_batch("lto1", 8));
        eng.ingest(&w, ts(361));
        eng.ingest(&[], ts(421)); // closes lto1's window
        let weak = snap_entry(&eng, "lto1");
        assert_eq!(weak.class, BiasClass::Bulk);
        assert!(weak.confidence < strong.confidence, "{weak:?} vs {strong:?}");

        let re = regex::Regex::new("^(cc1plus|lto1)$").unwrap();
        eng.reconfigure(BiasConfig::default(), vec![("build".to_string(), re)]);

        let names: Vec<String> = eng.snapshot().entries.into_iter().map(|e| e.name).collect();
        assert!(names.contains(&"build".to_string()), "names: {names:?}");
        assert!(!names.contains(&"cc1plus".to_string()));
        assert!(!names.contains(&"lto1".to_string()));

        let merged = snap_entry(&eng, "build");
        assert_eq!(merged.confidence, strong.confidence, "higher-confidence side survives");
        assert_eq!(merged.class, BiasClass::Bulk);
        assert_eq!(merged.bias, 3, "the larger |bias| must never be lost in a merge");
        assert_eq!(eng.target_nice("cc1plus", 0), 3, "members resolve to the pooled entry");

        // Dropping the family again gives the pooled tag back its own entry
        // (the members' original keys are not recoverable — documented).
        eng.reconfigure(BiasConfig::default(), Vec::new());
        assert!(eng.snapshot().entries.iter().any(|e| e.name == "build"));
        assert_eq!(eng.target_nice("build", 0), 3);
        assert!(eng.snapshot().entries.iter().all(|e| e.name != "cc1plus"));
    }

    /// Shadow mode is observation only: the same scripted run must produce
    /// the same durable state and the same reported moves with the flag off
    /// and on. Ten windows, so hysteresis, the attacks and the releases all
    /// happen inside the run.
    #[test]
    fn shadow_off_changes_nothing() {
        fn run(shadow: bool) -> (BiasSnapshot, Vec<Change>) {
            let mut eng = BiasEngine::new(BiasConfig {
                shadow,
                ..BiasConfig::default()
            });
            let mut changes = Vec::new();
            for i in 0..10u64 {
                let t = i * 60 + 1;
                changes.extend(eng.ingest(&shadow_window(), ts(t)));
                changes.extend(eng.tick(ts(t + 30)));
            }
            assert!(eng.shadow_stats() == (0, 0, 0) || shadow);
            (eng.snapshot(), changes)
        }
        let (off_snap, off_changes) = run(false);
        let (on_snap, on_changes) = run(true);
        assert_eq!(off_changes, on_changes, "shadow must not move any bias");
        // The two capture fields are the one durable difference by design
        // (ADR 0003): the verdict must be replayable offline. Everything the
        // state machine reads must still be identical.
        fn without_lean(mut s: BiasSnapshot) -> BiasSnapshot {
            for e in &mut s.entries {
                e.durable.burst_ns = 0;
                e.durable.hop_hz = 0;
                e.durable.lean_at = 0;
            }
            s.lean_capture = None;
            s
        }
        assert_eq!(
            without_lean(off_snap.clone()),
            without_lean(on_snap.clone()),
            "shadow must not reach any decision state"
        );
        assert!(
            on_snap.entries.iter().any(|e| e.durable.burst_ns > 0),
            "shadow-on must record the lean inputs for the replay"
        );
        assert!(
            off_snap.entries.iter().all(|e| e.durable.burst_ns == 0),
            "shadow-off must record nothing"
        );
        // The capture marker is the replay's licence: absent when the daemon
        // never ran the shadow classifier, and it names the boundary in force
        // (the anchors are live config keys).
        assert!(off_snap.lean_capture.is_none());
        let capture = on_snap.lean_capture.expect("shadow-on records the capture");
        let cfg = BiasConfig {
            shadow: true,
            ..BiasConfig::default()
        };
        let b = cfg.boundary();
        assert_eq!((capture.stage_min_ns, capture.hop_min_hz), (b.stage_min_ns, b.hop_min_hz));
        assert_eq!(capture.deadband_pct, b.deadband_pct);
        assert!(
            on_snap.entries.iter().any(|e| e.durable.lean_at > 0),
            "the capture dates its entries"
        );
        // ...and the off run really did compute nothing.
        assert_eq!(off_snap.version, BIAS_STATE_VERSION);
    }

    /// The lean anchors are live config keys. A reload that changes them makes
    /// every recorded input describe a classifier the daemon no longer runs,
    /// so the capture restarts; the same boundary keeps it.
    #[test]
    fn a_boundary_change_invalidates_the_capture() {
        let mut eng = BiasEngine::new(BiasConfig {
            shadow: true,
            ..BiasConfig::default()
        });
        eng.ingest(&shadow_window(), ts(1));
        eng.ingest(&[], ts(61));
        assert!(eng.snapshot().lean_capture.is_some());
        assert!(eng.entries.values().any(|e| e.durable.lean_at > 0));

        eng.reconfigure(
            BiasConfig {
                shadow: true,
                ..BiasConfig::default()
            },
            Vec::new(),
        );
        assert!(
            eng.snapshot().lean_capture.is_some(),
            "an unchanged boundary keeps the capture"
        );

        eng.reconfigure(
            BiasConfig {
                shadow: true,
                lean_stage_min_ns: 32_000,
                ..BiasConfig::default()
            },
            Vec::new(),
        );
        let snap = eng.snapshot();
        assert!(
            snap.lean_capture.is_none(),
            "a different classifier invalidates the capture"
        );
        assert!(snap.entries.iter().all(|e| e.durable.lean_at == 0));
    }

    /// Switching the shadow off must not discard a capture that already
    /// exists: the marker records the boundary the inputs were measured with.
    #[test]
    fn a_capture_survives_turning_shadow_off() {
        let mut eng = BiasEngine::new(BiasConfig {
            shadow: true,
            ..BiasConfig::default()
        });
        eng.ingest(&shadow_window(), ts(1));
        eng.ingest(&[], ts(61));
        eng.reconfigure(
            BiasConfig {
                shadow: false,
                ..BiasConfig::default()
            },
            Vec::new(),
        );
        let snap = eng.snapshot();
        assert!(snap.lean_capture.is_some(), "the earlier capture is still usable");
        assert!(snap.entries.iter().any(|e| e.durable.lean_at > 0));
    }

    /// A window the population boundary calls BULK but the lean axis calls
    /// RESPONSIVE must be counted, and its per-name log stamp must stick for
    /// `SHADOW_LOG_S` (no per-window spam).
    #[test]
    fn shadow_counts_a_disagreement() {
        let mut eng = BiasEngine::new(BiasConfig {
            shadow: true,
            ..BiasConfig::default()
        });
        assert_eq!(eng.shadow_stats(), (0, 0, 0));

        eng.ingest(&shadow_window(), ts(1));
        eng.ingest(&[], ts(61)); // closes window 1
        // "app" disagrees (pop Bulk, lean Responsive); the background is a
        // deadband-hold for the population but a lean verdict: unilateral.
        assert_eq!(eng.shadow_stats(), (2, 1, 1));
        let app = &eng.entries["app"];
        assert_eq!(app.durable.class, BiasClass::Bulk, "the actuator still classifies");
        assert_eq!(app.live.shadow_logged_at, Some(T0 + 61));

        // A second window, well inside the 10-minute throttle: counted, not
        // re-logged.
        eng.ingest(&shadow_window(), ts(61));
        eng.ingest(&[], ts(121));
        assert_eq!(eng.shadow_stats(), (4, 2, 2));
        assert_eq!(
            eng.entries["app"].live.shadow_logged_at,
            Some(T0 + 61),
            "the stamp must not move inside SHADOW_LOG_S"
        );
    }

    /// The cumulative summary is emitted at most once per `SHADOW_LOG_S`,
    /// even though windows close every 60 s. `shadow_summary_at` is the
    /// emission record (the test does not sleep and captures no log).
    #[test]
    fn shadow_summary_is_rate_limited() {
        let mut eng = BiasEngine::new(BiasConfig {
            shadow: true,
            ..BiasConfig::default()
        });
        eng.ingest(&shadow_window(), ts(1));
        eng.ingest(&[], ts(61)); // closes window 1 → first (and only) summary
        assert_eq!(eng.shadow_summary_at, Some(T0 + 61));

        eng.ingest(&shadow_window(), ts(61));
        eng.ingest(&[], ts(121)); // closes window 2, 60 s later
        assert_eq!(eng.shadow_stats().0, 4, "the second window WAS counted");
        assert_eq!(
            eng.shadow_summary_at,
            Some(T0 + 61),
            "exactly one summary inside the interval"
        );

        // Past the interval a fresh summary is due again — the catch-up
        // closes on the way to 661 emit it once, and 721 is suppressed again.
        eng.ingest(&shadow_window(), ts(661));
        eng.ingest(&[], ts(721));
        assert_eq!(eng.shadow_summary_at, Some(T0 + 661));
    }

    /// The flag is reconfigurable: flipping it on a reload starts the shadow
    /// counters without touching history.
    #[test]
    fn reconfigure_picks_up_shadow() {
        let mut eng = BiasEngine::new(BiasConfig::default());
        eng.ingest(&shadow_window(), ts(1));
        eng.ingest(&[], ts(61));
        assert_eq!(eng.shadow_stats(), (0, 0, 0), "default is off");

        eng.reconfigure(
            BiasConfig {
                shadow: true,
                ..BiasConfig::default()
            },
            Vec::new(),
        );
        eng.ingest(&shadow_window(), ts(61));
        eng.ingest(&[], ts(121));
        assert_eq!(eng.shadow_stats(), (2, 1, 1));
    }
}

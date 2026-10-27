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
//!   scored samples; absence is never evidence (streaks/untouched confidence).
//! * **Controller:** BULK → `max_bias`, RESPONSIVE + share-starved →
//!   `min_bias`, RESPONSIVE → 0, deadband/unknown → hold then relax toward 0
//!   at the release rate. Attack (away from 0 toward target) ≤ 1 step /
//!   `attack_step_s` from want-onset; release (toward 0) ≤ 1 step /
//!   `release_step_s`; one bias step globally per `global_step_s` (shared
//!   budget, deterministic BTreeMap order).
//! * **History never permanently fixes bias:** confidence decays with
//!   wall-time since last activity (half-life `half_life_s`) and each
//!   `RELAX_STEP` of decay releases one step toward 0; zero confidence forces
//!   bias = 0. Entry cap with LRU eviction by `last_seen`.
//!
//! All time enters through `now: SystemTime` parameters (never
//! `Instant::now()`), so behavior is deterministic given (samples, timestamps).
#![allow(dead_code)] // public surface is wired into daemon.rs by the caller

use crate::bias_state::{BiasClass, BiasSnapshot, EntrySnapshot, BIAS_STATE_VERSION};
use crate::sampler::SchedSample;
use std::collections::BTreeMap;
use std::time::SystemTime;

/// Confidence assigned when a closed-loop mismatch forces a re-baseline.
pub const LOW_CONFIDENCE: f64 = 0.25;

/// Confidence at or below this flushes to zero (zero confidence ⇒ zero bias).
const CONFIDENCE_EPS: f64 = 1e-9;

/// Each `RELAX_STEP` of confidence decay earns one release step toward 0.
const RELAX_STEP: f64 = 0.25;

/// EWMA time constant (seconds) of the population threshold T_us.
const THRESHOLD_TC_S: f64 = 60.0;

fn unix_secs(t: SystemTime) -> u64 {
    t.duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// Weight-free per-sample score (the loop breaker): lavd's lat_cri formula
/// shape with `weight_ft := 1`, so our applied nice can never feed back into
/// the signal that justifies the bias. `None` when any freq/runtime field is
/// zero (= absent under lenient parsing): such samples are skipped, never
/// guessed.
pub fn sample_score(s: &SchedSample) -> Option<u64> {
    if s.wait_freq == 0 || s.wake_freq == 0 || s.avg_runtime_wall == 0 {
        return None;
    }
    let wait_ft = s.wait_freq.min(100_000) + 1;
    let wake_ft = s.wake_freq.min(100_000) + 1;
    let runtime_ft = (1_000_000_000u64 / s.avg_runtime_wall.max(1)).max(1);
    let s0 = u64::from((wait_ft * wake_ft).ilog2() + runtime_ft.ilog2());
    Some(s0 * s0)
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
            attack_step_s: 60,
            release_step_s: 300,
            global_step_s: 5,
            starved_ratio: 0.5,
            starved_min_demand: 64,
        }
    }
}

#[derive(Debug, Clone)]
struct Entry {
    // persisted fields (mirrors EntrySnapshot)
    score_ema: f64,
    sample_count_ewma: f64,
    class: BiasClass,
    bias: i8,
    last_seen: u64,
    confidence: f64,
    frozen: bool,
    // internal
    /// Last nice value this daemon actually applied for the name (the
    /// closed-loop expectation; `None` = never actuated by us).
    applied_nice: Option<i8>,
    target: i8,
    /// Timestamp the stored confidence is valid at (aging anchor).
    aged_at: u64,
    /// Confidence watermark since the last relax step / evidence gain.
    relax_watermark: f64,
    cpu_util_ewma: f64,
    demand_ewma: f64,
    util_init: bool,
    starved: bool,
    win_score_sum: u64,
    win_score_n: u64,
    win_n: u64,
    win_cpu_sum: u64,
    win_util_sum: u64,
    win_util_n: u64,
    pending: Option<BiasClass>,
    streak: u32,
    want_since_attack: Option<u64>,
    want_since_release: Option<u64>,
    last_step_attack: Option<u64>,
    last_step_release: Option<u64>,
    bias_changed_at: Option<u64>,
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
    let relax_pending = e.bias != 0 && e.relax_watermark - e.confidence >= RELAX_STEP;
    let delta = i16::from(e.target) - i16::from(e.bias);
    let (mut attack_want, mut release_want) = (false, relax_pending);
    if delta != 0 {
        let next = e.bias + delta.signum() as i8;
        if next.abs() > e.bias.abs() {
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
            score_ema: 0.0,
            sample_count_ewma: 0.0,
            class: BiasClass::Unknown,
            bias: 0,
            last_seen: now,
            confidence: 0.0,
            frozen: false,
            target: 0,
            aged_at: now,
            relax_watermark: 0.0,
            cpu_util_ewma: 0.0,
            demand_ewma: 0.0,
            util_init: false,
            starved: false,
            win_score_sum: 0,
            win_score_n: 0,
            win_n: 0,
            win_cpu_sum: 0,
            win_util_sum: 0,
            win_util_n: 0,
            pending: None,
            streak: 0,
            want_since_attack: None,
            want_since_release: None,
            last_step_attack: None,
            last_step_release: None,
            bias_changed_at: None,
            applied_nice: None,
        }
    }

    fn reset_window(&mut self) {
        self.win_score_sum = 0;
        self.win_score_n = 0;
        self.win_n = 0;
        self.win_cpu_sum = 0;
        self.win_util_sum = 0;
        self.win_util_n = 0;
    }

    /// Closed-loop re-baseline after a frozen mismatch: unfreeze and drop
    /// confidence to low; the recorded bias and class stay until evidence or
    /// aging moves them.
    fn rebaseline(&mut self, now: u64) {
        self.frozen = false;
        self.confidence = LOW_CONFIDENCE;
        self.relax_watermark = LOW_CONFIDENCE;
        self.pending = None;
        self.streak = 0;
        self.want_since_attack = None;
        self.want_since_release = None;
        self.reset_window();
        self.aged_at = now;
    }

    fn close_window(
        &mut self,
        cfg: &BiasConfig,
        threshold: Option<f64>,
        alpha: f64,
        close_t: u64,
    ) {
        if self.win_n == 0 {
            // Absent this window: silence is never evidence. Streaks,
            // confidence and target all hold; wall-time aging handles decay.
            return;
        }
        self.aged_at = close_t;

        if self.win_util_n > 0 {
            let cpu = self.win_cpu_sum as f64 / self.win_util_n as f64;
            let dem = self.win_util_sum as f64 / self.win_util_n as f64;
            if self.util_init {
                self.cpu_util_ewma += (cpu - self.cpu_util_ewma) * alpha;
                self.demand_ewma += (dem - self.demand_ewma) * alpha;
            } else {
                self.cpu_util_ewma = cpu;
                self.demand_ewma = dem;
                self.util_init = true;
            }
        }
        self.starved = self.demand_ewma > cfg.starved_min_demand as f64
            && self.cpu_util_ewma < cfg.starved_ratio * self.demand_ewma;

        if self.sample_count_ewma == 0.0 {
            self.sample_count_ewma = self.win_n as f64;
        } else {
            self.sample_count_ewma += (self.win_n as f64 - self.sample_count_ewma) * alpha;
        }
        let score_mean = if self.win_score_n > 0 {
            let mean = self.win_score_sum as f64 / self.win_score_n as f64;
            if self.score_ema == 0.0 {
                self.score_ema = mean;
            } else {
                self.score_ema += (mean - self.score_ema) * alpha;
            }
            Some(mean)
        } else {
            None
        };

        if self.win_score_n >= cfg.min_samples {
            if let (Some(mean), Some(th)) = (score_mean, threshold) {
                let low = th * ((100.0 - cfg.deadband_pct as f64) / 100.0);
                let high = th * ((100.0 + cfg.deadband_pct as f64) / 100.0);
                let cand = if mean < low {
                    Some(BiasClass::Bulk)
                } else if mean > high {
                    Some(BiasClass::Responsive)
                } else {
                    None
                };
                match cand {
                    Some(c) => {
                        if self.pending == Some(c) {
                            self.streak += 1;
                        } else {
                            self.pending = Some(c);
                            self.streak = 1;
                        }
                        if self.streak >= 2 {
                            self.class = c;
                            self.pending = None;
                            self.streak = 0;
                        }
                        self.confidence += (1.0 - self.confidence) * 0.5;
                        self.relax_watermark = self.relax_watermark.max(self.confidence);
                    }
                    None => {
                        // Deadband: class holds, but the bias relaxes toward 0
                        // through the release rule (target 0).
                        self.pending = None;
                        self.streak = 0;
                    }
                }
                self.target = if cand.is_some() {
                    target_for(cfg, self.class, self.starved)
                } else {
                    0
                };
            }
        } else {
            // Observed but too sparse to classify: hold the class, shed
            // confidence (magnitude unspecified in the brief: halve).
            self.confidence *= 0.5;
            self.pending = None;
            self.streak = 0;
        }
        self.reset_window();
    }

    fn release_ready(&self, cfg: &BiasConfig, now: u64) -> bool {
        self.want_since_release.is_some_and(|w| {
            now >= w.saturating_add(cfg.release_step_s)
                && self
                    .last_step_release
                    .is_none_or(|l| now >= l.saturating_add(cfg.release_step_s))
        })
    }

    fn attack_ready(&self, cfg: &BiasConfig, now: u64) -> bool {
        self.want_since_attack.is_some_and(|w| {
            now >= w.saturating_add(cfg.attack_step_s)
                && self
                    .last_step_attack
                    .is_none_or(|l| now >= l.saturating_add(cfg.attack_step_s))
        })
    }
}

/// Per-name estimator + controller with injectable time.
#[derive(Debug)]
pub struct BiasEngine {
    cfg: BiasConfig,
    entries: BTreeMap<String, Entry>,
    /// Population threshold T_us (EWMA of mean score), if known.
    threshold: Option<f64>,
    window_end: Option<u64>,
    pop_score_sum: u64,
    pop_score_n: u64,
    /// Names with samples in the current window (fast empty-window close).
    win_names: u64,
    last_global_step: Option<u64>,
    last_now: u64,
}

impl BiasEngine {
    pub fn new(mut cfg: BiasConfig) -> Self {
        // Library-level invariant: i8::clamp panics on an inverted range.
        // Config::parse already warns and resets; normalize here too so any
        // other caller is safe.
        if cfg.min_bias > cfg.max_bias {
            std::mem::swap(&mut cfg.min_bias, &mut cfg.max_bias);
        }
        BiasEngine {
            cfg,
            entries: BTreeMap::new(),
            threshold: None,
            window_end: None,
            pop_score_sum: 0,
            pop_score_n: 0,
            win_names: 0,
            last_global_step: None,
            last_now: 0,
        }
    }

    /// Ingest one sampler batch observed at `now`.
    pub fn record_batch(&mut self, batch: &[SchedSample], now: SystemTime) {
        let t = unix_secs(now);
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
            let e = self
                .entries
                .entry(s.comm.clone())
                .or_insert_with(|| Entry::new(t));
            // Frozen by closed-loop mismatch: no evidence updates until a
            // matching sample un-freezes (verify_applied); aging still
            // relaxes the stored bias toward 0 meanwhile.
            if e.frozen {
                continue;
            }
            if e.win_n == 0 {
                self.win_names += 1;
            }
            e.last_seen = t;
            e.win_n += 1;
            e.win_cpu_sum = e.win_cpu_sum.saturating_add(s.cpu_util_wall);
            e.win_util_sum = e.win_util_sum.saturating_add(s.task_util_est);
            e.win_util_n += 1;
            if let Some(score) = sample_score(s) {
                e.win_score_sum = e.win_score_sum.saturating_add(score);
                e.win_score_n += 1;
                self.pop_score_sum = self.pop_score_sum.saturating_add(score);
                self.pop_score_n += 1;
            }
        }
        self.enforce_cap();
        self.slew_pass(t, true);
    }

    /// Wall-time aging: confidence decays while a name is unseen (half-life),
    /// decay releases bias toward 0 one step per `RELAX_STEP`, and zero
    /// confidence forces bias = 0. Never attacks — an absent stream must only
    /// relax. The owner calls this on its own cadence.
    pub fn age(&mut self, now: SystemTime) {
        let t = unix_secs(now);
        self.last_now = self.last_now.max(t);
        let hl = self.cfg.half_life_s.max(1) as f64;
        for e in self.entries.values_mut() {
            let base = e.aged_at.max(e.last_seen);
            let dt = t.saturating_sub(base);
            if dt > 0 {
                e.confidence *= 0.5f64.powf(dt as f64 / hl);
                e.aged_at = t;
            }
            if e.confidence < CONFIDENCE_EPS {
                e.confidence = 0.0;
            }
            if e.confidence == 0.0 && e.bias != 0 {
                // History must never permanently fix the bias.
                e.bias = 0;
                e.relax_watermark = 0.0;
                e.want_since_attack = None;
                e.want_since_release = None;
                e.bias_changed_at = Some(t);
            }
        }
        self.slew_pass(t, false);
        self.enforce_cap();
    }

    /// Applied nice = `clamp(base_nice + bias, -20, 19)`, recomputed from the
    /// base every call (idempotent — never increments on itself).
    pub fn target_nice(&self, name: &str, base_nice: i8) -> i8 {
        let bias = self.entries.get(name).map_or(0, |e| e.bias);
        (i16::from(base_nice) + i16::from(bias)).clamp(-20, 19) as i8
    }

    /// Closed-loop check against the nice this daemon last applied for
    /// `name`. Observed nice = `observed_static_prio - 20` (lavd's sample
    /// field equals nice + 20). Semantics:
    /// - names we never applied are not verified (their ambient nice is not
    ///   ours to compare against);
    /// - `static_prio == 0` (nice −20) is indistinguishable from the lenient
    ///   default of an absent field — skip rather than fabricate a mismatch;
    /// - mismatch freezes the entry (no evidence updates); a later matching
    ///   sample re-baselines it (unfreeze, confidence to low). A persistent
    ///   mismatch therefore cannot pin the bias: aging still flushes it.
    pub fn verify_applied(&mut self, name: &str, observed_static_prio: u64) {
        if observed_static_prio == 0 {
            return;
        }
        let Some(e) = self.entries.get_mut(name) else {
            return;
        };
        let Some(expected_nice) = e.applied_nice else {
            return;
        };
        let observed_nice = observed_static_prio as i64 - 20;
        if observed_nice == i64::from(expected_nice) {
            if e.frozen {
                e.rebaseline(self.last_now);
            }
        } else if !e.frozen {
            e.frozen = true;
        }
    }

    /// Record the nice this daemon applied for `name` (`None` when the apply
    /// did not take, e.g. Denied/Failed — then verification is skipped).
    pub fn note_applied(&mut self, name: &str, nice: Option<i8>) {
        if let Some(e) = self.entries.get_mut(name) {
            e.applied_nice = nice;
        }
    }

    /// The nice this daemon last applied for `name`, if any.
    pub fn applied_nice(&self, name: &str) -> Option<i8> {
        self.entries.get(name).and_then(|e| e.applied_nice)
    }

    /// Serializable state for persistence (see `bias_state::save`).
    pub fn snapshot(&self) -> BiasSnapshot {
        BiasSnapshot {
            version: BIAS_STATE_VERSION,
            threshold_ema: self.threshold,
            entries: self
                .entries
                .iter()
                .map(|(name, e)| EntrySnapshot {
                    name: name.clone(),
                    score_ema: e.score_ema,
                    sample_count_ewma: e.sample_count_ewma,
                    class: e.class,
                    bias: e.bias,
                    last_seen: e.last_seen,
                    confidence: e.confidence,
                    frozen: e.frozen,
                })
                .collect(),
        }
    }

    /// Restore persisted state (startup). Aging resumes from each entry's
    /// `last_seen`; frozen entries stay frozen until a matching sample
    /// re-baselines them.
    pub fn restore(&mut self, snapshot: BiasSnapshot) {
        self.threshold = snapshot.threshold_ema;
        for es in snapshot.entries {
            let confidence = es.confidence.clamp(0.0, 1.0);
            let mut e = Entry::new(es.last_seen);
            e.score_ema = es.score_ema;
            e.sample_count_ewma = es.sample_count_ewma;
            e.class = es.class;
            e.bias = es.bias.clamp(self.cfg.min_bias, self.cfg.max_bias);
            e.confidence = confidence;
            e.frozen = es.frozen;
            e.relax_watermark = confidence;
            e.aged_at = es.last_seen;
            // Starved status is rebuilt from fresh samples; restored
            // RESPONSIVE entries target 0 until evidence says otherwise.
            e.target = target_for(&self.cfg, es.class, false);
            self.entries.insert(es.name, e);
        }
        self.last_now = self
            .entries
            .values()
            .map(|e| e.last_seen)
            .max()
            .unwrap_or(0);
        self.enforce_cap();
    }

    /// Names whose bias moved since the last call — each move logs exactly
    /// once (consuming the marks is what rate-limits the summary line, so
    /// marks are never dropped for being stale, e.g. moves that happened
    /// while the sample stream was down).
    pub fn take_changed_recently(&mut self) -> Vec<(String, i8)> {
        let mut out = Vec::new();
        for (name, e) in self.entries.iter_mut() {
            if e.bias_changed_at.take().is_some() {
                out.push((name.clone(), e.bias));
            }
        }
        out
    }

    fn close_window(&mut self, close_t: u64) {
        let alpha = 1.0 - (-(self.cfg.window_s.max(1) as f64) / THRESHOLD_TC_S).exp();
        if self.pop_score_n > 0 || self.win_names > 0 {
            if self.pop_score_n > 0 {
                let mean = self.pop_score_sum as f64 / self.pop_score_n as f64;
                self.threshold = Some(match self.threshold {
                    Some(th) => th + (mean - th) * alpha,
                    None => mean,
                });
            }
            let threshold = self.threshold;
            for e in self.entries.values_mut() {
                e.close_window(&self.cfg, threshold, alpha, close_t);
            }
        }
        self.pop_score_sum = 0;
        self.pop_score_n = 0;
        self.win_names = 0;
    }

    /// Apply slew limits. `allow_attack = false` (aging) only releases.
    /// The shared global budget buys at most one step per pass; the winner is
    /// chosen FIFO by want-onset (release outranks attack, name breaks ties)
    /// and re-arms its want after stepping, so a tight lexicographic order can
    /// never starve later names. Deterministic given (samples, timestamps).
    fn slew_pass(&mut self, now: u64, allow_attack: bool) {
        for e in self.entries.values_mut() {
            if e.bias == 0 && e.relax_watermark - e.confidence >= RELAX_STEP {
                e.relax_watermark = e.confidence; // nothing left to relax
            }
            let (attack_want, release_want, _) = direction_wants(e);
            e.want_since_release = want_stamp(e.want_since_release, release_want, now);
            e.want_since_attack = want_stamp(e.want_since_attack, attack_want, now);
        }

        let budget_used = self
            .last_global_step
            .is_some_and(|g| now < g.saturating_add(self.cfg.global_step_s));
        if budget_used {
            return;
        }

        // (rank, want-onset, name, is_release): 0 = release, 1 = attack.
        let mut best: Option<(u8, u64, String, bool)> = None;
        for (name, e) in &self.entries {
            let (attack_want, release_want, _) = direction_wants(e);
            if release_want && e.release_ready(&self.cfg, now) {
                let key = (0, e.want_since_release.unwrap_or(now), name.clone(), true);
                if best.as_ref().is_none_or(|b| key < *b) {
                    best = Some(key);
                }
            }
            if allow_attack && attack_want && e.attack_ready(&self.cfg, now) {
                let key = (1, e.want_since_attack.unwrap_or(now), name.clone(), false);
                if best.as_ref().is_none_or(|b| key < *b) {
                    best = Some(key);
                }
            }
        }

        if let Some((_, _, name, is_release)) = best {
            let e = self.entries.get_mut(&name).expect("selected entry exists");
            if is_release {
                e.bias -= e.bias.signum();
                e.last_step_release = Some(now);
                e.want_since_release = Some(now); // re-arm: back of the FIFO
                e.relax_watermark = e.confidence;
            } else {
                let delta = i16::from(e.target) - i16::from(e.bias);
                e.bias = (e.bias + delta.signum() as i8).clamp(self.cfg.min_bias, self.cfg.max_bias);
                e.last_step_attack = Some(now);
                e.want_since_attack = Some(now); // re-arm: back of the FIFO
            }
            e.bias_changed_at = Some(now);
            self.last_global_step = Some(now);
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
            .map(|(name, e)| (e.last_seen, name.clone()))
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

    fn tick(eng: &mut BiasEngine, t: u64) {
        eng.record_batch(&[], ts(t));
    }

    fn sample(comm: &str, wait: u64, wake: u64, rt: u64, util: u64, cpu: u64) -> SchedSample {
        SchedSample {
            pid: 1,
            comm: comm.to_string(),
            wait_freq: wait,
            wake_freq: wake,
            avg_runtime_wall: rt,
            task_util_est: util,
            cpu_util_wall: cpu,
            static_prio: 20,
            ..Default::default()
        }
    }

    // Hand-computed score fixtures (see score_fixtures_and_skip):
    //   400 = 20²  : ilog2(1024·1024) = 20, runtime_ft = 1
    //   100 = 10²  : ilog2(32·32) = 10, runtime_ft = 1
    //   784 = 28²  : 20 + ilog2(256) = 28
    fn noise_batch(n: usize) -> Batch {
        (0..n)
            .map(|_| sample("noise", 1023, 1023, 1_000_000_000, 500, 500))
            .collect()
    }
    fn bulk_batch(name: &str, n: usize) -> Batch {
        (0..n)
            .map(|_| sample(name, 31, 31, 1_000_000_000, 500, 500))
            .collect()
    }
    fn resp_batch(name: &str, n: usize) -> Batch {
        (0..n)
            .map(|_| sample(name, 1023, 1023, 3_906_250, 500, 500))
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

    fn snap_entry(eng: &BiasEngine, name: &str) -> EntrySnapshot {
        eng.snapshot()
            .entries
            .into_iter()
            .find(|e| e.name == name)
            .expect("entry present")
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
        eng.record_batch(&w1, ts(1));
        eng.record_batch(&w2, ts(61));
        eng.record_batch(&[], ts(121));
    }

    fn drive_bulk(eng: &mut BiasEngine, names: &[&str]) {
        drive_class(eng, names, bulk_batch);
    }

    /// Three attack steps at want+60/120/180: bias 0 → 3.
    fn ramp_attack(eng: &mut BiasEngine) {
        tick(eng, 181);
        tick(eng, 241);
        tick(eng, 301);
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
        // zero (= absent) freq/runtime fields: skipped, never guessed
        assert_eq!(sample_score(&sample("x", 0, 100, 1_000_000_000, 0, 0)), None);
        assert_eq!(sample_score(&sample("x", 100, 0, 1_000_000_000, 0, 0)), None);
        assert_eq!(sample_score(&sample("x", 100, 100, 0, 0, 0)), None);
    }

    #[test]
    fn actuator_independence() {
        // Same behavior at nice 0 and nice +3 must score identically: the
        // score ignores static_prio and the lavd lat_cri fields entirely, so
        // our own bias cannot feed the signal that justifies the bias.
        let mut at_base = sample("x", 1023, 1023, 3_906_250, 500, 500);
        at_base.static_prio = 20; // nice 0
        at_base.lat_cri = 1_000;
        at_base.avg_lat_cri = 500;
        let mut at_plus3 = at_base.clone();
        at_plus3.static_prio = 23; // nice +3
        at_plus3.lat_cri = 90_000;
        at_plus3.avg_lat_cri = 77_000; // lavd-side signal moved with weight
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
            eng.record_batch(&w1, ts(1));
            eng.record_batch(&w2, ts(61));
            eng.record_batch(&[], ts(121));
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

        eng.record_batch(&w1, ts(1));
        eng.record_batch(&w2, ts(61)); // closes w1: 1 window below threshold
        assert_eq!(snap_entry(&eng, "app").class, BiasClass::Unknown);

        eng.record_batch(&[], ts(121)); // closes w2: 2nd window → BULK
        assert_eq!(snap_entry(&eng, "app").class, BiasClass::Bulk);

        // crossing into the deadband holds the class
        let mut w3 = noise_batch(60);
        w3.extend(dead_batch("app", 32));
        eng.record_batch(&w3, ts(181));
        eng.record_batch(&[], ts(241)); // closes w3: deadband
        assert_eq!(snap_entry(&eng, "app").class, BiasClass::Bulk);
    }

    #[test]
    fn hysteresis_responsive_symmetric() {
        let mut eng = BiasEngine::new(BiasConfig::default());
        let mut w1 = noise_batch(60);
        w1.extend(resp_batch("app", 32));
        let mut w2 = noise_batch(60);
        w2.extend(resp_batch("app", 32));

        eng.record_batch(&w1, ts(1));
        eng.record_batch(&w2, ts(61));
        assert_eq!(snap_entry(&eng, "app").class, BiasClass::Unknown);
        eng.record_batch(&[], ts(121));
        assert_eq!(snap_entry(&eng, "app").class, BiasClass::Responsive);

        let mut w3 = noise_batch(60);
        w3.extend(dead_batch("app", 32));
        eng.record_batch(&w3, ts(181));
        eng.record_batch(&[], ts(241));
        assert_eq!(snap_entry(&eng, "app").class, BiasClass::Responsive);
    }

    #[test]
    fn slew_attack_and_release() {
        let mut eng = BiasEngine::new(BiasConfig::default());
        drive_bulk(&mut eng, &["app"]); // class Bulk at t=121, wants armed

        // attack: at most 1 step / 60 s, first step 60 s after want-onset
        assert_eq!(snap_entry(&eng, "app").bias, 0);
        tick(&mut eng, 180);
        assert_eq!(snap_entry(&eng, "app").bias, 0);
        tick(&mut eng, 181);
        assert_eq!(snap_entry(&eng, "app").bias, 1);
        tick(&mut eng, 240);
        assert_eq!(snap_entry(&eng, "app").bias, 1);
        tick(&mut eng, 241);
        assert_eq!(snap_entry(&eng, "app").bias, 2);
        tick(&mut eng, 300);
        assert_eq!(snap_entry(&eng, "app").bias, 2);
        tick(&mut eng, 301);
        assert_eq!(snap_entry(&eng, "app").bias, 3); // 3 steps × 60 s
        tick(&mut eng, 400);
        assert_eq!(snap_entry(&eng, "app").bias, 3); // clamped at max

        // deadband window → target 0 → release at most 1 step / 300 s
        let mut w = noise_batch(60);
        w.extend(dead_batch("app", 32));
        eng.record_batch(&w, ts(420));
        eng.record_batch(&[], ts(480)); // closes deadband window (want at 480)
        tick(&mut eng, 700);
        assert_eq!(snap_entry(&eng, "app").bias, 3);
        tick(&mut eng, 780);
        assert_eq!(snap_entry(&eng, "app").bias, 2);
        tick(&mut eng, 1080);
        assert_eq!(snap_entry(&eng, "app").bias, 1);
        tick(&mut eng, 1380);
        assert_eq!(snap_entry(&eng, "app").bias, 0); // 3 steps × 300 s: slower
    }

    #[test]
    fn global_step_budget_one_per_five_seconds() {
        let mut eng = BiasEngine::new(BiasConfig::default());
        drive_bulk(&mut eng, &["aaa", "bbb"]); // both Bulk, wants armed at 121

        tick(&mut eng, 181);
        assert_eq!(snap_entry(&eng, "aaa").bias, 1); // sorted order spends budget
        assert_eq!(snap_entry(&eng, "bbb").bias, 0);
        tick(&mut eng, 182);
        assert_eq!(snap_entry(&eng, "aaa").bias, 1);
        assert_eq!(snap_entry(&eng, "bbb").bias, 0); // within the 5 s cap
        tick(&mut eng, 186);
        assert_eq!(snap_entry(&eng, "bbb").bias, 1); // budget free again
        assert_eq!(snap_entry(&eng, "aaa").bias, 1);
    }

    #[test]
    fn starved_gate_only_low_minus_one() {
        // RESPONSIVE + starved (delivered 100 « 50% of demand 1000) → −1
        let mut eng = BiasEngine::new(BiasConfig::default());
        drive_class(&mut eng, &["starved"], starved_batch);
        assert_eq!(snap_entry(&eng, "starved").class, BiasClass::Responsive);
        ramp_attack(&mut eng);
        assert_eq!(snap_entry(&eng, "starved").bias, -1);
        tick(&mut eng, 400);
        assert_eq!(snap_entry(&eng, "starved").bias, -1); // clamped at min

        // RESPONSIVE but delivered == demand → target 0, no −1
        let mut eng2 = BiasEngine::new(BiasConfig::default());
        drive_class(&mut eng2, &["ok"], resp_batch);
        ramp_attack(&mut eng2);
        tick(&mut eng2, 400);
        assert_eq!(snap_entry(&eng2, "ok").bias, 0);

        // bad ratio but demand below the noise floor → gate stays shut
        let mut eng3 = BiasEngine::new(BiasConfig::default());
        drive_class(&mut eng3, &["tiny"], tiny_demand_batch);
        ramp_attack(&mut eng3);
        tick(&mut eng3, 400);
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
        eng.age(ts(121 + 1800));
        let e = snap_entry(&eng, "app");
        assert!((e.confidence - 0.375).abs() < 1e-12);
        assert_eq!(e.bias, 3); // release is rate-limited: still parked

        // continued absence: confidence keeps decaying, bias relaxes toward 0
        // through release steps and finally the zero-confidence reset — the
        // stream being absent never freezes history.
        let mut t = 121 + 1800;
        while t < 121 + 200_000 {
            t += 100;
            eng.age(ts(t));
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

        eng.age(ts(121 + 1800 * 80)); // decay underflows the epsilon in one call
        let e = snap_entry(&eng, "app");
        assert_eq!(e.confidence, 0.0);
        assert_eq!(e.bias, 0);
    }

    #[test]
    fn entry_cap_evicts_least_recently_seen() {
        let cfg = BiasConfig {
            max_entries: 4,
            ..BiasConfig::default()
        };
        let mut eng = BiasEngine::new(cfg);
        for (i, name) in ["a", "b", "c", "d", "e"].iter().enumerate() {
            eng.record_batch(&bulk_batch(name, 1), ts(1 + i as u64));
        }
        eng.age(ts(100));
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
        eng.verify_applied("app", 20); // observed nice 0 ≠ 3 would mismatch
        assert!(!snap_entry(&eng, "app").frozen);
        // Actuated now — the remembered value is the expectation.
        eng.note_applied("app", Some(3));
        assert_eq!(eng.applied_nice("app"), Some(3));
        // static_prio 0 (nice −20 / absent field default) → skip verification.
        eng.verify_applied("app", 0);
        assert!(!snap_entry(&eng, "app").frozen);
        // Releasing the claim stops verification again.
        eng.note_applied("app", None);
        assert_eq!(eng.applied_nice("app"), None);
        eng.verify_applied("app", 20);
        assert!(!snap_entry(&eng, "app").frozen);
        // Unknown names are ignored (no entry created).
        eng.verify_applied("ghost", 23);
        assert!(eng.snapshot().entries.iter().all(|e| e.name != "ghost"));
    }

    #[test]
    fn verify_mismatch_freezes_until_a_matching_sample_rebaselines() {
        let mut eng = BiasEngine::new(BiasConfig::default());
        drive_bulk(&mut eng, &["app"]);
        ramp_attack(&mut eng); // bias 3
        eng.note_applied("app", Some(3));
        assert_eq!(eng.target_nice("app", 0), 3);

        // observed nice 0 (static_prio 20) ≠ applied 3 → freeze
        eng.verify_applied("app", 20);
        let e = snap_entry(&eng, "app");
        assert!(e.frozen);
        assert_eq!(e.bias, 3);
        assert_eq!(e.confidence, 0.75);

        // samples for other names leave the frozen entry untouched
        eng.record_batch(&bulk_batch("other", 5), ts(361));
        let e = snap_entry(&eng, "app");
        assert!(e.frozen);
        assert_eq!(e.confidence, 0.75);
        assert_eq!(e.bias, 3);

        // a persistent mismatch never re-baselines: samples are dropped and
        // confidence must not churn at LOW on every batch
        eng.record_batch(&bulk_batch("app", 5), ts(362));
        eng.verify_applied("app", 20);
        let e = snap_entry(&eng, "app");
        assert!(e.frozen);
        assert_eq!(e.confidence, 0.75);

        // a matching observation re-baselines: unfreeze + low confidence,
        // bias kept (evidence/aging will move it from here)
        eng.verify_applied("app", 23); // nice 23 − 20 == 3
        let e = snap_entry(&eng, "app");
        assert!(!e.frozen);
        assert_eq!(e.confidence, LOW_CONFIDENCE);
        assert_eq!(e.bias, 3);
    }

    #[test]
    fn persistent_mismatch_cannot_pin_the_bias() {
        let mut eng = BiasEngine::new(BiasConfig::default());
        drive_bulk(&mut eng, &["app"]);
        ramp_attack(&mut eng); // bias 3
        eng.note_applied("app", Some(3));
        eng.verify_applied("app", 20); // mismatch → freeze
        // Samples keep flowing while the loop keeps lying: the entry stays
        // frozen and un-updated...
        for i in 0..5u64 {
            eng.record_batch(&bulk_batch("app", 5), ts(400 + i * 60));
            eng.verify_applied("app", 20);
            assert!(snap_entry(&eng, "app").frozen);
        }
        // ...but aging still flushes the bias: old learnings never pin it.
        let mut t = 400 + 5 * 60;
        while t < 200_000 {
            t += 100;
            eng.age(ts(t));
        }
        assert_eq!(snap_entry(&eng, "app").bias, 0);
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
            entries: vec![crate::bias_state::EntrySnapshot {
                name: "app".into(),
                bias: 4,
                ..crate::bias_state::EntrySnapshot::default()
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
    fn changed_recently_logs_each_move_exactly_once() {
        let mut eng = BiasEngine::new(BiasConfig::default());
        drive_bulk(&mut eng, &["app"]);
        tick(&mut eng, 181); // app bias 0 → 1 at T0+181
        assert_eq!(eng.take_changed_recently(), vec![("app".to_string(), 1)]);
        // consuming the marks keeps the summary line from repeating
        assert!(eng.take_changed_recently().is_empty());
        // stale marks (e.g. moves during a stream outage) are still reported
        // exactly once — the window must not swallow them
        tick(&mut eng, 241); // app bias 1 → 2
        eng.age(ts(241 + 10 * 60)); // far beyond window_s
        assert_eq!(eng.take_changed_recently(), vec![("app".to_string(), 2)]);
        assert!(eng.take_changed_recently().is_empty());
    }

    #[test]
    fn snapshot_restore_round_trip_preserves_state() {
        let mut eng = BiasEngine::new(BiasConfig::default());
        drive_bulk(&mut eng, &["app"]);
        ramp_attack(&mut eng);
        let snap = eng.snapshot();

        let mut eng2 = BiasEngine::new(BiasConfig::default());
        eng2.restore(snap.clone());
        assert_eq!(eng2.snapshot(), snap);
        assert_eq!(eng2.target_nice("app", 0), 3);

        // aging resumes across the restart from each entry's last_seen
        let before = snap_entry(&eng2, "app").confidence;
        eng2.age(ts(61 + 1800));
        let after = snap_entry(&eng2, "app").confidence;
        assert!((after - before * 0.5).abs() < 1e-12);
    }
}

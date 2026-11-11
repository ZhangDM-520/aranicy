//! Policy synthesis: *what* to apply to one process, and the single place
//! that accounts for the result.
//!
//! The exec path and the drift corrector used to carry two copies of this
//! logic, and the copies drifted (drift did not touch `matched`/`unmatched`,
//! logged at a different level, and had to remember the claim ordering
//! itself). Here the knowledge lives once:
//!
//! * [`plan`] decides. A process is *managed* when the rule dictates a nice,
//!   when a bias is active, or when we previously claimed a nice and the
//!   target moved (relaxation must reach running processes). A matched rule
//!   with no `nice` and no bias is still applied at exec time (its
//!   cgroup/sched dimensions are due), but the drift corrector leaves it
//!   alone — [`settle`] re-asserts only a `nice` that actually moved.
//! * [`execute`] applies, tallies, records the closed-loop claim, and logs.
//!   The order is apply → tally → claim: the claim records what the nice
//!   *became*, so the closed loop can compare the next sample against it.
//!
//! # Names
//!
//! A decision straddles two namespaces, and swapping them is silent, so they
//! travel together in a [`BiasKey`]: rules match the **match name**
//! (`basename(/proc/<pid>/exe)`, then `argv[0]`, then comm, resolved by the
//! collector at capture time), bias
//! history is filed under the **comm** (the kernel task name). They differ in
//! real processes (an Electron binary execs `/usr/bin/electron` while its
//! threads stay `Chrome_ChildIOT`).
//!
//! # Counters and the 60 s summary
//!
//! [`Counters`] is instantiated once per route (`exec`, `drift`) rather than
//! shared: a drift scan re-plans *every* pid on the host each cadence, so a
//! shared `matched` would be dominated by scan traffic (hundreds per minute
//! against a handful of execs) and the event counters would stop being
//! comparable. Both sets are produced by the same [`execute`], and the
//! summary line labels them apart (`events/matched/unmatched/unresolved` for
//! exec, `drift_seen/drift_managed/drift_unmatched` for drift). `seen` means
//! "decisions observed": exec events on the exec route, pids examined on the
//! drift route.
//!
//! # Hot path
//!
//! No `String`/`format!` per decision: names stay borrowed `&str` and the
//! plan owns a [`Policy`] clone (refcount bumps on its two `Arc<str>`
//! fields, no allocation). Formatting happens only when a summary is emitted.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use aranicy_apply::{apply_policy, ActionOutcome, ApplyReport, Capabilities, CgroupManager};
use aranicy_model::{IoClass, Policy, SchedPolicy};
use aranicy_rules::RuleSet;

use crate::bias::BiasEngine;
use crate::config::Config;

const DIMS: [&str; 8] = [
    "nice",
    "latency_nice",
    "sched",
    "ionice",
    "oom_score_adj",
    "cgroup",
    "cpuset",
    "autogroup",
];

/// The two names of one process, bundled so a caller cannot swap them.
///
/// [`BiasKey::comm`] is the history key (engine lookups, the closed-loop
/// claim); [`BiasKey::name`] is the rule-match name and the operator log's
/// `rule` label.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BiasKey<'a> {
    name: &'a str,
    comm: &'a str,
}

impl<'a> BiasKey<'a> {
    /// The bias-history key: the kernel comm.
    pub fn comm(&self) -> &'a str {
        self.comm
    }

    /// The rule-match name (`basename(/proc/<pid>/exe)`, then `argv[0]`, then
    /// comm).
    pub fn name(&self) -> &'a str {
        self.name
    }
}

/// What to apply for one process, or why nothing is due.
#[derive(Debug, PartialEq)]
pub enum Plan<'a> {
    /// Apply `policy` to one process. `target` is the nice the decision
    /// asks for, `base` the rule's nice (0 when no rule dictates one) — the
    /// log reports their difference as the bias.
    Apply {
        policy: Policy,
        target: i8,
        base: i8,
        key: BiasKey<'a>,
    },
    /// Nothing is due; `reason` selects the counter that records it.
    Skip { reason: Skip },
}

/// Why a decision produced no policy. Named rather than boolean so the
/// counters are produced in one place ([`execute`]) instead of five.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Skip {
    /// The match name could not be resolved (the process was already gone).
    Unresolved,
    /// The match name is not UTF-8, so it can never equal a JSON rule name.
    NonUtf8,
    /// No rule matched and no bias applies: not ours to touch.
    NoRule,
    /// Drift only: the process already has the nice the plan asks for.
    NoChange,
}

/// When to claim nice for a process: the rule dictates a nice (always
/// managed, drift re-asserts it), a bias is active (target ≠ 0), or we
/// previously claimed this name and the target moved (relaxation back to base
/// must reach running processes). A matched rule with no `nice` and no bias
/// leaves the ambient nice alone — it is not ours to overwrite.
fn eff_nice_for(rule_nice: Option<i8>, target: i8, applied: Option<i8>) -> Option<i8> {
    if rule_nice.is_some() || target != 0 || applied.is_some_and(|a| a != target) {
        Some(target)
    } else {
        None
    }
}

/// Whether the learned axis may move this process at all.
///
/// PID 1 is handled above (it is never a target at all); this is the name
/// axis: [`Config::bias_protect`], matched as a comm prefix because `comm` is
/// capped at 15 bytes.
fn bias_may_apply(cfg: &Config, name: &str, comm: &str, pid: u32) -> bool {
    // The daemon's own process is where the policy is executed, not where it
    // is applied: biasing it fights the unit's `Nice=` (measured: the unit's
    // -5 became +3 under load) and there is no way to notice from outside.
    if pid == std::process::id() {
        return false;
    }
    // Both identities, because rules match the executable while the history
    // is keyed by comm: a protected name reached through a differently named
    // entry point must still be protected.
    !cfg.bias_protect
        .iter()
        .any(|p| name.starts_with(p.as_str()) || comm.starts_with(p.as_str()))
}

/// Decide for one process seen at `pid`.
///
/// `name` is the match name bytes (capture-time resolution, or the local
/// resolver's fallback) and `comm` the kernel task name bytes. Both stay
/// borrowed: the returned plan lives as long as they do.
pub fn plan<'a>(
    name: &'a [u8],
    comm: &'a [u8],
    pid: u32,
    rules: &RuleSet,
    bias: &BiasEngine,
    cfg: &Config,
) -> Plan<'a> {
    if name.is_empty() {
        return Plan::Skip {
            reason: Skip::Unresolved,
        };
    }
    let Ok(name) = std::str::from_utf8(name) else {
        return Plan::Skip {
            reason: Skip::NonUtf8,
        };
    };
    // A comm that is not UTF-8 still has to name a history entry: everything
    // the collector reports is a kernel comm, and the match name is the
    // closest valid stand-in (both callers did this, so it lives here).
    let comm = std::str::from_utf8(comm).unwrap_or(name);

    // Init is never a policy target, rule or no rule: it owns the whole
    // machine, and "no rule corpus is worth renicing PID 1". Anything an
    // older daemon left there is put back to the kernel default.
    if pid == 1 {
        return Plan::Apply {
            policy: Policy {
                nice: Some(0),
                ..Policy::default()
            },
            target: 0,
            base: 0,
            key: BiasKey { name, comm },
        };
    }

    // The daemon's own process is where the policy executes, not where it
    // applies: a corpus entry named `aranicy` used to walk the daemon from the
    // unit's Nice=-5 to +19 and the unit kept claiming -5. Same for the
    // collector (its comm is in the default protect list).
    if pid == std::process::id() {
        return Plan::Skip {
            reason: Skip::NoRule,
        };
    }

    let matched = rules.lookup(name);
    let base = matched.as_ref().map_or(0, |(p, _)| p.nice.unwrap_or(0));
    // `"bias": false` on the matched rule is the per-name opt-out: the corpus
    // pins the process and the learner may not actuate on it. It follows the
    // rule's identity (the executable), where `bias_protect` is a global comm
    // prefix list — either one silences the axis.
    let rule_opted_out = matched
        .as_ref()
        .is_some_and(|(p, _)| p.bias == Some(false));
    // The learned axis is a bet on the workload. PID 1 and the core system
    // services are not a workload: losing the journal or the session manager
    // loses the machine, and nothing in the rules asked for it. An explicit
    // rule still applies to them — the operator asked for that by name.
    let (target, applied) = if !cfg.bias_enable {
        (base, None)
    } else if !rule_opted_out && bias_may_apply(cfg, name, comm, pid) {
        (bias.target_nice(comm, base), bias.applied_nice(comm))
    } else {
        // No new bias, but a claim recorded before this name was protected
        // (or before an upgrade changed the default list) must still unwind:
        // carrying `applied` is what lets `eff_nice_for` ask for `base`.
        (base, bias.applied_nice(comm))
    };
    let eff_nice = eff_nice_for(matched.as_ref().and_then(|(p, _)| p.nice), target, applied);
    let key = BiasKey { name, comm };
    tracing::trace!(pid, name, comm, target, base, "decision");

    match matched {
        // The rule already dictates this nice: the clone is the cheapest way
        // to hand the policy out (Arc refcount bumps, no allocation).
        Some((p, _)) if eff_nice == p.nice => Plan::Apply {
            policy: (**p).clone(),
            target,
            base,
            key,
        },
        Some((p, _)) => Plan::Apply {
            policy: Policy {
                nice: eff_nice,
                ..(**p).clone()
            },
            target,
            base,
            key,
        },
        None if eff_nice.is_some() => Plan::Apply {
            policy: Policy {
                nice: eff_nice,
                ..Policy::default()
            },
            target,
            base,
            key,
        },
        None => Plan::Skip {
            reason: Skip::NoRule,
        },
    }
}

/// Production entry point for [`plan`]: owns the engine lock protocol.
///
/// The guard is released before it returns, so the plan can be handed to
/// [`execute`] (which re-locks the engine for the claim) without the caller
/// having to keep the ordering straight.
pub fn plan_locked<'a>(
    name: &'a [u8],
    comm: &'a [u8],
    pid: u32,
    rules: &RuleSet,
    bias: &Mutex<BiasEngine>,
    cfg: &Config,
) -> Plan<'a> {
    let engine = bias.lock().unwrap_or_else(PoisonError::into_inner);
    plan(name, comm, pid, rules, &engine, cfg)
}

/// What a rule that is no longer in the corpus must give back: every
/// dimension it set, back to the kernel default.
///
/// `cpuset` is deliberately excluded — there is no safe "unset" (the process
/// was confined on purpose, possibly by its unit, and widening to every CPU is
/// worse than leaving the set in place). ADR 0002 records the trade-off.
fn release_policy(old: &Policy) -> Policy {
    Policy {
        nice: old.nice.map(|_| 0),
        // `latency_nice` falls back to `nice` at apply time, so a nice-only
        // rule still set it.
        latency_nice: old.latency_nice.or(old.nice).map(|_| 0),
        sched: old.sched.map(|_| SchedPolicy::Normal),
        rtprio: old.rtprio.map(|_| 0),
        ioclass: old.ioclass.map(|_| IoClass::None),
        ionice: old.ionice.map(|_| 0),
        oom_score_adj: old.oom_score_adj.map(|_| 0),
        cgroup: old.cgroup.as_ref().map(|_| Arc::from("")),
        cpuset: None,
        autogroup: old.autogroup.map(|_| 0),
        bias: None,
    }
}

/// Fill the dimensions a deleted rule has to give back into a plan the new
/// corpus produced (the learner's own `nice` survives; everything else the
/// old rule set is reset).
fn fill_released(desired: &mut Policy, old: &Policy) {
    let rel = release_policy(old);
    desired.nice = desired.nice.or(rel.nice);
    // `latency_nice` falls back to `nice` at apply time, so a plan that
    // carries a nice already has a latency value — filling the reset 0 here
    // would replace the fallback (a `nice: -5` rule would become latency 0
    // on every no-op reload).
    if desired.latency_nice.is_none() && desired.nice.is_none() {
        desired.latency_nice = rel.latency_nice;
    }
    desired.sched = desired.sched.or(rel.sched);
    desired.rtprio = desired.rtprio.or(rel.rtprio);
    desired.ioclass = desired.ioclass.or(rel.ioclass);
    desired.ionice = desired.ionice.or(rel.ionice);
    desired.oom_score_adj = desired.oom_score_adj.or(rel.oom_score_adj);
    desired.cgroup = desired.cgroup.clone().or(rel.cgroup);
    desired.autogroup = desired.autogroup.or(rel.autogroup);
}

/// Dimensions the old rule set that the new corpus does not *reset*: the
/// give-back set. `cpuset` is excluded even though a deletion is still owed —
/// there is no safe value to write back, so counting it as released would
/// report a revert that never happened (it is reported separately, see
/// [`Reconciled::kept_cpuset`]).
fn revertable_dims(old: &Policy, new: Option<&Policy>) -> usize {
    let unset = |f: fn(&Policy) -> bool| !new.is_some_and(f);
    usize::from(old.nice.is_some() && unset(|p| p.nice.is_some()))
        + usize::from(old.latency_nice.is_some() && unset(|p| p.latency_nice.is_some()))
        + usize::from(old.sched.is_some() && unset(|p| p.sched.is_some()))
        + usize::from(old.rtprio.is_some() && unset(|p| p.rtprio.is_some()))
        + usize::from(old.ioclass.is_some() && unset(|p| p.ioclass.is_some()))
        + usize::from(old.ionice.is_some() && unset(|p| p.ionice.is_some()))
        + usize::from(
            old.oom_score_adj.is_some() && unset(|p| p.oom_score_adj.is_some()),
        )
        + usize::from(old.cgroup.is_some() && unset(|p| p.cgroup.is_some()))
        + usize::from(old.autogroup.is_some() && unset(|p| p.autogroup.is_some()))
}

/// The old rule set gave the process a `cpuset` the new one does not. Nothing
/// is written back (no safe value), but an operator reading the reload log
/// must be able to see that this dimension was left in place on purpose.
fn cpuset_dropped(old: &Policy, new: Option<&Policy>) -> bool {
    old.cpuset.is_some() && !new.is_some_and(|p| p.cpuset.is_some())
}

/// What a reload owes one running process.
pub struct Reconciled<'a> {
    /// `None` = nothing to do.
    pub plan: Option<Plan<'a>>,
    /// The process was re-planned and at least one dimension the old tree set
    /// is reset by that plan. False for a process the pass could not reach.
    pub released: bool,
    /// The process was re-planned and a `cpuset` was dropped but deliberately
    /// left in place: no safe value exists to write back (ADR 0002). Counted
    /// apart so `released` never reports a revert that did not happen.
    pub kept_cpuset: bool,
}

/// Decide what a reload owes a process that is already running.
///
/// `old` is the rule tree that was live until this reload. A process the new
/// corpus still matches gets its full plan re-applied (idempotent, and the
/// only way an edit reaches a dimension drift does not re-assert); a process
/// whose rule was deleted gets the old rule's dimensions reset, under any
/// `nice` the learner now wants. `None` = nothing to do.
pub fn reconcile<'a>(
    name: &'a [u8],
    comm: &'a [u8],
    pid: u32,
    old: &RuleSet,
    new: &RuleSet,
    bias: &BiasEngine,
    cfg: &Config,
) -> Reconciled<'a> {
    let nothing = Reconciled {
        plan: None,
        released: false,
        kept_cpuset: false,
    };
    if name.is_empty() || pid == 1 || pid == std::process::id() {
        return nothing;
    }
    let Ok(name) = std::str::from_utf8(name) else {
        return nothing;
    };
    let comm = std::str::from_utf8(comm).unwrap_or(name);
    let old_policy = old.lookup(name).map(|(p, _)| &**p);
    let new_policy = new.lookup(name).map(|(p, _)| &**p);
    let plan = match plan(name.as_bytes(), comm.as_bytes(), pid, new, bias, cfg) {
        Plan::Apply {
            mut policy,
            target,
            base,
            key,
        } => {
            // Whatever the old rule set and the new plan does not is given
            // back — a deleted rule and a field deleted from a live rule are
            // the same reconciliation.
            if let Some(old) = old_policy {
                fill_released(&mut policy, old);
            }
            Some(Plan::Apply {
                policy,
                target,
                base,
                key,
            })
        }
        // No rule and no bias: only a rule that disappeared has anything owed.
        Plan::Skip {
            reason: Skip::NoRule,
        } => old_policy.map(|old| Plan::Apply {
            policy: release_policy(old),
            target: 0,
            base: 0,
            key: BiasKey { name, comm },
        }),
        Plan::Skip { reason } => Some(Plan::Skip { reason }),
    };
    // A skip (the process vanished, or /proc hid it) issues no syscall, so
    // it must not be counted as a revert: the journal's `released` is what an
    // operator reads as "this was put back".
    let executed = matches!(plan, Some(Plan::Apply { .. }));
    Reconciled {
        plan,
        released: executed && old_policy.is_some_and(|o| revertable_dims(o, new_policy) > 0),
        kept_cpuset: executed && old_policy.is_some_and(|o| cpuset_dropped(o, new_policy)),
    }
}

/// [`reconcile`] with the engine-lock protocol handled here.
pub fn reconcile_locked<'a>(
    name: &'a [u8],
    comm: &'a [u8],
    pid: u32,
    old: &RuleSet,
    new: &RuleSet,
    bias: &Mutex<BiasEngine>,
    cfg: &Config,
) -> Reconciled<'a> {
    let engine = bias.lock().unwrap_or_else(PoisonError::into_inner);
    reconcile(name, comm, pid, old, new, &engine, cfg)
}

/// Drift correction's delta rule: re-assert only a `nice` that actually
/// moved. A policy that dictates no nice (a cgroup/sched-only rule) has
/// nothing for the corrector to re-assert, and a process already at the
/// target is left alone — both become [`Skip::NoChange`].
pub fn settle<'a>(plan: Plan<'a>, current_nice: i8) -> Plan<'a> {
    match plan {
        Plan::Apply { policy, .. } if policy.nice.is_none_or(|t| t == current_nice) => Plan::Skip {
            reason: Skip::NoChange,
        },
        plan => plan,
    }
}

/// Which caller produced a decision — labels the operator log so a drift
/// re-assertion stays distinguishable from an exec-time apply.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Route {
    Exec,
    Drift,
    /// A reload re-planned a process that was already running (ADR 0002).
    Reload,
}

impl Route {
    pub fn label(self) -> &'static str {
        match self {
            Route::Exec => "exec",
            Route::Drift => "drift",
            Route::Reload => "reload",
        }
    }
}

/// The one effect [`execute`] cannot perform itself: the syscall layer.
///
/// Production is [`SystemApplier`]; tests inject a fake, which is what makes
/// the decision logic observable without root.
pub trait Applier {
    fn apply(&self, pid: u32, policy: &Policy) -> ApplyReport;
}

/// Blanket pass-through so a caller can hand `execute` the adapter it already
/// holds (`&applier`) instead of moving it.
impl<T: Applier + ?Sized> Applier for &T {
    fn apply(&self, pid: u32, policy: &Policy) -> ApplyReport {
        (**self).apply(pid, policy)
    }
}

/// Production adapter over [`apply_policy`]: static dispatch, no allocation.
pub struct SystemApplier<'a> {
    pub cfg: &'a Config,
    pub caps: &'a Capabilities,
    pub cgroups: &'a CgroupManager,
}

impl Applier for SystemApplier<'_> {
    fn apply(&self, pid: u32, policy: &Policy) -> ApplyReport {
        apply_policy(
            pid,
            policy,
            &self.cfg.apply_options(),
            self.caps,
            self.cgroups,
        )
    }
}

/// The effects of a decision that outlive it: the closed-loop claim and the
/// operator log. Built once per call site via [`Sink::new`].
pub struct Sink<'a> {
    /// Closed-loop claim target; `None` when bias is disabled (`bias_enable =
    /// false`) — there is no expectation to record then.
    bias: Option<&'a Mutex<BiasEngine>>,
    log_applied: bool,
    route: Route,
}

impl<'a> Sink<'a> {
    pub fn new(cfg: &Config, bias: &'a Mutex<BiasEngine>, route: Route) -> Self {
        Self {
            bias: cfg.bias_enable.then_some(bias),
            log_applied: cfg.log_applied_rule,
            route,
        }
    }
}

/// What [`execute`] did with a plan.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Outcome {
    /// At least one dimension actually landed (`Applied`).
    pub applied_any: bool,
    /// The closed-loop claim recorded, when a bias sink was present:
    /// `Some(None)` = released (the nice apply did not take).
    pub claim: Option<Option<i8>>,
}

/// Apply a plan and account for it: counters, apply, claim, log.
///
/// The order is load-bearing: the apply happens first, then the claim records
/// what the nice *became* (a nice that did not take is recorded as `None`, so
/// the closed loop knows the expectation is void), then the operator log fires
/// only if some dimension actually landed. Locking the engine for the claim
/// happens here, after the apply, so a caller never has to hold the guard.
pub fn execute<A: Applier>(
    plan: Plan<'_>,
    pid: u32,
    applier: A,
    counters: &Counters,
    sink: &Sink<'_>,
) -> Outcome {
    counters.seen.fetch_add(1, Ordering::Relaxed);
    let (policy, target, base, key) = match plan {
        Plan::Skip { reason } => {
            counters.note_skip(reason);
            return Outcome {
                applied_any: false,
                claim: None,
            };
        }
        Plan::Apply {
            policy,
            target,
            base,
            key,
        } => (policy, target, base, key),
    };

    counters.matched.fetch_add(1, Ordering::Relaxed);
    let report = applier.apply(pid, &policy);
    counters.tally(&report);
    let applied_any = report
        .ordered()
        .iter()
        .any(|(_dim, outcome)| *outcome == ActionOutcome::Applied);
    // The claim states what the nice became; a nice that did not take is
    // recorded as `None` so the closed loop knows this expectation is void.
    let claim = (report.nice == ActionOutcome::Applied).then_some(target);
    if let Some(bias) = sink.bias {
        bias.lock()
            .unwrap_or_else(PoisonError::into_inner)
            .note_applied(key.comm(), claim);
    }
    if sink.log_applied && applied_any {
        tracing::info!(
            rule = key.name(),
            pid,
            bias = target - base,
            route = sink.route.label(),
            "applied rule"
        );
    }
    Outcome {
        applied_any,
        claim: sink.bias.map(|_| claim),
    }
}

/// Aggregate per-dimension outcome counters.
#[derive(Debug, Default)]
struct DimTally {
    applied: AtomicU64,
    skipped: AtomicU64,
    gone: AtomicU64,
    denied: AtomicU64,
    failed: AtomicU64,
}

/// One route's decision and apply counters (see the module docs: the exec and
/// drift routes each own a set).
#[derive(Debug)]
pub struct Counters {
    /// Decisions observed (exec events, or pids examined by a drift scan).
    seen: AtomicU64,
    /// Decisions that synthesised a policy.
    matched: AtomicU64,
    /// `Skip::NoRule` decisions.
    unmatched: AtomicU64,
    /// `Skip::Unresolved` / `Skip::NonUtf8` decisions.
    unresolved: AtomicU64,
    /// Gone/Denied/Failed outcomes since the last snapshot.
    problems: AtomicU64,
    dims: [DimTally; 8],
}

impl Default for Counters {
    fn default() -> Self {
        Self {
            seen: AtomicU64::new(0),
            matched: AtomicU64::new(0),
            unmatched: AtomicU64::new(0),
            unresolved: AtomicU64::new(0),
            problems: AtomicU64::new(0),
            dims: std::array::from_fn(|_| DimTally::default()),
        }
    }
}

/// A drained, plain-data view of one [`Counters`] set for the summary line.
#[derive(Debug, Default, Clone)]
pub struct Snapshot {
    pub seen: u64,
    pub matched: u64,
    pub unmatched: u64,
    pub unresolved: u64,
    /// Cumulative per-dimension problem breakdown
    /// (`sched:gone=1,denied=2 ionice:denied=1`); empty when clean.
    pub dims: String,
}

impl Counters {
    fn note_skip(&self, reason: Skip) {
        match reason {
            Skip::Unresolved | Skip::NonUtf8 => {
                self.unresolved.fetch_add(1, Ordering::Relaxed);
            }
            Skip::NoRule => {
                self.unmatched.fetch_add(1, Ordering::Relaxed);
            }
            // A managed process that already has its nice: the drift happy
            // path, not an anomaly worth a counter of its own.
            Skip::NoChange => {}
        }
    }

    fn tally(&self, report: &ApplyReport) {
        for (slot, (_dim, outcome)) in self.dims.iter().zip(report.ordered()) {
            let (counter, problem) = match outcome {
                ActionOutcome::Applied => (&slot.applied, false),
                ActionOutcome::Skipped => (&slot.skipped, false),
                ActionOutcome::Gone => (&slot.gone, true),
                ActionOutcome::Denied(_) => (&slot.denied, true),
                ActionOutcome::Failed(_) => (&slot.failed, true),
            };
            counter.fetch_add(1, Ordering::Relaxed);
            if problem {
                self.problems.fetch_add(1, Ordering::Relaxed);
            }
        }
    }

    /// Gone/Denied/Failed since the last snapshot, without draining — lets
    /// the summary decide whether to emit at all.
    pub fn problems(&self) -> u64 {
        self.problems.load(Ordering::Relaxed)
    }

    /// Take a view for the summary line. Drains `problems` (each problem
    /// belongs to exactly one emitted line); dimension tallies stay
    /// cumulative, as before.
    pub fn snapshot(&self) -> Snapshot {
        // Drain: each problem belongs to exactly one emitted summary line.
        self.problems.swap(0, Ordering::Relaxed);
        Snapshot {
            seen: self.seen.load(Ordering::Relaxed),
            matched: self.matched.load(Ordering::Relaxed),
            unmatched: self.unmatched.load(Ordering::Relaxed),
            unresolved: self.unresolved.load(Ordering::Relaxed),
            dims: self.dims_summary(),
        }
    }

    /// Compact per-dimension problem breakdown; zero counters are omitted.
    fn dims_summary(&self) -> String {
        let mut out = String::new();
        for (dim, t) in DIMS.iter().zip(&self.dims) {
            let gone = t.gone.load(Ordering::Relaxed);
            let denied = t.denied.load(Ordering::Relaxed);
            let failed = t.failed.load(Ordering::Relaxed);
            if gone + denied + failed == 0 {
                continue;
            }
            if !out.is_empty() {
                out.push(' ');
            }
            out.push_str(dim);
            let mut sep = ':';
            for (count, label) in [(gone, "gone"), (denied, "denied"), (failed, "failed")] {
                if count > 0 {
                    out.push_str(&format!("{sep}{label}={count}"));
                    sep = ',';
                }
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bias::BiasConfig;
    use crate::bias_state::{BiasSnapshot, EntrySnapshot};
    use std::cell::Cell;
    use std::sync::Arc;

    /// Fake adapter: records the nice it was handed and reports what the test
    /// dictates. No syscalls, no root, no /proc.
    struct FakeApplier {
        nice: ActionOutcome,
        applied: Cell<Option<i8>>,
    }

    impl FakeApplier {
        fn new(nice: ActionOutcome) -> Self {
            Self {
                nice,
                applied: Cell::new(None),
            }
        }
    }

    impl Applier for FakeApplier {
        fn apply(&self, _pid: u32, policy: &Policy) -> ApplyReport {
            self.applied.set(policy.nice);
            ApplyReport {
                nice: self.nice,
                ..ApplyReport::default()
            }
        }
    }

    fn rules_with(name: &str, nice: Option<i8>) -> RuleSet {
        rules_with_bias(name, nice, None)
    }

    fn rules_with_bias(name: &str, nice: Option<i8>, bias: Option<bool>) -> RuleSet {
        let mut rules = RuleSet::empty();
        rules.exact.insert(
            Arc::from(name),
            Arc::new(Policy {
                nice,
                bias,
                ..Policy::default()
            }),
        );
        rules
    }

    /// An engine whose history holds one entry for `comm` (restore is the
    /// only way in without driving real sample windows).
    fn engine_with(comm: &str, bias: i8) -> BiasEngine {
        let mut engine = BiasEngine::new(BiasConfig::default());
        engine.restore(BiasSnapshot {
            version: 1,
            threshold_ema: None,
            lean_capture: None,
            entries: vec![EntrySnapshot {
                name: comm.to_string(),
                durable: crate::bias_state::Durable {
                    bias,
                    confidence: 1.0,
                    last_seen: 1_000,
                    ..crate::bias_state::Durable::default()
                },
            }],
        });
        engine
    }

    fn sink<'a>(cfg: &Config, bias: &'a Mutex<BiasEngine>) -> Sink<'a> {
        Sink::new(cfg, bias, Route::Exec)
    }

    #[test]
    fn rule_match_with_bias_synthesises_biased_policy_and_claims() {
        let rules = rules_with("cc1plus", Some(5));
        let engine = Mutex::new(engine_with("cc1plus", 3));
        let cfg = Config::default();
        let counters = Counters::default();
        let fake = FakeApplier::new(ActionOutcome::Applied);

        let plan = plan_locked(b"cc1plus", b"cc1plus", 42, &rules, &engine, &cfg);
        let Plan::Apply {
            policy,
            target,
            base,
            key,
        } = &plan
        else {
            panic!("expected an apply, got {plan:?}");
        };
        assert_eq!(policy.nice, Some(8)); // base 5 + bias 3
        assert_eq!((*target, *base), (8, 5));
        assert_eq!(key.comm(), "cc1plus");
        assert_eq!(key.name(), "cc1plus");

        let outcome = execute(plan, 42, &fake, &counters, &sink(&cfg, &engine));
        assert!(outcome.applied_any);
        assert_eq!(outcome.claim, Some(Some(8)));
        assert_eq!(fake.applied.get(), Some(8));
        // The closed loop now expects 8 for the comm.
        assert_eq!(
            engine.lock().unwrap().applied_nice("cc1plus"),
            Some(8)
        );
        let snap = counters.snapshot();
        assert_eq!((snap.seen, snap.matched, snap.unmatched), (1, 1, 0));
    }

    #[test]
    fn no_rule_no_bias_is_skipped_as_norule_and_counts_unmatched() {
        let rules = RuleSet::empty();
        let engine = Mutex::new(engine_with("other", 3));
        let cfg = Config::default();
        let counters = Counters::default();
        let fake = FakeApplier::new(ActionOutcome::Applied);

        let plan = plan_locked(b"stranger", b"stranger", 7, &rules, &engine, &cfg);
        assert_eq!(
            plan,
            Plan::Skip {
                reason: Skip::NoRule
            }
        );
        let outcome = execute(plan, 7, &fake, &counters, &sink(&cfg, &engine));
        assert!(!outcome.applied_any);
        assert_eq!(outcome.claim, None);
        assert_eq!(fake.applied.get(), None, "nothing may be applied");
        let snap = counters.snapshot();
        assert_eq!((snap.seen, snap.matched, snap.unmatched), (1, 0, 1));
    }

    #[test]
    fn bias_only_name_applies_the_bias_as_the_policy_nice() {
        let rules = RuleSet::empty(); // no rule: the bias is the whole policy
        let engine = Mutex::new(engine_with("electron", 3));
        let cfg = Config::default();
        let counters = Counters::default();
        let fake = FakeApplier::new(ActionOutcome::Applied);

        let plan = plan_locked(b"electron", b"electron", 9, &rules, &engine, &cfg);
        let Plan::Apply {
            policy, target, base, ..
        } = &plan
        else {
            panic!("a bias target must produce an apply, got {plan:?}");
        };
        assert_eq!(*policy, Policy { nice: Some(3), ..Policy::default() });
        assert_eq!((*target, *base), (3, 0));

        let outcome = execute(plan, 9, &fake, &counters, &sink(&cfg, &engine));
        assert!(outcome.applied_any);
        assert_eq!(fake.applied.get(), Some(3), "bias-only nice must reach apply");
        assert_eq!(counters.snapshot().matched, 1);
    }

    #[test]
    fn a_rule_can_opt_a_name_out_of_the_learned_axis() {
        // The operator pinned the game at -5 and said "leave it there": the
        // learned +3 must not spend itself on a closed loop the corpus owns.
        let rules = rules_with_bias("game", Some(-5), Some(false));
        let engine = Mutex::new(engine_with("game", 3));
        let cfg = Config::default();

        let plan = plan_locked(b"game", b"game", 4242, &rules, &engine, &cfg);
        let Plan::Apply {
            policy, target, base, ..
        } = &plan
        else {
            panic!("the rule still applies, got {plan:?}");
        };
        assert_eq!(policy.nice, Some(-5), "the pinned value, not -5 + 3");
        assert_eq!((*target, *base), (-5, -5));
    }

    #[test]
    fn the_opt_out_unwinds_a_claim_recorded_before_the_rule() {
        // A name learned to +3 before the corpus pinned it must come back.
        let rules = rules_with_bias("game", None, Some(false));
        let engine = Mutex::new(engine_with("game", 3));
        engine.lock().unwrap().note_applied("game", Some(3));
        let cfg = Config::default();

        let plan = plan_locked(b"game", b"game", 4242, &rules, &engine, &cfg);
        let Plan::Apply { policy, target, .. } = &plan else {
            panic!("a stale claim under an opted-out rule must be corrected, got {plan:?}");
        };
        assert_eq!(policy.nice, Some(0), "back to the rule's base");
        assert_eq!(*target, 0);
    }

    #[test]
    fn the_opt_out_is_per_name_and_true_leaves_the_axis_on() {
        let mut rules = rules_with_bias("game", Some(-5), Some(false));
        rules.exact.insert(
            Arc::from("build"),
            Arc::new(Policy {
                bias: Some(true),
                ..Policy::default()
            }),
        );
        let engine = Mutex::new(engine_with("build", 3));
        let cfg = Config::default();

        let plan = plan_locked(b"build", b"build", 4243, &rules, &engine, &cfg);
        let Plan::Apply { policy, .. } = &plan else {
            panic!("bias: true must behave like an absent key, got {plan:?}");
        };
        assert_eq!(policy.nice, Some(3), "the learned axis still applies");
    }

    fn rules_of(name: &str, policy: Policy) -> RuleSet {
        let mut rules = RuleSet::empty();
        rules.exact.insert(Arc::from(name), Arc::new(policy));
        rules
    }

    /// `kaz-8`: an edit reached a running process only for `nice`, because
    /// drift re-asserts that one dimension. A reload must re-apply the whole
    /// plan even when `nice` did not move.
    #[test]
    fn a_reload_reapplies_a_dimension_drift_never_reasserts() {
        let old = rules_with("game", Some(-5));
        let new = rules_of(
            "game",
            Policy {
                nice: Some(-5),
                ioclass: Some(IoClass::BestEffort),
                ionice: Some(7),
                ..Policy::default()
            },
        );
        let engine = Mutex::new(engine_with("game", 0));
        let cfg = Config::default();

        let plan = reconcile_locked(b"game", b"game", 4242, &old, &new, &engine, &cfg)
            .plan
            .expect("a matched process is re-planned");
        let Plan::Apply { policy, .. } = &plan else {
            panic!("expected an apply, got {plan:?}");
        };
        assert_eq!(policy.nice, Some(-5), "unchanged nice");
        assert_eq!(policy.ioclass, Some(IoClass::BestEffort), "the edit landed");
        assert_eq!(policy.ionice, Some(7));
    }

    /// `kaz-11`: deleting a rule left its dimensions on the process. They must
    /// be given back, not merely forgotten.
    #[test]
    fn a_deleted_rule_gives_back_every_dimension_it_set() {
        let old = rules_of(
            "game",
            Policy {
                nice: Some(-7),
                ioclass: Some(IoClass::BestEffort),
                ionice: Some(3),
                oom_score_adj: Some(-500),
                cgroup: Some(Arc::from("games")),
                autogroup: Some(5),
                ..Policy::default()
            },
        );
        let new = RuleSet::empty();
        let engine = Mutex::new(engine_with("game", 0));
        let cfg = Config::default();

        let plan = reconcile_locked(b"game", b"game", 4242, &old, &new, &engine, &cfg)
            .plan
            .expect("a released process is re-planned");
        let Plan::Apply { policy, target, .. } = &plan else {
            panic!("expected an apply, got {plan:?}");
        };
        assert_eq!(policy.nice, Some(0), "the kernel default");
        assert_eq!(*target, 0);
        assert_eq!(policy.ioclass, Some(IoClass::None));
        assert_eq!(policy.ionice, Some(0));
        assert_eq!(policy.oom_score_adj, Some(0));
        assert_eq!(policy.cgroup.as_deref(), Some(""));
        assert_eq!(policy.autogroup, Some(0));
        assert!(policy.sched.is_none(), "the old rule set no scheduler");
        assert!(policy.cpuset.is_none());
    }

    /// `latency_nice` falls back to `nice` at apply time. A no-op reload of a
    /// `nice`-only rule must not fill the release's 0 into that fallback:
    /// the process would move from latency −5 to 0 and stay there.
    #[test]
    fn a_reload_does_not_replace_the_latency_fallback_with_zero() {
        let old = rules_with("game", Some(-5));
        let new = rules_with("game", Some(-5));
        let engine = Mutex::new(engine_with("game", 0));
        let cfg = Config::default();

        let plan = reconcile_locked(b"game", b"game", 4242, &old, &new, &engine, &cfg)
            .plan
            .expect("a matched process is re-planned");
        let Plan::Apply { policy, .. } = &plan else {
            panic!("expected an apply, got {plan:?}");
        };
        assert_eq!(policy.nice, Some(-5));
        assert_eq!(
            policy.latency_nice, None,
            "absent latency_nice must keep falling back to the planned nice"
        );
    }

    /// `kaz-8` is also "the field was deleted, the rule stayed": the old
    /// ionice was never revisited, so it has to be given back the same way.
    #[test]
    fn a_field_removed_from_a_live_rule_is_given_back_too() {
        let old = rules_of(
            "game",
            Policy {
                nice: Some(-5),
                ioclass: Some(IoClass::BestEffort),
                ionice: Some(7),
                ..Policy::default()
            },
        );
        let new = rules_with("game", Some(-5));
        let engine = Mutex::new(engine_with("game", 0));
        let cfg = Config::default();

        let plan = reconcile_locked(b"game", b"game", 4242, &old, &new, &engine, &cfg)
            .plan
            .expect("a matched process is re-planned");
        let Plan::Apply { policy, .. } = &plan else {
            panic!("expected an apply, got {plan:?}");
        };
        assert_eq!(policy.nice, Some(-5), "the surviving field is the new rule's");
        assert_eq!(policy.ioclass, Some(IoClass::None), "the deleted field is reset");
        assert_eq!(policy.ionice, Some(0));
    }

    #[test]
    fn a_deleted_rule_leaves_the_cpuset_in_place() {
        let old = rules_of(
            "game",
            Policy {
                nice: Some(-5),
                cpuset: Some(aranicy_model::CpuSet::Alias(Arc::from("big-cores"))),
                ..Policy::default()
            },
        );
        let new = RuleSet::empty();
        let engine = Mutex::new(engine_with("game", 0));
        let cfg = Config::default();
        let plan = reconcile_locked(b"game", b"game", 4242, &old, &new, &engine, &cfg)
            .plan
            .expect("a released process is re-planned");
        let Plan::Apply { policy, .. } = &plan else {
            panic!("expected an apply, got {plan:?}");
        };
        assert_eq!(policy.nice, Some(0));
        assert!(
            policy.cpuset.is_none(),
            "widening affinity to every CPU is worse than leaving it"
        );
    }

    #[test]
    fn a_deleted_rule_keeps_the_learners_nice_but_gives_back_the_rest() {
        let old = rules_of(
            "game",
            Policy {
                nice: Some(-7),
                ioclass: Some(IoClass::BestEffort),
                ionice: Some(3),
                ..Policy::default()
            },
        );
        let new = RuleSet::empty();
        let engine = Mutex::new(engine_with("game", 3));
        let cfg = Config::default();

        let plan = reconcile_locked(b"game", b"game", 4242, &old, &new, &engine, &cfg)
            .plan
            .expect("a released process is re-planned");
        let Plan::Apply { policy, target, .. } = &plan else {
            panic!("expected an apply, got {plan:?}");
        };
        assert_eq!(policy.nice, Some(3), "the learner's say, not 0");
        assert_eq!(*target, 3);
        assert_eq!(policy.ioclass, Some(IoClass::None), "the rule's IO class goes");
        assert_eq!(policy.ionice, Some(0));
    }

    /// The journal counter must see a *field* deletion (and a `cpuset` one,
    /// which is deliberately not reset) — not only a rule that disappeared.
    #[test]
    fn reconcile_reports_a_field_deletion_as_released() {
        let engine = Mutex::new(engine_with("game", 0));
        let cfg = Config::default();
        let old = rules_of(
            "game",
            Policy {
                nice: Some(-5),
                ionice: Some(7),
                cpuset: Some(aranicy_model::CpuSet::Alias(Arc::from("big-cores"))),
                ..Policy::default()
            },
        );
        let new = rules_with("game", Some(-5));
        let r = reconcile_locked(b"game", b"game", 4242, &old, &new, &engine, &cfg);
        assert!(r.plan.is_some());
        assert!(r.released, "ionice was dropped from a live rule");
        assert!(r.kept_cpuset, "the cpuset is dropped but left in place");

        // Nothing removed: not a release.
        let same = rules_of(
            "game",
            Policy {
                nice: Some(-5),
                ionice: Some(7),
                ..Policy::default()
            },
        );
        let r = reconcile_locked(b"game", b"game", 4242, &same, &same, &engine, &cfg);
        assert!(!r.released, "an unchanged rule owes nothing");
        assert!(!r.kept_cpuset);
    }

    /// A cpuset is the one dimension a deleted rule does not give back, so a
    /// reload that only loses a cpuset must report the keep — and must not
    /// report a release it did not perform.
    #[test]
    fn a_dropped_cpuset_alone_is_reported_as_kept_not_released() {
        let engine = Mutex::new(engine_with("game", 0));
        let cfg = Config::default();
        let old = rules_of(
            "game",
            Policy {
                cpuset: Some(aranicy_model::CpuSet::Alias(Arc::from("big-cores"))),
                ..Policy::default()
            },
        );
        let new = RuleSet::empty();
        let r = reconcile_locked(b"game", b"game", 4242, &old, &new, &engine, &cfg);
        assert!(!r.released, "nothing was reset");
        assert!(r.kept_cpuset, "the affinity was left in place");
    }

    /// A process the pass cannot identify (no match name) issues no syscall,
    /// so it must not inflate the re-plan line's `released`/`kept_cpuset`.
    #[test]
    fn an_unidentifiable_process_is_not_counted_as_a_revert() {
        let engine = Mutex::new(engine_with("game", 0));
        let cfg = Config::default();
        let old = rules_of(
            "game",
            Policy {
                nice: Some(-5),
                cpuset: Some(aranicy_model::CpuSet::Alias(Arc::from("big-cores"))),
                ..Policy::default()
            },
        );
        let r = reconcile_locked(&[0xff, 0xfe], b"", 4242, &old, &old, &engine, &cfg);
        assert!(r.plan.is_none(), "nothing can be planned for it");
        assert!(!r.released, "no dimension was reset");
        assert!(!r.kept_cpuset, "nothing was kept on it either");
    }

    #[test]
    fn reconcile_ignores_names_neither_corpus_knows_and_never_pid_one() {
        let empty = RuleSet::empty();
        let engine = Mutex::new(engine_with("other", 0));
        let cfg = Config::default();
        assert!(
            reconcile_locked(b"stranger", b"stranger", 4242, &empty, &empty, &engine, &cfg)
                .plan
                .is_none()
        );
        let old = rules_with("init", Some(19));
        assert!(
            reconcile_locked(b"init", b"init", 1, &old, &empty, &engine, &cfg)
                .plan
                .is_none(),
            "PID 1 has its own route"
        );
    }

    #[test]
    fn protected_names_take_no_learned_bias() {
        let rules = RuleSet::empty();
        let engine = Mutex::new(engine_with("systemd", 3));
        let cfg = Config::default(); // "systemd" is in the default list

        let plan = plan_locked(b"systemd", b"systemd", 209, &rules, &engine, &cfg);
        assert_eq!(
            plan,
            Plan::Skip {
                reason: Skip::NoRule
            },
            "a protected name with no rule and no claim must be left alone"
        );
    }

    #[test]
    fn a_rule_still_applies_to_a_protected_name_but_without_the_bias() {
        let rules = rules_with("systemd", Some(5));
        let engine = Mutex::new(engine_with("systemd", 3));
        let cfg = Config::default();

        let plan = plan_locked(b"systemd", b"systemd", 209, &rules, &engine, &cfg);
        let Plan::Apply { policy, target, .. } = &plan else {
            panic!("an explicit rule must still apply, got {plan:?}");
        };
        assert_eq!(policy.nice, Some(5), "the rule value, not 5 + 3");
        assert_eq!(*target, 5);
    }

    #[test]
    fn a_protected_name_unwinds_a_claim_recorded_before_protection() {
        let rules = RuleSet::empty();
        let engine = Mutex::new(engine_with("systemd", 2));
        let cfg = Config::default();
        // What a pre-fix daemon (or an upgrade) left behind: PID 1 at +2.
        engine.lock().unwrap().note_applied("systemd", Some(2));

        let plan = settle(
            plan_locked(b"systemd", b"systemd", 209, &rules, &engine, &cfg),
            2,
        );
        let Plan::Apply { policy, target, .. } = &plan else {
            panic!("a stale claim on a protected name must be corrected, got {plan:?}");
        };
        assert_eq!(policy.nice, Some(0), "drifted init must be brought back");
        assert_eq!(*target, 0);
    }

    #[test]
    fn pid_one_is_only_ever_put_back_to_the_kernel_default() {
        let fork = |rules: RuleSet, comm: &'static str| {
            let engine = Mutex::new(engine_with(comm, 3));
            let cfg = Config::default();
            let plan = plan_locked(comm.as_bytes(), comm.as_bytes(), 1, &rules, &engine, &cfg);
            let Plan::Apply { policy, target, .. } = plan else {
                panic!("pid 1 must always be planned towards 0, got {plan:?}");
            };
            (policy.nice, target)
        };

        // No rule, a learned bias of +3, and a name outside the protect list:
        // none of it may reach init.
        assert_eq!(fork(RuleSet::empty(), "init"), (Some(0), 0));
        // An explicit rule is not a licence either.
        assert_eq!(fork(rules_with("init", Some(19)), "init"), (Some(0), 0));
        // Nor is a rule that raises priority.
        assert_eq!(fork(rules_with("init", Some(-20)), "init"), (Some(0), 0));
    }

    #[test]
    fn the_daemon_never_biases_its_own_process() {
        let rules = RuleSet::empty();
        let engine = Mutex::new(engine_with("aranicy", 3));
        let cfg = Config::default();

        let plan = plan_locked(
            b"aranicy",
            b"aranicy",
            std::process::id(),
            &rules,
            &engine,
            &cfg,
        );
        assert_eq!(
            plan,
            Plan::Skip {
                reason: Skip::NoRule
            },
            "the unit's Nice= must survive: the daemon is not its own workload"
        );
    }

    #[test]
    fn a_rule_cannot_retune_the_daemon_itself() {
        let rules = rules_with("aranicy", Some(19));
        let engine = Mutex::new(engine_with("aranicy", 0));
        let cfg = Config::default();

        let plan = plan_locked(b"aranicy", b"aranicy", std::process::id(), &rules, &engine, &cfg);
        assert_eq!(
            plan,
            Plan::Skip {
                reason: Skip::NoRule
            },
            "the unit's Nice= is the operator's statement about the daemon itself"
        );
    }

    #[test]
    fn protection_follows_the_protected_name_not_only_the_comm() {
        let rules = RuleSet::empty();
        // The history is keyed by the launcher's comm; the rule identity is
        // the game's executable. Protection must cover both.
        let engine = Mutex::new(engine_with("launcher", 3));
        let cfg = Config {
            bias_protect: vec!["game".into()],
            ..Config::default()
        };

        let plan = plan_locked(b"game", b"launcher", 4242, &rules, &engine, &cfg);
        assert_eq!(
            plan,
            Plan::Skip {
                reason: Skip::NoRule
            },
            "a renaming entry point must not smuggle the bias past the list"
        );
    }

    #[test]
    fn an_empty_protect_list_leaves_only_pid_one_protected() {
        let rules = RuleSet::empty();
        let engine = Mutex::new(engine_with("systemd", 3));
        let cfg = Config {
            bias_protect: Vec::new(),
            ..Config::default()
        };

        let plan = plan_locked(b"systemd", b"systemd", 500, &rules, &engine, &cfg);
        let Plan::Apply { policy, .. } = &plan else {
            panic!("with the list cleared, a non-init pid must be biased, got {plan:?}");
        };
        assert_eq!(policy.nice, Some(3));
    }

    #[test]
    fn failed_nice_apply_releases_the_claim() {
        let rules = rules_with("cc1plus", Some(5));
        let engine = Mutex::new(engine_with("cc1plus", 3));
        let cfg = Config::default();
        let counters = Counters::default();
        let fake = FakeApplier::new(ActionOutcome::Failed(libc::EPERM));

        // A previous apply recorded an expectation; the failed one must clear
        // it, or the closed loop would freeze the entry on a mismatch it
        // cannot attribute.
        engine
            .lock()
            .unwrap()
            .note_applied("cc1plus", Some(8));
        assert_eq!(engine.lock().unwrap().applied_nice("cc1plus"), Some(8));

        let plan = plan_locked(b"cc1plus", b"cc1plus", 42, &rules, &engine, &cfg);
        let outcome = execute(plan, 42, &fake, &counters, &sink(&cfg, &engine));
        assert!(!outcome.applied_any);
        assert_eq!(outcome.claim, Some(None));
        assert_eq!(
            engine.lock().unwrap().applied_nice("cc1plus"),
            None,
            "a nice that did not take must release the claim"
        );
        assert_eq!(counters.problems(), 1);
        let snap = counters.snapshot();
        assert!(snap.dims.contains("nice:failed=1"), "{}", snap.dims);
    }

    #[test]
    fn drift_and_exec_plan_identically_for_the_same_input() {
        let rules = rules_with("cc1plus", Some(5));
        let engine = Mutex::new(engine_with("cc1plus", 3));
        let cfg = Config::default();

        // Capsule run: the exec path's plan, then drift's (plan + settle with
        // a mismatch of 19), must agree field for field.
        let exec = plan_locked(b"cc1plus", b"cc1plus", 42, &rules, &engine, &cfg);
        let drift = settle(
            plan_locked(b"cc1plus", b"cc1plus", 42, &rules, &engine, &cfg),
            19,
        );
        assert_eq!(exec, drift);
        assert!(matches!(exec, Plan::Apply { .. }), "{exec:?}");

        // And the same process already at the target is not re-applied.
        let already = settle(
            plan_locked(b"cc1plus", b"cc1plus", 42, &rules, &engine, &cfg),
            8,
        );
        assert_eq!(
            already,
            Plan::Skip {
                reason: Skip::NoChange
            }
        );

        // Drift never re-asserts a rule that dictates no nice.
        let rules = rules_with("cgroup-only", None);
        let no_nice = settle(
            plan_locked(b"cgroup-only", b"cgroup-only", 43, &rules, &engine, &cfg),
            0,
        );
        assert_eq!(
            no_nice,
            Plan::Skip {
                reason: Skip::NoChange
            }
        );
    }

    #[test]
    fn unresolved_and_non_utf8_names_account_as_unresolved() {
        let rules = RuleSet::empty();
        let engine = Mutex::new(engine_with("x", 0));
        let cfg = Config::default();
        let counters = Counters::default();
        let fake = FakeApplier::new(ActionOutcome::Applied);

        for (name, reason) in [
            (&b""[..], Skip::Unresolved),
            (&b"\xff\xfe"[..], Skip::NonUtf8),
        ] {
            let plan = plan_locked(name, b"x", 1, &rules, &engine, &cfg);
            assert_eq!(plan, Plan::Skip { reason });
            execute(plan, 1, &fake, &counters, &sink(&cfg, &engine));
        }
        let snap = counters.snapshot();
        assert_eq!(snap.unresolved, 2);
        assert_eq!(snap.seen, 2);
        assert_eq!(fake.applied.get(), None);
    }

    #[test]
    fn each_route_tallies_into_its_own_counter_set() {
        let rules = rules_with("cc1plus", Some(5));
        let engine = Mutex::new(engine_with("cc1plus", 3));
        let cfg = Config::default();
        let (exec, drift) = (Counters::default(), Counters::default());
        let fake = FakeApplier::new(ActionOutcome::Applied);

        // Any ordinary pid: pid 1 is reserved for init and never carries a
        // bias, which would make this a different test.
        let pid = 4321;
        let plan = plan_locked(b"cc1plus", b"cc1plus", pid, &rules, &engine, &cfg);
        execute(plan, pid, &fake, &exec, &Sink::new(&cfg, &engine, Route::Exec));

        // Drift re-plans the same process (already at the target: NoChange)
        // and one unmanaged process (NoRule): scan traffic, tallied apart.
        let settled = settle(
            plan_locked(b"cc1plus", b"cc1plus", pid, &rules, &engine, &cfg),
            8,
        );
        execute(settled, pid, &fake, &drift, &Sink::new(&cfg, &engine, Route::Drift));
        let stranger = plan_locked(b"nobody", b"nobody", 2, &rules, &engine, &cfg);
        execute(
            stranger,
            2,
            &fake,
            &drift,
            &Sink::new(&cfg, &engine, Route::Drift),
        );

        let exec = exec.snapshot();
        assert_eq!((exec.seen, exec.matched, exec.unmatched), (1, 1, 0));
        let drift = drift.snapshot();
        assert_eq!(
            (drift.seen, drift.matched, drift.unmatched),
            (2, 0, 1),
            "a settled scan is neither a match nor an anomaly"
        );
    }

    #[test]
    fn eff_nice_never_touches_nice_less_rules_at_zero_bias() {
        // Regression: a matched rule without `nice` must not be force-reniced
        // to 0 (ambient nice is preserved).
        assert_eq!(eff_nice_for(None, 0, None), None);
        // bias active → synthesized claim
        assert_eq!(eff_nice_for(None, 3, None), Some(3));
        // relaxation: previously claimed, target moved back to base → apply 0
        assert_eq!(eff_nice_for(None, 0, Some(3)), Some(0));
        // at rest after relaxation → untouched again
        assert_eq!(eff_nice_for(None, 0, Some(0)), None);
        // rule dictates nice → always managed (drift re-asserts)
        assert_eq!(eff_nice_for(Some(5), 5, None), Some(5));
        assert_eq!(eff_nice_for(Some(5), 8, Some(8)), Some(8));
    }

    #[test]
    fn counters_tally_tracks_outcomes() {
        let c = Counters::default();
        let report = ApplyReport {
            nice: ActionOutcome::Applied,
            latency_nice: ActionOutcome::Skipped,
            sched: ActionOutcome::Gone,
            ionice: ActionOutcome::Denied("ionice"),
            oom_score_adj: ActionOutcome::Failed(5),
            ..ApplyReport::default()
        };
        c.tally(&report);
        assert_eq!(c.problems(), 3);
        let snap = c.snapshot();
        assert_eq!(c.problems(), 0);
        assert!(snap.dims.contains("sched:gone=1"), "{}", snap.dims);
        assert!(snap.dims.contains("ionice:denied=1"), "{}", snap.dims);
        assert!(snap.dims.contains("oom_score_adj:failed=1"), "{}", snap.dims);
        assert!(!snap.dims.split(' ').any(|d| d.starts_with("nice:")), "{}", snap.dims);

    }

    #[test]
    fn no_claim_is_recorded_when_bias_is_disabled() {
        let rules = rules_with("cc1plus", Some(5));
        let engine = Mutex::new(engine_with("cc1plus", 3));
        let cfg = Config::parse("bias_enable = false\n");
        let counters = Counters::default();
        let fake = FakeApplier::new(ActionOutcome::Applied);

        let plan = plan_locked(b"cc1plus", b"cc1plus", 42, &rules, &engine, &cfg);
        let Plan::Apply { target, .. } = &plan else {
            panic!("a rule nice is always managed, got {plan:?}");
        };
        assert_eq!(*target, 5, "without bias the rule's nice is the target");
        let outcome = execute(plan, 42, &fake, &counters, &sink(&cfg, &engine));
        assert_eq!(outcome.claim, None);
        assert_eq!(engine.lock().unwrap().applied_nice("cc1plus"), None);
    }
}

//! `aranicy start` runtime: singleton, signals, worker loop, drift corrector.
//!
//! Threading model:
//! * **worker** (main thread): `next_event()` → name resolution → lookup →
//!   `apply_policy`. Stack buffers only; no `format!`/`String` per event.
//! * **signal** thread: `sigwait` over SIGHUP/SIGTERM/SIGINT. SIGHUP performs
//!   the reload *on this thread* (the rule swap is an `RwLock<Arc<..>>` store,
//!   so the worker sees it on its next event); SIGTERM/SIGINT stop the daemon.
//!   Note: `ExecSource::wake()` is terminal (it latches shutdown), so it is
//!   used only for real shutdown — reload must not wake the worker.
//! * **drift** thread: every `check_freq` s, compares rule `nice` against
//!   `/proc/<pid>/stat` and re-applies only on mismatch.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, PoisonError, RwLock};
use std::time::{Duration, Instant};

use aranicy_apply::{
    apply_policy, probe_capabilities, ActionOutcome, ApplyReport, Capabilities, CgroupManager,
};
use aranicy_events::{BpfExecSource, ExecEvent, ExecSource};
use aranicy_model::CgroupDef;
use aranicy_rules::RuleSet;

use crate::config::Config;
use crate::load;
use crate::paths::{self, Paths};
use crate::pidfile::{self, PidFile};
use crate::procname::{self, ProcResolver, NAME_CAP, STAT_CAP};
use crate::bias::BiasEngine;
use crate::{bias_state, sampler};
use crate::sdnotify;

const SUMMARY_INTERVAL: Duration = Duration::from_secs(60);
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

/// Aggregate per-dimension outcome counters (the "counter table").
#[derive(Debug, Default, Clone, Copy)]
struct DimTally {
    applied: u64,
    skipped: u64,
    gone: u64,
    denied: u64,
    failed: u64,
}

#[derive(Debug, Default)]
struct Counters {
    events: u64,
    matched: u64,
    unmatched: u64,
    unresolved: u64,
    dims: [DimTally; 8],
    /// Outcomes that are neither Applied nor Skipped since the last summary.
    problems_since_last: u64,
}

impl Counters {
    fn tally(&mut self, report: &ApplyReport) {
        for (i, (_dim, outcome)) in report.ordered().into_iter().enumerate() {
            let slot = &mut self.dims[i];
            match outcome {
                ActionOutcome::Applied => slot.applied += 1,
                ActionOutcome::Skipped => slot.skipped += 1,
                ActionOutcome::Gone => {
                    slot.gone += 1;
                    self.problems_since_last += 1;
                }
                ActionOutcome::Denied(_) => {
                    slot.denied += 1;
                    self.problems_since_last += 1;
                }
                ActionOutcome::Failed(_) => {
                    slot.failed += 1;
                    self.problems_since_last += 1;
                }
            }
        }
    }

    /// Compact per-dimension problem breakdown for the summary line
    /// (`sched:gone=1 ionice:denied=1`); zero counters are omitted.
    fn dims_summary(&self) -> String {
        let mut out = String::new();
        for (dim, t) in DIMS.iter().zip(&self.dims) {
            if t.gone + t.denied + t.failed == 0 {
                continue;
            }
            if !out.is_empty() {
                out.push(' ');
            }
            out.push_str(dim);
            let mut sep = ':';
            for (count, label) in [(t.gone, "gone"), (t.denied, "denied"), (t.failed, "failed")] {
                if count > 0 {
                    out.push_str(&format!("{sep}{label}={count}"));
                    sep = ',';
                }
            }
        }
        out
    }
}

/// Everything the worker, drift and signal threads share.
struct Shared {
    rules: RwLock<Arc<RuleSet>>,
    config: RwLock<Arc<Config>>,
    cgroups: RwLock<Arc<CgroupManager>>,
    caps: Capabilities,
    /// Startup-owned `/proc` dirfd resolver (openat hot path; closed on drop).
    proc: ProcResolver,
    counters: Mutex<Counters>,
    /// Dynamic nice bias engine (plan.md §4.2); lookups use the sample `comm`
    /// namespace, rule bases use the argv[0]-derived match name.
    bias: Mutex<BiasEngine>,
    shutdown: AtomicBool,
    drift_cv: Condvar,
    drift_m: Mutex<()>,
}

impl Shared {
    fn rules(&self) -> Arc<RuleSet> {
        self.rules.read().unwrap_or_else(PoisonError::into_inner).clone()
    }
    fn config(&self) -> Arc<Config> {
        self.config.read().unwrap_or_else(PoisonError::into_inner).clone()
    }
    fn cgroups(&self) -> Arc<CgroupManager> {
        self.cgroups
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
    fn stop(&self) {
        self.shutdown.store(true, Ordering::Release);
        let _guard = self.drift_m.lock().unwrap_or_else(PoisonError::into_inner);
        self.drift_cv.notify_all();
    }
}

/// Run the daemon (root only). Returns `Err` for startup failures.
pub fn start(paths: Paths) -> anyhow::Result<()> {
    let euid = unsafe { libc::geteuid() };
    if euid != 0 {
        anyhow::bail!(
            "aranicy start requires root (effective uid {euid}); \
             run it via sudo or the aranicy.service unit"
        );
    }

    let pidfile_path = paths::pidfile_path();
    let _pidfile: PidFile = match pidfile::acquire(&pidfile_path) {
        Ok(pf) => pf,
        Err(pidfile::PidFileError::AlreadyRunning { pid, path }) => {
            anyhow::bail!(
                "another aranicy instance is already running (pid {}); \
                 if that pid is stale, remove {}",
                pid.map(|p| p.to_string())
                    .unwrap_or_else(|| "unknown".into()),
                path.display()
            );
        }
        Err(pidfile::PidFileError::Io(e)) => {
            anyhow::bail!("cannot create pidfile {}: {e}", pidfile_path.display())
        }
    };

    let (cfg, rules) = load::load_all(&paths)
        .map_err(|e| anyhow::anyhow!("cannot load config/rules: {e}"))?;
    init_tracing(&cfg.loglevel);
    log_startup_warnings(&cfg, &rules);

    let caps = probe_capabilities();
    if !caps.latency_nice {
        tracing::warn!(
            "kernel does not support latency_nice (sched_setattr LATENCY_NICE); \
             latency_nice policy will be skipped"
        );
    }

    let (cgroups, cg_warnings) = init_cgroups(&rules.cgroups);
    for w in cg_warnings {
        tracing::warn!("{w}");
    }

    let source = BpfExecSource::new().map_err(|e| anyhow::anyhow!("{e}"))?;
    let source = Arc::new(source);

    // Bias engine (plan.md §4.2-4.3): restore learned history, then run the
    // sampler + bias threads alongside the worker.
    let bias_enable = cfg.bias_enable;
    let bias_state_path = cfg.bias_state_path.clone();
    let bias_cadence = std::time::Duration::from_millis(cfg.bias_cadence_ms.clamp(50, 60_000));
    // Compile `bias_group.*` regexes once; invalid patterns are warned and
    // skipped so a typo cannot take the daemon down (warn-and-fallback).
    let bias_groups: Vec<crate::bias::BiasGroup> = cfg
        .bias_groups
        .iter()
        .filter_map(|(tag, src)| match regex::Regex::new(src) {
            Ok(re) => Some((tag.clone(), re)),
            Err(e) => {
                tracing::warn!("bias: skipping bias_group.{tag}: invalid regex {src:?}: {e}");
                None
            }
        })
        .collect();
    let mut bias_engine = BiasEngine::new_with_groups(cfg.bias_config(), bias_groups);
    match bias_state::load(std::path::Path::new(&bias_state_path)) {
        Ok(Some(snap)) => {
            tracing::info!(entries = snap.entries.len(), "bias: restored history");
            bias_engine.restore(snap);
        }
        Ok(None) => {}
        Err(e) => tracing::warn!("bias: cannot load {}: {e}", bias_state_path),
    }

    let shared = Arc::new(Shared {
        rules: RwLock::new(Arc::new(rules)),
        config: RwLock::new(Arc::new(cfg)),
        cgroups: RwLock::new(Arc::new(cgroups)),
        caps,
        proc: ProcResolver::open(),
        counters: Mutex::new(Counters::default()),
        bias: Mutex::new(bias_engine),
        shutdown: AtomicBool::new(false),
        drift_cv: Condvar::new(),
        drift_m: Mutex::new(()),
    });

    let sigset = block_signals()?;

    let signal_done = Arc::new(AtomicBool::new(false));
    let signal_thread = {
        let shared = Arc::clone(&shared);
        let source = Arc::clone(&source);
        let done = Arc::clone(&signal_done);
        let paths = paths.clone();
        std::thread::spawn(move || {
            signal_loop(&sigset, &shared, &source, &paths);
            done.store(true, Ordering::Release);
        })
    };
    let drift_thread = {
        let shared = Arc::clone(&shared);
        std::thread::spawn(move || drift_loop(&shared))
    };
    let bias_threads = if bias_enable {
        let (tx, rx) = std::sync::mpsc::channel();
        let handle = sampler::spawn(&sampler::default_socket_paths(), bias_cadence, tx);
        let bias_thread = {
            let shared = Arc::clone(&shared);
            std::thread::spawn(move || bias_loop(&shared, rx))
        };
        Some((handle, bias_thread))
    } else {
        None
    };

    let socket = sdnotify::socket_from_env();
    sdnotify::notify(socket.as_deref(), "READY=1");

    worker_loop(&shared, &source);

    // Clean shutdown: wake + join threads, then STOPPING (brief's order).
    shared.stop();
    source.wake();
    if !signal_done.load(Ordering::Acquire) {
        // Unblock the sigwait thread if the worker exited on its own (e.g. a
        // ringbuf error): SIGTERM is blocked process-wide, so this self-signal
        // stays pending until `sigwait` consumes it.
        unsafe {
            libc::kill(libc::getpid(), libc::SIGTERM);
        }
    }
    if let Some((handle, bias_thread)) = bias_threads {
        handle.shutdown();
        let _ = handle.join();
        let _ = bias_thread.join(); // channel closes with the sampler thread
        let snap = shared.bias.lock().unwrap_or_else(PoisonError::into_inner).snapshot();
        if let Err(e) = bias_state::save(&snap, std::path::Path::new(&bias_state_path)) {
            tracing::warn!("bias: cannot save state: {e}");
        }
    }
    let _ = drift_thread.join();
    let _ = signal_thread.join();
    sdnotify::notify(socket.as_deref(), "STOPPING=1");
    Ok(())
}

/// Bias thread: consumes sampler batches, updates the engine, verifies our own
/// applied nice against lavd's `static_prio` view, ages history, debounces
/// state writes. Aging continues on `Timeout` so a dead stream relaxes bias
/// instead of freezing it (owner rule).
fn bias_loop(shared: &Arc<Shared>, rx: std::sync::mpsc::Receiver<sampler::Batch>) {
    use std::sync::mpsc::RecvTimeoutError;
    let state_path = shared.config().bias_state_path.clone();
    let mut last_save = std::time::Instant::now();
    loop {
        match rx.recv_timeout(std::time::Duration::from_secs(1)) {
            Ok(batch) => {
                let now = std::time::SystemTime::now();
                let mut engine = shared.bias.lock().unwrap_or_else(PoisonError::into_inner);
                for s in &batch {
                    // Closed loop verifies ONLY against the nice this daemon
                    // applied for the comm — never re-derived from rules
                    // (comm and the rule-name namespace differ) and skipped
                    // for comms we never touched.
                    engine.verify_applied(&s.comm, s.static_prio);
                }
                engine.record_batch(&batch, now);
                engine.age(now);
                for (name, bias) in engine.take_changed_recently() {
                    tracing::info!(name, bias, "bias changed");
                }
                drop(engine);
                if last_save.elapsed() >= std::time::Duration::from_secs(60) {
                    let snap =
                        shared.bias.lock().unwrap_or_else(PoisonError::into_inner).snapshot();
                    if let Err(e) = bias_state::save(&snap, std::path::Path::new(&state_path)) {
                        tracing::warn!("bias: cannot save state: {e}");
                    }
                    last_save = std::time::Instant::now();
                }
            }
            Err(RecvTimeoutError::Timeout) => {
                shared
                    .bias
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .age(std::time::SystemTime::now());
            }
            Err(RecvTimeoutError::Disconnected) => break,
        }
    }
}

// ---------------------------------------------------------------------------
// Tracing
// ---------------------------------------------------------------------------

fn init_tracing(level: &str) {
    use tracing_subscriber::filter::LevelFilter;
    let lvl = match level {
        "trace" => LevelFilter::TRACE,
        "debug" => LevelFilter::DEBUG,
        "info" => LevelFilter::INFO,
        "warn" => LevelFilter::WARN,
        "error" | "critical" => LevelFilter::ERROR,
        "off" => LevelFilter::OFF,
        _ => LevelFilter::INFO,
    };
    let _ = tracing_subscriber::fmt()
        .with_max_level(lvl)
        .with_writer(std::io::stderr)
        .with_ansi(false)
        .with_target(false)
        .try_init();
}

fn log_startup_warnings(cfg: &Config, rules: &RuleSet) {
    for w in &cfg.warnings {
        tracing::warn!("config: {w}");
    }
    for w in &rules.warnings {
        tracing::warn!("{}:{}: {}", w.file.display(), w.line, w.message);
    }
}

// ---------------------------------------------------------------------------
// cgroups: failures are warnings, never fatal
// ---------------------------------------------------------------------------

fn init_cgroups(defs: &BTreeMap<Arc<str>, CgroupDef>) -> (CgroupManager, Vec<String>) {
    let defs: BTreeMap<String, CgroupDef> = defs
        .iter()
        .map(|(name, def)| (name.to_string(), def.clone()))
        .collect();
    match CgroupManager::init(defs.clone()) {
        Ok(manager) => (manager, Vec::new()),
        Err(whole) => {
            // Isolate the failing definition(s): each one gets its own init.
            let mut warnings = Vec::new();
            let mut ok = BTreeMap::new();
            for (name, def) in defs {
                let mut one = BTreeMap::new();
                one.insert(name.clone(), def.clone());
                match CgroupManager::init(one) {
                    Ok(_) => {
                        ok.insert(name, def);
                    }
                    Err(e) => warnings.push(format!("cgroup {name:?} not created: {e}")),
                }
            }
            if warnings.is_empty() {
                warnings.push(format!("batch cgroup creation failed: {whole}"));
            }
            let manager = CgroupManager::init(ok).unwrap_or_else(|e| {
                warnings.push(format!("cgroup manager unavailable: {e}"));
                // Degenerate mount point: assigns fail cleanly as errors.
                CgroupManager {
                    mount: std::path::PathBuf::from("/dev/null"),
                    created: BTreeMap::new(),
                }
            });
            (manager, warnings)
        }
    }
}

// ---------------------------------------------------------------------------
// Signals
// ---------------------------------------------------------------------------

fn signal_set() -> libc::sigset_t {
    unsafe {
        let mut set: libc::sigset_t = std::mem::zeroed();
        libc::sigemptyset(&mut set);
        libc::sigaddset(&mut set, libc::SIGHUP);
        libc::sigaddset(&mut set, libc::SIGTERM);
        libc::sigaddset(&mut set, libc::SIGINT);
        set
    }
}

/// Block SIGHUP/SIGTERM/SIGINT process-wide (threads inherit the mask; only
/// `sigwait` in the signal thread consumes them).
fn block_signals() -> std::io::Result<libc::sigset_t> {
    let set = signal_set();
    let r = unsafe { libc::pthread_sigmask(libc::SIG_BLOCK, &raw const set, std::ptr::null_mut()) };
    if r == 0 {
        Ok(set)
    } else {
        Err(std::io::Error::from_raw_os_error(r))
    }
}

fn signal_loop(set: &libc::sigset_t, shared: &Arc<Shared>, source: &Arc<BpfExecSource>, paths: &Paths) {
    loop {
        let mut sig = 0;
        let r = unsafe { libc::sigwait(&raw const *set, &raw mut sig) };
        if r != 0 {
            tracing::warn!("sigwait failed ({r}); signal thread exiting");
            return;
        }
        match sig {
            libc::SIGHUP => reload(shared, paths),
            libc::SIGTERM | libc::SIGINT => {
                shared.stop();
                source.wake();
                return;
            }
            _ => {}
        }
    }
}

fn reload(shared: &Arc<Shared>, paths: &Paths) {
    match load::load_all(paths) {
        Ok((cfg, rules)) => {
            let (cgroups, warnings) = init_cgroups(&rules.cgroups);
            for w in warnings {
                tracing::warn!("{w}");
            }
            for w in &rules.warnings {
                tracing::warn!("{}:{}: {}", w.file.display(), w.line, w.message);
            }
            let n = load::rule_count(&rules);
            *shared.config.write().unwrap_or_else(PoisonError::into_inner) = Arc::new(cfg);
            *shared.rules.write().unwrap_or_else(PoisonError::into_inner) = Arc::new(rules);
            *shared.cgroups.write().unwrap_or_else(PoisonError::into_inner) = Arc::new(cgroups);
            tracing::info!("reloaded {n} rules");
        }
        Err(e) => {
            tracing::warn!("reload failed; keeping previous config and rules: {e}");
        }
    }
}

// ---------------------------------------------------------------------------
// Worker (hot path)
// ---------------------------------------------------------------------------

fn worker_loop(shared: &Arc<Shared>, source: &Arc<BpfExecSource>) {
    let mut summary = SummaryState {
        last: Instant::now(),
        dropped: 0,
    };
    loop {
        match source.next_event() {
            Ok(None) => return,
            Ok(Some(ev)) => {
                shared
                    .counters
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .events += 1;
                handle_event(shared, &ev);
                maybe_summary(shared, source, &mut summary);
            }
            Err(e) => {
                tracing::error!("event source failed: {e}; shutting down");
                return;
            }
        }
    }
}

/// Rate-limit state for the aggregate summary line.
struct SummaryState {
    last: Instant,
    /// `dropped_events()` value at the last emit (delta detection).
    dropped: u64,
}

/// When to claim nice for a process: the rule dictates a nice (always
/// managed, drift re-asserts it), a bias is active (target ≠ 0), or we
/// previously claimed this name and the target moved (relaxation back to
/// base must reach running processes). A matched rule with no `nice` and no
/// bias is left untouched — ambient nice is not ours to overwrite.
fn eff_nice_for(rule_nice: Option<i8>, target: i8, applied: Option<i8>) -> Option<i8> {
    if rule_nice.is_some() || target != 0 || applied.is_some_and(|a| a != target) {
        Some(target)
    } else {
        None
    }
}

fn handle_event(shared: &Shared, ev: &ExecEvent) {
    let mut buf = [0u8; NAME_CAP];
    let len = shared.proc.derive_name(ev.pid, ev.comm_bytes(), &mut buf);
    if len == 0 {
        shared
            .counters
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .unresolved += 1;
        return;
    }
    let Ok(name) = std::str::from_utf8(&buf[..len]) else {
        // Non-UTF8 argv[0] cannot match a JSON rule name.
        shared
            .counters
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .unresolved += 1;
        return;
    };

    let rules = shared.rules();
    let matched = rules.lookup(name);
    let cfg = shared.config();
    let base = matched.as_ref().map(|(p, _)| p.nice.unwrap_or(0)).unwrap_or(0);
    // Bias history is keyed by the kernel comm (sample namespace); rules match
    // on the argv[0]-derived name (plan.md §4.2).
    let comm_str = std::str::from_utf8(ev.comm_bytes()).unwrap_or(name);
    let (target, applied) = if cfg.bias_enable {
        let engine = shared.bias.lock().unwrap_or_else(PoisonError::into_inner);
        (engine.target_nice(comm_str, base), engine.applied_nice(comm_str))
    } else {
        (base, None)
    };
    let eff_nice = eff_nice_for(matched.as_ref().and_then(|(p, _)| p.nice), target, applied);

    let owned: aranicy_model::Policy;
    let policy: &aranicy_model::Policy = match matched {
        Some((p, _)) if eff_nice == p.nice => p,
        Some((p, _)) => {
            owned = aranicy_model::Policy {
                nice: eff_nice,
                ..(**p).clone()
            };
            &owned
        }
        None if eff_nice.is_some() => {
            owned = aranicy_model::Policy {
                nice: eff_nice,
                ..Default::default()
            };
            &owned
        }
        None => {
            shared
                .counters
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .unmatched += 1;
            return;
        }
    };
    shared
        .counters
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .matched += 1;

    let report = apply_policy(
        ev.pid,
        policy,
        &cfg.apply_options(),
        &shared.caps,
        &shared.cgroups(),
    );
    let applied_any = report
        .ordered()
        .iter()
        .any(|(_dim, outcome)| *outcome == ActionOutcome::Applied);
    shared
        .counters
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .tally(&report);
    if cfg.bias_enable {
        // Remember what we applied so the closed loop verifies against it
        // (and releases the claim when the apply did not take).
        shared
            .bias
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .note_applied(comm_str, (report.nice == ActionOutcome::Applied).then_some(target));
    }
    if cfg.log_applied_rule && applied_any {
        tracing::info!(rule = name, pid = ev.pid, bias = target - base, "applied rule");
    }
}

/// One rate-limited aggregate line per ~60 s, and only when something went
/// off the happy path (Gone/Denied/Failed or newly dropped events). Per-event
/// logging of Gone is deliberately suppressed (storms).
fn maybe_summary(shared: &Shared, source: &BpfExecSource, state: &mut SummaryState) {
    if state.last.elapsed() < SUMMARY_INTERVAL {
        return;
    }
    state.last = Instant::now();
    let dropped = source.dropped_events();
    let new_dropped = dropped.saturating_sub(state.dropped);
    state.dropped = dropped;
    let mut counters = shared.counters.lock().unwrap_or_else(PoisonError::into_inner);
    if counters.problems_since_last == 0 && new_dropped == 0 {
        return;
    }
    counters.problems_since_last = 0;
    tracing::warn!(
        events = counters.events,
        matched = counters.matched,
        unmatched = counters.unmatched,
        unresolved = counters.unresolved,
        dropped,
        dims = %counters.dims_summary(),
        "apply summary"
    );
}

// ---------------------------------------------------------------------------
// Drift corrector (delta-based)
// ---------------------------------------------------------------------------

fn drift_loop(shared: &Shared) {
    loop {
        let freq = shared.config().check_freq;
        let mut guard = shared.drift_m.lock().unwrap_or_else(PoisonError::into_inner);
        if freq == 0 {
            guard = shared
                .drift_cv
                .wait_while(guard, |_| !shared.shutdown.load(Ordering::Acquire))
                .unwrap_or_else(PoisonError::into_inner);
        } else {
            let (g, _timeout) = shared
                .drift_cv
                .wait_timeout_while(
                    guard,
                    Duration::from_secs(freq),
                    |_| !shared.shutdown.load(Ordering::Acquire),
                )
                .unwrap_or_else(PoisonError::into_inner);
            guard = g;
        }
        drop(guard);
        if shared.shutdown.load(Ordering::Acquire) {
            return;
        }
        if freq > 0 {
            drift_scan(shared);
        }
    }
}

/// Compare rule `nice` with `/proc/<pid>/stat`; re-apply only on mismatch.
fn drift_scan(shared: &Shared) {
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return;
    };
    for entry in entries.flatten() {
        if shared.shutdown.load(Ordering::Relaxed) {
            return;
        }
        let Some(pid) = procname::entry_pid(&entry.file_name()) else {
            continue;
        };
        let mut stat = [0u8; STAT_CAP];
        let Some(n) = shared.proc.read_stat(pid, &mut stat) else {
            continue;
        };
        let Some(current_nice) = procname::stat_nice(&stat[..n]) else {
            continue;
        };
        let comm = procname::stat_comm(&stat[..n]).unwrap_or(b"");

        let mut buf = [0u8; NAME_CAP];
        let len = shared.proc.derive_name(pid, comm, &mut buf);
        if len == 0 {
            continue;
        }
        let Ok(name) = std::str::from_utf8(&buf[..len]) else {
            continue;
        };
        let rules = shared.rules();
        let matched = rules.lookup(name);
        let cfg = shared.config();
        let base = matched.as_ref().map(|(p, _)| p.nice.unwrap_or(0)).unwrap_or(0);
        let comm_str = std::str::from_utf8(comm).unwrap_or(name);
        let (bias_target, applied) = if cfg.bias_enable {
            let engine = shared.bias.lock().unwrap_or_else(PoisonError::into_inner);
            (engine.target_nice(comm_str, base), engine.applied_nice(comm_str))
        } else {
            (base, None)
        };
        let eff_nice =
            eff_nice_for(matched.as_ref().and_then(|(p, _)| p.nice), bias_target, applied);
        let Some(target) = eff_nice else {
            continue;
        };
        if current_nice == target {
            continue; // delta-based: nothing drifted
        }
        tracing::debug!(
            pid,
            name,
            current = current_nice,
            target,
            "drift mismatch"
        );
        let owned: aranicy_model::Policy;
        let policy: &aranicy_model::Policy = match matched {
            Some((p, _)) if eff_nice == p.nice => p,
            Some((p, _)) => {
                owned = aranicy_model::Policy {
                    nice: eff_nice,
                    ..(**p).clone()
                };
                &owned
            }
            None => {
                owned = aranicy_model::Policy {
                    nice: eff_nice,
                    ..Default::default()
                };
                &owned
            }
        };
        let report = apply_policy(
            pid,
            policy,
            &cfg.apply_options(),
            &shared.caps,
            &shared.cgroups(),
        );
        shared
            .counters
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .tally(&report);
        if cfg.bias_enable {
            shared
                .bias
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .note_applied(comm_str, (report.nice == ActionOutcome::Applied).then_some(target));
        }
        if cfg.log_applied_rule {
            tracing::info!(rule = name, pid, "applied rule (drift)");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn config_to_apply_options_mapping() {
        let cfg = Config::parse(
            "apply_nice = false\napply_latnice = false\napply_ionice = false\n\
             apply_sched = false\napply_oom_score_adj = false\napply_cgroup = false\n\
             apply_cpuset = false\napply_autogroup = false\ncgroup_realtime_workaround = false\n",
        );
        let opts = cfg.apply_options();
        assert!(!opts.nice);
        assert!(!opts.latency_nice);
        assert!(!opts.sched);
        assert!(!opts.ionice);
        assert!(!opts.oom_score_adj);
        assert!(!opts.cgroup);
        assert!(!opts.cpuset);
        assert!(!opts.autogroup);
        assert!(!opts.cgroup_realtime_workaround);
    }

    #[test]
    fn apply_options_defaults_are_all_on() {
        let opts = Config::default().apply_options();
        assert!(opts.nice && opts.latency_nice && opts.sched && opts.ionice);
        assert!(opts.oom_score_adj && opts.cgroup && opts.cpuset && opts.autogroup);
        assert!(opts.cgroup_realtime_workaround);
    }

    #[test]
    fn counters_tally_tracks_outcomes() {
        let mut c = Counters::default();
        let report = ApplyReport {
            nice: ActionOutcome::Applied,
            latency_nice: ActionOutcome::Skipped,
            sched: ActionOutcome::Gone,
            ionice: ActionOutcome::Denied("ionice"),
            oom_score_adj: ActionOutcome::Failed(5),
            ..ApplyReport::default()
        };
        c.tally(&report);
        assert_eq!(c.dims[0].applied, 1);
        assert_eq!(c.dims[1].skipped, 1);
        assert_eq!(c.dims[2].gone, 1);
        assert_eq!(c.dims[3].denied, 1);
        assert_eq!(c.dims[4].failed, 1);
        assert_eq!(c.problems_since_last, 3);
        let summary = c.dims_summary();
        assert!(summary.contains("sched:gone=1"));
        assert!(summary.contains("ionice:denied=1"));
    }

    #[test]
    fn init_cgroups_rejects_nothing_on_empty_defs() {
        let (manager, warnings) = init_cgroups(&BTreeMap::new());
        // Empty defs must not fail even if creation is impossible.
        assert!(warnings.is_empty() || !manager.mount.as_os_str().is_empty());
    }
}

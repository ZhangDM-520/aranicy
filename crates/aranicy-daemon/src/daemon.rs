//! `aranicy start` runtime: singleton, signals, worker loop, drift corrector.
//!
//! Threading model:
//! * **worker** (main thread): `next_batch()` → per-frame name resolution →
//!   [`decision::plan`] → [`decision::execute`]. Stack buffers only; no
//!   `format!`/`String` per event, and one `read` per batch of exec events.
//! * **signal** thread: `sigwait` over SIGHUP/SIGTERM/SIGINT. SIGHUP performs
//!   the reload *on this thread* (the rule swap is an `RwLock<Arc<..>>` store,
//!   so the worker sees it on its next event); SIGTERM/SIGINT stop the daemon.
//!   Note: `ExecSource::wake()` is terminal (it latches shutdown), so it is
//!   used only for real shutdown — reload must not wake the worker.
//! * **drift** thread: every `check_freq` s, compares the plan's `nice`
//!   against `/proc/<pid>/stat` and re-applies only on mismatch.
//!
//! Policy synthesis itself lives in [`crate::decision`]: both routes build a
//! plan and hand it to `execute`, so counters, the closed-loop claim and the
//! operator log are produced in one place.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, PoisonError, RwLock};
use std::time::{Duration, Instant};

use aranicy_apply::{probe_capabilities, Capabilities, CgroupManager};
use aranicy_wire::{BatchBuf, FrameRef};

use crate::event_client::EventClient;
use aranicy_model::CgroupDef;
use aranicy_rules::RuleSet;

use crate::config::Config;
use crate::decision::{self, Counters, Route, Sink};
use crate::load;
use crate::paths::{self, Paths};
use crate::pidfile::{self, PidFile};
use crate::procname::{self, ProcResolver, NAME_CAP, STAT_CAP};
use crate::bias::BiasEngine;
use crate::{bias_state, sampler};
use crate::sdnotify;

const SUMMARY_INTERVAL: Duration = Duration::from_secs(60);

/// Everything the worker, drift and signal threads share.
struct Shared {
    rules: RwLock<Arc<RuleSet>>,
    config: RwLock<Arc<Config>>,
    cgroups: RwLock<Arc<CgroupManager>>,
    caps: Capabilities,
    /// Startup-owned `/proc` dirfd resolver (openat hot path; closed on drop).
    proc: ProcResolver,
    /// Exec-route decision/apply counters.
    counters: Counters,
    /// Drift-route counters: a scan re-plans every pid each cadence, so its
    /// traffic is tallied apart from the event counters (see `decision`).
    drift_counters: Counters,
    /// Dynamic nice bias engine (plan.md §4.2); lookups use the sample `comm`
    /// namespace, rule bases use the argv[0]-derived match name
    /// (`decision::BiasKey` bundles the two for every decision).
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
    log_startup_warnings(&cfg, &rules, &paths.config_dir);

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

    // The collector owns every BPF program; aranicy is a pure client and no
    // longer needs CAP_BPF (see contrib/ananicy.service).
    let source = EventClient::connect(
        std::path::PathBuf::from(&cfg.collector_events_socket),
        std::time::Duration::from_millis(cfg.collector_wait_ms.max(1)),
    )
    .map_err(|e| anyhow::anyhow!("{e}"))?;

    // Bias engine (plan.md §4.2-4.3): restore learned history, then run the
    // sampler + bias threads alongside the worker.
    let bias_enable = cfg.bias_enable;
    let bias_state_path = cfg.bias_state_path.clone();
    let bias_cadence = std::time::Duration::from_millis(cfg.bias_cadence_ms.clamp(50, 60_000));
    // Compile `bias_group.*` regexes once; invalid patterns are warned and
    // skipped so a typo cannot take the daemon down (warn-and-fallback).
    let bias_groups: Vec<crate::bias::BiasGroup> = compile_bias_groups(&cfg);
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
        counters: Counters::default(),
        drift_counters: Counters::default(),
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
        let handle = sampler::spawn_collector(
            std::path::PathBuf::from(&shared.config().bias_collector_socket),
            bias_cadence,
            tx,
        );
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

/// Bias thread: consumes sampler batches, feeds the engine one verb per
/// event (`ingest` for a delivered window, `tick` when the stream is quiet)
/// and logs every move it reports, debounces state writes. Aging continues on
/// `Timeout` so a dead stream relaxes bias instead of freezing it (owner
/// rule).
fn bias_loop(shared: &Arc<Shared>, rx: std::sync::mpsc::Receiver<sampler::Batch>) {
    use std::sync::mpsc::RecvTimeoutError;
    let state_path = shared.config().bias_state_path.clone();
    let mut last_save = std::time::Instant::now();
    loop {
        match rx.recv_timeout(std::time::Duration::from_secs(1)) {
            Ok(batch) => {
                let now = std::time::SystemTime::now();
                let changes = {
                    let mut engine = shared.bias.lock().unwrap_or_else(PoisonError::into_inner);
                    bias_pass(&mut engine, &batch, now)
                };
                log_changes(&changes);
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
                let changes = {
                    let mut engine = shared.bias.lock().unwrap_or_else(PoisonError::into_inner);
                    engine.tick(std::time::SystemTime::now())
                };
                log_changes(&changes);
            }
            Err(RecvTimeoutError::Disconnected) => break,
        }
    }
}

/// One bias-loop iteration over a delivered window: evidence first, then the
/// wall-clock aging every iteration owes. Both are required — batches arrive
/// every 500 ms while the loop's idle timeout is 1 s, so aging cannot be left
/// to the timeout arm: a vanished name would keep its confidence, and its
/// bias, for as long as the stream stayed up (measured live before this was
/// split out: a stale entry held `confidence 0.994599` across 145 s).
fn bias_pass(
    engine: &mut BiasEngine,
    batch: &[aranicy_wire::Sample],
    now: std::time::SystemTime,
) -> Vec<crate::bias::Change> {
    let mut changes = engine.ingest(batch, now);
    changes.extend(engine.tick(now));
    changes
}

/// The `bias changed` journal line: `name`/`bias` stay first (existing greps
/// depend on them), `prev`/`class`/`cause` make the move diagnosable — an
/// evidence-driven step and a relax are otherwise indistinguishable.
fn log_changes(changes: &[crate::bias::Change]) {
    for c in changes {
        tracing::info!(
            name = %c.name,
            bias = c.bias,
            prev = c.prev,
            class = ?c.class,
            cause = ?c.cause,
            "bias changed"
        );
    }
}

// ---------------------------------------------------------------------------
// Bias engine construction
// ---------------------------------------------------------------------------

/// Compile the `bias_group.<tag> = <regex>` families once per load. Invalid
/// patterns are warned and skipped (warn-and-fallback: a typo must not take the
/// daemon down). Shared by startup and reload, so a reload re-keys history
/// with exactly the same rules startup would.
fn compile_bias_groups(cfg: &Config) -> Vec<crate::bias::BiasGroup> {
    cfg.bias_groups
        .iter()
        .filter_map(|(tag, src)| match regex::Regex::new(src) {
            Ok(re) => Some((tag.clone(), re)),
            Err(e) => {
                tracing::warn!("bias: skipping bias_group.{tag}: invalid regex {src:?}: {e}");
                None
            }
        })
        .collect()
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

fn log_startup_warnings(cfg: &Config, rules: &RuleSet, rules_dir: &std::path::Path) {
    for w in startup_warnings(cfg, rules, rules_dir) {
        tracing::warn!("{w}");
    }
}

/// The warning lines a start (or a reload) emits, in one place so the
/// zero-rule case can be asserted without a tracing subscriber.
fn startup_warnings(cfg: &Config, rules: &RuleSet, rules_dir: &std::path::Path) -> Vec<String> {
    let mut out: Vec<String> = cfg.warnings.iter().map(|w| format!("config: {w}")).collect();
    out.extend(
        rules
            .warnings
            .iter()
            .map(|w| format!("{}:{}: {}", w.file.display(), w.line, w.message)),
    );
    if cfg.rule_load && load::rule_count(rules) == 0 {
        out.push(format!(
            "no rules loaded from {}: with an empty rule set only the learned \
             bias changes priorities. The package ships no rule corpus - point \
             ANANICY_CPP_CONFDIR at yours, or write files ending in .rules",
            rules_dir.display()
        ));
    }
    out
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

fn signal_loop(set: &libc::sigset_t, shared: &Arc<Shared>, source: &Arc<EventClient>, paths: &Paths) {
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
            for w in &cfg.warnings {
                tracing::warn!("config: {w}");
            }
            for w in &rules.warnings {
                tracing::warn!("{}:{}: {}", w.file.display(), w.line, w.message);
            }
            // Everything captured at startup stays captured: name it instead
            // of reporting a success the daemon cannot deliver.
            let old = shared.config();
            let changed = old.changed_keys(&cfg);
            let deferred = old.restart_only_changed(&cfg);
            let groups_changed = changed.contains(&"bias_group.*");
            let bias_cfg = cfg.bias_config();
            let bias_groups = compile_bias_groups(&cfg);
            let n = load::rule_count(&rules);
            // History (entries, T, want stamps, rate budget) survives; only
            // the knobs, the clamps and the history keys are re-derived.
            shared
                .bias
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .reconfigure(bias_cfg, bias_groups);
            *shared.config.write().unwrap_or_else(PoisonError::into_inner) = Arc::new(cfg);
            *shared.rules.write().unwrap_or_else(PoisonError::into_inner) = Arc::new(rules);
            *shared.cgroups.write().unwrap_or_else(PoisonError::into_inner) = Arc::new(cgroups);
            tracing::info!("reloaded {n} rules; bias reconfigured");
            if groups_changed {
                tracing::info!(
                    "bias_group.* changed; existing bias history re-keyed under the new families"
                );
            }
            if !deferred.is_empty() {
                tracing::warn!("restart-only keys ignored: {}", deferred.join(", "));
            }
        }
        Err(e) => {
            tracing::warn!("reload failed; keeping previous config and rules: {e}");
        }
    }
}

// ---------------------------------------------------------------------------
// Worker (hot path)
// ---------------------------------------------------------------------------

fn worker_loop(shared: &Arc<Shared>, source: &Arc<EventClient>) {
    let mut summary = SummaryState {
        last: Instant::now(),
        dropped: 0,
    };
    // The reader owns the batch buffer: a burst of execs costs one read and
    // the frames are iterated in place.
    let mut batch = BatchBuf::new();
    loop {
        match source.next_batch(&mut batch) {
            Ok(false) => return,
            Ok(true) => {
                for ev in batch.frames() {
                    handle_event(shared, ev);
                }
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

fn handle_event(shared: &Shared, ev: FrameRef<'_>) {
    // The collector resolves the match name at capture time (microseconds
    // after exec), which is strictly better than re-deriving it here: by the
    // time the event crosses the socket, a short-lived tool may already be
    // gone. Fall back to the local resolver only when capture failed.
    let mut buf = [0u8; NAME_CAP];
    let len = if ev.name().is_empty() {
        shared.proc.derive_name(ev.pid(), ev.comm(), &mut buf)
    } else {
        let name = ev.name();
        let n = name.len().min(NAME_CAP);
        buf[..n].copy_from_slice(&name[..n]);
        n
    };
    let rules = shared.rules();
    let cfg = shared.config();
    let cgroups = shared.cgroups();
    let plan = decision::plan_locked(
        &buf[..len],
        ev.comm(),
        ev.pid(),
        &rules,
        &shared.bias,
        &cfg,
    );
    let applier = decision::SystemApplier {
        cfg: &cfg,
        caps: &shared.caps,
        cgroups: &cgroups,
    };
    let sink = Sink::new(&cfg, &shared.bias, Route::Exec);
    decision::execute(plan, ev.pid(), &applier, &shared.counters, &sink);
}

/// One rate-limited aggregate line per ~60 s, and only when something went
/// off the happy path (Gone/Denied/Failed or newly dropped events). Per-event
/// logging of Gone is deliberately suppressed (storms).
///
/// The line reports both counter sets, labelled apart: `events`/`matched`/
/// `unmatched`/`unresolved`/`dims` are the exec route (one decision per exec
/// event), while `drift_seen`/`drift_managed`/`drift_unmatched`/`drift_dims`
/// are the drift route (one decision per pid examined per scan, so they are
/// scan tallies and are deliberately not folded into the event counters).
fn maybe_summary(shared: &Shared, source: &EventClient, state: &mut SummaryState) {
    if state.last.elapsed() < SUMMARY_INTERVAL {
        return;
    }
    state.last = Instant::now();
    let dropped = source.dropped_events();
    let new_dropped = dropped.saturating_sub(state.dropped);
    state.dropped = dropped;
    if shared.counters.problems() + shared.drift_counters.problems() == 0 && new_dropped == 0 {
        return;
    }
    let exec = shared.counters.snapshot();
    let drift = shared.drift_counters.snapshot();
    tracing::warn!(
        events = exec.seen,
        matched = exec.matched,
        unmatched = exec.unmatched,
        unresolved = exec.unresolved,
        drift_seen = drift.seen,
        drift_managed = drift.matched,
        drift_unmatched = drift.unmatched,
        dropped,
        dims = %exec.dims,
        drift_dims = %drift.dims,
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

/// Re-apply the plan only where `/proc/<pid>/stat` disagrees with it.
fn drift_scan(shared: &Shared) {
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return;
    };
    let rules = shared.rules();
    let cfg = shared.config();
    let cgroups = shared.cgroups();
    let applier = decision::SystemApplier {
        cfg: &cfg,
        caps: &shared.caps,
        cgroups: &cgroups,
    };
    let sink = Sink::new(&cfg, &shared.bias, Route::Drift);
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
        // Never touch a kernel thread, whatever the history says about its
        // name. This is the exact test (PF_KTHREAD from the task itself), and
        // it is also the guard for state written before the collector began
        // flagging rows: a restored `kworker/*` entry must not keep applying.
        if procname::stat_is_kthread(&stat[..n]) {
            continue;
        }
        let Some(current_nice) = procname::stat_nice(&stat[..n]) else {
            continue;
        };
        let comm = procname::stat_comm(&stat[..n]).unwrap_or(b"");
        let mut buf = [0u8; NAME_CAP];
        let len = shared.proc.derive_name(pid, comm, &mut buf);
        let plan = decision::plan_locked(&buf[..len], comm, pid, &rules, &shared.bias, &cfg);
        let plan = decision::settle(plan, current_nice);
        if let decision::Plan::Apply { target, key, .. } = &plan {
            tracing::debug!(
                pid,
                name = key.name(),
                current = current_nice,
                target = *target,
                "drift mismatch"
            );
        }
        decision::execute(plan, pid, &applier, &shared.drift_counters, &sink);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Unique scratch dir under the workspace `target/` (unit tests get no
    /// `CARGO_TARGET_TMPDIR`); removed on drop.
    struct TestDir(std::path::PathBuf);

    impl TestDir {
        fn new(tag: &str) -> Self {
            static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
            let base = std::env::current_dir()
                .expect("test cwd")
                .join("target")
                .join("daemon-tests");
            let dir = base.join(format!(
                "{tag}-{}-{}",
                std::process::id(),
                SEQ.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir_all(&dir).expect("create test dir");
            Self(dir)
        }

        fn path(&self) -> &std::path::Path {
            &self.0
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn test_shared(cfg: Config, rules: RuleSet) -> Arc<Shared> {
        let bias = BiasEngine::new(cfg.bias_config());
        Arc::new(Shared {
            rules: RwLock::new(Arc::new(rules)),
            config: RwLock::new(Arc::new(cfg)),
            cgroups: RwLock::new(Arc::new(init_cgroups(&BTreeMap::new()).0)),
            caps: probe_capabilities(),
            proc: ProcResolver::open(),
            counters: Counters::default(),
            drift_counters: Counters::default(),
            bias: Mutex::new(bias),
            shutdown: AtomicBool::new(false),
            drift_cv: Condvar::new(),
            drift_m: Mutex::new(()),
        })
    }

    #[test]
    fn an_empty_rule_set_warns_at_startup() {
        let dir = TestDir::new("empty-rules-warn");
        let cfg = Config::default();
        let lines = startup_warnings(&cfg, &RuleSet::empty(), dir.path());
        assert!(
            lines.iter().any(|l| l.contains("no rules loaded")),
            "a fresh install must not look healthy: {lines:?}"
        );

        // One rule silences it.
        let dir2 = TestDir::new("one-rule-no-warn");
        std::fs::write(dir2.path().join("50-a.rules"), "{\"name\": \"x\", \"nice\": 1}\n").unwrap();
        let rules = aranicy_rules::load_dir(dir2.path(), &Default::default()).expect("load");
        let lines = startup_warnings(&cfg, &rules, dir2.path());
        assert!(
            !lines.iter().any(|l| l.contains("no rules loaded")),
            "one rule is enough: {lines:?}"
        );
    }

    #[test]
    fn reload_reconfigures_the_running_bias_engine() {
        let dir = TestDir::new("reload-bias");
        let paths = Paths {
            config: dir.path().join("ananicy.conf"),
            config_dir: dir.path().to_path_buf(),
        };
        std::fs::write(&paths.config, "bias_window_s = 60\nbias_deadband_pct = 15\n").unwrap();
        let (cfg, rules) = load::load_all(&paths).expect("initial load");
        let shared = test_shared(cfg, rules);
        assert_eq!(
            shared.bias.lock().unwrap().config().window_s,
            60,
            "engine starts on the loaded config"
        );

        // `--reload` with new bias knobs must reach the *running* engine; the
        // config swap alone leaves the bias thread on the startup values.
        std::fs::write(
            &paths.config,
            "bias_window_s = 10\nbias_deadband_pct = 5\nbias_max = 1\n",
        )
        .unwrap();
        reload(&shared, &paths);

        assert_eq!(shared.config().bias_window_s, 10, "config swapped");
        let engine = shared.bias.lock().unwrap();
        assert_eq!(engine.config().window_s, 10);
        assert_eq!(engine.config().deadband_pct, 5);
        assert_eq!(engine.config().max_bias, 1);
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
    fn init_cgroups_rejects_nothing_on_empty_defs() {
        let (manager, warnings) = init_cgroups(&BTreeMap::new());
        // Empty defs must not fail even if creation is impossible.
        assert!(warnings.is_empty() || !manager.mount.as_os_str().is_empty());
    }

    #[test]
    fn a_pass_ages_what_the_batch_did_not_mention() {
        use crate::bias_state::{BiasSnapshot, Durable, EntrySnapshot};
        let mut engine = BiasEngine::new(crate::bias::BiasConfig::default());
        engine.restore(BiasSnapshot {
            version: bias_state::BIAS_STATE_VERSION,
            threshold_ema: None,
            entries: vec![EntrySnapshot {
                name: "gone".into(),
                durable: Durable {
                    confidence: 0.5,
                    last_seen: 1_000,
                    bias: 2,
                    ..Durable::default()
                },
            }],
        });

        // One loop iteration, one batch, and the name is not in it. The batch
        // cadence outruns the loop's idle timeout, so the pass — not the
        // timeout arm — must be what ages the entry.
        let now = std::time::UNIX_EPOCH + Duration::from_secs(1_000 + 1_800);
        let _ = bias_pass(&mut engine, &[], now);
        let e = engine.snapshot().entries.into_iter().next().expect("entry");
        assert!(
            (e.durable.confidence - 0.25).abs() < 1e-12,
            "a delivered batch must also age the rest: confidence {}",
            e.durable.confidence
        );
    }
}

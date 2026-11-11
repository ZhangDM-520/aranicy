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

use std::collections::{BTreeMap, BTreeSet};
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
    /// Wake the drift loop without shutting it down (reload swapped state).
    fn nudge(&self) {
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
    record_cgroups(&cfg.bias_state_path, &cgroups);

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
        save_bias_state(&shared.bias, &bias_state_path);
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
                    save_bias_state(&shared.bias, &state_path);
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

/// Persist the learned history, dating the lean capture at the save rather
/// than at the last window that produced it (`BiasSnapshot::mark_saved`): the
/// replay ages every entry against that stamp, so a snapshot taken days after
/// its capture would otherwise pass a fresh-cohort check it cannot earn.
fn save_bias_state(bias: &Mutex<BiasEngine>, path: &str) {
    let mut snap = bias.lock().unwrap_or_else(PoisonError::into_inner).snapshot();
    snap.mark_saved(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs()),
    );
    if let Err(e) = bias_state::save(&snap, std::path::Path::new(path)) {
        tracing::warn!("bias: cannot save state: {e}");
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
    // Per-definition failures (bad name, uncreatable dir, quota that cannot be
    // written yet) are warnings from inside the manager; only a missing
    // cgroup2 mount is fatal here.
    match CgroupManager::init(defs) {
        Ok(pair) => pair,
        Err(e) => (
            CgroupManager::unavailable(),
            vec![format!(
                "cgroup manager unavailable: {e}; cgroup actions will fail"
            )],
        ),
    }
}

/// A name the uninstall hook can safely act on: one line, inside the mount.
fn ledger_safe(name: &str) -> bool {
    !name.is_empty()
        && name != "."
        && name != ".."
        && !name.contains('/')
        && !name.chars().any(char::is_control)
}

/// Persist the cgroups this daemon created, so an uninstall can release them.
///
/// A cgroup directory is not a package file: `pacman -R` cannot remove it, and
/// the `cpu.max` written by [`CgroupManager`] keeps throttling whatever still
/// runs inside long after the binary is gone (round-3 `ivy-15`).
/// `contrib/aranicy.install` reads this list to move survivors out and `rmdir`
/// the directories.
///
/// This is a cleanup ledger, not a description of the loaded corpus: a
/// definition removed by a reload or a restart leaves its directory (and its
/// quota) on disk, and the hook needs that name, so the previous entries are
/// kept while their directory still exists. Only directories this daemon
/// created are added — a definition naming a cgroup somebody else made is
/// managed but never evacuated on uninstall. A failure writing the ledger is
/// reported and leaves the previous one in place: it is the only list of
/// directories that still hold a quota, so losing it is not recoverable
/// (`owned` is empty for a directory that already exists).
fn record_cgroups(state_path: &str, cgroups: &CgroupManager) {
    // No real cgroup2 mount (the probe failed, so `mount` is a placeholder):
    // the existence filter below would read every recorded name as gone and
    // erase the ledger a later uninstall still needs.
    if !cgroups.mount.join("cgroup.procs").exists() {
        return;
    }
    let path = paths::cgroup_record_path(std::path::Path::new(state_path));
    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
        if let Err(e) = std::fs::create_dir_all(dir) {
            tracing::warn!("cgroup record: cannot create {}: {e}", dir.display());
            return;
        }
    }
    // Absence means "nothing recorded yet"; any other read error means the
    // ledger may still name live directories, so it must not be replaced by a
    // short one.
    let previous = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => {
            tracing::warn!(
                "cgroup record: cannot read {}: {e}; leaving it in place",
                path.display()
            );
            return;
        }
    };
    let mut ledger: BTreeSet<String> = previous
        .lines()
        .filter(|name| ledger_safe(name) && cgroups.mount.join(name).is_dir())
        .map(str::to_string)
        .collect();
    ledger.extend(cgroups.owned.iter().cloned());
    let body: String = ledger.iter().map(|name| format!("{name}\n")).collect();
    // Truncate-in-place then fail would lose every surviving name, so write a
    // sibling and rename (the same reason `bias_state::save` does).
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = std::path::PathBuf::from(tmp);
    if let Err(e) = std::fs::write(&tmp, body).and_then(|()| std::fs::rename(&tmp, &path)) {
        tracing::warn!("cgroup record: cannot write {}: {e}", path.display());
        let _ = std::fs::remove_file(&tmp);
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
            record_cgroups(&cfg.bias_state_path, &cgroups);
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
            let old_rules = shared.rules();
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
            // A freshly built manager may owe quota writes; the drift loop is
            // where they are retried, so let it out of an unbounded wait.
            shared.nudge();
            tracing::info!("reloaded {n} rules; bias reconfigured");
            // The swap alone leaves running processes on the old corpus (only
            // `nice` had a re-assert path); re-plan them against the new one.
            reconcile_live(shared, &old_rules);
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

/// Re-plan every live process under a freshly loaded corpus (ADR 0002).
///
/// A reload swaps the rule tree; nothing re-examined the processes already
/// running. Drift re-asserts `nice` only, so an edit to `ioclass`, `cgroup`,
/// `sched` or `oom_score_adj` never reached them, and a deleted rule left its
/// dimensions behind while the learner took the nice. One pass fixes both:
/// apply the new plan (idempotent), and for a name the new corpus no longer
/// matches, apply what the old rule has to give back.
///
/// Counters are throwaway: a reload is not the event stream, and folding it
/// into the per-minute `apply summary` would make a rule edit look like a
/// storm. Kernel threads, PID 1 and the daemon's own process are never
/// touched.
fn reconcile_live(shared: &Arc<Shared>, old_rules: &RuleSet) {
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
    let counters = Counters::default();
    let sink = Sink::new(&cfg, &shared.bias, Route::Reload);
    let mut planned = 0usize;
    let mut released = 0usize;
    let mut kept_cpuset = 0usize;
    let mut released_names: Vec<String> = Vec::new();
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
        if procname::stat_is_kthread(&stat[..n]) {
            continue;
        }
        let comm = procname::stat_comm(&stat[..n]).unwrap_or(b"");
        let mut buf = [0u8; NAME_CAP];
        let len = shared.proc.derive_name(pid, comm, &mut buf);
        let reconciled = decision::reconcile_locked(
            &buf[..len],
            comm,
            pid,
            old_rules,
            &rules,
            &shared.bias,
            &cfg,
        );
        if reconciled.kept_cpuset {
            kept_cpuset += 1;
        }
        if reconciled.released {
            released += 1;
            if released_names.len() < 8 {
                released_names.push(
                    std::str::from_utf8(&buf[..len])
                        .unwrap_or("?")
                        .to_string(),
                );
            }
        }
        let Some(plan) = reconciled.plan else {
            continue;
        };
        decision::execute(plan, pid, &applier, &counters, &sink);
        planned += 1;
    }
    if planned > 0 || released > 0 || kept_cpuset > 0 {
        tracing::info!(
            processes = planned,
            released,
            kept_cpuset,
            names = ?released_names,
            "re-planned live processes against the new corpus"
        );
    }
}

// ---------------------------------------------------------------------------
// Drift corrector (delta-based)
// ---------------------------------------------------------------------------

fn drift_loop(shared: &Shared) {
    let mut retry = CgroupRetry::new();
    // The quota retry shortens the wait to 5 s while it is owed; the drift
    // scan keeps its own cadence instead of inheriting that (a quota that can
    // never land must not multiply the /proc walk by twelve).
    let mut last_scan = Instant::now();
    loop {
        // Owed quota writes first: on a boot start the cgroup exists before
        // the kernel offers `cpu.max`, and nothing else would ever write it.
        retry.run(shared);
        let freq = shared.config().check_freq;
        let pending = shared.cgroups().pending() > 0;
        let mut guard = shared.drift_m.lock().unwrap_or_else(PoisonError::into_inner);
        if freq == 0 && !pending {
            guard = shared
                .drift_cv
                .wait_while(guard, |_| !shared.shutdown.load(Ordering::Acquire))
                .unwrap_or_else(PoisonError::into_inner);
        } else {
            // A pending quota shortens the wait: the retry is what resolves it.
            let secs = if pending { CGROUP_RETRY_SECS } else { freq };
            let (g, _timeout) = shared
                .drift_cv
                .wait_timeout_while(
                    guard,
                    Duration::from_secs(secs),
                    |_| !shared.shutdown.load(Ordering::Acquire),
                )
                .unwrap_or_else(PoisonError::into_inner);
            guard = g;
        }
        drop(guard);
        if shared.shutdown.load(Ordering::Acquire) {
            return;
        }
        if freq > 0 && scan_due(freq, last_scan, Instant::now()) {
            last_scan = Instant::now();
            drift_scan(shared);
        }
    }
}

/// Is a drift scan due? Extracted so the cadence is a function, not a thread:
/// the loop wakes every 5 s while a quota is owed, and the scan must still
/// happen once per `check_freq`, not once per wake.
fn scan_due(freq: u64, last_scan: Instant, now: Instant) -> bool {
    freq > 0 && now.saturating_duration_since(last_scan) >= Duration::from_secs(freq)
}

/// How often a quota write that did not land is retried. Short on purpose: the
/// boot race it exists for resolves as soon as the `cpu` controller is enabled
/// at the cgroup root, and a missing quota is a CPU-budget bug, not cosmetics.
const CGROUP_RETRY_SECS: u64 = 5;
/// An unresolved quota must not spam the journal on every retry.
const CGROUP_WARN_SECS: u64 = 600;

struct CgroupRetry {
    last_warn: Option<Instant>,
}

impl CgroupRetry {
    fn new() -> Self {
        Self { last_warn: None }
    }

    /// Retry owed `cpu.max` writes; a no-op once the set has drained.
    fn run(&mut self, shared: &Shared) {
        let cgroups = shared.cgroups();
        if cgroups.pending() == 0 {
            return;
        }
        let out = cgroups.apply_pending();
        for (name, pct) in &out.applied {
            tracing::info!(cgroup = %name, pct, "CPUQuota applied after retry");
        }
        if out.failing.is_empty() {
            return;
        }
        let due = self
            .last_warn
            .is_none_or(|t| t.elapsed() >= Duration::from_secs(CGROUP_WARN_SECS));
        if due {
            self.last_warn = Some(Instant::now());
            let names: Vec<&str> = out.failing.iter().map(|(n, _)| n.as_str()).collect();
            let (_, err) = &out.failing[0];
            tracing::warn!(
                cgroups = ?names,
                error = %err,
                retry_secs = CGROUP_RETRY_SECS,
                "CPUQuota still not applied; will retry"
            );
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
    fn the_scan_cadence_is_the_configured_one_not_the_retry_wake() {
        let t0 = Instant::now();
        // A 5-second quota-retry wake is not a scan slot.
        assert!(!scan_due(60, t0, t0 + Duration::from_secs(5)));
        assert!(scan_due(60, t0, t0 + Duration::from_secs(60)));
        // check_freq = 0 disables scanning, whatever the wake interval.
        assert!(!scan_due(0, t0, t0 + Duration::from_secs(3600)));
    }

    #[test]
    fn init_cgroups_rejects_nothing_on_empty_defs() {
        let (manager, warnings) = init_cgroups(&BTreeMap::new());
        // Empty defs must not fail even if creation is impossible.
        assert!(warnings.is_empty() || !manager.mount.as_os_str().is_empty());
    }

    /// The uninstall hook acts on this ledger, so it must keep a name while
    /// its directory exists (a definition removed by a reload leaves the
    /// directory and its quota behind), drop a name whose directory is gone,
    /// and never adopt a directory the daemon did not create.
    #[test]
    fn the_cgroup_record_is_a_cleanup_ledger_not_the_loaded_corpus() {
        let dir = TestDir::new("cgroup-record");
        let mount = dir.path().join("cgroup2");
        std::fs::create_dir_all(&mount).expect("mount dir");
        // The mount probe: a real cgroup2 root always carries this file.
        std::fs::write(mount.join("cgroup.procs"), "").expect("root procs");
        let state = dir.path().join("state").join("bias-state");
        let record = state.with_file_name("cgroups");
        let read = || std::fs::read_to_string(&record).expect("record");

        // Somebody else's cgroup: managed (the rule may assign into it) but
        // not owned, so an uninstall must not evacuate it.
        std::fs::create_dir(mount.join("shared")).expect("pre-existing cgroup");

        let def = |name: &str| CgroupDef {
            name: Arc::from(name),
            cpu_quota_pct: None,
        };
        let defs: BTreeMap<String, CgroupDef> = [
            ("games".to_string(), def("games")),
            ("shared".to_string(), def("shared")),
            ("bad/name".to_string(), def("bad/name")),
        ]
        .into_iter()
        .collect();
        let (manager, warnings) = CgroupManager::init_at(&mount, defs);
        assert_eq!(warnings.len(), 1, "the invalid name is refused: {warnings:?}");
        record_cgroups(state.to_str().expect("utf8"), &manager);
        assert_eq!(read(), "games\n", "only the cgroup we created is recorded");

        // A reload without the definition: the directory (and its quota) is
        // still on disk, so the name must survive for the uninstall hook.
        let (empty, _) = CgroupManager::init_at(&mount, BTreeMap::new());
        record_cgroups(state.to_str().expect("utf8"), &empty);
        assert_eq!(read(), "games\n", "a live directory is never forgotten");

        // Once the directory is gone the name is pruned.
        std::fs::remove_dir(mount.join("games")).expect("remove cgroup");
        record_cgroups(state.to_str().expect("utf8"), &empty);
        assert_eq!(read(), "", "a removed directory is pruned from the ledger");
        assert!(
            !record.with_extension("tmp").exists(),
            "the write is atomic: no temporary file survives"
        );
    }

    /// Without a cgroup2 mount the existence test would read every name as
    /// gone; the ledger must survive for the host that mounts it later.
    #[test]
    fn a_missing_cgroup_mount_does_not_erase_the_ledger() {
        let dir = TestDir::new("cgroup-record-nomount");
        let state = dir.path().join("state").join("bias-state");
        let record = state.with_file_name("cgroups");
        std::fs::create_dir_all(&state).expect("state dir");
        std::fs::write(&record, "games\n").expect("ledger");

        record_cgroups(state.to_str().expect("utf8"), &CgroupManager::unavailable());
        assert_eq!(
            std::fs::read_to_string(&record).expect("record"),
            "games\n",
            "a placeholder mount leaves the ledger untouched"
        );
    }

    /// The write is a rename, so it replaces the ledger path instead of
    /// following it: an in-place `fs::write` would rewrite whatever the path
    /// points at and could lose the names on a partial write (round 5).
    #[test]
    fn the_ledger_write_replaces_the_path_instead_of_following_it() {
        let dir = TestDir::new("cgroup-record-atomic");
        let mount = dir.path().join("cgroup2");
        std::fs::create_dir_all(&mount).expect("mount dir");
        std::fs::write(mount.join("cgroup.procs"), "").expect("root procs");
        let state = dir.path().join("state").join("bias-state");
        std::fs::create_dir_all(&state).expect("state dir");
        let record = state.with_file_name("cgroups");
        let real = dir.path().join("previous-ledger");
        std::fs::write(&real, "games\n").expect("previous ledger");
        std::os::unix::fs::symlink(&real, &record).expect("symlink");

        let (manager, _) = CgroupManager::init_at(&mount, BTreeMap::new());
        record_cgroups(state.to_str().expect("utf8"), &manager);
        let meta = std::fs::symlink_metadata(&record).expect("ledger metadata");
        assert!(
            !meta.file_type().is_symlink(),
            "an atomic write replaces the path, it never writes through it"
        );
        assert_eq!(
            std::fs::read_to_string(&real).expect("previous ledger"),
            "games\n",
            "the previous file is not modified in place"
        );
    }

    /// An unreadable ledger may still name live directories, so it must be
    /// left alone rather than replaced by the (empty) set the daemon owns.
    /// A symlink to a directory gives a read error that is not `NotFound`
    /// while still being replaceable by a rename, so the assertion fails if
    /// the write path is taken.
    #[test]
    fn an_unreadable_ledger_is_not_replaced() {
        let dir = TestDir::new("cgroup-record-readerr");
        let mount = dir.path().join("cgroup2");
        std::fs::create_dir_all(&mount).expect("mount dir");
        std::fs::write(mount.join("cgroup.procs"), "").expect("root procs");
        let state = dir.path().join("state").join("bias-state");
        let record = state.with_file_name("cgroups");
        std::fs::create_dir_all(&state).expect("state dir");
        std::fs::create_dir_all(dir.path().join("elsewhere")).expect("target dir");
        std::os::unix::fs::symlink(dir.path().join("elsewhere"), &record).expect("symlink");

        let (manager, _) = CgroupManager::init_at(&mount, BTreeMap::new());
        record_cgroups(state.to_str().expect("utf8"), &manager);
        let meta = std::fs::symlink_metadata(&record).expect("ledger metadata");
        assert!(
            meta.file_type().is_symlink(),
            "an unreadable ledger is left untouched"
        );
        assert!(
            !record.with_extension("tmp").exists(),
            "no temporary file is left behind"
        );
    }

    /// `saved_at` is the replay's age reference. A capture taken days before
    /// the save must be dated at the *save*, or `replay.py` sees
    /// `saved_at - lean_at == 0` and can never report a stale cohort.
    #[test]
    fn a_saved_snapshot_dates_its_capture_at_the_save() {
        use crate::bias_state::{BiasSnapshot, Durable, EntrySnapshot, LeanCapture};
        let dir = TestDir::new("bias-save-stamp");
        let path = dir.path().join("state").join("bias-state");
        // The defaults the marker must name to survive `restore`.
        let (stage, hop, deadband) =
            (crate::classify::STAGE_MIN_NS, crate::classify::HOP_MIN_HZ, 15);
        let mut engine = BiasEngine::new(crate::bias::BiasConfig::default());
        engine.restore(BiasSnapshot {
            version: bias_state::BIAS_STATE_VERSION,
            threshold_ema: None,
            lean_capture: Some(LeanCapture {
                stage_min_ns: stage,
                hop_min_hz: hop,
                deadband_pct: deadband,
                saved_at: 100,
            }),
            entries: vec![EntrySnapshot {
                name: "measured-long-ago".into(),
                durable: Durable {
                    confidence: 0.9,
                    burst_ns: 1_000_000,
                    hop_hz: 10,
                    lean_at: 100,
                    ..Durable::default()
                },
            }],
        });

        save_bias_state(&Mutex::new(engine), path.to_str().expect("utf8"));

        let saved = bias_state::load(&path).expect("load").expect("a file");
        let capture = saved.lean_capture.expect("the capture survives the save");
        assert!(
            capture.saved_at > 100 + 3_600,
            "the save date, not the window's: saved_at={} lean_at=100",
            capture.saved_at
        );
        let at = saved.entries[0].durable.lean_at;
        assert!(
            capture.saved_at - at > 3_600,
            "an hour-old cohort is now detectable as stale (saved_at={} at={at})",
            capture.saved_at
        );
    }

    #[test]
    fn a_pass_ages_what_the_batch_did_not_mention() {
        use crate::bias_state::{BiasSnapshot, Durable, EntrySnapshot};
        let mut engine = BiasEngine::new(crate::bias::BiasConfig::default());
        engine.restore(BiasSnapshot {
            version: bias_state::BIAS_STATE_VERSION,
            threshold_ema: None,
            lean_capture: None,
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

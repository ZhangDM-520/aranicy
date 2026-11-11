//! `scx_stats` per-task sampling client.
//!
//! Interface frozen between workstreams: [`Sample`] / [`Batch`].
//!
//! There is exactly one stats server — our collector (`aranicy-collectord`, a
//! separate GPL-2.0 package) — and it serves exactly one target
//! ([`STATS_TARGET`]) with one row key ([`ROWS_KEY`]). No scheduler is probed,
//! no schema is discovered and no target name is guessed: a server that does
//! not answer that one request is a transport failure, full stop. The socket
//! path is configurable (`bias_collector_socket`) for tests and relocated
//! installs.
//!
//! Wire contract, one JSON line in and one JSON line out per request:
//!
//! * `{"req":"stats","args":{"target":"sched_samples","nr_samples":"512"}}`
//!   — `nr_samples` is sent as a decimal *string*; the response envelope is
//!   `{"errno":N,"args":{"resp":{"samples":[...]}}}`. On `errno != 0` the
//!   `resp` carries a message string. (`scx_stats` serializes concurrent
//!   samplers itself; it never reports that another one took over.)
//! * Each request asks for the full observation budget
//!   ([`aranicy_wire::SAMPLE_CAP`]); the response reports `truncated` — how
//!   many rows the budget dropped — and the sampler logs that at `warn!`
//!   (interval-only: at most once a minute). Sequential round-trips only —
//!   pipelining serializes server-side.
//!
//! Field names are the collector's verbatim; [`canonical_field`] is the only
//! schema-drift hook and [`Sample`] parses leniently.
//!
//! [`spawn_collector`] runs the single sampler thread: it delivers [`Batch`]es
//! through an `mpsc::Sender`, logs exactly one line per [`SamplerState`]
//! transition, and reconnects with exponential backoff after any transport
//! failure. The socket sits behind [`Endpoint`]/[`Link`], so the reconnect,
//! backoff and pacing loop is exercised without a socket, a sleep or a
//! five-second timeout.

use std::fmt;
use std::io::{BufRead, BufReader, Write};
use std::net::Shutdown;
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::mpsc::Sender;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};

/// Parse a per-task row into the shared wire sample. Parsing is lenient:
/// unknown fields ignored, missing fields defaulted (`serde(default)`).
///
/// Because of that leniency a *missing* field is indistinguishable from a
/// meaningful zero (`wait_freq == 0` legitimately means "never slept"). That
/// is deliberate: the collector always emits all fields, and the leniency is
/// what keeps an older/newer server parseable.
pub use aranicy_wire::Sample;

/// One batch: lossy, ordered by demand, cut to the observation budget.
pub type Batch = Vec<Sample>;

/// Default wall-clock pacing between sample requests.
pub const DEFAULT_CADENCE: Duration = Duration::from_millis(500);

/// The collector is the only server: one target, one row key.
const STATS_TARGET: &str = "sched_samples";
const ROWS_KEY: &str = "samples";

const REASON_STARTING: &str = "starting";
const REASON_TRANSPORT: &str = "transport down";

/// Exponential reconnect backoff: 1 s doubling to a 30 s cap, ±25% jitter.
const BACKOFF_START: Duration = Duration::from_secs(1);
const BACKOFF_CAP: Duration = Duration::from_secs(30);
/// Minimum spacing of the truncation warning; an unchanged dropped count is
/// also logged only after this much time (never once per cadence).
const TRUNC_WARN_INTERVAL: Duration = Duration::from_secs(60);

/// Wedged-server guard for response reads. `shutdown()` still unblocks the
/// thread immediately — it calls `shutdown(2)` on the live socket.
const READ_TIMEOUT: Duration = Duration::from_secs(15);

// ---------------------------------------------------------------------------
// Transport seam
// ---------------------------------------------------------------------------

/// Everything the loop can observe from the wire. I/O, EOF, timeout,
/// malformed JSON and a server-side `errno != 0` are the same failure: the
/// link is dead, reconnect with backoff.
pub(crate) enum Fail {
    Transport(String),
}

/// One sequential request/response on a stats socket.
pub(crate) trait Link: Send {
    fn round_trip(&mut self, req: &Value) -> Result<Value, Fail>;
    /// Wake a blocked read (shutdown path); parked by the handle.
    fn abort_handle(&self) -> Option<Arc<dyn Abortable>>;
}

/// The shutdown side of a live link.
pub(crate) trait Abortable: Send + Sync {
    fn abort(&self);
}

/// How the loop obtains a fresh link. The only thing tests substitute.
pub(crate) trait Endpoint: Send + Sync + 'static {
    fn open(&self) -> Result<Box<dyn Link>, Fail>;
}

impl Abortable for UnixStream {
    fn abort(&self) {
        let _ = self.shutdown(Shutdown::Both);
    }
}

/// The production link: a connected socket plus a cloned fd for line reads.
struct UnixLink {
    stream: Arc<UnixStream>,
    reader: BufReader<UnixStream>,
}

impl UnixLink {
    fn new(stream: UnixStream) -> std::io::Result<Self> {
        stream.set_read_timeout(Some(READ_TIMEOUT))?;
        // A second fd for line reads; `shutdown(2)` on `stream` (same socket
        // object) still wakes them.
        let reader = BufReader::new(stream.try_clone()?);
        Ok(Self {
            stream: Arc::new(stream),
            reader,
        })
    }
}

impl Link for UnixLink {
    fn round_trip(&mut self, req: &Value) -> Result<Value, Fail> {
        let line = serde_json::to_string(req)
            .map_err(|e| Fail::Transport(format!("serialize request: {e}")))?;
        let mut w = &*self.stream;
        w.write_all(line.as_bytes())
            .and_then(|_| w.write_all(b"\n"))
            .and_then(|_| w.flush())
            .map_err(|e| Fail::Transport(format!("write: {e}")))?;
        let mut buf = String::new();
        let line = match self.reader.read_line(&mut buf) {
            Ok(0) => return Err(Fail::Transport("connection closed (EOF)".to_owned())),
            Ok(_) => buf,
            Err(e) => return Err(Fail::Transport(format!("read: {e}"))),
        };
        let (errno, resp) = parse_envelope(&line)?;
        check_errno(errno, &resp)?;
        Ok(resp)
    }

    fn abort_handle(&self) -> Option<Arc<dyn Abortable>> {
        Some(Arc::clone(&self.stream) as Arc<dyn Abortable>)
    }
}

/// The only production endpoint: connect to one UNIX socket.
struct UnixEndpoint {
    path: PathBuf,
}

impl Endpoint for UnixEndpoint {
    fn open(&self) -> Result<Box<dyn Link>, Fail> {
        let fail = |e: std::io::Error| Fail::Transport(format!("{}: {e}", self.path.display()));
        let stream = UnixStream::connect(&self.path).map_err(fail)?;
        let link = UnixLink::new(stream).map_err(fail)?;
        Ok(Box::new(link))
    }
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Sampler state machine, exposed for the daemon/log.
///
/// `Degraded` means "not delivering batches": the daemon keeps applying the
/// already-learned bias and stops updating it until the sampler is
/// [`SamplerState::Live`] again. The `reason` strings are a stable contract:
/// `"starting"` (before the first round trip) and `"transport down"` (I/O,
/// EOF, timeout, malformed payload or a server-side error).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SamplerState {
    /// The first `stats` round trip parsed; batches flowing.
    Live,
    /// Not delivering batches; `reason` is `"starting"` or `"transport down"`.
    Degraded { reason: String },
}

impl SamplerState {
    fn degraded(reason: &str) -> Self {
        Self::Degraded {
            reason: reason.to_owned(),
        }
    }
}

impl fmt::Display for SamplerState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Live => f.write_str("live"),
            Self::Degraded { reason } => write!(f, "degraded: {reason}"),
        }
    }
}

/// Handle to the sampler thread (the daemon spawns exactly one).
pub struct SchedSamplerHandle {
    /// The daemon never inspects the state (it reacts to batches, and the
    /// transition itself is traced); only tests observe it.
    #[cfg(test)]
    shared: Arc<Mutex<Shared>>,
    stop: Arc<StopFlag>,
    slot: AbortSlot,
    thread: Option<JoinHandle<()>>,
}

impl SchedSamplerHandle {
    /// Request shutdown and unblock the thread promptly: the inter-batch wait
    /// is a condvar wait, and a blocked socket read is woken by `shutdown(2)`
    /// on the live link. Idempotent.
    pub fn shutdown(&self) {
        self.stop.request();
        let abort = locked(&self.slot).take();
        if let Some(abort) = abort {
            abort.abort();
        }
    }

    /// Wait for the sampler thread to exit (after [`Self::shutdown`], or once
    /// the batch consumer is gone).
    pub fn join(mut self) -> std::thread::Result<()> {
        self.thread.take().map(|t| t.join()).unwrap_or(Ok(()))
    }
}

impl Drop for SchedSamplerHandle {
    fn drop(&mut self) {
        self.shutdown();
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// Spawn against the collector's fixed stats socket.
pub fn spawn_collector(socket: PathBuf, cadence: Duration, tx: Sender<Batch>) -> SchedSamplerHandle {
    let live_detail = format!("bound SchedSample via {STATS_TARGET} at {}", socket.display());
    spawn_with(
        Box::new(UnixEndpoint { path: socket }),
        tuning(cadence),
        live_detail,
        tx,
    )
}

fn tuning(cadence: Duration) -> Tuning {
    Tuning {
        cadence: if cadence.is_zero() {
            DEFAULT_CADENCE
        } else {
            cadence
        },
        backoff_start: BACKOFF_START,
        backoff_cap: BACKOFF_CAP,
    }
}

/// Spawn the single sampler thread over an arbitrary [`Endpoint`].
///
/// It requests [`STATS_TARGET`] every `cadence` (wall-clock paced: missed
/// slots are skipped, never caught up in bursts) and delivers each parsed
/// [`Batch`] to `tx`. A dropped receiver stops the thread. Exactly one
/// tracing line is emitted per [`SamplerState`] transition; batches are never
/// logged. A zero `cadence` falls back to [`DEFAULT_CADENCE`].
fn spawn_with(
    endpoint: Box<dyn Endpoint>,
    tuning: Tuning,
    live_detail: String,
    tx: Sender<Batch>,
) -> SchedSamplerHandle {
    let shared = Arc::new(Mutex::new(Shared {
        state: SamplerState::degraded(REASON_STARTING),
    }));
    let stop = Arc::new(StopFlag::new());
    let slot: AbortSlot = Arc::new(Mutex::new(None));
    let thread = std::thread::Builder::new()
        .name("stats-sampler".to_owned())
        .spawn({
            let shared = Arc::clone(&shared);
            let stop = Arc::clone(&stop);
            let slot = Arc::clone(&slot);
            let runner = Runner {
                endpoint,
                tuning,
                live_detail,
                tx,
            };
            move || run(runner, shared, stop, slot)
        })
        .expect("spawn stats sampler thread");
    SchedSamplerHandle {
        #[cfg(test)]
        shared,
        stop,
        slot,
        thread: Some(thread),
    }
}

// ---------------------------------------------------------------------------
// Internals
// ---------------------------------------------------------------------------

/// Where the live link's abort handle is parked so `shutdown()` can wake a
/// blocked read.
type AbortSlot = Arc<Mutex<Option<Arc<dyn Abortable>>>>;

struct Tuning {
    cadence: Duration,
    backoff_start: Duration,
    backoff_cap: Duration,
}

/// What the sampler thread needs to run: the transport, the cadence, and the
/// consumer.
struct Runner {
    endpoint: Box<dyn Endpoint>,
    tuning: Tuning,
    /// Log detail for `Live` transitions (which socket, which target).
    live_detail: String,
    tx: Sender<Batch>,
}

struct Shared {
    state: SamplerState,
}

fn locked<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The one-and-only log site: a single line per state transition, never
/// per-batch. `detail` carries the bind summary for `Live` transitions and the
/// transport error for `Degraded` ones.
fn transition(shared: &Mutex<Shared>, new: SamplerState, detail: &str) {
    let mut s = locked(shared);
    if s.state == new {
        return;
    }
    match &new {
        SamplerState::Live => {
            tracing::info!(prev = %s.state, detail = %detail, "stats sampler live")
        }
        SamplerState::Degraded { reason } => {
            tracing::warn!(prev = %s.state, reason = %reason, detail = %detail, "stats sampler degraded")
        }
    }
    s.state = new;
}

struct StopFlag {
    flag: Mutex<bool>,
    cv: Condvar,
}

impl StopFlag {
    fn new() -> Self {
        Self {
            flag: Mutex::new(false),
            cv: Condvar::new(),
        }
    }

    fn request(&self) {
        *locked(&self.flag) = true;
        self.cv.notify_all();
    }

    fn requested(&self) -> bool {
        *locked(&self.flag)
    }

    /// Wait until `deadline`; returns `false` when shutdown was requested.
    fn wait_until(&self, deadline: Instant) -> bool {
        let mut guard = locked(&self.flag);
        while !*guard {
            let now = Instant::now();
            if now >= deadline {
                return true;
            }
            let (g, _timed) = self
                .cv
                .wait_timeout(guard, deadline - now)
                .unwrap_or_else(PoisonError::into_inner);
            guard = g;
        }
        false
    }

    fn wait(&self, delay: Duration) -> bool {
        self.wait_until(Instant::now() + delay)
    }
}

fn seed_rng() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0x9E37_79B9_7F4A_7C15)
        | 1
}

/// SplitMix64 step — jitter without new dependencies.
fn next_rand(state: &mut u64) -> u64 {
    *state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
    let mut z = *state;
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

/// `base` scaled by a random factor in [0.75, 1.25].
fn jitter(base: Duration, state: &mut u64) -> Duration {
    let milli = 750 + next_rand(state) % 501;
    base.mul_f64(milli as f64 / 1000.0)
}

struct Backoff {
    start: Duration,
    next: Duration,
    cap: Duration,
    rng: u64,
}

impl Backoff {
    fn new(start: Duration, cap: Duration) -> Self {
        Self {
            start,
            next: start,
            cap,
            rng: seed_rng(),
        }
    }

    /// Delay for this failure, then advance the doubling schedule.
    fn delay(&mut self) -> Duration {
        let d = jitter(self.next, &mut self.rng);
        self.next = self.next.saturating_mul(2).min(self.cap);
        d
    }

    fn reset(&mut self) {
        self.next = self.start;
    }
}

fn run(runner: Runner, shared: Arc<Mutex<Shared>>, stop: Arc<StopFlag>, slot: AbortSlot) {
    let Runner {
        endpoint,
        tuning,
        live_detail,
        tx,
    } = runner;
    let mut backoff = Backoff::new(tuning.backoff_start, tuning.backoff_cap);
    // Observation-budget visibility survives reconnects: warn when the served
    // window is truncated, at most once per `TRUNC_WARN_INTERVAL`.
    let mut last_trunc_warn: Option<Instant> = None;
    'outer: loop {
        if stop.requested() {
            break;
        }
        // One socket, one request shape: opening is the whole "bind", and a
        // server that accepts but never answers a `stats` request stays
        // `Degraded` — the state flips only on a parsed round trip.
        let mut link = match endpoint.open() {
            Ok(link) => link,
            Err(Fail::Transport(detail)) => {
                transition(&shared, SamplerState::degraded(REASON_TRANSPORT), &detail);
                if !stop.wait(backoff.delay()) {
                    break;
                }
                continue 'outer;
            }
        };
        *locked(&slot) = link.abort_handle();
        let mut next = Instant::now();
        loop {
            if !stop.wait_until(next) {
                break 'outer;
            }
            let req = json!({
                "req": "stats",
                // Full observation budget: the collector cuts to the same
                // number, so asking for less would silently shrink it.
                "args": { "target": STATS_TARGET, "nr_samples": aranicy_wire::SAMPLE_CAP.to_string() }
            });
            let (batch, truncated) = match link
                .round_trip(&req)
                .and_then(|resp| parse_batch(&resp).map_err(Fail::Transport))
            {
                Ok(parsed) => parsed,
                Err(Fail::Transport(detail)) => {
                    if stop.requested() {
                        break 'outer;
                    }
                    // Clear the parked handle before announcing the state, so
                    // an observer of `Degraded` can never find a dead link.
                    *locked(&slot) = None;
                    transition(&shared, SamplerState::degraded(REASON_TRANSPORT), &detail);
                    if !stop.wait(backoff.delay()) {
                        break 'outer;
                    }
                    continue 'outer;
                }
            };
            // A parsed round trip is the whole "bind": only then is the
            // connection known good, so only then does the backoff reset.
            backoff.reset();
            transition(&shared, SamplerState::Live, &live_detail);
            // Interval-only: the count moves by a few rows every window, so a
            // "changed" trigger would fire at the cadence (measured: 26
            // warnings/min) — a flood, not a diagnostic.
            if truncated > 0
                && last_trunc_warn.is_none_or(|t| t.elapsed() >= TRUNC_WARN_INTERVAL)
            {
                tracing::warn!(
                    truncated,
                    rows = batch.len(),
                    "observation budget truncated the sample window"
                );
                last_trunc_warn = Some(Instant::now());
            }
            if tx.send(batch).is_err() {
                break 'outer; // consumer is gone
            }
            // Wall-clock pacing: advance one cadence and skip any missed
            // slots — never a catch-up burst after a slow round-trip or a
            // reconnect.
            next += tuning.cadence;
            let now = Instant::now();
            while next <= now {
                next += tuning.cadence;
            }
        }
    }
    let abort = locked(&slot).take();
    if let Some(abort) = abort {
        abort.abort();
    }
}

// ---------------------------------------------------------------------------
// Wire parsing (lenient)
// ---------------------------------------------------------------------------

fn parse_envelope(line: &str) -> Result<(i64, Value), Fail> {
    let root: Value = serde_json::from_str(line.trim())
        .map_err(|e| Fail::Transport(format!("malformed response JSON: {e}")))?;
    // `errno` may arrive as a number or a numeric string across versions.
    let errno = match root.get("errno") {
        Some(Value::Number(n)) => n.as_i64().unwrap_or(0),
        Some(Value::String(s)) => s.trim().parse().unwrap_or(0),
        _ => 0,
    };
    // `args.resp` is the target-specific payload; probe the root for
    // envelope-less responses (tolerated shape).
    let resp = root.pointer("/args/resp").cloned().unwrap_or(root);
    Ok((errno, resp))
}

/// A server-side rejection (`errno != 0`) carries its message in `resp`; it is
/// as fatal as a dropped connection.
fn check_errno(errno: i64, resp: &Value) -> Result<(), Fail> {
    if errno == 0 {
        return Ok(());
    }
    let msg = match resp {
        Value::String(s) => s.clone(),
        Value::Object(map) => map
            .get("message")
            .or_else(|| map.get("msg"))
            .or_else(|| map.get("error"))
            .and_then(Value::as_str)
            .map(str::to_owned)
            .unwrap_or_else(|| resp.to_string()),
        other => other.to_string(),
    };
    Err(Fail::Transport(format!("errno {errno}: {msg}")))
}

/// Numeric fields of [`Sample`] (`pid` signed, the rest unsigned); these are
/// the fields accepted as int-or-string.
const UNSIGNED_FIELDS: &[&str] = &[
    "wait_freq",
    "wake_freq",
    "avg_runtime_wall",
    "task_util_est",
    "cpu_util_wall",
    "static_prio",
    "runs",
    "sleeps",
    // A flag, but numeric for the same reason: the `scx_stats` row type has no
    // `bool`. Dropping it here silently turned every kernel thread into a
    // user task (measured: 83 kthread entries created right after the flag
    // shipped, because this whitelist is a second projection of the schema).
    "kthread",
];

/// Coerce an int-or-string field to a JSON number (`pid` accepts i64, the
/// unsigned fields u64). Anything else returns `None`, and the field falls
/// back to its `serde(default)`.
fn coerce_number(val: &Value, signed: bool) -> Option<Value> {
    match val {
        Value::Number(n) => {
            if n.as_u64().is_some() || (signed && n.as_i64().is_some()) {
                Some(val.clone())
            } else {
                None
            }
        }
        Value::String(s) => {
            let t = s.trim();
            if signed {
                t.parse::<i64>().ok().map(Value::from)
            } else {
                t.parse::<u64>().ok().map(Value::from).or_else(|| {
                    t.parse::<i64>()
                        .ok()
                        .and_then(|i| u64::try_from(i).ok())
                        .map(Value::from)
                })
            }
        }
        _ => None,
    }
}

/// Canonicalisation hook for schema drift: map an on-wire field name onto
/// [`Sample`]'s field names. The collector emits them verbatim; verified
/// synonyms of a future schema get an arm here — never speculative guesses.
fn canonical_field(raw: &str) -> &str {
    raw
}

/// Lenient [`Sample`] parse: unknown fields ignored (a server may carry more
/// fields than [`Sample`] knows), numbers accepted as ints or strings,
/// wrong-typed or missing fields defaulted via `serde(default)`.
fn lenient_sample(raw: &Value) -> Option<Sample> {
    let raw = raw.as_object()?;
    let mut norm = serde_json::Map::with_capacity(raw.len());
    for (key, val) in raw {
        let key = canonical_field(key);
        let coerced = match key {
            "pid" => coerce_number(val, true),
            "comm" => val.as_str().map(|_| val.clone()),
            name if UNSIGNED_FIELDS.contains(&name) => coerce_number(val, false),
            _ => None, // unknown field: serde ignores it anyway
        };
        if let Some(v) = coerced {
            norm.insert(key.to_owned(), v);
        }
    }
    serde_json::from_value(Value::Object(norm)).ok()
}

/// `resp` → ([`Batch`], `truncated`). Unreadable entries are dropped
/// individually (leniency); a payload without the [`ROWS_KEY`] array is a
/// transport-level failure.
///
/// `truncated` is the observation budget's report: how many rows the server
/// dropped on this window (aggregation cut plus the requested `nr_samples`).
/// An older server does not send it and defaults to zero.
fn parse_batch(resp: &Value) -> Result<(Batch, u32), String> {
    let samples = resp
        .get(ROWS_KEY)
        .and_then(Value::as_array)
        .ok_or_else(|| format!("response missing {ROWS_KEY:?} rows array"))?;
    let truncated = resp.get("truncated").and_then(Value::as_u64).unwrap_or(0);
    let mut out = Vec::with_capacity(samples.len().min(64));
    for raw in samples {
        match lenient_sample(raw) {
            Some(sample) => out.push(sample),
            None => tracing::trace!("dropping unreadable sched sample"),
        }
    }
    Ok((out, u32::try_from(truncated).unwrap_or(u32::MAX)))
}

// ---------------------------------------------------------------------------
// Tests: a scripted transport for the loop, a real socket for the kernel.
//
// Real-socket dirs live under `CARGO_TARGET_TMPDIR` when available, else under
// the crate's `target/` dir — never `/tmp` (hard policy on this host).
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;
    use std::os::unix::net::UnixListener;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
    use std::sync::mpsc;

    /// Template mirroring a live sample captured on this host (values from a
    /// real `kworker`). Most fields are unknown to [`Sample`] — their presence
    /// proves leniency in every test.
    fn live_sample(pid: i64, comm: &str) -> Value {
        json!({
            "avg_lat_cri": 1789,
            "avg_runtime_wall": 19644,
            "comm": comm,
            "cpu_heat": 19,
            "cpu_id": 12,
            "cpu_util_invr": 3,
            "cpu_util_wall": 3,
            "cpuperf_cur": 48,
            "dom_pinned_util_invr": 0,
            "dom_pinned_util_wall": 0,
            "dsq_consume_lat": 431,
            "dsq_id": 0,
            "lat_cri": 2304,
            "lat_headroom": 1004,
            "mseq": 686,
            "norm_lat_cri": 790,
            "nr_active": 13,
            "perf_cri": 43,
            "pid": pid,
            "prev_cpu_id": 12,
            "rerunnable_interval_wall": 29_777_410_989u64,
            "resched_interval_wall": 3_747_689,
            "run_freq": 2133,
            "runs": 4063,
            "slice_used_wall": 9718,
            "slice_wall": 5_000_000,
            "sleeps": 121,
            "stat": "LIBG",
            "static_prio": 0,
            "steal_util_invr": 0,
            "steal_util_wall": 0,
            "suggested_cpu_id": 12,
            "task_util_est": 7,
            "thr_perf_cri": 46,
            "vuln_thresh": 0,
            "wait_freq": 2876,
            "wake_freq": 3,
            "waker_comm": "",
            "waker_pid": 0,
            "warm_cpu_id": 12
        })
    }

    fn batch_line(samples: Vec<Value>) -> String {
        json!({ "errno": 0, "args": { "resp": { "samples": samples } } }).to_string()
    }

    /// One scripted exchange, shared by the scripted endpoint and the real
    /// socket server.
    #[derive(Clone)]
    enum Reply {
        Line(String),
        /// Answer after blocking this long (a slow server).
        Slow(Duration, String),
        /// Close the connection without answering (EOF mid-request).
        Eof,
        /// Accept and read but never answer (wedged server).
        Hang,
    }

    /// A scripted [`Endpoint`]: one reply script per `open()` (the last one
    /// repeats forever) plus the open/abort counts a test needs to observe.
    /// No socket, and no sleep beyond what a [`Reply::Slow`] asks for.
    struct Script {
        conns: Vec<Vec<Reply>>,
        opened: AtomicU64,
        aborts: AtomicU64,
    }

    impl Script {
        fn new(conns: Vec<Vec<Reply>>) -> Self {
            assert!(!conns.is_empty(), "a script needs at least one connection");
            Self {
                conns,
                opened: AtomicU64::new(0),
                aborts: AtomicU64::new(0),
            }
        }

        fn opened(&self) -> u64 {
            self.opened.load(Ordering::SeqCst)
        }

        fn aborts(&self) -> u64 {
            self.aborts.load(Ordering::SeqCst)
        }
    }

    impl Endpoint for Arc<Script> {
        fn open(&self) -> Result<Box<dyn Link>, Fail> {
            let index = self.opened.fetch_add(1, Ordering::SeqCst) as usize;
            let replies = self
                .conns
                .get(index)
                .or_else(|| self.conns.last())
                .cloned()
                .unwrap_or_default();
            Ok(Box::new(ScriptLink {
                replies,
                cursor: 0,
                script: Arc::clone(self),
            }))
        }
    }

    struct ScriptLink {
        replies: Vec<Reply>,
        cursor: usize,
        script: Arc<Script>,
    }

    impl ScriptLink {
        /// The same envelope handling [`UnixLink`] applies to a real line.
        fn parse(line: &str) -> Result<Value, Fail> {
            let (errno, resp) = parse_envelope(line)?;
            check_errno(errno, &resp)?;
            Ok(resp)
        }
    }

    impl Link for ScriptLink {
        fn round_trip(&mut self, _req: &Value) -> Result<Value, Fail> {
            let reply = self
                .replies
                .get(self.cursor)
                .or_else(|| self.replies.last())
                .cloned()
                .unwrap_or(Reply::Eof);
            self.cursor += 1;
            match reply {
                Reply::Line(line) => Self::parse(&line),
                Reply::Slow(delay, line) => {
                    std::thread::sleep(delay);
                    Self::parse(&line)
                }
                Reply::Eof => Err(Fail::Transport("connection closed (EOF)".to_owned())),
                Reply::Hang => loop {
                    std::thread::park_timeout(Duration::from_millis(20));
                },
            }
        }

        fn abort_handle(&self) -> Option<Arc<dyn Abortable>> {
            Some(Arc::new(ScriptAbort(Arc::clone(&self.script))))
        }
    }

    struct ScriptAbort(Arc<Script>);

    impl Abortable for ScriptAbort {
        fn abort(&self) {
            self.0.aborts.fetch_add(1, Ordering::SeqCst);
        }
    }

    /// A real AF_UNIX server replaying the same [`Reply`] vocabulary: only the
    /// two kernel-facing tests need it.
    struct FakeServer {
        path: PathBuf,
        stop: Arc<AtomicBool>,
        thread: Option<JoinHandle<()>>,
    }

    impl FakeServer {
        /// Replay `script` as a FIFO across all requests/connections; once
        /// exhausted, repeat the last entry forever.
        fn start(dir: &Path, script: Vec<Reply>) -> Self {
            let path = dir.join("stats");
            let listener = UnixListener::bind(&path).expect("bind fake stats socket");
            listener.set_nonblocking(true).expect("nonblocking listener");
            let stop = Arc::new(AtomicBool::new(false));
            let script = Arc::new(script);
            let thread = std::thread::spawn({
                let stop = Arc::clone(&stop);
                move || {
                    let mut served = 0usize;
                    while !stop.load(Ordering::SeqCst) {
                        let (mut conn, _) = match listener.accept() {
                            Ok(c) => c,
                            Err(_) => {
                                std::thread::sleep(Duration::from_millis(5));
                                continue;
                            }
                        };
                        conn.set_read_timeout(Some(Duration::from_millis(50)))
                            .expect("server read timeout");
                        let mut buf: Vec<u8> = Vec::new();
                        let mut chunk = [0u8; 512];
                        loop {
                            if stop.load(Ordering::SeqCst) {
                                return;
                            }
                            match conn.read(&mut chunk) {
                                Ok(0) => break,
                                Ok(n) => buf.extend_from_slice(&chunk[..n]),
                                Err(ref e)
                                    if e.kind() == std::io::ErrorKind::WouldBlock
                                        || e.kind() == std::io::ErrorKind::TimedOut =>
                                {
                                    continue;
                                }
                                Err(_) => break,
                            }
                            if buf.contains(&b'\n') {
                                buf.clear();
                                let reply = script
                                    .get(served)
                                    .or_else(|| script.last())
                                    .cloned()
                                    .unwrap_or(Reply::Eof);
                                served += 1;
                                match reply {
                                    Reply::Line(line) => {
                                        let _ = conn.write_all(line.as_bytes());
                                        let _ = conn.write_all(b"\n");
                                        let _ = conn.flush();
                                    }
                                    Reply::Slow(delay, line) => {
                                        std::thread::sleep(delay);
                                        let _ = conn.write_all(line.as_bytes());
                                        let _ = conn.write_all(b"\n");
                                        let _ = conn.flush();
                                    }
                                    Reply::Eof => break,
                                    Reply::Hang => {
                                        while !stop.load(Ordering::SeqCst) {
                                            std::thread::sleep(Duration::from_millis(5));
                                        }
                                        return;
                                    }
                                }
                            }
                        }
                    }
                }
            });
            Self {
                path,
                stop,
                thread: Some(thread),
            }
        }

        fn socket(&self) -> PathBuf {
            self.path.clone()
        }
    }

    impl Drop for FakeServer {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::SeqCst);
            if let Some(t) = self.thread.take() {
                let _ = t.join();
            }
            let _ = std::fs::remove_file(&self.path);
        }
    }

    struct TestDir(PathBuf);

    impl TestDir {
        fn new(tag: &str) -> Self {
            static SEQ: AtomicU64 = AtomicU64::new(0);
            let base = option_env!("CARGO_TARGET_TMPDIR").map_or_else(
                || {
                    std::env::current_dir()
                        .expect("test cwd")
                        .join("target")
                        .join("sampler-tests")
                },
                PathBuf::from,
            );
            let dir = base.join(format!("{}{}", tag, SEQ.fetch_add(1, Ordering::Relaxed)));
            std::fs::create_dir_all(&dir).expect("create test dir");
            Self(dir)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn fast_tuning() -> Tuning {
        tuning_with(
            Duration::from_millis(30),
            Duration::from_millis(120),
        )
    }

    fn tuning_with(backoff_start: Duration, backoff_cap: Duration) -> Tuning {
        Tuning {
            cadence: Duration::from_millis(25),
            backoff_start,
            backoff_cap,
        }
    }

    fn spawn_scripted(
        script: &Arc<Script>,
        tuning: Tuning,
        tx: Sender<Batch>,
    ) -> SchedSamplerHandle {
        spawn_with(
            Box::new(Arc::clone(script)),
            tuning,
            format!("bound SchedSample via {STATS_TARGET} at script"),
            tx,
        )
    }

    fn recv_batch(rx: &mpsc::Receiver<Batch>, timeout: Duration) -> Batch {
        rx.recv_timeout(timeout).expect("timed out waiting for batch")
    }

    fn recv_timed(rx: &mpsc::Receiver<Batch>, timeout: Duration) -> (Batch, Instant) {
        let batch = recv_batch(rx, timeout);
        (batch, Instant::now())
    }

    fn wait_for_state<F: Fn(&SamplerState) -> bool>(
        h: &SchedSamplerHandle,
        pred: F,
        timeout: Duration,
    ) -> SamplerState {
        let t0 = Instant::now();
        loop {
            let s = locked(&h.shared).state.clone();
            if pred(&s) {
                return s;
            }
            assert!(
                t0.elapsed() < timeout,
                "state never matched within {timeout:?}, last: {s:?}"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn happy_path_batches_delivered() {
        let mut tweaked = live_sample(2210, "mpv");
        tweaked["static_prio"] = json!("120"); // number-as-string leniency
        let script = Arc::new(Script::new(vec![vec![
            Reply::Line(batch_line(vec![
                live_sample(1557, "kworker/u99:1"),
                live_sample(2210, "firefox"),
            ])),
            Reply::Line(batch_line(vec![tweaked])),
            Reply::Line(batch_line(vec![live_sample(4242, "spinner")])),
        ]]));
        let (tx, rx) = mpsc::channel();
        let h = spawn_scripted(&script, fast_tuning(), tx);

        let first = recv_batch(&rx, Duration::from_secs(5));
        assert_eq!(first.len(), 2);
        assert_eq!(first[0].pid, 1557);
        assert_eq!(first[0].comm, "kworker/u99:1");
        assert_eq!(first[0].wait_freq, 2876);
        assert_eq!(first[0].wake_freq, 3);
        assert_eq!(first[0].avg_runtime_wall, 19644);
        assert_eq!(first[0].task_util_est, 7);
        assert_eq!(first[0].cpu_util_wall, 3);
        assert_eq!(first[0].static_prio, 0);
        assert_eq!(wait_for_state(&h, |s| matches!(s, SamplerState::Live), Duration::from_secs(2)), SamplerState::Live);

        let second = recv_batch(&rx, Duration::from_secs(5));
        assert_eq!(second.len(), 1);
        assert_eq!(second[0].static_prio, 120); // coerced from "120"

        let third = recv_batch(&rx, Duration::from_secs(5));
        assert_eq!(third.len(), 1);
        assert_eq!(third[0].pid, 4242);

        // Three requests, three replies: there is no handshake round trip.
        assert_eq!(script.opened(), 1);

        h.shutdown();
        let t0 = Instant::now();
        h.join().unwrap();
        assert!(t0.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn transport_failure_degraded_then_reconnect_live() {
        // The degraded -> live transition is the contract; the cause is a dead
        // link, not a lost sampler race.
        let script = Arc::new(Script::new(vec![
            vec![Reply::Eof],
            vec![Reply::Line(batch_line(vec![live_sample(7, "resumed")]))],
        ]));
        let (tx, rx) = mpsc::channel();
        // Long backoff so the Degraded window is easy to observe.
        let h = spawn_scripted(
            &script,
            tuning_with(Duration::from_millis(400), Duration::from_millis(400)),
            tx,
        );

        let degraded = wait_for_state(
            &h,
            |s| matches!(s, SamplerState::Degraded { reason } if reason == "transport down"),
            Duration::from_secs(3),
        );
        assert_eq!(degraded, SamplerState::degraded(REASON_TRANSPORT));

        let degraded_at = Instant::now();
        wait_for_state(&h, |s| matches!(s, SamplerState::Live), Duration::from_secs(5));
        // The 400 ms backoff carries ±25 % jitter (300..500 ms), and the
        // observation itself can lag: only a loop that ignores the backoff
        // could reconnect this fast.
        assert!(
            degraded_at.elapsed() >= Duration::from_millis(150),
            "reconnected after {:?}, without waiting for the backoff",
            degraded_at.elapsed()
        );
        let batch = recv_batch(&rx, Duration::from_secs(5));
        assert_eq!(batch.len(), 1);
        assert_eq!(batch[0].pid, 7);
        assert_eq!(script.opened(), 2);

        h.shutdown();
        h.join().unwrap();
    }

    #[test]
    fn eof_mid_request_reconnects() {
        let script = Arc::new(Script::new(vec![
            vec![
                Reply::Line(batch_line(vec![live_sample(10, "before-eof")])),
                Reply::Eof,
            ],
            vec![Reply::Line(batch_line(vec![live_sample(11, "after-eof")]))],
        ]));
        let (tx, rx) = mpsc::channel();
        let h = spawn_scripted(&script, fast_tuning(), tx);

        assert_eq!(recv_batch(&rx, Duration::from_secs(5))[0].pid, 10);
        let batch = recv_batch(&rx, Duration::from_secs(5));
        assert_eq!(batch.len(), 1);
        assert_eq!(batch[0].pid, 11);
        wait_for_state(&h, |s| matches!(s, SamplerState::Live), Duration::from_secs(2));
        assert_eq!(script.opened(), 2);

        h.shutdown();
        h.join().unwrap();
    }

    #[test]
    fn malformed_json_line_tolerated_and_reconnects() {
        let script = Arc::new(Script::new(vec![
            vec![
                Reply::Line(batch_line(vec![live_sample(12, "pre")])),
                Reply::Line("{\"errno\":0,\"args\":{\"resp\":{\"samples\":[".to_owned()),
            ],
            vec![Reply::Line(batch_line(vec![live_sample(13, "after-malformed")]))],
        ]));
        let (tx, rx) = mpsc::channel();
        let h = spawn_scripted(&script, fast_tuning(), tx);

        assert_eq!(recv_batch(&rx, Duration::from_secs(5))[0].pid, 12);
        let batch = recv_batch(&rx, Duration::from_secs(5));
        assert_eq!(batch.len(), 1);
        assert_eq!(batch[0].pid, 13);
        wait_for_state(&h, |s| matches!(s, SamplerState::Live), Duration::from_secs(2));
        assert_eq!(script.opened(), 2);

        h.shutdown();
        h.join().unwrap();
    }

    #[test]
    fn empty_batch_delivered() {
        let script = Arc::new(Script::new(vec![vec![
            Reply::Line(batch_line(vec![])),
            Reply::Line(batch_line(vec![])),
        ]]));
        let (tx, rx) = mpsc::channel();
        let h = spawn_scripted(&script, fast_tuning(), tx);

        assert!(recv_batch(&rx, Duration::from_secs(5)).is_empty());
        assert!(recv_batch(&rx, Duration::from_secs(5)).is_empty());

        h.shutdown();
        h.join().unwrap();
    }

    #[test]
    fn batch_at_ringbuf_cap() {
        let samples: Vec<Value> = (0..60i64).map(|i| live_sample(1000 + i, "spinner")).collect();
        let script = Arc::new(Script::new(vec![vec![Reply::Line(batch_line(samples))]]));
        let (tx, rx) = mpsc::channel();
        let h = spawn_scripted(&script, fast_tuning(), tx);

        let batch = recv_batch(&rx, Duration::from_secs(5));
        assert_eq!(batch.len(), 60); // the ~60-sample ringbuf cap shape
        assert_eq!(batch[0].pid, 1000);
        assert_eq!(batch[59].pid, 1059);

        h.shutdown();
        h.join().unwrap();
    }

    /// Every field of the contract must survive the lenient parse. This
    /// whitelist is a SECOND projection of the schema, so a field added to
    /// `Sample` without a line here is silently dropped — which is exactly how
    /// the kernel-thread flag shipped dead (83 kthread entries created right
    /// after deploy). Round-tripping a fully populated row pins all of them.
    #[test]
    fn every_contract_field_survives_the_lenient_parse() {
        let full = aranicy_wire::Sample {
            pid: 4242,
            comm: "kworker/3".into(),
            static_prio: 23,
            wait_freq: 17,
            wake_freq: 19,
            avg_runtime_wall: 424_242,
            task_util_est: 900,
            cpu_util_wall: 800,
            runs: 21,
            sleeps: 22,
            kthread: 1,
        };
        let json = serde_json::to_value(&full).expect("serialize");
        let back = lenient_sample(&json).expect("parse");
        assert_eq!(back, full, "a contract field was dropped by the whitelist");
        assert!(back.is_kernel_thread(), "the flag decides a policy: keep it");
    }

    #[test]
    fn lenient_samples_string_numbers_unknowns_defaults() {
        let mut a = live_sample(7, "a");
        a["task_util_est"] = json!(" 999 "); // trimmed string number
        a["static_prio"] = json!("120");
        let garbage = json!(["not", "a", "sample"]);
        let c = json!({ "pid": 42, "comm": 7, "wait_freq": "oops" });
        let script = Arc::new(Script::new(vec![vec![Reply::Line(batch_line(vec![
            a, garbage, c,
        ]))]]));
        let (tx, rx) = mpsc::channel();
        let h = spawn_scripted(&script, fast_tuning(), tx);

        let batch = recv_batch(&rx, Duration::from_secs(5));
        assert_eq!(batch.len(), 2); // non-object entry skipped
        assert_eq!(batch[0].task_util_est, 999);
        assert_eq!(batch[0].static_prio, 120);
        assert_eq!(batch[0].cpu_util_wall, 3); // unknown sibling fields ignored
        assert_eq!(batch[0].runs, 4063); // per-window counts parse too
        assert_eq!(batch[0].sleeps, 121);
        assert_eq!(batch[1].pid, 42);
        assert_eq!(batch[1].comm, ""); // wrong-typed -> serde(default)
        assert_eq!(batch[1].wait_freq, 0); // unparseable string -> serde(default)
        // An old collector sends no per-window counts: 0 = "no evidence" on
        // the lean axis only (the score axis has its own gates).
        assert_eq!(batch[1].runs, 0);
        assert_eq!(batch[1].sleeps, 0);

        h.shutdown();
        h.join().unwrap();
    }

    #[test]
    fn truncated_is_read_from_the_envelope() {
        let with = json!({
            "samples": [live_sample(1, "a")],
            "truncated": 3,
        });
        let (batch, truncated) = parse_batch(&with).expect("parses");
        assert_eq!(batch.len(), 1);
        assert_eq!(truncated, 3);

        let without = json!({ "samples": [] });
        let (batch, truncated) = parse_batch(&without).expect("parses");
        assert!(batch.is_empty());
        assert_eq!(truncated, 0, "an older server sends no truncation count");
    }

    #[test]
    fn failed_round_trip_clears_the_abort_slot() {
        // A dead link must not stay parked: `shutdown()` would abort a stale
        // handle while the thread is already reconnecting.
        let script = Arc::new(Script::new(vec![vec![Reply::Eof]]));
        let (tx, rx) = mpsc::channel();
        let h = spawn_scripted(
            &script,
            tuning_with(Duration::from_millis(400), Duration::from_millis(400)),
            tx,
        );
        wait_for_state(
            &h,
            |s| matches!(s, SamplerState::Degraded { reason } if reason == "transport down"),
            Duration::from_secs(5),
        );
        assert!(
            locked(&h.slot).is_none(),
            "a dead link must not stay parked in the abort slot"
        );
        drop(rx);
        h.shutdown();
        h.join().unwrap();
    }

    #[test]
    fn backoff_doubles_to_cap() {
        // The schedule itself, without waiting for it: 1, 2, 4, 8, 16, 30, 30
        // seconds with ±25 % jitter, and a reset that really returns to the
        // floor. (That the loop honours these delays is the reconnect tests'
        // job — they would never reach `Live` otherwise.)
        let start = Duration::from_millis(100);
        let cap = Duration::from_millis(3000);
        let mut backoff = Backoff::new(start, cap);
        for base_ms in [100u64, 200, 400, 800, 1600, 3000, 3000] {
            let delay = backoff.delay();
            let lo = Duration::from_millis(base_ms * 3 / 4);
            let hi = Duration::from_millis(base_ms * 5 / 4);
            assert!(
                delay >= lo && delay <= hi,
                "delay {delay:?} outside [{lo:?}, {hi:?}] for a {base_ms} ms step"
            );
        }
        backoff.reset();
        let delay = backoff.delay();
        assert!(
            delay >= Duration::from_millis(75) && delay <= Duration::from_millis(125),
            "reset must return to the {start:?} floor, got {delay:?}"
        );
    }

    #[test]
    fn pacing_skips_missed_slots() {
        // A round trip that overruns several 40 ms slots must resume at the
        // next slot, never replay the missed ones back to back.
        const CADENCE: Duration = Duration::from_millis(40);
        let script = Arc::new(Script::new(vec![vec![
            Reply::Slow(
                Duration::from_millis(130),
                batch_line(vec![live_sample(1, "slow")]),
            ),
            Reply::Line(batch_line(vec![live_sample(2, "next")])),
        ]]));
        let (tx, rx) = mpsc::channel();
        let h = spawn_with(
            Box::new(Arc::clone(&script)),
            Tuning {
                cadence: CADENCE,
                backoff_start: Duration::from_millis(30),
                backoff_cap: Duration::from_millis(120),
            },
            "scripted".to_owned(),
            tx,
        );

        let (first, t1) = recv_timed(&rx, Duration::from_secs(5));
        assert_eq!(first[0].pid, 1);
        let (second, t2) = recv_timed(&rx, Duration::from_secs(5));
        assert_eq!(second[0].pid, 2);
        // Same slot as the slow round trip ended in -> ~20 ms later; a
        // catch-up burst would deliver both back to back.
        let gap = t2 - t1;
        assert!(
            gap >= Duration::from_millis(15),
            "catch-up burst: second batch arrived {gap:?} after the first (cadence {CADENCE:?})"
        );

        h.shutdown();
        h.join().unwrap();
    }

    #[test]
    fn consumer_drop_exits_promptly() {
        let script = Arc::new(Script::new(vec![vec![Reply::Line(batch_line(vec![
            live_sample(1, "a"),
        ]))]]));
        let (tx, rx) = mpsc::channel();
        let h = spawn_scripted(&script, fast_tuning(), tx);
        // Prove the loop is running before pulling the consumer away.
        let _ = recv_batch(&rx, Duration::from_secs(5));

        let t0 = Instant::now();
        drop(rx);
        h.join().unwrap();
        assert!(
            t0.elapsed() < Duration::from_secs(2),
            "thread outlived its consumer by {:?}",
            t0.elapsed()
        );
        assert_eq!(script.opened(), 1, "a dead consumer must not reconnect");
        assert_eq!(script.aborts(), 1, "the live link must be aborted on the way out");
    }

    // -----------------------------------------------------------------------
    // Real socket: framing and `shutdown(2)` are kernel/libc behaviour.
    // -----------------------------------------------------------------------

    #[test]
    fn unix_socket_round_trip() {
        let dir = TestDir::new("unix-happy");
        let mut tweaked = live_sample(2210, "mpv");
        tweaked["static_prio"] = json!("120");
        let server = FakeServer::start(
            dir.path(),
            vec![
                Reply::Line(batch_line(vec![live_sample(1557, "kworker/u99:1")])),
                Reply::Line(batch_line(vec![tweaked])),
            ],
        );
        let (tx, rx) = mpsc::channel();
        let h = spawn_collector(server.socket(), Duration::from_millis(25), tx);

        let first = recv_batch(&rx, Duration::from_secs(5));
        assert_eq!(first[0].pid, 1557);
        assert_eq!(first[0].comm, "kworker/u99:1");
        let second = recv_batch(&rx, Duration::from_secs(5));
        assert_eq!(second[0].static_prio, 120, "number-as-string survives framing");
        wait_for_state(&h, |s| matches!(s, SamplerState::Live), Duration::from_secs(2));

        h.shutdown();
        h.join().unwrap();
    }

    #[test]
    fn shutdown_unblocks_unresponsive_server() {
        let dir = TestDir::new("hang");
        let server = FakeServer::start(dir.path(), vec![Reply::Hang]);
        let (tx, rx) = mpsc::channel();
        let h = spawn_collector(server.socket(), DEFAULT_CADENCE, tx);
        // Let the thread block on the never-answered stats read.
        std::thread::sleep(Duration::from_millis(150));
        let t0 = Instant::now();
        h.shutdown();
        h.join().unwrap();
        assert!(
            t0.elapsed() < Duration::from_secs(2),
            "join took {:?}",
            t0.elapsed()
        );
        drop(rx);
    }
}

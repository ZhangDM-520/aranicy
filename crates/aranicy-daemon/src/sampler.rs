//! lavd `sched_samples` client (plan.md §4.1).
//!
//! Interface frozen between workstreams: [`SchedSample`] / [`Batch`].
//!
//! Client side of scx_lavd's stats protocol: one JSON line in, one JSON line
//! out over a root-only UNIX socket (verified live against scx_lavd 1.1.4):
//!
//! * `{"req":"stats","args":{"target":"sched_samples","nr_samples":"200"}}`
//!   — `nr_samples` is sent as a decimal *string*; the response envelope is
//!   `{"errno":N,"args":{"resp":{"samples":[...]}}}` (on `errno != 0` the
//!   `resp` carries a message string).
//! * `{"req":"stats_meta"}` reports field availability and is re-run on every
//!   (re)connect (feature detection). On 1.1.4 the payload is
//!   `{"SchedSample":{"fields":{name:{datum,desc},..}}}`; other builds may
//!   send an array of names — both shapes are accepted.
//! * Each request arms a machine-wide budget of `nr_samples` dispatch events,
//!   then drains a 16 KB ringbuf: at most ~60 samples per batch regardless of
//!   `nr_samples`, silently lossy (`mseq` contiguous), hot-task biased.
//!   Sequential round-trips only — pipelining serializes server-side.
//! * Only one client may sample `sched_samples` at a time; a competing client
//!   answers `errno != 0` with a message containing "preempted by another
//!   sampler" (substring match, weak contract). Reconnecting reclaims
//!   ownership; an idle connection steals it on its first request.
//!
//! [`spawn`] runs the single sampler thread: it delivers [`Batch`]es through
//! an `mpsc::Sender`, logs exactly one line per [`SamplerState`] transition,
//! and reconnects with exponential backoff after any transport failure.

// The daemon wiring (owned elsewhere) consumes this API; until that lands the
// items are intentionally unreachable from `main`.
#![allow(dead_code)]

use std::collections::BTreeSet;
use std::fmt;
use std::io::{BufRead, BufReader, Write};
use std::net::Shutdown;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};

/// One per-task sample from scx_lavd's `sched_samples` stats target.
/// Parsing is lenient: unknown fields ignored, missing fields defaulted —
/// the lavd schema drifts across versions (`stats_meta` detects availability).
#[derive(Debug, Clone, Default, serde::Deserialize)]
#[serde(default)]
pub struct SchedSample {
    pub pid: i64,
    pub comm: String,
    /// 4-char flag string, e.g. "LHBG": [0]='L'|'R' latency-critical|regular,
    /// [1] H/I, [2] B/T, [3]='G' over fair-share.
    pub stat: String,
    pub lat_cri: u64,
    pub avg_lat_cri: u64,
    pub norm_lat_cri: u64,
    /// EWMA of sleep-interval inverse (weight-independent).
    pub wait_freq: u64,
    /// EWMA of wake rate (weight-independent).
    pub wake_freq: u64,
    pub run_freq: u64,
    /// EWMA per-schedule runtime, nanoseconds (weight-independent).
    pub avg_runtime_wall: u64,
    /// Demand estimate.
    pub task_util_est: u64,
    /// Delivered CPU, same scale as task_util_est.
    pub cpu_util_wall: u64,
    /// nice + 20 (includes our own applied bias — closed-loop check).
    pub static_prio: u64,
}

/// One batch: at most ~60 samples (16 KB ringbuf cap), lossy, hot-task biased.
pub type Batch = Vec<SchedSample>;

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Default wall-clock pacing between `sched_samples` requests.
pub const DEFAULT_CADENCE: Duration = Duration::from_millis(500);

/// Sample budget requested per batch. Sent as a decimal *string* on the wire
/// (the server parses an integer-from-string); the 16 KB ringbuf caps actual
/// delivery at ~60 samples per batch regardless.
pub const NR_SAMPLES: u32 = 200;

const REASON_STARTING: &str = "starting";
const REASON_TRANSPORT: &str = "transport down";
/// Canonical preemption reason; also the substring matched in server messages.
const REASON_PREEMPTED: &str = "preempted by another sampler";

/// Slow reconnect cadence after losing the sampler race
/// (±25% jitter → 5.25–8.75 s).
const PREEMPT_RETRY: Duration = Duration::from_secs(7);
/// Exponential reconnect backoff: 1 s doubling to a 30 s cap, ±25% jitter.
const BACKOFF_START: Duration = Duration::from_secs(1);
const BACKOFF_CAP: Duration = Duration::from_secs(30);
/// Wedged-server guard for response reads. `shutdown()` still unblocks the
/// thread immediately — it calls `shutdown(2)` on the live socket.
const READ_TIMEOUT: Duration = Duration::from_secs(15);

/// The two well-known spellings of the stats socket (same inode on this host;
/// both are tried in order — a stale socket after a lavd crash answers
/// `ECONNREFUSED`, which counts as "absent").
pub fn default_socket_paths() -> [PathBuf; 2] {
    [
        PathBuf::from("/run/scx/root/stats"),
        PathBuf::from("/var/run/scx/root/stats"),
    ]
}

/// Sampler state machine, exposed for the daemon/log.
///
/// `Degraded` covers both "transport down" and "preempted by another
/// sampler": the daemon keeps applying the already-learned bias and stops
/// updating it until the sampler is [`SamplerState::Live`] again. The
/// `reason` strings are stable: `"starting"` (before first connect),
/// `"transport down"`, `"preempted by another sampler"`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SamplerState {
    /// Connected, `stats_meta` ran, batches flowing.
    Live,
    /// Not delivering batches; see the enum docs for `reason` values.
    Degraded { reason: String },
}

impl SamplerState {
    fn degraded(reason: &str) -> Self {
        Self::Degraded {
            reason: reason.to_owned(),
        }
    }

    pub fn is_live(&self) -> bool {
        matches!(self, Self::Live)
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

/// Field-availability snapshot of `SchedSample`, taken from `stats_meta` on
/// every (re)connect (feature detection across lavd versions).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FieldSet {
    names: BTreeSet<String>,
}

impl FieldSet {
    pub fn has(&self, field: &str) -> bool {
        self.names.contains(field)
    }

    pub fn len(&self) -> usize {
        self.names.len()
    }

    pub fn is_empty(&self) -> bool {
        self.names.is_empty()
    }

    pub fn sched_sample_fields(&self) -> impl Iterator<Item = &str> {
        self.names.iter().map(String::as_str)
    }

    fn insert(&mut self, name: String) {
        self.names.insert(name);
    }
}

/// Handle to the sampler thread (the daemon spawns exactly one).
pub struct SchedSamplerHandle {
    shared: Arc<Mutex<Shared>>,
    stop: Arc<StopFlag>,
    slot: StreamSlot,
    thread: Option<JoinHandle<()>>,
}

impl SchedSamplerHandle {
    /// Current state (cheap snapshot, for the daemon/log).
    pub fn state(&self) -> SamplerState {
        locked(&self.shared).state.clone()
    }

    /// Latest `stats_meta` field snapshot (empty until the first connect).
    pub fn fields(&self) -> FieldSet {
        locked(&self.shared).fields.clone()
    }

    /// Request shutdown and unblock the thread promptly: the inter-batch wait
    /// is a condvar wait, and a blocked socket read is woken by `shutdown(2)`
    /// on the shared stream. Idempotent.
    pub fn shutdown(&self) {
        self.stop.request();
        if let Some(stream) = locked(&self.slot).clone() {
            let _ = stream.shutdown(Shutdown::Both);
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

/// Spawn the single sampler thread.
///
/// It tries `socket_paths` in order on every (re)connect (`ENOENT` /
/// `ECONNREFUSED` count as "absent"), runs `stats_meta` for feature detection,
/// then requests `sched_samples` every `cadence` (wall-clock paced: missed
/// slots are skipped, never caught up in bursts) and delivers each parsed
/// [`Batch`] to `tx`. A dropped receiver stops the thread. Exactly one
/// tracing line is emitted per [`SamplerState`] transition; batches are never
/// logged. A zero `cadence` falls back to [`DEFAULT_CADENCE`].
pub fn spawn(socket_paths: &[PathBuf], cadence: Duration, tx: Sender<Batch>) -> SchedSamplerHandle {
    let tuning = Tuning {
        cadence: if cadence.is_zero() {
            DEFAULT_CADENCE
        } else {
            cadence
        },
        backoff_start: BACKOFF_START,
        backoff_cap: BACKOFF_CAP,
        preempt_retry: PREEMPT_RETRY,
    };
    spawn_with(socket_paths, tuning, tx)
}

fn spawn_with(socket_paths: &[PathBuf], tuning: Tuning, tx: Sender<Batch>) -> SchedSamplerHandle {
    let shared = Arc::new(Mutex::new(Shared {
        state: SamplerState::degraded(REASON_STARTING),
        fields: FieldSet::default(),
    }));
    let stop = Arc::new(StopFlag::new());
    let slot: StreamSlot = Arc::new(Mutex::new(None));
    let thread = std::thread::Builder::new()
        .name("lavd-stats-sampler".to_owned())
        .spawn({
            let paths = socket_paths.to_vec();
            let shared = Arc::clone(&shared);
            let stop = Arc::clone(&stop);
            let slot = Arc::clone(&slot);
            move || run(paths, tuning, tx, shared, stop, slot)
        })
        .expect("spawn lavd stats sampler thread");
    SchedSamplerHandle {
        shared,
        stop,
        slot,
        thread: Some(thread),
    }
}

// ---------------------------------------------------------------------------
// Internals
// ---------------------------------------------------------------------------

/// Where the live stream is parked so `shutdown()` can wake blocked reads.
type StreamSlot = Arc<Mutex<Option<Arc<UnixStream>>>>;

struct Tuning {
    cadence: Duration,
    backoff_start: Duration,
    backoff_cap: Duration,
    preempt_retry: Duration,
}

struct Shared {
    state: SamplerState,
    fields: FieldSet,
}

fn locked<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The one-and-only log site: a single line per state transition, never
/// per-batch.
fn transition(shared: &Mutex<Shared>, new: SamplerState, detail: &str) {
    let mut s = locked(shared);
    if s.state == new {
        return;
    }
    match &new {
        SamplerState::Live => tracing::info!(prev = %s.state, "lavd sampler live"),
        SamplerState::Degraded { reason } => {
            tracing::warn!(prev = %s.state, reason = %reason, detail = %detail, "lavd sampler degraded")
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

/// Failure classification: preemption takes the slow-retry path, everything
/// else (I/O, EOF, parse, other `errno`) takes the exponential backoff path.
enum Fail {
    Preempted(String),
    Transport(String),
}

struct Connection {
    stream: Arc<UnixStream>,
    reader: BufReader<UnixStream>,
}

impl Connection {
    fn open(path: &Path) -> std::io::Result<Self> {
        let stream = UnixStream::connect(path)?;
        stream.set_read_timeout(Some(READ_TIMEOUT))?;
        // A second fd for line reads; `shutdown(2)` on `stream` (same socket
        // object) still wakes them.
        let reader = BufReader::new(stream.try_clone()?);
        Ok(Self {
            stream: Arc::new(stream),
            reader,
        })
    }

    fn write_line(&self, payload: &Value) -> Result<(), Fail> {
        let line = serde_json::to_string(payload)
            .map_err(|e| Fail::Transport(format!("serialize request: {e}")))?;
        let mut w = &*self.stream;
        w.write_all(line.as_bytes())
            .and_then(|_| w.write_all(b"\n"))
            .and_then(|_| w.flush())
            .map_err(|e| Fail::Transport(format!("write: {e}")))
    }

    fn read_line(&mut self) -> Result<String, Fail> {
        let mut buf = String::new();
        match self.reader.read_line(&mut buf) {
            Ok(0) => Err(Fail::Transport("connection closed (EOF)".to_owned())),
            Ok(_) => Ok(buf),
            Err(e) => Err(Fail::Transport(format!("read: {e}"))),
        }
    }

    /// One sequential round-trip: request line out, response line in, unwrap
    /// the envelope, check `errno`.
    fn round_trip(&mut self, req: &Value) -> Result<Value, Fail> {
        self.write_line(req)?;
        let line = self.read_line()?;
        let (errno, resp) = parse_envelope(&line)?;
        check_errno(errno, &resp)?;
        Ok(resp)
    }
}

fn open_connection(paths: &[PathBuf], slot: &StreamSlot) -> Result<Connection, String> {
    let mut errs = Vec::new();
    for path in paths {
        match Connection::open(path) {
            Ok(conn) => {
                *locked(slot) = Some(Arc::clone(&conn.stream));
                return Ok(conn);
            }
            Err(e) => errs.push(format!("{}: {e}", path.display())),
        }
    }
    if errs.is_empty() {
        Err("no socket paths configured".to_owned())
    } else {
        Err(errs.join("; "))
    }
}

fn run(
    paths: Vec<PathBuf>,
    tuning: Tuning,
    tx: Sender<Batch>,
    shared: Arc<Mutex<Shared>>,
    stop: Arc<StopFlag>,
    slot: StreamSlot,
) {
    let mut backoff = Backoff::new(tuning.backoff_start, tuning.backoff_cap);
    let mut rng = seed_rng();
    'outer: loop {
        if stop.requested() {
            break;
        }
        let mut conn = match open_connection(&paths, &slot) {
            Ok(conn) => conn,
            Err(e) => {
                if stop.requested() {
                    break;
                }
                transition(
                    &shared,
                    SamplerState::degraded(REASON_TRANSPORT),
                    &format!("connect failed: {e}"),
                );
                if !stop.wait(backoff.delay()) {
                    break;
                }
                continue 'outer;
            }
        };
        if stop.requested() {
            break;
        }
        // Feature detection on every (re)connect.
        match conn.round_trip(&json!({ "req": "stats_meta" })) {
            Ok(meta) => locked(&shared).fields = parse_meta(&meta),
            Err(fail) => {
                if stop.requested() {
                    break;
                }
                let (reason, detail, wait) = match fail {
                    Fail::Preempted(msg) => (
                        REASON_PREEMPTED,
                        msg,
                        jitter(tuning.preempt_retry, &mut rng),
                    ),
                    Fail::Transport(msg) => (REASON_TRANSPORT, msg, backoff.delay()),
                };
                transition(&shared, SamplerState::degraded(reason), &detail);
                drop(conn);
                if !stop.wait(wait) {
                    break;
                }
                continue 'outer;
            }
        }
        backoff.reset();
        transition(&shared, SamplerState::Live, "");
        let mut next = Instant::now();
        loop {
            if !stop.wait_until(next) {
                break 'outer;
            }
            let req = json!({
                "req": "stats",
                "args": { "target": "sched_samples", "nr_samples": NR_SAMPLES.to_string() }
            });
            let batch = match conn.round_trip(&req) {
                Ok(resp) => match parse_batch(&resp) {
                    Ok(batch) => batch,
                    Err(detail) => {
                        // Malformed payload: close and resync via reconnect.
                        if stop.requested() {
                            break 'outer;
                        }
                        transition(&shared, SamplerState::degraded(REASON_TRANSPORT), &detail);
                        drop(conn);
                        if !stop.wait(backoff.delay()) {
                            break 'outer;
                        }
                        continue 'outer;
                    }
                },
                Err(fail) => {
                    if stop.requested() {
                        break 'outer;
                    }
                    let (reason, detail, wait) = match fail {
                        Fail::Preempted(msg) => (
                            REASON_PREEMPTED,
                            msg,
                            jitter(tuning.preempt_retry, &mut rng),
                        ),
                        Fail::Transport(msg) => (REASON_TRANSPORT, msg, backoff.delay()),
                    };
                    transition(&shared, SamplerState::degraded(reason), &detail);
                    drop(conn);
                    if !stop.wait(wait) {
                        break 'outer;
                    }
                    continue 'outer;
                }
            };
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
    *locked(&slot) = None;
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

fn check_errno(errno: i64, resp: &Value) -> Result<(), Fail> {
    if errno == 0 {
        return Ok(());
    }
    let msg = resp_message(resp);
    if msg.contains(REASON_PREEMPTED) {
        Err(Fail::Preempted(msg))
    } else {
        Err(Fail::Transport(format!("errno {errno}: {msg}")))
    }
}

fn resp_message(resp: &Value) -> String {
    match resp {
        Value::String(s) => s.clone(),
        Value::Object(map) => map
            .get("message")
            .or_else(|| map.get("msg"))
            .or_else(|| map.get("error"))
            .and_then(Value::as_str)
            .map(str::to_owned)
            .unwrap_or_else(|| resp.to_string()),
        other => other.to_string(),
    }
}

/// `SchedSample` field names from a `stats_meta` payload. Accepts the live
/// 1.1.4 shape (`fields` = name → {datum, desc} map) and the documented array
/// shape (names, or objects carrying a `name`).
fn parse_meta(resp: &Value) -> FieldSet {
    let mut set = FieldSet::default();
    let fields = resp
        .pointer("/SchedSample/fields")
        .or_else(|| resp.pointer("/args/resp/SchedSample/fields"));
    match fields {
        Some(Value::Object(map)) => {
            for name in map.keys() {
                set.insert(name.clone());
            }
        }
        Some(Value::Array(items)) => {
            for item in items {
                match item {
                    Value::String(name) => set.insert(name.clone()),
                    Value::Object(map) => {
                        if let Some(name) = map.get("name").and_then(Value::as_str) {
                            set.insert(name.to_owned());
                        }
                    }
                    _ => {}
                }
            }
        }
        _ => {}
    }
    set
}

/// Numeric fields of [`SchedSample`] (`pid` signed, the rest unsigned); these
/// are the fields accepted as int-or-string.
const UNSIGNED_FIELDS: &[&str] = &[
    "lat_cri",
    "avg_lat_cri",
    "norm_lat_cri",
    "wait_freq",
    "wake_freq",
    "run_freq",
    "avg_runtime_wall",
    "task_util_est",
    "cpu_util_wall",
    "static_prio",
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

/// Lenient [`SchedSample`] parse: unknown fields ignored (the lavd schema has
/// 38 fields and drifts), numbers accepted as ints or strings, wrong-typed or
/// missing fields defaulted via `serde(default)`.
fn lenient_sample(raw: &Value) -> Option<SchedSample> {
    let raw = raw.as_object()?;
    let mut norm = serde_json::Map::with_capacity(raw.len());
    for (key, val) in raw {
        let coerced = match key.as_str() {
            "pid" => coerce_number(val, true),
            "comm" | "stat" => val.as_str().map(|_| val.clone()),
            name if UNSIGNED_FIELDS.contains(&name) => coerce_number(val, false),
            _ => None, // unknown field: serde ignores it anyway
        };
        if let Some(v) = coerced {
            norm.insert(key.clone(), v);
        }
    }
    serde_json::from_value(Value::Object(norm)).ok()
}

/// `resp` → [`Batch`]. Unreadable entries are dropped individually (leniency);
/// a payload without a `samples` array is a transport-level failure.
fn parse_batch(resp: &Value) -> Result<Batch, String> {
    let samples = resp
        .get("samples")
        .and_then(Value::as_array)
        .ok_or_else(|| "response missing \"samples\" array".to_owned())?;
    let mut out = Vec::with_capacity(samples.len().min(64));
    for raw in samples {
        match lenient_sample(raw) {
            Some(sample) => out.push(sample),
            None => tracing::trace!("dropping unreadable sched sample"),
        }
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// Tests: fake stats server replaying live-shaped canned payloads.
//
// Socket dirs live under `CARGO_TARGET_TMPDIR` when available, else under the
// crate's `target/` dir — never `/tmp` (hard policy on this host).
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;
    use std::os::unix::net::UnixListener;
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
    use std::sync::mpsc;

    /// Real scx_lavd 1.1.4 `SchedSample` schema captured live on this host:
    /// 38 fields, name → datum type (as reported by `stats_meta`).
    const LIVE_FIELDS: &[(&str, &str)] = &[
        ("avg_lat_cri", "u64"),
        ("avg_runtime_wall", "u64"),
        ("comm", "string"),
        ("cpu_heat", "u64"),
        ("cpu_id", "u64"),
        ("cpu_util_invr", "u64"),
        ("cpu_util_wall", "u64"),
        ("cpuperf_cur", "u64"),
        ("dom_pinned_util_invr", "u64"),
        ("dom_pinned_util_wall", "u64"),
        ("dsq_consume_lat", "u64"),
        ("dsq_id", "u64"),
        ("lat_cri", "u64"),
        ("lat_headroom", "u64"),
        ("mseq", "u64"),
        ("norm_lat_cri", "u64"),
        ("nr_active", "u64"),
        ("perf_cri", "u64"),
        ("pid", "i64"),
        ("prev_cpu_id", "u64"),
        ("rerunnable_interval_wall", "u64"),
        ("resched_interval_wall", "u64"),
        ("run_freq", "u64"),
        ("slice_used_wall", "u64"),
        ("slice_wall", "u64"),
        ("stat", "string"),
        ("static_prio", "u64"),
        ("steal_util_invr", "u64"),
        ("steal_util_wall", "u64"),
        ("suggested_cpu_id", "u64"),
        ("task_util_est", "u64"),
        ("thr_perf_cri", "u64"),
        ("vuln_thresh", "u64"),
        ("wait_freq", "u64"),
        ("wake_freq", "u64"),
        ("waker_comm", "string"),
        ("waker_pid", "i64"),
        ("warm_cpu_id", "u64"),
    ];

    /// Template mirrors a live sample captured on this host (values from a
    /// real `kworker` sample). `cpu_heat`/`warm_cpu_id`/… are unknown to
    /// `SchedSample` — their presence proves leniency in every test.
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
            "slice_used_wall": 9718,
            "slice_wall": 5_000_000,
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

    /// Live 1.1.4 `stats_meta` shape: `fields` is a name → {datum, desc} map.
    fn meta_map_line() -> String {
        let fields: serde_json::Map<String, Value> = LIVE_FIELDS
            .iter()
            .map(|(name, datum)| {
                ((*name).to_owned(), json!({ "datum": datum, "desc": format!("{name} field") }))
            })
            .collect();
        json!({
            "errno": 0,
            "args": { "resp": {
                "SchedSample": { "fields": fields },
                "SysStats": { "fields": { "nr_running": { "datum": "u64", "desc": "Runnable tasks" } } }
            }}
        })
        .to_string()
    }

    /// The brief's documented shape: `fields` is an array of names.
    fn meta_array_line() -> String {
        let names: Vec<&str> = LIVE_FIELDS.iter().map(|(n, _)| *n).collect();
        json!({
            "errno": 0,
            "args": { "resp": { "SchedSample": { "fields": names } } }
        })
        .to_string()
    }

    fn preempt_line() -> String {
        json!({
            "errno": 22,
            "args": { "resp": "sched_samples: preempted by another sampler" }
        })
        .to_string()
    }

    #[derive(Clone)]
    enum Reply {
        Line(String),
        /// Close the connection without answering (EOF mid-request).
        Eof,
        /// Accept and read but never answer (wedged server).
        Hang,
    }

    struct FakeServer {
        path: PathBuf,
        conns: Arc<AtomicU64>,
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
            let conns = Arc::new(AtomicU64::new(0));
            let thread = std::thread::spawn({
                let stop = Arc::clone(&stop);
                let conns = Arc::clone(&conns);
                move || {
                    let mut script = script;
                    if script.is_empty() {
                        script.push(Reply::Eof);
                    }
                    let mut served = 0usize;
                    while !stop.load(Ordering::SeqCst) {
                        let (mut conn, _) = match listener.accept() {
                            Ok(c) => c,
                            Err(_) => {
                                std::thread::sleep(Duration::from_millis(5));
                                continue;
                            }
                        };
                        conns.fetch_add(1, Ordering::SeqCst);
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
                conns,
                stop,
                thread: Some(thread),
            }
        }

        fn socket(&self) -> PathBuf {
            self.path.clone()
        }

        fn connections(&self) -> u64 {
            self.conns.load(Ordering::SeqCst)
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

    fn fast_tuning(preempt_retry: Duration) -> Tuning {
        Tuning {
            cadence: Duration::from_millis(25),
            backoff_start: Duration::from_millis(30),
            backoff_cap: Duration::from_millis(120),
            preempt_retry,
        }
    }

    fn recv_batch(rx: &mpsc::Receiver<Batch>, timeout: Duration) -> Batch {
        rx.recv_timeout(timeout).expect("timed out waiting for batch")
    }

    fn wait_for_state<F: Fn(&SamplerState) -> bool>(
        h: &SchedSamplerHandle,
        pred: F,
        timeout: Duration,
    ) -> SamplerState {
        let t0 = Instant::now();
        loop {
            let s = h.state();
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
    fn live_shaped_payloads_have_38_fields() {
        let sample = live_sample(1, "x");
        let obj = sample.as_object().unwrap();
        assert_eq!(obj.len(), 38);
        let keys: BTreeSet<&str> = obj.keys().map(String::as_str).collect();
        let meta: BTreeSet<&str> = LIVE_FIELDS.iter().map(|(n, _)| *n).collect();
        assert_eq!(keys, meta);
    }

    #[test]
    fn happy_path_batches_delivered() {
        let dir = TestDir::new("happy");
        let b1 = batch_line(vec![
            live_sample(1557, "kworker/u99:1"),
            live_sample(2210, "firefox"),
        ]);
        let mut tweaked = live_sample(2210, "mpv");
        tweaked["static_prio"] = json!("120"); // number-as-string leniency
        let b2 = batch_line(vec![tweaked]);
        let b3 = batch_line(vec![live_sample(4242, "spinner")]);
        let server = FakeServer::start(
            dir.path(),
            vec![
                Reply::Line(meta_map_line()),
                Reply::Line(b1),
                Reply::Line(b2),
                Reply::Line(b3),
            ],
        );
        let (tx, rx) = mpsc::channel();
        let h = spawn(&[server.socket()], Duration::from_millis(60), tx);

        let first = recv_batch(&rx, Duration::from_secs(5));
        assert_eq!(first.len(), 2);
        assert_eq!(first[0].pid, 1557);
        assert_eq!(first[0].comm, "kworker/u99:1");
        assert_eq!(first[0].stat, "LIBG");
        assert_eq!(first[0].lat_cri, 2304);
        assert_eq!(first[0].avg_lat_cri, 1789);
        assert_eq!(first[0].norm_lat_cri, 790);
        assert_eq!(first[0].wait_freq, 2876);
        assert_eq!(first[0].wake_freq, 3);
        assert_eq!(first[0].run_freq, 2133);
        assert_eq!(first[0].avg_runtime_wall, 19644);
        assert_eq!(first[0].task_util_est, 7);
        assert_eq!(first[0].cpu_util_wall, 3);
        assert_eq!(first[0].static_prio, 0);
        assert!(h.state().is_live());

        let second = recv_batch(&rx, Duration::from_secs(5));
        assert_eq!(second.len(), 1);
        assert_eq!(second[0].static_prio, 120); // coerced from "120"

        let third = recv_batch(&rx, Duration::from_secs(5));
        assert_eq!(third.len(), 1);
        assert_eq!(third[0].pid, 4242);

        h.shutdown();
        let t0 = Instant::now();
        h.join().unwrap();
        assert!(t0.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn stats_meta_field_detection() {
        let dir = TestDir::new("meta");
        let server = FakeServer::start(
            dir.path(),
            vec![
                Reply::Line(meta_map_line()),
                Reply::Line(batch_line(vec![live_sample(1, "x")])),
            ],
        );
        let (tx, rx) = mpsc::channel();
        let h = spawn_with(&[server.socket()], fast_tuning(Duration::from_millis(300)), tx);
        let _ = recv_batch(&rx, Duration::from_secs(5));
        wait_for_state(&h, |s| s.is_live(), Duration::from_secs(2));

        let fields = h.fields();
        assert_eq!(fields.len(), 38);
        assert!(fields.has("norm_lat_cri"));
        assert!(fields.has("cpu_heat")); // known to lavd, unknown to SchedSample
        assert!(fields.has("warm_cpu_id"));
        assert!(fields.has("static_prio"));
        assert!(!fields.has("definitely_not_a_field"));
        assert!(fields.sched_sample_fields().any(|f| f == "wait_freq"));

        h.shutdown();
        h.join().unwrap();
    }

    #[test]
    fn stats_meta_array_shape_parses() {
        let dir = TestDir::new("metaarr");
        let server = FakeServer::start(
            dir.path(),
            vec![
                Reply::Line(meta_array_line()),
                Reply::Line(batch_line(vec![live_sample(2, "y")])),
            ],
        );
        let (tx, rx) = mpsc::channel();
        let h = spawn_with(&[server.socket()], fast_tuning(Duration::from_millis(300)), tx);
        let _ = recv_batch(&rx, Duration::from_secs(5));
        let fields = h.fields();
        assert_eq!(fields.len(), 38);
        assert!(fields.has("warm_cpu_id"));
        h.shutdown();
        h.join().unwrap();
    }

    #[test]
    fn preempted_degraded_then_reconnect_live() {
        let dir = TestDir::new("preempt");
        let server = FakeServer::start(
            dir.path(),
            vec![
                Reply::Line(meta_map_line()),
                Reply::Line(preempt_line()),
                Reply::Line(meta_map_line()),
                Reply::Line(batch_line(vec![live_sample(7, "resumed")])),
            ],
        );
        let (tx, rx) = mpsc::channel();
        // Long preempt retry so the Degraded window is easy to observe.
        let h = spawn_with(&[server.socket()], fast_tuning(Duration::from_millis(500)), tx);

        let degraded = wait_for_state(
            &h,
            |s| matches!(s, SamplerState::Degraded { reason } if reason == REASON_PREEMPTED),
            Duration::from_secs(3),
        );
        assert_eq!(degraded, SamplerState::degraded(REASON_PREEMPTED));

        // The reconnect reclaims ownership and batches flow again.
        wait_for_state(&h, |s| s.is_live(), Duration::from_secs(5));
        let batch = recv_batch(&rx, Duration::from_secs(5));
        assert_eq!(batch.len(), 1);
        assert_eq!(batch[0].pid, 7);
        assert!(server.connections() >= 2);

        h.shutdown();
        h.join().unwrap();
    }

    #[test]
    fn eof_mid_request_reconnects() {
        let dir = TestDir::new("eof");
        let server = FakeServer::start(
            dir.path(),
            vec![
                Reply::Line(meta_map_line()),
                Reply::Eof,
                Reply::Line(meta_map_line()),
                Reply::Line(batch_line(vec![live_sample(11, "after-eof")])),
            ],
        );
        let (tx, rx) = mpsc::channel();
        let h = spawn_with(&[server.socket()], fast_tuning(Duration::from_millis(300)), tx);

        let batch = recv_batch(&rx, Duration::from_secs(5));
        assert_eq!(batch.len(), 1);
        assert_eq!(batch[0].pid, 11);
        wait_for_state(&h, |s| s.is_live(), Duration::from_secs(2));
        assert!(server.connections() >= 2);

        h.shutdown();
        h.join().unwrap();
    }

    #[test]
    fn malformed_json_line_tolerated_and_reconnects() {
        let dir = TestDir::new("badjson");
        let server = FakeServer::start(
            dir.path(),
            vec![
                Reply::Line(meta_map_line()),
                Reply::Line("{\"errno\":0,\"args\":{\"resp\":{\"samples\":[".to_owned()),
                Reply::Line(meta_map_line()),
                Reply::Line(batch_line(vec![live_sample(12, "after-malformed")])),
            ],
        );
        let (tx, rx) = mpsc::channel();
        let h = spawn_with(&[server.socket()], fast_tuning(Duration::from_millis(300)), tx);

        let batch = recv_batch(&rx, Duration::from_secs(5));
        assert_eq!(batch.len(), 1);
        assert_eq!(batch[0].pid, 12);
        wait_for_state(&h, |s| s.is_live(), Duration::from_secs(2));
        assert!(server.connections() >= 2);

        h.shutdown();
        h.join().unwrap();
    }

    #[test]
    fn shutdown_unblocks_unresponsive_server() {
        let dir = TestDir::new("hang");
        let server = FakeServer::start(dir.path(), vec![Reply::Hang]);
        let (tx, rx) = mpsc::channel();
        let h = spawn(&[server.socket()], DEFAULT_CADENCE, tx);
        // Let the thread block on the never-answered stats_meta read.
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

    #[test]
    fn empty_batch_delivered() {
        let dir = TestDir::new("empty");
        let server = FakeServer::start(
            dir.path(),
            vec![
                Reply::Line(meta_map_line()),
                Reply::Line(batch_line(vec![])),
                Reply::Line(batch_line(vec![])),
            ],
        );
        let (tx, rx) = mpsc::channel();
        let h = spawn_with(&[server.socket()], fast_tuning(Duration::from_millis(300)), tx);

        assert!(recv_batch(&rx, Duration::from_secs(5)).is_empty());
        assert!(recv_batch(&rx, Duration::from_secs(5)).is_empty());

        h.shutdown();
        h.join().unwrap();
    }

    #[test]
    fn batch_at_ringbuf_cap() {
        let dir = TestDir::new("cap60");
        let samples: Vec<Value> = (0..60i64).map(|i| live_sample(1000 + i, "spinner")).collect();
        let server = FakeServer::start(
            dir.path(),
            vec![
                Reply::Line(meta_map_line()),
                Reply::Line(batch_line(samples)),
            ],
        );
        let (tx, rx) = mpsc::channel();
        let h = spawn_with(&[server.socket()], fast_tuning(Duration::from_millis(300)), tx);

        let batch = recv_batch(&rx, Duration::from_secs(5));
        assert_eq!(batch.len(), 60); // the ~60-sample ringbuf cap shape
        assert_eq!(batch[0].pid, 1000);
        assert_eq!(batch[59].pid, 1059);

        h.shutdown();
        h.join().unwrap();
    }

    #[test]
    fn lenient_samples_string_numbers_unknowns_defaults() {
        let dir = TestDir::new("lenient");
        let mut a = live_sample(7, "a");
        a["lat_cri"] = json!(" 999 "); // trimmed string number
        a["static_prio"] = json!("120");
        let garbage = json!(["not", "a", "sample"]);
        let c = json!({ "pid": 42, "comm": 7, "wait_freq": "oops" });
        let server = FakeServer::start(
            dir.path(),
            vec![
                Reply::Line(meta_map_line()),
                Reply::Line(batch_line(vec![a, garbage, c])),
            ],
        );
        let (tx, rx) = mpsc::channel();
        let h = spawn_with(&[server.socket()], fast_tuning(Duration::from_millis(300)), tx);

        let batch = recv_batch(&rx, Duration::from_secs(5));
        assert_eq!(batch.len(), 2); // non-object entry skipped
        assert_eq!(batch[0].lat_cri, 999);
        assert_eq!(batch[0].static_prio, 120);
        assert_eq!(batch[0].norm_lat_cri, 790); // unknown sibling fields ignored
        assert_eq!(batch[1].pid, 42);
        assert_eq!(batch[1].comm, ""); // wrong-typed -> serde(default)
        assert_eq!(batch[1].wait_freq, 0); // unparseable string -> serde(default)

        h.shutdown();
        h.join().unwrap();
    }

    #[test]
    fn first_socket_path_missing_falls_back() {
        let dir = TestDir::new("fallback");
        let server = FakeServer::start(
            dir.path(),
            vec![
                Reply::Line(meta_map_line()),
                Reply::Line(batch_line(vec![live_sample(55, "fallback")])),
            ],
        );
        let (tx, rx) = mpsc::channel();
        let missing = dir.path().join("missing-stats");
        let h = spawn_with(
            &[missing, server.socket()],
            fast_tuning(Duration::from_millis(300)),
            tx,
        );

        let batch = recv_batch(&rx, Duration::from_secs(5));
        assert_eq!(batch[0].pid, 55);

        h.shutdown();
        h.join().unwrap();
    }
}

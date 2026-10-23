//! `BpfExecSource`: the eBPF-only kernel event source.
//!
//! Loads the embedded CO-RE object (`bpf/exec.bpf.c`, compiled by `build.rs`),
//! attaches exactly one tracepoint (`tp/sched/sched_process_exec`), and decodes
//! ringbuf samples with no heap allocation in the steady state (pre-reserved
//! queue, `Copy` events).
//!
//! Wake path: `wake()` sets a shutdown flag and writes an eventfd that is
//! registered — together with the ring buffer's own epoll fd (`RingBuffer::
//! epoll_fd`) — in one epoll set. `next_event` blocks in `epoll_wait`, so
//! shutdown is immediate, not timeout-limited; the bounded 200 ms timeout is
//! only a guard against pathological epoll behavior.
//!
//! Batching: every wakeup greedily drains ALL pending ringbuf records
//! (`ring_buffer__consume`) into the pre-reserved queue before blocking again,
//! and a bounded busy-poll window (`ANANICY_RS_BUSY_SPIN_US`, default 0)
//! runs before each blocking wait — under burst arrivals the next record lands
//! within microseconds of the previous one and is served without an epoll
//! round trip. The spin is bounded and checks the shutdown flag every
//! iteration, so `wake()`/shutdown latency stays far below the 200 ms
//! worst-case budget regardless of the setting.

use std::collections::VecDeque;
use std::ffi::OsStr;
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, Once, PoisonError};
use std::time::{Duration, Instant};

use libbpf_rs::{Link, MapCore, Object, ObjectBuilder, PrintLevel, RingBuffer, RingBufferBuilder};

use crate::{btf_available, ExecEvent, SourceError};

/// Embedded BPF object: `bpf/exec.bpf.c` compiled to BTF-bearing bytecode.
/// Strategy: `include_bytes!` + `ObjectBuilder::open_memory` (available in
/// libbpf-rs 0.27) — the packaged daemon depends on no external files. (No
/// temp-file/`open_file` fallback needed for that reason.)
static BPF_OBJ: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/exec.bpf.o"));

const PROG_NAME: &str = "handle_exec";
const MAP_NAME: &str = "events";

/// Backstop only; the eventfd makes wake-ups immediate. Kept bounded so a
/// lost wake can never hang shutdown longer than ~200 ms.
const POLL_TIMEOUT: Duration = Duration::from_millis(200);
const POLL_TIMEOUT_MS: i32 = POLL_TIMEOUT.as_millis() as i32;

/// Default busy-poll window before the blocking wait (µs). Measured A/B on
/// the paced 2000 execs/s storm (2026-10-23, `docs/BENCHMARKS.md`): a 30 µs
/// spin cost ~30 µs of CPU per event (+157% wall, +357% instructions/event)
/// while context-switches stayed at ~1.0/event — paced arrivals are uniform,
/// so the spin rides out its window empty and collapses nothing. Default is
/// therefore 0 (the consume-all-before-blocking drain stays unconditional).
/// For genuinely bursty workloads (e.g. `make -j` storms) raise it via
/// [`BUSY_SPIN_ENV`]; the window is bounded and the shutdown flag is checked
/// every iteration, so `wake()` latency stays tiny regardless of the setting.
const BUSY_SPIN_DEFAULT_US: u64 = 0;
/// Cap on the configurable window (µs): keeps the pre-block busy-poll bounded.
const BUSY_SPIN_MAX_US: u64 = 1_000;
/// Override for the busy-poll window, in µs (`0` disables the spin but keeps
/// the consume-all-before-blocking drain).
const BUSY_SPIN_ENV: &str = "ANANICY_RS_BUSY_SPIN_US";

/// Pre-reserved steady-state queue depth (~exec bursts between polls).
const QUEUE_CAPACITY: usize = 4096;

/// Cap on captured libbpf log lines folded into error messages.
const LOG_LINES_MAX: usize = 256;

const WAKE_TOKEN: u64 = 1;
const RING_TOKEN: u64 = 2;

// ---------------------------------------------------------------------------
// libbpf diagnostics: Debug-level, printed to stderr AND captured so every
// SourceError can carry the log text. Load/attach failures are never silent.
// ---------------------------------------------------------------------------

static PRINT_INIT: Once = Once::new();
static LOG: Mutex<Vec<String>> = Mutex::new(Vec::new());

fn print_cb(level: PrintLevel, msg: String) {
    eprint!("[libbpf:{:?}] {}", level, msg);
    let mut log = LOG.lock().unwrap_or_else(PoisonError::into_inner);
    if log.len() >= LOG_LINES_MAX {
        log.remove(0);
    }
    log.push(msg);
}

fn take_log() -> String {
    let mut log = LOG.lock().unwrap_or_else(PoisonError::into_inner);
    let text = log.concat();
    log.clear();
    text
}

fn install_print() {
    PRINT_INIT.call_once(|| {
        libbpf_rs::set_print(Some((PrintLevel::Debug, print_cb)));
    });
}

/// Append the captured libbpf log (if any) to an error string.
fn with_log(msg: String) -> String {
    let log = take_log();
    if log.is_empty() {
        msg
    } else {
        format!("{msg}\nlibbpf log:\n{log}")
    }
}

fn load_err(what: &str, e: std::io::Error) -> SourceError {
    SourceError::LoadFailed(with_log(format!("{what}: {e}")))
}

/// Busy-poll window in µs: unparsable values fall back to
/// [`BUSY_SPIN_DEFAULT_US`], everything is clamped to [`BUSY_SPIN_MAX_US`]
/// (the pre-block spin must stay bounded).
fn busy_spin_us_from(value: Option<&std::ffi::OsStr>) -> u64 {
    value
        .and_then(|v| v.to_str())
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(BUSY_SPIN_DEFAULT_US)
        .min(BUSY_SPIN_MAX_US)
}

// ---------------------------------------------------------------------------

#[derive(Debug)]
struct QueueState {
    events: VecDeque<ExecEvent>,
    /// Samples dropped because the queue was full (consumer stalled).
    dropped: u64,
}

impl QueueState {
    fn new() -> Self {
        let mut events = VecDeque::new();
        events.reserve(QUEUE_CAPACITY);
        Self {
            events,
            dropped: 0,
        }
    }
}

/// Concrete `ExecSource` over the kernel's `sched_process_exec` tracepoint.
///
/// Threading: `wake()` is safe from any thread (that is its purpose);
/// `next_event` is single-consumer — concurrent calls are serialized by an
/// internal lock and simply interleave events between callers.
pub struct BpfExecSource {
    // Field order is load-bearing for Drop: `rb` holds the ring map's fd and
    // must be destroyed before `obj`; links detach before the object unloads.
    rb: Mutex<RingBuffer<'static>>,
    links: Vec<Link>,
    // Owned for lifetime only: it holds the ring map fd `rb` consumes, and it
    // must outlive `rb` (field order above guarantees the drop order).
    #[expect(dead_code, reason = "keeps maps/programs alive for rb and links")]
    obj: Object,
    queue: Arc<Mutex<QueueState>>,
    shutdown: AtomicBool,
    /// Bounded busy-poll window before each blocking wait (0 = drain only).
    busy_spin: Duration,
    wake_fd: OwnedFd,
    wait_fd: OwnedFd,
}

impl BpfExecSource {
    /// Load and attach the embedded program. Fails loudly with libbpf's log
    /// text: no BTF -> `NoBtf`, open/load/ringbuf setup -> `LoadFailed`,
    /// tracepoint attach -> `AttachFailed`.
    pub fn new() -> Result<Self, SourceError> {
        install_print();

        if !btf_available() {
            return Err(SourceError::NoBtf);
        }

        let mut builder = ObjectBuilder::default();
        let open_obj = builder.open_memory(BPF_OBJ).map_err(|e| {
            SourceError::LoadFailed(with_log(format!(
                "failed to open embedded BPF object: {e}"
            )))
        })?;
        let obj = open_obj.load().map_err(|e| {
            SourceError::LoadFailed(with_log(format!(
                "failed to load sched_process_exec program: {e}"
            )))
        })?;

        // Attach exactly one program; anything else in the object is a bug.
        let mut links = Vec::new();
        let mut prog_seen = false;
        for prog in obj.progs_mut() {
            if prog.name() != OsStr::new(PROG_NAME) {
                continue;
            }
            prog_seen = true;
            links.push(prog.attach().map_err(|e| {
                SourceError::AttachFailed(with_log(format!(
                    "failed to attach tp/sched/sched_process_exec ({PROG_NAME}): {e}"
                )))
            })?);
        }
        if !prog_seen {
            return Err(SourceError::LoadFailed(with_log(format!(
                "embedded object has no program named {PROG_NAME}"
            ))));
        }

        let map = obj
            .maps()
            .find(|m| m.name() == OsStr::new(MAP_NAME))
            .ok_or_else(|| {
                SourceError::LoadFailed(with_log(format!(
                    "embedded object has no ringbuf map named {MAP_NAME}"
                )))
            })?;

        let queue = Arc::new(Mutex::new(QueueState::new()));
        let cb_queue = Arc::clone(&queue);
        let mut rbb = RingBufferBuilder::new();
        rbb.add(&map, move |sample: &[u8]| -> i32 {
            // Steady state: no allocation (queue pre-reserved; ExecEvent is Copy).
            if let Some(ev) = ExecEvent::from_sample(sample) {
                let mut q = cb_queue.lock().unwrap_or_else(PoisonError::into_inner);
                if q.events.len() >= QUEUE_CAPACITY {
                    q.dropped += 1;
                } else {
                    q.events.push_back(ev);
                }
            } else {
                eprintln!(
                    "aranicy-events: malformed {}-byte ringbuf sample dropped",
                    sample.len()
                );
            }
            0
        })
        .map_err(|e| {
            SourceError::LoadFailed(with_log(format!("failed to set up ring buffer: {e}")))
        })?;
        // NOTE: libbpf-rs 0.27 erases the map borrow's lifetime at build();
        // the ring buffer keeps using `map`'s fd, so `rb` MUST drop before
        // `obj` (guaranteed by field order + explicit Drop below).
        let rb = rbb.build().map_err(|e| {
            SourceError::LoadFailed(with_log(format!("failed to build ring buffer: {e}")))
        })?;

        let wake_fd = eventfd().map_err(|e| load_err("failed to create eventfd", e))?;
        let wait_fd = epoll_create().map_err(|e| load_err("failed to create epoll fd", e))?;
        epoll_add(&wait_fd, wake_fd.as_raw_fd(), WAKE_TOKEN)
            .map_err(|e| load_err("failed to register eventfd", e))?;
        epoll_add(&wait_fd, rb.epoll_fd(), RING_TOKEN)
            .map_err(|e| load_err("failed to register ring buffer epoll fd", e))?;

        Ok(Self {
            rb: Mutex::new(rb),
            links,
            obj,
            queue,
            shutdown: AtomicBool::new(false),
            busy_spin: Duration::from_micros(busy_spin_us_from(
                std::env::var_os(BUSY_SPIN_ENV).as_deref(),
            )),
            wake_fd,
            wait_fd,
        })
    }

    /// Samples dropped because the consumer stalled past `QUEUE_CAPACITY`
    /// (observability; the kernel-side ringbuf drop counter is not exposed).
    pub fn dropped_events(&self) -> u64 {
        self.queue
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .dropped
    }

    /// True once `wake()` (or drop) has requested shutdown.
    pub fn is_shutdown(&self) -> bool {
        self.shutdown.load(Ordering::Acquire)
    }

    fn pop(&self) -> Option<ExecEvent> {
        self.queue
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .events
            .pop_front()
    }

    /// Greedily consume ALL pending ringbuf records into the queue
    /// (`ring_buffer__consume` runs the sample callback on this thread until
    /// the ring is empty). `EINTR` is not an error.
    fn drain_ring(&self) -> io::Result<()> {
        let c = self
            .rb
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .consume_raw();
        if c < 0 {
            let err = io::Error::from_raw_os_error(-c);
            return if err.kind() == io::ErrorKind::Interrupted {
                Ok(())
            } else {
                Err(err)
            };
        }
        Ok(())
    }

    /// Bounded busy-poll before the blocking wait: non-blocking ring drains +
    /// queue checks until an event is ready, shutdown is requested, or the
    /// `busy_spin` window expires. Even with the window at zero this performs
    /// one drain pass, so ALL pending records are consumed before blocking.
    fn busy_poll(&self) -> io::Result<Option<ExecEvent>> {
        let deadline = Instant::now().checked_add(self.busy_spin);
        loop {
            if self.shutdown.load(Ordering::Acquire) {
                return Ok(None);
            }
            self.drain_ring()?;
            if let Some(ev) = self.pop() {
                return Ok(Some(ev));
            }
            match deadline {
                Some(d) if Instant::now() < d => std::hint::spin_loop(),
                _ => return Ok(None),
            }
        }
    }

    /// One blocking wait + drain. Errors are real (ringbuf/epoll failures).
    fn wait_once(&self) -> io::Result<()> {
        let mut ready = [
            libc::epoll_event {
                events: 0,
                u64: 0,
            },
            libc::epoll_event {
                events: 0,
                u64: 0,
            },
        ];
        let n = unsafe {
            libc::epoll_wait(
                self.wait_fd.as_raw_fd(),
                ready.as_mut_ptr(),
                ready.len() as i32,
                POLL_TIMEOUT_MS,
            )
        };
        if n < 0 {
            let err = io::Error::last_os_error();
            return if err.kind() == io::ErrorKind::Interrupted {
                Ok(())
            } else {
                Err(err)
            };
        }

        let mut ring_ready = false;
        for slot in ready.iter().take(n as usize) {
            match slot.u64 {
                WAKE_TOKEN => {
                    // Drain the eventfd counter (ignore EAGAIN: already drained).
                    let mut buf = [0u8; 8];
                    loop {
                        let r = unsafe {
                            libc::read(
                                self.wake_fd.as_raw_fd(),
                                buf.as_mut_ptr().cast(),
                                buf.len(),
                            )
                        };
                        if r <= 0 {
                            break;
                        }
                    }
                }
                RING_TOKEN => ring_ready = true,
                _ => {}
            }
        }

        if ring_ready {
            // Runs the sample callback on this thread; fills the queue.
            self.drain_ring()?;
        }
        Ok(())
    }
}

impl crate::ExecSource for BpfExecSource {
    fn next_event(&self) -> io::Result<Option<ExecEvent>> {
        loop {
            // Shutdown wins over queued events: exit must be prompt.
            if self.shutdown.load(Ordering::Acquire) {
                return Ok(None);
            }
            if let Some(ev) = self.pop() {
                return Ok(Some(ev));
            }
            // Drain + bounded busy-poll first: consume ALL pending records and
            // ride burst arrivals before paying for a blocking epoll round trip.
            if let Some(ev) = self.busy_poll()? {
                return Ok(Some(ev));
            }
            self.wait_once()?;
        }
    }

    fn wake(&self) {
        self.shutdown.store(true, Ordering::Release);
        let one: u64 = 1;
        // Best-effort: EAGAIN means a wake is already pending.
        let _ = unsafe {
            libc::write(
                self.wake_fd.as_raw_fd(),
                (&raw const one).cast::<libc::c_void>(),
                std::mem::size_of::<u64>(),
            )
        };
    }
}

impl Drop for BpfExecSource {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::Release);
        // Explicit detach: destroying the link removes the tracepoint hook
        // (perf-event links — which `tp/...` attaches produce — do not support
        // bpf_link__detach [EOPNOTSUPP]; bpf_link__destroy is the detach path
        // and is what `Link`'s own Drop runs). Field drops would do the same
        // later; doing it here makes teardown order deterministic.
        self.links.clear();
    }
}

// Fd plumbing ---------------------------------------------------------------

fn eventfd() -> io::Result<OwnedFd> {
    let fd = unsafe { libc::eventfd(0, libc::EFD_CLOEXEC | libc::EFD_NONBLOCK) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(unsafe { OwnedFd::from_raw_fd(fd) })
}

fn epoll_create() -> io::Result<OwnedFd> {
    let fd = unsafe { libc::epoll_create1(libc::EPOLL_CLOEXEC) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(unsafe { OwnedFd::from_raw_fd(fd) })
}

fn epoll_add(epoll: &OwnedFd, fd: i32, token: u64) -> io::Result<()> {
    let mut ev = libc::epoll_event {
        events: libc::EPOLLIN as u32,
        u64: token,
    };
    let rc = unsafe { libc::epoll_ctl(epoll.as_raw_fd(), libc::EPOLL_CTL_ADD, fd, &mut ev) };
    if rc < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The wire-format size the loader assumes (mirrors bpf/exec_event.h).
    #[test]
    fn loader_uses_wire_size() {
        assert_eq!(crate::raw::RAW_EVENT_SIZE, 24);
    }

    /// BTF absence maps to the hard-fail variant — no fallback exists.
    #[test]
    fn source_error_no_btf_is_loud() {
        let msg = SourceError::NoBtf.to_string();
        assert!(msg.contains("BTF"), "unhelpful message: {msg}");
    }

    /// Busy-poll window parsing: default on absence/garbage, bounded above.
    #[test]
    fn busy_spin_env_parsing() {
        use std::ffi::OsStr;
        assert_eq!(busy_spin_us_from(None), BUSY_SPIN_DEFAULT_US);
        assert_eq!(busy_spin_us_from(Some(OsStr::new("0"))), 0);
        assert_eq!(busy_spin_us_from(Some(OsStr::new("45"))), 45);
        assert_eq!(busy_spin_us_from(Some(OsStr::new("100000"))), BUSY_SPIN_MAX_US);
        assert_eq!(
            busy_spin_us_from(Some(OsStr::new("junk"))),
            BUSY_SPIN_DEFAULT_US
        );
        assert_eq!(busy_spin_us_from(Some(OsStr::new(""))), BUSY_SPIN_DEFAULT_US);
        assert_eq!(
            busy_spin_us_from(Some(OsStr::new("-5"))),
            BUSY_SPIN_DEFAULT_US
        );
        assert!(BUSY_SPIN_DEFAULT_US <= BUSY_SPIN_MAX_US);
    }
}

//! Exec-event capture and delivery.
//!
//! The collector owns the exec ring buffer (the daemon's own BPF program was
//! retired). Each event is enriched with the match name — `basename(argv[0])`
//! from `/proc/<pid>/cmdline`, then the exe link, then the kernel comm — at
//! CAPTURE time, microseconds after the event. That removes the process-exit
//! race a downstream consumer would hit: the same lookup run later in
//! aranicy's process often finds `/proc/<pid>` already gone for short-lived
//! build tools.
//!
//! Transport is a private fixed-size frame protocol (not scx_stats): event
//! streams are push-shaped, while scx_stats is request/response.
//!
//! Frames travel in batches: the pump takes everything queued (up to
//! `BATCH_FRAMES`) and writes it with one `writev`, so the daemon needs one
//! `read` per batch instead of one per exec event.
//!
//! Frame layout: see `aranicy_wire` (280 bytes, 256-byte match name).

use std::collections::VecDeque;
use std::io::{ErrorKind, IoSlice, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use anyhow::{Context, Result};

pub use aranicy_wire::{BATCH_FRAMES, FrameBuf, COMM_LEN, FRAME_LEN};

/// Bounded backlog while no client is attached (or it is slow). Exec events
/// are advisory: dropping the oldest is better than blocking the producer.
pub const QUEUE_CAP: usize = 8192;

/// Shared queue written by the BPF ringbuffer callback, read by the server.
/// A condvar (not a sleep loop) wakes the pump: polling at 500 Hz burned a
/// measurable share of a core for events that arrive a few times a second.
#[derive(Clone, Default)]
pub struct EventQueue {
    inner: Arc<(Mutex<VecDeque<FrameBuf>>, Condvar)>,
    dropped: Arc<AtomicU64>,
}

impl EventQueue {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&self, frame: FrameBuf) {
        let (q, cv) = &*self.inner;
        let mut q = q.lock().unwrap_or_else(|e| e.into_inner());
        if q.len() >= QUEUE_CAP {
            q.pop_front();
            self.dropped.fetch_add(1, Ordering::Relaxed);
        }
        q.push_back(frame);
        cv.notify_one();
    }

    /// Put a frame back after a failed write so a reconnect does not lose it.
    pub fn push_front(&self, frame: FrameBuf) {
        let (q, cv) = &*self.inner;
        q.lock()
            .unwrap_or_else(|e| e.into_inner())
            .push_front(frame);
        cv.notify_one();
    }

    /// Wait up to `wait` for the next frame, then take everything else
    /// already queued, up to `out.len()`. One lock acquisition for the whole
    /// batch: the pump hands it to a single `writev`.
    pub fn pop_batch(&self, wait: Duration, out: &mut [FrameBuf]) -> usize {
        let (q, cv) = &*self.inner;
        let guard = q.lock().unwrap_or_else(|e| e.into_inner());
        let (mut guard, _) = cv
            .wait_timeout_while(guard, wait, |q| q.is_empty())
            .unwrap_or_else(|e| e.into_inner());
        let n = guard.len().min(out.len());
        for slot in out.iter_mut().take(n) {
            *slot = guard.pop_front().expect("len was taken under the same lock");
        }
        n
    }

    #[cfg(test)]
    pub fn pop(&self) -> Option<FrameBuf> {
        self.inner
            .0
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .pop_front()
    }

    pub fn dropped(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }
}

/// A stalled client must not wedge the only pump thread: give up on the write
/// (and drop the connection) instead of blocking forever.
const WRITE_TIMEOUT: Duration = Duration::from_millis(500);
/// Accept poll interval; the listener is non-blocking so `stop` is honoured
/// even while nobody connects.
const ACCEPT_POLL: Duration = Duration::from_millis(20);

/// How long the pump waits for a frame before re-checking the client and the
/// shutdown flag.
const PUMP_WAIT: Duration = Duration::from_millis(50);

/// Bind the event socket: unlink a stale file, bind, chmod 0600 and make the
/// listener non-blocking. Separate from [`serve`] so a bind failure aborts
/// startup instead of leaving a running collector without an event stream.
pub fn bind(path: &Path) -> Result<UnixListener> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)
            .with_context(|| format!("failed to create socket dir {}", dir.display()))?;
    }
    // A stale socket file from an unclean exit would make bind fail EADDRINUSE.
    match std::fs::remove_file(path) {
        Ok(()) => {}
        Err(e) if e.kind() == ErrorKind::NotFound => {}
        Err(e) => {
            return Err(e)
                .with_context(|| format!("failed to clear stale socket {}", path.display()))
        }
    }
    let listener = UnixListener::bind(path)
        .with_context(|| format!("failed to bind event socket {}", path.display()))?;
    // Exec events are consumed by aranicy (root); nobody else may connect
    // first and starve it.
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
        .with_context(|| format!("failed to chmod {}", path.display()))?;
    listener
        .set_nonblocking(true)
        .with_context(|| format!("failed to set {} non-blocking", path.display()))?;
    Ok(listener)
}

/// Serve the event stream on an already-bound listener until `stop` is set.
/// One consumer at a time; a write failure (client gone, or too slow) returns
/// to accept.
pub fn serve(
    listener: UnixListener,
    path: &Path,
    queue: EventQueue,
    stop: Arc<AtomicBool>,
) -> Result<()> {
    tracing::info!(socket = %path.display(), "collector serving exec events");

    while !stop.load(Ordering::Acquire) {
        let mut stream = match listener.accept() {
            Ok((s, _)) => s,
            Err(e) if e.kind() == ErrorKind::WouldBlock => {
                std::thread::sleep(ACCEPT_POLL);
                continue;
            }
            Err(e) => {
                tracing::warn!(error = %e, "event accept failed");
                std::thread::sleep(ACCEPT_POLL);
                continue;
            }
        };
        tracing::info!("exec-event client connected");
        let _ = stream.set_write_timeout(Some(WRITE_TIMEOUT));
        if let Err(e) = pump(&mut stream, &queue, &stop) {
            tracing::info!(
                error = %e,
                dropped = queue.dropped(),
                "exec-event client disconnected"
            );
        }
    }
    Ok(())
}

/// A client that closed its end reports EOF immediately; without this the
/// pump would sit on the dead socket until the next exec event. `MSG_DONTWAIT`
/// keeps the probe from blocking (`UnixStream::peek` is still unstable).
fn peer_closed(stream: &UnixStream) -> bool {
    use std::os::unix::io::AsRawFd;

    let mut probe = [0u8; 1];
    let n = unsafe {
        libc::recv(
            stream.as_raw_fd(),
            probe.as_mut_ptr().cast(),
            probe.len(),
            libc::MSG_PEEK | libc::MSG_DONTWAIT,
        )
    };
    n == 0
}

fn pump(stream: &mut UnixStream, queue: &EventQueue, stop: &AtomicBool) -> std::io::Result<()> {
    // One stack batch, reused: a burst of execs costs one write, not one each.
    let mut batch = [FrameBuf::default(); BATCH_FRAMES];
    while !stop.load(Ordering::Acquire) {
        let n = queue.pop_batch(PUMP_WAIT, &mut batch);
        if n == 0 {
            if peer_closed(stream) {
                return Ok(());
            }
            continue;
        }
        let mut written = 0;
        if let Err(e) = write_batch(stream, &batch[..n], &mut written) {
            // Requeue only what the peer does not already have: frames a
            // successful write put in its socket buffer are read and handled
            // before it sees EOF, so resending them would duplicate execs. A
            // frame cut in half is discarded by the reader's buffer reset and
            // is resent from its own first byte.
            let sent = written / FRAME_LEN;
            for frame in batch[sent..n].iter().rev() {
                queue.push_front(*frame);
            }
            return Err(e);
        }
    }
    Ok(())
}

/// One `writev` for the whole batch. A short write advances through the
/// remaining frames; a zero-length write is a dead socket, not a retry.
/// `written` accumulates the bytes the peer already has, so a caller can tell
/// delivered frames from unsent ones when a later write fails.
fn write_batch<W: Write>(
    stream: &mut W,
    frames: &[FrameBuf],
    written: &mut usize,
) -> std::io::Result<()> {
    let mut slices: [IoSlice<'_>; BATCH_FRAMES] = std::array::from_fn(|i| {
        IoSlice::new(frames.get(i).map_or(&[][..], |f| &f.as_bytes()[..]))
    });
    let mut slices = &mut slices[..frames.len()];
    while !slices.is_empty() {
        let n = stream.write_vectored(slices)?;
        if n == 0 {
            return Err(std::io::Error::new(
                ErrorKind::WriteZero,
                "exec-event batch write returned 0",
            ));
        }
        *written += n;
        IoSlice::advance_slices(&mut slices, n);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use aranicy_wire::BatchBuf;
    use std::io::Read;
    use std::sync::atomic::AtomicBool;

    #[test]
    fn pop_batch_takes_what_is_queued_and_leaves_the_rest() {
        let q = EventQueue::new();
        for i in 0..3 {
            q.push(FrameBuf::new(i, [0; COMM_LEN], 0, b"n"));
        }
        let mut out = [FrameBuf::default(); 2];
        assert_eq!(q.pop_batch(Duration::from_millis(10), &mut out), 2);
        assert_eq!(out[0].pid(), 0);
        assert_eq!(out[1].pid(), 1);
        assert_eq!(q.pop_batch(Duration::from_millis(10), &mut out), 1);
        assert_eq!(out[0].pid(), 2, "the rest stays queued, in order");
        assert_eq!(q.pop_batch(Duration::from_millis(10), &mut out), 0);
    }

    /// A batch that fails mid-write must report how many bytes the peer
    /// already has: requeueing delivered frames would duplicate execs, and a
    /// half-written frame must be resent from its own first byte.
    #[test]
    fn a_failed_batch_write_reports_what_was_sent() {
        struct BudgetWriter {
            left: usize,
        }
        impl Write for BudgetWriter {
            fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
                let n = buf.len().min(self.left);
                if n == 0 {
                    return Err(std::io::Error::from(ErrorKind::WouldBlock));
                }
                self.left -= n;
                Ok(n)
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }

        let frames: Vec<FrameBuf> = (0..3)
            .map(|i| FrameBuf::new(i, [0; COMM_LEN], 0, b"n"))
            .collect();
        let mut writer = BudgetWriter {
            left: FRAME_LEN + 10,
        };
        let mut written = 0;
        let err = write_batch(&mut writer, &frames, &mut written).expect_err("socket blocks");
        assert_eq!(err.kind(), ErrorKind::WouldBlock);
        assert_eq!(written, FRAME_LEN + 10);
        assert_eq!(
            written / FRAME_LEN,
            1,
            "one whole frame delivered, the rest is resent"
        );
    }

    /// The pump is the other half of the batch seam: whatever is queued when
    /// it wakes reaches the peer in order, and the peer's one buffer admits
    /// the whole burst.
    #[test]
    fn the_pump_delivers_a_queued_burst_in_order() {
        let (mut server, mut client) = UnixStream::pair().expect("socketpair");
        let queue = EventQueue::new();
        for i in 0..3 {
            queue.push(FrameBuf::new(i, [0; COMM_LEN], 0, b"n"));
        }
        let stop = Arc::new(AtomicBool::new(false));
        let pump_handle = {
            let queue = queue.clone();
            let stop = Arc::clone(&stop);
            std::thread::spawn(move || pump(&mut server, &queue, &stop))
        };

        let mut got = Vec::new();
        let mut buf = BatchBuf::new();
        client
            .set_read_timeout(Some(Duration::from_secs(2)))
            .expect("read timeout");
        while got.len() < 3 {
            let n = client.read(buf.spare()).expect("read");
            assert!(n > 0, "pump wrote nothing");
            buf.commit(n);
            got.extend(buf.frames().map(|f| f.pid()));
            buf.clear();
        }
        assert_eq!(got, vec![0, 1, 2], "arrival order is preserved");

        stop.store(true, Ordering::Release);
        pump_handle
            .join()
            .expect("pump thread returns")
            .expect("pump saw no write error");
    }

    #[test]
    fn requeued_frame_survives_a_failed_write() {
        let q = EventQueue::new();
        q.push(FrameBuf::new(1, [0; COMM_LEN], 0, b"first"));
        let mut out = [FrameBuf::default(); 1];
        assert_eq!(q.pop_batch(Duration::from_millis(10), &mut out), 1);
        q.push_front(out[0]);
        assert_eq!(q.pop().map(|f| f.pid()), Some(1));
        assert!(q.pop().is_none());
    }

    #[test]
    fn pop_batch_times_out_to_zero_when_idle() {
        let q = EventQueue::new();
        let mut out = [FrameBuf::default(); BATCH_FRAMES];
        assert_eq!(q.pop_batch(Duration::from_millis(20), &mut out), 0);
    }

    #[test]
    fn queue_is_bounded_and_counts_drops() {
        let q = EventQueue::new();
        for i in 0..(QUEUE_CAP + 5) {
            q.push(FrameBuf::new(i as u32, [0; COMM_LEN], 0, b"n"));
        }
        assert_eq!(q.dropped(), 5);
        assert_eq!(q.pop().map(|f| f.pid()), Some(5), "oldest dropped first");
    }
}

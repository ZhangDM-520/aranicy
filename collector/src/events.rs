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
//! Frame layout: see `aranicy_wire` (280 bytes, 256-byte match name).

use std::collections::VecDeque;
use std::io::{ErrorKind, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use anyhow::{Context, Result};

pub use aranicy_wire::{COMM_LEN, ExecFrame};

/// Bounded backlog while no client is attached (or it is slow). Exec events
/// are advisory: dropping the oldest is better than blocking the producer.
pub const QUEUE_CAP: usize = 8192;

/// Shared queue written by the BPF ringbuffer callback, read by the server.
/// A condvar (not a sleep loop) wakes the pump: polling at 500 Hz burned a
/// measurable share of a core for events that arrive a few times a second.
#[derive(Clone, Default)]
pub struct EventQueue {
    inner: Arc<(Mutex<VecDeque<ExecFrame>>, Condvar)>,
    dropped: Arc<AtomicU64>,
}

impl EventQueue {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&self, frame: ExecFrame) {
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
    pub fn push_front(&self, frame: ExecFrame) {
        let (q, cv) = &*self.inner;
        q.lock()
            .unwrap_or_else(|e| e.into_inner())
            .push_front(frame);
        cv.notify_one();
    }

    /// Wait up to `wait` for the next frame.
    pub fn pop_timeout(&self, wait: Duration) -> Option<ExecFrame> {
        let (q, cv) = &*self.inner;
        let guard = q.lock().unwrap_or_else(|e| e.into_inner());
        let (mut guard, _) = cv
            .wait_timeout_while(guard, wait, |q| q.is_empty())
            .unwrap_or_else(|e| e.into_inner());
        guard.pop_front()
    }

    #[cfg(test)]
    pub fn pop(&self) -> Option<ExecFrame> {
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
    while !stop.load(Ordering::Acquire) {
        let Some(frame) = queue.pop_timeout(PUMP_WAIT) else {
            if peer_closed(stream) {
                return Ok(());
            }
            continue;
        };
        if let Err(e) = stream.write_all(&frame.encode()) {
            // Hand the frame back: the next client (a restarted aranicy)
            // must still see this exec, and `dropped` must stay meaningful.
            queue.push_front(frame);
            return Err(e);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requeued_frame_survives_a_failed_write() {
        let q = EventQueue::new();
        q.push(ExecFrame::new(1, [0; COMM_LEN], 0, b"first"));
        let frame = q
            .pop_timeout(Duration::from_millis(10))
            .expect("queued frame");
        q.push_front(frame);
        assert_eq!(q.pop().map(|f| f.pid), Some(1));
        assert!(q.pop().is_none());
    }

    #[test]
    fn pop_timeout_returns_none_when_idle() {
        let q = EventQueue::new();
        assert!(q.pop_timeout(Duration::from_millis(20)).is_none());
    }

    #[test]
    fn queue_is_bounded_and_counts_drops() {
        let q = EventQueue::new();
        for i in 0..(QUEUE_CAP + 5) {
            q.push(ExecFrame::new(i as u32, [0; COMM_LEN], 0, b"n"));
        }
        assert_eq!(q.dropped(), 5);
        assert_eq!(q.pop().map(|f| f.pid), Some(5), "oldest dropped first");
    }
}

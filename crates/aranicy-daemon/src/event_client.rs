//! Exec-event client for `aranicy-collectord`.
//!
//! The collector owns the exec ring buffer and resolves the match name at
//! capture time (see its `events` module); this client only decodes frames.
//! Reconnects with a fixed short backoff and counts the gaps so a collector
//! restart is visible in the summary line instead of silently losing events.

use std::io::{ErrorKind, Read};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use aranicy_wire::{ExecFrame, FRAME_LEN};

/// Backoff between reconnect attempts after a dropped collector connection.
const RECONNECT_DELAY: Duration = Duration::from_millis(200);

pub struct EventClient {
    path: PathBuf,
    /// `Arc` so the reader can clone the handle out and drop the mutex guard
    /// before blocking. Holding the guard across `read_exact` would deadlock
    /// [`Self::wake`] — the only unblock path — until the peer closed.
    stream: Mutex<Option<Arc<UnixStream>>>,
    stop: AtomicBool,
    /// Reconnect attempts that found no collector (proxies "events lost").
    gaps: AtomicU64,
}

impl EventClient {
    /// Connect, waiting up to `wait` for the collector socket to appear.
    /// A missing collector after the deadline is a loud error: aranicy
    /// cannot run rules without an event source, and silent rule-less
    /// operation is worse than a failed start.
    pub fn connect(path: PathBuf, wait: Duration) -> std::io::Result<Arc<Self>> {
        let deadline = Instant::now() + wait;
        loop {
            match UnixStream::connect(&path) {
                Ok(stream) => {
                    return Ok(Arc::new(Self {
                        path,
                        stream: Mutex::new(Some(Arc::new(stream))),
                        stop: AtomicBool::new(false),
                        gaps: AtomicU64::new(0),
                    }));
                }
                Err(e) if Instant::now() >= deadline => {
                    return Err(std::io::Error::new(
                        e.kind(),
                        format!(
                            "no collector at {} after {:?}: {e} \
                             (is aranicy-collector.service running?)",
                            path.display(),
                            wait
                        ),
                    ));
                }
                Err(_) => std::thread::sleep(Duration::from_millis(50)),
            }
        }
    }

    /// Block for the next exec event; `Ok(None)` only after [`Self::wake`].
    pub fn next_event(&self) -> std::io::Result<Option<ExecFrame>> {
        loop {
            if self.stop.load(Ordering::Acquire) {
                return Ok(None);
            }
            // Clone the handle and release the lock BEFORE blocking: `wake`
            // needs the lock to shut the socket down, and the blocking read
            // only ends once the peer goes away or the socket is shut down.
            let stream = self
                .stream
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .clone();
            if let Some(stream) = stream {
                let mut buf = [0u8; FRAME_LEN];
                let mut reader = &*stream;
                match reader.read_exact(&mut buf) {
                    Ok(()) => return Ok(Some(ExecFrame::decode(&buf))),
                    Err(e) if e.kind() == ErrorKind::Interrupted => continue,
                    Err(_) => {
                        // Collector went away (or this handle was shut down):
                        // drop it and count the gap.
                        *self.stream.lock().unwrap_or_else(PoisonError::into_inner) = None;
                        self.gaps.fetch_add(1, Ordering::Relaxed);
                    }
                }
            }
            if self.stop.load(Ordering::Acquire) {
                return Ok(None);
            }
            match UnixStream::connect(&self.path) {
                Ok(stream) => {
                    *self.stream.lock().unwrap_or_else(PoisonError::into_inner) =
                        Some(Arc::new(stream));
                }
                Err(_) => std::thread::sleep(RECONNECT_DELAY),
            }
        }
    }

    /// Unblock [`Self::next_event`] from another thread (shutdown path).
    /// The handle is cloned out so the blocking reader is never starved of
    /// the lock; `shutdown` makes the pending `read_exact` return.
    pub fn wake(&self) {
        self.stop.store(true, Ordering::Release);
        let stream = self
            .stream
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        if let Some(stream) = stream {
            let _ = stream.shutdown(std::net::Shutdown::Both);
        }
    }

    /// Reconnect attempts that found no collector (surfaced in the summary).
    pub fn dropped_events(&self) -> u64 {
        self.gaps.load(Ordering::Relaxed)
    }
}

impl Drop for EventClient {
    fn drop(&mut self) {
        self.wake();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::net::UnixListener;
    use std::sync::atomic::AtomicU64;
    use std::sync::mpsc;

    struct TestDir(PathBuf);

    impl TestDir {
        fn new(tag: &str) -> Self {
            static SEQ: AtomicU64 = AtomicU64::new(0);
            let base = option_env!("CARGO_TARGET_TMPDIR").map_or_else(
                || {
                    std::env::current_dir()
                        .expect("test cwd")
                        .join("target")
                        .join("event-client-tests")
                },
                PathBuf::from,
            );
            let dir = base.join(format!("{}{}", tag, SEQ.fetch_add(1, Ordering::Relaxed)));
            std::fs::create_dir_all(&dir).expect("create test dir");
            Self(dir)
        }

        fn path(&self) -> &std::path::Path {
            &self.0
        }
    }

    /// The shutdown path must interrupt a reader that is already blocked in
    /// `read_exact`: `wake` needs the stream lock, so the reader may not hold
    /// it while waiting for bytes (that deadlocked the daemon on SIGTERM).
    #[test]
    fn wake_unblocks_a_pending_read() {
        let dir = TestDir::new("wake");
        let path = dir.path().join("events.sock");
        let _listener = UnixListener::bind(&path).expect("bind");

        let client = EventClient::connect(path, Duration::from_millis(200)).expect("connect");
        let (tx, rx) = mpsc::channel();
        let reader = {
            let client = Arc::clone(&client);
            std::thread::spawn(move || {
                let got = client.next_event();
                let _ = tx.send(matches!(got, Ok(None)));
            })
        };

        std::thread::sleep(Duration::from_millis(150));
        let waker = {
            let client = Arc::clone(&client);
            std::thread::spawn(move || client.wake())
        };

        assert!(
            rx.recv_timeout(Duration::from_secs(2)).expect("wake must unblock the reader"),
            "next_event returns Ok(None) after wake"
        );
        waker.join().expect("wake returns");
        reader.join().expect("reader returns");
        let _ = std::fs::remove_dir_all(dir.path());
    }
}

//! Exec-event client for `aranicy-collectord`.
//!
//! The collector owns the exec ring buffer and resolves the match name at
//! capture time (see its `events` module); this client only decodes frames,
//! a batch at a time into a caller-owned [`BatchBuf`].
//! Reconnects with a fixed short backoff and counts the gaps so a collector
//! restart is visible in the summary line instead of silently losing events.

use std::io::{ErrorKind, Read};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use aranicy_wire::BatchBuf;

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

    /// Block until at least one whole frame is in `out`; `Ok(false)` only
    /// after [`Self::wake`].
    ///
    /// The caller owns the buffer, so a burst of execs costs one `read`: a
    /// single `read` admits whatever the collector wrote in one batch, and a
    /// frame split across two reads waits in the buffer's tail. `out` is
    /// reset on entry.
    pub fn next_batch(&self, out: &mut BatchBuf) -> std::io::Result<bool> {
        loop {
            if self.stop.load(Ordering::Acquire) {
                return Ok(false);
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
                out.clear();
                // Read until at least one whole frame arrived. A partial
                // frame costs a second read, never a second decode path.
                let mut alive = true;
                while out.is_empty() && alive {
                    let mut reader = &*stream;
                    match reader.read(out.spare()) {
                        Ok(0) => alive = false,
                        Ok(n) => out.commit(n),
                        Err(e) if e.kind() == ErrorKind::Interrupted => {}
                        Err(_) => alive = false,
                    }
                }
                if !out.is_empty() {
                    return Ok(true);
                }
                // Collector went away (or this handle was shut down): drop it
                // and count the gap. A partial frame here belongs to the dead
                // stream — carrying it into the next connection would shift
                // every later frame by a constant offset.
                out.reset();
                *self.stream.lock().unwrap_or_else(PoisonError::into_inner) = None;
                self.gaps.fetch_add(1, Ordering::Relaxed);
            }
            if self.stop.load(Ordering::Acquire) {
                return Ok(false);
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

    /// Unblock [`Self::next_batch`] from another thread (shutdown path).
    /// The handle is cloned out so the blocking reader is never starved of
    /// the lock; `shutdown` makes the pending `read` return.
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

    /// AF_UNIX paths are capped at 108 bytes, so the tag must stay short:
    /// the base alone is ~72 bytes here.
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
            let dir = base.join(format!(
                "{}{}-{}",
                tag,
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

    /// Read one batch on a helper thread so a truncated stream fails the test
    /// inside two seconds instead of blocking the suite forever.
    fn next_batch_pids(client: &Arc<EventClient>) -> Vec<u32> {
        let (tx, rx) = mpsc::channel();
        let client = Arc::clone(client);
        std::thread::spawn(move || {
            let mut buf = BatchBuf::new();
            let pids = client
                .next_batch(&mut buf)
                .map(|_| buf.frames().map(|f| f.pid()).collect::<Vec<_>>());
            let _ = tx.send(pids);
        });
        rx.recv_timeout(Duration::from_secs(2))
            .expect("a batch must arrive within 2 s")
            .expect("next_batch succeeds")
    }

    /// The shutdown path must interrupt a reader that is already blocked in
    /// `read`: `wake` needs the stream lock, so the reader may not hold it
    /// while waiting for bytes (that deadlocked the daemon on SIGTERM).
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
                let mut buf = BatchBuf::new();
                let got = client.next_batch(&mut buf);
                let _ = tx.send(matches!(got, Ok(false)));
            })
        };

        std::thread::sleep(Duration::from_millis(150));
        let waker = {
            let client = Arc::clone(&client);
            std::thread::spawn(move || client.wake())
        };

        assert!(
            rx.recv_timeout(Duration::from_secs(2)).expect("wake must unblock the reader"),
            "next_batch returns Ok(false) after wake"
        );
        waker.join().expect("wake returns");
        reader.join().expect("reader returns");
        let _ = std::fs::remove_dir_all(dir.path());
    }

    /// One collector batch must cost one read: the client hands back every
    /// frame the socket already holds, in order.
    #[test]
    fn a_written_burst_arrives_as_one_batch() {
        use aranicy_wire::{FrameBuf, COMM_LEN};
        use std::io::Write;

        let dir = TestDir::new("batch");
        let path = dir.path().join("events.sock");
        let listener = UnixListener::bind(&path).expect("bind");
        let client = EventClient::connect(path, Duration::from_millis(200)).expect("connect");
        let (mut collector, _) = listener.accept().expect("accept");

        let mut wire = Vec::new();
        for i in 0..3u32 {
            wire.extend_from_slice(FrameBuf::new(i, [0; COMM_LEN], 0, b"n").as_bytes());
        }
        collector.write_all(&wire).expect("write batch");

        assert_eq!(
            next_batch_pids(&client),
            vec![0, 1, 2],
            "one read admits the whole burst, in order"
        );

        client.wake();
        let _ = std::fs::remove_dir_all(dir.path());
    }

    /// A frame the collector's write split across reads is reassembled from
    /// the buffer tail — no event is lost and none is duplicated.
    #[test]
    fn a_frame_split_across_writes_is_reassembled() {
        use aranicy_wire::{FrameBuf, COMM_LEN};
        use std::io::Write;

        let dir = TestDir::new("split");
        let path = dir.path().join("events.sock");
        let listener = UnixListener::bind(&path).expect("bind");
        let client = EventClient::connect(path, Duration::from_millis(200)).expect("connect");
        let (mut collector, _) = listener.accept().expect("accept");

        let frame = FrameBuf::new(77, [0; COMM_LEN], 0, b"split");
        let bytes = frame.as_bytes();
        collector.write_all(&bytes[..100]).expect("partial write");
        collector.write_all(&bytes[100..]).expect("tail");

        assert_eq!(next_batch_pids(&client), vec![77]);

        client.wake();
        let _ = std::fs::remove_dir_all(dir.path());
    }

    /// A partial frame belongs to the stream it arrived on. After the
    /// collector restarts, the old tail must not be glued in front of the new
    /// stream: the frame format has no sync word, so every later frame would
    /// decode at a fixed wrong offset and the daemon would act on invented
    /// pids and names until it restarted.
    #[test]
    fn a_partial_frame_does_not_survive_a_reconnect() {
        use aranicy_wire::{FrameBuf, COMM_LEN};
        use std::io::Write;

        let dir = TestDir::new("rc");
        let path = dir.path().join("events.sock");
        let listener = UnixListener::bind(&path).expect("bind");
        let client = EventClient::connect(path, Duration::from_millis(200)).expect("connect");
        let (mut first, _) = listener.accept().expect("accept first");

        // The dead stream's truncated frame is a *different* event: if its
        // 100 bytes survive, the new stream's frame decodes as pid 7.
        let doomed = FrameBuf::new(7, [0; COMM_LEN], 0, b"dead-stream");
        first.write_all(&doomed.as_bytes()[..100]).expect("partial");
        drop(first);
        let frame = FrameBuf::new(42, [0; COMM_LEN], 0, b"second-stream");

        let (tx, rx) = mpsc::channel();
        {
            let client = Arc::clone(&client);
            std::thread::spawn(move || {
                let mut buf = BatchBuf::new();
                let pids = client
                    .next_batch(&mut buf)
                    .map(|_| buf.frames().map(|f| f.pid()).collect::<Vec<_>>());
                let _ = tx.send(pids);
            });
        }
        // The client reconnects by itself; hand it a clean stream.
        let (mut second, _) = listener.accept().expect("accept second");
        second.write_all(frame.as_bytes()).expect("whole frame");

        let pids = rx
            .recv_timeout(Duration::from_secs(2))
            .expect("a batch after the reconnect")
            .expect("next_batch succeeds");
        assert_eq!(pids, vec![42], "the new stream decodes from offset 0");

        client.wake();
        let _ = std::fs::remove_dir_all(dir.path());
    }
}

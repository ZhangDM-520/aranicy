//! Kernel integration test: real `tp/sched/sched_process_exec` delivery.
//!
//! Root required (BPF load). Run:
//!   cargo test -p aranicy-events --test integration -- --ignored --nocapture
//! Process exit (and `BpfExecSource::drop`) detaches the tracepoint; nothing
//! is pinned, so no BPF objects outlive the test.
use std::collections::HashSet;
use std::os::unix::fs::PermissionsExt;
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use aranicy_events::{btf_available, BpfExecSource, ExecSource};

const SPAWNS: u32 = 30;

#[test]
#[ignore = "needs root + BTF kernel; run via: cargo test -- --ignored --nocapture"]
fn exec_events_deliver_unique_comm_within_2s() {
    assert!(btf_available(), "kernel BTF missing at /sys/kernel/btf/vmlinux");
    assert_eq!(
        unsafe { libc::geteuid() },
        0,
        "root required to load BPF programs"
    );

    // Unique comm <= 15 bytes (TASK_COMM_LEN minus NUL), backed by a real file:
    // the kernel sets comm from the executed file's basename, not argv[0].
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .subsec_nanos();
    let comm = format!("ait{:x}{:x}", std::process::id() & 0xffff, nanos & 0xffff);
    assert!(comm.len() < 16);

    // Fixtures live under the workspace target dir (never /tmp).
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/aranicy-events-it")
        .join(format!("{}-{}", std::process::id(), nanos));
    std::fs::create_dir_all(&dir).expect("create fixture dir");
    let fixture = dir.join(&comm);
    std::fs::copy("/bin/true", &fixture).expect("copy /bin/true");
    let mut perm = std::fs::metadata(&fixture).unwrap().permissions();
    perm.set_mode(0o755);
    std::fs::set_permissions(&fixture, perm).expect("chmod fixture");

    let source = Arc::new(BpfExecSource::new().expect("BpfExecSource::new"));
    let done = Arc::new(AtomicBool::new(false));
    let watchdog = {
        let source = Arc::clone(&source);
        let done = Arc::clone(&done);
        thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(2);
            while !done.load(Ordering::Relaxed) && Instant::now() < deadline {
                thread::sleep(Duration::from_millis(10));
            }
            source.wake();
        })
    };

    // 30 real execve()s of the uniquely named binary.
    let mut pids = HashSet::new();
    for _ in 0..SPAWNS {
        let mut child = Command::new(&fixture).spawn().expect("spawn fixture");
        pids.insert(child.id());
        let status = child.wait().expect("wait fixture");
        assert!(status.success(), "fixture exited {status:?}");
    }

    // Collect until our 30 events arrive (or the 2 s watchdog ends the stream).
    let start = Instant::now();
    let mut matched = 0u32;
    let mut total_seen = 0u32;
    while let Some(ev) = source.next_event().expect("next_event") {
        total_seen += 1;
        if ev.comm_bytes() == comm.as_bytes() {
            assert!(pids.contains(&ev.pid), "event pid {} not spawned", ev.pid);
            matched += 1;
            if matched >= SPAWNS {
                break;
            }
        }
    }
    let elapsed = start.elapsed();

    done.store(true, Ordering::Relaxed);
    source.wake();
    watchdog.join().expect("watchdog join");
    let dropped = source.dropped_events();
    drop(source); // explicit detach before the leftover check

    println!(
        "INTEGRATION RESULT: matched={matched} total_seen={total_seen} spawns={SPAWNS} elapsed_ms={} dropped={dropped}",
        elapsed.as_millis()
    );

    // Clean before asserting so failed runs do not leave root-owned fixtures.
    std::fs::remove_dir_all(&dir).expect("clean fixture dir");

    assert_eq!(
        matched, SPAWNS,
        "expected {SPAWNS} events for comm {comm:?}, got {matched} (total_seen={total_seen})"
    );
    assert!(
        elapsed < Duration::from_secs(2),
        "events arrived late: {elapsed:?}"
    );
    assert_eq!(dropped, 0, "userspace queue dropped samples");
}

/// `wake()` must unblock a blocked `next_event` promptly (contract: ~<=200 ms).
#[test]
#[ignore = "needs root + BTF kernel; run via: cargo test -- --ignored --nocapture"]
fn wake_unblocks_next_event_promptly() {
    assert_eq!(unsafe { libc::geteuid() }, 0, "root required");
    let source = Arc::new(BpfExecSource::new().expect("BpfExecSource::new"));
    let waker = {
        let source = Arc::clone(&source);
        thread::spawn(move || {
            thread::sleep(Duration::from_millis(50));
            source.wake();
        })
    };
    let start = Instant::now();
    let ev = source.next_event().expect("next_event");
    let latency = start.elapsed();
    println!("WAKE LATENCY: {:?} (ev_present={})", latency, ev.is_some());
    assert!(ev.is_none(), "shutdown must return Ok(None)");
    assert!(
        latency < Duration::from_millis(200),
        "wake latency {latency:?} exceeds 200 ms"
    );
    waker.join().unwrap();
}

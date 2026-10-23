
## 2026-10-22 — eBPF event source (task: ebpf-events)

- Implemented `crates/aranicy-events` + `bpf/` (exec.bpf.c, exec_event.h):
  ringbuf-only sched_process_exec source, embedded via include_bytes! +
  ObjectBuilder::open_memory (libbpf-rs 0.27 has open_memory; no temp file).
- Loader: BpfExecSource {new, dropped_events, is_shutdown} + trait ExecSource;
  wake() = eventfd + AtomicBool, next_event blocks on an epoll set covering
  ringbuf.epoll_fd() + eventfd (measured wake latency ~0.1 ms).
- Verification: 8 unit tests; root integration 30/30 events (matched=30
  total_seen=30 elapsed_ms=0 dropped=0); wake latency 50.1 ms incl. 50 ms
  pre-wake sleep; cargo check --workspace rc=0; mutation probe (decode offset
  shift) turned both unit and integration red, then restored green.
- Leftover audit: 0 progs loaded during test windows; link count 72 = baseline.
- Docs appended here + MEMORY.md; no commits made (per brief).

## 2026-10-22 — daemon + CLI wiring (task: daemon-cli)

- Implemented `crates/aranicy-daemon` end to end: paths/procname/pidfile/
  sdnotify/load/dump/daemon modules + CLI actions start/dump/lint/match-test;
  SIGHUP reload (sigwait thread), delta drift corrector, flock pidfile
  singleton, sd_notify READY/STOPPING.
- Verification: cargo test -p aranicy-daemon 36/36; cargo check --workspace
  rc=0; clippy clean for aranicy-daemon (1 pre-existing warning in
  aranicy-apply, untouched). Root e2e smoke: exec→apply→log line, --reload →
  "reloaded 2 rules", SIGTERM clean shutdown + pidfile removal, abstract
  NOTIFY_SOCKET received READY=1/STOPPING=1, drift re-applies on renice.
- Smoke testing caught and fixed an sd_notify bug: abstract sockaddr length
  double-counted the leading NUL (addrlen off by one) — datagrams silently
  lost. Fix + unit tests added.
- No commits per brief. Note: brief said "only edit crates/aranicy-daemon/**";
  docs/ entries here are the standing global-instruction record, code changes
  are confined to the daemon crate.

## 2026-10-23 — OPT2 batch drain + busy-poll, OPT3 /proc dirfd (tasks: opt2-batch-drain, opt3-proc-dirfd)

- Implemented OPT3: `ProcResolver` (/proc dirfd `O_PATH|O_DIRECTORY|O_CLOEXEC`,
  `openat`/`readlinkat` on `"<pid>/<suffix>"` from stack buffers; absolute-path
  fallback when /proc is unopenable), owned by `daemon::Shared` (opened at
  startup, closed on drop); free `procname` fns delegate to a LazyLock
  resolver (tests/dump unchanged). OPT2: `drain_ring()` greedy consume-all +
  bounded `busy_poll()` before each blocking wait
  (`ANANICY_RS_BUSY_SPIN_US`, clamped ≤1 ms; default 0 after measurement).
- Verification: `cargo test --workspace` 81/81 green (77 baseline + 4 new);
  clippy clean in touched crates (1 pre-existing aranicy-apply warning);
  root integration 2/2 ×3 (30/30 events delivered; wake latency 50.13 ms
  incl. 50 ms pre-wake sleep); spoofed-argv[0] functional proof green twice
  (`nice=7`, `best-effort: prio 3`, `applied rule` log line); strace proof of
  the dirfd path (3 534 dirfd-relative opens, 0 absolute resolver opens).
- Integration-test flake (`wake_unblocks_next_event_promptly`,
  `ev_present=true`) diagnosed and proven pre-existing: pristine stash
  reproduces 3/3 — both tests see system-wide execs under the parallel
  harness. Fixed by serializing test bodies with a shared `TEST_LOCK`;
  assertions untouched.
- A/B per protocol (same session, governor pinned): the mandated 30 µs
  busy-spin REGRESSED cost at the paced 2000/s workload (+157% wall,
  +357% instructions/event, cs/s flat at ~1.0/event) → shipped default 0 µs
  with the knob; deviation from "default ~30 µs" documented in
  docs/BENCHMARKS.md. B(spin=0) vs A within noise at the invariants.
- No commits; awaiting owner's disposition decision.

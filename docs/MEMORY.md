
## eBPF event source (aranicy-events), reviewed 2026-10-22

- libbpf-rs 0.27 `RingBufferBuilder::build()` erases the map-borrow lifetime
  (`RingBuffer<'cb>` keeps only the callback lifetime) while the ring buffer
  still uses the map's fd — owning structs must keep the `Object` field after
  the `RingBuffer` field so drop order stays rb -> obj.
- `bpf_link__detach` fails EOPNOTSUPP for perf-event links produced by
  `Program::attach()` on `tp/...` sections; the detach path is
  `bpf_link__destroy`, i.e. dropping the `Link`.
- `bpf_get_current_comm()` returns 0/-errno, not a length; comm_len must be
  derived by scanning for NUL inside TASK_COMM_LEN.
- Kernel `comm` is the executed file's basename, never argv[0]; tests need a
  real uniquely named file (arg0 tricks do not move comm).
- Leftover-BPF audits must diff against a pre-run baseline: this host already
  had a `handle_exec` tracepoint prog (id 332, loaded 2026-10-22 19:42) with
  links 69/70 from another consumer; grep-zero is not the right oracle.

## Host interference: ananicy-cpp runs alongside tests (found 2026-10-22)

- This host runs `ananicy-cpp.service` (root, check_freq=60, loglevel
  critical). It reverts `renice`/`setpriority` on test processes back to their
  inherited nice ~30–230 ms after the change (verified: flip disappears with
  `systemctl stop ananicy-cpp`). Any nice-stability test must either stop the
  service or expect counter-writes; aranicy's drift corrector visibly
  "misfires" while actually correcting it.
- aranicy `ExecSource::wake()` latches shutdown=true (next_event then returns
  Ok(None) forever) — wake is terminal; it cannot be used to nudge the event
  loop for reloads. Reload must run on the signal thread and swap an
  RwLock<Arc<RuleSet>> instead.
- Linux `/proc/<pid>/stat` field 18 (priority) = nice + 20, not 20 - nice;
  field 19 (index 16 after the comm's last ')') is nice.

## Busy-poll vs arrival distributions (measured 2026-10-23)

- The paced `tools/storm` generator delivers ~uniform arrivals (~1.0
  context-switch per exec event measured). A constant pre-block busy-spin
  therefore burns its whole window per event and collapses no wakeups: a
  30 µs spin measured +357% instructions/event with cs/s unchanged. Spins
  only pay when arrivals cluster within the window (e.g. `make -j` bursts) —
  gate or configure them, never assume "storm" implies microsecond clustering.
- Cross-set wall-% on this host drifts with IPC/core-placement (0.67–0.87 IPC
  observed in one session) and ambient load; instructions/event is the stable
  invariant. Pair A/B captures back-to-back and report per-pair deltas.
- Integration tests over `sched_process_exec` see ALL system execs: parallel
  test-harness runs contaminate each other's no-event windows (wake-latency
  test flaky on pristine code). Serialize such tests with a shared lock.
- `strace` prints fds as bare numbers unless `-y`: dirfd usage shows as
  `openat(12, "NNN/cmdline", ...)` — do not grep for `</proc>` annotations
  when proving `openat`-on-dirfd behavior.

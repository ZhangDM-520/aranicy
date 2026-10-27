
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

## lavd stats protocol pitfalls (learned 2026-10-27, sampler.rs)

- scx_lavd 1.1.4 `stats_meta` reports `SchedSample.fields` as a name ->
  {datum, desc} MAP, not the array of names its docs suggest. Any consumer
  must accept both shapes (map keys / array strings / objects with "name").
- `sched_samples` batches are capped at ~60 samples by the 16 KB ringbuf no
  matter the `nr_samples` budget; drops are silent (mseq contiguous) and the
  batch is hot-task biased — never infer anything from absence of a task.
- `CARGO_TARGET_TMPDIR` is set only when building integration tests, not for
  `#[cfg(test)]` unit tests inside src — and this host forbids /tmp writes
  outright. Test socket/file fixtures need a fallback under the crate's
  `target/` dir (mind the 108-byte AF_UNIX path limit).
- A scripted fake-socket server's reply queue must use one cursor across
  connections; resetting per connection silently replays the first (often the
  failure) entry and wedges reconnect tests in confusing ways.

## Bias-engine control-law lessons (reviewed 2026-10-27)

- "No faster than N steps × interval" rate limits must gate the FIRST step on
  want-onset + interval too; gating only last-step + interval lets a ramp of
  N steps finish in N−1 intervals.
- A shared global step budget needs FIFO-by-want-onset scheduling (winner
  re-arms its want = goes to the back). Lexicographic first-eligible hands
  every slot to the alphabetically first name and starves the rest — a test
  caught it, not review.
- A threshold that is the mean of the same population being classified
  deadbands whenever one name dominates the population; fixtures need a
  stable background population or T drifts to the target's own mean.
- Restart-safe confidence aging anchors decay to max(last state change,
  last_seen) and applies per-call deltas; recomputing "since last_seen" per
  call compounds and over-decays.
- "cargo test green" is not a crate-done gate: the rewrite crates shipped
  clippy-clean-looking tests that `clippy --all-targets` rejects (deny-by-default
  lints only surface there), and manifest-level unused-dependency warnings
  (cargo::unused_dependencies) plus stale workspace deps (declare-only, e.g.
  notify after SIGHUP-only reload) show up nowhere else. Run
  `cargo clippy --workspace --all-targets` before calling a crate done.


# AGENTS.md — global memory (aranicy)

Last reviewed: 2026-10-31 (migrated from `docs/MEMORY.md`; per-entry fact dates preserved below)

## Memory architecture

- **Global memory — this file:** project overview, pitfalls, long-term rules.
- **Active buffer — `.logs/scratchpad.md`:** current task state and working scratch.
- **Historical log — `docs/JOURNAL.md`:** chronological action journal (one-line
  summary pointers to `.logs/action-*.md` summaries).
- `docs/BENCHMARKS.md` and `docs/COMPATIBILITY.md` are ordinary project docs, not memory.
- Single source of truth: never duplicate rules or lists across docs — link with
  explicit Markdown pointers. Stamp entries `Last reviewed: YYYY-MM-DD` and
  re-verify on edit.

## Project overview

aranicy — auto-nice daemon in Rust (ananicy-cpp replacement): eBPF
`sched_process_exec` event source, ananicy-compatible JSONL rules (nice/ionice/
sched/cgroup/cpuset), dynamic per-name nice bias learned from sched_ext per-task
stats (schema-driven `scx_stats` binding), closed-loop verification + drift
correction. Crates: `aranicy-events` (+`bpf/`), `aranicy-daemon`, `aranicy-rules`,
`aranicy-apply`, `aranicy-model`; packaging in `contrib/`. Verify with
`cargo test --workspace` + `cargo clippy --workspace --all-targets` (both must be
green/clean — the done-gate for any code wave).

## Long-term rules

- Never run schedulers or daemons in harness bash sessions (they die with the
  session); services run under systemd only. `scx_lavd` is owner-managed — never
  touch it. (2026-10-30 lesson.)
- Every commit carries the `Co-authored-by: Copilot <223556219+Copilot@users.noreply.github.com>`
  trailer; never commit or push without confirming disposition with the owner.
- Shared worktrees (gsa tree): show full untruncated `git status` first; commit
  with explicit pathspec; never sweep shared journal files you did not author
  end-to-end. (2026-10-28 multi-writer lesson.)
- Falsify new checks (mutation probe / red-first) before trusting a green suite;
  never weaken assertions to pass. Judge runs by target artifacts, not pipe exit codes.
- Journal telemetry is only `bias changed` lines — freeze/rebaseline/classify
  events are silent. Bias state comes from `aranicy dump bias` and
  `/var/lib/aranicy/bias-state`.

## Pitfalls & lessons

## eBPF event source (aranicy-events) — Last reviewed: 2026-10-22

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

## Host interference: external nice-writers fight tests — Last reviewed: 2026-10-31

Historical note: `ananicy-cpp.service` was retired 2026-10-24 (aranicy replaced
it live); the counter-write lesson below survives for any external nice-writer.

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

## Busy-poll vs arrival distributions — Last reviewed: 2026-10-23 (measured)

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

## sched_ext stats-protocol pitfalls (sampler.rs) — Last reviewed: 2026-10-31

Currency: since commit `01710f9` (2026-10-30) the sampler is a schema-driven
`scx_stats` client (socket/target discovery by probing), so the scx_lavd-specific
protocol notes below are historical; the general test/fixture pitfalls remain live.

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

## Bias-engine control-law lessons — Last reviewed: 2026-10-31 (threshold-mean pitfall is live again: the v0.2.3 ramp-starvation defect is exactly this failure mode)

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

## Bias-engine config defaults — Last reviewed: 2026-10-31 (verify against `crates/aranicy-daemon/src/config.rs`; do not re-derive)

- Window 60 s, deadband 15%, min_samples 30, attack 30 s/step, release 300 s/step,
  global step 5 s, bias clamp [−1, +3], confidence half-life 1800 s, max_entries 4096.
- Classification → target: `Bulk` → +3; `Responsive` + share-starved → −1; else 0.
- Polarity reminder: LOW score = bulk (candidate when window mean < threshold_ema × (1 − deadband)).
- Persisted state (`/var/lib/aranicy/bias-state`) keeps name/score_ema/
  sample_count_ewma/class/bias/last_seen/confidence/frozen — NOT target/want
  stamps; a restart re-stamps wants at the first slew pass.

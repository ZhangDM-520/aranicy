
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
  events are silent. A restart that restores history emits a one-off burst
  (each restored entry re-stamps wants on the first slew pass). Bias state comes from `aranicy dump bias` and
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
- The restored cohort stamps all wants at restart: the release side is capped
  (no two release wins in a row while an attack waits) but the cold-start
  attack queue still delays fresh ramps (measured 2026-10-31: 32–45 wanters →
  3.5–8 min/turn vs the 30 s law — the global budget 0.2 steps/s under-serves
  ~1/s aggregate demand; rate laws are hardcoded, no config keys). A
  fresh-state run shows the intended law (0→3 in ~2.5 min).
- A frozen entry whose closed-loop expectation is gone (applied_nice not
  persisted — restart) used to brick forever; it re-baselines on a fresh
  sample. A RELEASED claim (note_applied(None)) must keep the freeze — only
  total absence of a record unwinds it (`claim_recorded` flag).
- "cargo test green" is not a crate-done gate: the rewrite crates shipped
  clippy-clean-looking tests that `clippy --all-targets` rejects (deny-by-default
  lints only surface there), and manifest-level unused-dependency warnings
  (cargo::unused_dependencies) plus stale workspace deps (declare-only, e.g.
  notify after SIGHUP-only reload) show up nowhere else. Run
  `cargo clippy --workspace --all-targets` before calling a crate done.

## Scheduler-agnostic collector (2026-11-07) — Last reviewed: 2026-11-07

- Per-task BPF accounting needs THREE hooks, not one: `sched_switch` alone is
  blind to a task that execs and then runs to completion (a compiler) — it
  never switches out until exit. Add `sched_stat_runtime` (per-tick runtime for
  the running task) and seed at `sched_process_exec` (no switch happens at
  exec). Upstream emits `sched_stat_runtime` from the FAIR class only, so a
  kernel that does not route it through sched_ext produces no ticks at all —
  keep a `sched_switch` residual (`last_tick`/`last_run_start`) as the exact
  fallback rather than assuming the tick exists.
- `static_prio` must be refreshed on every switch/tick, never only at entry
  creation or exec: the bias engine's closed-loop check compares the applied
  nice against the sampled one, so a stale value freezes every entry after its
  first apply (measured live: 76/400 entries frozen).
- Never delete the accumulator in `sched_process_exit`: it fires from `do_exit()`
  BEFORE the final `sched_switch`, so the delete discards the last window and
  the switch re-creates a zeroed entry (a phantom row that drags demand down).
  LRU eviction already bounds the table; pid reuse is caught by the comm check.
- Attribute `wakes_issued` from `sched_waking` (waker's context), not
  `sched_wakeup` (target CPU, so "current" is the occupant, not the waker).
- A full `BPF_MAP_TYPE_HASH` silently drops `BPF_NOEXIST` inserts: new tasks
  get no accounting and vanish from samples (observed at 16384/16384). Use
  `BPF_MAP_TYPE_LRU_HASH` for per-task tables.
- `wait_freq == 0` and `wake_freq == 0` are meaningful zeros (never slept /
  never woke anyone), not absent fields: treating them as "no evidence" blinds
  the classifier to CPU-bound build tools, which are the canonical BULK case
  (a compiler forks its children instead of waking them). Only
  `avg_runtime_wall == 0` is a hard gate.
- Instantaneous window rates are jumpy (one sleep in one window reads as 1/s,
  then 0); smooth them with a wall-clock EWMA (~30 s) seeded from the first
  observation, matching lavd's persistent per-event EWMAs. Decay them for
  every task still present in the map, not only the ones that moved — an idle
  task must not report its pre-idle rate when it wakes up.
- Exec-name resolution must run at CAPTURE time (collector), not in the
  consumer: by the time an event crosses a socket, a short-lived tool's
  `/proc/<pid>` may be gone. The wire frame must carry the full 256-byte
  `NAME_CAP` (and a u16 length): a shorter field truncates names — possibly
  mid-UTF-8, which the daemon drops as unresolved — so exec matching would
  disagree with every other path.
- A blocking read must never hold the mutex that the shutdown path needs:
  clone the `Arc<UnixStream>` out, drop the guard, then `read_exact`. Holding
  it deadlocks `wake()` and freezes SIGTERM handling until systemd SIGKILLs.
- Collector shutdown: block SIGTERM/SIGINT before spawning any thread (threads
  inherit the mask; otherwise a worker takes the default action and the sockets
  leak), poll a non-blocking listener so `accept` cannot outlive `stop`, and
  set a write timeout on the event socket so a stalled client cannot wedge the
  only pump thread.
- IRQ-context wakes: `sched_waking` runs on the interrupted task's context when
  it fires from a timer/device IRQ, so crediting `bpf_get_current_task_btf()`
  invents wakes for whatever was burning that CPU (measured: a pure spinloop
  reported 22-45 wakes/s). Gate on `__preempt_count` via
  `bpf_this_cpu_ptr(&__preempt_count) & 0x00ffff00` (hardirq/softirq/NMI bits).
- The accumulator must track task IDENTITY, not just the pid: refresh `comm` on
  every switch/tick (prctl renames), fully reset the counters on exec (a reused
  pid exec'ing the same name otherwise inherits them and a stale `last_tick`
  back-dates the slice), and delete the child's pid entry in
  `sched_process_fork` so a recycled pid starts from zero.
- `d_run > 0 && d_runs == 0` is the normal shape of a CPU-bound task that never
  switched out: dividing by `runs` reports runtime 0 and the bias engine then
  drops exactly the samples it wants. Fall back to `d_run` / the in-flight
  `now - last_run_start`.
- A pid absent from the baseline was created after it (its entry starts at
  zero), so its cumulative counters are the window's activity — skipping the
  first sighting makes every sub-cadence build helper invisible.
- `score == 0` is a real observation (never-sleeping, never-waking task): a
  `score_ema == 0.0` sentinel would re-seed on the next window instead of
  smoothing. Track `score_init` like `util_init`.
- Draining a 16k-entry map key-by-key costs two `bpf()` calls per task and is a
  measurable share of a core every cadence; `MapCore::lookup_batch` cuts it to
  one call per 1024 entries (fallback kept). Do not busy-poll an event queue
  either: a condvar costs nothing and a 2 ms loop burned ~4% of a core.
- Event-socket bind must happen before the serve thread starts, otherwise a
  bind failure leaves a "healthy" collector with no event stream; and a frame
  popped before a failed write must be pushed back, or a daemon restart eats it.
- makepkg's global `lto` option injects `-flto=auto` into CFLAGS/CXXFLAGS **and
  LDFLAGS**. A vendored static libbpf built with those CFLAGS is an LTO archive,
  and the final Rust link (cc/ccache → `ld.lld`) cannot consume it: every
  `bpf_*` symbol comes out undefined, while the same tree links fine with plain
  `cargo build --release`. The collector therefore declares `libbpf-rs` with
  `default-features = false` and links the distro `libbpf` (one `libbpf`
  dependency instead of a vendored C build). `--no-as-needed` does not help;
  only removing `-flto=auto` or dropping the vendored archive does.

## Architecture invariants (2026-11-08 audit) — Last reviewed: 2026-11-08

- **One sample contract.** `aranicy_wire::Sample` is the only definition of the
  8-field sample (plus `MAX_UTIL`, `NICE_OFFSET`, `SAMPLE_CAP`). The collector
  converts into it; the daemon deserializes it. Never declare a second struct
  with the same field names — the units drifted twice that way.
- **The observation budget is one number, and it must exceed the working set.**
  `SAMPLE_CAP` is used by both sides (the collector cuts, the daemon requests
  exactly it). Measured on this host: an idle window carries ~260 rows, a full
  `cargo build` ~5 600 short-lived processes. 512 hid ~91 % of the evidence
  during builds, 4 096 truncated ~27 %, 8 192 none at ~0.3 % (daemon) / ~0.6 %
  (collector) of a core. The served payload reports `truncated`; the daemon
  warns at most once a minute (warning per *change* fires at the cadence —
  measured 26/min).
- **Policy synthesis lives in `decision`.** `daemon.rs` resolves bytes and
  calls `decision::plan_locked` → `decision::execute`; the drift path adds
  `decision::settle`. Do not re-add rule/bias/eff-nice logic to `daemon.rs` —
  two copies drifted once (drift stopped counting matched/unmatched).
- **Config liveness is a table, not a comment.** `config.rs::CONFIG_KEYS`
  classifies every key as live / reconfigurable / restart-only / accepted-only;
  a field without a row fails `every_config_field_has_one_row`. Reload calls
  `BiasEngine::reconfigure` and names the restart-only keys it ignored.
  `x3d_mode` is accepted and not applied (it warns).
- `bias_state::load` refuses a file whose `version` is newer than the binary
  supports: the format is lenient, so loading then saving would silently
  downgrade it.

## Test runs pollute live history (2026-11-08, measured)

The collector samples every *thread*, and libtest names test threads after the
test path, so `cargo test` on this host creates bias entries whose comm is a
truncated test name (`abi::tests:`, `a_populated_non`) — 11-43 samples each,
classified BULK, applied to nothing. Not a bug, but never read bias history
immediately after a test run, and do not use entry counts as a regression
signal while tests are running.

## Open finding — no-op attack cycle (2026-11-08, unfixed)

An entry with `confidence == 0` and `bias == 0` can still be selected for an
**attack** step (`direction_wants` compares `target` to `bias`): the step sets
`bias = ±1`, then `age()` in the same `bias_loop` iteration zeroes it because
confidence is 0. Net effect: the global step budget is spent, the FIFO
re-arms, and the journal logs `bias changed … bias=0` (a "change" to the same
value) roughly every 11 minutes per such name. Fix belongs with the wave-2
classifier work, not as a drive-by change to the control law.

## Bias-engine config defaults — Last reviewed: 2026-10-31 (verify against `crates/aranicy-daemon/src/config.rs`; do not re-derive)

- Window 60 s, deadband 15%, min_samples 30, attack 30 s/step, release 300 s/step,
  global step 5 s, bias clamp [−1, +3], confidence half-life 1800 s, max_entries 4096.
- Classification → target: `Bulk` → +3; `Responsive` + share-starved → −1; else 0.
- Polarity reminder: LOW score = bulk (candidate when window mean < threshold_ema × (1 − deadband)).
- Persisted state (`/var/lib/aranicy/bias-state`) keeps name/score_ema/
  sample_count_ewma/class/bias/last_seen/confidence/frozen — NOT target/want
  stamps; a restart re-stamps wants at the first slew pass.

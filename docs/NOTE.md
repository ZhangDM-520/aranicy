
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

## 2026-10-27 — lavd stats-sampler client (sampler.rs, sub-agent workstream)

- Implemented `crates/aranicy-daemon/src/sampler.rs`: `spawn()` sampler thread
  (UNIX socket, one JSON line in/out), `SchedSamplerHandle`
  (shutdown/join/state/fields), `SamplerState` Live/Degraded{reason}; one
  tracing line per state transition only, never per batch. `stats_meta` runs on
  every (re)connect (feature detection → `FieldSet`); batch loop sends
  `nr_samples` as string "200" every 500 ms (wall-clock paced, skip-forward),
  sequential round-trips only.
- Robustness: errno!=0 / EOF / ECONNREFUSED / ENOENT / envelope parse failure
  -> close + exponential backoff 1->30 s with ±25% jitter (SplitMix64, no new
  deps) + reconnect + re-meta; "preempted by another sampler" -> Degraded +
  ~7 s slow retry; shutdown() = condvar wake + shutdown(2) on the parked
  stream -> prompt thread join.
- Leniency: unknown fields dropped, int-or-string numbers coerced
  (`coerce_number`), wrong-typed/missing fields fall back to serde(default),
  non-object entries in `samples` skipped individually.
- Live read-only probe (scx_lavd 1.1.4 via sudo python3, stats_meta + one stats
  request): `SchedSample.fields` is a name->{datum,desc} MAP (not the array
  shape the docs suggest — parser accepts both); SchedSample = 38 fields incl.
  cpu_heat/warm_cpu_id unknowns; one batch = 60 samples, 64 ms round-trip.
  Canned test payloads are schema-identical (38-field guard test).
- Tests: 12 new in sampler.rs behind a fake UNIX-socket server with a global
  FIFO scripted reply queue (happy path, meta map+array shapes,
  preempt->Degraded->reconnect->Live, EOF mid-request reconnect, malformed
  JSON tolerated+reconnect, shutdown join <2 s against a hung server, empty
  batch, 60-cap batch, leniency (string numbers/unknowns/defaults), socket
  path fallback, 38-field schema guard). `cargo test -p aranicy-daemon`
  54/54 green, stable over 3 reruns; clippy: 0 findings in sampler.rs;
  `cargo check --workspace` green (warnings only in other owners' files).
- Red-first evidence: 3 reconnect-family tests failed initially — the fake
  server replayed its script per connection, wedging reconnect loops on the
  failure entry; fixed by hoisting the cursor. Suite proven able to fail.
- Files touched (scope): sampler.rs; aranicy-daemon/Cargo.toml (+serde from
  workspace deps — required by the frozen struct's derive); Cargo.lock
  (manifest artifact). bias.rs/bias_state.rs/daemon.rs/main.rs untouched.
  No commits; awaiting owner disposition.

## 2026-10-27 — bias engine (tasks: bias-estimator, bias-controller, bias-state)

- Implemented `crates/aranicy-daemon/src/bias.rs` + `bias_state.rs` per the
  self-contained brief (plan.md 4.2–4.3): weight-free estimator
  (S0 = ilog2(wait_ft·wake_ft) + ilog2(runtime_ft), score = S0²; zero fields
  ⇒ sample skipped), T_us = 60 s EWMA of the population mean score (brief's
  self-consistent scale, NOT plan's de-biased avg_lat_cri), 2-window
  hysteresis (0.85/1.15 T, ≥30 scored samples/window, deadband holds class),
  share-starved gate (cpu_util_ewma < 0.5·demand_ewma, demand > 64, −1 only),
  asymmetric slew (attack 1 step/60 s from want-onset, release 1/300 s toward
  0, global 1 step/5 s FIFO-shared, clamp [−1,+3]), aging (half-life decay
  while unseen, one release step per 0.25 confidence drop, zero-confidence ⇒
  bias 0), LRU cap, verify_applied freeze + fresh-sample re-baseline,
  serde_json atomic save/load with tolerant Ok(None) load.
- Verification: `cargo test -p aranicy-daemon` 68/68 green (17 new: 14 bias +
  3 bias_state); clippy clean for both files. Red-first discipline: the
  starved-gate test caught a real global-budget fairness bug (lexicographic
  first-eligible starved later names; fixed by FIFO-by-want-onset, winner
  re-arms want); mutation probes red — streak 2→1 fails both hysteresis
  tests, score+=static_prio fails actuator-independence + score fixtures.
- Files touched (scope): bias.rs, bias_state.rs only. Cargo.toml serde line
  is the sampler workstream's (duplicate-key race resolved in their favor;
  net-zero from this pass). No commits; awaiting owner disposition.

## 2026-10-28 (dump bias + clippy sweep + validation dispatch)

- `dump bias` CLI implemented (main.rs value_parser + dump.rs
  bias_json/bias_json_at: config → bias_state::load → {path,note,state,error},
  tolerant null-state). 2 new tests; CLI smoke: absent state → null; real
  state via --config override → full entries JSON. 70 daemon tests green.
- Clippy sweep (gates were failing pre-existing): dropped tautological
  `assert!(BUSY_SPIN_DEFAULT_US <= BUSY_SPIN_MAX_US)` (absurd_extreme_comparisons
  = deny), ActionOutcome Default → derive, removed unused deps (serde,
  thiserror in aranicy-rules; notify in workspace — reload is SIGHUP-based).
  `cargo clippy --workspace --all-targets` clean; `cargo test --workspace`
  112 passed.
- docs/COMPATIBILITY.md: added "Dynamic nice bias (superset)" section (incl.
  lavd-absent semantics: keeps applying / stops updating / aging-capped
  history), dump bias in CLI list, bias_* keys pointer.
- bias-validation dispatched (background agent, live scx_lavd phases A/B +
  storm A/B). scx_lavd runs manually: `sudo scx_lavd --autopilot
  --per-cpu-dsq --enable-cpu-bw` (no systemd unit). No commits.
- README corrected: comm-prescreen claim was stale (locked decision: no
  prescreen — `exec -a` spoofing; cmdline read + O(1) lookup is the hot path);
  added dynamic-nice bias paragraph, `dump bias` example, test count 112.
- State correction: GitHub repo ZhangDM-520/aranicy EXISTS (public, tag
  v0.1.0, pushed 2026-10-24); gsa recipe packages/stable/aranicy already
  landed 2026-10-23 (proven makepkg build) — the "repo not created / recipe
  pending" checkpoint notes were stale.

## 2026-10-28 (audit pass 1: code-review -> fix)

- code-review agent: 8 findings on the uncommitted change set; ALL verified
  at their claimed sites before fixing (high: nice-less rules force-reniced
  at bias 0; closed-loop expectation re-derived in the wrong name namespace;
  verification of never-adjusted comms + rebaseline-every-batch pinning;
  medium: inverted bias clamps panic i8::clamp; absent static_prio reads as
  nice -20 and freezes everything; low: corrupt state silently dropped;
  "bias changed" logged ~120x per move; 3 wrong COMPATIBILITY statements).
- Fixes (all with red-first mutation probes on the new tests): eff_nice_for
  predicate (claims nice only for rule-nice / active bias / owned-name
  moves — relaxation back to base now reaches running processes); closed
  loop verifies against the nice WE applied per comm (note_applied/
  applied_nice), skips never-touched comms and static_prio==0; freeze now
  clears only on a matching sample and aging still flushes pinned bias;
  config warns+resets inverted bias clamps and BiasEngine::new normalizes;
  corrupt bias state surfaces as Err(InvalidData)+warning; change-summary
  marks are consumed once; COMPATIBILITY/README corrected (COMM keying,
  verification source, /proc stat field numbering: field 18 = nice+20,
  field 19 = nice).
- Result: 117 workspace tests green, clippy --workspace --all-targets clean.
- Perf audit (code side): net-new per exec event = 2 uncontended mutex
  round-trips + 3 O(1) map ops, zero allocations; Arc-only Policy clone when
  bias moves nice. No fixable hot-path finding. Measured A/B verdict pending
  the live validation agent (its binary predates these fixes — post-fix
  re-run required before shipping numbers).

## 2026-10-28 (live validation returned: PRE-FIX vs POST-FIX)

- R3 (lavd absent: bias keeps applying, learning stops) PASS both runs; R4
  (aging caps history: 30s half-life decay + stepwise relax + zero flush)
  PASS both runs (scope: full flush needs ~15 min — release_step_s=300).
- R1 substance PASS (learning/classes/bounds/confidence); "wake -> -1" was a
  brief miscalibration: responsive target is 0 unless share-starved (locked
  design) and starved+responsive is unreachable in tested mixes — documented
  scope, gate kept. comm-vs-argv0 keying split (samples carry only comm) =
  locked design; documented scope.
- R2 substance PASS (nice always == clamp(base+bias); pre-fix wedge GONE).
  Transient freeze = lavd static_prio view-lag 30-60s behind apply; designed
  conservative path, self-clearing (captured: evidence/postfix/bias-A-5.json
  frozen -> A-7 cleared). Accepted; no grace window. Pre-fix per-batch
  freeze/rebaseline churn (confidence pinned 0.25) confirmed the reviewed
  bug and its fix.
- Storm matrix confounded by brief-mandated log_applied_rule=true (shipping
  default is false — config.rs:58); ~179k per-exec "applied rule" lines for
  the synthesized storm name dominate instr/event (79-83k vs 28-29k).
  Clean gate round dispatched (log off, fresh state, 3+ captures/condition).
  One report handle was misattributed (90k rule="true" -> actually 179,589
  rule="gsa-storm-probe"); counts re-verified by grep before use.

## 2026-10-28 (final delta review — direct, per delegation policy)

- Background reviewer was rejected by content filter; the fix delta (~150
  lines) fits a single direct read, so the final review ran directly over
  the 7 priority interactions: claim/release vs drift early-continue
  (sound; release-on-exec of at-rest names is safe-direction), note_applied
  existing-entry gate (safe direction), verify->record_batch->age ordering
  with rebaseline(last_now) (dt=0 in the live order; zero-timestamp garbage
  flushes harmlessly), new()/restore clamp normalization (consistent),
  corrupt-file callers (warn + fresh; dump surfaces error), eff_nice_for
  truth table (all 8 cases as designed).
- One real finding (low): take_changed_recently still window-filtered stale
  marks, contradicting its "each move logs exactly once" contract — relax
  moves during a stream outage were silently dropped. Fixed (consume-only,
  no window), test extended for the stale-mark case, mutation probe red on
  the named test, 75 daemon tests green.
- Verdict: fix delta shippable; pending only the clean storm gate numbers.

## 2026-10-28 (shipped: a34eaf8)

- Clean gate round (quiet config log_applied_rule=false, fresh state,
  3 captures/condition): storm gates PASS both variants both conditions
  (unruled 3.88/3.07, ruled 4.93/4.75 median wall-% at 2000 execs/s, <=5%);
  instr/event 35.19k vs 34.75k medians = +1.3% within noise (the earlier
  one-capture +21% did not reproduce); 115-entry populated state +2.2%
  single capture (no map-size cliff); sampler 0.15% one core. Count
  attribution corrected with exact greps (179,589 rule="gsa-storm-probe").
- Final validation: 117 workspace tests, clippy clean, secret-pattern scan
  clean, inventory exactly 16 modified + 3 new files.
- Committed a34eaf8 (19 files, +3510/-37) with Co-authored-by trailer and
  PUSHED to ZhangDM-520/aranicy main (7b0dcc1..a34eaf8). Host left healthy:
  aranicy.service active/enabled, scx_lavd original args, governors
  restored, toys/kthread residue cleaned. Not done (not requested): tag /
  gsa recipe bump to a post-bias release.

# Wave 2 — three deepening designs (2026-11-08)

Design only. No source file was modified for this document.

**Tree state.** All citations were taken at 2026-11-08T23:35 +08:00 on a
*working tree in flux*: line numbers moved repeatedly while this was written
(the `aranicy-wire::Sample` extraction, the observation-budget `truncated`
field, and a new `decision.rs` that already owns the apply-side `Plan` are all
uncommitted on top of `0af25dd`; `bias_loop` moved from `daemon.rs:316` to
`:248` mid-session). Every anchor below was re-checked immediately before
writing; the appendix has the one-liner to re-anchor if they drift again.

**Vocabulary.** module / interface / depth / deep-shallow / seam / adapter /
leverage / locality per the codebase-design glossary; domain terms from
`CONTEXT.md`. A *deep* module hides a protocol its caller would otherwise have
to perform correctly; a *shallow* one makes the caller hold invariants it
cannot see.

| # | Candidate | Seam | Surface | Order |
|---|---|---|---|---|
| 6 | Retire the discovery adapter | `Link` + `Endpoint` (one adapter, one target) | −≈350 src, −10 tests, +3 tests | **1st** |
| 4 | One ingest interface | `ingest` / `tick` | 11 methods → 9, 3 ordering rules deleted | **2nd** |
| 7 | Calibrate the classifier | absolute boundary behind `classify` | `T` loses its boundary role, stays as canary | **3rd** |

---

# Candidate 6 — Retire the discovery adapter

## 6.1 What the module is now

`spawn_collector` is the whole interface the daemon uses
(`crates/aranicy-daemon/src/daemon.rs:267-271`). Behind it sit **two**
adapters, not one:

1. the real one — our collector, which serves exactly one struct
   (`collector/src/stats_server.rs:26-49`) under exactly one target
   (`collector/src/stats_server.rs:99-111`);
2. the *test* one — `FakeServer` (`sampler.rs:1157-1275`), a real AF_UNIX
   listener replaying canned payloads so discovery can be exercised against
   `scx_lavd`/`scx_mavd` shapes.

So the discovery machinery is adapter-shaped but adapter-less: multi-scheduler
binding (`find_task_struct` `:782`, `target_candidates` `:832`, `snake_case`
`:846`, `probe_target` `:890`, `rows_key` `:868`, `bind_handshake` `:543`,
`FieldSet` `:165-193`, `SIGNAL_FIELDS` `:96-107`, `PROBE_TARGETS` `:108`) for a
world that no longer exists. `docs/COMPATIBILITY.md:109` already states the
target design — *"no scheduler is probed or named"* — so the code contradicts
its own contract.

`#![allow(dead_code)]` (`sampler.rs:48`) hides the rot: `spawn()` (`:253`) is a
test-only duplicate of `spawn_collector` (`:258`), and `FieldSet`'s only
production consumer is one log field (`binding.fields.len()`, `:645`).

## 6.2 Decision

**(c) both.** Delete discovery outright (a) *and* keep a `Transport`/`Endpoint`
seam (b): the deletion removes surface, the seam makes the remaining ~180 lines
of reconnect/backoff/pacing testable without AF_UNIX, sleeps or 5-second
timeouts. They are independent, so the migration deletes first (no behaviour
change, tests still real-socket) and seams second (behaviour-neutral, tests
become deterministic).

**Preemption is already dead code.** The string `"preempted by another
sampler"` exists only in `sched-ext/scx` `scheds/rust/scx_lavd/src/stats.rs`
and `scheds/experimental/scx_mavd/src/stats.rs` (code search on
`sched-ext/scx`); `scx_stats` 1.1.4 — the crate our collector links, and the
server implementation it uses — contains no such logic
(`grep -rn preempt ~/.cargo/registry/src/*/scx_stats-1.1.4/src/` → no match).
Against the collector, `Fail::Preempted` (`sampler.rs:454-458`),
`BindOutcome::Preempted` (`:509-517`), `REASON_PREEMPTED` (`:86`) and
`PREEMPT_RETRY` (`:112`) are unreachable.

## 6.3 The seam

```rust
pub(crate) enum Fail {
    /// I/O, EOF, timeout, malformed JSON, or a server-side `errno != 0`.
    Transport(String),
}

/// One sequential request/response on a stats socket.
pub(crate) trait Link: Send {
    fn round_trip(&mut self, req: &Value) -> Result<Value, Fail>;
    /// Wake a blocked read (shutdown path); registered with the handle.
    fn abort_handle(&self) -> Option<Arc<dyn Abortable>>;
}

pub(crate) trait Abortable: Send + Sync { fn abort(&self); }

/// How the loop obtains a fresh link. The only thing tests substitute.
pub(crate) trait Endpoint: Send + Sync + 'static {
    fn open(&self) -> Result<Box<dyn Link>, Fail>;
}
```

Behind it: `UnixLink` is today's `Connection` body (`sampler.rs:460-507`) —
`Arc<UnixStream>` + `BufReader` + read timeout; `abort_handle()` returns the
stream as `dyn Abortable` (`shutdown(Both)`), so `SchedSamplerHandle::shutdown`
(`:220-227`) keeps working unchanged. `UnixEndpoint { path }` is
`connect` + timeouts. `Script` (test-only) pops a connection script and a
replay script, and counts connections — ~60 lines replacing ~120.

`run()` (`sampler.rs:567`) becomes generic over `Box<dyn Endpoint>` and keeps
*all* the state machine: backoff (`:421-449`), jitter, pacing (`:729-735`),
`StopFlag` waits (`:354-396`).

Fixed binding replaces the handshake wholesale:

```rust
/// The collector is the only server: one target, one row key.
const STATS_TARGET: &str = "sched_samples";
const ROWS_KEY: &str = "samples";
```

`Live` now means "the first `stats` round trip parsed", not "the handshake
succeeded": one round trip saved per reconnect, and a broken schema is still
loud — it just arrives on the first cadence tick.

## 6.4 Interface sketch

```rust
pub fn spawn_collector(socket: PathBuf, cadence: Duration, tx: Sender<Batch>) -> SchedSamplerHandle;

impl SchedSamplerHandle {
    pub fn state(&self) -> SamplerState;
    pub fn shutdown(&self);
    pub fn join(self) -> std::thread::Result<()>;
    // `fields()` deleted: FieldSet no longer exists.
}

pub enum SamplerState { Live, Degraded { reason: String } }
// reason ∈ {"starting", "transport down"}
// ("preempted…" and "no per-task stats" deleted)
```

## 6.5 What moves / deletes

| Region | Lines | Disposition |
|---|---|---|
| `REASON_PREEMPTED`, `REASON_NO_PER_TASK`, `PREEMPT_RETRY`, `Tuning.preempt_retry` | `:86`, `:90`, `:112-113`, `:271`, `:310` | delete |
| `SIGNAL_FIELDS`, `PROBE_TARGETS` | `:96-107`, `:108` | delete (13) |
| `FieldSet` + impl | `:165-193` | delete (29) |
| `TargetBinding` | `:317-325` | delete (9); `Shared.binding` → `Option<&'static str>` |
| `Connection` | `:460-507` | **keep** as `UnixLink` |
| `BindOutcome`, `map_fail`, `try_bind` | `:509-541` | delete (32); `Endpoint::open` replaces them |
| `bind_handshake` | `:543-565` | delete (23) |
| bind loop + outcome match | `:577-640` | collapse to ~15 (−50) |
| preempt arms | `:685-691`, `:901` | delete (~20) |
| `check_errno` + `resp_message` | `:751-776` | shrink to ~8 (−18) |
| `find_task_struct`, `target_candidates`, `snake_case`, `rows_key`, `probe_target` | `:782-911` | delete (130) |
| `canonical_field` | `:954-956` | keep — still the schema-drift hook |
| `spawn()` | `:253` | delete (duplicate of `spawn_collector`) |

**≈350 lines deleted, ≈70 added** out of 1960: the file loses ~14 % of its
source, but the *reasoning surface a maintainer must hold* drops much further —
"which schema shapes does this bind" ceases to be a question.

## 6.6 Test disposition (21 tests today)

**Disappear (10)** — machinery that ceases to exist: `live_shaped_payloads_have_38_fields`
(`:1338`, asserts *lavd's* 38-field schema; our collector serves 8),
`stats_meta_field_detection` (`:1399`), `stats_meta_array_shape_parses`
(`:1427`), `mavd_schema_binds_and_delivers` (`:1704`),
`renamed_struct_binds_via_derived_target` (`:1729`),
`unknown_targets_fall_through_to_probe_list` (`:1761`),
`aggregate_only_reports_no_per_task` (`:1790`, reason string gone),
`find_task_struct_gates_on_identity_and_signals` (`:1857`),
`rows_key_rejects_aggregate_arrays` (`:1894`),
`target_candidates_prefer_struct_derived_names` (`:1909`).

**Become transport-scripted (8)** — same behaviour, no socket, no sleep:
`happy_path_batches_delivered` (`:1348`, also kept once as a real socket),
`eof_mid_request_reconnects` (`:1481`),
`malformed_json_line_tolerated_and_reconnects` (`:1506`),
`empty_batch_delivered` (`:1550`), `batch_at_ringbuf_cap` (`:1571`),
`lenient_samples_string_numbers_unknowns_defaults` (`:1594`),
`truncated_is_read_from_the_envelope` (`:1625`), and
`preempted_degraded_then_reconnect_live` (`:1447`) rewritten as
`transport_failure_degraded_then_reconnect_live` — the degraded→live
transition is still a contract, the preemption cause is not.
`empty_samples_rows_still_bind` (`:1832`) disappears with the probe it guards.

**Stay real-socket (2)** — they prove kernel/libc behaviour a fake cannot:
`happy_path_batches_delivered` (AF_UNIX framing, `\n` round trips, lenient parse
end to end) and `shutdown_unblocks_unresponsive_server` (`:1531`): `shutdown(2)`
wakes a thread blocked in `read_line`, the only guard on "SIGTERM is honoured
while the collector is wedged".

**New (3)** — pure gain from the seam, all untested today:

1. `backoff_doubles_to_cap` — 1, 2, 4, 8, 16, 30, 30 s, jitter within ±25 %.
2. `pacing_skips_missed_slots` — a slow round trip must not cause a catch-up
   burst (`:729-735`).
3. `consumer_drop_exits_promptly` — `tx.send` fails → thread ends, link
   aborted, slot cleared (today covered only incidentally by `Drop`).

## 6.7 Migration steps (each independently green)

1. **Seam, no deletion.** Add the traits; `Connection` → `UnixLink`,
   `Connection::open` → `UnixEndpoint::open`; `run()` takes `Box<dyn Endpoint>`;
   `try_bind` still calls `round_trip` for the handshake. All 21 tests
   unchanged. *Behaviour identical.*
2. **Delete discovery.** Replace `bind_handshake` with the two constants;
   delete `:96-193`, `:509-565`, `:577-640`, `:782-911`. Delete the 10 tests
   above. `Live` moves after the first `stats`. Green: 11 tests.
3. **Delete preemption.** Drop `Fail::Preempted`, `BindOutcome`, `map_fail`,
   `REASON_PREEMPTED`, `PREEMPT_RETRY` plumbing; rewrite the preempt test as a
   transport test. Green.
4. **Script the loop.** Implement `Script`; convert the 8; keep the 2 socket
   tests; add the 3 new ones. Green, and ~8 s faster.
5. **Delete the residue.** `spawn()`, `fields()`, `FieldSet` mentions in the
   module docs (`sampler.rs:1-47`), then remove `#![allow(dead_code)]` (`:48`)
   and let the compiler prove nothing else is dead. Green.

## 6.8 Capability ledger

**Not lost — worth stating**, because `sampler.rs:6` ("verified live against
`scx_lavd` 1.1.4") implies otherwise: lavd and mavd serve the *same* target name
our collector uses (`sched_samples` is the first `PROBE_TARGETS` entry and the
first struct-derived candidate for `SchedSample`, `:832-844`), so binding the
name literally keeps them working if anyone points `bias_collector_socket` at a
scheduler socket.

**Genuinely lost:**

* *Aggregate-only schedulers* (`scx_bpfland`, `scx_flow`, `scx_p2dq`) used to
  degrade with `no per-task stats`; now they degrade with `transport down`.
  Mitigation: map a server-side `errno` to a distinct reason — 4 lines, no
  discovery.
* *A renamed schema/target* no longer binds. Nothing in this repo produces one.
* *Field availability reporting* (`FieldSet`, `fields()`). This matters for #7:
  with new optional wire fields, "the collector is too old" becomes invisible
  until classification silently stops. Cheaper mitigation than the machinery
  being deleted: a one-shot *data* check on the first batch (`runs == 0 &&
  sleeps == 0` for every row → warn once), not a schema check.

**Already lost, now made true:** preemption. Two concurrent samplers against
our collector are serialized by `scx_stats`, not reported.

## 6.9 Risks

* `Live` now requires a successful *stats* round trip, so a server that accepts
  but always errors stays `Degraded{transport down}` — same observable state,
  and the `detail` string is now the only diagnostic. Keep `detail`.
* The stable reason set shrinks from four strings to two. `SamplerState`'s docs
  (`:133-141`) claim they are a contract; `docs/COMPATIBILITY.md` does not list
  them. Update the enum doc in the same commit.
* No `stats_meta` means a newer collector serving fewer fields is undetectable
  until the data check fires — accept, daemon and collector ship together.

---

# Candidate 4 — Bias engine: one ingest interface

## 4.1 The protocol the caller must know today

`bias_loop` (`daemon.rs:248-289`) is 40 lines of protocol that nothing in
`bias.rs` enforces:

```rust
for s in &batch { engine.verify_applied(&s.comm, s.static_prio); }  // :261-263
engine.record_batch(&batch, now);                                   // :264
engine.age(now);                                                    // :265
for (name, bias) in engine.take_changed_recently() { info!(..) }    // :266-268
```

Four invariants live only in the caller:

1. **Verification precedes ingestion.** `record_batch` skips frozen entries
   (`bias.rs:546-553`), so a re-baselining row must be verified *before* the
   batch is absorbed, or the first matching window is silently dropped.
2. **Aging follows ingestion**, never precedes it.
3. **A timeout ages with no evidence** (`daemon.rs:279-284`) — a *different* slew mode:
   `age()` passes `allow_attack = false` (`:579-604`), `record_batch(&[])`
   passes `true` (`:532`, `:576`).
4. **Changes must be drained**, or the `bias changed` line never fires.

Invariant 3 is a trap the test suite already walked into: the helper
`fn tick(eng, t) { eng.record_batch(&[], ts(t)) }` (`bias.rs:895-897`) is named
"tick" but implements the *attacking* pass — it is not what the daemon's
timeout does. Two verbs with opposite semantics, one of them unnamed. A fifth,
cheaper leak: the timeout path never drains changes, so a relax step taken
while the collector is down is logged at the next batch — or never.

## 4.2 The seam

| today | after |
|---|---|
| `verify_applied` + `record_batch` | `ingest(&[Sample], now) -> Vec<Change>` |
| `age` | `tick(now) -> Vec<Change>` |
| `take_changed_recently` | return value of the two above |
| `target_nice`, `applied_nice`, `note_applied` | unchanged |
| `snapshot` / `restore` | unchanged (contract tightened, §4.5) |

The caller can no longer *not* verify before ingesting (`verify_applied` stops
being public), and can no longer pick the wrong slew mode (`tick` is the only
no-evidence verb).

**Not in scope: the apply path.** The ownership rule (`eff_nice_for`) has
already left `daemon.rs` for `decision::plan` / `decision::settle`
(`decision.rs:138-235`), which calls `target_nice`/`applied_nice`
(`decision.rs:164`) and records the claim in `execute` (`decision.rs:355`).
That is the right home — an engine that knew about rule niceness would have to
know about rules — so candidate 4 must *not* invent a `BiasEngine::plan`. It
only removes the ingest-side protocol.

## 4.3 Interface sketch

```rust
impl BiasEngine {
    /// One delivered window. Verifies the closed loop against every row,
    /// absorbs the evidence, then advances the clock. A row that re-baselines
    /// a frozen entry counts as evidence in *this* batch.
    pub fn ingest(&mut self, batch: &[Sample], now: SystemTime) -> Vec<Change>;

    /// Time passed with no evidence (collector down, or an idle cadence):
    /// confidence decays, bias relaxes toward 0, nothing attacks.
    pub fn tick(&mut self, now: SystemTime) -> Vec<Change>;

    /// Record the outcome of applying the plan's nice; `None` = the apply did
    /// not take (Denied/Failed/Gone), so the claim is explicitly released.
    /// Called by `decision::execute`, exactly as today.
    pub fn note_applied(&mut self, name: &str, nice: Option<i8>);

    pub fn snapshot(&self) -> BiasSnapshot;
    pub fn restore(&mut self, snapshot: BiasSnapshot);
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Change {
    pub name: String,
    pub prev: i8,
    pub bias: i8,
    pub class: BiasClass,
    pub cause: Cause,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cause {
    /// A window closed with classifiable evidence.
    Evidence,
    /// A relax step toward 0 (aging or deadband).
    Relax,
    /// Zero confidence flushed the bias.
    Flush,
}
```

The daemon's `Ok` arm becomes two statements:

```rust
let changes = { let mut e = shared.bias.lock()...; e.ingest(&batch, now) };
for c in &changes {
    tracing::info!(name = %c.name, bias = c.bias, prev = c.prev,
                   class = ?c.class, cause = ?c.cause, "bias changed");
}
```

The guard now drops before logging (today an explicit `drop(engine)`,
`daemon.rs:269`), so the mutex is never held across a write.

**Correction (implemented 2026-11-09).** The sketch above is wrong for the
batch arm: batches arrive every 500 ms (`bias_cadence_ms`) while `recv_timeout`
is 1 s, so `tick` would never run on a healthy stream and wall-clock aging
would stop — measured live, a vanished name held `confidence 0.994599`
unchanged across 145 s. The batch arm therefore runs `bias_pass` =
`ingest` then `tick` at the same timestamp, exactly the pre-seam
`record_batch` → `age` sequence; `tick` remains the only no-evidence verb (it
never attacks) and the timeout arm calls it alone. Post-fix the same cohort
decayed at 0.9772 per 60 s (one half-life law). Two consequences: aging is not
optional per iteration, and a no-op attack now shows both its `Evidence` and
its `Flush` line.

## 4.4 What `Change` contains, and the journal contract

Today the line is `tracing::info!(name, bias, "bias changed")`
(`daemon.rs:266-268`). Keep those two fields first and untouched — every
existing grep (`name="…" bias=N`) still matches — and append:

```
name="build" bias=3 prev=2 class=Bulk cause=Evidence bias changed
```

`prev` and `cause` make the journal diagnosable: today a line cannot
distinguish "evidence said bulk" from "relaxed because the process vanished" —
exactly the distinction the v0.2.3 ramp-starvation investigation needed and
could not read. **No new event name**, so `AGENTS.md`'s "journal telemetry is
only `bias changed` lines" stays literally true. One honest consequence: `tick`
now reports relax steps taken during downtime (today deferred or lost), so the
line *rate* rises when a stream dies — say so in the commit.

## 4.5 Transient vs durable — expressed by the type, not a comment

Today the contract is a 23-line hand-written projection (`bias.rs:681-703`)
against an 8-field snapshot (`bias_state.rs:36-51`) and a 24-field entry
(`bias.rs:191-238`). Adding a durable field and forgetting the projection is
silent data loss — not hypothetical: `threshold_ema` is persisted,
`relax_watermark` is not, and nothing in the code says which is which.

```rust
/// Exactly what survives a restart. Adding a field here persists it; there is
/// no second list to update.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Durable {
    pub score_ema: f64,
    pub sample_count_ewma: f64,
    pub class: BiasClass,
    pub bias: i8,
    pub last_seen: u64,
    pub confidence: f64,
    pub frozen: bool,
}

/// Never persisted. Rebuilt only by `Live::restore`, which is the single
/// statement of the transient rules.
struct Live {
    applied_nice: Option<i8>,   // deliberately NOT persisted: a restart cannot
    claim_recorded: bool,       // verify anything (bias.rs:640-651)
    target: i8,
    aged_at: u64,
    relax_watermark: f64,
    cpu_util_ewma: f64, demand_ewma: f64, util_init: bool, score_init: bool,
    starved: bool,
    // window accumulators, hysteresis and slew stamps: exactly as today
}

struct Entry { durable: Durable, live: Live }

impl Live {
    fn restore(d: &Durable, cfg: &BiasConfig) -> Self {
        Live {
            applied_nice: None,
            claim_recorded: false,
            target: target_for(cfg, d.class, false), // starved is rebuilt
            aged_at: d.last_seen,        // aging resumes from the last sighting
            relax_watermark: d.confidence,
            score_init: true,            // a persisted score is real
            ..Self::default()
        }
    }
}
```

`EntrySnapshot` keeps its exact on-disk shape by flattening:

```rust
pub struct EntrySnapshot { pub name: String, #[serde(flatten)] pub durable: Durable }
```

so `snapshot()` collapses to
`entries.iter().map(|(n, e)| EntrySnapshot { name: n.clone(), durable: e.durable.clone() })`
and `restore()` to `Entry { durable: es.durable, live: Live::restore(&d, cfg) }`.
`Live` has no `Deserialize`, so "transient state loaded from disk" is a compile
error rather than a review question.

## 4.6 Surface ledger

`impl BiasEngine`: 11 public methods → 9. One new type (`Change`) and one enum
(`Cause`) replace a `Vec<(String, i8)>`. Net: the caller's model goes from a
4-step ordered protocol to *ingest or tick*, with the moves arriving as data
instead of as a separate drain call.

## 4.7 Test migration (31 tests in `bias.rs`, 4 in `bias_state.rs`)

| Group | Tests | Migration |
|---|---|---|
| Pure helpers | `score_fixtures_and_skip`, `normalize_name_…`, `inverted_clamps_…`, `attack_step_default_…` | unchanged |
| Apply path | `decision::plan`'s own tests (`decision.rs:599-801`) | untouched — the ownership rule already lives there |
| Closed loop | `verify_skips_…` (`:1322`), `verify_mismatch_freezes_…` (`:1347`), `persistent_mismatch_…` (`:1386`), `frozen_with_no_actuation_…` (`:1416`), `frozen_entry_with_released_claim_…` (`:1443`), `rebaseline_preserves_want_onset_…` (`:1628`), verify half of `bias_group_pools_…` (`:1534`) | pass a one-row batch with the wanted `static_prio` to `ingest`. `verify_skips_…`'s "unknown names are ignored" half moves to a `#[cfg(test)]` unit test of the internal row check, because ingesting *creates* the entry. |
| Window / class | `hysteresis_bulk_…` (`:1080`), `hysteresis_responsive_…` (`:1103`), `decisive_evidence_…` (`:1575`), `non_decisive_…` (`:1594`), `sparse_active_windows_…` (`:1682`), `unscored_active_windows_…` (`:1707`), `zero_score_compiler_batch_is_bulk` (`:1734`), `borderline_flips_…` (`:1649`), `actuator_independence` (`:1042`) | mechanical `record_batch(b, ts(t))` → `ingest`; helpers unchanged |
| Slew / budget | `slew_attack_and_release` (`:1124`), `global_step_budget_…` (`:1167`), `release_flood_…` (`:1190`) | mechanical |
| Aging | `aging_halves_…` (`:1259`), `zero_confidence_…` (`:1288`), `entry_cap_…` (`:1301`) | `age` → `tick`; **new** `tick_reports_relax_moves` (red-first: fails today) |
| Starved | `starved_gate_only_low_minus_one` (`:1233`), `starved_gate_honours_custom_thresholds` (`:1766`) | mechanical |
| Changes | `changed_recently_logs_each_move_exactly_once` (`:1494`) | assert on the returned `Vec<Change>`, including `cause` |
| Persistence | `snapshot_restore_round_trip_…` (`:1795`), 4 in `bias_state` | extend: a snapshot taken mid-ramp round-trips `bias` but **not** the claim, the want stamps or the window accumulators |

The helper `fn tick` (`:895`) is deleted; each call site becomes
`ingest(&[], ts(t))` (evidence-closing) or `tick(ts(t))` (no-evidence) — the
two are now visibly different everywhere, which is the point.
Estimated churn: ~80 call-site renames, 6 non-mechanical rewrites, 3 new tests.

## 4.8 Migration steps (each independently green)

1. **Add `ingest`** as a facade (verify loop → `record_batch` → drain); keep
   `record_batch`/`age`/`verify_applied` public. Switch `bias_loop`'s `Ok` arm.
   Tests untouched → 31 green. `Change` starts as `(String, i8)` + `Evidence`.
2. **Add `tick`** = `age` + drain; switch the timeout arm; add
   `tick_reports_relax_moves` (red-first). Both arms now log identically.
3. **Privatize** `record_batch`, `age`, `verify_applied`,
   `take_changed_recently`; migrate the 31 tests. Green — this is the step that
   proves the interface is sufficient: if a test cannot be expressed, the
   interface is wrong.
4. **Split `Entry`** into `Durable` + `Live`, flatten `EntrySnapshot`, replace
   the hand projection. Green; the round-trip test gains the transient-state
   assertion.

Only step-ordering constraint: 3 must follow 1–2, or the suite is red for the
duration of the migration.

## 4.9 Risks

* **Ordering regression (highest).** Folding verification into ingestion must
  preserve *verify-before-absorb*. Red-first test:
  `ingest_rebaselines_and_counts_the_same_batch` — freeze an entry, ingest one
  batch whose `static_prio` matches, assert class/confidence moved. Verifying
  after absorbing fails it by exactly one window.
* **Timestamp improvement, not preservation.** `verify_applied` uses
  `self.last_now` (`bias.rs:642-648`), which today is the *previous* batch's
  time (the daemon verifies before `record_batch` sets it). Folding makes
  re-baseline timestamps exact; `aged_at` moves by ≤ cadence (500 ms).
  Harmless, but state it in the commit rather than letting a bisect find it.
* `#[serde(flatten)]` + `#[serde(default)]`: pin the on-disk format with a
  literal v1 fixture (`{"name":…,"score_ema":…}`), not a round-trip test.
* `Vec<Change>` is the right container: `ingest` can report **at most one**
  move per call (the global step budget buys one step per pass,
  `bias.rs:809-845`), but `tick` can report a whole cohort when zero
  confidence flushes (or when a restored cohort relaxes together). A
  `SmallVec` would be wrong for exactly that case.

---

# Candidate 7 — Calibrate the classifier

## 7.1 What is actually measured (one correction to the brief)

The brief reads cake's ratio test as a *classifier*. In `scx` v1.1.4 it is a
**starvation** predicate; the class boundary is a single absolute duration:

* `cake_burst_ns = se.sum_exec_runtime / (nvcsw | 1)` — mean runtime per
  voluntary yield (`scheds/rust/scx_cake/src/bpf/cake.bpf.c:570-573`).
* `cake_stage` — `sum_exec_runtime >= SEAT_BURST_MIN_NS * nvcsw`, i.e. **mean
  burst ≥ 64 µs** (`cake.bpf.c:587-594`; `intf.h:75`:
  `SEAT_BURST_MIN_NS = 64 * NSEC_PER_USEC`). *This* is the class boundary:
  "stage" (long, contended seat) vs "hop".
* `cake_starved` — `run_delay * nvcsw > sum_exec_runtime * pcount`
  (`cake.bpf.c:597-605`): mean runqueue wait per voluntary switch exceeds mean
  run per dispatch. A *share-starvation* test; `cake_starved_turn`
  (`:608-620`) is its own-slice-margin variant.
* `cake_handoff_max_ns = 1464` (`cake.bpf.c:567`, loader-probed) with
  `cake_subhandoff` (`:578-583`) is the "don't pay relocation for a sub-handoff
  task" floor — the role `scx_flash`'s `wakeup_throttle` plays for wake credit.

That distinction matters because **our `starved` gate is already the better
version of cake's ratio**: `demand > min_demand && delivered < ratio × demand`
(`bias.rs:368-370`) compares two duty cycles on one normalized scale, not two
cumulative-time ratios that need a pre-shift to avoid overflow. The starvation
half of the brief is already done; it is not where the population bet lives.

The population bet is entirely here:

```rust
let low = th * (1.0 - d);   // th = T = population EWMA of the mean score
let high = th * (1.0 + d);  // bias.rs:391-392; T updated at :749-762
```

and `T` is what one dominant name redefines for everyone (`CONTEXT.md:55-58`
says so explicitly).

## 7.2 Evidence from this host's persisted state

Replayed `/var/lib/aranicy/bias-state` (v1, 2824 entries, `T = 1259.63`;
565 `bulk` / 255 `responsive` / 2004 `unknown`; 368 nonzero bias; 45 frozen):

| Population | n | p10 | p50 | p90 | min | max |
|---|---|---|---|---|---|---|
| `bulk` | 565 | 173 | 424 | 1009 | 25 | 2204 |
| `responsive` | 255 | 939 | 1675 | 2209 | 222 | 2500 |

The distributions **overlap around T's low edge**: 42/565 bulk entries sit
above `0.85·T = 1071`, and 91/255 responsive entries sit below
`1.15·T = 1449`. On this population the boundary runs through both classes.

Restricted to entries with `confidence > 0.5` (n = 163 — evidence strong enough
to have classified twice) there is a clean gap:

| Class | n | p10 | p50 | p90 |
|---|---|---|---|---|
| `bulk` | 84 | 198 | 583 | 938 |
| `responsive` | 79 | 1410 | 1886 | 2209 |

An **absolute** anchor at 1185 with the existing ±15 % deadband (band
1007–1363): bulk at or above 1363 → **0**; responsive at or below 1007 → **0**.
Best hard threshold without a deadband: 1184.7 with 3 errors (1.8 %), and those
3 points fall inside the deadband — so **0 hard violations**.

**Honest caveat, load-bearing for the migration:** those 163 labels were
*produced by the population classifier*. This shows an absolute boundary is
**compatible** with everything we have learned, not that it is **better**.
Superiority can only be shown against labels that are not circular (§7.7).

Scale stability: with `score = s0²` and
`s0 = ilog2(wait_ft·wake_ft) + ilog2(1e9/rt)`, halving every runtime adds 1 to
`s0`, so at `s0 ≈ 34` the score moves `(35²−34²)/34² ≈ +6 %` — inside the 15 %
deadband. The score is therefore *approximately* hardware-invariant, which is
what makes a constant anchor defensible.

## 7.3 Options

| | Design | Diff | New fields | Validatable today | Verdict |
|---|---|---|---|---|---|
| **A** | Absolute score anchor: `T` → constant; deadband/streak/decisive unchanged | ~30 lines | none | **yes** (§7.2) | **do first** |
| **B** | Two-axis absolute box in log space: `lean = log2(burst/64 µs) − log2(rate/100 Hz)` | ~120 lines | `runs`, `sleeps` | no — needs shadow data | after A, gated on §7.7 |
| **C** | Cake's `run_delay·nvcsw` ratio replaces the starved gate | — | `runnable_ns`, `sleeps` | — | **reject**: our demand/delivered gate is the same idea in better units, and C adds a second starvation notion |

Rejected too: "keep `T` and add a guard rail" — two boundaries, no deletion.

## 7.4 Chosen design

**7a (ship now):** one knob replaces one statistic.

```rust
pub struct BiasConfig {
    /// Absolute class boundary in score units (see `sample_score`). Replaces
    /// the population EWMA `T`. 1185 measured on 163 high-confidence entries of
    /// this host's history (§7.2); the deadband and ×2 decisive bands apply to
    /// it unchanged.
    pub class_threshold: f64,   // default 1185.0
    pub deadband_pct: u64,      // unchanged, 15
    // window_s, min_samples, half-life and the slew laws: unchanged
}
```

`close_window` reads `cfg.class_threshold` instead of `self.threshold`. The
population EWMA keeps running (`bias.rs:749-762`) but becomes **diagnostic**:
still computed, still persisted in `threshold_ema`, still shown by `dump bias`,
and now a *canary* — warn when the population mean drifts outside [0.5, 2]×
the anchor (a kernel/hardware change that would silently re-classify
everything). That is the one real weakness of an absolute boundary and it is
cheap to watch. The rest of the state machine is untouched: two-window streaks,
`min_samples`, decisive-on-one-window at `min_samples/4` (`bias.rs:405-411`),
deadband-hold, absence-is-not-evidence, slew laws.

**7b (behind a shadow flag, after A):** a squared sum cannot express "long
bursts **and** low interaction rate" independently, so replace `sample_score`'s
shape:

```rust
/// Weight-free lean: positive = throughput (Bulk), negative = latency
/// (Responsive). Both axes absolute, so no population enters.
fn lean(s: &Sample) -> Option<f64> {
    if s.avg_runtime_wall == 0 { return None; }                  // never ran
    let burst = (s.avg_runtime_wall as f64).max(BURST_FLOOR_NS); // 1_464, cake handoff
    let rate  = (s.wait_freq as f64).max(HOP_MIN_HZ);            // 1.0
    Some((burst / STAGE_MIN_NS).log2() - (rate / HOP_MIN_HZ).log2()) // 64_000 / 100
}
```

`lean` is dimensionless — no host calibration; the deadband becomes
`|lean| < log2(1.15)` and decisive `|lean| ≥ 2·log2(1.15)`. `wake_freq`
**leaves the classifier** (flash needed a throttle because wake credit is
gameable, `main.bpf.c:393-403`; bpfland documents stale `wakeup_freq` on a
continuously running task, `main.bpf.c:704-715`); it stays on the wire as a
diagnostic.

Open question only shadow data can answer: a browser render thread (5 ms bursts
at 60 Hz) reads `lean ≈ +7` → Bulk → +3 nice, while lavd's `lat_cri` would call
it responsive. Which is wrong is a *policy* question, not a formula question —
hence the gate in §7.7.

## 7.5 Where the seam is

`sample_score` (`bias.rs:82`) stays a free function of one `Sample`, and the
*decision* collapses into one pure function so that offline replay and live
classification run the same code:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lean { Bulk, Responsive }

/// `None` = inside the deadband or no evidence. Pure; the only place the
/// boundary is expressed.
pub fn classify(s: &Sample, b: &Boundary) -> Option<Lean>;

pub struct Boundary { pub stage_min_ns: u64, pub hop_min_hz: u64, pub deadband_pct: u64 }
```

`Entry::close_window` (`bias.rs:342-460`) then only accumulates means and calls
`classify`. No threshold arithmetic inside the state machine — which is what
makes the offline replay a loop over recorded rows instead of a
reimplementation.

## 7.6 Fields the collector must publish

| Field | Today | Needed by | Verdict |
|---|---|---|---|
| `avg_runtime_wall` (mean ns per schedule) | yes (`aranicy_wire/src/lib.rs:41-59`) | 7a, 7b (burst) | keep |
| `wait_freq` (sleeps/s) | yes | 7b (rate) | keep |
| `task_util_est`, `cpu_util_wall` | yes | starved gate (already absolute) | keep |
| `static_prio` | yes | closed loop only — never evidence | keep |
| `runs`, `sleeps` (per-window counts) | **no** — computed at `collector/src/agg.rs:168-169` then discarded | 7b: exact per-window burst, explicit "no dispatch" instead of a smoothed rate | **add** (2 × u32, additive) |
| `run_delay`, `pcount` (`sched_info`) | no | nothing | **do not add** |

Why not `sched_info`:

* `sched_stat_runtime` does **not** carry delay — its signature is
  `(struct task_struct *tsk, u64 runtime, u64 vruntime)`, visible in our own
  program (`collector/bpf/collector.bpf.c:188-190`). There is no `run_delay` to
  be had from it.
* `p->sched_info.run_delay` / `.pcount` *are* CO-RE-reachable — `scx_cake`
  reads exactly that from a BPF program (`cake.bpf.c:600-601`) — and on this
  host `CONFIG_SCHED_INFO=y` with `sched_info_on()` compiled to
  `IS_ENABLED(CONFIG_SCHED_INFO)`
  (`/usr/src/linux-zeus/include/linux/sched/stat.h:25`), i.e. unconditional.
  (`kernel.sched_schedstats` is 0 here, but that knob only gates
  `CONFIG_SCHEDSTATS`' `schedstat_*` accounting, not `sched_info`.)
* We do not need them: the collector already accumulates `runnable_ns`, `runs`
  and `sleeps` (`collector/bpf/collector.bpf.c:62-77`) — the same quantities,
  and reset at exec (`:213-227`), which `sched_info` does **not** do.

## 7.7 Offline validation, in the right order

**Stage 0 (today, no capture).** `tools/replay-bias` (or a `#[test]` over a
committed fixture) loads a `BiasSnapshot` and re-derives, per entry, whether
`score_ema` is inside or outside the candidate band. This is what §7.2 did by
hand; make it a *gate*: 0 hard violations on the high-confidence cohort before
7a ships, re-run against the live `/var/lib/aranicy/bias-state` on flip day.

**Stage 1 (7b only).** The snapshot cannot validate 7b: it stores only
`score_ema`, the raw counters are gone. Two options, in preference order:

1. **Shadow mode** (`bias_shadow = true`): run both classifiers on the same
   window state, actuate the old one, and log **only on disagreement** plus a
   10-minute summary:
   `bias shadow name="cc1plus" old=Bulk new=Bulk lean=20.5 burst_ns=900000000 rate=0`.
   Bounded volume, no new file format, and it measures the decision, not the
   score.
2. If volume proves hard to bound: a `bias_trace_path` JSONL of raw
   name-windows (sums, not samples) for 24 h, replayed offline.

**Flip gate:** over ≥ 1000 classified name-windows, disagreements must be
≤ the violations the current boundary shows on this host (18 % of bulk, 36 % of
responsive at the edges, §7.2) *and* the high-confidence cohort must classify
identically. If the render-thread case dominates the disagreements, that is a
knob (`hop_min_hz`), not a formula change.

## 7.8 Migration

* **7a.1** Add `class_threshold` to `BiasConfig` + config parsing
  (`bias_class_threshold`, near `config.rs:41-43`), default 1185.
  `close_window` reads it; `threshold_ema` is persisted unchanged. Green — the
  31 tests pass **only if** test configs set `class_threshold: 400.0` (the
  fixtures' synthetic population, `bias.rs:912-926`); that is the whole
  migration, and `noise_batch` becomes optional rather than mandatory.
* **7a.2** Remove `threshold` from classification; add the drift canary
  (`warn!` at most hourly when `threshold_ema / class_threshold ∉ [0.5, 2]`).
* **7a.3** Add `tools/replay-bias` + a checked-in snapshot excerpt as a
  `#[test]` (the live file is root-owned). Green, and it gates 7b too.
* **7b.1** Add `runs`/`sleeps` to `aranicy_wire::Sample` and
  `collector/src/stats_server.rs`; `agg.rs` already has the deltas. Old
  daemon/new collector and new daemon/old collector both parse (serde default).
* **7b.2** Add `lean`/`classify` + `bias_shadow`, default off.
* **7b.3** Flip the default after the gate; delete `sample_score` and the
  population EWMA's classification role in one commit (the canary stays).

## 7.9 What breaks

* **`BiasSnapshot`:** 7a — nothing (the field stays, loses authority). 7b — if
  the classifier state becomes `lean_ema`, the entry grows; additive with
  `#[serde(default)]`, v1 files load and entries re-seed on their first window
  (the path `score_init` already covers, `bias.rs:220` / `:295`). Bump
  `BIAS_STATE_VERSION` 1 → 2 as a marker only; the loader ignores it
  (`bias_state.rs:15-18`).
* **`bias changed` journal:** unchanged in name and in its first two fields;
  7b may append `class`/`lean`. Nothing in `docs/COMPATIBILITY.md` promises the
  field set.
* **Tests:** 7a — `score_fixtures_and_skip` gains absolute assertions,
  `noise_batch` disappears from ~10 fixtures (it existed only to set `T`),
  `hysteresis_*`/`decisive_*`/`non_decisive_*` keep their semantics via
  `class_threshold: 400.0`. 7b — fixtures need realistic raw fields, but the
  current `bulk_batch` burst (5 ms, `bias.rs:931-935`) is 78× the 64 µs anchor
  and reads Bulk under both laws, so it is already compatible.
* **Docs:** `CONTEXT.md:52-58` and `docs/COMPATIBILITY.md:129-141` become
  contradictory statements — see the last section.
* **Tuning surface:** `bias_class_threshold` (and 7b's `hop_min_hz`) are new
  keys. `AGENTS.md`'s "rate laws are hardcoded, no config keys" stays true of
  the *slew* laws; a class boundary is not a rate law.

## 7.10 Risks

* **No adaptation.** An absolute boundary cannot absorb a population-wide
  shift (all bursts double after a kernel change). The canary is advisory:
  shout, do not auto-relax — silent adaptation is what we are deleting.
* **Circular validation** (§7.2): never quote 1.8 % as accuracy. It is a
  compatibility result.
* **7b's policy disagreement** (render threads) is the real risk; it is why 7b
  is gated on shadow data and 7a is not.
* The 64 µs anchor is cake's *seat* threshold for a CPU-placement decision; our
  nice bias is a gentler actuator. If it proves too aggressive, the knob is
  `stage_min_ns`, not the formula.

---

# Ordering

**6 → 4 → 7.**

1. **#6 shrinks the surface #4 must keep.** `FieldSet` is reachable from
   `SchedSamplerHandle` — a bias-adjacent type #4 would otherwise have to thread
   through its review. Deleting it first leaves #4's question ("what does the
   engine know about its source?") with one answer: `&[Sample]`.
2. **#6 makes #7's harness cheap.** #7 needs many deterministic multi-window
   runs (shadow, replay, drift canary). With `Script`, a 60-window scenario is
   a `Vec<Reply>` and 3 ms; without it, 60 s of real cadence or a bespoke fake
   socket.
3. **#4 before #7: migrate the fixtures once.** #7 rewrites every classifier
   fixture; doing it on `record_batch` and again on `ingest` pays the churn
   twice, and after #4 the new durable fields slot into `Durable` in one line
   instead of into a hand-written projection.
4. **#7 last because it is the only behavioural change.** #6 and #4 must be
   invisible at runtime; #7 moves classification for real workloads and needs
   the validation gate. Last also means a bisect lands on exactly one commit.

Counter-argument considered and rejected: doing #7 first for user value. Its
only offline validator today is circular, so it needs the shadow harness —
which #6 pays for.

---

# Contradictions this design would introduce

1. **`CONTEXT.md:52-58` — "score … population-relative on purpose" and "T —
   the population EWMA … the class boundary is `T × (1 ± deadband)`".** #7
   removes the boundary role of `T` and redefines score/lean. `CONTEXT.md` is
   the *glossary*, so it must be edited in the same commit, not later: `T` and
   *score* currently encode the exact bet being removed. Proposed: `T` →
   "population score mean — a canary, not a boundary"; `score`/`lean` →
   "absolute, weight-free class signal".
2. **`docs/COMPATIBILITY.md:129-141`** — "Classification: per-sample score …
   against a population threshold (EWMA of the population mean score). BULK
   below and RESPONSIVE above the `bias_deadband_pct` band". Same rewrite; the
   deadband, streak and decisive sentences survive verbatim.
3. **`docs/COMPATIBILITY.md:141`** — "decisive evidence (beyond 2× the deadband
   — past the actuator-swing envelope)". The envelope was measured against
   *self-induced* movement of `T`; an absolute anchor has none, so the
   *reasoning* is stale even though the rule is kept.
4. **`docs/COMPATIBILITY.md:95-99`** — "`scx_lavd`/`scx_mavd`, any other
   `sched_ext` scheduler, and plain EEVDF all behave identically". True of the
   *collector*; after #6 a reader may infer the daemon binds any scheduler. It
   cannot (only servers answering target `sched_samples` with a `samples` row
   array). One clarifying sentence, no behaviour change.
5. **`sampler.rs:6`** — "verified live against `scx_lavd` 1.1.4 and the
   byte-identical `scx_mavd` schema": after #6 that is a statement about the
   *wire*, not about a bind. Reword; do not delete (it explains the field
   names).
6. **`AGENTS.md` "journal telemetry is only `bias changed` lines"** — still true
   by name; the *rate* rises when a stream dies (relax steps now reported by
   `tick`). Append one clause so a future reader does not read the new lines as
   a leak.
7. **`AGENTS.md` "rate laws are hardcoded, no config keys"** — 7a/7b add
   `bias_class_threshold` / `bias_hop_hz`. Not rate laws, but the note is
   blanket enough to mislead; scope it to "slew rate laws".
8. **No contradiction with `CONTEXT.md:72` (starved)** — the design
   deliberately leaves it alone (§7.1), which is why cake's ratio test is *not*
   imported.

---

# Appendix — reproduce the evidence

```sh
# Tree state and the anchors this document cites
git status --porcelain | wc -l && git log --oneline -1
grep -n "pub fn record_batch\|pub fn age\|pub fn verify_applied\|pub fn note_applied\|\
pub fn take_changed_recently\|pub fn snapshot\|pub fn restore\|pub fn target_nice" \
  crates/aranicy-daemon/src/bias.rs
grep -n "fn find_task_struct\|fn probe_target\|fn bind_handshake\|struct FieldSet\|fn rows_key" \
  crates/aranicy-daemon/src/sampler.rs

# §7.2 distribution and threshold sweep (root-readable, no mutation)
python3 - <<'EOF'
import json, statistics as st
E = json.load(open('/var/lib/aranicy/bias-state'))['entries']
hi = [e for e in E if e['confidence'] > 0.5 and e['class'] in ('bulk','responsive')]
b = sorted(e['score_ema'] for e in hi if e['class']=='bulk')
r = sorted(e['score_ema'] for e in hi if e['class']=='responsive')
q = lambda v,p: v[min(len(v)-1, int(p*(len(v)-1)))]
print(len(E), 'entries;', len(b), 'bulk;', len(r), 'responsive (conf>0.5)')
print('bulk p10/p50/p90', q(b,.1), st.median(b), q(b,.9))
print('resp p10/p50/p90', q(r,.1), st.median(r), q(r,.9))
for T in (1185.0, 1259.63):
    lo, hi_ = T*0.85, T*1.15
    print(T, 'band', round(lo), round(hi_),
          'bulk>=hi', sum(x>=hi_ for x in b), 'resp<=lo', sum(x<=lo for x in r))
EOF

# §6.2 preemption exists only in the schedulers, not in scx_stats
gh api "search/code?q=%22preempted+by+another+sampler%22+repo:sched-ext/scx" --jq '.items[].path'
grep -rn preempt ~/.cargo/registry/src/*/scx_stats-1.1.4/src/ | head

# §7.1 cake's actual boundary (v1.1.4)
gh api repos/sched-ext/scx/contents/scheds/rust/scx_cake/src/bpf/intf.h?ref=v1.1.4 \
  --jq .content | base64 -d | grep -n "SEAT_BURST_MIN_NS\|CAKE_RATIO_SHIFT"

# §7.6 sched_info is compile-time gated (this host: y)
grep -n "sched_info_on" /usr/src/linux-zeus/include/linux/sched/stat.h
```

**Not verified in this pass:** the `scx_stats` server's behaviour for two
concurrent samplers against our collector (assumed serialized, no preemption
message — the string's absence from the crate is the evidence, not a live
two-client test); `pcount`'s increment site in the 7.4 kernel (`kernel/sched/`
is absent from `/usr/src/linux-zeus`, so the `sched_info` citation rests on the
header's gating alone); and whether an `errno`-answering server can be told from
a dead socket cheaply (§6.8) without keeping `check_errno`.

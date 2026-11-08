# Compatibility contract (superset of ananicy-cpp)

Everything below is behavior/data-format re-implementation. No code is copied
from GPL sources (ananicy-cpp GPL-3.0, JohnRTitor/ananicy-rs GPL-3.0, original
Ananicy GPL-3.0) — implementations here are clean-room from this contract.

## Filesystem surface

- Rules root `$ANANICY_RS_CONFDIR`, falling back to `$ANANICY_CPP_CONFDIR`,
  default `/etc/ananicy.d`. Scanned recursively.
- File kinds: `*.rules` (program rules), `*.types` (type bundles),
  `*.cgroups` (cgroup definitions), gated by `rule_load`/`type_load`/
  `cgroup_load`.
- Daemon config `$ANANICY_RS_CONF` → `$ANANICY_CPP_CONF` →
  `<rules-root>/ananicy.conf`; `key = value` lines, `#` comments.

## Rule format

One JSON object per line. Blank lines and `#` comments skipped. Text from the
first `{` through the last `}` on the line is parsed (trailing garbage ignored,
ananicy-cpp parity); CRLF tolerated. Bad lines warn and are skipped.

| field | type | semantics |
|---|---|---|
| `name` | string | match key, exact, `basename(argv[0])` (exe fallback) |
| `name_regex` | string | unanchored regex, tried only when no exact name matches; first in load order wins; compiled once at load |
| `cmdlines` | [string] | accepted, ignored (lint note) |
| `type` | string | type bundle reference; rule fields override; explicit `null` suppresses |
| `nice` | −20…19 | `setpriority` per task |
| `latency_nice` | −20…19 | `sched_setattr`; falls back to `nice`; kernel-gated |
| `sched` | `fifo\|rr\|normal\|other\|batch\|idle` | `sched_setscheduler` |
| `rtprio` | 0…99 | RT priority for fifo/rr, default 1 |
| `ioclass` | `best-effort\|realtime\|idle\|none` | `ioprio_set` |
| `ionice` | 0…7 | level for best-effort/realtime |
| `oom_score_adj` | −1000…1000 | `/proc/<pid>/oom_score_adj` |
| `cgroup` | string | move into named cgroup (created from `*.cgroups`) |
| `cpuset` | string | `0-3,8-11` or aliases `big-cores`, `little-cores`, `performance-cores`, `efficiency-cores`, `all-cores`, `llc-N`, `node-N`. `turbo-cores` and `x3d-cache`/`x3d-frequency` **parse** (lint parity) but are **rejected at apply time**: no kernel exposes a turbo-core marker, and 3D V-Cache mode is host-controlled |
| `autogroup` | int | **superset**: writes the process autogroup nice (ananicy-cpp only reports) |

Cgroup definitions: `{"cgroup": name, "CPUQuota": N}` — N = percent of *all*
CPUs (v2 `cpu.max`, period 1 s). Duplicates: later-loaded `name` replaces
outright. Load order: sorted file paths (deterministic superset over
ananicy-cpp's OS order).

## Matching and apply

1. Exact `name` lookup (case-sensitive) wins outright.
2. Else first matching `name_regex` in load order.
3. Apply order (parity): nice → latency_nice → sched+rtprio → ioclass+ionice →
   oom_score_adj → cgroup → cpuset. Gates from `ananicy.conf` apply.

## Dynamic nice bias (superset)

Optional dynamic nice adjustment driven by per-task samples from
**`aranicy-collectord`** — a standalone, scheduler-agnostic BPF collector
shipped as its own package (`aranicy-collector`, GPL-2.0-only). aranicy is a
pure client: it loads no BPF, owns no scheduler dependency, and therefore
learns on *every* scheduler, including none (plain EEVDF). Disable with
`bias_enable = false`.

Sockets (`/run/aranicy/`, systemd `RuntimeDirectory`):
`collector-stats.sock` (scx_stats protocol, target `sched_samples`) and
`collector-events.sock` (framed exec events). Paths are configurable via
`bias_collector_socket` / `collector_events_socket`; `collector_wait_ms`
(default 5000) is the startup readiness budget — a missing collector is a
loud failure, never silent rule-less operation.

**Sample contract** — defined once, as the Rust type
`aranicy_wire::Sample` (`crates/aranicy-wire/src/lib.rs`) and its doc
comments; the wire names are unchanged (`pid`, `comm`, `static_prio`,
`wait_freq`, `wake_freq`, `avg_runtime_wall`, `task_util_est`,
`cpu_util_wall`). Both util fields share the `aranicy_wire::MAX_UTIL` scale,
so the starved gate
(`demand > bias_starved_min_demand && delivered < bias_starved_ratio * demand`)
compares like with like; lavd emitted demand in 0..1024 and delivered in
0..100, which made the comparison meaningless. A `wait_freq` of zero is a
*meaningful* zero (the task never sleeps — the canonical bulk case), not a
missing field.

**Observation budget** — `aranicy_wire::SAMPLE_CAP` (8192) bounds the rows one
window may carry. It is one constant shared by producer and consumer: the
collector cuts to it and the daemon requests exactly it. Rows are ordered by
demand (`task_util_est`), then delivered (`cpu_util_wall`), so a starved but
responsive task survives the cut. The `sched_samples` payload reports
`truncated` (rows dropped by the budget, `#[serde(default)]`) and the daemon
logs it at `warn!`, at most once a minute.

The size is measured, not guessed: an idle window carries ~260 rows, while a
full `cargo build` on a 24-thread host reaches ~5 600 short-lived processes.
The former 512 budget therefore hid ~91 % of the evidence exactly during the
bulk workload the bias exists for; 4 096 still truncated ~27 %. Serving 8 192
rows at 2 Hz costs ~0.3 % (daemon) and ~0.6 % (collector) of a core during a
full rebuild, with no truncation observed.

PELT is deliberately unused: `se.avg.*` is a FAIR-class path and reads zero
under sched_ext (measured). The collector accounts runtime from
`sched_stat_runtime` plus a `sched_switch` residual (upstream emits that
tracepoint from the fair class only, so a kernel that does not route it
through the core path yields no ticks — the switch path keeps the figure
exact either way), and runnable time from `sched_switch`. Hot-path cost is one
extra O(1) map lookup per event — the bias history is a separate table from
the rule tables.

**Scheduler support**: every scheduler. The collector derives the signals from
core tracepoints (`sched_switch`, `sched_waking`/`sched_wakeup`,
`sched_stat_runtime`, `sched_process_exec`), so per-task learning no longer
depends on what the scheduler exposes — `scx_lavd`/`scx_mavd`, any other
`sched_ext` scheduler, and plain EEVDF all behave identically.

Runtime dependencies: the daemon needs only glibc/gcc-libs; the collector links
the distro `libbpf` (plus clang-built BPF objects at build time). It ships
`libbpf`-version-independent behaviour because every kernel interaction goes
through libbpf's stable API.

The sampler speaks the `scx_stats` protocol to whatever server is bound at
`sched_collector_socket` (default the collector's
`/run/aranicy/collector-stats.sock`) and reconnects with backoff if it goes
away; no scheduler is probed or named.

- History keyed by the **kernel comm** (the `scx_lavd` sample namespace; rule
  matching uses the separate argv[0]-derived name), with fuzzy keying: a
  `bias_group.<tag> = <regex>` family pools every matching comm into one entry
  named `<tag>` (a build's cc1plus→as→ld→lto1 handoff learns as one stream),
  otherwise per-instance suffixes (`kworker/u97:10` → `kworker/u97`) and one
  trailing version run (`clang-19` → `clang`) collapse. Entries hold class
  (`bulk`/`responsive`/`unknown`), integer `bias`, evidence `confidence` 0..1.
- Applied nice = `clamp(rule_nice + bias, -20, 19)`; unruled names synthesize a
  policy when bias ≠ 0. Live check: `/proc/<pid>/stat` field 19 (`nice`) must
  equal that value (field 18 is `nice + 20`).
- Closed loop: applied values are re-checked against the nice observed in the
  lavd sample stream (`static_prio` there = nice + 20), against the value this
  daemon actually applied — never against re-derived rules. A mismatch freezes
  the entry (no updates) until a matching sample re-baselines it; entries the
  daemon never adjusted are not verified. A frozen entry with no actuation on
  record at all (after a restart — the expectation is not persisted) re-baselines
  on a fresh sample; an explicitly released claim (`note_applied(name, None)`:
  the apply did not take) keeps the freeze.
- Classification: per-sample score from weight-free sched stats against a
  population threshold (EWMA of the population mean score). BULK below and
  RESPONSIVE above the `bias_deadband_pct` band for 2 consecutive
  `bias_window_s` windows of `bias_min_samples` scored samples each; the
  deadband holds the class. Decisive evidence (beyond 2× the deadband — past
  the actuator-swing envelope) classifies on ONE window at
  `bias_min_samples / 4` (≥ 4) samples, so short-lived phases are learned
  before they end.
- Slew & fairness: attack (away from 0 toward the class target) ≤ 1 step per
  `attack_step_s` (30 s default) from want-onset; release (toward 0) ≤ 1 step
  per `release_step_s` (300 s); one bias step globally per 5 s. The attack
  want survives deadband excursions shorter than a full hysteresis
  confirmation (2 windows) — borderline windows flipping `cand` must not
  starve the ramp — while a sustained deadband abandons it and runs the
  release law. Releases outrank attacks but never win two global steps in a
  row while an attack waits, so a restarted cohort's releases cannot
  monopolize the budget.
- **lavd absent ≠ bias 0**: history entries keep their bias and continue to
  apply; learning stops updating. Wall-time aging (half-life) decays
  `confidence` and relaxes bias toward 0 stepwise, so old learnings can never
  pin a bias permanently. Presence is never counter-evidence: windows too
  sparse to classify (or fully unscored) hold the class and confidence —
  only absence decays them.
- Persistence: `bias_state_path` (default `/var/lib/aranicy/bias-state`),
  versioned JSON, atomic write, debounced ≥ 60 s. `dump bias` reads it
  unprivileged (on-disk state may lag live bias by up to ~60 s).

Config keys (all `bias_*`): `bias_enable`, `bias_min`, `bias_max`,
`bias_window_s`, `bias_deadband_pct`, `bias_min_samples`, `bias_cadence_ms`,
`bias_max_entries`, `bias_half_life_s`, `bias_state_path`,
`bias_collector_socket`, `bias_starved_min_demand`, `bias_starved_ratio`, plus
the non-`bias_*` collector keys `collector_events_socket` and
`collector_wait_ms`, and repeatable `bias_group.<tag> = <regex>` family
declarations (first definition wins; bad tags/regexes warn and skip).

## Config keys (superset)

`check_freq`, `apply_nice`, `apply_latnice`, `apply_ionice`, `apply_ioclass`,
`apply_sched`, `apply_oom_score_adj`, `apply_cgroup`, `apply_cpuset`,
`apply_autogroup` (superset), `cgroup_load`, `type_load`, `rule_load`,
`loglevel`, `log_applied_rule`, `cgroup_realtime_workaround`, `x3d_mode`
(accepted for ananicy-cpp compatibility, **never applied** — 3D V-Cache mode is
host-controlled; warned at load). Superset-only `bias_*` keys and
`bias_group.<tag>` are listed under *Dynamic nice bias*.

### Reload liveness

`--reload` re-reads `ananicy.conf` and the rules tree, and re-applies the bias
knobs to the running engine. Every key is classified exactly once (the
`CONFIG_KEYS` table in `crates/aranicy-daemon/src/config.rs` is the single
source of truth):

- **live** — re-read per event/drift scan, or re-applied by the reload's own
  load pass: `check_freq`, `cgroup_load`, `type_load`, `rule_load`,
  `apply_nice`, `apply_latnice`, `apply_ionice`, `apply_sched`,
  `apply_oom_score_adj`, `apply_cgroup`, `apply_cpuset`, `apply_autogroup`,
  `cgroup_realtime_workaround`, `log_applied_rule`.
- **reconfigurable** — reach the running `BiasEngine::reconfigure`:
  `bias_min`, `bias_max`, `bias_window_s`, `bias_deadband_pct`,
  `bias_min_samples`, `bias_max_entries`, `bias_half_life_s`,
  `bias_starved_min_demand`, `bias_starved_ratio`, `bias_group.*`.
  Reconfiguring keeps the learned history (entries, population threshold T,
  score/confidence EWMAs, want stamps, the global step budget), clamps each
  bias into the new `[bias_min, bias_max]`, re-derives each target from its
  class, and re-keys history through the new `bias_group` families (collisions
  merge: the higher-confidence entry survives, the larger `|bias|` wins, the
  later `last_seen` is kept).
- **restart-only** — captured at startup; a reload logs them by name instead of
  pretending it applied them: `loglevel`, `bias_enable` (the sampler/bias
  thread lifetime; the actuation gate itself is read per event),
  `bias_cadence_ms`, `bias_state_path`, `bias_collector_socket`,
  `collector_events_socket`, `collector_wait_ms`.

`x3d_mode` is the one *accepted-but-inert* key: parsed, warned at load, read by
nothing.

## CLI and service

- `aranicy start` (root; no BPF of its own — it requires
  `aranicy-collector.service` and fails loudly if the collector's sockets do not
  appear within `collector_wait_ms`),
- `aranicy dump {rules,types,cgroups,proc,autogroup,bias}` (unprivileged;
  `bias` shows the persisted bias snapshot),
- `aranicy lint`, `aranicy match-test <name>` (dry-run, superset),
- `--reload` reloads config **and rules** (superset: ananicy-cpp reloads
  config only) and reconfigures the bias engine. It logs
  `reloaded {n} rules; bias reconfigured`, calls out a changed `bias_group.*`
  (history keys change) and warns
  `restart-only keys ignored: <sorted key list>` for any deferred edit,
  `--config`, `--config-dir`.
- Unit `aranicy.service`, `ExecReload=/usr/bin/aranicy --reload`,
  `Conflicts=ananicy-cpp.service` for drop-in cutover.

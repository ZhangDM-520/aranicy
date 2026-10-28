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
| `cpuset` | string | `0-3,8-11` or aliases `big-cores`, `little-cores`, `turbo-cores`, `performance-cores`, `efficiency-cores`, `all-cores`, `llc-N`, `node-N` |
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

Optional dynamic nice adjustment driven **solely** by `scx_lavd` scheduler
samples over its stats socket — no fallback sampler is shipped; without lavd
the mechanism degrades as described below. Disable with `bias_enable = false`.
Hot-path cost is one extra O(1) map lookup per event — the bias history is a
separate table from the rule tables.

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
  daemon never adjusted are not verified.
- Classification: per-sample score from weight-free sched stats against a
  population threshold (EWMA of the population mean score). BULK below and
  RESPONSIVE above the `bias_deadband_pct` band for 2 consecutive
  `bias_window_s` windows of `bias_min_samples` scored samples each; the
  deadband holds the class. Decisive evidence (beyond 2× the deadband — past
  the actuator-swing envelope) classifies on ONE window at
  `bias_min_samples / 4` (≥ 4) samples, so short-lived phases are learned
  before they end.
- **lavd absent ≠ bias 0**: history entries keep their bias and continue to
  apply; learning stops updating. Wall-time aging (half-life) decays
  `confidence` and relaxes bias toward 0 stepwise, so old learnings can never
  pin a bias permanently.
- Persistence: `bias_state_path` (default `/var/lib/aranicy/bias-state`),
  versioned JSON, atomic write, debounced ≥ 60 s. `dump bias` reads it
  unprivileged (on-disk state may lag live bias by up to ~60 s).

Config keys (all `bias_*`): `bias_enable`, `bias_min`, `bias_max`,
`bias_window_s`, `bias_deadband_pct`, `bias_min_samples`, `bias_cadence_ms`,
`bias_max_entries`, `bias_half_life_s`, `bias_state_path`, plus repeatable
`bias_group.<tag> = <regex>` family declarations (first definition wins; bad
tags/regexes warn and skip).

## Config keys (superset)

`check_freq`, `apply_nice`, `apply_latnice`, `apply_ionice`, `apply_ioclass`,
`apply_sched`, `apply_oom_score_adj`, `apply_cgroup`, `apply_cpuset`,
`apply_autogroup` (superset), `cgroup_load`, `type_load`, `rule_load`,
`loglevel`, `log_applied_rule`, `cgroup_realtime_workaround`, `x3d_mode`
(accepted; cpuset alias input only). Superset-only `bias_*` keys and
`bias_group.<tag>` are listed under *Dynamic nice bias*.

## CLI and service

- `aranicy start` (root; eBPF-only: hard failure with a clear message when
  BTF/tracepoint support is missing — no silent fallback),
- `aranicy dump {rules,types,cgroups,proc,autogroup,bias}` (unprivileged;
  `bias` shows the persisted bias snapshot),
- `aranicy lint`, `aranicy match-test <name>` (dry-run, superset),
- `--reload` reloads config **and rules** (superset: ananicy-cpp reloads
  config only), `--config`, `--config-dir`.
- Unit `aranicy.service`, `ExecReload=/usr/bin/aranicy --reload`,
  `Conflicts=ananicy-cpp.service` for drop-in cutover.

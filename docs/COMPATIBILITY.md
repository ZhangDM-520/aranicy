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

## Config keys (superset)

`check_freq`, `apply_nice`, `apply_latnice`, `apply_ionice`, `apply_ioclass`,
`apply_sched`, `apply_oom_score_adj`, `apply_cgroup`, `apply_cpuset`,
`apply_autogroup` (superset), `cgroup_load`, `type_load`, `rule_load`,
`loglevel`, `log_applied_rule`, `cgroup_realtime_workaround`, `x3d_mode`
(accepted; cpuset alias input only).

## CLI and service

- `aranicy start` (root; eBPF-only: hard failure with a clear message when
  BTF/tracepoint support is missing — no silent fallback),
- `aranicy dump {rules,types,cgroups,proc,autogroup}` (unprivileged),
- `aranicy lint`, `aranicy match-test <name>` (dry-run, superset),
- `--reload` reloads config **and rules** (superset: ananicy-cpp reloads
  config only), `--config`, `--config-dir`.
- Unit `aranicy.service`, `ExecReload=/usr/bin/aranicy --reload`,
  `Conflicts=ananicy-cpp.service` for drop-in cutover.

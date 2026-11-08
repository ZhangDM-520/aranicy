# CONTEXT — aranicy domain model

Vocabulary for humans and agents. Architecture terms (module, interface, depth,
seam, adapter, leverage, locality) come from the codebase-design glossary; this
file defines only the *domain*. Decisions are recorded as ADRs in `docs/adr/`
(none yet).

## Roles

**exec event** — a `sched_process_exec` observation. The collector resolves the
**match name** at capture time (microseconds after the exec), because by the
time an event crosses a socket a short-lived build tool's `/proc/<pid>` is gone.

**match name** — `basename(argv[0])` → `/proc/<pid>/exe` basename → kernel
`comm`. Rules match on this.

**comm** — the kernel task name (16 bytes). Bias history is keyed by comm, never
by match name: the two namespaces differ (a Go or Electron binary execs
`/usr/bin/electron` while its threads stay `Chrome_ChildIOT`).

**policy** — the flattened set of adjustments a rule implies: `nice`,
`latency_nice`, `sched`, `ioclass`/`ionice`, `oom_score_adj`, `cgroup`,
`cpuset`, `autogroup`. Rules (`name`, `name_regex`), types and cgroup
definitions all flatten into a policy at load; matching never re-merges.

**collector** (`aranicy-collectord`) — the GPL-2.0-only package that owns every
BPF program. Serves per-task **samples** over the `scx_stats` protocol and
pushes exec events over a framed socket. aranicy is a pure client.

**drift correction** — the periodic re-assertion of the applied policy on
running processes (a foreign writer may change nice behind our back).

## Bias learning

**sample** — one per-task row per drain window: `pid`, `comm`, `static_prio`
(nice + 20), `wait_freq` (sleeps/s), `wake_freq` (wakes issued/s),
`avg_runtime_wall` (ns per schedule), `task_util_est` (**demand**: running +
runnable duty, 0..1024), `cpu_util_wall` (**delivered**: running duty, 0..1024).

**observation budget** — how many samples a window may carry and which ones
survive the cut. Part of the contract, not an implementation detail: a name
that never appears never learns.

**history key** — the name a bias **entry** is filed under: the comm, folded
through fuzzy rules (per-instance suffixes such as `kworker/u97:10` → `kworker/u97`)
or a declared `bias_group.<tag>` regex family (`cc1plus`, `as`, `ld` → one
stream).

**class** — `Bulk` (throughput, cheap to delay), `Responsive` (latency
sensitive), `Unknown` until two consecutive classifying windows agree.

**score** — weight-free per-sample signal derived from wait/wake/runtime, so
our own applied nice can never feed back into the evidence that justifies it.

**T** — the population EWMA of the mean score. It is still the *actuated*
class boundary, `T × (1 ± deadband)`. Population-relative on purpose; its
weakness is that one dominant name redefines "bulk" for everyone. ADR 0001
records why an absolute constant was rejected and what replaces it.

**lean** — the absolute, population-free class signal proposed to replace the
`T` boundary: `log2(burst / 64 µs) − log2(hop / 100 Hz)`, positive = bulk. It
separates long bursts from short frequent ones without a population statistic,
which `score` cannot (see ADR 0001). Runs in **shadow** today.

**shadow** — `bias_shadow` (default off): the lean classifier runs beside the
actuated one, never changes a bias, and logs only disagreements and a 10-minute
summary. The data that gates the flip.

**window** — the classification interval (default 60 s), closed on wall time.
A window needs `min_samples` scored samples; decisive evidence (beyond twice
the deadband) may classify on one window with `min_samples / 4`.

**want / target / applied** — `target` is the nice the current class asks for;
`want` is the timestamped desire to reach it; `applied` is what this daemon
actually set. Rate laws gate transitions between them.

**claim** — the daemon's record that it applied a given nice for a name. The
**closed loop** verifies a fresh sample's `static_prio` against the claim; a
mismatch **freezes** the entry (evidence stops updating) until a matching sample
**re-baselines** it.

**starved** — RESPONSIVE but not getting its share: `demand > min_demand` and
`delivered < ratio × demand`. Starvation lowers nice; bulk raises it.

**confidence** — 0..1 evidence strength, halved per `half_life_s` while a name
is unseen. Absence decays; presence never counts as counter-evidence.

## Units (load-bearing)

| quantity | unit | scale |
|---|---|---|
| `static_prio` | nice + 20 | 0..39 in practice |
| `task_util_est` | duty cycle, running + runnable | 0..1024 = one CPU |
| `cpu_util_wall` | duty cycle, running | 0..1024 = one CPU |
| `avg_runtime_wall` | nanoseconds per schedule | > 0 means "ran" |
| `wait_freq`, `wake_freq` | events per second (EWMA) | 0 is a real zero |
| `runs`, `sleeps` | schedules / sleeps in one window (counts) | 0 is a real zero |

A zero `wait_freq` or `wake_freq` is a **measurement** (a compiler neither
sleeps nor wakes anyone — it forks). Only `avg_runtime_wall == 0` means "no
evidence".

# ADR 0001 — the class boundary is not an absolute constant (yet)

Status: **accepted** — supersedes the "ship now" half of candidate 7 (§7.4 7a) in
`docs/reviews/2026-11-08-wave2-design.md`. Date: 2026-11-09.

## Context

The bias engine classifies a name from `score = s0²`, where
`s0 = ilog2(wait_ft·wake_ft) + ilog2(1e9 / avg_runtime_wall)`, compared against a
population mean `T` with a ±15 % deadband (two-window hysteresis, decisive
window at 2× the deadband). The known weakness is the population bet: one
dominant name redefines the boundary for everyone (`CONTEXT.md`).

Candidate 7 proposed replacing `T` with an absolute constant, 1185, measured on
163 high-confidence entries of one snapshot: "0 hard violations, so the
boundary is compatible with everything we have learned". The design was explicit
that its labels are circular and that this is a compatibility claim only.

This ADR records an attempt to reproduce and extend that calibration, and the
decision it forces.

## Evidence (measured 2026-11-09, `tools/replay-bias/replay.py`)

Test-thread entries (libtest names its threads after the test path, truncated to
15 bytes) are excluded; they classify BULK at score ≈ 200 and depress `T`.

| snapshot | entries | `T` | cohort (conf > 0.5) | bulk p50 | resp p50 | best hard boundary | errors | violations at 1185 |
|---|---|---|---|---|---|---|---|---|
| live, 2026-11-09 | 4096 | 402 | 996 | 227 | 658 | 381 | 39 (3.9 %) | **181 (18.2 %)** |
| `pre-v030`, 2026-11-07 | 4096 | 893 | 75 | 674 | 1004 | 859 | 8 (10.7 %) | 13 (17.3 %) |
| `pre-cleangate`, 2026-10-31 | 4096 | 827 | 78 | 517 | 893 | 606 | 12 (15.4 %) | 36 (46.2 %) |

Three findings:

The live row is a point-in-time reading (the daemon rewrites the file every
60 s, so `T` and the cohort move between runs); the two backup files are
immutable and reproduce exactly. Re-running the same command about 40 minutes
later — after the host's test-thread entries aged out and `T` rose — gave
97/1136 (8.5 %) at the same anchor, which is itself part of the argument: a
constant can only be validated against a population that does not move.

1. **1185 fails its own gate today.** 181 of 996 high-confidence labels sit
   outside its band. The design's "0 hard violations" was true of a snapshot
   that no longer exists (2824 entries, `T = 1259.63`); the live file has since
   been rewritten by the daemon, so that snapshot cannot be re-checked.
2. **The best boundary tracks `T` in every snapshot** (381 vs 402; 859 vs 893;
   606 vs 827). That is expected — the labels were produced by `T`, so the
   sweep recovers `T`. It also means the sweep cannot rank candidate constants;
   it can only say whether a constant preserves today's behaviour.
3. **The two backups come from the old event sampler** (pre-v0.3.0), so the
   cross-snapshot comparison is confounded by the producer change. There is one
   post-collector snapshot (`T = 402`) and one unreproducible design-time one
   (`T = 1259.63`); their boundary estimates differ 3×. That is not enough
   evidence to call the score scale stable, and it is enough to refuse a
   constant.

### Non-circular check (hand-curated rules, not the classifier)

Joining the live snapshot with `/etc/ananicy.d` rule types (`LowLatency_RT`,
negative `nice`, `Heavy_CPU`) gives a small but non-circular sample:

| role (from rules) | name | score | current class |
|---|---|---|---|
| latency | pipewire | 595 | responsive |
| latency | Xwayland | 531 | responsive |
| latency | noctalia | 691 | responsive |
| latency | wireplumber | 315 | **bulk** |
| latency | easyeffects | 331 | responsive |
| bulk | rustc | 277 | **responsive** |
| bulk | cargo | 253 | bulk |
| bulk | build | 485 | responsive |

The latency and bulk roles **overlap in score** (253–691). A boundary high
enough to leave `rustc`/`cargo` above it (bulk = low score) would put `pipewire`
and `Xwayland` there too; a boundary low enough for the audio names puts the
compilers on the bulk side but also every idle daemon. No single scalar
separates them, because `score` is dominated by *interaction rate*: a compiler
that never sleeps and an idle audio manager that rarely wakes look alike.

The design's hardware-invariance argument also assumes `s0 ≈ 34`, where a
runtime doubling moves the score ≈ +6 %. The measured cohort sits at `s0 ≈ 16`
(`score ≈ 250`), where the same doubling moves it ≈ +13 % — at the deadband
edge. The invariance margin the design relied on is not present at the observed
scale.

## Decision

1. **Do not ship `class_threshold` / `bias_class_threshold`.** `T` remains the
   classification boundary. No user-visible classifier change in this wave.
2. **Ship the harness** — `tools/replay-bias/replay.py`, which reproduces the
   table above on any snapshot, filters test threads, and exits non-zero when an
   anchor exceeds a violation budget (`--max-violations`). Any future boundary
   change must run it against the live file *and* a quiet-host capture, and
   report both.
3. **The next classifier attempt is candidate 7b (two-axis `lean`), not another
   constant.** `lean = log2(burst / 64 µs) − log2(rate / 100 Hz)` separates long
   bursts (bulk) from short, frequent ones (latency) on absolute axes; it does
   not reduce to interaction rate, which is what defeats `score`. It needs
   `runs`/`sleeps` on the wire (additive) and ships behind `bias_shadow`,
   actuating the current classifier while logging disagreements.
4. **Flip gate for 7b** (recorded so it is not renegotiated later): over ≥ 1000
   classified name-windows, disagreements ≤ the current boundary's
   high-confidence violation rate (18 % on the live cohort), *and* no
   known-role regression on the rule-corpus names above — pipewire,
   wireplumber, easyeffects, Xwayland, noctalia stay off the bulk side while
   rustc/cargo/build move to it.
5. **The no-op attack cycle** (`AGENTS.md`) is not part of this decision; it is
   a control-law defect with its own fix and test.

## Consequences

- The population bet stays in force, with its documented failure mode, until
  7b's shadow data justifies the flip. That is deliberate: a constant that
  fails its own compatibility gate is worse than a boundary with a known bias.
- `tools/replay-bias` becomes the cheapest way to answer "would this boundary
  change anything?" — including for the next `T`-based tuning.
- Test-thread pollution is now measured (`302` of 4096 live entries,
  `3341` of 4096 in the Nov 7 backup) and filtered by default; do not quote a
  calibration number without saying whether they were excluded.
- 7b is a separate wave (collector fields + shadow mode + gate). No part of it
  is started by this ADR.

## Addendum (2026-11-11) — the constant was measured against a broken producer

The 18 % violation figure above was measured on a **distorted** score. The
v0.3.0 collector computed `wait_freq`/`wake_freq` per WINDOW second
(`delta * 1e9 / dt`), which multiplies the rate by the task's duty cycle and so
collapsed the score of exactly the short-lived tools this boundary has to
separate (`grep` 1033 → 180, `bash` 804 → 225). With the producer fixed to
per-ACTIVE-second rates (the quantity scx_lavd's per-event EWMA reported), the same
snapshot that produced 181/996 violations produces:

| reading | anchor 1185 band | bulk p90 | responsive p10 |
|---|---|---|---|
| 2026-11-09 (window rates) | 267 violations (23.0 %) | 314 | 387 |
| 2026-11-11 (active-second rates) | **2 violations (0.55 %)** | 970 | 1 444 |

Two things follow. First, the cohorts now separate cleanly, so the
`T`-tracking boundary is no longer papering over an inversion. Second, the
rejected constant 1185 is nearly compatible *now* — which is evidence that it
was a reasonable calibration of the intended metric, and that the metric, not
the constant, was the defect. The decision below is unchanged (the population
bet still ships, the flip still waits on shadow data), but no future boundary
argument may cite the old violations number without saying which producer
produced it.

## Reproduce

```sh
sudo tools/replay-bias/replay.py /var/lib/aranicy/bias-state
sudo tools/replay-bias/replay.py /var/lib/aranicy/bias-state.pre-v030
sudo tools/replay-bias/replay.py /var/lib/aranicy/bias-state.pre-cleangate-20261031
```

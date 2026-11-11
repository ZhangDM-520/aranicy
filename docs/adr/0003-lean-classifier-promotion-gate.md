# ADR 0003 — the lean classifier stays observational until it is replayed

Status: **accepted**. Date: 2026-11-11. Depends on ADR 0001. Evidence:
round-3 `dana-4`.

## Context

ADR 0001 killed the absolute-anchor proposal (the best anchor simply tracks
the live population `T`; re-measured today: `T = 1375`, 909 confident entries,
anchor 1185 violates 6.93 %) and named the next attempt: the two-axis `lean`
classifier already behind `bias_shadow`.

That classifier is implemented (`classify.rs`): `lean = log2(burst/64 µs) −
log2(hop/100 Hz)`, a multiplicative deadband, and burst 0 = no evidence. It is
free of population statistics and is wired into `close_window` where it is
compared — as an observer only — against the pre-hysteresis `cand`, with
counters (`windows`, `disagreements`, `unilateral`) and rate-limited logs.

What kept it observational is not doubt about the arithmetic; it is the
capture. The persisted snapshot stored only `score_ema`, so the two-axis
verdict could not be re-run offline at all: promoting it would have been
assertion, and the round-3 `dana-4` reading (a shell at score 1011 and a CPU
spinner at 3.9, both actuated `Bulk +3`) shows the cost of a wrong label.

## Decision

1. The snapshot now records the lean inputs — `burst_ns`, `hop_hz` and the
   `lean_at` second they were written — when `bias_shadow` is on (`Durable`,
   so they survive a restart and need no second projection), plus a top-level
   `lean_capture` naming the boundary in force (`stage_min_ns`, `hop_min_hz`,
   `deadband_pct`, `saved_at`). The anchors are live config keys; a replay
   that assumed the defaults would judge a classifier the daemon never ran.
   No marker = those entries are not a capture (the shadow never ran, the
   file predates the marker, or a boundary change discarded the capture), and
   the replay refuses to score them. The marker is adopted at restart only when it names the
   boundary the reloaded binary runs, so a `bias_lean_*` edit applied by a
   restart drops the stale inputs instead of re-stamping them. Entries older
   than `--max-lean-age` (default 1 h) are reported as stale and excluded.
2. `tools/replay-bias/replay.py` re-runs the same arithmetic (mirrored
   constants) over a captured snapshot and reports agreement, disagreement and
   the concrete inverted names. It is the offline half of the gate.
3. **The learner's actuation input is unchanged in this wave**:
   `bias_shadow` remains observation-only.

## Promotion gate (all three, on a quiet-host capture)

A "quiet host" means no `cargo test` (libtest names fill the history with
BULK-at-~200 entries) and no build: the capture is taken on an idle VM.

1. **Disagreement < 10 % of decided entries, over at least 200 decided
   entries**, computed by the replay, not by the live counters (the live
   denominator includes deadband windows). The replay reports the ratio as
   `null` and `gate_cohort_ok: false` below that count: a small cohort cannot
   clear the gate by being empty or unanimous.
2. **Every inversion is inspected, not just counted** — the ADR 0001 flip gate.
   A chatty interactive name classified `Bulk` (the `shell` shape) is a
   blocker even if the percentage passes: that is the +3 the engine would add.
3. **A two-window lean hysteresis** exists before any actuation, symmetric with
   the score classifier's streak, so a single odd window cannot step.

If the capture passes 1–2, the next wave is `bias_lean_actuate`: the lean
verdict replaces `cand` behind a config switch, with the hysteresis from 3 and
the same clamped targets. If it does not pass, the numbers and the inverted
names go in this ADR and the score classifier stays.

## Consequences

- The state file grows by two integers per entry (~24 bytes × 4096).
- A run with `bias_shadow = false` records no *new* lean inputs; a snapshot
  restored from an earlier capture still carries them and their marker, so
  turning the flag off does not destroy a capture. Replay reports "captured,
  but no entry has fresh burst/hop inputs" when the marker is present but no
  entry still has fresh inputs, and "not a capture" when there is no marker at
  all.
- The live counters remain the only view of the comparison against `cand`;
  the replay compares against the persistent `class` (post-hysteresis) and its
  output says so.
- A snapshot written by a build that did not restore the marker is not
  distinguishable from a clean one: its marker can name a boundary that did
  not measure the entries' inputs. Discard such a history (or take a fresh
  capture) before a promotion measurement; the file format carries no
  per-entry boundary to test.

## First capture (2026-11-11, and why it does not clear the gate)

A 2 vCPU VM, the wave's own payload, `bias_shadow = true`, 4 min of mixed
load — deliberately including 35 spin loops, which saturated the box (load 39)
before the capture was read. 84 entries, 38 with lean inputs, 26 decided:
**24 agree / 2 disagree (7.7 %)**. The two inversions, both inspected:
`sshd-session` (lean +0.86, burst 306 µs at 264 hops/s) and `sudo` (+0.26,
294 µs at 385 hops/s) — interactive one-shots pushed just past the burst
anchor by the saturation itself.

The capture predates the `lean_capture` marker added with this ADR (the file
has raw `burst_ns`/`hop_hz` but no boundary record), so those numbers were
read with the boundary that was in force — the defaults — by hand. A capture
taken today carries its boundary and refuses to be scored without one.

Verdict: **gate not met, promotion stays blocked.** 26 decided entries is not
a population, the load is the exact confound the quiet-host rule exists to
remove, and the two inversions sit close enough to the anchor that a +3 on an
interactive name is imaginable. The replay tool works
(`tools/replay-bias/replay.py <snapshot>`), so the remaining work is a capture,
not code: a quiet host with a real population (≥ 200 decided entries), then
inspect every inversion by name.

## Addendum — measured capture

The first capture also prices the score axis it would replace: on the same
snapshot the best absolute anchor recovers `T` exactly (115, 0 errors) and
anchor 1185 violates 42 % of the cohort, which is ADR 0001's argument
reproduced on new data.

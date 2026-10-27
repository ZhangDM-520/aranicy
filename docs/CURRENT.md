
## 2026-10-22 (aranicy-events owner pass)

- `crates/aranicy-events` + `bpf/` implemented and verified end-to-end
  (integration 30/30); todo `ebpf-events` done. No commits; awaiting owner's
  disposition decision.

## 2026-10-22 (aranicy-daemon owner pass)

- `crates/aranicy-daemon` implemented and verified end-to-end (36/36 unit
  tests + root smoke of start/reload/drift/shutdown/sd_notify). Todo
  `daemon-cli` done. No commits; awaiting owner's disposition decision.

## 2026-10-23 (OPT2+OPT3 perf round)

- Ringbuf batch drain + bounded busy-poll (aranicy-events) and cached /proc
  dirfd resolver (aranicy-daemon) implemented and A/B-measured. Busy-spin
  default set to 0 µs from evidence (30 µs regressed the paced workload 2.6×;
  `ANANICY_RS_BUSY_SPIN_US` opts in). Todos `opt2-batch-drain` and
  `opt3-proc-dirfd` done. No commits; awaiting owner's disposition decision.

## 2026-10-27 (bias-engine owner pass)

- `bias.rs` (estimator + controller + aging) and `bias_state.rs` (serde_json
  atomic save/load) implemented; `cargo test -p aranicy-daemon` 68/68 green
  (17 new). Todos `bias-estimator`, `bias-controller`, `bias-state` done.
  Remaining for the caller: wire engine into daemon.rs/config, debounced
  persistence, `dump bias`. No commits; awaiting owner's disposition decision.

## 2026-10-28 (dump bias done; live validation in flight)

- `dump bias` shipped + CLI-smoked; clippy --workspace --all-targets clean;
  workspace tests 112 green. COMPATIBILITY.md gained the dynamic-nice
  contract (lavd-absent: bias keeps applying, learning stops, aging caps
  history).
- Live bias validation RUNNING (background agent): lavd learn/closed-loop,
  lavd-stop semantics, storm A/B bias-on/off (≤5% gate, sampler ≤1%).
  Evidence → ~/Workspace/aranicy-bias-val/evidence/. After results:
  BENCHMARKS.md bias section, packaging + gsa tree, commit disposition ask.
  No commits so far.

## 2026-10-28 (dynamic nice bias shipped)

- a34eaf8 pushed to ZhangDM-520/aranicy main: dynamic nice bias (lavd-only
  learning, closed loop, aging-capped history) + `dump bias` + audit fixes.
  Gates: storm both variants <=5% (3.88/4.93 bias-on), instr/event delta
  +1.3% noise-level, sampler 0.15%. Full evidence:
  ~/Workspace/aranicy-bias-val/evidence/{,postfix,postfix2}.
- Open items (owner's call): tag a post-bias release and bump
  packages/stable/aranicy in the gsa tree; live cutover already done
  (ananicy-cpp retired 2026-10-24) but the running service is v0.1.0-1 —
  rebuild/install to pick this up.

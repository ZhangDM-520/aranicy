
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

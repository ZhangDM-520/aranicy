
## eBPF event source (aranicy-events), reviewed 2026-10-22

- libbpf-rs 0.27 `RingBufferBuilder::build()` erases the map-borrow lifetime
  (`RingBuffer<'cb>` keeps only the callback lifetime) while the ring buffer
  still uses the map's fd — owning structs must keep the `Object` field after
  the `RingBuffer` field so drop order stays rb -> obj.
- `bpf_link__detach` fails EOPNOTSUPP for perf-event links produced by
  `Program::attach()` on `tp/...` sections; the detach path is
  `bpf_link__destroy`, i.e. dropping the `Link`.
- `bpf_get_current_comm()` returns 0/-errno, not a length; comm_len must be
  derived by scanning for NUL inside TASK_COMM_LEN.
- Kernel `comm` is the executed file's basename, never argv[0]; tests need a
  real uniquely named file (arg0 tricks do not move comm).
- Leftover-BPF audits must diff against a pre-run baseline: this host already
  had a `handle_exec` tracepoint prog (id 332, loaded 2026-10-22 19:42) with
  links 69/70 from another consumer; grep-zero is not the right oracle.

## Host interference: ananicy-cpp runs alongside tests (found 2026-10-22)

- This host runs `ananicy-cpp.service` (root, check_freq=60, loglevel
  critical). It reverts `renice`/`setpriority` on test processes back to their
  inherited nice ~30–230 ms after the change (verified: flip disappears with
  `systemctl stop ananicy-cpp`). Any nice-stability test must either stop the
  service or expect counter-writes; aranicy's drift corrector visibly
  "misfires" while actually correcting it.
- aranicy `ExecSource::wake()` latches shutdown=true (next_event then returns
  Ok(None) forever) — wake is terminal; it cannot be used to nudge the event
  loop for reloads. Reload must run on the signal thread and swap an
  RwLock<Arc<RuleSet>> instead.
- Linux `/proc/<pid>/stat` field 18 (priority) = nice + 20, not 20 - nice;
  field 19 (index 16 after the comm's last ')') is nice.

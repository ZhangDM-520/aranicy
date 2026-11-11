# aranicy-collectord

The measurement half of aranicy. It exists as its own package and its own
binary for one reason: it is the only component that loads eBPF programs and
owns the privileges to do it, so the daemon itself needs no `CAP_BPF`,
`CAP_PERFMON` or `CAP_SYS_ADMIN`.

It is licensed GPL-2.0-only (it links `scx_stats` and ships a GPL BPF object);
`aranicy` is MIT.

## What it does

Two independent jobs, both on the same eBPF object:

| job | hook | output |
|---|---|---|
| exec events | `sched_process_exec` | one frame per exec: pid, ppid, kernel `comm`, and the match name resolved at capture time |
| per-task accounting | `sched_switch`, `sched_stat_runtime`, `sched_waking` | a periodic sample per task: runtime, runnable time, wakeups issued, wait count, utilisation estimate |

The sample stream is what the daemon's learned nice bias reasons about; the
exec stream is what applies rules. **Neither is required for the other**: with
the collector down, rules stop being applied and the bias history stops
updating, but nothing crashes and the daemon keeps retrying.

## Sockets

| socket | direction | consumer |
|---|---|---|
| `/run/aranicy/collector-stats.sock` | collector serves, daemon connects | bias sampling |
| `/run/aranicy/collector-events.sock` | collector serves, daemon connects | rule application |

Both are `AF_UNIX` `SOCK_STREAM`, created under `RuntimeDirectory=aranicy` by
the unit, and removed on stop.

## Checking it works

```sh
systemctl status aranicy-collector
journalctl -u aranicy-collector -n 20    # "exec-event client connected" when the daemon attached
aranicy-collectord probe                 # one batch of rows, printed and exited
```

`probe` is the honest check: if it prints rows, the BPF programs are attached
and the accounting hooks are firing on this kernel. An empty or missing batch
means the kernel does not route the accounting hooks (some non-`sched_ext`
configurations do not emit `sched_stat_runtime` at all), and the daemon will
print `transport down` in its own journal.

## Privileges

The unit runs as root because eBPF loading needs it. Everything it sends is
plain data over the two sockets; it never applies a priority to anything.

## Stopping it

`systemctl stop aranicy-collector` detaches the programs and closes the
sockets; the daemon reports a degraded sampler and keeps retrying with a
backoff. `systemctl restart aranicy-collector` is safe at any time.

## Requirements

* a kernel with `CONFIG_BPF_SYSCALL`, tracepoint support and `sched_ext` (for
  the per-task stats the scheduler exports);
* `libbpf.so` at runtime (the package depends on `libbpf`).

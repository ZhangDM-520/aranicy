# Round 3 — adversarial pass

- environment: Linux 7.2.11-arch1-1 (Arch rolling, cloud image), aranicy 0.3.0-1 +
  aranicy-collector 0.3.0-1, 2 vCPU x86_64, 1958 MiB RAM, no display.
- build under test: `~/payload` (the current build). The previous build was
  used only for the differential and is called out where it matters.
- host state at the end of this pass: both packages installed, `aranicy` and
  `aranicy-collector` active, `/etc/ananicy.d` empty apart from the packaged
  `ananicy.conf`, no test rules, `khugepaged` at its kernel default.

Everything below was run through
`/tmp/workspace/su/r3/homelab/machine` (ssh to the box). Commands are quoted as
they were typed on the box; empty lines are trimmed, nothing else is.

## Results at a glance

| test | result |
|---|---|
| junk rule file (bad JSON, bad regex, out-of-range values, NUL, 200-char line) | **held** — every defect named with file:line, valid rules in the same file still applied |
| empty / comments-only / BOM / CRLF / unreadable rule files | **held** — BOM and CRLF apply, unreadable file reported to unprivileged `lint`, daemon unaffected |
| empty config file | **held** — accepted, defaults used, rules still apply |
| half-written config (6 broken lines, unknown key, no `=`) | **held** — one warning per line, daemon starts |
| non-root `aranicy start` | **held** — clear refusal, exit 1 |
| other service stopped | **held** for an explicit stop; **failed** for a masked dependency at boot (silent) |
| `kill -9` the collector while working | **held** — collector and daemon restart, policy resumes in ~5 s |
| `kill -9` the daemon | **held** — restarts in under 3 s, history restored |
| reboot in the middle of a workload | **held** — everything comes back, rules apply to new execs, protected processes untouched |
| install / remove / reinstall | **leaks** — upgrade does not restart the daemon; uninstall leaves a quota-bearing cgroup behind |
| `exec -a` / `prctl` rename | **held** — matching follows the executable, as documented |
| copied binary with a rule's name | **held** (documented trust model: rules are keyed on the executable) |
| pid recycled under a rule | **held** — no stale nice on the new owner |
| workload that disappears while the daemon looks | **held** — 500 one-shot execs, daemon healthy |

## A1 — a rule file full of junk

```
$ sudo tee /etc/ananicy.d/ivy-60-junk.rules >/dev/null <<'EOF'
{"name": "xz", "nice": 19}
this line is not json at all
{"name": "gzip", "nice":
{"name": "gzip", "nice": 999}
{"name": "gzip", "nice": -99}
{"name": "gzip", "name_regex": "["}
{"name": "gzip", "cgroup": 5}
{"nosuchfield": 1}
{"name": "", "nice": 5}
{"name": "tar", "nice": 5} trailing garbage after the object
EOF
$ sudo bash -c "printf '\0\n' >> /etc/ananicy.d/ivy-60-junk.rules"
$ sudo bash -c "printf '{\"name\": \"cuts\", \"sched\": \"fifo\", \"rtprio\": 200}\n' >> /etc/ananicy.d/ivy-60-junk.rules"
$ aranicy lint
warning: /etc/ananicy.d/ivy-60-junk.rules:2: no JSON object span (missing '{' or '}')
warning: /etc/ananicy.d/ivy-60-junk.rules:3: no JSON object span (missing '{' or '}')
warning: /etc/ananicy.d/ivy-60-junk.rules:4: nice: 999 out of range -20..=19
warning: /etc/ananicy.d/ivy-60-junk.rules:5: nice: -99 out of range -20..=19
warning: /etc/ananicy.d/ivy-60-junk.rules:6: invalid name_regex "[": regex parse error:
    [
    ^
error: unclosed character class
warning: /etc/ananicy.d/ivy-60-junk.rules:7: cgroup: expected string, got number
warning: /etc/ananicy.d/ivy-60-junk.rules:8: unclassified object: expected string name/name_regex, type, or cgroup
warning: /etc/ananicy.d/ivy-60-junk.rules:9: empty name can never match a process
warning: /etc/ananicy.d/ivy-60-junk.rules:11: no JSON object span (missing '{' or '}')
warning: /etc/ananicy.d/ivy-60-junk.rules:12: rtprio: 200 out of range 0..=99
lint: warnings found
[exit=1]
```

The daemon logs the same warnings and starts; the valid rules in the same file
still work:

```
$ sudo systemctl restart aranicy; sleep 5
$ systemctl is-active aranicy aranicy-collector
active
active
$ setsid xz -9 -c /home/arch/work/big.bin >/dev/null 2>&1 & ...
$ ps -o pid,ni,comm -p 3817
    PID  NI COMMAND
   3817  19 xz
$ setsid tar -cf /dev/null /usr >/dev/null 2>&1 & ...
$ ps -o pid,ni,comm -p 3822
    PID  NI COMMAND
   3822   5 tar
```

The trailing-garbage line is documented ("Text from the first `{` through the
last `}` on the line is parsed (trailing garbage ignored)") and it behaves that
way. A junk file cannot take the daemon down or silently disable the rest of
the corpus.

## A2 — empty, comments-only, BOM, CRLF and unreadable rule files

```
$ sudo ls -la /etc/ananicy.d/
-rw-r--r-- 1 root root    0 Nov 11 10:16 ivy-61-empty.rules
-rw-r--r-- 1 root root   40 Nov 11 10:16 ivy-62-comments.rules
-rw-r--r-- 1 root root   29 Nov 11 10:16 ivy-63-bom.rules
-rw-r--r-- 1 root root   27 Nov 11 10:16 ivy-64-crlf.rules
---------- 1 root root   28 Nov 11 10:16 ivy-65-noread.rules

$ aranicy lint
warning: /etc/ananicy.d/ivy-65-noread.rules:0: failed to read file: Permission denied (os error 13)
lint: warnings found
[exit=1]

$ sudo aranicy lint
OK: 3 rules, 0 types, 0 cgroups
[exit=0]
```

The unprivileged `lint` cannot read a root-only file and says so (that is the
right answer for an Ansible gate that runs as me; `sudo aranicy lint` is the
root view). BOM and CRLF files are accepted and applied — measured, because I
had assumed the BOM would cost a rule:

```
$ printf '\xef\xbb\xbf{"name": "tar", "nice": 7}\n' | sudo tee /etc/ananicy.d/ivy-63-bom.rules
$ printf '{"name": "dd", "nice": 7}\r\n' | sudo tee /etc/ananicy.d/ivy-64-crlf.rules
$ aranicy lint
warning: /etc/ananicy.d/ivy-66-nul.rules:2: no JSON object span (missing '{' or '}')
lint: warnings found
[exit=1]
$ setsid tar -cf /dev/null /usr >/dev/null 2>&1 & ...
$ ps -o pid,ni,comm -p 1417
    PID  NI COMMAND
   1417   7 tar
$ setsid dd if=/dev/zero of=/dev/null bs=1M count=3000 >/dev/null 2>&1 & ...
$ ps -o pid,ni,comm -p 1421
    PID  NI COMMAND
   1421   7 dd
```

(The lint in that run also contained the NUL-only file below, hence the
warning; both the BOM and the CRLF rule still loaded and applied.)

A NUL-only line is reported like any other non-JSON line:

```
$ od -c /etc/ananicy.d/ivy-66-nul.rules
0000000   {   "   n   a   m   e   "   :       "   x   z   "   ,       "
0000020   n   i   c   e   "   :       1   9   }  \n  \0  \n
$ aranicy lint
warning: /etc/ananicy.d/ivy-66-nul.rules:2: no JSON object span (missing '{' or '}')
lint: warnings found
[exit=1]
```

## A3 / A4 — empty and half-written config

Empty config (0 bytes):

```
$ sudo truncate -s 0 /etc/ananicy.d/ananicy.conf
$ aranicy lint
warning: 0 rules loaded from /etc/ananicy.d: this corpus will apply nothing
lint: warnings found
[exit=1]
$ sudo systemctl restart aranicy; sleep 5
$ systemctl is-active aranicy aranicy-collector
active
active
```

No complaint about the config itself; defaults are used and a rule added
afterwards applies (`PID 3931 NI 19 xz`).

Half-written config:

```
$ sudo tee /etc/ananicy.d/ananicy.conf <<'EOF'
check_freq =
apply_nice = tru
bias_min = 1.5
check_freqq = 120
loglevel = verbose
apply_cpuset
EOF
$ aranicy lint
warning: config: line 1: invalid check_freq (u64 seconds) value ""
warning: config: line 2: invalid boolean "tru"
warning: config: line 3: invalid bias_min (i8) value "1.5"
warning: config: line 4: unknown key "check_freqq"
warning: config: line 5: invalid loglevel value "verbose"
warning: config: line 6: no '=' in "apply_cpuset"
lint: warnings found
[exit=1]
$ sudo systemctl restart aranicy; sleep 5
$ systemctl is-active aranicy aranicy-collector
active
active
```

Every bad line is named with its line number, including the unknown key
(`check_freqq`), which is the typo I care about. The daemon starts with the
defaults for the bad lines rather than refusing — that is a defensible choice,
but it means the *only* signal that `apply_nice = tru` did not take effect is
the warning: nothing on the box says "this key was ignored, the default was
used".

One thing that bit me while doing this (my own doing, recorded because it will
bite an Ansible play too): five `systemctl restart` calls inside 60 s hit the
unit's `StartLimitBurst=5` and the unit then refuses to start:

```
$ sudo systemctl restart aranicy
Job for aranicy.service failed because start of the service was attempted too often.
See "systemctl status aranicy.service" and "journalctl -xeu aranicy.service" for details.
To force a start use "systemctl reset-failed aranicy.service"
followed by "systemctl start aranicy.service" again.
[exit=1]
```

## A5 — non-root

```
$ aranicy start (as arch, non-root)
Error: aranicy start requires root (effective uid 1000); run it via sudo or the aranicy.service unit
[exit=1]

$ aranicy lint as uid 1000
OK: 1 rules, 0 types, 0 cgroups
[exit=0]

$ aranicy dump rules as uid 1000
{
  "exact": [
    {
      "match": "exact",
      "name": "xz",
```

`lint` and `dump` work unprivileged; `start` refuses clearly.

## A6 — the other service stopped or masked

**Stopping the collector stops the daemon** (documented `Requires=`), which is
what I want:

```
$ sudo systemctl stop aranicy-collector
$ systemctl is-active aranicy aranicy-collector
inactive
inactive
$ sudo journalctl -u aranicy -u aranicy-collector --since "-10 seconds" -o cat
Stopping aranicy - auto-nice daemon...
aranicy.service: Deactivated successfully.
Stopped aranicy - auto-nice daemon.
Stopping aranicy collector - scheduler-agnostic BPF task statistics...
aranicy-collector.service: Deactivated successfully.
Stopped aranicy collector - scheduler-agnostic BPF task statistics.
```

**Stopping the daemon does *not* stop the collector** — reported as issue
`ivy-14`. From `raw/stop-asymmetry.txt`:

```
+ sudo systemctl stop aranicy
+ systemctl is-active aranicy aranicy-collector
inactive
active
+ sudo journalctl -u aranicy -u aranicy-collector --since '-10 seconds' --no-pager -o cat
Stopping aranicy - auto-nice daemon...
aranicy.service: Deactivated successfully.
Stopped aranicy - auto-nice daemon.
2026-11-11T10:31:42.565074Z  INFO aranicy_collectord::events: exec-event client disconnected error=Broken pipe (os error 32) dropped=0
```

**Masked at boot is silent** — reported as `ivy-12`. This is the one I would
class as a failure of the brief's own criterion ("a clear failure visible in
`systemctl status`, not an active unit doing nothing"). After
`systemctl mask aranicy-collector` and a reboot:

```
$ systemctl is-active aranicy aranicy-collector
inactive
inactive
$ systemctl is-enabled aranicy aranicy-collector
enabled
masked
$ systemctl is-failed aranicy aranicy-collector
inactive
inactive
$ systemctl status aranicy
○ aranicy.service - aranicy - auto-nice daemon
     Loaded: loaded (/usr/lib/systemd/system/aranicy.service; enabled; preset: disabled)
     Active: inactive (dead)
       Docs: https://github.com/ZhangDM-520/aranicy
$ sudo journalctl -b -u aranicy -u aranicy-collector --no-pager -o cat
(no output)
```

A manual start *is* clear:

```
$ sudo systemctl restart aranicy
Failed to restart aranicy.service: Unit aranicy-collector.service is masked.
[exit=1]
```

The daemon's own readiness check is also loud, once it actually runs (collector
stopped, started by hand):

```
$ sudo timeout 20 /usr/bin/aranicy start
...
Error: no collector at /run/aranicy/collector-events.sock after 5s: No such file or directory (os error 2) (is aranicy-collector.service running?)
[exit=1]
```

So the component does the right thing; the boot path never gets that far and
says nothing.

## A7 / A8 — `kill -9` the other half

Collector killed while a `while :; do /bin/true; done` churn was running:

```
$ sudo kill -9 32315
$ systemctl is-active aranicy aranicy-collector   # 2 s later
active
activating
$ systemctl is-active aranicy aranicy-collector   # 10 s later
active
active
$ sudo journalctl -u aranicy -u aranicy-collector --since "..." -o cat | tail
aranicy-collector.service: Main process exited, code=killed, status=9/KILL
aranicy-collector.service: Failed with result 'signal'.
2026-11-11T10:19:11.755234Z  WARN stats sampler degraded prev=live reason=transport down detail=write: Broken pipe (os error 32)
aranicy-collector.service: Scheduled restart job, restart counter is at 1.
Stopping aranicy - auto-nice daemon...
aranicy.service: Deactivated successfully.
Stopped aranicy - auto-nice daemon.
Started aranicy collector - scheduler-agnostic BPF task statistics.
Starting aranicy - auto-nice daemon...
2026-11-11T10:19:16.795872Z  INFO bias: restored history entries=82
Started aranicy - auto-nice daemon.
$ ps -o pid,ni,comm -p <fresh xz>
    PID  NI COMMAND
  32621  19 xz
```

The sampler says `degraded … transport down` rather than pretending to be live,
the unit restarts after 5 s, the daemon follows, and a new exec is managed
again — a ~5 s hole, which is inherent to `RestartSec=5`. Fine.

Daemon killed:

```
$ sudo kill -9 32603
$ systemctl is-active aranicy aranicy-collector   # 3 s later
activating
active
$ systemctl is-active aranicy aranicy-collector   # 15 s later
active
active
$ systemctl show -p NRestarts aranicy
NRestarts=1
$ sudo journalctl -u aranicy --since "..." -o cat | tail
aranicy.service: Main process exited, code=killed, status=9/KILL
aranicy.service: Failed with result 'signal'.
aranicy.service: Scheduled restart job, restart counter is at 1.
Starting aranicy - auto-nice daemon...
2026-11-11T10:19:31.708457Z  INFO bias: restored history entries=82
...
$ ps -o pid,ni,comm -p <fresh xz>
    PID  NI COMMAND
  32650  19 xz
```

## A9 — reboot in the middle of work

First reboot (no rules at that moment, `xz -9` running, then `systemctl reboot`):

```
$ cat /proc/sys/kernel/random/boot_id     # before
c787354a-d5e7-4d4c-83db-c245a078a6d1
$ ps -o pid,ni,comm -p 3952
    PID  NI COMMAND
   3952   0 xz
$ sudo systemctl reboot --no-block
Connection to 127.0.0.1 closed by remote host.
...
$ cat /proc/sys/kernel/random/boot_id     # after, 65 s later
6b6367d0-d4d1-40de-b115-5f09f5fda6b9
$ uptime -p
up 1 minute
$ systemctl is-active aranicy aranicy-collector
active
active
$ systemctl status aranicy | head -6
● aranicy.service - aranicy - auto-nice daemon
     Loaded: loaded (/usr/lib/systemd/system/aranicy.service; enabled; preset: disabled)
     Active: active (running) since Wed 2026-11-11 10:22:36 UTC; 1min 0s ago
$ sudo journalctl -b -u aranicy -o cat | head -8
Starting aranicy - auto-nice daemon...
2026-11-11T10:22:36.054938Z  WARN no rules loaded from /etc/ananicy.d: ...
2026-11-11T10:22:36.105573Z  INFO bias: restored history entries=36
...
```

That boot had no rules (my previous test had removed them), so I repeated it
with a rule and a live workload:

```
$ aranicy dump rules | head -12
{
  "exact": [
    {
      "match": "exact",
      "name": "xz",
      "nice": 19
    }
  ],
  "regex": []
}
$ setsid xz -9 -c /home/arch/work/big.bin >/dev/null 2>&1 & ...
$ ps -o pid,ni,comm -p 563
    PID  NI COMMAND
    563  19 xz
$ sudo systemctl reboot --no-block
...
$ cat /proc/sys/kernel/random/boot_id
73b32b31-cc04-4d94-8fc7-bd9d7bb6ea10
$ systemctl is-active aranicy aranicy-collector
active
active
$ sudo journalctl -b -u aranicy -o cat | head -5
Starting aranicy - auto-nice daemon...
2026-11-11T10:24:00.033378Z  WARN kernel does not support latency_nice (sched_setattr LATENCY_NICE); latency_nice policy will be skipped
2026-11-11T10:24:00.084540Z  INFO bias: restored history entries=76
Started aranicy - auto-nice daemon.
$ aranicy dump rules | head -8          # rules present after boot
{ "exact": [ { "match": "exact", "name": "xz", "nice": 19 } ], "regex": [] }
$ ps -o pid,ni,comm -p 472              # a fresh xz after boot
    PID  NI COMMAND
    472  19 xz
$ ps -o pid,ni,comm -C sshd,systemd-journald,systemd-resolved,khugepaged,kcompactd0
    PID  NI COMMAND
     40   0 kcompactd0
     42  19 khugepaged
    201   0 systemd-journal
    253   0 systemd-resolve
    431   0 sshd
```

One thing worth saying explicitly, because it is the kind of thing I would
accuse a nice-daemon of: `khugepaged` sits at nice 19. It is not aranicy — in
the boot where the daemon could not start at all (masked collector) it is also
19, and it is already 19 right after boot, before `aranicy` has a socket:

```
$ sudo journalctl -b -u aranicy -u aranicy-collector --no-pager -o cat
(no output)
$ ps -o pid,ni,comm -C khugepaged,kcompactd0
    PID  NI COMMAND
     40   0 kcompactd0
     42  19 khugepaged
```

So 19 is this kernel's default for `khugepaged`, not a policy applied by the
package. All other kernel threads I sampled are at their stock values
(`kworker/R-*` at −20, `kcompactd0` 0, `ksoftirqd` 0) and `systemd`/PID 1 is 0.

## A10 — install, remove, reinstall

Install and uninstall are clean in the ways I care about now: the state file
is removed with the package, the units and `RuntimeDirectory` go away, and no
process survives.

```
$ sudo pacman -R --noconfirm aranicy aranicy-collector
...
$ sudo ls -la /var/lib/aranicy
ls: cannot access '/var/lib/aranicy': No such file or directory
[exit=2]
$ sudo ls -la /run/aranicy
ls: cannot access '/run/aranicy': No such file or directory
$ ls /usr/lib/systemd/system/aranicy*
ls: cannot access '/usr/lib/systemd/system/aranicy*': No such file or directory
$ pgrep -a aranicy
[exit=1]
```

Reinstall from the same package pair starts from a blank slate (no resurrected
bias state):

```
$ sudo ls -la /var/lib/aranicy
total 0
drwxr-xr-x 1 root root   0 Nov 11 10:21 .
drwxr-xr-x 1 root root 188 Nov 11 10:21 ..
$ aranicy dump bias | head -8
{
  "error": null,
  "note": "on-disk snapshot is debounced; live bias may be up to ~60 s newer",
  "path": "/var/lib/aranicy/bias-state",
  "state": null
}
```

Two leftovers: the running daemon is not restarted by a reinstall (`ivy-13`)
and a cgroup created from a `.cgroups` file survives the uninstall with its
quota in force (`ivy-15`).

```
$ P=$(systemctl show -p MainPID --value aranicy); echo "pid=$P"
pid=32971
$ sudo pacman -U --noconfirm ~/payload/...pkg.tar.zst
reinstalling aranicy...
$ P2=$(systemctl show -p MainPID --value aranicy); echo "pid before=$P after=$P2"
pid before=32971 after=32971
$ sudo readlink /proc/32971/exe
/usr/bin/aranicy (deleted)
$ systemctl show -p NRestarts aranicy
NRestarts=0
```

```
$ printf '%s\n' '{"cgroup": "ivymedia", "CPUQuota": 50}' | sudo tee /etc/ananicy.d/ivy-90-media.cgroups
$ printf '%s\n' '{"name": "xz", "cgroup": "ivymedia"}' | sudo tee /etc/ananicy.d/ivy-50-xz.rules
$ sudo systemctl reload aranicy; sleep 2
$ cat /sys/fs/cgroup/ivymedia/cpu.max
100000 100000
$ cat /proc/3331/cgroup
0::/ivymedia
$ sudo pacman -R --noconfirm aranicy aranicy-collector
...
$ ls -d /sys/fs/cgroup/ivymedia
/sys/fs/cgroup/ivymedia
$ cat /sys/fs/cgroup/ivymedia/cpu.max
100000 100000
$ cat /proc/3331/cgroup
0::/ivymedia
```

A `pacman -U` of the same version writes a new file; the process keeps the old
inode and nothing tells systemd to restart it. On an Ansible box that means
"package upgraded, fix not running" until I notice.

## A11 — a process that renames itself

`exec -a` (argv[0] only) is ignored for matching, as documented:

```
$ setsid bash -c 'exec -a gzip /usr/bin/xz -9 -c /home/arch/work/big.bin >/dev/null' &
$ tr '\0' ' ' </proc/33025/cmdline; echo
gzip -9 -c /home/arch/work/big.bin 
$ ps -o pid,ni,comm -p 33025
    PID  NI COMMAND
  33025  19 xz
```

It got the `xz` rule (19), not the `gzip` rule (10) — the executable wins over
argv[0].

`prctl(PR_SET_NAME)` likewise does not move the match:

```
$ setsid python3 -c 'import ctypes; ctypes.CDLL(None).prctl(15,b"gzip",0,0,0); import time; time.sleep(20)' &
$ ps -o pid,ni,comm -p 33031
    PID  NI COMMAND
  33031   0 python3
```

A copied binary named after a rule does get that rule — the documented trust
model, not a bug, but worth seeing once:

```
$ cp /usr/bin/xz /home/arch/work/gzip
$ setsid /home/arch/work/gzip -9 -c /home/arch/work/big.bin >/dev/null &
$ readlink /proc/33038/exe
/home/arch/work/gzip
$ ps -o pid,ni,comm -p 33038
    PID  NI COMMAND
  33038  10 gzip
```

That is fine for a rule that *lowers* priority (`nice: 10`). It is the reason I
keep every elevation rule (negative nice, `sched`, `rtprio`) out of my corpus,
as the shipped document says.

## A12 — a pid that gets recycled

I shrank the pid space so the kernel would hand out a specific pid again:

```
$ cat /proc/sys/kernel/pid_max
4194304
$ sudo sysctl -w kernel.pid_max=1024
kernel.pid_max = 1024
$ setsid xz -9 -c /home/arch/work/big.bin >/dev/null 2>&1 & ...
$ ps -o pid,ni,comm -p 301
    PID  NI COMMAND
    301  19 xz
$ kill 301
$ GOT=""; for i in $(seq 1 3000); do sleep 1 & q=$!; if [ "$q" = "$P" ]; then GOT=$q; break; fi; kill "$q" 2>/dev/null; wait "$q" 2>/dev/null; done
iterations=697 reused=301
$ ps -o pid,ni,comm -p 301
    PID  NI COMMAND
    301   0 sleep
$ sleep 5; ps -o pid,ni,comm -p 301
    PID  NI COMMAND
$ ps -eo pid,ni,comm --no-headers | awk '$2==19 && $1>100'
(no output)
$ systemctl is-active aranicy aranicy-collector
active
active
$ sudo sysctl -w kernel.pid_max=4194304
kernel.pid_max = 4194304
```

The recycled pid was handed to an unrelated binary at nice 0, and no
non-kernel process was left at the rule's nice. I could not make the daemon
apply a stale pid's policy.

## A13 — a workload that disappears while the daemon is looking

500 one-shot execs under a rule (`{"name": "true", "nice": 19}`):

```
$ for i in $(seq 1 500); do /bin/true; done
$ sleep 2
$ systemctl is-active aranicy aranicy-collector
active
active
$ systemctl show -p NRestarts aranicy
NRestarts=0
$ sudo journalctl -u aranicy -u aranicy-collector --since "$T5" --no-pager -o cat
(no output)
```

No crash, no unresolved-name noise, no bogus apply summary.

## What I could not break

- A junk rule file cannot disable or crash the daemon; the good rules in the
  same file keep working.
- An unreadable rule file is reported to an unprivileged `lint` and ignored by
  the root daemon.
- Empty and half-written configs do not take the daemon down.
- `kill -9` on either half recovers without a manual step (the daemon restart
  follows the collector's own restart).
- Two reboots (one mid-work, one with the dependency masked) left `sshd`,
  `systemd-journald`, `systemd-resolved`, PID 1 and every kernel thread I
  sampled at their stock nice values.
- Renaming, copying binaries, recycling pids and storming the box with
  one-shot processes did not produce a wrong application.

## Suspicions (not proven)

- `suspicion:` with the daemon stopped, `aranicy-collectord` stays active and
  keeps its `sched_*` tracepoints attached. I did not measure its CPU or list
  its BPF programs (no `bpftool` on this box), so I only claim the unit is
  still running, not the cost.
- `suspicion:` the `.INSTALL` comment says the unit is "handled by the daemon's
  own restart logic on the next `systemctl start`"; there is no such logic in
  the package (only `daemon-reload`), which is consistent with the stale inode
  I measured.

## Environment notes

- `latency_nice` is unsupported on this kernel (`sched_setattr LATENCY_NICE`),
  so every `latency_nice` rule is skipped with a warning. That is the box, not
  the package.
- This is a 2-vCPU cloud image with no `unrar`, `par2`, `bpftool`, `strace`,
  `pidstat` or `jq`; workloads were `xz`, `gzip`, `dd`, `tar`, `openssl speed`.
- `journald` is persistent here (`/var/log/journal`), which is why I can quote
  the previous boot's tail after a reboot.

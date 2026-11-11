# Adversarial pass — round 3

Machine: `su-skeptic-r3`, Linux 7.2.11-arch1-1 (Arch rolling), 2 vCPU, 1958 MiB,
fresh cloud image, passwordless sudo, `arch` = uid 1000. Build under test:
`aranicy 0.3.0-1` + `aranicy-collector 0.3.0-1` from `~/payload`, installed
exactly as `README.md` says. `strace`, `perf`, `bpftool` and `pidstat` are not
installed and I did not install third-party software; every number below comes
from `/proc`, `/sys`, `journalctl`, `systemctl` and the tool's own `dump`
subcommands.

The intent of this pass is the one in the brief: find the failures that happen
*silently* or leave the machine worse than before. Four things failed that way
and are filed: `dana-11` (cgroup quota lost after reboot), `dana-12` (upgrade
does not replace the running binary), `dana-13` (daemon re-nices itself),
`dana-15` (`--reload` signals an unverified pid). Everything else I attacked
held, and I say so below with the output that convinced me.

---

## 1. A rule file full of junk — held

I wrote a file with a truncated object, an out-of-range nice, a string where an
integer belongs, an unknown key, a nameless object, an empty object, a JSON
array and a bare `{`.

```
$ cat ~/j/junk/junk.rules
{not json at all
{"name": "a", "nice": 5,
{"name": "b", "nice": 999}
{"name": "c", "nice": "five"}
{"name": "d", "unknown_key": 1}
{"nice": 3}
{}
[]
{"name": "e", "nice": 4}

$ aranicy lint --config-dir /home/arch/j/junk
warning: config: config "/home/arch/j/junk/ananicy.conf" not found; using defaults
warning: /home/arch/j/junk/junk.rules:1: no JSON object span (missing '{' or '}')
warning: /home/arch/j/junk/junk.rules:2: no JSON object span (missing '{' or '}')
warning: /home/arch/j/junk/junk.rules:3: nice: 999 out of range -20..=19
warning: /home/arch/j/junk/junk.rules:4: nice: expected integer, got string
warning: /home/arch/j/junk/junk.rules:5: unknown key "unknown_key" ignored
warning: /home/arch/j/junk/junk.rules:6: unclassified object: expected string name/name_regex, type, or cgroup
warning: /home/arch/j/junk/junk.rules:7: unclassified object: expected string name/name_regex, type, or cgroup
warning: /home/arch/j/junk/junk.rules:8: no JSON object span (missing '{' or '}')
lint: warnings found          (rc=1)
```

Every line is accounted for, with the line number, and the daemon prints the
same list at startup (I ran it as root with the same directory). The two rules
that are merely odd (`d` with an unknown key, `e` which is valid) survive:

```
$ aranicy dump rules --config-dir /home/arch/j/junk
{
  "exact": [
    { "match": "exact", "name": "d" },
    { "match": "exact", "name": "e", "nice": 4 }
  ],
  "regex": []
}
```

One nit, not worth an issue: line 2 is reported as "no JSON object span
(missing '{' or '}')" although the line does contain a `{` — the real defect is
that it is unterminated. The message is close enough for a human and exact
enough to find the line.

## 2. Empty config file, half-written config, unknown key — held

```
$ : > empty.conf ; sudo aranicy start --config empty.conf --config-dir empty
WARN no rules loaded from /home/arch/j/empty: with an empty rule set only the learned bias changes priorities. The package ships no rule corpus - point ANANICY_CPP_CONFDIR at yours, or write files ending in .rules
WARN kernel does not support latency_nice (sched_setattr LATENCY_NICE); latency_nice policy will be skipped
INFO bias: restored history entries=43
INFO stats sampler live prev=degraded: starting detail=bound SchedSample via sched_samples at /run/aranicy/collector-stats.sock
```

```
$ half.conf = (check_freq, "loglevel = inf", "bias_protect = " )   # last line truncated by a crash
WARN config: line 2: invalid loglevel value "inf"
```

```
$ unknown.conf = (check_freq = 60, "bogus_key = 42")
WARN config: line 2: unknown key "bogus_key"

$ bad2.conf = ("check_freq = not-a-number")
WARN config: line 1: invalid check_freq (u64 seconds) value "not-a-number"
```

All four degrade to defaults, name the file line, and keep running. That is the
right behaviour for a daemon that must not be a SPOF, and the warnings are
specific. `lint` exits 1 when it had warnings and 0 when clean
(`OK: 1 rules, 0 types, 0 cgroups`, rc=0), which is usable in CI.

## 3. Non-root — held, with a good message

```
$ aranicy start
Error: aranicy start requires root (effective uid 1000); run it via sudo or the aranicy.service unit
(rc=1)
```

`dump rules`, `dump proc`, `dump bias`, `dump cgroups` and `lint` all work
unprivileged, which the README claims and which matters for an operator who
wants to look without a shell as root.

## 4. The peer service stopped or masked — held (fail-closed, but see note)

Masked, cold start:

```
$ sudo systemctl mask aranicy-collector.service
$ sudo systemctl start aranicy
Failed to start aranicy.service: Unit aranicy-collector.service is masked.        (rc=1)
$ systemctl is-active aranicy aranicy-collector
inactive
inactive
$ aranicy dump proc >/dev/null; /home/arch/j/try.sh skepticnaive     # rule not applied
skepticnaive pid=2024 nice=0
```

Masked under a running pair:

```
$ sudo systemctl mask --now aranicy-collector.service
$ sleep 2; systemctl is-active aranicy aranicy-collector
inactive
inactive
```

Nothing is applied while the collector is gone, and the tool does not pretend
otherwise (the units are down, and an exec gets its inherited nice). The
mechanism is systemd's `Requires=` deactivation propagating to `aranicy`, not
the daemon deciding to stop; either way the observable state is honest.

Note (documentation gap, not filed as a bug): nothing in the README says that
masking or removing the collector takes the whole product down, and
`CapabilityBoundingSet=` on the daemon explains why — the daemon cannot load
BPF itself. A user who "just wants the rule part" has no supported way to get
it.

## 5. `kill -9` on the paired service while it is working — held, verified recovery

```
$ C=$(systemctl show -p MainPID --value aranicy-collector); sudo kill -9 $C
$ /home/arch/j/try.sh skepticbat           # during the outage
skepticbat pid=728 nice=0
$ sleep 15
collector=active daemon=active
$ /home/arch/j/try.sh skepticbat           # after the collector came back
skepticbat pid=757 nice=12
```

The daemon logs the transport loss (`WARN stats sampler degraded prev=live
reason=transport down detail=write: Broken pipe`), systemd restarts the
collector, `Requires=` brings the daemon back with it, and rules apply again
within ~15 s. No wedge, no stale socket (the socket files are re-created; the
unit has `RuntimeDirectory=aranicy`).

Killing the daemon itself (`kill -9`, mid-flight):

```
$ sudo kill -9 $(systemctl show -p MainPID --value aranicy)
$ sleep 13
daemon=active newpid=1645 pidfile=1645
$ /home/arch/j/try.sh skepticbat
skepticbat pid=781 nice=12
```

`Restart=always` does its job, the pid file is rewritten, and bias history is
reloaded from disk (`INFO bias: restored history entries=46`) — at most the
last debounce window (~60 s per the `dump bias` note) is lost. This is the one
place where the split design is visibly good.

## 6. Reboot in the middle — mostly good, one real failure

Two reboots. What came back:

```
$ ~/j/postboot.sh
== units ==
active
active
enabled
disabled
== rules applied at exec? ==
skepticnaive pid=498 nice=12
== bias state on disk ==
-rw-r--r-- 1 root root 12910 Nov 11 10:08 bias-state
== journal after boot ==
... INFO bias: restored history entries=61
```

Both units up, `aranicy` enabled and the collector pulled in by `Requires=`
(it is deliberately `disabled`), rules applied at exec, learned history
restored. What did **not** come back is the cgroup CPU quota → `dana-11`:

```
WARN cgroup "skepticcg" not created: Permission denied (os error 13)
t+20 s after boot: cpu.max absent
t+80 s:            cpu.max = max 100000      (unlimited, while `dump cgroups` says 50 %)
after `systemctl restart aranicy`: cpu.max = 100000 100000
```

## 7. Install, remove, reinstall — uninstall is clean, upgrade is not

Uninstall leaves nothing of its own:

```
$ sudo pacman -R --noconfirm aranicy aranicy-collector
removing aranicy...
removing aranicy-collector...
$ ls -la /usr/share/doc/aranicy
ls: cannot access '/usr/share/doc/aranicy': No such file or directory
$ ls -la /usr/bin/aranicy*
ls: cannot access '/usr/bin/aranicy*': No such file or directory
$ ls -la /usr/lib/systemd/system/aranicy*
ls: cannot access '/usr/lib/systemd/system/aranicy*': No such file or directory
$ sudo ls -la /var/lib/aranicy          (and the same for /run/aranicy, /run/aranicy.pid)
ls: cannot access '/var/lib/aranicy': No such file or directory
$ ls -la /etc/systemd/system/multi-user.target.wants/ | grep -i arani   # nothing
$ pgrep -a aranicy                                                     # nothing
```

The only things left are my own unowned rule files in `/etc/ananicy.d`, which
is correct (the package does not own them) — but note that the learned bias
history in `/var/lib/aranicy` goes with the package, so a remove/reinstall
throws away everything the daemon learned. That is defensible for
`StateDirectory=`, and the config does not mention it.

Reinstall is where it bites: `pacman -U` of a *different* binary leaves the old
process running from a deleted inode → `dana-12`. Both packages are
`pkgver = 0.3.0-1`, so `pacman -Q` cannot tell you which build you have.

## 8. A process that renames itself — held, and the rule identity is real

```
baseline: my shell nice=0 sshd nice=0
1. control — file whose basename is the rule name:
   skepticnaive pid=817 nice=12
2. prctl(PR_SET_NAME,"skepticnaive") on a python3.14 image:
   pid=824 comm=skepticnaive exe=/usr/bin/python3.14 nice=0
3. exec -a skepticnaive /home/arch/innocentthird:
   pid=830 comm=innocentthird exe=/home/arch/innocentthird argv0=[skepticnaive 8 ] nice=0
```

So a process cannot borrow a rule with `argv[0]` or `comm`; only the executable
basename counts, and that is what the README claims. This is a real improvement
over the mechanism round 1 reported, and I could not break it.

What it does *not* stop — and what `docs/COMPATIBILITY.md` now documents under
"Trust model" — is copying any binary to a file named after an elevation rule:
the exe is unforgeable but its *name* is chosen by whoever creates the file. I
re-ran round 2's reproduction in the new build:

```
$ cp /bin/sleep ~/skepticnaive && setsid ~/skepticnaive 20 &
skepticnaive pid=817 nice=12
```

An unprivileged user gets whatever a rule with that name grants. Since this
build now says so in the shipped compatibility document, I am not filing it
again; dana-7 from round 2 stands as "documented, still true" rather than as an
undisclosed hole.

## 9. A pid that gets recycled — not demonstrated

I could not force the kernel to hand a specific pid back within a bounded time
on a 2-vCPU box, so I have no run in which reuse happened naturally. What I did
show is the precondition: after `kill -9` the pid file survives with a now-free
pid, and `--reload`/`start` trust it (`dana-15`). I am labelling the pid-reuse
consequence `unreproduced` in that issue rather than claiming it.

## 10. A workload that disappears while the daemon is looking at it — held

Short-lived processes are the normal case here rather than an adversarial one:
every `try.sh` run execs a copy of `/bin/sleep`, gets its nice inside ~1 s, and
is killed. The exec path never produced an error, a stuck apply, or a growth in
the daemon's CPU time that I noticed across the ~25 such runs in this session.
I also killed the long-lived spinners (`skepticspin`, `skepticcpu2`) under the
bias engine's nose, and `dump bias` kept working with their entries simply
ageing rather than erroring.

That is a weak claim: I cannot see the daemon's internal queue, so "no error
observed in ~25 runs" is the whole of it.

## 11. Blast radius — PID 1 held, kernel threads held, its own process did not

With rules naming `systemd`, `ksoftirqd/0`, `rcu_preempt` and a `kworker`:

```
t=20s pid1=0 ksoftirqd15=0 rcu16=0 sshd=0
t=40s pid1=0 ksoftirqd15=0 rcu16=0 sshd=5
t=60s pid1=0 ksoftirqd15=0 rcu16=0 sshd=5
```

PID 1 and the two kernel threads I kept in view were untouched over 60 s and
four samples; `sshd` was reniced at the 60 s pass, which is the silent
already-running-process application round 2 filed as dana-8 and which is
unchanged in this build (same mechanism, same `check_freq`). I dropped that
rule afterwards, because a reniced sshd is inherited by every new session and
would have quietly offset every later measurement by +5 (it did, once — see the
log).

The own-process case is the one that failed → `dana-13`: a rule named
`aranicy` moves the daemon from the unit's `Nice=-5` to 19 within 60 s, with no
journal line, on every boot.

## What I would still attack with more time

- A regex entry (`name_regex`) instead of exact names, especially one that
  matches the daemon, the collector, or a shell — `dana-13` used the narrowest
  possible form and still worked.
- `latency_nice` and `sched: fifo` grants: this kernel has no `latency_nice`
  (`WARN kernel does not support latency_nice`), so that half of the policy
  surface is untestable here; the `SCHED_FIFO` grant is round 2's dana-7 and I
  did not re-run the starvation case (2 vCPU, risk of losing the box).
- A `cpuset` rule, and `CPUQuota` with more than one cgroup — `dana-11` shows
  the boot path is where to look.

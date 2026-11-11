# Adversarial pass — ren, round 3

Machine: `su-dev-r3` (disposable), Arch Linux rolling, kernel 7.2.11-arch1-1,
2 vCPU, 1958 MiB, no display, passwordless sudo, no `sched_ext` scheduler
running. Build under test: `aranicy 0.3.0-1` + `aranicy-collector 0.3.0-1`
(`~/payload`, `Build Date: Wed Nov 11 09:52:22 2026`). Every command below was
run through `./machine '<cmd>'`; output is pasted verbatim. The machine started
with neither package installed (`error: package 'aranicy' was not found`).

Two things bit hard enough to get their own issue files:

- **a second `aranicy-collectord run` silently takes over the socket path and
  leaves the running pair unrecoverable** (`issues/ren-12-…`),
- **after a cold boot the configured cgroup exists but its `CPUQuota` is not
  applied, and the only warning says the cgroup was "not created"**
  (`issues/ren-13-…`).

Everything else in this pass behaved the way the shipped docs said it would, or
failed loudly in a way I could act on.

---

## 1. A rules file full of junk

Nine corpora — `empty`, `trunc`, `unknown`, `range`, `regex`, `mixed`, `conf`,
`half`, `typed` — each a directory with one file, `aranicy --config-dir <dir>
lint`. Six of the outputs, verbatim:

```sh
$ aranicy --config-dir ~/adv/trunc lint
warning: config: config "/home/arch/adv/trunc/ananicy.conf" not found; using defaults
warning: /home/arch/adv/trunc/a.rules:1: no JSON object span (missing '{' or '}')
warning: 0 rules loaded from /home/arch/adv/trunc: this corpus will apply nothing
lint: warnings found
exit=1

$ aranicy --config-dir ~/adv/unknown lint
warning: config: config "/home/arch/adv/unknown/ananicy.conf" not found; using defaults
warning: /home/arch/adv/unknown/a.rules:1: unknown key "nise" ignored
lint: warnings found
exit=1

$ aranicy --config-dir ~/adv/range lint
warning: config: config "/home/arch/adv/range/ananicy.conf" not found; using defaults
warning: /home/arch/adv/range/a.rules:1: nice: 99 out of range -20..=19
warning: /home/arch/adv/range/a.rules:2: rtprio: 200 out of range 0..=99
warning: 0 rules loaded from /home/arch/adv/range: this corpus will apply nothing
lint: warnings found
exit=1

$ aranicy --config-dir ~/adv/regex lint
warning: config: config "/home/arch/adv/regex/ananicy.conf" not found; using defaults
warning: /home/arch/adv/regex/a.rules:1: invalid name_regex "[unclosed": regex parse error:
    [unclosed
    ^
error: unclosed character class
warning: 0 rules loaded from /home/arch/adv/regex: this corpus will apply nothing
lint: warnings found
exit=1

$ aranicy --config-dir ~/adv/mixed lint      # valid rule, duplicate-ish, prose line, string nice, comment, non-ASCII name
warning: config: config "/home/arch/adv/mixed/ananicy.conf" not found; using defaults
warning: /home/arch/adv/mixed/a.rules:3: no JSON object span (missing '{' or '}')
warning: /home/arch/adv/mixed/a.rules:4: nice: expected integer, got string
lint: warnings found
exit=1

$ aranicy --config-dir ~/adv/typed lint
warning: config: config "/home/arch/adv/typed/ananicy.conf" not found; using defaults
warning: /home/arch/adv/typed/a.rules:1: unknown type "missingtype" referenced; rule keeps its own fields
lint: warnings found
exit=1
```

Every message carries file and line, no stack trace, exit code 1 whenever
anything is wrong. A *clean* rule file with a config beside it:

```sh
$ aranicy --config-dir ~/adv/good lint
OK: 2 rules, 0 types, 0 cgroups
exit=0
```

So the property I care most about — "OK with exit 0 on a file full of typos" —
does not happen. Two exit-code traps remain, both filed:

- an empty/`types`-only corpus is a **failure** (`issues/ren-14-…`) — `lint
  --help` says this is deliberate ("A corpus that is green is a corpus that will
  produce policy"), so I filed it as an enhancement and named the workflow it
  blocks,
- a valid corpus in a directory without `ananicy.conf` is a **failure**
  (`issues/ren-15-…`), which is exactly the "version-controlled rules dir in
  CI" workflow.

## 2. An empty config, a half-written config, an unknown key

Empty (0-byte) `/etc/ananicy.d/ananicy.conf`, daemon restarted:

```sh
$ sudo truncate -s 0 /etc/ananicy.d/ananicy.conf; sudo systemctl restart aranicy; sleep 5
$ systemctl is-active aranicy
active
$ sudo journalctl -u aranicy -b --no-pager -o cat | tail -5
Starting aranicy - auto-nice daemon...
2026-11-11T10:26:27.097145Z  WARN kernel does not support latency_nice (sched_setattr LATENCY_NICE); latency_nice policy will be skipped
2026-11-11T10:26:27.097497Z  INFO bias: restored history entries=4
Started aranicy - auto-nice daemon.
2026-11-11T10:26:27.099460Z  INFO stats sampler live prev=degraded: starting detail=bound SchedSample via sched_samples at /run/aranicy/collector-stats.sock
$ ps -eo pid,comm,nice | grep -w renapp | grep -v grep
   1389 renapp            7
   1900 renapp            7
```

Defaults are used, rules still apply, nothing pretends the file was read.

Half-written config (`apply_nice = tru`), daemon restarted:

```sh
$ sudo journalctl -u aranicy -b --no-pager -o cat | grep -nE "config:"
19:2026-11-11T10:20:14.028489Z  WARN config: line 4: invalid boolean "tru"
```

and the rule still applied (`972 renapp 7`), i.e. one bad value does not
disable the rest of the file. A config with unknown/invalid keys on a *reload*:

```sh
$ sudo aranicy --reload
sent SIGHUP to aranicy (pid 1608)
$ sudo journalctl -u aranicy --no-pager -o cat -n 5
2026-11-11T09:58:00.748352Z  WARN config: line 2: invalid boolean "fal"
2026-11-11T09:58:00.748417Z  INFO reloaded 2 rules; bias reconfigured
2026-11-11T09:58:00.748420Z  INFO bias_group.* changed; existing bias history re-keyed under the new families
```

The reload reports the bad line by number and keeps going.

## 3. A rules file that is broken *during* a reload

The half-written-file case that matters for a policy daemon. Live corpus with
`renapp` (nice 7, ionice idle) and one regex rule; then one truncated line is
added and `--reload` issued:

```sh
$ printf '%s\n' '{"name": "renapp", "nice": 7, "ioclass": "idle"}' '{"name": "renbroken", "nice": 5' | sudo tee /etc/ananicy.d/zz-test.rules
$ sudo aranicy --reload; sleep 1
sent SIGHUP to aranicy (pid 1608)
$ aranicy dump rules
{
  "exact": [
    {
      "ioclass": "idle",
      "match": "exact",
      "name": "renapp",
      "nice": 7
    }
  ],
  "regex": []
}
$ sudo journalctl -u aranicy --no-pager -o cat -n 2
2026-11-11T09:57:58.670048Z  WARN /etc/ananicy.d/zz-test.rules:2: no JSON object span (missing '{' or '}')
2026-11-11T09:57:58.670389Z  INFO reloaded 1 rules; bias reconfigured
```

The good rule survives, the bad one is named and skipped, and the count on the
reload line tells me the corpus shrank from 2 to 1 — no silent policy loss
without a log line.

## 4. Running it as a non-root user

```sh
$ aranicy start
Error: aranicy start requires root (effective uid 1000); run it via sudo or the aranicy.service unit
exit=1
$ sudo -u nobody /usr/bin/aranicy start
Error: aranicy start requires root (effective uid 65534); run it via sudo or the aranicy.service unit
exit=1
$ aranicy-collectord run
libbpf: Failed to bump RLIMIT_MEMLOCK (err = -EPERM), you might need to do it explicitly!
libbpf: Error in bpf_object__probe_loading(): -EPERM. Couldn't load trivial BPF program. Make sure your kernel supports BPF (CONFIG_BPF_SYSCALL=y) and/or that RLIMIT_MEMLOCK is set to big enough value.
Error: attaching collector BPF programs

Caused by:
    0: failed to load collector BPF object
    1: Operation not permitted (os error 1)
exit=1
$ aranicy dump bias
{
  "error": null,
  "note": "on-disk snapshot is debounced; live bias may be up to ~60 s newer",
  "path": "/var/lib/aranicy/bias-state",
  "state": null
}
exit=0
```

Refuses to run, says why, and the unprivileged read path (`dump`) still works —
which is what I need for a "why is this process at this priority" script.

## 5. The other half stopped, and masked

`systemctl stop aranicy-collector` (in the fixed check set):

```sh
$ sudo systemctl stop aranicy-collector; sleep 3
$ systemctl is-active aranicy aranicy-collector
inactive
inactive
$ pgrep -f '^/usr/bin/aranicy start' || echo "daemon gone"
daemon gone
```

Same as last round: the collector unit's stop takes the daemon with it, so the
"degraded sampler, keeps retrying" state in the collector README is not
reachable this way. Still open from round 2 (`ren-11`), unchanged in this
build — see `differential.md`.

Masked, then trying to restart the daemon:

```sh
$ sudo systemctl mask aranicy-collector
Created symlink '/etc/systemd/system/aranicy-collector.service' → '/dev/null'.
$ sudo systemctl restart aranicy
Failed to restart aranicy.service: Unit aranicy-collector.service is masked.
restart_rc=1
$ systemctl is-active aranicy aranicy-collector
active
active
$ pgrep -af "aranicy start|aranicy-collect"
1320 /usr/bin/aranicy-collectord run
1322 /usr/bin/aranicy start
```

The restart is refused with one clear line and the running pair is left alone —
no half-restarted daemon.

## 6. `kill -9` on either half

**Collector killed (daemon working):** clean.

```sh
$ sudo kill -9 $(pgrep -x aranicy-collect)
$ sleep 9; systemctl is-active aranicy aranicy-collector
active
active
$ sudo journalctl -u aranicy --no-pager -o cat -n 12
2026-11-11T09:57:14.614495Z  WARN stats sampler degraded prev=live reason=transport down detail=write: Broken pipe (os error 32)
Stopping aranicy - auto-nice daemon...
aranicy.service: Deactivated successfully.
Stopped aranicy - auto-nice daemon.
Starting aranicy - auto-nice daemon...
2026-11-11T09:57:19.828894Z  INFO bias: restored history entries=70
...
$ sudo journalctl -u aranicy-collector --no-pager -o cat -n 3
aranicy-collector.service: Failed with result 'signal'.
aranicy-collector.service: Scheduled restart job, restart counter is at 1.
2026-11-11T09:57:19.836387Z  INFO aranicy_collectord::events: exec-event client connected
$ ps -eo pid,comm,nice | grep -E "renapp|renrerun" | grep -v grep
   1420 renapp            7
   1636 renrerun          6
```

Cost: a ~5 s window with no daemon at all (09:57:14 → 09:57:19) because
`Requires=` propagates the collector's restart to the daemon. On a build
machine that is a small unmanaged window, not a failure — and the policy
resumes.

**Daemon killed (healthy pair):** clean.

```sh
$ sudo kill -9 $(pgrep -f "^/usr/bin/aranicy start")
$ for i in 1 2 3 ...; do sleep 5; echo "t+$((i*5))s daemon=$(pgrep -f "^/usr/bin/aranicy start" || echo none) col=$(pgrep -x aranicy-collect || echo none) state=$(systemctl is-active aranicy)"; done
t+5s daemon=none col=1178 state=activating
t+10s daemon=none col=1178 state=activating
t+15s daemon=1243 col=1178 state=active
t+20s daemon=1243 col=1178 state=active
...
$ sudo journalctl -u aranicy-collector -b --no-pager -o cat | tail -2
2026-11-11T10:22:48.783364Z  INFO aranicy_collectord::events: exec-event client disconnected error=Broken pipe (os error 32) dropped=0
2026-11-11T10:22:58.835300Z  INFO aranicy_collectord::events: exec-event client connected
```

Ten seconds down, state restored (`restored history entries=81`), rules live
again (`1294 renapp 7`). No stale-pidfile lockout either — `/run/aranicy.pid`
is re-written by the new process.

**But** the combined case is a trap: after a second `aranicy-collectord run`
had been started and exited, the same `kill -9` restarted the daemon five times
into a failed unit, because the collector that is still "active" no longer
accepts connections. That is `issues/ren-12-…`; the short version:

```sh
$ sudo timeout 8 /usr/bin/aranicy-collectord run
2026-11-11T10:23:40.986726Z  INFO aranicy_collectord: collector serving sched_samples + exec events socket=/run/aranicy/collector-stats.sock events_socket=/run/aranicy/collector-events.sock cadence_ms=500
second_exit=124
$ sudo stat -c "%n inode=%i" /run/aranicy/collector-stats.sock
/run/aranicy/collector-stats.sock inode=2149          # was 2117 before
$ python3 - <<PY  # connect probe, as a client would
...
/run/aranicy/collector-stats.sock -> ConnectionRefusedError [Errno 111] Connection refused
/run/aranicy/collector-events.sock -> ConnectionRefusedError [Errno 111] Connection refused
PY
$ sudo kill -9 $(pgrep -f "^/usr/bin/aranicy start"); sleep 20
state=activating daemon=none col=1178
Error: no collector at /run/aranicy/collector-events.sock after 5s: Connection refused (os error 111) (is aranicy-collector.service running?)
aranicy.service: Main process exited, code=exited, status=1/FAILURE
```

Two things are wrong there and both are in the issue: the second instance
never refuses to start (it silently takes the path), and the daemon's error
tells me to check `aranicy-collector.service`, which is running and healthy by
every other measure.

## 7. Reboot in the middle of the work

Reboot with a load running and a rule-applied process alive:

```sh
$ ps -eo pid,comm,nice | grep -E "rebootload|renapp"
    662 rebootload        0
    664 renapp            7
$ sudo ls -la /var/lib/aranicy
-rw-r--r-- 1 root root 3377 Nov 11 10:05 bias-state
$ sudo systemctl reboot
```

After the reboot:

```sh
$ systemctl is-enabled aranicy aranicy-collector; systemctl is-active aranicy aranicy-collector
enabled
disabled
active
active
$ sudo journalctl -u aranicy -u aranicy-collector -b --no-pager -o cat | head -20
Started aranicy collector - scheduler-agnostic BPF task statistics.
Starting aranicy - auto-nice daemon...
2026-11-11T10:06:05.282022Z  WARN kernel does not support latency_nice (sched_setattr LATENCY_NICE); latency_nice policy will be skipped
2026-11-11T10:06:05.288136Z  WARN cgroup "rencg" not created: Permission denied (os error 13)
2026-11-11T10:06:05.338607Z  INFO bias: restored history entries=22
Started aranicy - auto-nice daemon.
$ aranicy dump rules | head -12
{
  "exact": [
    {
      "ioclass": "idle",
      "match": "exact",
      "name": "renapp",
      "nice": 7
    },
    {
      "cgroup": "rencg",
      "match": "exact",
      "name": "rencgapp"
    }
$ ps -eo pid,comm,nice | grep -w renapp | grep -v grep
    483 renapp            7
```

What came back: both units started (aranicy is `enabled`, the collector is
`disabled` but pulled in by `Requires=`), sockets re-created, learned history
restored from disk (22 entries), my rules loaded, new processes niced. What did
not: the previous boot's processes (expected), and — the finding — the
`CPUQuota` of the configured cgroup:

```sh
$ ls -d /sys/fs/cgroup/rencg; cat /sys/fs/cgroup/rencg/cpu.max
/sys/fs/cgroup/rencg
max 100000
$ ( ~/rencgapp 30 & ); sleep 3; cat /proc/$(pgrep -x rencgapp)/cgroup
0::/rencg
$ sudo aranicy --reload; sleep 1; cat /sys/fs/cgroup/rencg/cpu.max
100000 100000
```

`max 100000` means *no quota at all*; the rule is supposed to cap it at 50 % of
the 2 CPUs. The directory is created, matching processes are moved into it, and
nothing caps them. One reload later the quota appears. Filed as
`issues/ren-13-…`; reproduced on three consecutive boots of the current build
(10:06, 10:09, 10:11) and once on the previous build (10:18).

## 8. Install, remove, reinstall

Install is a plain `pacman -U` of the pair, no prompts, nothing enabled:

```sh
$ sudo pacman -U --noconfirm ~/payload/aranicy-collector-…zst ~/payload/aranicy-…zst
installing aranicy-collector...
installing aranicy...
(1/2) Reloading system manager configuration...
```

Removal, with a niced process and live bias state present:

```sh
$ ps -eo pid,comm,nice | grep -E "renapp|renbulk"
   1389 renapp            7
   1391 renbulk           3
$ sudo pacman -R --noconfirm aranicy aranicy-collector
$ sudo ls /var/lib/aranicy
ls: cannot access '/var/lib/aranicy': No such file or directory
$ ls /run/aranicy; ls /run/aranicy.pid
ls: cannot access '/run/aranicy': No such file or directory
ls: cannot access '/run/aranicy.pid': No such file or directory
$ pgrep -f "^/usr/bin/aranicy" || echo none
none
$ ps -eo pid,comm,nice | grep -E "renapp|renbulk"
   1389 renapp            7
   1391 renbulk            3
```

Nothing of the package survives — not the unit, not the enable symlink, not the
bias history: `/var/lib/aranicy` and `/run/aranicy.pid` are gone, so a reinstall
starts from a clean slate (`bias: restored history entries=4` on the next
start). Two leftovers that are mine to reason about, not the package's:

- the *effects* survive: `renapp` keeps nice 7 and `renbulk` keeps the bias-3
  nice it was given, because nice is not a lease and nothing records what was
  changed once `bias-state` is deleted. Any removal story should say this out
  loud; I would want an `aranicy dump applied` that can be replayed before
  removal, and there is none.
- `/sys/fs/cgroup/rencg` (with its `cpu.max`) stays until the next boot. It has
  no members and nothing moves anything into it, so it is inert.

Reboot after removal: nothing comes back — no units, no sockets, no state, no
`systemctl --failed` entries:

```sh
$ pgrep -af aranicy || echo "no aranicy processes"   # only my own bash -c line matched
$ ls /run/aranicy /var/lib/aranicy
ls: cannot access '/run/aranicy': No such file or directory
ls: cannot access '/var/lib/aranicy': No such file or directory
$ systemctl is-enabled aranicy aranicy-collector
not-found
not-found
$ systemctl --failed --no-pager
0 loaded units listed.
```

Reinstall after that is a clean first install.

## 9. A process that renames itself, and a workload that vanishes

`exec -a`, `prctl(PR_SET_NAME)`, and a renamed copy, against a rule for
`renapp` (nice 7):

```sh
$ ( bash -c "exec -a renapp /bin/sleep 25" & ); sleep 2
$ ps -eo pid,ppid,comm,nice,args | grep renapp | grep -v grep
   1720       1 renapp            7 /home/arch/renapp 30
   1738       1 sleep             0 renapp 25
$ ( python3 ~/renprctl.py & ); sleep 2      # prctl(PR_SET_NAME,"renapp"), then sleep
$ ps -eo pid,comm,nice | grep -E "python3|renapp" | grep -v grep
   1720 renapp            7
   1745 renapp            0
$ cp /bin/sleep ~/renapp_copy; ( ~/renapp_copy 25 & ); sleep 2
   1752 renapp_copy       0
$ aranicy dump proc | python3 -c '...'
[{'name': 'renrerun', 'nice': 6, 'pid': 1636}, {'name': 'renapp', 'nice': 7, 'pid': 1720}, {'name': 'renapp_copy', 'nice': 0, 'pid': 1752}]
```

Both evasions fail in the useful direction: a process that impersonates a
privileged rule's name (`argv[0]` or comm) gets nothing, and the only thing
that matches is the executable's own basename. That is the documented
executable-identity model working.

A workload that disappears while the daemon looks at it — 2 000 ruled execs
that exit immediately:

```sh
$ time ( for i in $(seq 1 2000); do ~/renapp 0; done )
real	0m0.774s
$ echo "before dae=1608 col=1607 / after dae=$(pgrep -f "^/usr/bin/aranicy start") col=$(pgrep -x aranicy-collect)"
before dae=1608 col=1607
after  dae=1608 col=1607
$ journal lines: 31 -> 31
$ ( ~/renapp 20 & ); sleep 2; ps -eo pid,comm,nice | grep -w renapp | grep -v grep
   3791 renapp            7
```

No crash, no log storm (the journal did not grow by a single line for 2 000
exec events), and the very next long-lived instance is still niced.

**PID recycling** I could not force here: `cat /proc/sys/kernel/pid_max` →
`4194304`, so within a session a pid is not reused. I am not claiming anything
about it, and the 2 000-exec churn above is the closest thing I could run.

## 10. Two instances at once (not in the brief, but a real thing a user does)

```sh
$ sudo timeout 6 /usr/bin/aranicy start
Error: another aranicy instance is already running (pid 986); if that pid is stale, remove /run/aranicy.pid
second_daemon_exit=1
```

Good. The collector is the opposite:

```sh
$ sudo timeout 8 /usr/bin/aranicy-collectord run
2026-11-11T10:23:40.986726Z  INFO aranicy_collectord: collector serving sched_samples + exec events socket=/run/aranicy/collector-stats.sock events_socket=/run/aranicy/collector-events.sock cadence_ms=500
```

It starts, takes the socket path over, and when it exits it leaves a dead
socket behind for the collector that systemd still reports as active. Filed as
`issues/ren-12-…`; the documented diagnostic `aranicy-collectord probe` is
*not* affected:

```sh
$ sudo timeout 30 /usr/bin/aranicy-collectord probe 2>&1 | head -6
--- 0 rows
--- 8 rows
pid=1391    comm=renbulk          prio=23  wait=0      wake=0      rt=331351783    demand=1017  deliv=1017
pid=1863    comm=aranicy-collect  prio=10  wait=20220  wake=0      rt=26551        demand=1     deliv=0
$ systemctl is-active aranicy aranicy-collector
active
active
```

## What I could not break

- Rule parsing, including non-ASCII names, CRLF-ish junk, prose lines, unknown
  keys and out-of-range numbers: always a file:line message, never a stack
  trace, never silence.
- Reload under a broken file: old rules kept, count reported.
- Non-root use: refused loudly for the daemon, works for inspection.
- A reboot with the package installed: everything comes back except the one
  cgroup quota above.
- Removal: nothing of the package survives a reboot.
- exec-name evasion: does not work, by design and in practice.

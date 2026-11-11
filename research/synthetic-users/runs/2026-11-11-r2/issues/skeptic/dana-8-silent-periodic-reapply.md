# A rule loaded (or reloaded) is applied to processes that were already running, 60 s later and with no log line — including system daemons

- severity: major
- category: behaviour
- environment: Linux 7.2.11-arch1-1 (Arch rolling), aranicy 0.3.0-1 + aranicy-collector 0.3.0-1, 2 vCPU x86_64, 1958 MiB, `check_freq = 60`, `log_applied_rule = false` (both shipped defaults)

## What I was trying to do

Establish the blast radius of the periodic pass after the PID 1 fix landed in
this build (round 1's dana-1 is "gone": a `systemd` rule no longer moves PID 1).
The shipped config describes that pass as harmless:

> Seconds between optional drift-corrector passes (0 disables; the eBPF event
> path is authoritative). Delta-based: only processes whose policy drifted are
> touched, never a full re-apply scan.

I read "drifted" as "was applied a policy and no longer matches it". So I started
a process *before* any rule existed for it, loaded the rule, and watched.

## Steps to reproduce

```
cp /bin/sleep ~/skepticsleep
setsid --fork /home/arch/skepticsleep 300 </dev/null >/dev/null 2>&1
p=$(pgrep -x skepticsleep)                       # already running, no rule matches it, inherits nice 2
echo '{"name": "skepticsleep", "nice": 15}' | sudo tee /etc/ananicy.d/sl.rules
sudo aranicy --reload
J0=$(date +%s)
for i in $(seq 1 7); do sleep 10; awk '{print "t='"'"'$((i*10))s nice="$19}'"'"' /proc/$p/stat; done
sudo journalctl -u aranicy --no-pager --since "@$J0" | grep -c skepticsleep
```

## Expected

Either the pass only touches processes it has already applied a policy to (the
config's claim), or the change is logged — `log_applied_rule` exists and this is
exactly the kind of write a user would want a record of. Round 1's complaint
about PID 1 was accepted; the same event for any other name is still silent.

## Actual

At the first pass after the reload (t = 60 s) the running process is reniced
from 2 to 15. The journal contains no line mentioning it; `grep -c` is 0. With
the shipped `log_applied_rule = false`, a rule that was just written and
reloaded silently changes the priority of every already-running process it
matches, one minute later.

The same mechanism reaches system daemons. I loaded `{"name": "sshd", "nice":
19}` and the live sshd (which never exec'd during the test) was reniced at the
60 s pass — and note that `bias_protect`'s default list contains `sshd`, which
protects it from the learned axis only:

```
$ ... rule {"name": "sshd", "nice": 19} loaded, reloaded ...
sshd before: pid=745 nice=0
t=10s sshd[745] nice=0
...
t=50s sshd[745] nice=0
t=60s sshd[745] nice=19
t=70s sshd[745] nice=19
```

## Evidence

```
$ p=$(pgrep -x skepticsleep); echo "pid=$p nice_before=$(awk '{print $19}' /proc/$p/stat)"
pid=162610 nice_before=2
--- load rule now (process already running) ---
$ echo '{"name": "skepticsleep", "nice": 15}' | sudo tee /etc/ananicy.d/sl.rules; sudo aranicy --reload
sent SIGHUP to aranicy (pid 162598)
t=10s nice=2
t=20s nice=2
t=30s nice=2
t=40s nice=2
t=50s nice=2
t=60s nice=15
t=70s nice=15
--- any journal line about skepticsleep? ---
$ sudo journalctl -u aranicy --no-pager --since "@$J0" | grep -c skepticsleep
0
$ sudo journalctl -u aranicy --no-pager --since "@$J0" | tail -4
Nov 11 09:29:30 su-skeptic-r2 aranicy[162598]: 2026-11-11T09:29:30.757967Z  INFO reloaded 2 rules; bias reconfigured
```

```
$ ... sshd rule ...
sshd before: pid=745 nice=0
t=10s sshd[745] nice=0
t=20s sshd[745] nice=0
t=30s sshd[745] nice=0
t=40s sshd[745] nice=0
t=50s sshd[745] nice=0
t=60s sshd[745] nice=19
t=70s sshd[745] nice=19
restored sshd nice=0
```

The only journal output of the two runs is the reload line itself; nothing names
the processes that were moved.

## What I could not determine

- Whether the pass walks all of `/proc` or a cached set. I can show the effect on
  two processes that never exec'd under the daemon, not the syscall (`strace` is
  not installed and I did not install third-party software).
- Whether `log_applied_rule = true` logs this specific path — I did not enable it
  (it is documented as noisy and I had a 2 vCPU box to keep responsive). If it
  does, the finding narrows to "the default is silent", which is still the state
  a user gets.
- Whether applying a newly-loaded rule to an already-running process is intended
  ananicy-compatibility behaviour. I have no ananicy-cpp to compare with
  (round 1 recorded the same gap). What I can defend is that the config comment
  says the opposite, and that the write is not logged.
- PID 1 is exempt and kernel threads are not touched: 2 rules naming
  `ksoftirqd/0` and `kworker/0:0-events` produced no change on `/proc/15/stat`
  or `/proc/9/stat` over 140 s (14 samples each).

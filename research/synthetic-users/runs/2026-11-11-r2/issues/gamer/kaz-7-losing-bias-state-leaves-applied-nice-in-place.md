# Losing (or corrupting) the bias-state file leaves the processes the bias had already moved at their biased nice, with no record of why and nothing to put them back

- severity: major
- category: safety
- environment: Arch Linux rolling, kernel 7.2.11-arch1-1, aranicy 0.3.0-1 + aranicy-collector 0.3.0-1, 2 vCPU x86_64, 1958 MiB RAM

## What I was trying to do

Find out what happens when the learned-bias state file is damaged, because I do
not want my machine to end up in a state I cannot see or undo. I chose a
CPU-bound process with **no rule at all** (the case the bias invents a policy
for), let the bias move it, then deleted the state file and restarted the daemon.

## Steps to reproduce

```
sudo cp /usr/bin/openssl /usr/local/bin/kazbulk
aranicy match-test kazbulk                       # confirm: no rule
setsid nohup /usr/local/bin/kazbulk speed -seconds 400 rsa2048 >/dev/null 2>&1 </dev/null &
sleep 160
ps -o pid,ni,etime,comm -C kazbulk               # bias has moved it to nice 3
sudo systemctl stop aranicy
sudo rm -f /var/lib/aranicy/bias-state
sudo systemctl start aranicy
sleep 4
aranicy dump bias                                # "state": null
ps -o pid,ni,comm -C kazbulk                     # still 3
sleep 90
ps -o pid,ni,comm -C kazbulk                     # still 3
```

## Expected

If the daemon has no history for a name, it has no policy for it — and a process
that was moved only by learned policy should end up back where it started, or at
the very least the daemon should report that it moved something it can no longer
account for. `COMPATIBILITY.md` says the on-disk state is the record ("`dump
bias` reads it unprivileged"), and `dump proc` is supposed to be the list of what
is applied.

## Actual

1. The bias moves an unruled name to nice 3 in ~2.5 minutes.
2. Deleting the state file is accepted silently (no warning at all on this path —
   the file is simply absent).
3. After the restart, `dump bias` says there is no state, and the process is
   still at nice 3, and stays there. Nothing in the daemon's output mentions it,
   `dump proc` shows `{"nice": 3}`, and there is no rule to change and no bias
   entry to relax. The only way back is `renice` by hand on every pid, which I
   would have to notice first.

The same applies to a corrupt file, which is the more likely way to hit it: the
daemon warns and starts with **empty** history, so every name that was previously
biased is instantly un-recorded while the processes keep the value.

## Evidence

No rule, starts at 0, the bias moves it:

```
$ ./machine 'aranicy match-test kazbulk'
match-test: no rule matches "kazbulk"
$ ./machine 'ps -o pid,ni,comm -C kazbulk'
    PID  NI COMMAND
   2022   0 kazbulk
$ ./machine 'sleep 160; ps -o pid,ni,etime,comm -C kazbulk; sudo journalctl -u aranicy --no-pager | grep "name=kazbulk" | head -3'
    PID  NI     ELAPSED COMMAND
   2022   3       02:52 kazbulk
Nov 11 09:16:56 su-gamer-r2 aranicy[1994]: 2026-11-11T09:16:56.228001Z  INFO bias changed name=kazbulk bias=1 prev=0 class=Bulk cause=Evidence
Nov 11 09:17:26 su-gamer-r2 aranicy[1994]: 2026-11-11T09:17:26.227977Z  INFO bias changed name=kazbulk bias=2 prev=1 class=Bulk cause=Evidence
Nov 11 09:17:56 su-gamer-r2 aranicy[1994]: 2026-11-11T09:17:56.227976Z  INFO bias changed name=kazbulk bias=3 prev=2 class=Bulk cause=Evidence
```
State deleted, daemon restarted, no complaint:

```
$ ./machine 'sudo systemctl stop aranicy; sudo rm -f /var/lib/aranicy/bias-state; sudo systemctl start aranicy; sleep 4; aranicy dump bias 2>/dev/null | head -8; ps -o pid,ni,comm -C kazbulk'
{
  "error": null,
  "note": "on-disk snapshot is debounced; live bias may be up to ~60 s newer",
  "path": "/var/lib/aranicy/bias-state",
  "state": null
}
    PID  NI COMMAND
   2022   3 kazbulk
$ ./machine 'sudo journalctl -u aranicy --no-pager --since "-30s" | tail -4'
Nov 11 09:18:33 su-gamer-r2 aranicy[2125]: 2026-11-11T09:18:33.465777Z  WARN /etc/ananicy.d/70-kazdemo2.rules:2: no JSON object span (missing '{' or '}')
Nov 11 09:18:33 su-gamer-r2 aranicy[2125]: 2026-11-11T09:18:33.465801Z  WARN kernel does not support latency_nice (sched_setattr LATENCY_NICE); latency_nice policy will be skipped
Nov 11 09:18:33 su-gamer-r2 systemd[1]: Started aranicy - auto-nice daemon.
Nov 11 09:18:33 su-gamer-r2 aranicy[2125]: 2026-11-11T09:18:33.467587Z  INFO stats sampler live prev=degraded: starting detail=bound SchedSample via sched_samples at /run/aranicy/collector-stats.sock
```

Still 3 after two drift passes:

```
$ ./machine 'sleep 70; ps -o pid,ni,comm -C kazbulk; aranicy dump proc 2>/dev/null | python3 -c "import json,sys; d=json.load(sys.stdin); print([x for x in d if x[\"pid\"]==2022])"'
    PID  NI COMMAND
   2022   3 kazbulk
[{'name': 'kazbulk', 'nice': 3, 'pid': 2022}]
```

A corrupt file (the likelier accident) is handled the same way — warn, start
empty:

```
$ ./machine 'sudo systemctl stop aranicy; echo "GARBAGE{{{" | sudo tee /var/lib/aranicy/bias-state >/dev/null; sudo systemctl start aranicy; sleep 4; sudo journalctl -u aranicy --no-pager --since "-1min" | grep -viE "kazdemo2.rules" | tail -4'
Nov 11 09:15:26 su-gamer-r2 aranicy[1994]: 2026-11-11T09:15:26.227070Z  WARN kernel does not support latency_nice (sched_setattr LATENCY_NICE); latency_nice policy will be skipped
Nov 11 09:15:26 su-gamer-r2 aranicy[1994]: 2026-11-11T09:15:26.227387Z  WARN bias: cannot load /var/lib/aranicy/bias-state: expected value at line 1 column 1
Nov 11 09:15:26 su-gamer-r2 systemd[1]: Started aranicy - auto-nice daemon.
Nov 11 09:15:26 su-gamer-r2 aranicy[1994]: 2026-11-11T09:15:26.227916Z  INFO stats sampler live prev=degraded: starting detail=bound SchedSample via sched_samples at /run/aranicy/collector-stats.sock
```

## What I could not determine

- How long the stale value lasts. For `kazbulk` it re-learned the same +3 within
  two windows, which hides the problem. A process that exits soon after the state
  loss, or an entry that does not re-ramp (e.g. one that is no longer running
  enough to classify), stays at the biased value for as long as it lives. I did
  not construct that second case — the run above only proves the value is not
  unwound when the record disappears.
- Whether the daemon is *supposed* to derive its applied values from the state
  file at all. It clearly re-derives per drift pass from live history, so an
  entry that vanishes takes its policy with it. `suspicion:` the drift scan only
  acts on names it has a policy for, and after the wipe it has none.
- Whether a version-mismatch refusal (documented) also starts empty or refuses to
  start; I did not test that path.

`workflow this blocks`: "I must be able to undo a decision that hurts one
specific game, without becoming a sysadmin" — if the file that holds the
decisions goes away, the decisions stay in effect and become invisible.

`related`: kaz-5 (round 1) — `dump proc` shows the number, not the reason, so a
stale number is indistinguishable from a deliberate one.

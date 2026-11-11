# Changing a rule only reaches an already-running process for `nice`: `ioclass`, `cgroup` and a removed `nice` field are silently ignored until the process restarts

- severity: minor
- category: behaviour
- environment: Arch Linux rolling, kernel 7.2.11-arch1-1, aranicy 0.3.0-1 + aranicy-collector 0.3.0-1, 2 vCPU x86_64, 1958 MiB RAM

## What I was trying to do

Tune a rule against a process that is already open — change my mind about
`ionice`, add a `cgroup`, and try to undo a `nice` by taking the field out of the
rule. That is how I work: the game is running, I edit the file, reload, look at
the process.

## Steps to reproduce

Three separate one-field changes, each followed by a reload and a 75 s wait
(`check_freq = 60`, so at least one drift pass):

```
# A: add ionice to a running process, and take the nice field out at the same time
sudo cp /usr/bin/sleep /usr/local/bin/kazdrop
echo '{"name": "kazdrop", "nice": -7}' | sudo tee /etc/ananicy.d/97-kazdrop.rules
sudo systemctl reload aranicy
setsid nohup /usr/local/bin/kazdrop 300 >/dev/null 2>&1 </dev/null &
sleep 2; ps -o pid,ni,comm -C kazdrop
echo '{"name": "kazdrop", "ioclass": "idle"}' | sudo tee /etc/ananicy.d/97-kazdrop.rules
sudo systemctl reload aranicy
sleep 75
P=$(pgrep -x kazdrop); ps -o pid,ni,comm -p $P; echo "kazdrop ionice: $(ionice -p $P)"

# B: add a cgroup to a running process
sudo cp /usr/bin/sleep /usr/local/bin/kaztype
echo '{"name": "kaztype", "nice": -4}' | sudo tee /etc/ananicy.d/96-kaztype.rules
sudo systemctl reload aranicy
setsid nohup /usr/local/bin/kaztype 120 >/dev/null 2>&1 </dev/null &
sleep 2
echo '{"name": "kaztype", "nice": -4, "cgroup": "kazgame"}' | sudo tee /etc/ananicy.d/96-kaztype.rules
sudo systemctl reload aranicy
sleep 75
cat /proc/$(pgrep -x kaztype)/cgroup
```

(`kazgame` is a cgroup defined in `/etc/ananicy.d/kaz.cgroups` as
`{"cgroup": "kazgame", "CPUQuota": 50}`; a freshly started process with that rule
**is** moved into it, so the cgroup machinery works.)

## Expected

Either all rule fields are re-applied to a live process on reload/drift, or none
are and the doc says so. `COMPATIBILITY.md` says reload "re-reads `ananicy.conf`
and the rules tree" and `check_freq` "only processes whose policy drifted are
touched" — a process whose *rule* changed has drifted from what the rule now
says.

## Actual

- `nice` **is** corrected on a running process (two positive controls below).
- `ioclass` added to a running process: nothing happens, `ionice` still `none`.
- `nice` removed from the rule of a running process: the old value stays, forever.
- `cgroup` added to a running process: nothing happens, still in the session scope.

So a rule edit reaches a live process for one field out of the eight in the rule
format, and for the others the rule file and the machine disagree with no
indication.

## Evidence

A — one process, rule changed from `{"nice": -7}` to `{"ioclass": "idle"}`:

```
$ ./machine 'P=$(pgrep -x kazdrop|tail -1); ps -o pid,ni,comm -p $P'
    PID  NI COMMAND
   2233  -7 kazdrop
$ ./machine 'P=$(pgrep -x kazdrop|tail -1); ps -o pid,ni,comm -p $P; echo "kazdrop ionice: $(ionice -p $P)"'
    PID  NI COMMAND
   2233  -7 kazdrop
kazdrop ionice: none: prio 0
```

75 s and a reload later, neither the new `ioclass` nor the removal of `nice` took
effect. The rule as loaded says:

```
$ ./machine 'aranicy dump rules | python3 -c "import json,sys; print([r for r in json.load(sys.stdin)[\"exact\"] if r[\"name\"]==\"kazdrop\"])"'
[{'ioclass': 'idle', 'match': 'exact', 'name': 'kazdrop'}]
```

B — cgroup added to a running process:

```
$ ./machine 'QT=$(pgrep -x kaztype|tail -1); cat /proc/$QT/cgroup'
0::/user.slice/user-1000.slice/session-55.scope
```

…75 s after the reload that added `"cgroup": "kazgame"` to its rule. A process
started *after* the same rule existed is moved:

```
$ ./machine 'sudo cp /usr/bin/sleep /usr/local/bin/kazcg; printf "%s\n" "{\"name\": \"kazcg\", \"cgroup\": \"kazgame\"}" | sudo tee /etc/ananicy.d/99-kazcg.rules; sudo systemctl reload aranicy; sleep 1; setsid nohup /usr/local/bin/kazcg 60 >/dev/null 2>&1 </dev/null & sleep 2; P=$(pgrep -x kazcg|tail -1); echo "pid=$P"; cat /proc/$P/cgroup; cat /sys/fs/cgroup/kazgame/cpu.max'
{"name": "kazcg", "cgroup": "kazgame"}
pid=2489
0::/kazgame
100000 100000
```

Positive controls, so this is not "drift never works":

```
$ ./machine 'ps -o pid,ni,comm -C kazdemo'      # rule said nice 5, at exec
    PID  NI COMMAND
   1339   5 kazdemo
$ ./machine 'echo "{\"name\": \"kazdemo\", \"nice\": 0}" | sudo tee /etc/ananicy.d/60-kazdemo.rules; sudo systemctl reload aranicy; sleep 70; ps -o pid,ni,comm -C kazdemo; sudo journalctl -u aranicy --no-pager | grep "rule=\"kazdemo\" pid=1339"'
{"name": "kazdemo", "nice": 0}
    PID  NI COMMAND
   1339   0 kazdemo
Nov 11 09:08:31 su-gamer-r2 aranicy[1165]: 2026-11-11T09:08:31.641023Z  INFO applied rule rule="kazdemo" pid=1339 bias=0 route="drift"
```

```
$ ./machine 'ps -o pid,ni,comm -C kazdemo2'     # no rule when it started
    PID  NI COMMAND
   1476   0 kazdemo2
$ ./machine 'printf "%s\n" "{\"name\": \"kazdemo2\", \"nice\": -8}" "this is not json" | sudo tee /etc/ananicy.d/70-kazdemo2.rules; sudo systemctl reload aranicy; sleep 70; ps -o pid,ni,comm -C kazdemo2'
{"name": "kazdemo2", "nice": -8}
this is not json
    PID  NI COMMAND
   1476  -8 kazdemo2
```

## What I could not determine

- Whether `sched`, `oom_score_adj`, `cpuset` and `autogroup` behave like `nice`
  or like `ioclass`. I tested `ioclass` and `cgroup` only; the two positive
  controls both happened to be `nice`.
- Whether the drift pass *verifies* ionice/cgroup at all or simply never
  considers them. `suspicion:` the drift comparison is on the nice value only,
  because that is the one number both paths share.

`workflow this blocks`: "I tune a rule while the thing is running and look at the
result" — for anything except nice the answer is always "restart the process",
and nothing tells me that.

`related`: kaz-5 (round 1) — this is another case where the number on screen and
the file on disk disagree with no way to ask why.

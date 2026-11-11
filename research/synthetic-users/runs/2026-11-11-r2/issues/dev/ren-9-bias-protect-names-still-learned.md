# Names listed in `bias_protect` are still learned, logged and relaxed — only the `setpriority` is suppressed

- severity: minor
- category: behaviour
- environment: Arch Linux cloud image, kernel 7.2.11-arch1-1, aranicy 0.3.0-1 +
  aranicy-collector 0.3.0-1 (build 2026-11-11 08:59 UTC), 2 vCPU / 1958 MiB / no display

## What I was trying to do

Trust the one config key that tells the learned-nice engine which names it
must not touch. `bias_protect` is the only guard between "the engine learns my
build is bulk" and "the engine learns my session manager is bulk", and the
shipped comment is unambiguous: *"Names the learned bias must never move (comm
prefixes …) Rules still apply to these names - this gates the learned axis
only."* `COMPATIBILITY.md` repeats it: *"The learned axis additionally skips any
comm matching `bias_protect`"*.

I expected those names to have **no history entry and no journal line**, so that
the shared global step budget ("one bias step globally per 5 s") is not spent on
them and `dump bias` lists only names that can actually be moved.

## Steps to reproduce

Start a CPU-bound process whose comm matches a protected prefix, alongside a
control whose comm matches nothing. `sshd` is on the shipped list; both files
are copies of `python3`, so the only difference is the name.

```sh
cp /usr/bin/python3 ~/sshd2
cp /usr/bin/python3 ~/zzctl
nohup setsid ~/sshd2 -c 'while True: pass' >/dev/null 2>&1 &
nohup setsid ~/zzctl -c 'while True: pass' >/dev/null 2>&1 &
sleep 180
ps -eo pid,comm,nice | grep -E 'sshd2|zzctl'
sudo journalctl -u aranicy --no-pager -o cat | grep -E 'name=(sshd2|zzctl)'
```

## Expected

No `sshd2` entry and no `bias changed name=sshd2` line (it is on the list); the
control `zzctl` may be learned and moved.

## Actual

`sshd2` is classified, entry created, and its bias walked 0 → 3 with three
`bias changed` lines, exactly like an unprotected name. The only thing the
protection changes is that the nice value is never written: `sshd2` stays at 0
while the control ends up niced.

The same happens to the shipped system service names, continuously, on an
otherwise idle machine:

```
$ ps -eo pid,comm,nice | grep -E "dbus-broker|systemd-journal|systemd-homed|systemd-logind|sshd|systemd-udevd"
    205 systemd-journal   0
    261 systemd-udevd     0
    320 dbus-broker-lau   0
    330 dbus-broker       0
    333 systemd-homed     0
    334 systemd-logind    0
    745 sshd              0
 719614 sshd9             0
 719746 sshd-session      0
 719749 sshd-session      0
$ sudo journalctl -u aranicy --no-pager -o cat | grep "bias changed" | grep -E "name=(systemd|dbus-broker)"
2026-11-11T09:12:12.305387Z  INFO bias changed name=systemd bias=1 prev=0 class=Bulk cause=Evidence
2026-11-11T09:12:42.305385Z  INFO bias changed name=systemd bias=2 prev=1 class=Bulk cause=Evidence
2026-11-11T09:13:12.305580Z  INFO bias changed name=systemd bias=3 prev=2 class=Bulk cause=Evidence
2026-11-11T09:22:04.081423Z  INFO bias changed name=dbus-broker bias=-1 prev=0 class=Responsive cause=Evidence
2026-11-11T09:24:04.092041Z  INFO bias changed name=systemd-journal bias=-1 prev=0 class=Responsive cause=Evidence
2026-11-11T09:25:04.081572Z  INFO bias changed name=systemd-homed bias=-1 prev=0 class=Responsive cause=Evidence
2026-11-11T09:32:48.430031Z  INFO bias changed name=dbus-broker bias=0 prev=-1 class=Responsive cause=Relax
2026-11-11T09:32:53.430038Z  INFO bias changed name=systemd bias=2 prev=3 class=Responsive cause=Relax
2026-11-11T09:32:58.430135Z  INFO bias changed name=systemd-homed bias=0 prev=-1 class=Responsive cause=Relax
2026-11-11T09:33:03.430029Z  INFO bias changed name=systemd-journal bias=0 prev=-1 class=Responsive cause=Relax
2026-11-11T09:37:53.430019Z  INFO bias changed name=systemd bias=1 prev=2 class=Responsive cause=Relax
```

`systemd` is walked all the way to +3 with `cause=Evidence`, on an otherwise
idle machine, while PID 1 stays at nice 0. The relax (`cause=Relax`) steps run
for protected names too — four of them inside 16 seconds
(09:32:48–09:33:03), and again at 09:37:53 — so the guard does not save the
budget on the way back down either.

## Evidence

The protection is real for the *actuation*: no protected name ever changed its
nice, while an identical unprotected workload in the same instant did. Same
binary, same load, same demand; only the name differs (`sshd` is on the list,
`zzctl` is not):

```
$ ps -eo pid,comm,nice | grep -E "sshd9|zzctl"
 385538 zzctl             3
 719614 sshd9             0
$ aranicy dump proc | python3 -c 'import json,sys;print([e for e in json.load(sys.stdin) if e["name"] in ("sshd9","zzctl")])'
[{'name': 'zzctl', 'nice': 3, 'pid': 385538}, {'name': 'sshd9', 'nice': 0, 'pid': 719614}]
$ sudo aranicy-collectord probe | grep -E "sshd9|zzctl"
pid=385538  comm=zzctl            prio=23  wait=0      wake=0      rt=2343674      demand=1024  deliv=681  
pid=719614  comm=sshd9            prio=20  wait=0      wake=0      rt=2472911      demand=1023  deliv=681  
```

(`prio` in `probe` is `nice + 20`, i.e. 23 = nice 3 and 20 = nice 0.)

The learning itself is what leaks. Earlier in the same session, `sshd2` (also
`sshd*`) was classified and ramped like any other name, while never being
applied:

```
$ sudo journalctl -u aranicy --no-pager -o cat | grep -E "name=(sshd2|zzctl)"
2026-11-11T09:17:12.305359Z  INFO bias changed name=sshd2 bias=1 prev=0 class=Bulk cause=Evidence
2026-11-11T09:17:42.305911Z  INFO bias changed name=sshd2 bias=2 prev=1 class=Bulk cause=Evidence
2026-11-11T09:18:12.305394Z  INFO bias changed name=sshd2 bias=3 prev=2 class=Bulk cause=Evidence
2026-11-11T09:20:04.081686Z  INFO bias changed name=zzctl bias=1 prev=0 class=Bulk cause=Evidence
2026-11-11T09:20:34.083494Z  INFO bias changed name=zzctl bias=2 prev=1 class=Bulk cause=Evidence
2026-11-11T09:21:04.081726Z  INFO bias changed name=zzctl bias=3 prev=2 class=Bulk cause=Evidence
```
$ aranicy dump bias | python3 -c '...'
aranicy bias= 3 cls= responsive conf= 0.97
dbus-broker bias= -1 cls= responsive conf= 0.96
sshd bias= 0 cls= unknown conf= 0.0
sshd2 bias= 3 cls= bulk conf= 0.98
systemd bias= 3 cls= responsive conf= 0.97
systemd-homed bias= -1 cls= responsive conf= 0.74
systemd-journal bias= -1 cls= responsive conf= 0.92
```

And the budget accounting for the whole session, from the journal alone:

```
$ sudo journalctl -u aranicy --no-pager -o cat | grep -c "bias changed"
33
$ sudo journalctl -u aranicy --no-pager -o cat | grep "bias changed" | grep -cE "name=(systemd|systemd-homed|systemd-journal|dbus-broker|sshd2) "
14
```

14 of the 33 global steps in this session (which began on a fresh install) went
to names that can never be moved. At one global step per 5 s that is ~70 s of
the engine's single-step budget spent on no-ops, ahead of the names that do need
a step.

## What I could not determine

- Whether the entry is deliberately kept (so the history shows "we looked") or
  is an oversight. The config comment and `COMPATIBILITY.md` both say *skips*,
  so from the outside the state contradicts both.
- Why my clean re-run (`~/sshd9`, started alone with two unrelated CPU-bound
  controls, 170 s) produced no `bias changed` line at all for any of the three
  names, while the 09:17 run classified `sshd2` within 66 s. I did not
  instrument the threshold; the learning observation above is from the run where
  the engine was demonstrably classifying. The 09:36 A/B above therefore proves
  the actuation split only; the ramp itself is the 09:17 journal.

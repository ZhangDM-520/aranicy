# The daemon re-niced systemd (PID 1) and journald to +2 on its own and never put them back

- severity: major
- category: safety
- environment: Arch Linux rolling, kernel 7.2.11-arch1-1, aranicy 0.3.0-1 + aranicy-collector 0.3.0-1, 2 vCPU x86_64, 1958 MiB RAM

## What I was trying to do

Install it, leave it running unattended the way I would on my desktop (a CPU-bound
process standing in for the game, plus normal shell work), and then check what it had
actually changed on the machine. I had written two rules, one for `kazgame` and one for
`openssl`. I had written **no** rule for `systemd`, `systemd-journald` or anything else.

## Steps to reproduce

```
./machine 'cd ~/payload && sudo pacman -U --noconfirm aranicy-0.3.0-1-x86_64.pkg.tar.zst aranicy-collector-0.3.0-1-x86_64.pkg.tar.zst'
./machine 'sudo systemctl enable --now aranicy'
# leave the box alone for ~6 minutes with an ordinary workload running
# (I had a CPU-bound /usr/local/bin/kazgame = openssl speed, plus repeated
#  ssh shell commands; nothing unusual, no rules for any system daemon)
./machine 'ps -o pid,ni,comm -p 1,205'
./machine 'sudo journalctl -u aranicy --no-pager | grep "pid=1 "'
```

## Expected

Processes I never wrote a rule for are left alone. In particular PID 1 and
`systemd-journald` keep the priority the distribution gave them (0), because the only
policy input is a rules file that does not mention them.

## Actual

PID 1 (`systemd`) and `systemd-journald` (PID 205) end up at nice **2** and stay there —
through a reload, a stop and a start — until a human reniced them by hand. The journal
shows the daemon doing it, on its own, through the drift route, with a learned bias of 2
and no matching rule:

```
Nov 11 08:21:19 su-gamer-r1 aranicy[1126]: 2026-11-11T08:21:19.480336Z  INFO applied rule rule="systemd" pid=1 bias=2 route="drift"
Nov 11 08:21:19 su-gamer-r1 aranicy[1126]: 2026-11-11T08:21:19.480925Z  INFO applied rule rule="systemd-journald" pid=205 bias=2 route="drift"
```

It happened while the daemon was deciding that `systemd` is `Bulk`:

```
Nov 11 08:20:34 su-gamer-r1 aranicy[1126]: 2026-11-11T08:20:34.448280Z  INFO bias changed name=systemd bias=1 prev=0 class=Bulk cause=Evidence
Nov 11 08:21:14 su-gamer-r1 aranicy[1126]: 2026-11-11T08:21:14.446873Z  INFO bias changed name=systemd bias=2 prev=1 class=Bulk cause=Evidence
```

Nothing ever puts it back — not a reload, not a stop, not a start (command and output in
Evidence).

The only way back is to renice by hand (`sudo renice -n 0 -p 1`, which I did, and which
stuck because that name's bias had decayed back to 0 by then). I reniced PID 1 only;
`systemd-journald` is **still at 2** as I write this, because nothing told me it had been
touched and no part of the package puts it back.

## Evidence

PID 1 was at 0 while the daemon was already running, before the bias ramp — this is
`aranicy dump proc` taken shortly after the service came up:

```
$ ./machine 'aranicy dump proc 2>&1 | head -7'
[
  {
    "name": "systemd",
    "nice": 0,
    "pid": 1
  },
```

~5 minutes later, and ever since:

```
$ ./machine 'ps -o pid,ni,comm -p 1,205'
    PID  NI COMMAND
      1   2 systemd
    205   2 systemd-journal
```

The transition, from the daemon's own journal:

```
$ ./machine 'sudo journalctl -u aranicy --no-pager | grep -E "pid=1 |name=systemd "'
Nov 11 08:20:34 su-gamer-r1 aranicy[1126]: 2026-11-11T08:20:34.448280Z  INFO bias changed name=systemd bias=1 prev=0 class=Bulk cause=Evidence
Nov 11 08:21:14 su-gamer-r1 aranicy[1126]: 2026-11-11T08:21:14.446873Z  INFO bias changed name=systemd bias=2 prev=1 class=Bulk cause=Evidence
Nov 11 08:21:19 su-gamer-r1 aranicy[1126]: 2026-11-11T08:21:19.480336Z  INFO applied rule rule="systemd" pid=1 bias=2 route="drift"
```

Not reverting, verbatim:

```
$ ./machine 'echo "before: PID1=$(ps -o ni= -p 1) journald=$(ps -o ni= -p 205)"; sudo systemctl reload aranicy; sleep 3; echo "after reload: PID1=$(ps -o ni= -p 1) journald=$(ps -o ni= -p 205)"; sudo systemctl stop aranicy; sleep 2; echo "after stop:   PID1=$(ps -o ni= -p 1) journald=$(ps -o ni= -p 205)"; sudo systemctl start aranicy; sleep 3; echo "after start:  PID1=$(ps -o ni= -p 1) journald=$(ps -o ni= -p 205)"'
before: PID1=  2 journald=  2
after reload: PID1=  2 journald=  2
after stop:   PID1=  2 journald=  2
after start:  PID1=  2 journald=  2
```

## What I could not determine

This is **not reproducible on demand — the trigger is workload-dependent, the effect is
durable.** I ran a second, fresh-state session (bias history moved aside, shipped config,
service restarted, ~7 minutes with the same CPU-bound process) and in that session
`systemd` stayed `responsive` with bias 0, so PID 1 was not touched again. The effect of
the first session is still on the machine. So I cannot tell the project "do X and it
happens every time"; I can only tell them "run it for a while with a mixed load and it
can pick init".

I did not determine what workload pushes `systemd` from `responsive` to `Bulk` — the
classification moves with the rest of the running population, and I only saw the other
side of that flip (`sshd-session` went `Bulk`→+3 in the first session and `Responsive`→−1
in the second).

I could not see any configuration that limits the daemon to a list of names I care
about: there is no rule key and no documented config key for "only manage these names"
or "never touch this name" (see kaz-2).

Not a frame-rate claim: I am not saying nice 2 on PID 1 caused any stutter. I am saying
a process-priority daemon changed the priority of init and its journal, without being
asked, and left it changed.

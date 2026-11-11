# `aranicy --reload` signals whatever pid `/run/aranicy.pid` names and reports success, and `aranicy start` refuses to run while that pid is alive — neither checks that the pid belongs to an aranicy process

- severity: minor
- category: safety
- environment: Linux 7.2.11-arch1-1 (Arch rolling), aranicy 0.3.0-1 + aranicy-collector 0.3.0-1, 2 vCPU x86_64, 1958 MiB

## What I was trying to do

Adversarial pass on the pid file. A clean `systemctl stop` removes
`/run/aranicy.pid`; a `kill -9` does not (see below), so on a busy box the file
can outlive the process it names and name a recycled pid instead.

## Steps to reproduce

```
sudo systemctl stop aranicy                       # file is removed on a clean stop
sudo sh -c 'echo <pid-of-any-live-process> > /run/aranicy.pid'
sudo aranicy --reload                             # "sent SIGHUP to aranicy (pid <pid>)"
sudo aranicy start                                # refuses: "another aranicy instance is already running"
```

## Expected

Either the process is identified before it is signalled (`/proc/<pid>/exe`, or
a comm check), or the tool says it cannot verify and refuses. Sending SIGHUP to
an arbitrary process because a file says so, and printing success, is the kind
of failure that looks like it worked.

## Actual

`--reload` sends SIGHUP to the pid in the file with no verification, prints
`sent SIGHUP to aranicy (pid N)` and exits 0. In my test the target was a
`bash`; the session died and my SSH connection closed (exit 255 on the client
side) — that is the real-world shape of the bug, not a contrived one. The
second-instance guard in `start` also consults only liveness, and its remedy is
a message telling the operator to delete the file by hand:

```
Error: another aranicy instance is already running (pid 1516); if that pid is
stale, remove /run/aranicy.pid
```

That is honest but requires the operator to know that the pid is stale.

The file itself is `0600 root`, so planting it is a root action; the reachable
version of this is a `kill -9` (which leaves the file) followed by pid reuse.

## Evidence

```
$ sudo kill -9 1516
$ sleep 1; sudo cat /run/aranicy.pid
1516                                   # survived the SIGKILL; the pid is now free
$ sudo sh -c 'echo 1672 > /run/aranicy.pid'
$ sudo aranicy --reload
sent SIGHUP to aranicy (pid 1672)      # 1672 was a `bash`, not aranicy
$ echo $?                              # client died with 255: the SIGHUP landed
```

```
$ sudo aranicy start                  # with the same file in place, live pid
Error: another aranicy instance is already running (pid 1516); if that pid is stale, remove /run/aranicy.pid
```

For contrast, a clean stop does remove the file, and systemd's own restart
after a `kill -9` rewrites it correctly:

```
$ sudo systemctl stop aranicy; sudo cat /run/aranicy.pid
cat: /run/aranicy.pid: No such file or directory
$ sudo kill -9 <daemon>; sleep 13; sudo cat /run/aranicy.pid; systemctl is-active aranicy
1645
active
```

## What I could not determine

- I did **not** demonstrate natural pid reuse: I could not make the kernel hand
  out a specific pid within a bounded time, so the reproduction plants the
  value. The precondition (stale file + recycled pid) is argued from the
  observed behaviour of `kill -9`, not from a run in which the kernel did the
  recycling.
- How likely this is in practice. systemd `Restart=always` with `RestartSec=10`
  rewrites the file within 10 s of an unclean death, so the window is small
  unless the unit is also failing to start.

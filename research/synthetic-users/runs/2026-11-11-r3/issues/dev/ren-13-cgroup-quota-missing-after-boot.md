# After a cold boot the configured cgroup is created and used but its `CPUQuota` is not applied, and the only warning says the cgroup was "not created"

- severity: major
- category: behaviour
- environment: Arch Linux cloud image, kernel 7.2.11-arch1-1, aranicy 0.3.0-1 +
  aranicy-collector 0.3.0-1 (build 2026-11-11 09:52 UTC), 2 vCPU / 1958 MiB / no display

## What I was trying to do

Use a `.cgroups` definition to keep one workload inside a CPU cap, which is the
part of the tool that survives the bias engine being wrong: I write
`{"cgroup": "rencg", "CPUQuota": 50}` and `{"name": "rencgapp", "cgroup": "rencg"}`,
and after that no `rencgapp` should ever get more than half the machine, no
matter what classifies what.

I expect that cap to be in place for the life of the machine, including the
first daemon start after a reboot — a boot is exactly when a build machine
starts building.

## Steps to reproduce

```sh
sudo pacman -U --noconfirm ~/payload/aranicy-collector-0.3.0-1-x86_64.pkg.tar.zst \
                            ~/payload/aranicy-0.3.0-1-x86_64.pkg.tar.zst
sudo systemctl enable aranicy                      # enabled, do not start yet
sudo install -m 644 ~/adv/cg/zz-cg.rules /etc/ananicy.d/zz-cg.rules
sudo install -m 644 ~/adv/cg/zz-cg.cgroups /etc/ananicy.d/zz-cg.cgroups
                                         # {"cgroup": "rencg", "CPUQuota": 50}
sudo reboot
sudo journalctl -u aranicy -b --no-pager -o cat | grep -i cgroup
cat /sys/fs/cgroup/rencg/cpu.max
cp /bin/sleep ~/rencgapp; ( ~/rencgapp 30 & ); sleep 3
cat /proc/$(pgrep -x rencgapp)/cgroup
```

## Expected

`cpu.max` equals `100000 100000` (50 % of 2 CPUs: quota = 50/100 × 2 × 100000 µs
per 100 ms period, as `COMPATIBILITY.md` documents), or, if the cgroup could not
be set up, no `rencgapp` is moved into a cgroup that does not have the quota,
and the warning says the quota was not applied.

## Actual

- `cpu.max` is `max 100000`: **no quota at all**. The process inside the cgroup
  is unlimited.
- The warning says `cgroup "rencg" not created: Permission denied (os error 13)`
  — but the directory *does* exist and the process *is* moved into it. The
  message describes something that did not happen, so it sends the reader
  looking for the wrong thing.
- Nothing is retried within a minute of that daemon start: I read `cpu.max` at
  uptime ~1 min in two different boots (10:09:58 and 10:11:30) and both still
  said `max 100000`; the quota appeared only after a reload or restart
  (`aranicy --reload`), which a machine that has just booted has no reason to
  do.
- The failure is per-boot, not per-install: a `systemctl enable --now` start on
  a booted machine creates the cgroup correctly, and a reload creates it
  correctly. Only the start that happens during boot gets it wrong.

## Evidence

First cold boot (10:06), post-boot state before anything else touched it:

```
$ sudo journalctl -u aranicy -u aranicy-collector -b --no-pager -o cat | head -20
Started aranicy collector - scheduler-agnostic BPF task statistics.
Starting aranicy - auto-nice daemon...
2026-11-11T10:06:05.282022Z  WARN kernel does not support latency_nice (sched_setattr LATENCY_NICE); latency_nice policy will be skipped
2026-11-11T10:06:05.288136Z  WARN cgroup "rencg" not created: Permission denied (os error 13)
2026-11-11T10:06:05.338607Z  INFO bias: restored history entries=22
Started aranicy - auto-nice daemon.
```

The directory exists, and the ctime is the same instant as the warning:

```
$ stat -c "%n ctime=%z" /sys/fs/cgroup/rencg
/sys/fs/cgroup/rencg ctime=2026-11-11 10:06:05.287602045 +0000
```

Second cold boot (10:11), this time reading the quota before any reload:

```
$ sudo journalctl -u aranicy -b --no-pager -o cat | grep -iE "cgroup"
2026-11-11T10:11:30.307217Z  WARN cgroup "rencg" not created: Permission denied (os error 13)
$ ls -d /sys/fs/cgroup/rencg; echo "cpu.max=$(cat /sys/fs/cgroup/rencg/cpu.max 2>&1)"
/sys/fs/cgroup/rencg
cpu.max=max 100000
$ cat /sys/fs/cgroup/cgroup.subtree_control
cpu memory pids
$ ( ~/rencgapp 40 >/dev/null 2>&1 & ); sleep 3; P=$(pgrep -x rencgapp); echo "pid=$P"; cat /proc/$P/cgroup
pid=477
0::/rencg
```

A plain ruled process is unaffected, so this is specific to the cgroup path:

```
$ ( ~/renapp 20 >/dev/null 2>&1 & ); sleep 2; ps -eo pid,comm,nice | grep -w renapp | grep -v grep
    482 renapp            7
```

After a reload the same directory gets the quota, with no new log line:

```
$ sudo aranicy --reload; sleep 1; echo "cpu.max=$(cat /sys/fs/cgroup/rencg/cpu.max)"
cpu.max=100000 100000
```

When the cgroup is created by a reload from scratch, it is correct:

```
$ sudo rmdir /sys/fs/cgroup/rencg; sudo aranicy --reload; sleep 1
$ ls -d /sys/fs/cgroup/rencg; echo "cpu.max=$(cat /sys/fs/cgroup/rencg/cpu.max 2>&1)"
/sys/fs/cgroup/rencg
cpu.max=100000 100000
$ cat /proc/$(pgrep -x rencgapp)/cgroup
0::/rencg
```

Three consecutive boots of the current build show the same thing (10:06, 10:09,
10:11), and so does the previous build (10:18), so it is not new in this build:

```
2026-11-11T10:18:40.187387Z  WARN cgroup "rencg" not created: Permission denied (os error 13)
$ cat /sys/fs/cgroup/rencg/cpu.max
max 100000
$ cat /proc/$(pgrep -x rencgapp)/cgroup
0::/rencg
```

Removal does not clean the directory either (inert, but it is the same object):

```
$ sudo pacman -R --noconfirm aranicy aranicy-collector
$ ls -d /sys/fs/cgroup/rencg; echo "cpu.max=$(cat /sys/fs/cgroup/rencg/cpu.max)"
/sys/fs/cgroup/rencg
cpu.max=100000 100000
```

## What I could not determine

- Why a boot-time start differs from a later start. I can only see the
  difference in outcome: the boot start logs `Permission denied (os error 13)`,
  a start 60 s later (or a reload at any time) does not. I did not trace the
  syscalls, so I am not claiming a cause. `suspicion:` the boot start races the
  delegation of the cpu controller, but that is a guess and I did not test it.
- Whether the failure is in `mkdir`, in writing `cpu.max`, or in the delegation
  of the controller; the message says "not created" while the directory exists
  at the same millisecond, so the message is not a reliable description of
  whichever step failed.
- Whether any other `.cgroups` field (there is only `CPUQuota` today) would
  fail the same way.

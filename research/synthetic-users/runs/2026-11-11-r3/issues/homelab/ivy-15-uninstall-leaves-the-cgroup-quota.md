# Uninstalling leaves the cgroup it created (with its CPU quota) in `/sys/fs/cgroup`, and the processes inside stay throttled until reboot

- severity: minor
- category: packaging
- environment: Linux 7.2.11-arch1-1 (Arch rolling, cloud image), aranicy 0.3.0-1 + aranicy-collector 0.3.0-1, 2 vCPU x86_64, 1958 MiB RAM

## What I was trying to do

Remove the package and be sure nothing it installed still influences priority.
I already removed `/etc/ananicy.d/*.cgroups` and `*.rules` in the test, so the
cgroup definition was gone from disk — the kernel state it created was not.

## Steps to reproduce

```
sudo pacman -U --noconfirm ~/payload/aranicy-collector-0.3.0-1-x86_64.pkg.tar.zst \
                         ~/payload/aranicy-0.3.0-1-x86_64.pkg.tar.zst
sudo systemctl enable --now aranicy
printf '%s\n' '{"cgroup": "ivymedia", "CPUQuota": 50}' | sudo tee /etc/ananicy.d/ivy-90-media.cgroups
printf '%s\n' '{"name": "xz", "cgroup": "ivymedia"}' | sudo tee /etc/ananicy.d/ivy-50-xz.rules
sudo systemctl reload aranicy; sleep 2
setsid xz -9 -c /home/arch/work/big.bin >/dev/null 2>&1 &
X=$(pgrep -nx xz)
cat /sys/fs/cgroup/ivymedia/cpu.max
cat /proc/$X/cgroup
sudo pacman -R --noconfirm aranicy aranicy-collector
ls -d /sys/fs/cgroup/ivymedia
cat /sys/fs/cgroup/ivymedia/cpu.max
cat /proc/$X/cgroup
```

## Expected

After the uninstall, either the cgroup is removed (no processes left in it), or
the package says it left a cgroup behind. "A leftover config that keeps
affecting priority" is on my blocker list; this is the runtime half of it.

## Actual

The cgroup, its quota and the process inside it all survive the uninstall:

```
$ cat /sys/fs/cgroup/ivymedia/cpu.max
100000 100000
$ cat /proc/3331/cgroup
0::/ivymedia
$ sudo pacman -R --noconfirm aranicy aranicy-collector
...
removing aranicy...
removing aranicy-collector...
[exit=0]
$ ls -d /sys/fs/cgroup/ivymedia
/sys/fs/cgroup/ivymedia
$ cat /sys/fs/cgroup/ivymedia/cpu.max
100000 100000
$ cat /proc/3331/cgroup
0::/ivymedia
```

The xz process is still inside the cgroup and still throttled to half the box
after the package is gone. A reboot clears it; a package removal does not.

The same happens with the previous build, so it is not a regression:

```
$ ls -d /sys/fs/cgroup/ivymedia
/sys/fs/cgroup/ivymedia
$ cat /sys/fs/cgroup/ivymedia/cpu.max
100000 100000
$ cat /proc/<xz>/cgroup
0::/ivymedia
```

## Evidence

`raw/adv5.txt`, section F1 (current build):

```
$ printf '%s\n' '{"cgroup": "ivymedia", "CPUQuota": 50}' | sudo tee /etc/ananicy.d/ivy-90-media.cgroups
{"cgroup": "ivymedia", "CPUQuota": 50}
$ printf '%s\n' '{"name": "xz", "cgroup": "ivymedia"}' | sudo tee /etc/ananicy.d/ivy-50-xz.rules
{"name": "xz", "cgroup": "ivymedia"}
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

and `raw/prevfinal.txt`, section P2 (previous build):

```
$ cat /sys/fs/cgroup/ivymedia/cpu.max
100000 100000
$ cat /proc/<xz>/cgroup
0::/ivymedia
$ sudo pacman -R --noconfirm aranicy aranicy-collector
...
$ ls -d /sys/fs/cgroup/ivymedia
/sys/fs/cgroup/ivymedia
$ cat /sys/fs/cgroup/ivymedia/cpu.max
100000 100000
$ cat /proc/<xz>/cgroup
0::/ivymedia
```

## What I could not determine

- Whether the package could remove the cgroup at all: removing a non-empty
  cgroup needs the processes moved back to the parent (and the daemon is being
  stopped at that moment), and a cgroup created by a daemon is arguably
  runtime state that a reboot owns. If the answer is "reboot required", I want
  that in the uninstall path (`post_remove` cannot remove it, but the package
  could print one line or ship a note).
- Whether the cgroup is re-created and reused on the next install before the
  old quota is re-applied. I did not test install → uninstall → install with
  the same group name and a different quota.

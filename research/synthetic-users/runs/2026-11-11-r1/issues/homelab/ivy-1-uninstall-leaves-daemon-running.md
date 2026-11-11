# `pacman -R` leaves both daemons running and still changing process priorities

- severity: blocker
- category: packaging
- environment: Linux 7.2.11-arch1-1 (Arch rolling, cloud image), aranicy 0.3.0-1 + aranicy-collector 0.3.0-1, 2 vCPU x86_64, 1958 MiB RAM

## What I was trying to do

Remove the software from a box. My rule is simple: after `pacman -R`, nothing
the package installed may still be running, and nothing may still be touching
process priorities. I uninstalled the way I always do — no `systemctl stop`
first, because pacman users do not know which units a package owns.

## Steps to reproduce

```
sudo pacman -U --noconfirm ~/payload/aranicy-0.3.0-1-x86_64.pkg.tar.zst \
                         ~/payload/aranicy-collector-0.3.0-1-x86_64.pkg.tar.zst
sudo systemctl enable --now aranicy
# have a rule that is observable, e.g. /etc/ananicy.d/00-test.rules containing
#   {"name": "sleep", "nice": 5}
sudo pacman -R --noconfirm aranicy aranicy-collector
```

## Expected

Both units stopped and the enablement symlink removed before the binaries go
away, so nothing is left running and no new process gets its priority changed
after the transaction ends.

## Actual

pacman removes the files, prints no warning about a running service, and exits
0. Both processes keep running from their now-deleted binaries, systemd still
reports the unit as active with `Loaded: not-found`, the enablement symlink is
left dangling, and a process spawned *after* the removal is still renice-d to
the rule value.

## Evidence

One uninterrupted session, fresh install → enable → remove → observe:

```
$ cat /etc/ananicy.d/00-test.rules
{"name": "sleep", "nice": 5}

$ sudo pacman -U --noconfirm ~/payload/aranicy-0.3.0-1-x86_64.pkg.tar.zst \
                         ~/payload/aranicy-collector-0.3.0-1-x86_64.pkg.tar.zst
$ sudo systemctl enable --now aranicy; sleep 3
$ setsid sleep 502 & sleep 2; p1=$(pgrep -nx sleep)
PRE pid=1913 nice=5 units=active/active/ enabled=enabled

$ sudo pacman -R --noconfirm aranicy aranicy-collector
removing aranicy...
removing aranicy-collector...
REMOVE exit=0

$ echo "LOADSTATE=$(systemctl show -p LoadState --value aranicy) ACTIVESTATE=$(systemctl show -p ActiveState --value aranicy)"
LOADSTATE=not-found ACTIVESTATE=active

$ systemctl status aranicy --no-pager | head -4
● aranicy.service
     Loaded: not-found (Reason: Unit aranicy.service not found.)
     Active: active (running) since Wed 2026-11-11 08:33:43 UTC; 9s ago
 Invocation: 19af530090df41e4b9c740ee006f9c01

$ ls -l /etc/systemd/system/multi-user.target.wants/aranicy.service
lrwxrwxrwx 1 root root 39 Nov 11 08:29 /etc/systemd/system/multi-user.target.wants/aranicy.service -> /usr/lib/systemd/system/aranicy.service

$ pgrep -x aranicy | xargs -r ps -o pid,ni,cmd -p
    PID  NI CMD
   1898  -5 /usr/bin/aranicy start
$ pgrep -x aranicy-collect | xargs -r ps -o pid,ni,cmd -p
    PID  NI CMD
   1897 -10 /usr/bin/aranicy-collectord run

$ ls /usr/bin/aranicy
ls: cannot access '/usr/bin/aranicy': No such file or directory

# a process started AFTER the package was removed:
$ setsid sleep 503 & sleep 2; p2=$(pgrep -nx sleep)
POST pid=2000 nice=5
```

The enablement symlink is a residual of `systemctl enable`: pacman deletes
`/usr/lib/systemd/system/aranicy.service` but knows nothing about
`/etc/systemd/system/multi-user.target.wants/aranicy.service`, so the box keeps
a link to a unit that no longer exists.

Leftover state that also survives the removal:

```
$ sudo ls -la /var/lib/aranicy/
-rw-r--r-- 1 root root 18641 Nov 11 08:26 bias-state
```

`pacman -Qi` says `Install Script : No` for both packages, so nothing stops or
disables the units. If I remove the packages without `systemctl disable` first,
I also have to clean the dangling symlink by hand (I did it manually twice
during this session).

## What I could not determine

- Whether the orphan survives a reboot. The binaries are unlinked, so I expect
  the units to fail at the next boot — but I did not test it, and "expect" is
  not evidence.
- Why the leftover `/var/lib/aranicy/bias-state` matters in practice: I
  reinstalled afterwards and the daemon logged
  `INFO bias: restored history entries=119`, i.e. old learned nice deltas are
  re-applied on the next install. I could not determine whether that is
  intended for uninstall/reinstall cycles on a box that is meant to be clean.

# Removing the `aranicy` package leaves `aranicy-collector.service` running as root with no client to serve

- severity: minor
- category: packaging
- environment: Arch Linux rolling, kernel 7.2.11-arch1-1, aranicy 0.3.0-1 + aranicy-collector 0.3.0-1, 2 vCPU x86_64, 1958 MiB RAM

## What I was trying to do

Take the thing off the machine. I removed the daemon first because I wanted to
see whether removing one half of a two-package product leaves the other half
behind — a real uninstall test, not a clean-machine one.

## Steps to reproduce

```
sudo pacman -R --noconfirm aranicy
systemctl is-active aranicy aranicy-collector
systemctl is-enabled aranicy aranicy-collector
ps -o pid,etime,comm -C aranicy-collect
sudo ls -la /run/aranicy/
```

## Expected

The daemon's own `pre_remove` says: "A daemon that outlives its package keeps
applying priorities from deleted inodes, cannot be replaced by a reinstall …
and leaves an `enabled` symlink pointing at nothing. Stop it first." The
collector is a root daemon that owns BPF programs and two sockets; the same
reasoning applies to it. Removing the daemon should take its required service
down, or at least say that a root process is still running.

## Actual

`pacman -R aranicy` removes the daemon and stops it. The collector keeps
running: still `active`, still holding both sockets in `/run/aranicy`, still
the `aranicy-collect` process. Nothing is printed about it, `systemctl
is-enabled aranicy-collector` says `disabled` (it only ever ran because the
daemon required it), so the leftover is not even something I would find by
listing enabled services. It survives until I remove the collector package
explicitly.

The daemon's `/var/lib/aranicy` is removed by `post_remove`, so the machine ends
up in a half-cleaned state: my learned history is gone but a root BPF collector
is still up.

## Evidence

```
$ ./machine 'sudo pacman -R --noconfirm aranicy 2>&1 | tail -12; sleep 2; echo "--- after ---"; systemctl is-active aranicy aranicy-collector 2>&1; systemctl is-enabled aranicy aranicy-collector 2>&1; ls -la /var/lib/aranicy/ 2>&1; command -v aranicy; ps -o pid,etime,comm -C aranicy-collect'
removing aranicy...
:: Running post-transaction hooks...
(1/2) Reloading system manager configuration...
(2/2) Arming ConditionNeedsUpdate...
--- after ---
inactive
active
not-found
disabled
ls: cannot access '/var/lib/aranicy/': No such file or directory
    PID     ELAPSED COMMAND
   1712       01:15 aranicy-collect
```

(the `EXIT=0` line that followed that block in my transcript is `tail`'s exit
status, not pacman's — my mistake in the command; pacman itself reported no
error and the removal happened.)

…and it only goes away when the collector's own package is removed as well:

```
$ ./machine 'sudo pacman -R --noconfirm aranicy-collector 2>&1 | tail -8; sleep 2; systemctl is-active aranicy-collector 2>&1; ps -o pid,comm -C aranicy-collect'
removing aranicy-collector...
inactive
    PID COMMAND
```

The sockets it was still holding, seen as root just before the second removal:

```
$ ./machine 'sudo ls -la /run/aranicy/'
total 0
drwx------  2 root root  80 Nov 11 10:29 .
srw-------  1 root root   0 Nov 11 10:29 collector-events.sock
srwxr-xr-x  1 root root   0 Nov 11 10:29 collector-stats.sock
```

## What I could not determine

- What the collector *does* with no client. It was idle (0.1 % of a core in
  `ps`), the BPF programs stay attached and the sockets stay bound. Whether that
  is harmless or just untidy I cannot say from outside.
- Whether `systemctl --quiet disable --now aranicy.service` in `pre_remove` is
  supposed to propagate to the required unit. It does not on this box; the
  collector is started by `Requires=` and stopping the requiring unit does not
  stop the required one.

`workflow this blocks`: "uninstall it and have my machine back". One `pacman -R`
leaves a root process behind and nothing says so.

`related`: kaz-12 (the install side of the same lifecycle problem), kaz-7
(removal also destroys the record of what was applied).

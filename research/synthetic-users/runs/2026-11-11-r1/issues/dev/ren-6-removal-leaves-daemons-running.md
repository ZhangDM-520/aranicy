# `pacman -Rns` leaves both daemons running — and still applying nice — until reboot

- severity: major
- category: packaging
- environment: Arch Linux cloud image, kernel 7.2.11-arch1-1, aranicy 0.3.0-1 +
  aranicy-collector 0.3.0-1 (local/pacman), 2 vCPU / 1958 MiB / no display

## What I was trying to do

Remove the package and be sure nothing on the machine still applies priority
policy. This is the rollback path: if the tool misbehaves on a colleague's
machine, the first move is "take it off", and that move has to actually take it
off.

## Steps to reproduce

```sh
sudo pacman -Rns --noconfirm aranicy aranicy-collector
pgrep -af "/usr/bin/aranicy"
systemctl is-active aranicy aranicy-collector

# prove it is still doing its job: a name with a rule, after the package is gone
bash -c 'exec -a argvdemo /bin/sleep 10' & pid=$!
sleep 1; awk '{print "nice="$19}' /proc/$pid/stat
```

## Expected

Removal stops the services (a `pre_remove` hook or the package's own uninstall
path), so that after `pacman -Rns` there is no process left that nices anything.
Leftover *state* (the bias snapshot, the config, the enable symlink) is a
housekeeping matter; leftover *running daemons* are not.

## Actual

Nothing is stopped. `pacman` reports a clean removal, both binaries are deleted
from `/usr/bin`, and both daemons keep running from the deleted inodes as root:
`aranicy-collectord` and `aranicy start`. The daemon still applies policy — a
process matching a surviving rule gets `nice=14` **after** the package is gone.

Reinstalling does not repair it either: `systemctl start aranicy` is a no-op
because the unit is still "active" in systemd's view, so the machine keeps
running the old (now deleted, then recreated) binary until someone restarts the
unit or reboots.

## Evidence

```
$ sudo pacman -Rns --noconfirm aranicy aranicy-collector
checking dependencies...

Packages (2) aranicy-0.3.0-1  aranicy-collector-0.3.0-1

Total Removed Size:  6.19 MiB

:: Do you want to remove these packages? [Y/n] 
:: Processing package changes...
removing aranicy...
removing aranicy-collector...
:: Running post-transaction hooks...
(1/2) Reloading system manager configuration...
(2/2) Arming ConditionNeedsUpdate...
EXIT=0

$ pgrep -af "/usr/bin/aranicy"
1797 /usr/bin/aranicy-collectord run
115963 /usr/bin/aranicy start

$ systemctl is-active aranicy-collector
active

$ bash -c 'exec -a argvdemo /bin/sleep 10' & pid=$!; sleep 1
  argv: argvdemo 10 
  nice: 14

$ ls -l /etc/systemd/system/multi-user.target.wants/ | grep -i aranicy
lrwxrwxrwx 1 root root 39 Nov 11 08:15 aranicy.service -> /usr/lib/systemd/system/aranicy.service

$ sudo ls -la /var/lib/aranicy
-rw-r--r-- 1 root root 8990 Nov 11 08:41 bias-state
$ sudo ls -la /run/aranicy
srw-------  1 root root   0 Nov 11 08:19 collector-events.sock
srwxr-xr-x  1 root root   0 Nov 11 08:19 collector-stats.sock
```

And after reinstalling the same two packages:

```
$ sudo pacman -U --noconfirm aranicy-collector-0.3.0-1-x86_64.pkg.tar.zst aranicy-0.3.0-1-x86_64.pkg.tar.zst
...
$ sudo systemctl start aranicy; sleep 3; systemctl is-active aranicy aranicy-collector; pgrep -af "/usr/bin/aranicy"
active
active
1797 /usr/bin/aranicy-collectord run
115963 /usr/bin/aranicy start
```

Same pids: the reinstall left the deleted-binary processes in place.

## What I could not determine

- Whether the intended removal path is the README's `contrib/install.sh
  --uninstall`, which is not shipped in either package (see ren-4). If that script
  is what stops the units, then a plain `pacman -R` is the hole.
- Whether Arch packaging policy expects the user to stop services by hand. In my
  experience a daemon that keeps nicing processes after `pacman -Rns` is a
  surprise, and the package ships no `.INSTALL` script or note telling me to run
  `systemctl disable --now` first.
- Whether the two sockets in `/run/aranicy` and the 8.9 kB
  `/var/lib/aranicy/bias-state` are intentional leftovers. They are inert after a
  reboot, so I am not counting them as the defect; the running daemons are.

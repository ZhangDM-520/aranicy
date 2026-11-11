# `pacman -U` while the daemon runs leaves the old binary executing (`/usr/bin/aranicy (deleted)`); the upgrade needs a manual restart that nothing asks for

- severity: minor
- category: packaging
- environment: Linux 7.2.11-arch1-1 (Arch rolling, cloud image), aranicy 0.3.0-1 + aranicy-collector 0.3.0-1, 2 vCPU x86_64, 1958 MiB RAM

## What I was trying to do

Apply a rebuilt package pair the way Ansible does — `pacman -U` over the
installed version, no service surgery — and then verify that the running
daemon is the binary I just installed. Months later I check `pacman -Q` and
`ps` and expect them to agree.

## Steps to reproduce

```
sudo pacman -U --noconfirm ~/payload/aranicy-collector-0.3.0-1-x86_64.pkg.tar.zst \
                         ~/payload/aranicy-0.3.0-1-x86_64.pkg.tar.zst
sudo systemctl enable --now aranicy
sleep 5
P=$(systemctl show -p MainPID --value aranicy); echo "pid=$P"
sudo pacman -U --noconfirm ~/payload/aranicy-collector-0.3.0-1-x86_64.pkg.tar.zst \
                         ~/payload/aranicy-0.3.0-1-x86_64.pkg.tar.zst
P2=$(systemctl show -p MainPID --value aranicy); echo "pid before=$P after=$P2"
sudo readlink /proc/"$P2"/exe
systemctl show -p NRestarts aranicy
```

## Expected

Either the package restarts the service (the usual Arch `post_upgrade` +
`try-restart`), or it refuses/warns prominently that a restart is required, or
at minimum the stale process is visible in `systemctl status` (it is not).

## Actual

The PID does not change, the process's executable is the deleted inode, and
systemd records no restart:

```
$ P=$(systemctl show -p MainPID --value aranicy); echo "pid=$P"
pid=32971
$ sudo pacman -U --noconfirm ~/payload/aranicy-collector-0.3.0-1-x86_64.pkg.tar.zst \
                         ~/payload/aranicy-0.3.0-1-x86_64.pkg.tar.zst
reinstalling aranicy-collector...
reinstalling aranicy...
...
$ P2=$(systemctl show -p MainPID --value aranicy); echo "pid before=$P after=$P2"
pid before=32971 after=32971
$ sudo readlink /proc/32971/exe
/usr/bin/aranicy (deleted)
$ systemctl show -p NRestarts aranicy
NRestarts=0
```

The same happens on the previous build (`pid before=459 after=459`,
`/usr/bin/aranicy (deleted)`), so it is not a regression — it is just not
handled.

I only noticed because I was comparing builds. On the box itself the only
signal is that `readlink /proc/<pid>/exe` shows the deleted inode; `systemctl
status` shows a healthy unit and `pacman -Q` shows the new version.

## Evidence

`raw/adv5.txt`, section F0:

```
$ pid + exe before
pid=32971
/usr/bin/aranicy
-rwxr-xr-x 1 root root 3861288 Nov 11 09:52 /usr/bin/aranicy

$ sudo pacman -U same packages
reinstalling aranicy...
...
$ pid + exe after reinstall
pid before=32971 after=32971
/usr/bin/aranicy (deleted)
[readlink exit=0]
-rwxr-xr-x 1 root root 3861288 Nov 11 09:52 /usr/bin/aranicy

$ systemd noticed nothing
ActiveEnterTimestamp=Wed 2026-11-11 10:20:26 UTC
NRestarts=0
```

and the previous build, `raw/prevfinal.txt`:

```
$ exe after reinstall
pid before=459 after=459
/usr/bin/aranicy (deleted)
NRestarts=0
```

The shipped `.INSTALL` does not claim otherwise in code, but its comment does:

```
# Keep going on upgrade/reinstall: the files below are replaced, the unit is
# handled by the daemon's own restart logic on the next `systemctl start`.
post_upgrade() {
    systemctl --quiet daemon-reload 2>/dev/null || true
}
```

There is no restart logic in the package; `daemon-reload` only re-reads unit
files.

## What I could not determine

- Whether a `post_upgrade` restart is deliberately omitted (a restart drops
  the daemon briefly and re-stamps learned wants; I would accept "no restart,
  but log a warning"). I could not find any shipped note that says an upgrade
  needs a manual restart.
- Whether a `try-restart` would be safe with the collector — I did not test
  restarting both units in `post_upgrade` because the package does not do it.

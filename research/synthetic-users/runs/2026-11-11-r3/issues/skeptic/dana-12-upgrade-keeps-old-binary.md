# Installing a newer build does not restart the daemon, so the old binary keeps running from a deleted inode — and both builds carry the same version string, so nothing tells you

- severity: major
- category: packaging
- environment: Linux 7.2.11-arch1-1 (Arch rolling), aranicy 0.3.0-1 + aranicy-collector 0.3.0-1, 2 vCPU x86_64, 1958 MiB

## What I was trying to do

Swap from the previous round's build to the current one with `pacman -U` and
check that the new code is actually the code that is running.

## Steps to reproduce

```
# previous build installed and running
sudo pacman -U --noconfirm ~/payload/aranicy-collector-0.3.0-1-x86_64.pkg.tar.zst \
                             ~/payload/aranicy-0.3.0-1-x86_64.pkg.tar.zst
systemctl is-active aranicy                  # active
pgrep -a aranicy                             # the OLD pid, e.g. 1509
sudo readlink /proc/$(systemctl show -p MainPID --value aranicy)/exe
sudo sha256sum /proc/$(systemctl show -p MainPID --value aranicy)/exe /usr/bin/aranicy
pacman -Q aranicy
```

## Expected

After a package that ships a daemon is (re)installed, either the service is
restarted so the installed file is the running file, or something reports that
a restart is required. Arch's own convention for this is a pacman hook or a
`post_upgrade`/`post_install` that does `systemctl try-restart`.

## Actual

Both units stay `active` and the main process is the *previous* build's
process, running from `/usr/bin/aranicy (deleted)`; the installed file is a
different binary. Nothing restarts it — not the install hook, not systemd's
"Enqueuing marked services" step. The daemon only picks up the new binary if
you restart it yourself or reboot.

The two packages cannot even be told apart by the package manager: both are
`pkgver = 0.3.0-1`, differing only in `builddate`, and `pacman -Qkk` reports
`0 altered files` because the *installed* file is the new one.

The behaviour is deliberate according to the shipped install script, which is
the part I would ask the maintainer to revisit:

```
$ bsdtar -xOf ~/previous-build/aranicy-0.3.0-1-x86_64.pkg.tar.zst .INSTALL
# Keep going on upgrade/reinstall: the files below are replaced, the unit is
# handled by the daemon's own restart logic on the next `systemctl start`.
post_upgrade() {
    systemctl --quiet daemon-reload 2>/dev/null || true
}
```

"On the next `systemctl start`" never happens on a machine that is not
rebooting: `systemctl is-active` says `active` and the operator has no reason
to start anything.

## Evidence

```
$ sudo pacman -U --noconfirm .../aranicy-collector-0.3.0-1-x86_64.pkg.tar.zst .../aranicy-0.3.0-1-x86_64.pkg.tar.zst
:: Processing package changes...
reinstalling aranicy-collector...
reinstalling aranicy...
:: Running post-transaction hooks...
(1/3) Reloading system manager configuration...
(2/3) Enqueuing marked services...
(3/3) Arming ConditionNeedsUpdate...
$ systemctl is-active aranicy aranicy-collector
active
active
$ pgrep -a aranicy
1479 /usr/bin/aranicy-collectord run
1509 /usr/bin/aranicy start
$ ps -o lstart= -p 1509
Wed Nov 11 10:16:04 2026
$ sudo readlink /proc/1509/exe
/usr/bin/aranicy (deleted)
$ sudo sha256sum /proc/1509/exe | cut -c1-16
6c0bd1a9595b82cc            # = previous build
$ sha256sum /usr/bin/aranicy | cut -c1-16
16c10445927d9d40            # = new build
$ pacman -Q aranicy
aranicy 0.3.0-1
$ sudo pacman -Qkk aranicy | tail -2
aranicy: 21 total files, 0 altered files
$ sudo systemctl restart aranicy; sudo readlink /proc/$(systemctl show -p MainPID --value aranicy)/exe
/usr/bin/aranicy
$ sudo sha256sum /proc/1814/exe | cut -c1-16
16c10445927d9d40
$ for p in ~/payload ~/previous-build; do bsdtar -xOf $p/aranicy-0.3.0-1-x86_64.pkg.tar.zst .PKGINFO | grep -E '^pkgver|^builddate' | tr '\n' ' '; echo; done
pkgver = 0.3.0-1 builddate = 1794390742
pkgver = 0.3.0-1 builddate = 1794387575
```

The same `.INSTALL` is byte-identical in both packages (only `builddate`
differs), so the behaviour is not new in this build. I did not separately hash
the running collector's binary; what I can show is that its pid (1479) also
predates the install (the daemon's pid 1509 was started at 10:16:04, before the
10:19 upgrade).

## What I could not determine

- Whether a systemd hook exists elsewhere in the package that I failed to
  trigger. The only script in the package is the `.INSTALL` quoted above and
  the package defines no pacman hooks.
- Whether the maintainers intend "upgrade = reinstall, user restarts" as
  policy. The comment says it is deliberate; whether it is *acceptable* is a
  judgement call, which is why I am filing it as major rather than as a defect
  in behaviour: on a production box the effect is that an upgrade is a no-op
  until the next reboot.

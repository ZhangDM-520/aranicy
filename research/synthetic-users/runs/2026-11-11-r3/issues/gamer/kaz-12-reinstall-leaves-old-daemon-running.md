# Installing a package over a running aranicy leaves the old daemon and collector running from deleted files; the CLI reports the new version while the machine runs the old code

- severity: major
- category: packaging
- environment: Arch Linux rolling, kernel 7.2.11-arch1-1, aranicy 0.3.0-1 + aranicy-collector 0.3.0-1, 2 vCPU x86_64, 1958 MiB RAM

## What I was trying to do

Move the machine from one build to another with `pacman -U`, the way the README
tells me to install it, while the daemon is working. I had two package pairs for
the same version number (`0.3.0-1`) built at different times, so "install the
other one" is exactly the upgrade I care about.

## Steps to reproduce

```
sudo systemctl start aranicy
D=$(systemctl show -p MainPID --value aranicy); echo $D
cd ~/previous-build
sudo pacman -U --noconfirm aranicy-collector-0.3.0-1-x86_64.pkg.tar.zst \
                            aranicy-0.3.0-1-x86_64.pkg.tar.zst
sudo ls -l /proc/$D/exe
systemctl is-active aranicy
aranicy --version
sudo systemctl show -p MarkedForRestart -p MainPID aranicy.service
```

## Expected

Either pacman/the unit stops and restarts the daemon while replacing the binary,
or something tells me "your daemon is still the old one, restart it". The
package's own `pre_remove` explains exactly this hazard in a comment ("a daemon
that outlives its package keeps applying priorities from deleted inodes, cannot
be replaced by a reinstall"), and the box has a systemd pacman hook whose whole
job is marking units for restart, so both mechanisms exist on this machine.

## Actual

Nothing is stopped, restarted or marked. `pacman -U` prints
`reinstalling aranicy…`, the post-upgrade hook runs `daemon-reload`, and the
process keeps running the removed inode:

- `MainPID` is unchanged across the install;
- `/proc/<pid>/exe` becomes `/usr/bin/aranicy (deleted)`;
- `systemctl is-active aranicy` says `active`;
- `aranicy --version` (the new file on disk) prints the *new* version, so the
  CLI and the daemon disagree and only the CLI tells me what I think I installed;
- `systemctl show -p MarkedForRestart` is empty, so nothing will restart it later;
- the journal contains no `Starting`/`Stopping` line for the unit around the
  install.

The same happens to `aranicy-collectord`. On this box the observable difference
between the two builds is documentation only (see `differential.md`), so a user
who installs a fixed build to get a fixed daemon gets neither until they
remember to restart by hand.

## Evidence

Install of the other build over the running one (2026-11-11 10:10):

```
$ ./machine 'DP=$(systemctl show -p MainPID --value aranicy); echo "daemon pid=$DP"; sudo ls -l /proc/$DP/exe; systemctl is-active aranicy; cd ~/previous-build && sudo pacman -U --noconfirm aranicy-collector-0.3.0-1-x86_64.pkg.tar.zst aranicy-0.3.0-1-x86_64.pkg.tar.zst 2>&1 | tail -14; sleep 2; DP2=$(systemctl show -p MainPID --value aranicy); echo "daemon pid=$DP2"; sudo ls -l /proc/$DP2/exe; systemctl is-active aranicy aranicy-collector; echo "CLI version (new binary): $(aranicy --version)"'
daemon pid=876
lrwxrwxrwx 1 root root 0 Nov 11 10:10 /proc/876/exe -> /usr/bin/aranicy
active
…
reinstalling aranicy-collector...
reinstalling aranicy...
:: Running post-transaction hooks...
(1/3) Reloading system manager configuration...
(2/3) Enqueuing marked services...
(3/3) Arming ConditionNeedsUpdate...
daemon pid=876
lrwxrwxrwx 1 root root 0 Nov 11 10:10 /proc/876/exe -> /usr/bin/aranicy (deleted)
active
active
CLI version (new binary): aranicy 0.3.0
```

The collector, same install:

```
$ ./machine 'CP=$(systemctl show -p MainPID --value aranicy-collector); sudo ls -l /proc/$CP/exe'
lrwxrwxrwx 1 root root 0 Nov 11 10:11 /proc/875/exe -> /usr/bin/aranicy-collectord (deleted)
```

And the reverse install (back to the current build) behaves identically:

```
$ ./machine 'DP=$(systemctl show -p MainPID --value aranicy); cd ~/payload && sudo pacman -U --noconfirm aranicy-collector-0.3.0-1-x86_64.pkg.tar.zst aranicy-0.3.0-1-x86_64.pkg.tar.zst 2>&1 | grep -E "installing|reinstalling"; sleep 1; echo "stale check: $(sudo ls -l /proc/$(systemctl show -p MainPID --value aranicy)/exe | sed "s/.*-> //")"'
warning: aranicy-collector-0.3.0-1 is up to date -- reinstalling
warning: aranicy-0.3.0-1 is up to date -- reinstalling
reinstalling aranicy-collector...
reinstalling aranicy...
stale check: /usr/bin/aranicy (deleted)
```

The marking check, a third install, this time asking systemd directly:

```
$ ./machine 'DP=$(systemctl show -p MainPID --value aranicy); echo "daemon before: $DP"; sudo systemctl show -p MarkedForRestart aranicy.service aranicy-collector.service; cd ~/payload && sudo pacman -U --noconfirm aranicy-collector-0.3.0-1-x86_64.pkg.tar.zst aranicy-0.3.0-1-x86_64.pkg.tar.zst 2>&1 | grep -E "reinstalling|Enqueuing"; echo "--- immediately after ---"; sudo systemctl show -p MarkedForRestart -p ActiveState -p MainPID aranicy.service; sudo ls -l /proc/$DP/exe | sed "s/.*-> /exe -> /"; sudo journalctl -u aranicy --no-pager --since "-30s" | grep -E "Starting|Started|Stopping" | tail -3'
daemon before: 748
warning: aranicy-collector-0.3.0-1 is up to date -- reinstalling
warning: aranicy-0.3.0-1 is up to date -- reinstalling
reinstalling aranicy-collector...
reinstalling aranicy...
(2/3) Enqueuing marked services...
--- immediately after ---
ActiveState=active
MainPID=748
exe -> /usr/bin/aranicy (deleted)
```

(`MarkedForRestart` is absent from that output; the journal section is empty —
neither the unit nor the process was touched.)

## What I could not determine

- Whether a *version-number* upgrade behaves differently from this reinstall.
  Both packages here are `0.3.0-1`, so pacman took the "reinstalling" path; the
  unit's `post_upgrade` is the same script either way, but I had no `0.3.1` to
  try.
- Whether the daemon restarts itself when it notices its own binary changed. I
  watched it for a couple of minutes and it did not; longer-term it may, but I
  would not know how to make it do so on demand.
- Whether the maintainers intend `systemctl restart aranicy` to be part of the
  install ritual. The `Install` section of the README does not say so, and the
  unit is `Restart=always` (restarts on failure, not on file replacement).

`workflow this blocks`: "install the fixed build on the machine that has the
bug". Every install leaves the previous code running, and the only way to tell
is `ls -l /proc/<pid>/exe`.

`related`: kaz-7 (a reinstall/removal is one of the ways the applied values and
the daemon's records get out of step).

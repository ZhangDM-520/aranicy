# The shipped README points at files that are not shipped, and its statement that "no per-task stats -> no learning" is contradicted by this box, where bias was learned with sched_ext disabled

- severity: minor
- category: docs
- environment: Linux 7.2.11-arch1-1, aranicy 0.3.0-1 + aranicy-collector 0.3.0-1, 2 vCPU, 1958 MiB

## What I was trying to do

Follow the shipped documentation literally: install the package, then read the documents it
tells me to read and run the installer it tells me to run. Then check the parity claim
that the bias supplement is inert without `sched_ext` per-task statistics.

## Steps to reproduce

```
cd ~/payload && bsdtar -tf aranicy-0.3.0-1-x86_64.pkg.tar.zst
bsdtar -xOf aranicy-0.3.0-1-x86_64.pkg.tar.zst usr/share/doc/aranicy/README.md
sudo pacman -U --noconfirm ~/payload/*.pkg.tar.zst
ls /usr/share/doc/aranicy/ /usr/share/doc/aranicy-collector/
cat /sys/kernel/sched_ext/state          # -> disabled
sudo systemctl enable --now aranicy
sleep 120 && sudo journalctl -u aranicy --no-pager | grep 'bias changed'
```

## Expected

The four things the README instructs me to use exist after installing the package, and a
box with `sched_ext` disabled behaves as described: "No per-task stats -> no learning".

## Actual

The package ships exactly nine files; `docs/BENCHMARKS.md`, `docs/COMPATIBILITY.md`,
`tools/storm/` and `contrib/install.sh` are referenced by the README (and are the stated
evidence for the performance and compatibility claims) but exist nowhere on the installed
system. `./contrib/install.sh`, the documented install path, does not exist.

The parity claim is also wrong in the direction that matters: with
`/sys/kernel/sched_ext/state` = `disabled` and no `scx_*` process running, the daemon still
classified processes and stepped bias (`bias changed ... class=Bulk cause=Evidence`). So
"the sampler schema-binds whatever per-task `scx_stats` surface the running scheduler
exposes" is not the whole input; something else is feeding the engine on a plain box.

## Evidence

```
$ bsdtar -tf aranicy-0.3.0-1-x86_64.pkg.tar.zst
etc/ananicy.d/ananicy.conf
usr/bin/aranicy
usr/lib/systemd/system/aranicy.service
usr/share/doc/aranicy/README.md
usr/share/licenses/aranicy/LICENSE     (+ .PKGINFO/.MTREE/.BUILDINFO)

$ cat /sys/kernel/sched_ext/state
disabled
$ systemctl list-units "scx*" --no-pager --no-legend
(no output)
$ sudo journalctl -u aranicy --no-pager | grep 'bias changed' | tail -3
... INFO bias changed name=bash bias=3 prev=2 class=Bulk cause=Evidence
... INFO bias changed name=python3 bias=3 prev=2 class=Bulk cause=Evidence
... INFO bias changed name=aranicy bias=3 prev=2 class=Bulk cause=Evidence
```

Related, same category: `aranicy.service` grants `CAP_SYS_NICE CAP_SYS_RESOURCE
CAP_DAC_OVERRIDE CAP_SYS_PTRACE` while the README says "aranicy itself then needs only
`CAP_SYS_NICE` (no `CAP_BPF`)" — the extra three are needed by the `/proc/<pid>/exe`
fallback and are described in a unit comment, not in the README.

## What I could not determine

- Whether "No per-task stats -> no learning" is meant to describe only the sched_ext-
  statistics *source* and the collector is a documented second source somewhere I do not
  have; docs/COMPATIBILITY.md is not shipped, so I cannot check.
- Whether the missing files are a packaging choice (upstream keeps docs out of the
  package) or an error; either way the shipped README's own links are dead on the
  installed system, which is what a user has.

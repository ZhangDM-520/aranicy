# Differential — current build vs `previous-build`

Method: one build installed at a time, both from the package pair in
`~/payload` and `~/previous-build`. The machine was reset between runs by
`pacman -R` (verified clean, see `adversarial.md` §7) and the **same** rules
corpus (`/etc/ananicy.d/{naive.rules,cg.cgroups,cgrule.rules}`), the same
helper (`/home/arch/j/try.sh`) and the same script
(`/home/arch/j/battery.sh`, 15 checks, ~4 minutes each) were used for both.
Raw transcripts saved verbatim in `evidence/battery-current.txt` and
`evidence/battery-previous.txt`.
Both packages are versioned `0.3.0-1`; they differ only in `builddate`.

## What actually differs in the packages

```
$ sha256sum ~/work/c/usr/bin/aranicy ~/work/p/usr/bin/aranicy \
            ~/work/c.coll/usr/bin/aranicy-collectord ~/work/p.coll/usr/bin/aranicy-collectord
16c10445927d9d40bc7ed2384421010c39e3ec015fb66870913636c833ca4bc4  /home/arch/work/c/usr/bin/aranicy
6c0bd1a9595b82ccaefed6ceec97b4cedfd636c141f35336e62ae8494df79596  /home/arch/work/p/usr/bin/aranicy
8a17dff46604bdd2e42a744170852458269c176cfe144a3d69e6113d5e7fc11f  /home/arch/work/c.coll/usr/bin/aranicy-collectord
530975abf119559c64fe32fd539811616ecc646845defef60259036d4f25d3d8  /home/arch/work/p.coll/usr/bin/aranicy-collectord

$ cmp -s ~/work/c/etc/ananicy.d/ananicy.conf ~/work/p/etc/ananicy.d/ananicy.conf   → conf same
$ cmp -s ~/work/c/usr/lib/systemd/system/aranicy.service ~/work/p/...              → unit same
$ cmp -s ~/work/c.coll/.../aranicy-collector.service ~/work/p.coll/...             → coll unit same
```

So: both binaries were rebuilt (different content, same version string), and the
config, both unit files, `BENCHMARKS.md` and the `.INSTALL` scripts are
byte-identical. Three documents changed:

1. **File layout.** `BENCHMARKS.md`/`COMPATIBILITY.md` moved from
   `/usr/share/doc/aranicy/` to `/usr/share/doc/aranicy/docs/`
   (package listings: previous is `BENCHMARKS.md COMPATIBILITY.md
   README.md`, current is `README.md docs/`). `README.md` was not edited to match — its
   links already said `docs/...` and were broken in the previous build.
2. **`README.md` opening paragraph** — the performance budget was rewritten
   from "≤ 5% of one core at 2 000 execs/s" to "12.3% of one core at 2 000
   execs/s (6.1 + 6.2)" for the daemon *and* collector, with the 2.28 % figure
   labelled as the retired single-process architecture.
3. **`COMPATIBILITY.md`** — the `CPUQuota` period was corrected from "period
   1 s" to "the kernel's 100 ms, i.e. quota = N/100 × ncpu × 100000", and a new
   "Trust model" section states that any user can copy a binary to a rule's
   name and receive its grant.

## Per-check results

| check | previous | current | verdict |
|---|---|---|---|
| B1 `pacman -Q` | aranicy 0.3.0-1, aranicy-collector 0.3.0-1 | same | same (and see note) |
| B2 installed file list | 6 files, docs at top level | 6 files, docs under `docs/` | **better** |
| B3 README link targets | `docs/BENCHMARKS.md` → MISSING, `docs/COMPATIBILITY.md` → MISSING | both resolve to the shipped files | **better** (round 2's dana-9 fixed) |
| B4 duplicate doc copies | n/a (no `docs/`) | `docs/` present, no duplicate copies | **better** |
| B5 units after `enable --now` | active+active, enabled+disabled | same | same |
| B6 rule applied at exec (`nice: 12`) | `skepticbat pid=1422 nice=12` | `skepticbat pid=682 nice=12` | same |
| B7 `lint` on an empty corpus | 1 warning, rc=1 — does *not* warn that 0 rules loaded | 2 warnings, rc=1 — adds `0 rules loaded from ...: this corpus will apply nothing` | **better** |
| B8 `lint` on the junk corpus | 8 warnings, all junk named | 8 warnings, all junk named | same |
| B9 `start` as uid 1000 | refuses with the sudo hint, rc=1 | same text | same |
| B10 `dump rules/cgroups/proc/bias` | all four parse; `proc rows=129` | all four parse; `proc rows=131` | same |
| B11 `CPUQuota 50` → `cpu.max` | `100000 100000`; process in `0::/skepticcg` | `100000 100000`; process in `0::/skepticcg` | same |
| B12 collector `SIGKILL` | outage nice=0, recovery nice=12, both units active | identical | same |
| B13 daemon `SIGKILL` | `active`, new pid, pidfile rewritten, rules apply | identical | same |
| B14 bias learns a CPU-bound task | `class=bulk`, bias 0→2 at 30 s steps, `/proc nice=2` | identical | same |
| B15 daemon applies the corpus to itself | `daemon nice=19 (unit Nice=-5)` | identical | same |

Things that look like differences and are not:

- **B10 `bias entries`**: 0 in the previous run and 93 in the current one. This
  is elapsed time and persisted state, not behaviour — the previous run started
  from a freshly created `/var/lib/aranicy` (the uninstall removed it), the
  current one from a daemon that had been running for an hour. The shapes are
  the same (`entries`, `threshold_ema`, `version`).
- **B8 `rc`**: both printed `rc=0` because my line pipes `lint` into `tail` and
  reports `tail`'s status. The rc that matters is B7's (1 in both). My mistake,
  recorded in the log.
- **B4 in the raw transcript** says `DIFFER` for both builds: `cmp` returns 2
  when a file is missing, and my one-liner maps "not 0" to `DIFFER`. The real
  finding for the previous build is that `docs/` does not exist at all.

## Verdict per axis

- **Regressions: none found.** Every functional check — exec-time apply,
  cgroup move, collector-loss recovery, daemon restart, bias learning, non-root
  behaviour, junk/empty/half-written config, PID 1 and kernel-thread immunity —
  produced the same observable result on both builds.
- **Improvements: three, all documentation/packaging.** The README links now
  resolve (dana-9 closed), the README's headline budget now describes the
  package the user installs (dana-10 closed), and `lint` now warns when the
  corpus is empty. The `COMPATIBILITY.md` `CPUQuota` formula also now matches
  what the binary writes, which I checked against the kernel rather than
  against the text: the shipped value is `100000 100000`, i.e. a 100 ms period,
  so the new wording is right and the old wording was wrong.
- **Carried over, unfixed:** all four defects I filed this round (`dana-11`
  cgroup quota after reboot, `dana-12` upgrade leaves the old binary,
  `dana-13` daemon re-nices itself, `dana-15` `--reload` trusts the pid file)
  reproduce identically on the previous build — `dana-13` is in the battery and
  gives `nice=19` on both. They are not regressions; they are not fixed either.
- **New inconsistency introduced:** the README's Status section still quotes
  the retired 2.28 % figure as the product's result (`dana-14`). The new header
  paragraph and the Status paragraph disagree by more than 5× in the same file.

## A note on the version string

Both builds are `pkgver = 0.3.0-1`; `builddate` differs (1794387575 vs
1794390742). `pacman -Q` therefore cannot distinguish them, `pacman -Qkk`
reports `0 altered files` after an install, and — because the install does not
restart the unit — the *running* process can be the other build entirely
(`/usr/bin/aranicy (deleted)`, sha `6c0bd1a9…` = previous, while the installed
file is `16c10445…` = current). For a differential this is the single most
important defect: on this machine the only reliable way to know which build is
answering is to hash `/proc/<pid>/exe`.

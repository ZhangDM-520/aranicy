# Differential — current build vs previous build, ren round 3

- `current` = `~/payload`: `aranicy 0.3.0-1`, `.PKGINFO builddate = 1794390742`
  (Wed Nov 11 09:52:22 2026), `aranicy` sha256
  `16c10445927d9d40bc7ed2384421010c39e3ec015fb66870913636c833ca4bc4`.
- `previous` = `~/previous-build`: same version string, `.PKGINFO builddate =
  1794387575` (08:59:35 2026), `aranicy` sha256
  `6c0bd1a9595b82ccaefed6ceec97b4cedfd636c141f35336e62ae8494df79596`.
- Same machine, same kernel (7.2.11-arch1-1), same 2 vCPU / 1958 MiB, no
  `sched_ext` scheduler either time. Each build was installed fresh with
  `pacman -U`, enabled with `systemctl enable --now aranicy`, and put through
  the identical script (`~/bset.sh`, 12 steps) plus a 225 s learned-bias run and
  a reboot.

The two packages are not the same tree: `diff -r` of the extracted payloads
shows different binaries, the docs moved from `/usr/share/doc/aranicy/*.md`
into `/usr/share/doc/aranicy/docs/`, `COMPATIBILITY.md` gained a trust-model
section and a corrected cgroup sentence, and `aranicy`'s `.INSTALL` gained a
`post_remove` hook that deletes `/var/lib/aranicy/bias-state`.

## Verdict table

| # | check | current | previous | verdict |
|---|---|---|---|---|
| 1 | `--version`, subcommand surface | `aranicy 0.3.0`, start/dump/lint/match-test | same | same |
| 2 | lint: 7 malformed corpora (truncated JSON, unknown key, range, regex, mixed, junk config, unknown type) | file:line warning each, exit 1 | identical text, exit 1 | same |
| 3 | lint: corpus with 0 rules (empty `a.rules`, or `.types` only) | `lint: warnings found`, exit **1** | `OK: 0 rules, 0 types, 0 cgroups`, exit **0** | **worse** (ren-14) |
| 4 | lint: valid rules + `ananicy.conf` in the dir | `OK: 2 rules, 0 types, 0 cgroups`, exit 0 | same | same |
| 5 | lint: valid rules, no `ananicy.conf` in the dir | `config … not found; using defaults` + `lint: warnings found`, exit 1 | identical | same (ren-15) |
| 6 | daemon start with half-written / empty config | `config: line 4: invalid boolean "tru"`, rules still applied | identical line, rules applied | same |
| 7 | `systemctl enable --now` → both units, sockets in `/run/aranicy` | active/active, 2 sockets | same | same |
| 8 | `dump rules` / `dump proc` / `dump bias` JSON shape | exact/regex arrays; proc name+nice+pid; bias state file | same | same |
| 9 | rule apply: exact name `nice=7`, `ioclass=idle` | `/proc` nice 7, `ionice` idle | same | same |
| 10 | regex rule + `match-test` (hit / regex hit / miss) | 3 JSON/`no rule matches`, exit 0/0/1 | same | same |
| 11 | reload with a truncated rule line | bad line named, good rule kept, `reloaded 1 rules` | same | same |
| 12 | reload with half-written config | `invalid boolean`, other keys applied | same | same |
| 13 | `CPUQuota=50` on 2 CPUs → `cpu.max` | `100000 100000` | `100000 100000` | same (docs corrected, see #14) |
| 14 | cgroup at **cold boot** | `WARN cgroup "rencg" not created…`, `cpu.max = max 100000`, process moved in | identical | same (ren-13) |
| 15 | non-root `aranicy start` (uid 1000 and 65534) | clear error, exit 1 | same | same |
| 16 | `exec -a` / `prctl(PR_SET_NAME)` impersonation | not matched (nice 0) | same | same |
| 17 | `systemctl stop aranicy-collector` | daemon stops too (`inactive`/`inactive`) | same | same (ren-11 still open) |
| 18 | `pacman -R` leftovers | `/var/lib/aranicy` gone, `/run` gone, no symlink | `/var/lib/aranicy/bias-state` left (12 745 B) | **better** (fixed) |
| 19 | `bias_protect` name (`sshd9`) under load | learned 0→3, never applied (nice 0) | identical | same (ren-9 still open) |
| 20 | daemon's own nice under sustained load (`nice=-5` unit) | stays −5 for 225 s | stays −5 for 225 s | same (ren-10 not reproduced in either) |
| 21 | packaged doc locations vs README | README says `/usr/share/doc/aranicy/COMPATIBILITY.md`, file is in `docs/` | README's paths correct | **worse** (ren-16) |
| 22 | README performance claims | intro says 12.3 % of a core vs ananicy-cpp 13.7 %, Status still says "6.0× cheaper … 2.28 %" — the two cannot both be current | one budget ("≤ 5 % of one core") and one result ("2.28 % vs 13.7 %"), which are consistent | **worse** (ren-17) |
| 23 | reboot mid-work | units back, 22 history entries restored, rules applied | 58 entries restored, rules applied | same |
| 24 | second `aranicy-collectord run` while active | starts silently, steals socket path, pair broken afterwards | not tested separately (same binary behaviour expected, not claimed) | n/a (ren-12, found on current) |

Three regressions (#3, #21, #22) and one fix (#18). Everything else is
unchanged, including both defects I filed last round.

## Detail per check

### #3 — `lint` now fails a corpus that has no rules (regression)

Current:

```sh
$ mkdir -p ~/adv/typesonly
$ printf '%s\n' '{"type": "myt", "nice": 5}' > ~/adv/typesonly/a.types
$ cp /etc/ananicy.d/ananicy.conf ~/adv/typesonly/
$ aranicy --config-dir ~/adv/typesonly dump types     # the file is valid
{
  "myt": {
    "nice": 5
  }
}
exit=0
$ aranicy --config-dir ~/adv/typesonly lint
warning: 0 rules loaded from /home/arch/adv/typesonly: this corpus will apply nothing
lint: warnings found
exit=1
```

and an empty `a.rules`:

```sh
$ aranicy --config-dir ~/adv/empty lint
warning: config: config "/home/arch/adv/empty/ananicy.conf" not found; using defaults
warning: 0 rules loaded from /home/arch/adv/empty: this corpus will apply nothing
lint: warnings found
exit=1
```

Previous, same input file:

```sh
--- half
warning: 0 rules loaded from /home/arch/adv/half: this corpus will apply nothing   # (current)
lint: warnings found
exit=1
--- half
OK: 0 rules, 0 types, 0 cgroups
exit=0
```

(the previous build prints no `0 rules loaded` line at all.)

The stricter exit code is deliberate, not an accident: the current build's
`aranicy lint --help` says

```
Exit 1 on any finding: an error (a rule that would not load) or a warning (a rule that loads but can never match, a file that is never read). A corpus that is green is a corpus that will produce policy.
```

so "this corpus will apply nothing" is now classified as a finding. My
disagreement is with the granularity, not with the intent: a directory of
`*.types` that a `*.rules` file elsewhere references cannot be linted green on
its own. Ren-14 is filed as an enhancement for that reason.

### #13/#14 — cgroup quota: same code, corrected docs, still broken at boot

`CPUQuota = 50` on 2 vCPUs. Both builds write the same thing when the cgroup is
created by a reload:

```sh
# current
$ sudo aranicy --reload; sleep 1; cat /sys/fs/cgroup/rencg/cpu.max
100000 100000
# previous (cgroup dir removed first, then created by the reload)
$ sudo rmdir /sys/fs/cgroup/rencg
$ sudo aranicy --reload; sleep 1
/sys/fs/cgroup/rencg
cpu.max=100000 100000
$ cat /proc/$(pgrep -x rencgapp)/cgroup
0::/rencg
```

The previous build's `COMPATIBILITY.md` described this as "period 1 s" (i.e.
500000/1000000); the current one says "period fixed at the kernel's 100 ms".
The *behaviour* did not change, so the previous sentence was the wrong one and
the current one is right — docs better, code same.

At a cold boot both builds produce the same wrong state:

```sh
# current, 10:11:30 boot
$ sudo journalctl -u aranicy -b --no-pager -o cat | grep -iE "cgroup"
2026-11-11T10:11:30.307217Z  WARN cgroup "rencg" not created: Permission denied (os error 13)
$ ls -d /sys/fs/cgroup/rencg; cat /sys/fs/cgroup/rencg/cpu.max
/sys/fs/cgroup/rencg
max 100000
$ ( ~/rencgapp 40 & ); sleep 3; cat /proc/$(pgrep -x rencgapp)/cgroup
0::/rencg
# previous, 10:18:40 boot
$ sudo journalctl -u aranicy -b --no-pager -o cat | grep -iE "cgroup|restored"
2026-11-11T10:18:40.187387Z  WARN cgroup "rencg" not created: Permission denied (os error 13)
2026-11-11T10:18:40.238288Z  INFO bias: restored history entries=58
$ cat /sys/fs/cgroup/rencg/cpu.max
max 100000
```

### #18 — removal now cleans the learned history (fixed)

Current:

```sh
$ sudo pacman -R --noconfirm aranicy aranicy-collector
$ sudo ls /var/lib/aranicy
ls: cannot access '/var/lib/aranicy': No such file or directory
$ ls /run/aranicy /run/aranicy.pid
ls: cannot access '/run/aranicy': No such file or directory
ls: cannot access '/run/aranicy.pid': No such file or directory
```

Previous:

```sh
$ sudo pacman -R --noconfirm aranicy aranicy-collector
$ sudo ls -la /var/lib/aranicy
total 16
drwxr-xr-x 1 root root    20 Nov 11 10:19 .
drwxr-xr-x 1 root root    20 Nov 11 10:13 ..
-rw-r--r-- 1 root root 12745 Nov 11 10:19 bias-state
```

That is the `.INSTALL` hunk the current package adds, and it does what its
comment says. It also fixes the case the comment describes: with the previous
build, reinstalling after a removal would have restored 12 kB of learned biases
for processes that no longer exist.

### #19/#20 — the two behaviour complaints from last round

Both builds, 225 s with three `yes` copies (one named `sshd9`, a prefix on the
shipped `bias_protect` list) on an idle-besides load:

```sh
$ sudo journalctl -u aranicy -b --no-pager -o cat | grep "bias changed"     # current
2026-11-11T10:00:49.330892Z  INFO bias changed name=renbulk bias=1 prev=0 class=Bulk cause=Evidence
2026-11-11T10:00:54.329794Z  INFO bias changed name=sshd9 bias=1 prev=0 class=Bulk cause=Evidence
2026-11-11T10:01:59.329852Z  INFO bias changed name=zzunprot bias=3 prev=2 class=Bulk cause=Evidence
$ ps -eo pid,comm,nice | grep -E "renbulk|sshd9|zzunprot"
   3856 renbulk           2
   3858 sshd9             0
   3860 zzunprot          1
```

```sh
$ sudo journalctl -u aranicy -b --no-pager -o cat | grep "bias changed"     # previous
2026-11-11T10:15:04.417144Z  INFO bias changed name=renbulk bias=1 prev=0 class=Bulk cause=Evidence
2026-11-11T10:15:09.417129Z  INFO bias changed name=sshd9 bias=1 prev=0 class=Bulk cause=Evidence
2026-11-11T10:16:14.417318Z  INFO bias changed name=zzunprot bias=3 prev=2 class=Bulk cause=Evidence
$ ps -eo pid,comm,nice | grep -E "renbulk|sshd9|zzunprot"
   1161 renbulk           3
   1163 sshd9             0
   1165 zzunprot          3
```

Identical: the protected name is still learned (and still spends global steps,
three of the nine in each run) and still never actuated. `ren-9` unchanged.

The daemon's own priority under the same load (`Nice=-5` in the unit):

```sh
$ for i in 1 2 3 4 5; do sleep 45; echo "t+$((i*45))s daemon proc=$(awk '{print $19}' /proc/$D/stat) unit=$(systemctl show -p Nice --value aranicy)"; done   # current
t+45s  daemon proc=-5 unit=-5
t+90s  daemon proc=-5 unit=-5
t+135s daemon proc=-5 unit=-5
t+180s daemon proc=-5 unit=-5
t+225s daemon proc=-5 unit=-5
```

Same on the previous build. Both classify `aranicy` (current: `class=bulk,
conf 0.75`; previous: `class=responsive, conf 0.94`) but neither ever *acts* on
it, so `/proc` and `systemctl show -p Nice` agree for the whole run. `ren-10`
did not reproduce on either build on this machine — the difference from round 2
is the environment (no `scx_lavd` samples; the collector's own tracepoints feed
the engine), so I am reporting "not reproducible here", not "fixed".

### #21/#22 — the documentation got worse in this build

```sh
$ ls -R /usr/share/doc/aranicy
/usr/share/doc/aranicy:
README.md
docs

/usr/share/doc/aranicy/docs:
BENCHMARKS.md
COMPATIBILITY.md
$ grep -n "packaged documentation" -A2 /usr/share/doc/aranicy/README.md
117:The packaged documentation is `/usr/share/doc/aranicy/README.md`,
118-`COMPATIBILITY.md` and `BENCHMARKS.md`.
$ ls -l /usr/share/doc/aranicy/COMPATIBILITY.md /usr/share/doc/aranicy/BENCHMARKS.md
ls: cannot access '/usr/share/doc/aranicy/COMPATIBILITY.md': No such file or directory
ls: cannot access '/usr/share/doc/aranicy/BENCHMARKS.md': No such file or directory
```

Previous build: `ls ~/x/prev/usr/share/doc/aranicy/` → `BENCHMARKS.md
COMPATIBILITY.md README.md`, so there the sentence was true. The files moved
into `docs/` and the sentence did not move with them (ren-16).

And the README now states two different results for the same benchmark:

```sh
$ grep -n "12.3% of one core\|6.0× cheaper\|≤ 5% of one core" /usr/share/doc/aranicy/README.md
17:installs — the daemon *and* its collector**: measured **12.3% of one core at
50:Implemented and benchmarked (2026-10-22): **6.0× cheaper than ananicy-cpp
```

Line 17 says the package costs 12.3 % of a core against ananicy-cpp's 13.7 %
(a 1.1× difference); line 50 still advertises "6.0× cheaper … 2.28 % vs
13.7 %", which line 16-22 of the same file says describes "the retired
single-process architecture". The previous build did not contradict itself: it
stated a budget ("≤ 5 % of one core") and a measured result (2.28 %), which are
consistent with each other. The edit that added the 12.3 % sentence left the
Status paragraph behind, so the contradiction is new. ren-17.

## What I am not claiming

- Any performance number. I did not benchmark either build; the README's own
  figures are what I quote, and they disagree with each other.
- That `ren-10` is fixed. It did not reproduce in either build *on this
  machine*, whose scheduler setup differs from round 2's.
- Anything about `x3d_mode`, `cpuset`, `latency_nice` or RT scheduling: the
  kernel here has no `sched_setattr` LATENCY_NICE (`WARN kernel does not
  support latency_nice`), and I did not build a workload that needs RT.

## Appendix — the fixed check set (`~/bset.sh`, run identically on both builds)

```sh
#!/bin/bash
# fixed check set (differential). run on a clean machine with the build installed.
set -u
say() { echo; echo "########## $*"; }
say "B1 version"
aranicy --version; echo "exit=$?"
aranicy-collectord --version; echo "exit=$?"

say "B2 lint matrix"
for d in empty trunc unknown range regex mixed conf half typed good; do
  echo "--- $d"
  aranicy --config-dir ~/adv/$d lint 2>&1; echo "exit=$?"
done

say "B3 enable+start"
sudo systemctl enable --now aranicy >/dev/null 2>&1; echo "enable_exit=$?"
sleep 6
systemctl is-active aranicy aranicy-collector
sudo ls -1 /run/aranicy

say "B4 dump rules (empty corpus)"
aranicy dump rules

say "B5 live rule apply"
sudo install -m 644 ~/adv/good/zz-test.rules /etc/ananicy.d/zz-test.rules
sudo aranicy --reload; sleep 1
aranicy dump rules
cp /bin/sleep ~/renapp
( ~/renapp 40 >/dev/null 2>&1 & ); sleep 2
ps -eo pid,comm,nice | grep -w renapp | grep -v grep
ionice -p $(pgrep -x renapp)

say "B6 match-test"
aranicy --config-dir ~/adv/good match-test renapp; echo "exit=$?"
aranicy --config-dir ~/adv/good match-test renregex; echo "exit=$?"
aranicy --config-dir ~/adv/good match-test nosuch; echo "exit=$?"

say "B7 reload with a broken rules file"
printf '%s\n' '{"name": "renapp", "nice": 7, "ioclass": "idle"}' '{"name": "renbroken", "nice": 5' | sudo tee /etc/ananicy.d/zz-test.rules >/dev/null
sudo aranicy --reload; sleep 1
aranicy dump rules
sudo cp ~/adv/good/zz-test.rules /etc/ananicy.d/zz-test.rules
sudo aranicy --reload >/dev/null; sleep 1

say "B8 cgroup CPUQuota=50 on $(nproc) cpus"
sudo install -m 644 ~/adv/cg/zz-cg.rules /etc/ananicy.d/zz-cg.rules
sudo install -m 644 ~/adv/cg/zz-cg.cgroups /etc/ananicy.d/zz-cg.cgroups
sudo aranicy --reload >/dev/null; sleep 1
aranicy dump cgroups
cp /bin/sleep ~/rencgapp; ( ~/rencgapp 30 >/dev/null 2>&1 & ); sleep 3
P=$(pgrep -x rencgapp); echo "pid=$P"; grep cgroup /proc/$P/cgroup
for d in /sys/fs/cgroup/rencg /sys/fs/cgroup/*/rencg; do [ -d "$d" ] && echo "$d cpu.max=$(cat $d/cpu.max 2>/dev/null)"; done

say "B9 non-root start"
aranicy start; echo "exit=$?"
sudo -u nobody /usr/bin/aranicy start; echo "exit=$?"

say "B10 rename evasion (exec -a, prctl, copy)"
( bash -c 'exec -a renapp /bin/sleep 20' & ); sleep 2
ps -eo pid,comm,nice,args | grep -E 'sleep 20' | grep -v grep
( python3 ~/renprctl.py & ); sleep 2
ps -eo pid,comm,nice | grep renapp | grep -v grep

say "B11 collector stop (documented: daemon stays, degraded)"
sudo systemctl stop aranicy-collector; sleep 3
systemctl is-active aranicy aranicy-collector
pgrep -f '^/usr/bin/aranicy start' || echo "daemon gone"
sudo systemctl start aranicy-collector; sudo systemctl start aranicy; sleep 6
systemctl is-active aranicy aranicy-collector

say "B12 removal leftovers"
[ "${BSET_REMOVE:-1}" = "1" ] || { echo "(skipped)"; exit 0; }
sudo pacman -R --noconfirm aranicy aranicy-collector 2>&1 | tail -5
echo "--- leftovers"
ls -la /var/lib/aranicy 2>&1 | head -3
ls -la /run/aranicy 2>&1 | head -3
ls -la /etc/ananicy.d 2>&1
ls -l /etc/systemd/system/multi-user.target.wants/ 2>/dev/null | grep -i aranicy || echo "no enable symlink"
pgrep -f '^/usr/bin/aranicy start' || echo 'no daemon process'
pgrep -x aranicy-collect || echo 'no collector process'
script_done=1
```

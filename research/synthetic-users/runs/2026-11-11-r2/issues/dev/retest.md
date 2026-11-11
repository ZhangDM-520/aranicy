# Round 2 retest — aranicy 0.3.0-1 (build 2026-11-11 08:59 UTC)

Machine: Arch Linux cloud image, kernel `7.2.11-arch1-1`, 2 vCPU x86_64,
1958 MiB, no display. Packages from `~/payload`, installed with
`sudo pacman -U --noconfirm`:

```
$ sha256sum ~/payload/*.pkg.tar.zst
79cd087f2ea43dcb0559ab73f0138d340c46e822f94b890045975d994bf1e5b9  aranicy-0.3.0-1-x86_64.pkg.tar.zst
c201639e13e027df36c36fe1ccadbcb5a5f45a8fb119b25eb9339f6aaadda613  aranicy-collector-0.3.0-1-x86_64.pkg.tar.zst
$ tar --zstd -xOf aranicy-0.3.0-1-x86_64.pkg.tar.zst .PKGINFO | grep builddate
builddate = 1794387575
$ date -d @1794387575 -Is
2026-11-11T08:59:35+00:00
```

The version string is still `0.3.0-1`; the build timestamp and hashes above are
what tells this build apart from the one I tested last session. Everything below
was run against this pair.

| # | report | verdict |
|---|---|---|
| 1 | lint passes rules that cannot apply | **different** — three of the four holes closed, duplicates and undefined cgroup refs still silent |
| 2 | lint exits 1 without a daemon config | **still happens** (wording improved, a workaround exists) |
| 3 | rules matched on `basename(argv[0])` | **gone** — matching is now `basename(/proc/<pid>/exe)` |
| 4 | no shipped rule reference / dead doc links | **different** — `COMPATIBILITY.md` + `BENCHMARKS.md` now ship; README links and `contrib/install.sh` still dead |
| 5 | overhead above documented budget | **different** — roughly half of last round, still above the ≤5% line on 2 vCPU |
| 6 | `pacman -Rns` leaves the daemons running | **gone** — both packages now ship a `pre_remove` that disables/stops the units |
| 7 | bias demotes PID 1 and core services | **gone for the symptom** — PID 1 stays at 0; the protection is incomplete in a new way (separate issue) |
| 8 | no rule provenance | **still happens** |

---

## ren-1 — lint passes rules that cannot apply

### (a) glob in `name` — gone

```
$ printf '%s\n' '{ "name": "cc1*", "nice": 5 }' > ~/repro/rules/a.rules
$ aranicy --config-dir ~/repro lint; echo "LINT=$?"
warning: /home/arch/repro/rules/a.rules:1: name "cc1*" is matched exactly; patterns need name_regex
lint: warnings found
LINT=1
$ aranicy --config-dir ~/repro match-test cc1plus; echo "MT=$?"
match-test: no rule matches "cc1plus"
MT=1
```

### (b) empty name — gone

```
$ printf '%s\n' '{ "name": "", "nice": 5 }' > ~/repro/rules/a.rules
$ aranicy --config-dir ~/repro lint; echo "LINT=$?"
warning: /home/arch/repro/rules/a.rules:1: empty name can never match a process
lint: warnings found
LINT=1
```

The rule is still *loaded* (`dump rules` lists `"name": ""`), but lint now
refuses to call the corpus green, which is what I asked for.

### (d) file with the wrong extension — gone

```
$ printf '%s\n' '{ "name": "x", "nice": 5 }' > ~/repro/rules/a.json; rm -f ~/repro/rules/a.rules
$ aranicy --config-dir ~/repro lint; echo "LINT=$?"
warning: /home/arch/repro/rules/a.json:0: skipped: only *.rules /*.types /*.cgroups are loaded
lint: warnings found
LINT=1
```

Nit: the position of a whole-file diagnostic is reported as `:0:` — every other
message is `file:line:`.

### (c) the same name in two files — **still happens**

```
$ printf '%s\n' '{ "name": "zzsleep", "nice": 15 }' > ~/repro/rules/10-a.rules
$ printf '%s\n' '{ "name": "zzsleep", "nice": 2 }'  > ~/repro/rules/90-z.rules
$ aranicy --config-dir ~/repro lint; echo "LINT=$?"
OK: 1 rules, 0 types, 0 cgroups
LINT=0
$ aranicy --config-dir ~/repro match-test zzsleep
{
  "match": "exact",
  "name": "zzsleep",
  "policy": {
    "nice": 2
  }
}
```

Still no warning, and the duplicate is a **replace**, not a merge — the later
definition silently drops fields it does not repeat:

```
$ printf '%s\n' '{ "name": "dup", "nice": 9, "ioclass": "idle" }' > ~/t2/rules/10-dup.rules
$ printf '%s\n' '{ "name": "dup", "nice": 1 }'                  > ~/t2/rules/20-dup.rules
$ aranicy --config-dir ~/t2 match-test dup
{
  "match": "exact",
  "name": "dup",
  "policy": {
    "nice": 1
  }
}
```

`ioclass: idle` vanished; the rule that "won" is the *whole* later object. The
shipped `COMPATIBILITY.md` documents replace-outright only for cgroup
definitions ("Duplicates: later-loaded `name` replaces outright"), not for
rules, and lint stays green.

### undefined `cgroup` reference — **still happens** (runtime half of ren-1)

```
$ printf '%s\n' '{ "name": "zzcg", "cgroup": "zzgroup" }' | sudo tee /etc/ananicy.d/rules/60-cg.rules
$ sudo systemctl reload aranicy; sleep 1
$ aranicy lint --config-dir /etc/ananicy.d; echo "lint=$?"
OK: 3 rules, 0 types, 0 cgroups
lint=0
$ aranicy --config-dir /etc/ananicy.d match-test zzcg
{
  "match": "exact",
  "name": "zzcg",
  "policy": {
    "cgroup": "zzgroup"
  }
}
$ cp /bin/sleep ~/zzcg; ~/zzcg 12 & p1=$!; sleep 2
$ cat /proc/$p1/cgroup
0::/user.slice/user-1000.slice/session-57.scope
$ sudo journalctl -u aranicy --no-pager -o cat -n 3
2026-11-11T09:18:53.156586Z  INFO reloaded 3 rules; bias reconfigured
```

`lint` says 0 cgroups and still exits 0; `match-test` shows the reference; the
process is not moved; the journal has no line about `zzgroup`. Define the
cgroup and the identical rule works:

```
$ printf '%s\n' '{ "cgroup": "zzgroup", "CPUQuota": 20 }' | sudo tee /etc/ananicy.d/cgroups/zz.cgroups
$ sudo systemctl reload aranicy; sleep 1
$ ~/zzcg 8 & p=$!; sleep 2; cat /proc/$p/cgroup
0::/zzgroup
```

(The undefined-`type` case *does* warn at lint and at reload — so the two
reference kinds are still treated differently.)

---

## ren-2 — lint exits 1 on a rules-only checkout

**Still happens**, with two changes: `lint --help` now states the policy, and a
workaround exists.

```
$ rm -rf ~/repro2 && mkdir -p ~/repro2/rules
$ printf '%s\n' '{ "name": "x", "nice": 5 }' > ~/repro2/rules/a.rules
$ aranicy --config-dir ~/repro2 lint; echo "exit=$?"
warning: config: config "/home/arch/repro2/ananicy.conf" not found; using defaults
lint: warnings found
exit=1
$ printf '%s\n' '{ "name": "x", }' > ~/repro2/rules/a.rules
$ aranicy --config-dir ~/repro2 lint; echo "exit=$?"
warning: config: config "/home/arch/repro2/ananicy.conf" not found; using defaults
warning: /home/arch/repro2/rules/a.rules:1: invalid JSON in span: trailing comma at line 1 column 16
lint: warnings found
exit=1
$ aranicy lint --help | sed -n 1,4p
Validate rules and report problems without running.

Exit 1 on any finding: an error (a rule that would not load) or a warning (a rule that loads but can never match, a file that is never read). A corpus that is green is a corpus that will produce policy.
```

So the help text now matches the behaviour (it says *any* finding is exit 1),
which retires the "help says errors, behaviour says warnings" half of my report.
What remains is that a benign, expected state — a rules-only checkout, which is
exactly how the README tells me to keep my corpus ("Put your own files in
`/etc/ananicy.d/` … or point `ANANICY_CPP_CONFDIR` at a corpus") — is
indistinguishable from a broken rule in kind and in the summary line.

The workaround I guessed at last round does work and is undocumented:

```
$ aranicy --config-dir ~/repro2 --config /dev/null lint; echo "exit=$?"
warning: /home/arch/repro2/rules/a.rules:1: unknown type "Heavy" referenced; rule keeps its own fields
lint: warnings found
exit=1
```

(`/dev/null` as a config file is accepted and the missing-config warning
disappears; the remaining warning is a real finding about the rule. Neither
`--help` nor `COMPATIBILITY.md` mentions this.) The doubled prefix
`config: config "…"` is also still there.

---

## ren-3 — rules matched on `basename(argv[0])`

**Gone.** Matching is `basename(/proc/<pid>/exe)` now, which is what
`COMPATIBILITY.md` documents as a "deliberate divergence from ananicy-cpp".

```
$ sudo tee /etc/ananicy.d/rules/70-argvdemo.rules <<EOF
{ "name": "argvdemo", "nice": 14 }
EOF
$ sudo systemctl reload aranicy; sleep 1
$ cp /bin/sleep ~/argvdemo

# (1) argv[0] claims the name, the executable is /bin/sleep
$ bash -c 'exec -a argvdemo /bin/sleep 20' & pid=$!; sleep 1
$ tr '\0' ' ' < /proc/$pid/cmdline; echo; cat /proc/$pid/comm; awk '{print "nice=" $19}' /proc/$pid/stat; ls -l /proc/$pid/exe
argvdemo 20 
sleep
nice=0
lrwxrwxrwx 1 arch arch 0 Nov 11 09:07 /proc/1248/exe -> /usr/bin/sleep

# (2) the executable is named argvdemo, argv[0] says something else
$ bash -c 'exec -a innocuous ~/argvdemo 20' & pid2=$!; sleep 1
$ tr '\0' ' ' < /proc/$pid2/cmdline; echo; cat /proc/$pid2/comm; awk '{print "nice=" $19}' /proc/$pid2/stat; ls -l /proc/$pid2/exe
innocuous 20 
argvdemo
nice=14
lrwxrwxrwx 1 arch arch 0 Nov 11 09:07 /proc/1254/exe -> /home/arch/argvdemo
```

Both directions are now the safe way round: `exec -a` can no longer borrow a
rule, and a binary keeps its own rule after rewriting `argv[0]`.

---

## ren-4 — nothing shipped describes the rule format

**Different.** `COMPATIBILITY.md` (309 lines) and `BENCHMARKS.md` (437 lines)
now ship, and `COMPATIBILITY.md` answers almost everything I had to find by
trial and error: the full field table (`name`, `name_regex`, `cmdlines`, `type`,
`nice`, `latency_nice`, `sched`, `rtprio`, `ioclass`, `ionice`,
`oom_score_adj`, `cgroup`, `cpuset`, `autogroup`), the cgroup definition shape,
load order, the config key list, reload liveness per key.

```
$ ls /usr/share/doc/aranicy/
BENCHMARKS.md  COMPATIBILITY.md  README.md
$ ls /usr/share/doc/aranicy-collector/
README.md
```

What is left of the report:

1. The README's links are still dead *as installed*, because they are relative
   links into a source tree that is not shipped:

```
$ grep -n 'docs/\|contrib/\|tools/' /usr/share/doc/aranicy/README.md
13:[docs/BENCHMARKS.md](docs/BENCHMARKS.md)). The measured Rust candidate
42:([docs/COMPATIBILITY.md](docs/COMPATIBILITY.md)).
54:your own with `tools/storm/` (source tree) or `pidstat -p $(pgrep aranicy)`. 112 unit/integration tests (2026-10-28). See
58:- Compatibility surface: [docs/COMPATIBILITY.md](docs/COMPATIBILITY.md)
86:`contrib/PKGBUILD` builds both as one split package: `aranicy` depends on
122:./contrib/install.sh                  # build if needed, install, enable --now
125:./contrib/install.sh --uninstall      # remove binary + unit, keep config
$ ls /usr/share/doc/aranicy/docs
ls: cannot access '/usr/share/doc/aranicy/docs': No such file or directory
$ find / -name install.sh -path '*contrib*' 2>/dev/null | head
(no output)
```

   The files are siblings of the README now, so the link text should be
   `BENCHMARKS.md`, and `contrib/install.sh` (including the documented
   `--uninstall`) still does not exist on the machine.

2. The cgroup definition key changed and the previous build's accepted spelling
   is now rejected. Last round the only recognised key was `cpu_quota_pct`; this
   build documents and accepts `CPUQuota`:

```
$ printf '%s\n' '{ "cgroup": "zzgroup", "CPUQuota": 50 }' > ~/t1/cgroups/zz.cgroups
$ aranicy --config-dir ~/t1 lint; echo "exit=$?"; aranicy --config-dir ~/t1 dump cgroups
OK: 0 rules, 0 types, 1 cgroups
exit=0
{
  "zzgroup": {
    "cpu_quota_pct": 50
  }
}
$ printf '%s\n' '{ "cgroup": "zzgroup", "cpu_quota_pct": 50 }' > ~/t1/cgroups/zz.cgroups
$ aranicy --config-dir ~/t1 lint; echo "exit=$?"
warning: /home/arch/t1/cgroups/zz.cgroups:1: unknown key "cpu_quota_pct" ignored
lint: warnings found
exit=1
```

   That is the right direction (documented key works, the old one warns loudly
   with exit 1) — but a corpus written against the previous build now has to be
   edited, and `dump cgroups` still prints the internal name `cpu_quota_pct`
   rather than the key I wrote.

---

## ren-5 — overhead above the documented budget

**Different, still above the documented line on this box.** Same measurement
shape as last round (daemon and collector CPU from `/proc/<pid>/stat` fields
14+15, wall clock from `date +%s.%N`, 100 = one full core, empty rule corpus
except where marked "ruled"), same 2-vCPU machine.

```
$ ~/bench/measure.sh 20 idle -- sleep 20
idle elapsed=20.00s daemon_cpu=0.010s (0.0% of one core) collector_cpu=0.030s (0.1% of one core)

$ ~/bench/measure.sh 10 rate50 -- python3 ~/bench/rate.py 10 50
500
rate50 elapsed=10.02s daemon_cpu=0.020s (0.2% of one core) collector_cpu=0.040s (0.4% of one core)

$ ~/bench/measure.sh 10 rate200 -- python3 ~/bench/rate.py 10 200
2000
rate200 elapsed=10.03s daemon_cpu=0.030s (0.3% of one core) collector_cpu=0.100s (1.0% of one core)

$ ~/bench/measure.sh 10 rate395 -- python3 ~/bench/rate.py 10 395
3950
rate395 elapsed=10.02s daemon_cpu=0.090s (0.9% of one core) collector_cpu=0.200s (2.0% of one core)

# the documented protocol point
$ ~/bench/measure.sh 20 rate2000-unruled -- python3 ~/bench/rate.py 20 2000
40000
rate2000-unruled elapsed=20.03s daemon_cpu=0.450s (2.2% of one core) collector_cpu=1.170s (5.8% of one core)
$ ~/bench/measure.sh 20 rate2000-unruled-rep2 -- python3 ~/bench/rate.py 20 2000
40000
rate2000-unruled-rep2 elapsed=20.02s daemon_cpu=0.750s (3.7% of one core) collector_cpu=1.050s (5.2% of one core)

# ruled: { "name": "true", "nice": 7, "ioclass": "best-effort", "ionice": 5 }
$ ~/bench/measure.sh 20 rate2000-ruled -- python3 ~/bench/rate.py 20 2000
40000
rate2000-ruled elapsed=20.02s daemon_cpu=0.690s (3.4% of one core) collector_cpu=0.950s (4.7% of one core)
$ ~/bench/measure.sh 20 rate2000-ruled-rep2 -- python3 ~/bench/rate.py 20 2000
40000
rate2000-ruled-rep2 elapsed=20.03s daemon_cpu=0.720s (3.6% of one core) collector_cpu=1.050s (5.2% of one core)

# burst, two fork/exec workers (5500 execs/s achieved)
$ ~/bench/measure.sh 10 storm2-b -- bash ~/bench/stormn.sh 10 2
27200
28384
storm2-b elapsed=10.02s daemon_cpu=0.910s (9.1% of one core) collector_cpu=1.360s (13.6% of one core)
```

| load | daemon | collector | round-1 pair |
|---|---|---|---|
| idle | 0.0 % | 0.1 % | 0.2 / 0.75 % |
| 200 execs/s | 0.3 % | 1.0 % | 4.3 / 7.7 % |
| 395 execs/s | 0.9 % | 2.0 % | 7.2 / 9.7 % |
| 2 000 execs/s unruled | 2.2–3.7 % | 5.2–5.8 % | — |
| 2 000 execs/s ruled | 3.4–3.6 % | 4.7–5.2 % | — |
| ~5 500 execs/s burst | 9.1 % | 13.6 % | 5.1 / 13.4 % (at 1 644/s) |

Two things changed substantively: paced load is about an order of magnitude
cheaper than last round at the same rate (200/s: 12.0 % of a core for the pair
then, 1.3 % now), and the README no longer states the budget as an unconditional
guarantee in its *Status* section ("The budget is a target for the storm
protocol, not a guarantee for your machine"). Break the pair apart at the
documented 2 000 execs/s and the *daemon* is at **2.2–3.7 % of one core**, which
is the range `BENCHMARKS.md`'s "Shipping gate numbers" table reports (3.88 % /
4.93 % for the same cells, measured on a 12C/24T host with the governor pinned)
— so the gate, read as "daemon only", holds. What does not fit in ≤5 % is the
**pair**: daemon + collector is **8.0–8.9 % of one core** at the same rate, and
the collector alone is at 4.7–5.8 %. Since `aranicy.service` has
`Requires=aranicy-collector.service`, the pair is the package's cost. The
*Design* paragraph still states the budget without that distinction:
"aranicy is a clean-room rewrite with a hard performance budget: **≤ 5% of one
core at 2 000 execs/s**".

So: not the defect I reported (17–19 %), but not reproducibly inside the
documented budget on 2 vCPU either. I am reporting the number, not a regression
— I cannot separate the collector's fixed cadence cost from the 2-vCPU
environment with the evidence I have.

---

## ren-6 — `pacman -Rns` leaves both daemons running

**Gone.** Both packages now ship a `.INSTALL` with a `pre_remove`:

```
$ tar --zstd -xOf aranicy-0.3.0-1-x86_64.pkg.tar.zst .INSTALL
pre_remove() {
    systemctl --quiet disable --now aranicy.service 2>/dev/null || true
}
...
$ tar --zstd -xOf aranicy-collector-0.3.0-1-x86_64.pkg.tar.zst .INSTALL
pre_remove() {
    systemctl --quiet disable --now aranicy-collector.service 2>/dev/null || true
}
```

The exact scenario from the report:

```
$ pgrep -af "/usr/bin/aranicy"; systemctl is-active aranicy aranicy-collector
385663 /usr/bin/aranicy-collectord run
395967 /usr/bin/aranicy start
active
active
$ sudo pacman -Rns --noconfirm aranicy aranicy-collector
...
removing aranicy...
removing aranicy-collector...
:: Running post-transaction hooks...
(1/2) Reloading system manager configuration...
(2/2) Arming ConditionNeedsUpdate...
$ systemctl is-active aranicy aranicy-collector
inactive
inactive
$ pgrep -af "/usr/bin/aranicy"      # only my own bash -c line matches
$ sudo ls -la /run/aranicy
ls: cannot access '/run/aranicy': No such file or directory
$ ls -l /etc/systemd/system/multi-user.target.wants/ | grep -i aranicy
(no aranicy symlink)
$ sudo ls -la /var/lib/aranicy
-rw-r--r-- 1 root root 12188 Nov 11 09:27 bias-state
```

and the journal shows the stop happening inside the transaction:

```
$ sudo journalctl --no-pager -o short-iso -u aranicy -u aranicy-collector | tail -6
2026-11-11T09:27:08+00:00 su-dev-r2 systemd[1]: Stopping aranicy - auto-nice daemon...
2026-11-11T09:27:08+00:00 su-dev-r2 systemd[1]: aranicy.service: Deactivated successfully.
2026-11-11T09:27:08+00:00 su-dev-r2 systemd[1]: Stopped aranicy - auto-nice daemon.
2026-11-11T09:27:08+00:00 su-dev-r2 systemd[1]: Stopping aranicy collector - scheduler-agnostic BPF task statistics...
2026-11-11T09:27:08+00:00 su-dev-r2 systemd[1]: aranicy-collector.service: Deactivated successfully.
2026-11-11T09:27:08+00:00 su-dev-r2 systemd[1]: Stopped aranicy collector - scheduler-agnostic BPF task statistics.
```

Reinstalling now takes effect (the second half of my old report):

```
$ sudo pacman -U --noconfirm ~/payload/aranicy-collector-0.3.0-1-x86_64.pkg.tar.zst ~/payload/aranicy-0.3.0-1-x86_64.pkg.tar.zst
$ sudo systemctl start aranicy; sleep 4
$ systemctl is-active aranicy aranicy-collector
active
active
396492 /usr/bin/aranicy-collectord run
396493 /usr/bin/aranicy start
$ awk '{print "nice=" $19}' /proc/396493/stat
nice=-5
```

New pids, so the new binary is what runs. The only leftovers are
`/var/lib/aranicy/bias-state` (12 kB, inert until the package is reinstalled)
and my own unowned rule files in `/etc/ananicy.d/rules`. Nothing behind the
removal still changes priorities.

---

## ren-7 — bias demotes PID 1 and core system services

**Gone for the symptom I reported.** PID 1 was never moved in this session, and
this build ships the gate that keeps it out.

Configuration the daemon loads (the only config file on the machine):

```
$ sed -n "39,44p" /etc/ananicy.d/ananicy.conf
# Names the learned bias must never move (comm prefixes, space separated;
# comm is capped at 15 bytes, so `systemd` covers systemd-journald/-logind).
# Rules still apply to these names - this gates the learned axis only.
# PID 1 is always protected, list or no list. Set empty to protect nothing
# but init. Example: add your desktop's compositor shell or an editor.
bias_protect = systemd dbus-broker dbus-daemon sshd gdm sddm polkitd elogind
```

PID 1 under load, with the learned engine active and `systemd` in the history:

```
$ awk '{print "pid1 comm=" $2 " nice=" $19}' /proc/1/stat
pid1 comm=(systemd) nice=0
$ systemctl show -p Nice systemd
Nice=0
```

Even an explicit rule for the name does not move it (documented as "PID 1 is
never a target, rule or no rule"):

```
$ printf '%s\n' '{ "name": "systemd", "nice": 12 }' | sudo tee /etc/ananicy.d/rules/71-pid1.rules
$ sudo systemctl reload aranicy; sleep 2
$ aranicy --config-dir /etc/ananicy.d match-test systemd
{
  "match": "exact",
  "name": "systemd",
  "policy": {
    "nice": 12
  }
}
$ awk '{print "pid1 nice=" $19}' /proc/1/stat
pid1 nice=0
```

So the round-1 defect (PID 1 walked to nice 1 → 3 by bias) does not reproduce:
`/proc/1/stat` stayed at 0 for the whole session, including the windows where
the engine logged `bias changed name=systemd`.

That last fact is the new problem, and I filed it separately: the *learning*
half of the guard and the *actuation* half disagree, and the daemon's own name
is not on the list at all (`issues/ren-9-bias-protect-names-still-learned.md`,
`issues/ren-10-daemon-nices-itself.md`).

---

## ren-8 — no command reports which rule file produced a decision

**Still happens.** `dump rules` still has no provenance, `match-test` still stops
at the merged policy, and `dump proc` is still a live `/proc` reflection.

```
$ aranicy --config-dir ~/repro dump rules
{
  "exact": [
    {
      "match": "exact",
      "name": "zzsleep",
      "nice": 2
    }
  ],
  "regex": []
}
$ aranicy --config-dir ~/repro dump rules --help | head -5
Print loaded state as JSON (works unprivileged)

Usage: aranicy dump [OPTIONS] <WHAT>
$ aranicy dump proc | head -8
[
  {
    "name": "systemd",
    "nice": 0,
    "pid": 1
  },
```

The new documentation does not change this: `COMPATIBILITY.md` describes the
format of the *data*, not of the *provenance*. With a duplicate name resolving
silently (ren-1c above) I still cannot tell from the tool's output which of two
contradictory lines is in effect — I have to delete one and re-run.

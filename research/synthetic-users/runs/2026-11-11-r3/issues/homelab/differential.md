# Round 3 — differential: `~/previous-build` vs `~/payload`

- environment: Linux 7.2.11-arch1-1 (Arch rolling, cloud image), 2 vCPU
  x86_64, 1958 MiB RAM.
- both package pairs are version `0.3.0-1`; the build dates in `.PKGINFO`
  differ (`builddate = 1794387575` for `previous-build`,
  `1794390742` for `payload`), so this is a rebuild, not a version bump.
- method: install one pair, run a fixed scripted suite, uninstall, install the
  other pair, run the same suite. Every check below is the same command on
  both builds; the outputs are quoted from the two runs.

## What actually differs in the packages

```
$ diff -r prev/usr/share/doc cur/usr/share/doc
Only in prev/usr/share/doc/aranicy: BENCHMARKS.md
Only in prev/usr/share/doc/aranicy: COMPATIBILITY.md
diff -r prev/usr/share/doc/aranicy/README.md cur/usr/share/doc/aranicy/README.md
16,17c16,21
< clean-room rewrite with a hard performance budget: **≤ 5% of one core at
< 2 000 execs/s**.
---
> clean-room rewrite with a hard performance budget for **what the package
> installs — the daemon *and* its collector**: measured **12.3% of one core at
> 2 000 execs/s** (6.1 + 6.2) on the benchmark host, against ananicy-cpp's
> 13.7% for one process. ...
Only in cur/usr/share/doc/aranicy: docs

$ diff -u prev/.INSTALL cur/.INSTALL
@@ -13,4 +13,9 @@
 post_remove() {
     systemctl --quiet daemon-reload 2>/dev/null || true
+    # The learned history is derived state, not configuration: ...
+    rm -f /var/lib/aranicy/bias-state 2>/dev/null || true
+    rmdir /var/lib/aranicy 2>/dev/null || true
 }

$ diff -u prev/aranicy.service cur/aranicy.service ; echo $?
0
$ diff -u prev/aranicy-collector.service cur/aranicy-collector.service ; echo $?
0
$ diff -u prev/ananicy.conf cur/ananicy.conf ; echo $?
0
```

So: two behaviour changes (state cleanup on remove, doc layout), a rewritten
performance paragraph, identical units and identical default config.

## Per-check results

| # | check | previous | current | verdict |
|---|---|---|---|---|
| D1 | install, enable, start both units | active/active, `enabled`/`disabled` | active/active, `enabled`/`disabled` | **same** |
| D2 | `aranicy lint` on an empty corpus | `OK: 0 rules…`, exit 0 | `warning: 0 rules loaded…`, exit 1 | **better** |
| D2b | `journalctl -o json` parses | 3 records parsed | 3 records parsed | **same** |
| D3 | exact-name rule: `xz` nice 19 | `NI 19`, field19=19 | `NI 19`, field19=19 | **same** |
| D4 | `name_regex` rule: `gzip` nice 10 | `NI 10` | `NI 10` | **same** |
| D5 | sshd / journald / resolved / PID 1 / kworkers | 0/0/0/0, kworkers −20 | 0/0/0/0, kworkers −20 | **same** |
| D6 | `dump rules/types/cgroups/bias` | valid JSON | valid JSON | **same** |
| D7a | `dump proc` lists kernel threads | 0 `kworker` lines | 58 `kworker` lines | **better** |
| D7b | `match-test` against a defined `kworker/0:1-events` rule | cannot match (`no rule matches "0:1-events"`) | matches exactly; only the no-match message still prints `0:1` | **better (message nit left)** |
| D8 | rule naming an undefined cgroup | lint OK exit 0, silent | lint warning exit 1, reload warning | **better** |
| D9 | rule with `cpuset: turbo-cores` | lint OK, no log, affinity unchanged | lint OK, no log, affinity unchanged | **same** |
| D10 | `aranicy start` as uid 1000 | clear error, exit 1 | clear error, exit 1 | **same** |
| D11 | collector masked, manual restart | `Unit … is masked.` exit 1 | `Unit … is masked.` exit 1 | **same** |
| D12 | uninstall after learning | `/var/lib/aranicy/bias-state` left behind (6.7 kB) | `/var/lib/aranicy` removed | **better** |
| D13 | cgroup CPUQuota doc vs `cpu.max` | doc says "period 1 s"; `cpu.max` is `100000 100000` | doc says "period fixed at the kernel's 100 ms"; `cpu.max` is `100000 100000` | **better** |
| D14 | packaged doc paths in README | flat files exist and README names them | README names flat files, they are under `docs/` | **worse** |
| D15 | `pacman -U` over a running daemon | old inode keeps running | old inode keeps running | **same** |
| D16 | uninstall with a process in a managed cgroup | cgroup + quota left behind | cgroup + quota left behind | **same** |
| D17 | `systemctl stop aranicy` | units identical (diff 0) — verified on current only | collector stays active | **same (verified on current)** |
| D18 | `aranicy start` with no collector | not measured | loud error, exit 1 | **not compared** |

## Evidence for the verdicts that changed

**D2 — empty corpus lint (better).** Previous:

```
$ aranicy lint
OK: 0 rules, 0 types, 0 cgroups
[exit=0]
```

Current:

```
$ aranicy lint
warning: 0 rules loaded from /etc/ananicy.d: this corpus will apply nothing
lint: warnings found
[exit=1]
```

This is the fix I asked for last round: the Ansible gate now fails when the
corpus did not render.

**D7a — `dump proc` and kernel threads (better).** Previous:

```
$ sudo aranicy dump proc | grep -i kworker | head -5
[exit=0]
```

Current:

```
$ sudo aranicy dump proc | grep -c kworker
58
$ sudo aranicy dump proc | grep kworker | head -4
    "name": "kworker/R-rcu_gp",
    "name": "kworker/R-sync_wq",
    "name": "kworker/R-kvfree_rcu_reclaim",
    "name": "kworker/R-slub_flushwq",
```

**D7b — `match-test` and kernel-thread names (better, one cosmetic nit).** I set
up a config directory containing a rule keyed on a real kernel-thread name and
asked both builds about it.

Previous build cannot match the rule at all:

```
$ aranicy lint --config-dir ~/lrw
OK: 2 rules, 0 types, 0 cgroups
[exit=0]
$ aranicy match-test --config-dir ~/lrw "kworker/0:1-events"
match-test: no rule matches "0:1-events"
[exit=1]
$ aranicy match-test --config-dir ~/lrw "kworker/R-rcu_gp"
match-test: no rule matches "R-rcu_gp"
[exit=1]
```

Current build matches it exactly:

```
$ aranicy match-test --config-dir ~/lrw "kworker/0:1-events"
{
  "match": "exact",
  "name": "kworker/0:1-events",
  "policy": {
    "nice": 19
  }
}
[exit=0]
```

The one thing still odd is the *failure* message, which prints a name with
everything before the first `/` removed:

```
$ aranicy match-test --config-dir ~/lrw "kworker/0:1"
match-test: no rule matches "0:1"
[exit=1]
```

That is cosmetic and filed as `ivy-18`; the matching defect I reported last
round is fixed.

**D8 — undefined cgroup (better).** Previous:

```
$ aranicy lint
OK: 3 rules, 0 types, 0 cgroups
[exit=0]
$ sudo systemctl reload aranicy
2026-11-11T09:57:16.721374Z  INFO reloaded 3 rules; bias reconfigured
```

Current:

```
$ aranicy lint
warning: /etc/ananicy.d/ivy-52-openssl.rules:1: unknown cgroup "ivynosuch" referenced; the assignment will fail at apply time
lint: warnings found
[exit=1]
$ sudo journalctl -u aranicy --since "..." -o cat
Reloading aranicy - auto-nice daemon...
sent SIGHUP to aranicy (pid 3016)
2026-11-11T10:12:59.370525Z  WARN /etc/ananicy.d/ivy-52-openssl.rules:1: unknown cgroup "ivynosuch" referenced; the assignment will fail at apply time
2026-11-11T10:12:59.370784Z  INFO reloaded 3 rules; bias reconfigured
```

**D12 — uninstall (better).** Previous build, after ~45 learned entries:

```
$ sudo ls -la /var/lib/aranicy/
-rw-r--r-- 1 root root 6612 Nov 11 10:02 bias-state
$ sudo pacman -R --noconfirm aranicy aranicy-collector
...
$ sudo ls -la /var/lib/aranicy
total 8
drwxr-xr-x 1 root root   20 Nov 11 10:03 .
drwxr-xr-x 1 root root  188 Nov 11 10:00 ..
-rw-r--r-- 1 root root 6761 Nov 11 10:03 bias-state
[exit=0]
```

Current:

```
$ sudo ls -la /var/lib/aranicy
ls: cannot access '/var/lib/aranicy': No such file or directory
[exit=2]
```

**D13 — CPU quota documentation (better).** Both daemons write the same value;
only the sentence changed:

```
$ cat /sys/fs/cgroup/ivymedia/cpu.max
100000 100000
```

previous `COMPATIBILITY.md:41`: `CPUs (v2 `cpu.max`, period 1 s). Duplicates:
later-loaded `name` replaces`
current `COMPATIBILITY.md:41`: `CPUs (v2 `cpu.max`, period fixed at the kernel's
100 ms, i.e. quota =`

**D14 — packaged doc paths (worse).** Current README still says:

```
$ sed -n '115,122p' /usr/share/doc/aranicy/README.md
reports `0 rules` until you do, and the daemon warns about it at startup.

The packaged documentation is `/usr/share/doc/aranicy/README.md`,
`COMPATIBILITY.md` and `BENCHMARKS.md`.
```

but the package now installs:

```
$ ls -la /usr/share/doc/aranicy/
-rw-r--r-- 1 root root  6653 Nov 11 09:52 README.md
drwxr-xr-x 1 root root    58 Nov 11 10:28 docs
$ ls -la /usr/share/doc/aranicy/docs
-rw-r--r-- 1 root root 25420 Nov 11 09:52 BENCHMARKS.md
-rw-r--r-- 1 root root 19716 Nov 11 09:52 COMPATIBILITY.md
```

On the previous build the two files were where the README said they were, so
this is a small regression introduced by the doc move. Filed as `ivy-16`.

**D15 — upgrade over a running daemon (same).** Both builds leave the old
process running:

```
$ P=$(systemctl show -p MainPID --value aranicy); echo "pid=$P"
pid=459
$ sudo pacman -U --noconfirm ~/previous-build/...pkg.tar.zst
reinstalling aranicy...
$ sudo readlink /proc/459/exe
/usr/bin/aranicy (deleted)
$ systemctl show -p NRestarts aranicy
NRestarts=0
```

Current build, same output with `pid before=32971 after=32971` and
`/usr/bin/aranicy (deleted)`. Filed as `ivy-13`.

**D16 — cgroup after uninstall (same).** Both builds behave identically:

```
$ cat /sys/fs/cgroup/ivymedia/cpu.max
100000 100000
$ sudo pacman -R --noconfirm aranicy aranicy-collector
$ ls -d /sys/fs/cgroup/ivymedia
/sys/fs/cgroup/ivymedia
$ cat /sys/fs/cgroup/ivymedia/cpu.max
100000 100000
$ cat /proc/3331/cgroup
0::/ivymedia
```

Filed as `ivy-15`.

**D17 — stop asymmetry (same units).** The unit files are byte-identical
between the builds (`diff -u` exit 0), and on the current build:

```
$ sudo systemctl stop aranicy
$ systemctl is-active aranicy aranicy-collector
inactive
active
```

Filed as `ivy-14`.

## Things I did not compare

- D18 (direct `aranicy start` with no collector) was measured on the current
  build only: it fails loudly after 5 s with
  `Error: no collector at /run/aranicy/collector-events.sock ...`.
  I ran out of reasons to install the previous pair a fourth time; the
  readiness check and this code path are not what changed between the builds
  (the package diff above touches only documentation and the uninstall script).
- The junk-corpus battery (A1 in `adversarial.md`) was run on the current
  build only; it is not a regression check.
- The masked-collector **boot** behaviour is reported from the current build.
  The units are byte-identical, so the failure mode is the same; I did not
  spend a third reboot on the previous build to prove it.

## Regressions

One: `ivy-16` (README points at documentation paths that no longer exist in
the package). Everything else that changed got better, and the newly found
defects (masked boot, upgrade-without-restart, collector left running,
cgroup left behind, turbo-cores no-op) are present in both builds — they are
old, not new. `match-test`'s kernel-thread matching got better; only its
no-match message is still trimmed.

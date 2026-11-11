# Round 3, part B — differential against the previous build

## What the two builds are

Both package pairs say `0.3.0-1`, so the version number cannot tell them apart.
I distinguished them by build date and by content:

```
$ ./machine 'sudo pacman -Qip ~/payload/aranicy-0.3.0-1-x86_64.pkg.tar.zst | head -20; echo ---; sudo pacman -Qip ~/previous-build/aranicy-0.3.0-1-x86_64.pkg.tar.zst | head -20'
Name            : aranicy
Version         : 0.3.0-1
…
Installed Size  : 3825.27 KiB
…
Build Date      : Wed Nov 11 09:52:22 2026
---
Name            : aranicy
Version         : 0.3.0-1
…
Installed Size  : 3822.10 KiB
…
Build Date      : Wed Nov 11 08:59:35 2026
$ ./machine 'cd ~/pkgcmp; diff -rq payload-aranicy previous-build-aranicy; echo ---; diff -rq payload-aranicy-collector previous-build-aranicy-collector'
Files payload-aranicy/.BUILDINFO and previous-build-aranicy/.BUILDINFO differ
Files payload-aranicy/.INSTALL and previous-build-aranicy/.INSTALL differ
Files payload-aranicy/.MTREE and previous-build-aranicy/.MTREE differ
Files payload-aranicy/.PKGINFO and previous-build-aranicy/.PKGINFO differ
Files payload-aranicy/usr/bin/aranicy and previous-build-aranicy/usr/bin/aranicy differ
Only in previous-build-aranicy/usr/share/doc/aranicy: BENCHMARKS.md
Only in previous-build-aranicy/usr/share/doc/aranicy: COMPATIBILITY.md
Files payload-aranicy/usr/share/doc/aranicy/README.md and previous-build-aranicy/usr/share/doc/aranicy/README.md differ
Only in payload-aranicy/usr/share/doc/aranicy: docs
---
Files payload-aranicy-collector/.BUILDINFO and previous-build-aranicy-collector/.BUILDINFO differ
Files payload-aranicy-collector/.MTREE and previous-build-aranicy-collector/.MTREE differ
Files payload-aranicy-collector/.PKGINFO and previous-build-aranicy-collector/.PKGINFO differ
Files payload-aranicy-collector/usr/bin/aranicy-collectord and previous-build-aranicy-collectord differ
```

In this report: **A = current build** (`~/payload`, 09:52), **B = previous
build** (`~/previous-build`, 08:59).

## Method

The same script (`~/adv/checks.sh`) was run on both builds, plus a timed script
(`~/adv/slow2.sh`) for the things that need minutes, plus the two lifecycle
operations (remove/reinstall, reboot). Between builds I installed the other pair
with `pacman -U` and **restarted both services** — necessary, because installing
over a running daemon does not replace it (kaz-12, and it is the same bug in both
builds, so it would have poisoned every later check). Before each timed run I
stopped the daemon, deleted `/var/lib/aranicy/bias-state` and started it again,
so both builds learned from an empty history. Each timed run used fresh binary
names (`kba1/kha1` for A, `kbb1/khb1` for B) so no entry could be inherited.

## Scoreboard

| # | check | A (current) | B (previous) | verdict |
|---|---|---|---|---|
| 1 | README install command works end-to-end | yes | yes | same |
| 2 | `COMPATIBILITY.md` / `BENCHMARKS.md` where the README says they are | **MISSING** (they are in `docs/`) | PRESENT | **worse** |
| 3 | `lint` on a corpus with a junk line and a truncated line | 2 warnings, exit 1 | 2 warnings, exit 1 | same |
| 4 | `lint` on an empty corpus | warning + exit 1 | `OK … ` + exit 0 | better |
| 5 | `lint` on an impossible config | 3 warnings, exit 1 | 2 warnings, exit 1 | same (A adds one line) |
| 6 | `match-test` hit / miss | same JSON, exit 0 / 1 | same | same |
| 7 | `dump rules` | same shape | same | same |
| 8 | rule applied at exec to a new process | −5 then −2 with bias | −5 then −2 | same |
| 9 | 10 short-lived ruled processes | 10/10 correct | 10/10 correct | same |
| 10 | cgroup `CPUQuota: 50` → `cpu.max` | `100000 100000` | `100000 100000` | same |
| 11 | unruled CPU hog → bias 1 → 3 | ~95 s, ~155 s | ~95 s, ~155 s | same |
| 12 | rule −7 with learning on | −7 → −5 → −4 | −7 → −5 → −4 | same |
| 13 | half-written rule while the process runs | −7 → +3 (one WARN) | −7 → +1 (≈16 s) → +3 | same (both bad) |
| 14 | rule deleted while the process runs | −4 → +3, no log line | not run on B | — |
| 15 | `aranicy-collectord probe` unprivileged | libbpf EPERM wall, exit 1 | identical | same |
| 16 | `aranicy start` unprivileged | clear root error | identical | same |
| 17 | `kill -9` the collector | back in 6 s; daemon restarts too | back in 6 s; daemon restarts too | same |
| 18 | reboot with work in flight | services, sockets, 27 entries, rule OK | services, sockets, 61 entries, rule OK | same |
| 19 | `pacman -R aranicy` | collector keeps running | collector keeps running | same (both bad) |
| 20 | `pacman -R` both | bias-state **deleted** | bias-state **kept** | better |
| 21 | `pacman -U` over the running daemon | old binary keeps running | old binary keeps running | same (both bad) |
| 22 | privilege/trust caveat in the contract | **documented**, and true | absent | better |
| 23 | README performance claim | 12.3 % of a core for the pair | ≤ 5 % | changed; **not measured** |
| 24 | `aranicy --version` | `aranicy 0.3.0` | `aranicy 0.3.0` | same |

Regressions: **one** (check 2). Everything else is the same or slightly better.

## The regression

### 2. The packaged documentation moved and the README index sentence did not

```
$ ./machine 'for f in /usr/share/doc/aranicy/COMPATIBILITY.md /usr/share/doc/aranicy/docs/COMPATIBILITY.md /usr/share/doc/aranicy/BENCHMARKS.md /usr/share/doc/aranicy/docs/BENCHMARKS.md; do printf "%-52s " "$f"; test -e "$f" && echo PRESENT || echo MISSING; done'
/usr/share/doc/aranicy/COMPATIBILITY.md              MISSING
/usr/share/doc/aranicy/docs/COMPATIBILITY.md         PRESENT
/usr/share/doc/aranicy/BENCHMARKS.md                 MISSING
/usr/share/doc/aranicy/docs/BENCHMARKS.md            PRESENT
```

(with build B installed the same four lines read `PRESENT MISSING PRESENT
MISSING` — `result-B.txt` in `~/adv`.)

```
$ ./machine 'sed -n "115,120p" /usr/share/doc/aranicy/README.md'
reports `0 rules` until you do, and the daemon warns about it at startup.

The packaged documentation is `/usr/share/doc/aranicy/README.md`,
`COMPATIBILITY.md` and `BENCHMARKS.md`.
```

On B, the sentence was right and the README's own relative links
(`docs/BENCHMARKS.md`) were wrong. On A, the links are right and the sentence is
wrong. So this is a half-done move, and the half that is now wrong is the one a
user reads first. Filed as **kaz-14**.

## The improvements

### 4. An empty corpus is now a warning

```
A: warning: 0 rules loaded from /home/arch/adv/empty: this corpus will apply nothing
   lint: warnings found
   EXIT=1
B: OK: 0 rules, 0 types, 0 cgroups
   EXIT=0
```

The daemon on A also says it at startup:

```
$ ./machine 'sudo journalctl -u aranicy --no-pager --since "-10min" | head -4'
Nov 11 09:55:43 su-gamer-r3 aranicy[1244]: … WARN no rules loaded from /etc/ananicy.d: with an empty rule set only the learned bias changes priorities. The package ships no rule corpus - point ANANICY_CPP_CONFDIR at yours, or write files ending in .rules
```

That is the sentence I wanted on day one. Better. (It does mean `lint` exits 1
on a machine that is fine — I would not call that a bug, but a script that gates
on `lint` will now trip on a stock install.)

### 20. Removing the packages now cleans the learned history

```
$ diff ~/pkgcmp/payload-aranicy/.INSTALL ~/pkgcmp/previous-build-aranicy/.INSTALL
16,20d15
<     # The learned history is derived state, not configuration: leaving it
<     # behind means a later reinstall resurrects biases for a machine that has
<     # since changed (and the nice values they were applied to are gone).
<     rm -f /var/lib/aranicy/bias-state 2>/dev/null || true
<     rmdir /var/lib/aranicy 2>/dev/null || true
```

and in practice:

```
A: $ ./machine 'sudo pacman -R --noconfirm aranicy; … ls -la /var/lib/aranicy/'
   ls: cannot access '/var/lib/aranicy/': No such file or directory
B: $ ./machine 'sudo pacman -R --noconfirm aranicy; … ls -la /var/lib/aranicy/'
   -rw-r--r-- 1 root root 10145 Nov 11 10:21 bias-state
```

This is the right direction — it removes the "reinstall resurrects stale
biases" corner that kaz-7 was about. It does not fix kaz-7 itself (a *lost* state
file still leaves the applied nice in place, and the running processes are gone
only because the package removal stopped the daemon, not because anything
unwound them).

### 22. The contract now says out loud that any user can borrow a rule

New section in `docs/COMPATIBILITY.md` (build A):

```
* **A rule that raises priority is reachable by any user.** Identity is the
  executable, so an unprivileged process can copy any binary, name it after a
  rule (`cp /bin/sleep /tmp/game`), exec it, and receive that rule's
  `rtprio`/negative `nice` from the root daemon.
```

I checked it, from the unprivileged account, and it is true — and the wording
"negative nice" undersells it: `sched: fifo` comes through too.

```
$ ./machine 'cp /usr/bin/sleep ~/adv/kazborrow; cp /usr/bin/sleep ~/adv/kazfifo; setsid nohup ~/adv/kazborrow 20 >/dev/null 2>&1 </dev/null & setsid nohup ~/adv/kazfifo 20 >/dev/null 2>&1 </dev/null & sleep 3; ps -o pid,user,ni,comm -C kazborrow,kazfifo; chrt -p $(pgrep -x kazfifo|tail -1)'
    PID USER      NI COMMAND
    830 arch     -10 kazborrow
    831 arch       - kazfifo
pid 831's current scheduling policy: SCHED_FIFO
pid 831's current scheduling priority: 50
```

This is the single most useful change between the builds: the previous build let
me do that with no warning anywhere, the current one tells me before I write the
corpus. It is a documentation fix for a code behaviour that is unchanged.

## Where both builds are the same, and it is not good

### 12/13. A rule of −7 is not what the process gets, and losing the rule is silent

Timed run on B, fresh history, `kh` ruled at −7:

```
t=110 kb(ni)=0 kh(ni)=-7 [rule=-7]
--- truncate kh rule to 22 bytes (half-written save) ---
rule file now: {"name": "khb1", "nice
2026-11-11T10:13:53.086525Z  WARN /etc/ananicy.d/77-khb1.rules:1: no JSON object span (missing '{' or '}')
t=129 kb(ni)=1 kh(ni)=1
…
t=189 kb(ni)=3 kh(ni)=3
```

and with the rule kept whole (B, `kzb`, rule −7 in the file the whole time):

```
t=15 kzb(ni)=-7 rule=-7
t=30 kzb(ni)=-7 rule=-7
…
t=120 kzb(ni)=-5 rule=-7
…
t=181 kzb(ni)=-4 rule=-7
t=211 kzb(ni)=-4 rule=-7
```

(the current build shows the same two snapshots on `kazdiff`: `1353  -7  00:55
kazdiff` while the rule was whole, then `1353   3  04:28 kazdiff` after the
truncation described below. The intermediate −4 was not sampled on that name,
but it was on `kha1` in the same build, `slow-A.txt`:)

```
t=181 3 (kb)  -4 (kh, rule truncated)
```

(the label in that line is the script's own and it is wrong: in that run the
truncation step silently failed and the rule stayed whole — session log, item
35. So it reads −7 → −4 with the rule in force, which is the case I want it
for.)

Both builds: a −7 rule ends at **−4** while the file still says −7, and a
half-written rule turns −7 into **+3** within one drift pass with one line of
warning about a file and nothing about a process. Identical behaviour, so this
is not a regression — it is a bug that has been there for at least two rounds.
Filed as **kaz-11**.

### 17. A collector crash takes the daemon with it

```
A: collector=328 daemon=656 → collector back after 6s pid=875, daemon now=876 was=656
B: collector=1141 daemon=1483 → collector back after 6s pid=1634, daemon now=1635
```

Same on both. Neither unit file mentions that killing the stats collector
restarts the policy daemon.

### 21. Installing over a running service is a no-op

Same on both — the only difference is which stale binary you end up with:

```
A installed over B: stale check: /usr/bin/aranicy (deleted)
B installed over A: daemon pid=876 → /proc/876/exe -> /usr/bin/aranicy (deleted)
```

Both keep `ActiveState=active`, both leave `MarkedForRestart` empty. Filed as
**kaz-12**.

## Not measured

### 23. The performance claim changed and I did not verify either number

Build A's README says the pair costs **12.3 % of one core at 2 000 execs/s**
(6.1 + 6.2), replacing B's "≤ 5 % of one core". I did not try to reproduce it:
it needs a paced spawn storm on an idle host, this box has 2 vCPUs and a shared
hypervisor, and the README itself now says the number is a target for a specific
protocol, not a guarantee. What I did measure is that neither build's daemon or
collector is expensive at my kind of load:

```
$ ./machine 'for i in $(seq 300); do ~/adv/selfnice.py; done | sort -n | uniq -c; ps -o pid,pcpu,etime,comm -p $(systemctl show -p MainPID --value aranicy) $(systemctl show -p MainPID --value aranicy-collector)'
    300 -6
    PID %CPU     ELAPSED COMMAND
   1712  0.1       01:01 aranicy-collect
   1713  0.0       01:01 aranicy
```

0.1 % average over the daemon's first minute, a minute that contained a
300-exec burst, is not an A/B measurement and I am not treating it as one.

### 15. The collector probe still needs root

Unchanged: `aranicy-collectord probe` prints a libbpf/EPERM wall for a normal
user on both builds. kaz-10 (round 2) is still open.

## Verdict per check, in one line each

- Regression: **2** (docs moved, README index not updated).
- Better: **4** (empty corpus warns), **20** (uninstall cleans history), **22**
  (the privilege caveat is documented, and accurately).
- Same: 1, 3, 5, 6, 7, 8, 9, 10, 11, 12, 13, 15, 16, 17, 18, 19, 21, 24.
- Not comparable / not measured: 14 (only run on A), 23 (claim changed, not
  reproduced).

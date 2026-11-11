# Session log — dana, round 2

Machine `su-skeptic-r2`, provisioned fresh. All remote commands ran through
`./machine '<cmd>'` (ssh, user `arch`, uid 1000, passwordless sudo). Times are
UTC as printed by the box. Dead ends are kept.

## 0. Orientation (09:00–09:05)

- `cat card.md`, `cat sysinfo.txt`, `cat machine`, `ls previous-reports/` →
  six round-1 reports read in full.
- `uname -a; nproc; free -m; ls ~/payload; cat /sys/kernel/sched_ext/state; getconf CLK_TCK`
  → `7.2.11-arch1-1`, 2 vCPU, 1958 MiB, `disabled`, `100`.
- `bsdtar -xOf *.pkg.tar.zst .PKGINFO` → **both packages are still `0.3.0-1`**
  (`builddate = 1794387575`) even though the brief says "a newer build". Content
  differs from round 1, so the version string is not the discriminator.
- File lists → `/usr/share/doc/aranicy/BENCHMARKS.md` and `COMPATIBILITY.md`
  now **are** shipped (round 1: neither existed). `tools/storm/` and
  `contrib/install.sh` still are not, but the README now calls them "source
  tree".
- Read README, COMPATIBILITY, BENCHMARKS, both unit files, `ananicy.conf`.
  New keys since round 1: `bias_protect`, `bias_group.build`; new unit:
  `aranicy-collector.service` owns all BPF; identity is now
  `basename(/proc/<pid>/exe)`.
- `pacman -Q | grep -i aranicy`, `systemctl list-units` → nothing installed.

## 1. Naive pass (09:07)

```
sudo pacman -U --noconfirm aranicy-collector-... aranicy-...
sudo systemctl enable --now aranicy      # rc=0
systemctl status aranicy                # active (running), Main PID 1191
aranicy lint                            # OK: 0 rules, 0 types, 0 cgroups
```
Surprise: `aranicy lint` is happy with zero rules; the daemon warns about it at
startup instead ("no rules loaded ... with an empty rule set only the learned
bias changes priorities"). `ls /run/aranicy/` as uid 1000 → Permission denied
(the runtime dir is 0700). `aranicy dump *` works unprivileged.

## 2. Retest dana-1 (09:08)

`pid1.rules` = `{"name": "systemd", "nice": 19}`, reload, `dump rules` shows it
loaded, 70 s of `awk '{print $19}' /proc/1/stat` → **0 at every sample**.
Round 1 saw 19 at t=60 s. Gone. Noted the journal is silent either way.

## 3. Retest dana-2, part 1 (09:08–09:13)

Emptied `bias_protect` (weakest documented setting), restarted, started a
background poller (`~/poll1.sh` → `~/obs1.log`, 5 s × 48). 240 s, all zero for
PID 1, kthreadd and ksoftirqd/0. While that ran I did the identity test.

Dead end: wrote `~/poll1.sh` with a heredoc inside the ssh single-quoted
command; first attempt died on quoting, second (quoted `"EOF"`) worked.

## 4. Retest dana-3 (09:09)

`{"name":"skeptictest","nice":12}`; three cases. A: real binary, `exec -a
divider` → **nice 12** (was 0). B: unrelated binary, `exec -a skeptictest` →
**nice 0** (was 12). C: control → 12. The argv[0] vector is closed.

## 5. New: rule borrowing through the filename (09:09)

`cp /bin/sleep ~/skepticrt` + `{"name":"skepticrt","sched":"fifo","rtprio":10,"nice":-20}`
→ running as uid 1000: `nice=-20 rtprio=10 policy=1`, `chrt` says SCHED_FIFO 10,
`Uid: 1000`. Filed `dana-7`.

## 6. Retest dana-4 (09:09–09:13)

Spinner (`~/skepticcpu` + spin.py) and 10 ms sleeper (`~/skepticlat` + lat.py),
no rules. 200 s: spinner `bulk bias 3 score 2.8 nice 3`; sleeper `bulk bias 0
score 901 nice 0`. `bash` started at bias 0 and then stepped +1, +2 a few
minutes later; a child `bash -c` exec'd with **nice 2**. `threshold_ema` 1070.
Class label is `bulk` for everything I looked at, including PID 1 and
dbus-broker.

Side observation: journal warns `observation budget truncated the sample window
truncated=1025 rows=8192` during the storm — the documented 8192 cap does bite on
this box during a 3 k/s storm; not filed (it is documented and warned).

## 7. Kernel threads (09:13–09:16)

Rules named `ksoftirqd/0` and `kworker/0:0-events`, 140 s of `/proc/15,9,2/stat`
→ no change. `dump bias` shows no kthread-ish entries. Blast radius clean here.

## 8. Retest dana-5 (09:16)

60 000 × `/bin/true`, utime+stime of both pids: `rate=2858/s daemon=2.33%
coll=6.38% sum=8.72%`; idle `daemon=0.00% coll=0.50%`; two workers 30 000 each:
`rate=3786/s daemon=3.22% coll=10.54% sum=13.76%`. Still over 5 %, better per
exec than round 1. `perf` is not installed (`perf: command not found`), so no
instructions/event.

## 9. Retest dana-6 (09:16)

`pacman -Ql` → both docs shipped. `readlink -f /usr/share/doc/aranicy/docs/BENCHMARKS.md`
→ MISSING (the README links to a `docs/` subdir that the package does not
create). `sched_ext` still `disabled`, no `scx*` unit, learning still happens →
the prose fix is real. Filed the link breakage as `dana-9`.

## 10. Hostile: no peer service (09:17)

`systemctl mask --now aranicy-collector` then `systemctl restart aranicy` →
`Failed to restart aranicy.service: Unit aranicy-collector.service is masked.`,
rc=1, aranicy stays inactive. Loud, as documented. Later, `kill -9` on the
collector instead: systemd deactivated the daemon too (`Requires=`) and
restarted the pair ~1 s later; a ruled process exec'd inside that gap got the
inherited nice, not the rule. Not filed: this is systemd doing what the unit
asks, and the outage is ~1 s and visible in `systemctl status`.

## 11. Retest dana-2, part 2 — persisted bias (09:18)

Stopped the daemon, patched `/var/lib/aranicy/bias-state` so `systemd` had
`bias: 3, confidence 0.95, frozen false`, `renice 0 -p 1`, started, polled 70 s
→ **0 throughout** with `bias: restored history entries=71` in the journal.
Restored the pre-tamper file afterwards. Gone.

## 12. New: silent periodic re-apply (09:19, 09:29)

`sshd` rule → live sshd 0 → **19 at t=60 s**, no log line. Repeated cleanly with
my own `~/skepticsleep` started *before* the rule existed: 2 → 15 at t=60 s,
`journalctl --since` grep count 0. Filed `dana-8`.

## 13. Hostile: junk rule file (09:20)

First attempt used a bare heredoc into `/etc/ananicy.d/junk.rules` → `Permission
denied` (uid 1000). Redone with `sudo tee`. `aranicy lint` produced eight
line-numbered warnings (nice 999 out of range, no JSON span ×2, invalid sched,
empty name, invalid regex with a caret diagram, nice as string) and `rc=1`;
`dump rules` keeps only the parsable rules. The daemon logs the same warnings at
startup and at `--reload`. This is a genuine improvement over round 1 and is not
filed.

## 14. Hostile: half-written config (09:21)

Truncated the config at 700 bytes and appended `check_freq =`, `loglevel =
loud`, `bogus_key = 1`, a duplicate `check_freq = 45`. `aranicy lint` warned on
`loglevel` and `bogus_key` by line; the daemon warned identically at startup and
kept running. No refusal. Note for the reader: truncation silently drops the
later keys (including `bias_protect`) back to defaults — I did not treat that as
a bug since the format is line-based, but it is the failure mode to know about.
Restored the shipped config from the package afterwards.

## 15. New feature: `bias_group.build` (09:22–09:25)

No compiler on the box (`/usr/bin/gcc`, `/usr/bin/g++`: not found), so I
simulated the family with impostor comms: `cp /usr/bin/python3 ~/cc1plus` (spin)
and `~/lto1-ltrans` (10 ms sleep). After 170 s: one pooled entry
`build ... bias 3, score 466, samples 229`, and **both** processes at nice 3 —
including the one that only sleeps. Mechanism works, pools by comm not
behaviour.

## 16. Dead end: bias_protect dodge (09:22–09:28)

I also started `~/skepticren` which calls `prctl(PR_SET_NAME,"systemd-fake")`,
expecting the `systemd` prefix to exempt it from the learned axis and prove a
dodge. It stayed at bias 0 — but so did the **control**, `~/skepticren2`, an
identical 10 ms sleeper with an ordinary comm, same class `bulk`, similar score
(840 vs 916), over the same window. So the dodge is not demonstrated and nothing
was filed. Worth recording: neither sleeper was attacked in that window while a
real spinner was, so the attack gate is not the class label alone and I did not
invert it.

Incidental: `dump proc` reports `skepticren` for that process (exe basename), so
the `ps` comm and the daemon's idea of the name now disagree — that is the fix
working as documented.

## 17. SIGTERM mid-operation (09:29)

40 000-exec storm in the background, `systemctl stop aranicy` at t=3 s →
`Deactivated successfully` in **30 ms**, collector log `exec-event client
disconnected ... dropped=0`, no leftover daemon or collector processes, collector
unit still active (separate unit, correct).

## 18. Cleanup (09:30)

Removed every test rule file, restored `ananicy.conf` from the **package**
(`bias_protect = systemd dbus-broker dbus-daemon sshd gdm sddm polkitd elogind`;
my first restore had copied the already-emptied file back — caught it by
grepping the key), killed the leftover test processes, `aranicy lint` → 0 rules,
`/proc/1` nice = 0, daemon nice −5, collector nice −10.

## Environmental notes

- `strace`, `perf`, `gcc`/`g++` are all absent from this box; the brief forbids
  installing third-party software, so syscall-level and instruction-level
  evidence is unavailable this round. Round 1 had gcc, this one does not — an
  environment difference, not a finding.
- Two of my measurements (dana-4, dana-5) ran under a CPU spinner saturating one
  of two cores, as in round 1; the CPU-time ratios are unaffected but wall-clock
  is.

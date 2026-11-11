# Round 3, part A — adversarial pass

Machine: `su-gamer-r3`, Arch Linux rolling, kernel `7.2.11-arch1-1`, 2 vCPU,
1958 MiB, no display. Package under test: `aranicy 0.3.0-1` +
`aranicy-collector 0.3.0-1` from `~/payload` (the current build).
Everything below was run through `./machine '…'`; outputs are pasted verbatim.

The two builds are the same version number, built 53 minutes apart
(`builddate 1794390742` vs `1794387575`), so "which build am I on" is not
something the CLI can tell you — I got it from `pacman -Qip` and from the file
layout (see `differential.md`).

## Summary of the twelve attacks

| # | attack | silent? | leaves the machine worse? | issue |
|---|---|---|---|---|
| 1 | rule file full of junk | no — one WARN per bad line, rest of the corpus loads | no | — |
| 2 | empty config / empty corpus | no (current build warns; daemon warns at startup) | no | — |
| 3 | half-written config line | no — one WARN per line | no | — |
| 4 | config with an unknown key | no — WARN naming the key, daemon starts | no | — |
| 5 | **half-written rule file** | **one WARN about the line, none about the process** | **yes — the running game goes −7 → +1 → +3** | kaz-11 |
| 6 | **rule deleted** | **completely — not one line** | **yes — `reloaded N rules` is the only trace** | kaz-11 |
| 7 | run as a non-root user | no — clear errors | no | (kaz-10 recurs) |
| 8 | other service stopped / masked | no — refuses to start, loudly | collector left running after uninstall | kaz-13 |
| 9 | `kill -9` the collector | no — 6 s, then everything reconnects | daemon restarts too (undocumented) | — |
| 10 | reboot mid-work | no | no — state comes back | — |
| 11 | install / remove / reinstall | **yes — the old binary keeps running** | yes, until a manual restart | kaz-12, kaz-13 |
| 12 | self-renaming process / `exec -a` / exec into another binary | — | — | (see below) |

---

## 1–4. Broken config and junk corpus

I wrote every shape of broken file I could think of into a scratch config dir
and asked the tool itself (`lint`) before touching the live daemon.

```
$ ./machine 'mkdir -p ~/adv; cd ~/adv; rm -rf *;
mk() { d=$1; shift; mkdir -p $d; printf "%s\n" "$@" > $d/ananicy.conf; }
mk empty; mk half1 "check_freq ="; mk half2 "bias_window_s = abc"; mk half3 " = 5"; mk half4 "check_freq";
mkdir -p unknown; printf "check_freq = 60\napply_nice = maybe\nbias_max = -99\n" > unknown/ananicy.conf
for d in empty half1 half2 half3 half4 unknown; do echo "##### $d"; aranicy --config-dir ~/adv/$d lint; echo "EXIT=$?"; done'
##### empty
warning: 0 rules loaded from /home/arch/adv/empty: this corpus will apply nothing
lint: warnings found
EXIT=1
##### half1
warning: config: line 1: invalid check_freq (u64 seconds) value ""
warning: 0 rules loaded from /home/arch/adv/half1: this corpus will apply nothing
lint: warnings found
EXIT=1
##### half2
warning: config: line 1: invalid bias_window_s (u64) value "abc"
warning: 0 rules loaded from /home/arch/adv/half2: this corpus will apply nothing
lint: warnings found
EXIT=1
##### half3
warning: config: line 1: unknown key ""
warning: 0 rules loaded from /home/arch/adv/half3: this corpus will apply nothing
lint: warnings found
EXIT=1
##### half4
warning: config: line 1: no '=' in "check_freq"
warning: 0 rules loaded from /home/arch/adv/half4: this corpus will apply nothing
lint: warnings found
EXIT=1
##### unknown
warning: config: line 2: invalid boolean "maybe"
warning: config: bias_min (-1) > bias_max (-99); using defaults
warning: 0 rules loaded from /home/arch/adv/unknown: this corpus will apply nothing
lint: warnings found
EXIT=1
```

Four different half-written shapes, one wrong type, one impossible range — all
caught, none fatal, `EXIT=1` so a script notices. Good.

Same for a config with a key that does not exist, at the level that matters
(the live daemon, not the linter):

```
$ ./machine 'echo "bias_totally_bogus = 7" | sudo tee -a /etc/ananicy.d/ananicy.conf >/dev/null; sudo systemctl restart aranicy; sleep 5; systemctl is-active aranicy; sudo journalctl -u aranicy --no-pager --since "-15s" | grep -E "bogus" | sed "s/.*aranicy\[[0-9]*\]: //"'
active
2026-11-11T10:29:49.391407Z  WARN config: line 45: unknown key "bias_totally_bogus"
```

The daemon runs with its defaults and says which key it ignored.

Now the junk *corpus* — a file with plain text, an unterminated object, an
object followed by words (which the contract says is allowed and ignored), a
binary line, a line with two objects, and a 100 KB line:

```
$ ./machine 'sudo tee /etc/ananicy.d/91-junk.rules >/dev/null <<EOF
this is not json at all
{"name": "halfwritten", "nice": -3
{"name": "kaznull", "nice": 5} die here
EOF
… then appended a line of non-UTF-8 bytes, a line of NUL/control bytes,
  and a 100 KB line that is a valid object followed by 100 000 'x';
  and created a directory named 92-dir.rules containing i.rules'
```

(the heredoc above is the readable part; the three appended lines cannot be
pasted here, so here is the file as the daemon saw it, which is the evidence
that matters. Lines 4 and 6 are binary and are marked, not transcribed; the
pipeline cuts every line at 80 columns, so line 7 is 100 KB of `x` in the file
and 40 `x` here.)

```
$ ./machine 'sudo nl -ba /etc/ananicy.d/91-junk.rules | cut -c1-80 | head -8'
     1	this is not json at all
     2	{"name": "halfwritten", "nice": -3
     3	{"name": "kaznull", "nice": 5} die here
     4	[non-UTF-8 bytes] PNG garbage bytes
     5	{"name": "kaztwo", "nice": 2} {"name": "kaztwo", "nice": 9}
     6	[NUL/control bytes] binary
     7	{"name": "kazlong", "nice": 1}xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx
```

```
$ ./machine 'sudo journalctl -u aranicy --no-pager --since "-1min" | tail -6'
2026-11-11T09:57:10.172380Z  WARN /etc/ananicy.d/91-junk.rules:1: no JSON object span (missing '{' or '}')
2026-11-11T09:57:10.172394Z  WARN /etc/ananicy.d/91-junk.rules:2: no JSON object span (missing '{' or '}')
2026-11-11T09:57:10.172396Z  WARN /etc/ananicy.d/91-junk.rules:4: no JSON object span (missing '{' or '}')
2026-11-11T09:57:10.172397Z  WARN /etc/ananicy.d/91-junk.rules:5: invalid JSON in span: trailing characters at line 1 column 31
2026-11-11T09:57:10.172398Z  WARN /etc/ananicy.d/91-junk.rules:6: no JSON object span (missing '{' or '}')
2026-11-11T09:57:10.172619Z  INFO reloaded 4 rules; bias reconfigured
$ ./machine 'aranicy dump rules | python3 -c "import json,sys; d=json.load(sys.stdin); print([ (r[\"name\"], r.get(\"nice\")) for r in d[\"exact\"]])"'
[('kazdiff', -7), ('kazindir', 4), ('kazlong', 1), ('kaznull', 5)]
```

One bad file does not take the corpus down: four rules survived, including one
inside a *directory* named `*.rules` (recursive scan works) and one on a 100 KB
line. Line 3 is the "trailing garbage" case the contract allows, so no warning
there is correct.

Verdict: 1–4 are well behaved. Nothing silent, nothing broken.

## 5–6. The rule file the user edits — this is the one that bites

These are the two cases from the brief that go wrong, and they are the ones a
user will actually produce. Full write-up in **kaz-11**; the short form:

```
$ ./machine '… setsid nohup /usr/local/bin/kzdel speed -seconds 600 rsa2048 … ; sleep 150; echo "t=150 kzdel(ni)=$(ps -o ni= -C kzdel|tr -d " ")"; sudo rm /etc/ananicy.d/72-kzdel.rules; sudo systemctl reload aranicy; sleep 70; echo "t=224 kzdel(ni)=$(ps -o ni= -C kzdel|tr -d " ")"'
t=0 kzdel(ni)=-7 rule=-7
t=150 kzdel(ni)=-4 rule=-7  bias=bias=3
--- delete the rule file entirely, reload ---
2026-11-11T10:34:01.014804Z  INFO reloaded 15 rules; bias reconfigured
t=224 kzdel(ni)=3 (rule file deleted)
t=284 kzdel(ni)=3
--- any warning about the disappearance? ---
```

The truncation variant at least warns about the line:

```
{"name": "kazdiff", "nic
2026-11-11T09:57:24.041692Z  WARN /etc/ananicy.d/90-kazdiff.rules:1: no JSON object span (missing '{' or '}')
2026-11-11T09:57:24.041873Z  INFO reloaded 3 rules; bias reconfigured
…
    PID  NI     ELAPSED COMMAND
   1353   3       04:28 kazdiff
```

Note the intermediate: even with the rule intact, the process sat at **−4** while
its file said −7, because the learned bias had reached +3 and is added to the
rule. I did not file that as a separate issue — the composition is documented
(`clamp(rule_nice + bias, …)`) and kaz-5/kaz-6 already cover "the number is not
the reason" and "protect by executable name". But a user reading `ps` sees two
numbers that do not match and has nothing to ask.

## 7. As a non-root user

```
$ ./machine 'whoami; aranicy start; echo "EXIT=$?"; aranicy --reload; echo "EXIT=$?"; timeout 5 aranicy-collectord run 2>&1 | head -4; echo "EXIT=${PIPESTATUS[0]}"; aranicy dump types; echo "EXIT=$?"'
arch
Error: aranicy start requires root (effective uid 1000); run it via sudo or the aranicy.service unit
EXIT=1
Error: failed to signal aranicy (pid 1244): Operation not permitted (os error 1)
EXIT=1
libbpf: Failed to bump RLIMIT_MEMLOCK (err = -EPERM), you might need to do it explicitly!
libbpf: Error in bpf_object__probe_loading(): -EPERM. Couldn't load trivial BPF program. Make sure your kernel supports BPF (CONFIG_BPF_SYSCALL=y) and/or that RLIMIT_MEMLOCK is set to big enough value.
Error: attaching collector BPF programs
EXIT=1
{}
EXIT=0
```

The daemon's own refusals are perfect. The collector's are a kernel/libbpf wall
of text that reads as "this kernel is broken" — the same defect as kaz-10 last
round, unchanged in both builds (see `differential.md`).

## 8. The other service stopped, then masked

```
$ ./machine 'sudo systemctl stop aranicy-collector; sleep 3; systemctl is-active aranicy aranicy-collector'
inactive
inactive
$ ./machine 'sudo systemctl mask aranicy-collector; sudo systemctl start aranicy; echo "EXIT=$?"; sleep 8; systemctl is-active aranicy aranicy-collector'
Created symlink '/etc/systemd/system/aranicy-collector.service' → '/dev/null'.
Failed to start aranicy.service: Unit aranicy-collector.service is masked.
EXIT=1
inactive
inactive
```

Loud and correct: stopping the collector stops the daemon (it is `Requires=`),
and a masked collector prevents the start with systemd's own message. This is
the behaviour `COMPATIBILITY.md` promises ("a missing collector is a loud
failure, never silent rule-less operation") and I could not break it.

The uninstall path is the one that leaks — see kaz-13.

## 9. `kill -9` on the paired service while it is working

Twice, on both builds:

```
$ ./machine 'CP=$(systemctl show -p MainPID --value aranicy-collector); DP=$(systemctl show -p MainPID --value aranicy); echo "collector=$CP daemon=$DP"; K0=$(date +%s); sudo kill -9 $CP; for i in $(seq 30); do sleep 1; if [ "$(systemctl is-active aranicy-collector)" = active ] && [ "$(systemctl show -p MainPID --value aranicy-collector)" != "$CP" ]; then break; fi; done; echo "collector back after $(( $(date +%s) - K0 ))s pid=$(systemctl show -p MainPID --value aranicy-collector)"; echo "daemon pid now=$(systemctl show -p MainPID --value aranicy) was=$DP"; sleep 6; ( exec -a steam /usr/local/bin/kazexec speed -seconds 15 rsa2048 >/dev/null 2>&1 </dev/null & ); sleep 3; echo "rule after collector crash: $(ps -o ni= -C kazexec | tr -d " ")"'
collector=328 daemon=656
collector back after 6s pid=875
daemon pid now=876 was=656
rule after collector crash: -2
```

- The collector is back in 6 s (`RestartSec=5`), the sockets are re-created and
  the daemon reconnects (`exec-event client connected` in the collector log,
  `stats sampler live` in the daemon's).
- **The daemon restarts as well** (pid 656 → 876). Nothing in either unit file
  says that will happen; it is the `Requires=` propagating a stop. Consequence:
  a collector crash costs the daemon's in-memory state and a fresh window, not
  just a stats gap. It recovered every time (3 for 3), so I am reporting it as
  behaviour, not a bug — but "kill the stats collector and the policy daemon
  restarts" is not obvious from the README.
- Rules still apply to new processes afterwards (`-2` = rule −5 with a learned
  +3 on that name).

## 10. Reboot in the middle of the work

I rebooted with both services enabled, a ruled workload running and a bias
history. (Ten seconds before the reboot `aranicy dump bias` still said
`"state": null` — the on-disk snapshot is debounced — and after the reboot the
daemon announced `restored history entries=27`, so something wrote the state on
the way down. The new daemon cannot have learned 27 entries in the two seconds
between boot and that log line.) Current build:

```
$ ./machine '… sudo systemctl reboot'
$ ./machine 'uptime; systemctl is-active aranicy aranicy-collector; systemctl is-enabled aranicy aranicy-collector; sudo journalctl -b -u aranicy -u aranicy-collector --no-pager | tail -18'
 10:04:48 up 1 min,  1 user,  load average: 0.29, 0.12, 0.04
active
active
enabled
disabled
…
Nov 11 10:03:52 su-gamer-r3 aranicy[329]: 2026-11-11T10:03:52.202789Z  INFO bias: restored history entries=27
Nov 11 10:03:52 su-gamer-r3 aranicy[329]: 2026-11-11T10:03:52.204508Z  INFO stats sampler live prev=degraded: starting detail=bound SchedSample via sched_samples at /run/aranicy/collector-stats.sock
Nov 11 10:03:52 su-gamer-r3 aranicy-collectord[328]: 2026-11-11T10:03:52.207163Z  INFO exec-event client connected
```

Post-reboot, a newly started process still got its rule:

```
$ ./machine '( exec -a steam /usr/local/bin/kazexec speed -seconds 45 rsa2048 >/dev/null 2>&1 </dev/null & ); sleep 3; ps -o pid,ni,comm,args -C kazexec | cut -c1-70; aranicy dump bias | python3 -c "import json,sys; s=json.load(sys.stdin)[\"state\"]; print(\"bias entries:\", len(s[\"entries\"]) if s else None)"'
    PID  NI COMMAND         COMMAND
    508  -5 kazexec         steam speed -seconds 45 rsa2048
bias entries: 66
```

The previous build behaved the same (`restored history entries=61`, services
`active`, rule applied). Reboot is clean on both. Nothing to file.

## 11. Install, remove, reinstall

Three separate faults, two of which are filed:

**(a) installing does not install it.** Full write-up in kaz-12. The daemon and
collector keep running from deleted inodes, `systemctl` says `active`, the CLI
reports the new file's version, and nothing is marked for restart:

```
reinstalling aranicy...
--- immediately after ---
ActiveState=active
MainPID=748
exe -> /usr/bin/aranicy (deleted)
```

**(b) removing half of it leaves the other half running.** kaz-13:

```
removing aranicy...
--- after ---
inactive      <- aranicy
active        <- aranicy-collector
not-found
disabled
    PID     ELAPSED COMMAND
   1712       01:15 aranicy-collect
```

**(c) the previous build keeps the learned history through an uninstall.** This
one is a difference between the builds, not a bug in the current one — recorded
in `differential.md` (check 19).

## 12. Renaming, identity, disappearing workloads

All three ways a process can lie about itself, plus churn:

```
$ ./machine '( exec -a steam /usr/local/bin/kazexec speed -seconds 60 rsa2048 >/dev/null 2>&1 </dev/null & ); sleep 3; ps -o pid,ni,comm,args -C kazexec | cut -c1-90'
    PID  NI COMMAND         COMMAND
   1788  -5 kazexec         steam speed -seconds 60 rsa2048
```

`exec -a` sets `argv[0]` and leaves `/proc/<pid>/exe` alone → the rule matches
the executable, which is what `COMPATIBILITY.md` promises ("the executable is
the only one … the task cannot rewrite"). Good.

```
$ ./machine 'setsid nohup python3 ~/adv/ren.py kazren > ~/adv/ren.out 2>&1 </dev/null & sleep 3; ps -o pid,ni,comm,args -C kazren | cut -c1-80; aranicy match-test kazren | head -6'
    PID  NI COMMAND         COMMAND
    801   0 kazren          python3 /home/arch/adv/ren.py kazren
{
  "match": "exact",
  "name": "kazren",
  "policy": {
    "nice": -8
  }
```

A process that `prctl(PR_SET_NAME)`s itself to a ruled name gets **nothing**
(ni 0) — by design, and documented. The same name/executable split is kaz-6 from
last round; I am not re-filing it.

```
$ ./machine 'setsid nohup python3 ~/adv/into.py /usr/local/bin/kazex2 >/dev/null 2>&1 </dev/null & sleep 4; ps -o pid,ni,comm,args -C kazex2 | cut -c1-60'
    PID  NI COMMAND         COMMAND
    789  -5 kazex2          /usr/local/bin/kazex2 30
```

A process that `exec`s into a different binary (same pid, new identity) is
re-evaluated and gets the new rule. Correct.

**Short-lived / disappearing workloads.** A ruled binary that exits in tens of
milliseconds, 300 times in a row, asked itself what nice it got:

```
$ ./machine 'for i in $(seq 300); do ~/adv/selfnice.py; done | sort -n | uniq -c; ps -o pid,pcpu,etime,comm -p $(systemctl show -p MainPID --value aranicy) $(systemctl show -p MainPID --value aranicy-collector)'
    300 -6
    PID %CPU     ELAPSED COMMAND
   1712  0.1       01:01 aranicy-collect
   1713  0.0       01:01 aranicy
```

300/300 at −6, daemon and collector at 0.1 %/0.0 % of a core. Nothing missed,
nothing left behind. (This is also my best proxy for "a pid that gets
recycled": 300 sequential pids recycled inside one window, no phantom entries
and no wrong names in `dump bias`.)

**The trust model, verified.** The current build's `COMPATIBILITY.md` now warns
that any user can borrow a rule by copying a binary to a rule name. It is true,
and it is worse than "nice −10" — an unprivileged user also gets RT scheduling:

```
$ ./machine 'cp /usr/bin/sleep ~/adv/kazborrow; cp /usr/bin/sleep ~/adv/kazfifo; setsid nohup ~/adv/kazborrow 20 >/dev/null 2>&1 </dev/null & setsid nohup ~/adv/kazfifo 20 >/dev/null 2>&1 </dev/null & sleep 3; ps -o pid,user,ni,comm -C kazborrow,kazfifo; F=$(pgrep -x kazfifo|tail -1); chrt -p $F'
    PID USER      NI COMMAND
    830 arch     -10 kazborrow
    831 arch       - kazfifo
pid 831's current scheduling policy: SCHED_FIFO
pid 831's current scheduling priority: 50
```

Documented in the current build, *not* documented in the previous one — that is
the single most valuable change between the two builds (see `differential.md`,
check 21). Not filed as a bug, because it is now written down and the mitigation
is stated.

## What I could not break

- Empty / malformed / unknown-key config: caught, warned, non-fatal.
- Masked or stopped collector: loud refusal.
- `kill -9` on either service: both come back, 6 s, everything reconnects.
- Reboot: services, sockets, history and per-process policy all return.
- 300 short-lived ruled processes: 300/300 correct, daemon idle.
- Session safety: `systemd` (pid 1), `sshd` and `dbus-broker` stayed at nice 0
  through all of it:
```
$ ./machine 'ps -o pid,ni,user,comm -C sshd,systemd,dbus-broker'
    PID  NI USER     COMMAND
      1   0 root     systemd
    330   0 dbus     dbus-broker
    445   0 root     sshd
    472   0 arch     systemd
```

## What surprised me

- That the two failure modes I care about most (a rule saved half-way, a rule
  deleted) are the two the daemon *does not* report at the process level. Every
  other malformed input is handled beautifully; the one that changes a running
  game's priority is the quiet one.
- That `kill -9` on the stats collector takes the daemon down with it.
- That the new build documents exactly the privilege-escape I found, in the
  place I would look for it — the trust-model section is new and it is accurate.

## Dead ends (kept so nobody repeats them)

- In my first slow-run script the truncation never happened: `sudo head -c 22 f >
  ~/r.tmp` wrote a root-owned file the user shell could not read back, `cp`
  failed silently, and the "truncated" rule was in fact the whole rule. The run
  was still useful — it is the clean measurement of rule −7 + learned +3 = −4 —
  but the case I thought I was testing was not run. Second script used
  `sudo sh -c 'head -c 22 f > f.tmp && mv f.tmp f'` and worked.
- `ps -C python3` found nothing after `prctl(PR_SET_NAME)`: `-C` matches comm,
  which had just been rewritten. `ps -C kazren` was the right query.
- A reboot-and-poll loop looked hung for 5 minutes; the machine had actually
  come back in 50 s and the loop was stuck in a later long command. Polling with
  a bounded timeout is the fix.

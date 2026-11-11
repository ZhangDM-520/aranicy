# Round 2 retest — dana

Fresh machine `su-skeptic-r2`, kernel `7.2.11-arch1-1`, 2 vCPU, 1958 MiB,
`/sys/kernel/sched_ext/state` = `disabled`, CLK_TCK = 100.

Both packages are still versioned `0.3.0-1`, but the contents differ from
round 1: `/usr/share/doc/aranicy/` now ships `BENCHMARKS.md` and
`COMPATIBILITY.md` (round 1: neither existed), and the config now carries
`bias_protect` and `bias_group.build` keys that round 1 had no knowledge of.
`builddate = 1794387575` in both `.PKGINFO` files.

```
$ sudo pacman -U --noconfirm aranicy-collector-0.3.0-1-x86_64.pkg.tar.zst aranicy-0.3.0-1-x86_64.pkg.tar.zst
...
installing aranicy-collector...
installing aranicy...
$ sudo systemctl enable --now aranicy; systemctl status aranicy --no-pager | head -6
     Active: active (running) since Wed 2026-11-11 09:07:19 UTC; 3s ago
   Main PID: 1191 (aranicy)
$ aranicy lint
OK: 0 rules, 0 types, 0 cgroups
```

---

## dana-1 — "A rule named `systemd` is applied to PID 1 by the periodic pass"

**gone.**

Rule loaded and reloaded, PID 1 polled every 5 s for 70 s (the round-1
observation point was the 60 s drift pass):

```
$ echo '{"name": "systemd", "nice": 19}' | sudo tee /etc/ananicy.d/pid1.rules; sudo aranicy --reload; aranicy dump rules; echo "pid1_before=$(awk '{print $19}' /proc/1/stat)"; for i in $(seq 1 14); do sleep 5; echo "t=$((i*5))s pid1=$(awk '{print $19}' /proc/1/stat)"; done
{"name": "systemd", "nice": 19}
sent SIGHUP to aranicy (pid 1191)
{
  "exact": [
    {
      "match": "exact",
      "name": "systemd",
      "nice": 19
    }
  ],
  "regex": []
}
pid1_before=0
t=5s pid1=0
t=10s pid1=0
...
t=55s pid1=0
t=60s pid1=0
t=65s pid1=0
t=70s pid1=0
```

The rule is accepted and loaded (`dump rules` shows it), so this is a refusal to
apply, not a refusal to load. The same mechanism still fires for every other
name — see dana-8 — so PID 1 is special-cased on the path that previously moved
it. The config now says so (`ananicy.conf`: "PID 1 is always protected, list or
no list"), which it did not in round 1.

---

## dana-2 — "The learned-bias engine classified PID 1 as `Bulk`, stepped it, persisted it, re-applies at every start"

**gone**, on both the learning path and the apply path.

(a) Learning path: `bias_protect` emptied (the weakest documented setting),
daemon restarted, 240 s observation, 48 samples at 5 s:

```
$ sudo sed -i 's/^bias_protect = .*/bias_protect =/' /etc/ananicy.d/ananicy.conf; grep -n '^bias_protect' /etc/ananicy.d/ananicy.conf
44:bias_protect =
$ tail -3 ~/obs1.log
t=230s pid1=0 kthreadd=0 ksoftirqd0=0
t=235s pid1=0 kthreadd=0 ksoftirqd0=0
t=240s pid1=0 kthreadd=0 ksoftirqd0=0
$ wc -l ~/obs1.log
48 /home/arch/obs1.log
```

PID 1 *is* still sampled and classified — that has not changed — but no step is
taken against it:

```
$ aranicy dump bias | ...
{'name': 'systemd', 'class': 'bulk', 'bias': 0, 'confidence': 0.8992079883099807, 'score_ema': 658.9774954701819, 'sample_count_ewma': 10.202860081944689}
```

(b) Apply path: I edited the persisted history directly (bias 3 for `systemd`,
confidence 0.95, not frozen), restarted, and polled 70 s:

```
# (python3 one-liner: load /var/lib/aranicy/bias-state, set the "systemd" entry to bias=3,
#  confidence=0.95, frozen=False, write it back)
patched: {'name': 'systemd', 'score_ema': 658.9774954701819, 'sample_count_ewma': 10.202860081944689, 'class': 'bulk', 'bias': 3, 'last_seen': 1794388690, 'confidence': 0.95, 'frozen': False}
$ sudo renice -n 0 -p 1
1 (process ID) old priority 0, new priority 0
pid1 before start=0
$ sudo systemctl start aranicy; sudo journalctl -u aranicy --no-pager -n 5 | grep restored
Nov 11 09:18:14 su-skeptic-r2 aranicy[122113]: 2026-11-11T09:18:14.050538Z  INFO bias: restored history entries=71
t=5s pid1=0
...
t=70s pid1=0
```

So a restored entry of bias 3 for the name `systemd` is loaded and not applied to
PID 1. Round 1 re-applied it at the first pass.

---

## dana-3 — "Rules are matched on argv[0] ... anyone can take a rule's priority, or dodge its own rule"

**gone** for the argv[0] vector that the report demonstrated. **different** as a
privilege question — the escalation survives through a second door, newly filed
as `issues/dana-7-rule-borrow-by-filename.md`.

Same rule and same two binaries as round 1 (`/etc/ananicy.d/t.rules` =
`{"name": "skeptictest", "nice": 12}`):

```
$ cp /bin/sleep ~/skeptictest; cp /bin/sleep ~/innocent; echo "{\"name\": \"skeptictest\", \"nice\": 12}" | sudo tee /etc/ananicy.d/t.rules >/dev/null; sudo aranicy --reload; sleep 1; aranicy lint; ... setsid --fork bash -c "exec -a divider /home/arch/skeptictest 20" ... ; setsid --fork bash -c "exec -a skeptictest /home/arch/innocent 20" ... ; setsid --fork /home/arch/skeptictest 20 ...
--- A: real skeptictest, argv0=divider ---
pid=1330 comm=skeptictest nice=12 exe=/home/arch/skeptictest cmdline=[divider 20 ]
--- B: real innocent, argv0=skeptictest ---
pid=1343 comm=innocent nice=0 exe=/home/arch/innocent cmdline=[skeptictest 20 ]
--- C: plain skeptical control (real skeptictest, real name) ---
pid=1330 nice=12
pid=1352 nice=12
```

Both directions are now inverted from round 1: the real binary keeps its rule
when argv[0] is spoofed (case A: nice 12; round 1: nice 0), and a spoofed argv[0]
no longer attracts it (case B: nice 0; round 1: nice 12). This matches the new
`COMPATIBILITY.md` text ("The executable is the only one of the three the task
cannot rewrite").

What the fix does *not* do is bind a rule to a file. See dana-7: I still got
`nice -20` + `SCHED_FIFO` on my own code from an unprivileged account, by giving
the file the rule's name.

---

## dana-4 — "The learned classifier gives every workload the same verdict — `Bulk`, bias +3"

**different.** The actuation now separates the workloads; the class *label* still
does not.

Round-1 protocol (no rules, copies of python3 with distinct comms), 200 s:

```
$ aranicy dump bias | ...
{'name': 'skepticcpu', 'class': 'bulk', 'bias': 3, 'confidence': 0.9375, 'score_ema': 2.829344146940281, 'sample_count_ewma': 115.7183121203637}
{'name': 'skepticlat', 'class': 'bulk', 'bias': 0, 'confidence': 0.875, 'score_ema': 901.324757172773, 'sample_count_ewma': 115.7183121203637}
{'name': 'bash', 'class': 'bulk', 'bias': 0, 'confidence': 0.4518415628459717, 'score_ema': 575.2605983823573, 'sample_count_ewma': 7.577274083731456}
$ ps -eo pid,ni,comm --no-headers | grep skepticcpu
   1428   3 skepticcpu
   1430   0 skepticlat
```

(The same dump's `threshold_ema` I only printed later, at 09:28, with a different
workload running: `T= 1160.7432144601105`. I am not mixing the two: the label
column is what the daemon reported with these scores.)

- CPU spinner (score ~3): `Bulk`, bias 3, live nice 3 — unchanged and correct.
- 10 ms sleeper (score ~900, same class `bulk`): bias 0, live nice 0. Round 1 it
  was `Bulk` +3 like everything else.
- Interactive shell: bias 0 during the window, but it then climbed:

```
Nov 11 09:16:58 su-skeptic-r2 aranicy[1291]: 2026-11-11T09:16:58.149937Z  INFO bias changed name=bash bias=1 prev=0 class=Bulk cause=Evidence
Nov 11 09:17:28 su-skeptic-r2 aranicy[1291]: 2026-11-11T09:17:28.150003Z  INFO bias changed name=bash bias=2 prev=1 class=Bulk cause=Evidence
$ bash -c 'echo child pid=$$ nice=$(awk "{print \$19}" /proc/$$/stat)'
child bash pid=122206 nice=2 comm=bash
```

So the thing a human waits on is still classified `bulk` and is still niced —
round 1's second complaint — but it now takes several minutes and the *sleeper*
is no longer swept along with the spinner. Every entry I inspected still carries
`class: bulk`, including PID 1 and `dbus-broker`; the label remains
undiscriminating, the damage from it is now bounded by something else (demand
gating, not the class).

New capability that round 1 could not test because there was no compiler on the
box then or now (`gcc`/`g++`: `command not found`; I did not install one):
`bias_group.build` pooling works and pools on comm, not behaviour —

```
$ cp /usr/bin/python3 ~/cc1plus; cp /usr/bin/python3 ~/lto1-ltrans
$ setsid --fork ~/cc1plus ~/spin.py ; setsid --fork ~/lto1-ltrans ~/lat.py      # spinner + 10 ms sleeper
$ aranicy dump bias | ...
{'name': 'build', 'class': 'bulk', 'bias': 3, 'confidence': 0.875, 'score_ema': 466.8355660092624, 'sample_count_ewma': 229.17317734107095}
$ ps -eo pid,ni,comm --no-headers | grep -E "cc1plus|lto1"
 122452   3 cc1plus
 122454   3 lto1-ltrans
```

One pooled `build` entry reached +3 and both members got nice 3, including the
member that was a 10 ms sleeper. That is the documented design (compile tools
hand off), demonstrated with impostor comms because no compiler is installed.

---

## dana-5 — "The shipped two-service system costs 9.6% of one core at 1 732 execs/s, about twice the documented ≤ 5%"

**still happens**, with a smaller constant.

Same method as round 1 (`utime+stime` from `/proc/<pid>/stat`, 100 % = one core),
one rule loaded:

```
$ getcpu() { awk '{print $14+$15}' /proc/$1/stat; }; a0=$(getcpu $dp); b0=$(getcpu $cp); s0=$(date +%s%N); for j in $(seq 1 60000); do /bin/true; done; s1=$(date +%s%N); a1=$(getcpu $dp); b1=$(getcpu $cp)
execs=60000 wall=21.00s rate=2858/s daemon=2.33% coll=6.38% sum=8.72% of one core
$ e0=$(getcpu $dp); f0=$(getcpu $cp); sleep 20; e1=$(getcpu $dp); f1=$(getcpu $cp)
idle daemon=0.00% coll=0.50%
$ (for j in $(seq 1 30000); do /bin/true; done) & (for j in $(seq 1 30000); do /bin/true; done) & wait
execs=60000 wall=15.85s rate=3786/s daemon=3.22% coll=10.54% sum=13.76% of one core
```

Per 1 000 execs/s of throughput the pair costs 3.05 % (round 1: 5.55 %), so the
build is materially cheaper per event; but the collector is still ~2.7× the
daemon and the pair is still above the README's "≤ 5% of one core at 2 000
execs/s" at every rate I could produce above ~1 000/s. Round 1: 9.61 % at
1 732/s. This round: 8.72 % at 2 858/s, 13.76 % at 3 786/s.

The documentation now *says* the split is expensive — `BENCHMARKS.md` has a
"v0.3.0 split baseline" section with 6.1 % + 6.2 % = 12.3 % wall at 2 000 execs/s,
and states the ≤ 5 % invariant "describe[s] the retired single-process
architecture". Yet `README.md` still opens with "a hard performance budget: **≤ 5%
of one core at 2 000 execs/s**" as the reason to adopt it. Newly filed as
`issues/dana-10-budget-split-mismatch.md`.

Caveat, unchanged from round 1: one sample per cell, 2 vCPU box, `/bin/true` is
the cheapest exec, and `perf` is not installed (`perf: command not found`) so I
cannot report instructions/event, which is the invariant the documentation
itself says to trust.

---

## dana-6 — "The shipped README points at files that are not shipped, and 'no per-task stats → no learning' is contradicted"

**partly gone.**

- Shipped docs: fixed. `pacman -Ql aranicy` now lists
  `/usr/share/doc/aranicy/BENCHMARKS.md` and `COMPATIBILITY.md` alongside the
  README.
- The sched_ext claim: fixed in prose. `COMPATIBILITY.md` now says the daemon
  "learns on *every* scheduler, including none (plain EEVDF)", and I confirmed
  learning with `/sys/kernel/sched_ext/state` = `disabled` and no `scx*` unit
  (`scx_lavd` bias changes at 09:23–09:24, `sched_ext` still `disabled`).
- The README's links are still broken on the installed system, just differently:
  they are now relative links to `docs/BENCHMARKS.md` from a directory that has
  no `docs/`. Newly filed as `issues/dana-9-readme-links-broken.md`.
- The capability sentence is still wrong: README says "aranicy itself then needs
  only `CAP_SYS_NICE` (no `CAP_BPF`)", the installed unit still grants
  `CAP_SYS_NICE CAP_SYS_RESOURCE CAP_DAC_OVERRIDE CAP_SYS_PTRACE`. Not refiled —
  it is the same defect as round 1's dana-6, unchanged.

```
$ ls -l /usr/share/doc/aranicy/docs
ls: cannot access '/usr/share/doc/aranicy/docs': No such file or directory
$ readlink -f /usr/share/doc/aranicy/docs/BENCHMARKS.md || echo MISSING
MISSING
$ cat /sys/kernel/sched_ext/state
disabled
$ systemctl list-units "scx*" --no-pager --no-legend
(no output)
```

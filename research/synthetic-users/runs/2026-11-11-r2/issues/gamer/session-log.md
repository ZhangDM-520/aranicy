# Session log — kaz, round 2

Untidied, chronological. `./machine '<cmd>'` is my ssh wrapper onto the box; every
`$` line below ran there. Timestamps are the box's (UTC).

## Survey

1. `./machine 'uname -r; nproc; free -m | head -2; ls -la ~/payload; pacman -Q aranicy aranicy-collector; systemctl is-enabled aranicy; systemctl is-active aranicy'`
   → `7.2.11-arch1-1`, 2 vCPU, 1958 MiB; payload has the two 0.3.0-1 packages;
   both "was not found"; unit `not-found`/`inactive`. Fresh box, as advertised.
2. `./machine 'cd ~/payload && sudo pacman -U --noconfirm aranicy-0.3.0-1-x86_64.pkg.tar.zst aranicy-collector-0.3.0-1-x86_64.pkg.tar.zst'`
   → EXIT=0; `after install: is-enabled=disabled is-active=inactive`.
   Surprise: `pacman -Qi aranicy` now says `Install Script  : Yes` (round 1: No),
   and the script still enables/starts nothing and prints nothing.
3. Docs: `ls -la /usr/share/doc/aranicy/ /usr/share/doc/aranicy-collector/`
   → daemon dir now has README + BENCHMARKS.md + COMPATIBILITY.md; collector has
   its own 2642-byte README. Round-1 complaint #4 half-fixed.
4. `cmp` the two READMEs → differ (good). `find /usr/share/man -iname '*aranicy*'`
   → nothing (still no man page). Relative links still dead (5 of 5).
5. Read both READMEs and all of COMPATIBILITY.md (18 KB, new). Learned:
   PID 1 never a target; `bias_protect`; a demand gate for demotion; `match-test`.

## Install / quick start

6. `sudo systemctl enable --now aranicy` → symlink created; `is-active` active;
   collector active too (`Requires=` pulled it). `aranicy lint` → `OK: 0 rules, 0
   types, 0 cgroups`. **The README quick start now works with no guessing.**
7. `cat /etc/ananicy.d/ananicy.conf` → bias section, `bias_group.build`,
   `bias_protect = systemd dbus-broker ...` documented in comments. New since
   round 1, where the only mention of bias was the pool regex.
8. `sudo journalctl -u aranicy` at startup → `WARN no rules loaded ...`, `WARN
   kernel does not support latency_nice`, `INFO stats sampler live`.
   Environment note, not a bug (2-vCPU cloud kernel).

## Rules, and the bias overriding them (retest of kaz-2)

9. Wrote `/etc/ananicy.d/50-kaz.rules` with `kazgame` -5 and `openssl` 19+idle,
   `aranicy lint` → OK 2 rules; `reload` → `INFO reloaded 2 rules; bias
   reconfigured`; `dump rules` shows both.
10. `cp /usr/bin/openssl /usr/local/bin/kazgame`; ran `kazgame speed -seconds
    420 rsa2048` → `ps` shows `-5`, journal `applied rule rule="kazgame"
    pid=1241 bias=0 route="exec"`. Exec path fine.
11. Tried a per-rule opt-out: `{"name":"kazgame","nice":-5,"bias":false}` in a
    scratch dir → `warning: ... unknown key "bias" ignored`, lint exit 1.
    Still no rule-level key. (dead end)
12. `python3 -c "time.sleep(6)"` → nice 0. Round 1 this was 3; the demand gate
    changed that. Good.
13. `dump bias` 40 s in → `"state": null`. Surprise: nothing on disk until the
    first debounced save, so the very first "why" question has no answer at all.
14. Waited. `kazgame` 09:08:01 bias 1, 09:08:31 bias 2 (+`applied ... bias=2
    route="drift"`), 09:09:01 bias 3 → `ps` shows `-3`. Round-1 complaint #2(a)
    reproduces exactly.
15. Added `kazgame` to `bias_protect`, `systemctl reload` (no restart), waited one
    drift pass → `ps` shows `-5` again. **This is the thing I asked for last
    round and it works.** Round-1 #2(c) resolved.
16. But `dump bias` still holds `kazgame bias 3` while it is protected, and
    `last_seen` keeps advancing — so the bias keeps being learned, it is only not
    applied. Removing the name from the list later would apply +3 instantly.
    Recorded as a footgun in retest.md, not filed.
17. `match-test` works: `kazgame` → exact rule JSON, exit 0; `python3` → "no rule
    matches", exit 1.

## PID 1 (retest of kaz-1)

18. `ps -o pid,ni,comm -p 1,203` → 0 and 0, 18 minutes into the session.
19. Deliberate probe: rule `{"name":"systemd","nice":10}`, reload, 70 s →
    PID 1 still 0 (protection holds); **pid 931 `systemd --user` went to 10**.
    Surprise: "PID 1 is never a target" is exactly PID 1, and a rule named
    `systemd` hits my session manager with no warning.
20. Removed the rule, reload, 70 s → 931 back to 0. So deletion does unwind.
21. `ps -eo pid,ni,comm` audit → only my own test processes are non-zero (plus
    `systemd-coredump` 9 and `aranicy-collectord` -10, which come from the units'
    own `Nice=`). Nothing learned touched a daemon. **Round-1 #1 is gone.**

## Rule features I had not used before

22. `{"name":"kazdemo","nice":5,"ioclass":"idle","oom_score_adj":500}` →
    `ionice -p` says `idle`, `/proc/pid/oom_score_adj` says 500, nice 5. All good.
23. `log_applied_rule = false → true` + reload → takes effect live, new process
    logs `applied rule`. New to me.
24. `kazdemo` rule 5 → 0 + reload → drift applied 0 in ~30 s. Positive control.
25. Added a rule for an already-running `kazdemo2` → −8 within one drift pass.
    Positive control (rules do reach running processes, for nice).
26. Stuck a malformed line in the same file → `WARN ...:2: no JSON object span`,
    other rules still loaded, `reloaded 4 rules`. Good.
27. Regex rule `^kazsleep[0-9]+$` + an exact rule for `kazsleep3` with only
    `sched: batch` → `kazsleep3` got SCHED_BATCH but nice **0**, not the regex's
    −6. Documented ("exact wins outright") but it means my "one regex for all
    versions plus a per-version tweak" plan does not compose. Noted, not filed.
28. `kazsleep9` (regex only) → −6. Regex works.
29. cgroup: `{"cgroup":"kazgame","CPUQuota":50}` → `/sys/fs/cgroup/kazgame/cpu.max`
    = `100000 100000`, which is 50 % of 2 vCPU. Fresh process lands in
    `0::/kazgame`; the already-running one does not. (feeds kaz-8)
30. symlink `/usr/local/bin/mygame → kazgame`, ran it → `applied rule
    rule="kazgame"`, nice −5, but its comm is `mygame`... (this became kaz-6)
31. `{"autogroup": 500}` → `WARN autogroup: 500 out of range -20..=19`; wrong
    guess, my rule did not load. Dead end; retried with 10.
32. `{"autogroup": 10, "nice": 3}` → nice 3 and `/proc/pid/autogroup` = `nice 10`.
33. `{"cpuset": "1"}` → `Cpus_allowed_list: 1`. Works.
34. Type bundle: `kaz.types` with `{"type":"games","nice":-4,...}` +
    `{"name":"kaztype","type":"games"}` → nice −4, `ionice` best-effort.
35. Two rules for one name in different files → the later-sorted file wins
    (`95-...` −9 over `50-...` −5), silently. Only cgroups document that.

## Collector, down-paths and state (new failure modes)

36. `systemctl stop aranicy-collector` → **aranicy also stops** (`Requires=`).
    Dead end for "collector down but daemon retrying"; noted.
37. `systemctl mask aranicy-collector; systemctl start aranicy` → `Failed to
    start aranicy.service: Unit aranicy-collector.service is masked.` Loud, as
    documented. Unmasked, both back up.
38. `aranicy-collectord probe` as my user → libbpf RLIMIT/EPERM wall, EXIT=1;
    with `sudo` → a `--- 8 rows` batch. (kaz-10)
39. First corrupt-state attempt: wrote garbage then `systemctl restart` →
    `INFO bias: restored history entries=61`. Dead end: the daemon flushes state
    on shutdown, overwriting my garbage before it restarted.
40. Second attempt: `systemctl stop`, write garbage, `systemctl start` → `WARN
    bias: cannot load ...: expected value at line 1 column 1`, daemon up with
    empty history. That is the honest test.
41. Unruled CPU hog `kazbulk` (a copy of openssl, `match-test` says no rule) →
    0 → bias 1/2/3 at 30 s intervals → nice 3 in 2.5 min. Round-1 #2(b) narrowed
    to real work, not gone.
42. `systemctl stop aranicy; rm /var/lib/aranicy/bias-state; systemctl start` →
    `dump bias` = `"state": null`, `kazbulk` **still 3**, still 3 after two drift
    passes, `dump proc` shows `{"nice": 3}` and nothing else. kaz-7.
43. `kazdrop`: rule `{"nice":-7}` applied, then changed to `{"ioclass":"idle"}` +
    reload + 75 s → nice still −7, `ionice -p` still `none: prio 0`. Then
    `kaztype` with a cgroup added to its rule → still in the session scope.
    kaz-8.
44. A/B for kaz-6: `kazg2` (rule −5, in `bias_protect`) vs `mygame` (symlink to
    `kazg2`, same rule) → `kazg2` −5, `mygame` −2, and the journal shows
    `applied rule rule="kazg2" pid=2351 bias=3 route="drift"` — one name's rule
    applied with another name's bias. `kazg2-alias` −5 only because it shares a
    prefix with `kazg2`.
45. Doc audit for kaz-9: `bias_attack_min_util` is in the prose, not in the key
    list, and lint accepts it (a bogus key warns); `bias_protect` is in the key
    list but appears 0 times in the reload-liveness section although it is live;
    `autogroup` has no documented range (lint does catch the range, so that part
    is only a missing sentence).
46. Final audit: `ps -o pid,ni,comm -p 1,203,747` → 0/0/0; `dump proc` → 131 rows,
    42 of them at the kernel's own −20 (`kworker/R-*`); my interesting processes
    are 5 rows inside that.

## Dead ends I did not chase

- `ffmpeg` is not installed, so I used a second `openssl` instance as the
  "encoder". Machine has no display, so no compositor; browser-ish/chat-ish
  processes on the box were only `dbus-broker` and `gpg-agent`.
- `pidstat`/`strace` are not installed and I do not install software.
- I did not test `sched: fifo`/`rr`: putting a CPU-bound process in an RT class
  on a 2-vCPU box is a good way to lose the ssh session, and I could not promise
  I would be able to clean up.
- I did not test the documented "refuses a state file from a newer version" path.
- I did not re-test the version-mismatch/downgrade story, or reboot survival
  (a reboot would end the session).

## Verdict

I would keep it installed on my own machine, but only because of two things that
work now: a rule reaches a newly started process immediately and visibly
(`dump rules`, `match-test`, and `log_applied_rule = true` if I turn it on), and
adding my game's name to `bias_protect` really does stop the learned bias from
eating the nice I chose, on reload, without restarting the game — that was the
one thing missing last round and it is the one thing that makes the automatic
part safe for me. What would have to change first: the protection has to key on
the same name the rules key on (a game launched through a differently-named file
gets its rule and none of the protection — kaz-6), losing or corrupting the bias
state must not leave processes stuck at a learned nice with no record (kaz-7),
and a per-game `"bias": false` in the game's own rule file should exist instead of
a growing global list owned by root. Until then I would keep `bias_protect` as a
hand-maintained second config and check `ps` after every game patch, which is
exactly the sysadmin job I bought this to avoid.

# Session log — kaz, round 3 (adversarial + differential)

Times are the machine's clock (UTC), which is 8 h behind my desk. All remote
commands went through `./machine '…'`; local commands are marked `[local]`.
Nothing here is tidied — the mistakes are part of the record.

## 09:54–09:56 — getting the box and reading the package

1. `[local] ./machine 'uname -a; cat /etc/os-release; nproc; free -m'` → Arch,
   7.2.11-arch1-1, 2 vCPU, 1958 MiB. Nothing installed yet (`pacman -Q` empty,
   `systemctl is-active aranicy` inactive).
2. `[local] ./machine 'ls -la ~/payload ~/previous-build'` → two package pairs,
   both `0.3.0-1`, built 09:52:22 and 08:59:35.
3. `[local] ./machine 'tar -xf …'` both pairs into `~/pkgcmp` and `diff -rq` →
   **first surprise: the docs moved.** Current build has
   `usr/share/doc/aranicy/docs/{COMPATIBILITY,BENCHMARKS}.md`; previous build has
   them one level up. Binaries differ; collector differs.
4. Read both READMEs, the new COMPATIBILITY.md, both `.INSTALL` scripts and both
   unit files. Noted `Requires=aranicy-collector.service`, `Restart=always`,
   `RuntimeDirectory=aranicy`, and that `post_remove` (current build) deletes
   `bias-state` while `post_upgrade` only does `daemon-reload`.

## 09:55 — install, exactly as the README says

5. `[local] ./machine 'cd ~/payload && sudo pacman -U --noconfirm aranicy-collector… aranicy…'`
   → both installed, exit 0.
6. `sudo systemctl enable --now aranicy` → `active (running)`; collector pulled
   in by `Requires=` and also `active` (it is `disabled` as a unit, which is
   expected and worth remembering for the uninstall test).
7. `aranicy lint` → `warning: 0 rules loaded … this corpus will apply nothing`
   + `lint: warnings found`, exit 1.
8. `aranicy --help` → five subcommands, `--reload`, `--config`, `--config-dir`.
9. Doc-path check → `COMPATIBILITY.md` and `BENCHMARKS.md` MISSING at the paths
   the README names. (Filed later as kaz-14.)
10. `journalctl -u aranicy -u aranicy-collector` → startup warnings: no rules,
    no `latency_nice` on this kernel, sampler bound, `exec-event client
    connected`.

## 09:56 — first functional check (and the base for every later comparison)

11. `sudo cp /usr/bin/openssl /usr/local/bin/kazdiff` + rule
    `{"name":"kazdiff","nice":-7}` + `systemctl reload aranicy` + start →
    `ps` shows `-7` at 3 s. Rule at exec works.
12. Also started `kazbulk` (same binary, **no rule**) to watch the learner.
13. `match-test kazdiff` → exact hit; `match-test kazbulk` → `no rule matches`.
    `dump rules` → one entry. `dump proc` → every live pid including kernel
    threads at their own nice (`kworker/R-*` at −20) — noted, not filed.
14. `dump bias` → `"state": null` (debounced write, nothing on disk yet).

## 09:56–09:57 — junk config and junk corpus (attack 1–4)

15. Built six scratch config dirs (`empty`, `half1`…`half4`, `unknown`) and linted
    each. All six warn; exit 1. Nothing fatal.
16. `[mistake]` my first lint loop piped through `cut`, so `EXIT=$?` was cut's
    status (0) and one line looked like "exit 0". Re-ran with
    `aranicy lint > out 2>&1; echo EXIT=$?` → 1. Lesson: the pipeline exit code
    lies.
17. Wrote a junk `.rules` file: plain text, unterminated object, object+words,
    binary line, two objects on one line, 100 KB line, plus a *directory* named
    `92-dir.rules`. `lint` and a live reload both warn per line; `reloaded 4
    rules` and `dump rules` shows the four good ones, including the one inside
    the directory. Nothing silent.

## 09:57–10:01 — the one that bites (attack 5)

18. Truncated the `kazdiff` rule to 24 bytes (`{"name": "kazdiff", "nic`),
    reloaded → one WARN `no JSON object span`, `reloaded 3 rules`.
19. Waited 210 s → **kazdiff went from −7 to +3**. Same for the unruled
    `kazbulk`. Checked the bias log: `bias changed name=kazdiff bias=1/2/3
    cause=Evidence`, plus the earlier discovery that the entry was already
    ramping *before* the rule disappeared.
20. Noticed in passing: `dump proc` now has both pids at `"nice": 3`; there is no
    field anywhere saying "these were ruled ten minutes ago".

## 10:01–10:02 — non-root, masked collector, `kill -9` (attacks 7–9)

21. As `arch`: `aranicy start` → `requires root (effective uid 1000)`, exit 1.
    `aranicy --reload` → `failed to signal aranicy … Operation not permitted`,
    exit 1. `aranicy dump types` → `{}`, exit 0.
22. `aranicy-collectord run` as user → libbpf EPERM wall. Same class as kaz-10.
23. `systemctl stop aranicy-collector` → both services inactive. `mask` the
    collector, `start aranicy` → `Unit aranicy-collector.service is masked`,
    exit 1. Loud, correct.
24. `kill -9` on the collector → back in 6 s. **Surprise: the daemon restarted
    too** (656 → 876). Twice more later, always the same.

## 10:02–10:03 — install / remove / reinstall (attack 11)

25. `pacman -R aranicy` → daemon gone, **collector still active**, `/var/lib/aranicy`
    deleted. (kaz-13)
26. `pacman -R aranicy-collector` → collector gone, `/run/aranicy` gone, rules in
    `/etc/ananicy.d` kept.
27. Reinstalled both, `enable --now`, lint, started a fresh ruled process → −5.
    Everything back.
28. `[mistake]` before rebooting I looked at `dump bias` and saw `"state": null`
    after 30 s of uptime — the on-disk snapshot is debounced ≥ 60 s. Not a bug;
    I noted it so I would not read "no state" as "state lost".

## 10:03–10:05 — reboot (attack 10)

29. Started a long workload, `sudo systemctl reboot`, polled. Machine back in
    ~50 s. `[mistake]` the first poll loop printed nothing for 5 minutes and
    looked like a hang; the box had been up for minutes and the loop was stuck
    in a later long command. Stopped it, probed with a bounded `timeout 20`.
30. After boot: both services active, `bias: restored history entries=27`,
    sampler live, exec client connected. New ruled process → −5. Clean.

## 10:05–10:06 — cgroup quota, post-boot function

31. `{"cgroup": "kazgame", "CPUQuota": 50}` + a rule → process in
    `0::/kazgame`, `cpu.max = 100000 100000` on 2 vCPUs (= 50 % of the box).
    Matches the current doc's formula.
32. `dump bias` after boot → 66 entries; `/var/lib/aranicy/bias-state` present.

## 10:05–10:26 — differential, first half (build A → B)

33. Wrote `~/adv/checks.sh`: twelve fixed checks, identical wording, no state
    carried between runs except `/etc/ananicy.d`, which both builds see the
    same way.
34. Ran it on build A → `~/adv/result-A.txt`.
35. Wrote `~/adv/slow.sh` for the timed checks. **Mistake:** the truncation step
    was `sudo head -c 22 f > ~/adv/r.tmp` then `sudo cp r.tmp f`; the temp file
    was never created (root redirect into a user dir, then a copy of a
    non-existent file), the copy failed silently and the rule stayed whole. The
    run was still useful — it is the clean −7 → −4 measurement — but the case I
    meant to test did not run. (Rewrote it as
    `sudo sh -c 'head -c 22 f > f.tmp && mv f.tmp f'`.)
36. Result on A: unruled hog 0 → 1 at ~96 s → 3 at ~156 s, applied on the next
    drift pass; ruled name −7 → −4; collector kill 6 s + daemon restart; the new
    process after the crash got −2 (rule −5 + learned 3).
37. Installed build B over the running A → **`/proc/<pid>/exe -> (deleted)`,
    same pid, CLI says the new version**. (kaz-12) Restarted both by hand.
38. Ran `~/adv/checks.sh` on B → `~/adv/result-B.txt`. Differences: docs at the
    old paths; empty-corpus lint `OK` exit 0; one fewer warning line on the
    impossible config. Everything else identical.
39. `~slow2.sh b1` on B (working truncation): rule truncated at t=110 →
    **−7 to +1 within ~16 s**, then +3; unruled hog 0 → 1 at ~95 s → 3 at ~155 s.
40. Unknown key in the *live* config on B → WARN, daemon runs. Rule −7 with the
    file whole → −5 → −4. Collector kill → 6 s, daemon restarted again.

## 10:21–10:29 — lifecycle on B, then back to A

41. `pacman -R aranicy` on B → collector still running, **`bias-state` kept**
    (10145 bytes) — the one real behavioural difference in the package scripts
    (`post_remove` is new in A).
42. Reinstall B, `enable --now`, reboot → back with `restored history entries=61`,
    rule applied.
43. Installed A over B → `/usr/bin/aranicy (deleted)` again, restarted by hand.
44. Re-ran checks on A: unknown key at daemon startup → WARN `config: line 45`,
    still `active`.

## 10:29–10:33 — identity attacks (attack 12)

45. `exec -a steam /usr/local/bin/kazexec` → comm `kazexec`, `argv[0]` `steam`,
    rule applied (−5). Correct per the contract.
46. Python + `prctl(PR_SET_NAME, "kazren")` → comm `kazren`, rule for `kazren`
    exists, nice **0**. `[mistake]` `ps -C python3` found nothing because `-C`
    matches comm, which had just been rewritten; `ps -C kazren` found it.
47. Python `os.execv` into another ruled binary → the same pid gets the new
    rule (−5). Correct.
48. Unprivileged copy of `/usr/bin/sleep` named after a rule → `nice −10`; named
    after a `sched: fifo` rule → `SCHED_FIFO 50`. Documented in the current
    build, not in the previous one. Verified, not filed.

## 10:33–10:36 — deleting a rule, and the last reinstall

49. `kzdel` ruled at −7, watched 150 s → −4 (learned +3). Deleted the rule file,
    reloaded → `reloaded 15 rules; bias reconfigured` and nothing else; within
    ~70 s the process was at **+3**. Grepped the journal for the name,
    "removed", "deleted", "no longer" → empty. (kaz-11)
50. Reinstalled A over itself a third time and asked systemd directly:
    `ActiveState=active`, `MainPID=748` unchanged, `exe -> … (deleted)`,
    `MarkedForRestart` empty, no `Starting`/`Stopping` lines. (kaz-12)
51. Final state check: both services active, `aranicy 0.3.0`,
    `/run/aranicy` has both sockets.

## What I did not get to

- A deterministic pid-recycle test. 300 sequential short-lived ruled processes
  (300/300 correct) is the closest I got; I am not calling that a recycled pid.
- The performance claim (12.3 % vs ≤ 5 %). Needs a paced storm on an idle host.
  Recorded as not measured.
- The deleting-a-rule case on build B (only run on A). The truncation case was
  run on both, which is the same mechanism one step earlier.
- `sched`, `oom_score_adj`, `cpuset` and `autogroup` for the "rule edit reaches
  a live process" question (kaz-8, round 2). Not retested this round.

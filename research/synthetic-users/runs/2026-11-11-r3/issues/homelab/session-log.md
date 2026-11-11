# Session log — round 3 (adversarial + differential)

Box: `su-homelab-r3`, Arch rolling, kernel 7.2.11-arch1-1, 2 vCPU, 1958 MiB.
All access through `./machine` (ssh, port 22047). Local transcripts are in
`raw/`; the scripts I ran are in `tools/`. Times are the box's (UTC).

Dead ends and surprises are kept in, in order.

1. `ls`, `card.md`, `sysinfo.txt`, `machine`, `previous-reports/*` — read the
   brief, the persona and the six issues from the earlier round. Machine is a
   fresh cloud image: no aranicy installed, no `/etc/ananicy.d`.
2. `./machine 'pacman -Q | grep -i aranicy; ls /etc/ananicy.d; ...'` — nothing
   installed (`QEXIT=1`). Listed both package pairs' file lists: the current
   build moves `BENCHMARKS.md`/`COMPATIBILITY.md` into `docs/`.
3. Extracted both packages under `~/stage` and diffed them: docs moved, README
   rewritten; `.INSTALL` gained the bias-state cleanup; units and default
   config byte-identical.
4. Read the shipped `README.md` and `docs/COMPATIBILITY.md` in full. Noted the
   claims I would test later: "a missing collector is a loud failure", "the
   packaged documentation is …", "period fixed at the kernel's 100 ms".
5. Baseline snapshot: tools present (`openssl`, `xz`, `gzip`, `tar`, `python3`;
   no `unrar`, `par2`, `bpftool`, `strace`, `pidstat`, `jq`), nice values of
   `sshd`/`systemd-journald`/`systemd-resolved`/PID 1/`kworker`s.
6. Wrote `tools/diffcheck.sh` (fixed suite D0–D12) and ran it on the previous
   build. Two checks were **invalid** in that first run: `gzip` and `tar`
   finished before `pgrep` could sample them (empty `ps -p` output), so D4/D9
   measured nothing. The run also left the previous build uninstalled.
7. Fixed D4/D9 to use a long workload (`dd | gzip -9`, `tar -cf /dev/null
   /usr`) and a wait-loop for the pid, added a `journalctl -o json` parse
   check, re-ran the whole previous-build suite (`raw/diff-prev.txt`).
8. Ran the same suite on the current build (`raw/diff-cur.txt`). First results
   that matter: empty-corpus `lint` now exits 1 with a warning; unknown cgroup
   is warned at lint and reload; uninstall removes `/var/lib/aranicy`;
   `dump proc` lists kernel threads.
9. First adversarial batch (`tools/adv1.sh`) failed with `Permission denied`
   because I ran the script before `chmod +x`; re-ran.
   - Junk rule file: 12 warnings with line numbers, daemon healthy, good rules
     still applied. Empty/comment/BOM/CRLF/unreadable files: BOM and CRLF are
     accepted and applied; unprivileged `lint` reports the unreadable file.
   - Half-written config: one warning per line including the unknown key.
   - **My own mistake**: the config backup `sudo cp -n … ~/ananicy.conf.orig`
     never created the file (I ran it *before* `pacman -U`, and the package had
     been removed a minute earlier, so `/etc/ananicy.d/ananicy.conf` did not
     exist yet),
     so the "restore config" steps in A5–A7 failed and the broken config
     stayed. On top of that, five restarts inside 60 s hit
     `StartLimitBurst=5`, so A5–A7 measured the start limit, not the product.
     Results discarded; those checks were redone later (adv5, focus).
10. Second adversarial batch (`tools/adv2.sh`): first attempt produced an empty
    transcript — I wrote `machine 'cat > ~/adv2.sh; …'` without feeding the
    script on stdin. Re-ran with the redirect.
    - `kill -9` collector: sampler logs `degraded … transport down`, systemd
      restarts it after 5 s, the daemon follows, policy resumes.
    - `kill -9` daemon: restarts in under 3 s, `bias: restored history`.
    - reinstall while running: old pid kept; **surprise** — `readlink` did not
      show `(deleted)` because the process had been restarted after the last
      `pacman -U`; redone properly in adv5.
11. Third batch (`tools/adv3.sh`) **killed its own ssh session**: the first
    loop iteration read `MainPID=0` while systemd was between restarts and
    `sudo kill -9 0` signals the process group. Lost the rest of the batch;
    redid it as `adv3b.sh` with a guard (`skip: MainPID=0`).
12. Reboot #1 (mid-work, no rules at that moment): boot id changed, both units
    active, bias history restored, previous boot's journal still readable.
    **Surprise**: `khugepaged` at nice 19 — chased it as a possible
    kernel-thread violation until I got a daemon-free baseline.
13. Reboot #2 (rule present, `xz` running): rules survive, fresh `xz` gets
    nice 19, protected processes at 0, `khugepaged` still 19.
14. Reboot #3 (collector masked): **the finding** — `aranicy` comes back
    `inactive (dead)`, `is-failed` is false, and the unit journal is empty.
    Manual start is clear (`Unit … is masked`). Same boot gives the
    daemon-free `khugepaged` baseline: still 19, so it is the kernel default,
    not aranicy.
15. Focused runs (`tools/focus.sh prev|cur`): isolated `turbo-cores` lint is
    green on both; apply is silent even with `log_applied_rule = true` (the
    positive control `applied rule rule="xz" …` proves the logging path
    works); cgroup `cpu.max` is `100000 100000`; the previous build's doc says
    "period 1 s", the current one says "the kernel's 100 ms".
16. `adv5` (upgrade/uninstall): reinstall leaves `/usr/bin/aranicy (deleted)`
    and `NRestarts=0`; uninstall leaves `/sys/fs/cgroup/ivymedia` with
    `100000 100000` and the `xz` still inside it; `RuntimeDirectory` and the
    state file do go away.
17. `adv4` (renames/pid/one-shots): `exec -a gzip xz` gets the xz rule (19),
    `prctl` rename to gzip stays python3 (0), copied `/usr/bin/xz` named
    `gzip` gets the gzip rule (10) — documented trust model.
18. `adv6`: shrank `pid_max` to 1024, recycled pid 301 with an unrelated
    `sleep` — nice 0, no stale policy; restored `pid_max`. Ten successful
    applies produced zero journal lines (later filed as the logging
    enhancement).
19. Direct-start probes: `systemctl stop aranicy` leaves the collector active
    (**surprise**, one-way dependency); with both stopped,
    `/usr/bin/aranicy start` fails loudly after 5 s with
    `Error: no collector at …`. Saved as `raw/stop-asymmetry.txt`.
20. `adv7`: BOM and CRLF rule files both parse and apply (tar 7, dd 7); a
    NUL-only line is reported like any non-JSON line.
21. `prevfinal`: previous build behaves identically for upgrade-in-place and
    for the leftover cgroup; it still leaves `bias-state` on uninstall.
22. **Correction**: while writing `ivy-18` I described a `match-test` test with
    a kworker rule I had not actually run. Ran it (a real `--config-dir` with a
    `kworker/0:1-events` rule): the current build **matches exactly**, the
    previous build cannot match at all. Rewrote the issue down to the cosmetic
    "no rule matches `0:1`" message and corrected the differential table
    (D7b went from "same (still wrong)" to "better").
23. **Correction**: the first draft of `ivy-14` quoted a `grep` of the unit
    files that I had written from memory. Ran it, replaced the evidence block
    with the actual output.
24. Cleanup: removed test rules, the leftover cgroup, `~/lr`, `~/lrw`,
    `~/stage`, the copied binary; reinstalled the current pair; final state is
    `active/active`, `enabled/disabled`, no rules, `khugepaged` untouched.
25. Wrote `adversarial.md`, `differential.md`, `issues/ivy-12..19`, this log.

Open holes I could not close on this box: cost of the idle collector (no
`pidstat`/`bpftool`), whether a BPF-less kernel produces a failed collector
unit, and a boot-level comparison of the previous build (units are identical,
so I did not spend a fourth reboot).

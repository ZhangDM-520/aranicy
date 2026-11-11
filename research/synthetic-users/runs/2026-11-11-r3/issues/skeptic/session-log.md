# Session log — dana, round 3 (adversarial + differential)

Machine `su-skeptic-r3`, everything through
`/tmp/workspace/su/r3/skeptic/machine '<cmd>'`. Times are the machine's UTC
clock. Dead ends and mistakes are left in.

## Setup

- 09:54 `uname -a; id; nproc; free -m; ls ~/payload ~/previous-build`
  → `Linux 7.2.11-arch1-1`, uid 1000 (no extra groups), 2 vCPU, 1958 MiB,
  uptime 2 min. Nothing installed (`pacman -Q | grep arani` empty), both units
  unknown. **`strace`, `perf`, `bpftool`, `bpftrace`, `pidstat` are absent**;
  `chrt`, `taskset`, `getcap`, `python3`, `bsdtar` present. `gcc`/`cc` absent.
- 09:54 extracted both package pairs to `~/work/...`, read
  `README.md`, `BENCHMARKS.md`, `COMPATIBILITY.md`, `ananicy.conf`, both units,
  both `.INSTALL` scripts, `.PKGINFO`.
  → 4 BPF progs + 1 perf-event link in the collector, ringbuf map (1048576) +
  LRU hash (4/96/16384) visible via `/proc/<pid>/fdinfo`, no pinned maps.
      *(first attempt at this diff was wrong — see the 09:56 mistake below.)*

## Naive install (as the README says)

- 09:55 `sudo pacman -U --noconfirm ~/payload/*.zst; sudo systemctl enable --now aranicy`
  → both packages installed, `aranicy.service` enabled+started, collector
  started by `Requires=` (unit stays `disabled`). `aranicy lint` → `warning: 0
  rules loaded ...`, rc=1. Startup log: `WARN no rules loaded`, `WARN kernel
  does not support latency_nice`.
- 09:55 `aranicy dump` (no arg) → error, `<WHAT>` required; `dump
  rules|proc|bias|cgroups` all work unprivileged. `/run/aranicy` has
  `collector-events.sock` (0600) and `collector-stats.sock` (0755).
- 09:55 rule `{"name":"skepticnaive","nice":12}` + reload + exec a copy of
  `/bin/sleep` under that name → `nice=12`. Baseline works; wrote
  `/home/arch/j/try.sh` (`cp /bin/sleep /home/arch/<name>; setsid; read
  /proc/<pid>/stat`).

## Adversarial A — config and corpus

- 09:56 empty dir / junk file / half config / unknown key / garbage value:
  `lint` names every line, daemon warns and keeps running. Held. (Details in
  `adversarial.md` §1–2.)
- 09:56 **mistake**: I ran `timeout 8 setsid aranicy start ...` as root. The
  `timeout` kills `setsid`, not the daemon in its new session, so several
  daemons survived the test and ran concurrently. Cleaned up with `pkill`
  — see next line.
- 09:58 **mistake, cost a dropped connection**: `sudo pkill -f "aranicy start
  --config"` matched *my own* remote shell's command line and killed the SSH
  session (rc=255). Rule for the rest of the session: kill by explicit pid,
  never `pkill -f` a pattern that my own command line contains.
- 09:58 second-instance guard: `sudo aranicy start` while the service runs →
  `Error: another aranicy instance is already running (pid 1516); if that pid
  is stale, remove /run/aranicy.pid` (good), pid file not clobbered.
- 09:58 stale pid file planted at a live `bash` pid → `aranicy --reload` printed
  `sent SIGHUP to aranicy (pid 1672)` and the session died. Filed as dana-15.
- 09:58 `kill -9` the daemon mid-run → systemd restarts it after 10 s, pid file
  rewritten, rules still apply. Held.

## Adversarial B — peer service

- 09:58 **mistake**: my first collector-kill test ran while the daemon was down
  (the previous connection died before `systemctl start aranicy`), so the
  "control" exec read `nice=0` and the whole test was meaningless. Re-ran at
  09:59 with both units active.
- 09:59 collector `kill -9` with the daemon up → exec during the outage gets
  `nice=0`; collector restarts after 5 s, daemon is restarted by the
  `Requires=` deactivation, rules apply again (`nice=12`). Held.
- 09:59 collector masked → `systemctl start aranicy` fails with `Unit
  aranicy-collector.service is masked`; masking it under a running pair
  deactivates both (systemd `Requires=` stop propagation). Fail-closed and
  visible. Noted as an undocumented dependency, not filed.

## Falsification — does it really decide from observation?

- 10:00 started two named workers: `skepticspin` (python busy loop) and
  `skepticchat` (20 ms sleeps). `dump bias` (on-disk, debounced).
- 10:02–10:05 watched for 3 min: `skepticspin` → `class=bulk`,
  bias 0→1→2→3 at 30 s steps, **`/proc/2339/stat` nice went 0→2→3 in step**;
  `skepticchat` stayed `class=unknown`, bias 0, nice 0. Journal carries
  `INFO bias changed name=skepticspin bias=N prev=M class=Bulk cause=Evidence`.
  → the central claim survives this test on the bulk axis.
- 10:02 found the daemon at `nice=19` after I loaded `{"name":"aranicy","nice":19}`
  ~60 s earlier (`Nice=-5` in the unit, zero journal lines). Filed dana-13.
- 10:01 cgroup: `{"cgroup":"skepticcg","CPUQuota":50}` +
  `{"name":"skepticcguniq","cgroup":"skepticcg"}` → `/proc/<pid>/cgroup` =
  `0::/skepticcg`, `/sys/fs/cgroup/skepticcg/cpu.max` = `100000 100000`.
  → the *new* `docs/COMPATIBILITY.md` formula (100 ms period) is right and the
  *old* top-level one ("period 1 s") is wrong.
- 10:06 identity: `prctl(PR_SET_NAME)` and `exec -a` on a differently named
  file both got `nice=0` (inherited) while the control file named after the
  rule got `nice=12` → identity is the exe basename, as documented.

## Reboot 1

- 10:07 `sudo systemctl reboot`, machine back in 19 s. Both units active,
  `aranicy` enabled, bias history restored (61 entries), rules apply at exec
  (`nice=12`).
- 10:07 **`WARN cgroup "skepticcg" not created: Permission denied (os error 13)`**
  at startup.
- 10:09 `systemctl restart` created it correctly → started the dana-11 thread.
- 10:08 **mistake, poisoned a measurement**: my `sshd` rule (`nice: 5`) from the
  blast-radius test reniced the listener, and every *new* SSH session then
  inherited `nice=5`, which is why a later identity test read `nice=5` for
  processes that matched no rule. Fixed with `sudo renice -n 0 -p <sshd>` and
  by deleting the rule; re-ran the identity test at ~10:31 with a `nice=0`
  baseline (`nice=0` confirmed).
- 10:08 **mistake**: my first package-content diff said "binaries identical".
  Wrong: the previous-build extraction had failed its `cd` and un-tarred *into*
  the payload directory, overwriting it. Redone with `bsdtar -xf ... -C
  <fresh dir>` per package → **the binaries are different** (this is what made
  the differential meaningful).

## Battery / differential

- 10:10–10:14 fixed 15-check battery on the current build
  (`/home/arch/j/bat-current.txt`). Note B4 in the script is a bad check
  (`cmp` on a missing file → `DIFFER`), and B8's `rc` is `tail`'s.
- 10:15 `pacman -R aranicy aranicy-collector` → clean: no binaries, no units,
  no enable symlink, no `/var/lib/aranicy`, no `/run/aranicy`, no pid file.
  Only my unowned rules remain in `/etc/ananicy.d`.
- 10:15 installed the previous build, `enable --now`, docs at top level, README
  links MISSING (dana-9 reproduced).
- 10:16–10:20 battery on the previous build (`bat-prev.txt`). Same results
  except: `lint` on an empty corpus does not say "0 rules loaded"; links are
  broken; docs are at the top level.
- 10:19 `pacman -U` current over previous → **the old daemon keeps running**
  (`/usr/bin/aranicy (deleted)`, sha 6c0bd1a9 = previous) until a manual
  `systemctl restart` (then 16c10445 = current). Both packages `0.3.0-1`.
  Filed dana-12. `pacman -Qkk` says `0 altered files`.
- 10:19 upgrade removed the old top-level docs cleanly — no orphans, which
  answers the "does a file move leave junk" question: it does not.

## Reboot 2

- 10:20 reboot with a boot-time one-shot unit sampling `/sys/fs/cgroup/skepticcg/cpu.max`
  at t+20 s: `No such file or directory`. From t+80 s: `max 100000` (quota
  gone) while `dump cgroups` still reports `cpu_quota_pct: 50`.
- 10:22 `rmdir /sys/fs/cgroup/skepticcg; systemctl restart aranicy` →
  recreated with `100000 100000` within 1 s and held for 200 s.
  → dana-11 is a boot-path failure, not a general one.
- 10:26–10:30 second 200 s watch after a restart: `100000 100000` throughout.
  One earlier reading (10:23:41) of `max 100000` 46 s after a good restart I
  could not reproduce; recorded as unexplained, not claimed.

## Wrap-up

- 10:31 removed my `skeptic-cgcheck.service`, the extra rule files, and the
  test binaries; left `/etc/ananicy.d` with the four files used by both
  batteries and the current build installed and running.
- Not attempted: pid-reuse (could not force the kernel to hand back a pid),
  `SCHED_FIFO` starvation (2 vCPU, would have cost the box), `latency_nice`
  (unsupported by this kernel per the daemon's own warning).

## Verdict

Over two reboots, three install/remove cycles, ~40 exec-time probes, one
three-minute bias observation and two full check batteries, this build did the
thing it advertises — rules landed on exec within a second, a CPU-bound task was
classified `bulk` and walked 0→+3 nice in 30 s steps with `/proc` agreeing at
every step, an interactive task was left alone, PID 1 and the kernel threads I
kept in view were untouched, and every failure mode I could reach (junk corpus,
truncated config, unknown key, no root, collector masked or SIGKILLed, daemon
SIGKILLed, reboot) failed closed, said something in the journal and recovered —
with four exceptions, all silent from the operator's seat: a cgroup `CPUQuota`
that is created and then not applied after a boot (`dump cgroups` still reports
50 %; a service restart fixes it), an upgrade that leaves the previous binary
running from a deleted inode under an unchanged `0.3.0-1` version string, a
corpus entry named `aranicy` that walks the daemon from the unit's `Nice=-5` to
19 without a log line on every boot, and a `--reload` that SIGHUPs whatever pid
`/run/aranicy.pid` names without checking what that process is. I saw no
blocker — nothing destroyed priority state that could not be restored, and
nothing wrote to a kernel thread or PID 1 in any test I ran — but "active
(running)" here does not mean "doing what its config says", and none of the four
defects above can be seen from `systemctl status` or from `aranicy dump`.
Between the two builds I found three documentation improvements and zero
behavioural regressions: the executable-based identity, the cgroup move, the
collector-loss recovery and the bias engine behave identically on the previous
build, so what shipped this round is a docs change on top of a rebuild — and it
introduced one new contradiction inside the README it was meant to fix.

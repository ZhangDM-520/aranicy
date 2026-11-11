# session log — ivy, round 1 (su-homelab-r1)

Machine: Arch rolling, kernel 7.2.11-arch1-1, 2 vCPU, 1958 MiB, user `arch`.
All commands below were run through `./machine '<cmd>'`; machine clock is UTC
(the log times are the machine's, `date +%T`), my local time is +08:00.

## 1. Orientation

- `uname -a; id; nproc; free -m; ls -la ~/payload; sha256sum ~/payload/*`
  → `7.2.11-arch1-1`, `uid=1000(arch)`, 2 CPUs, 1958 MiB total, both files
  present, hashes match `sysinfo.txt`
  (`467d6dcf…89d7f` and `ba4cf0a1…6f0b`). Good — this is the package I was
  given.
- `pacman -Qip` both packages → `aranicy 0.3.0-1` (MIT, provides/conflicts
  `ananicy-cpp`), `aranicy-collector 0.3.0-1` (GPL-2.0-only, depends `libbpf`).
  Both: `Install Script : No`, `Signatures : None`.
- `pacman -Qlp` both → daemon: `aranicy`, `ananicy.service`,
  `/etc/ananicy.d/ananicy.conf`, README, LICENSE. Collector:
  `aranicy-collectord`, `aranicy-collector.service`, README, LICENSE.
  **No rule files in either package.**
- Pre-install checks: `pacman -Q libbpf` → 1.7.0-1; `/sys/kernel/btf/vmlinux`
  exists; `/sys/kernel/sched_ext` exists; `anatop` no. `systemctl --version` →
  262. `pacman -Q ananicy-cpp` → not installed. sched_ext `state` = `disabled`,
  no `scx_*` binary in `/usr/bin`.
- Baseline `/proc` priorities for sshd (744), systemd-journal (206),
  systemd-resolve (257) → all nice 0; 42 kernel threads at -20, `ksmd` 5,
  `khugepaged` 19. Saved a full `ps -eo pid,ni,comm` snapshot to
  `~/nice-before.txt` (128 rows) for later diffing.

## 2. Install and first start

- `sudo pacman -U --noconfirm ~/payload/*.zst` → exit 0, both installed,
  no post-install script, units land in `/usr/lib/systemd/system/`.
- `systemctl is-enabled/is-active aranicy aranicy-collector` → disabled/inactive
  (expected for a package without a scriptlet).
- Read `aranicy.service` / `aranicy-collector.service` / `ananicy.conf`.
  Noted: `Type=notify`, `Requires=aranicy-collector.service`, `Nice=-5` for the
  daemon and `-10` for the collector, hardening block, `StateDirectory=aranicy`.
  Config defaults: `check_freq = 60`, all `apply_*` true, `loglevel = info`,
  `log_applied_rule = false`, one `bias_group.build` regex.
- `sudo systemctl enable --now aranicy` → both units active within 4 s.
- `sudo journalctl -u aranicy -u aranicy-collector -o cat` → 8 lines total.
  **No line about rules.** One `WARN kernel does not support latency_nice`.
- `aranicy dump rules` → `{"exact": [], "regex": []}`.
- `sudo ls -la /etc/ananicy.d/` → only `ananicy.conf`.

## 3. Does it apply anything at all?

- Wrote `~/testrules/00-test.rules` (ffmpeg nice/ionice, unrar 19, xz 15,
  sleep 5), `aranicy lint --config-dir ~/testrules` → warning "config not
  found; using defaults" + `lint: warnings found` + **exit 1**.
  `aranicy match-test --config-dir ~/testrules ffmpeg` → matched, policy shown,
  exit 0.
- `sudo cp` the file to `/etc/ananicy.d/`, `sudo systemctl reload aranicy` →
  `INFO reloaded 4 rules; bias reconfigured`.
- `setsid sleep 300 &` → `/proc/<pid>/stat` field 19 = **5**. Rule applied.
- `ionice` via a copy of `/bin/sleep` named `ffmpeg` (`~/bin/ffmpeg`) → nice 10,
  `ionice -p` = `best-effort: prio 7`. Applied.
- `aranicy dump proc` → lists every process with `name`/`nice`/`pid` only, no
  rule, no ionice, no cgroup. `aranicy dump bias` → `state: null` at that point.

## 4. "kernel does not support latency_nice" — dead end, environment

I wanted to know whether that startup warning is true, because `apply_latnice`
defaults to true. Probed `sched_setattr` from python3 via ctypes
(syscall 314): every variant returned `EINVAL`, including a **NULL** pointer,
which should return `EFAULT`. So the syscall is being intercepted in this
sandbox and the probe says nothing about the kernel. Conclusion: environment
note, not a bug; I did not file it. (It also means sched-policy changes via
`sched_setattr` cannot be validated on this box at all.)

## 5. Applying to real workloads, and what it does not touch

- Ran two `xz -1 -c /dev/zero` for 150 s, diffed `ps -eo pid,ni,comm` before
  and after → only kworker *name* suffixes changed; no nice value changed
  except the new `xz` processes, which came up at **15** from the rule.
  sshd/sshd-session stayed 0.
- Journal during that window: still 8 lines — no per-apply logging at the
  default `log_applied_rule = false`.
- Set `log_applied_rule = true`, reload, spawn `sleep` → `INFO applied rule
  rule="sleep" pid=2117 bias=0 route="exec"`. Knob works. Noticed one pid
  logged twice in the same millisecond (pid 2118, two identical lines).
- Drift corrector: `sudo renice -n 0` on the ffmpeg-pid → back to **10** within
  75 s, logged as `route="drift"`. Works.
- Bias engine is live even with sched_ext `disabled`
  (`INFO stats sampler live prev=degraded: … bound SchedSample via sched_samples`).
  `INFO bias changed name=xz bias=3 prev=2 class=Bulk cause=Evidence`.
- Tried to make journald interesting: `while :; do seq 1 3000 | logger -t flood; done`
  + `openssl speed -seconds 100 -multi 2 sha256` for 90 s. journald nice stayed
  **0** (it is classified `bulk`, confidence 0.935, bias 0). `sshd`,
  `systemd-resolve`, `sshd-session`, all kernel threads: unchanged nice.
  `systemd-userwor` reached bias 2, `openssl` bias 1, `python3` bias 1.

## 6. Broken rule file

- `99-bad.rules` with `nice: 999`, a non-JSON line, and one good rule.
  `aranicy lint` → per-line warnings, exit 1. Restart → the same two warnings
  at `WARN` in the journal; the good line still loaded (`another` in
  `dump rules`), the bad one dropped. Reasonable behaviour, no issue filed.

## 7. Dependency / collector handling

- `sudo systemctl stop aranicy-collector` → `Requires=` propagates: both units
  `inactive`. `sudo systemctl start aranicy` → both back active.
- `sudo systemctl mask --runtime aranicy-collector; sudo systemctl restart aranicy`
  → `Failed to restart aranicy.service: Unit aranicy-collector.service is
  masked.` — visible failure, good. **Dead end**: the already-running daemon
  was not restarted, so this did not test "collector absent" the way I wanted.
- `sudo kill -9 <collector pid>` → the whole stack restarted (daemon stopped,
  collector restarted after 5 s, daemon started again) and a new `sleep` was
  still niced to 5 afterwards. Recovery works; the cost is that a collector
  crash also restarts the daemon and drops up to a minute of bias state.

## 8. Reboot

- `sudo systemctl reboot`, polled `./machine 'echo up'` every 5 s → back after
  ~65 s (one dead end: nothing answered for the first 60 s, which is just boot
  time on this box).
- After boot: both units `active`, `aranicy` `enabled`, `aranicy-collector`
  `disabled` but pulled in by `Requires=` (fine). Journal:
  `INFO bias: restored history entries=95`, sampler live. New `sleep` →
  nice **5**. It comes back doing its job with no manual step. Good.

## 9. Uninstall

- First attempt: `systemctl disable --now aranicy` then `pacman -R` → clean
  stop, but `pacman -R` saved `/etc/ananicy.d/ananicy.conf` as `.pacsave`,
  left `/var/lib/aranicy/bias-state` (18 641 bytes) behind, and left
  `/run/aranicy` with two stale sockets (that one is my own doing: the units
  were still running when I looked; a clean `systemctl stop` does remove
  `/run/aranicy`).
- Second attempt, the realistic one: `pacman -R --noconfirm` **without**
  stopping → exit 0, unit `Loaded: not-found` but `Active: active (running)`,
  both binaries still running from deleted inodes, dangling
  `multi-user.target.wants/aranicy.service` symlink, and a freshly spawned
  `sleep` still got nice 5. **Filed as the blocker.**
- Reinstalled and repeated it once more with a clean transcript (steps A–F in
  the issue) before cleaning up the orphans by hand.

## 10. Config liveness

- Appended `bias_enabled = false`, `log_applied_rule = flurble`,
  `totally_unknown_key = 1` → restart → `WARN config: line 39: unknown key
  "bias_enabled"`, `line 40: invalid boolean "flurble"`, `line 41: unknown key`.
  Good validation; no `bias_*` off switch exists.
- `apply_ionice = false` + reload → new `ffmpeg` process: nice 10,
  `ionice -p` = `none: prio 0`. Key is live.
- Removed the junk keys afterwards.

## 11. Final state and cleanup

- Reinstalled both packages, `systemctl enable --now aranicy`, removed my
  `00-test.rules` and the junk keys, restarted with the shipped defaults.
  Final: `active`, `active`, `enabled`, `aranicy 0.3.0-1`,
  `aranicy-collector 0.3.0-1`.
- Idle cost measured over 20 s: daemon 86.8 ms → 97.9 ms, collector
  138.6 ms → 171.0 ms of CPU time. Negligible on this box.
- `sudo journalctl -u aranicy -o json -n 1` → valid structured JSON with
  `MESSAGE`, so `journalctl -o json` parsing works as I need it.

## 12. Second pass at the uninstall, after the first transcript was not clean

- I re-ran the removal three more times because the transcripts kept mixing
  runs (the daemon from a previous install was still active, so
  `enable --now` did not restart it and the rule file was never loaded).
  Dead end that cost a turn: `pgrep -f "/usr/bin/aranicy"` matched the
  `bash -c` wrapper of my own command, so the `sudo kill` loop killed my SSH
  session (exit 255) halfway through a removal. `pgrep -x aranicy` /
  `pgrep -x aranicy-collect` is what works.
- The clean, single-session run is the one quoted in issue ivy-1: fresh
  install, `enable --now`, `sleep` → nice 5, `pacman -R` (no stop, no disable)
  → `LOADSTATE=not-found ACTIVESTATE=active`, dangling enable symlink, pids
  1898/1897 still running at -5/-10 from deleted binaries, and a `sleep`
  started *after* the removal → nice 5.
- Then killed the orphans by hand and reinstalled so the box is left in a sane
  state.

## Environment facts that shaped the session

- No media tooling on the box: `ffmpeg`, `unrar`, `par2`, `par2create` all
  MISSING. I used `xz`, `openssl speed`, `tar`/`gzip` and a copy of `/bin/sleep`
  renamed to `ffmpeg` instead of inventing a fake transcoder.
- `sched_setattr` is blocked in this sandbox (EINVAL even for NULL), so the
  latency_nice path could not be exercised.
- sched_ext is `disabled` and no `scx_*` scheduler is installed on the box; the
  collector still produces samples, so the bias engine runs regardless.

## Small things noticed, not filed

- `aranicy dump rules | head -3` makes the binary panic
  (`failed printing to stdout: Broken pipe (os error 32)`) instead of exiting
  quietly on SIGPIPE. Cosmetic, but it prints a Rust panic banner on a normal
  unix idiom.
- One pid was logged twice for the same `applied rule` at the same millisecond.
- Repeated install/remove cycles accumulate `ananicy.conf.pacsave`,
  `ananicy.conf.pacsave.1`, … (standard pacman behaviour, but worth knowing).
- `systemctl is-enabled aranicy-collector` stays `disabled` forever; the
  collector only ever runs because the daemon pulls it in.

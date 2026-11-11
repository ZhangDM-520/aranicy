# session log — round 2, `su-homelab-r2`

All times are the machine's UTC clock. Everything below was run through
`./machine '<cmd>'` from the task directory. Package under test:
`aranicy 0.3.0-1` + `aranicy-collector 0.3.0-1`, kernel 7.2.11-arch1-1,
2 vCPU, 1958 MiB.

## Recon

- `uname -a` → `Linux su-homelab-r2 7.2.11-arch1-1 #1 SMP PREEMPT_DYNAMIC Fri, 16 Oct 2026 00:28:58 +0000 x86_64 GNU/Linux`
- `id` → `uid=1000(arch) gid=1000(arch) groups=1000(arch)`
- `ls -la ~/payload` → the two `.pkg.tar.zst` files, both `145 Nov 11 09:01`.
- `free -m` → 1958 MiB total; `nproc` → 2.
- `pacman -Qi aranicy aranicy-collector` → `error: package 'aranicy' was not found` (clean box, no previous install).
- `ls /usr/lib/systemd/system/ | grep -i aran` → nothing. `/etc/ananicy.d/` absent.
- Tool probe: `openssl`, `xz`, `gzip`, `tar`, `python3`, `systemd-analyze` present; `stress` and `bpftool` **MISSING**. So my background job is `openssl speed -multi 2` / `xz -9`, as in round 1.
- `bsdtar -xOf … .PKGINFO` → `pkgver = 0.3.0-1`, `builddate = 1794387575`, `backup = etc/ananicy.d/ananicy.conf`, `depend = aranicy-collector=0.3.0`.
- `bsdtar -xOf … .INSTALL` → **new**: both packages now have `pre_remove()` calling `systemctl --quiet disable --now …`. That is the round-1 uninstall blocker addressed.
- Package contents → the daemon now ships `usr/share/doc/aranicy/BENCHMARKS.md` and `COMPATIBILITY.md`; the collector ships its own `README.md` instead of a byte-identical copy.
- Read `ananicy.conf` (new key `bias_protect`, `bias_group.build`), both unit files, the README, `COMPATIBILITY.md`, and later the collector README.

## Install and first run

- 09:06 `sudo pacman -U --noconfirm` both packages → `INSTALL_EXIT=0`. pacman picked `aranicy-collector` first (dependency order).
- `sudo systemctl enable --now aranicy` → symlink created, `ENABLE_EXIT=0`; after 5 s both units `active`.
- Journal at first start already contains
  `WARN no rules loaded from /etc/ananicy.d: with an empty rule set only the learned bias changes priorities…`
  — the round-1 "silent zero rules" complaint is answered.
- Added `{"name": "sleep", "nice": 5}`, restarted → **surprise**: with rules present the startup log says nothing about how many were loaded (only `reload` prints a count). Verified via `systemctl status` too.
- Rule applies: `setsid sleep 700`, `ps -o ni=` → 5, `/proc/<pid>/stat` field 19 → 5.

## Round-1 retests

- `aranicy lint --config-dir ~/testrules` → `warning: config: config "…/ananicy.conf" not found; using defaults` / `lint: warnings found` / `exit=1`. Help text has been rewritten to say exit 1 covers warnings, so the docs and the binary now agree — but the `--config-dir`-without-config case is still a hard fail.
- `aranicy lint` (default dir) → `OK: 1 rules, 0 types, 0 cgroups`, `exit=0`.
- Docs: `sha256sum` of the two READMEs now differs; `find /usr/share/doc/...` shows `BENCHMARKS.md` + `COMPATIBILITY.md`; but all five relative links in the README (`docs/*.md`, `tools/storm/`, `banner.svg`, `LICENSE`) point at paths that are not installed.
- 09:08 openssl bias experiment: `journalctl | grep "bias changed"` → `bias=1`, then `2`, then `3`, one per 30 s. Polling `ps -eo ni,comm | grep openssl` every 10 s: the live nice **lagged the bias line by ~30 s** (first reading 0 while bias was already 1, then 2, then 3). Round 1 saw nice 2 within 3 s; this build is slower to actuate. Not filed — the documentation says "applied nice = clamp(rule_nice + bias)" and does not promise a latency.
- 09:10 `bias_enable = false` + restart → openssl stayed nice 0 for 46 s with an on-disk bias of 3, and a rule `{"name":"openssl","nice":5}` was applied as exactly 5. The round-1 request is satisfied.
- `systemctl reload` with `bias_enable` edited → `WARN restart-only keys ignored: bias_enable`. Good.

## Things I tried that did not go to plan (dead ends, kept as-is)

- **Corrupting `/var/lib/aranicy/bias-state` while the daemon is running does nothing.** First attempt: `echo "not json at all" > bias-state` then `systemctl restart` → the restart *saves* the live state over the garbage on the way down, and the new process reports `restored history entries=75`. I had to stop both units first to get a real test. Result with the units stopped: `WARN bias: cannot load /var/lib/aranicy/bias-state: expected ident at line 1 column 2`, daemon starts with empty history, `dump bias` reports the same error. That is reasonable behaviour, so nothing filed.
- **My first cgroup test was invalid.** I had two `.rules` files naming `xz` (one setting `cgroup`, a later one setting `nice`), and the later file replaced the earlier rule outright, so the cgroup field never existed. Re-ran with a single file. Lesson: one name per corpus, or the later file wins silently (`dump rules` shows only the survivor, `lint` counts one rule).
- **`/var/lib/aranicy/bias-state` came back with no non-zero entries** at the end of the session (everything had relaxed), so my final uninstall/reinstall capture showed `openssl nice=0`. I used the 09:17 capture instead, where the state demonstrably held `openssl bias 3`.
- I first read the `CPUQuota: 50 → cpu.max 100000 100000` result as an off-by-two bug; it is not: `cpu.max`'s period is 100000 µs, so this *is* 50 % of two CPUs. The only error is the document's "period 1 s".

## New work (different rules, different workloads, different failure modes)

- 09:11 **reload + drift on already-running processes**: started `openssl speed` with no rule, added `{"name":"openssl","nice":7}`, `systemctl reload` (→ `INFO reloaded 2 rules; bias reconfigured`), polled every 10 s: `09:11:49 … 0,0,0` … `09:12:39 7,7,7`. Convergence in ~53 s, consistent with `check_freq = 60`. This is exactly the Ansible "deploy then reload" path working.
- 09:12 **type bundles, null suppression, ionice**: `.types` `{"type":"BG","nice":11,"ioclass":"idle","ionice":7}` + `{"name":"xz","type":"BG"}` → `xz nice=11 ionice=idle`; `{"name":"openssl","type":"BG","nice":null}` → `nice 0, ionice idle`. Documented behaviour holds.
- `apply_nice = false` → `xz nice=0 ionice=idle` (the other gates still apply). Correct.
- 09:13 **cgroup/cpuset/sched**: `{"cgroup":"media","CPUQuota":50}` + rule with `cgroup`, `cpuset 0-1`, `sched batch` → `/proc/<pid>/cgroup` = `0::/media`, `/sys/fs/cgroup/media/cpu.max` = `100000 100000`, `taskset -pc` = `0,1`, `chrt -p` = `SCHED_BATCH`. Works.
- `CPUQuota: 200` → `WARN …:1: CPUQuota: 200 out of range 0..=100` on reload. Fine (100 = all CPUs).
- `CPUQuota: 100` → `cpu.max` = `200000 100000`. Correct.
- 09:17 **uninstall**: `pacman -R --noconfirm aranicy aranicy-collector` → exit 0, both units `inactive`+`not-found`, no processes, symlink gone, binaries gone, new `sleep` nice 0. Blocker fixed. Leftover `/var/lib/aranicy/bias-state` filed as `ivy-6`.
- 09:16 **reboot**: `sudo systemctl reboot`, SSH back after ~100 s (`uptime -p` → `up 1 minute`), both units `active`, `xz` nice 10 + ionice idle on a fresh exec, `journalctl -b` clean. Note `systemctl is-enabled aranicy-collector` → `disabled` (it is pulled in by `Requires=`, not by an enablement symlink) — worth knowing, not a defect.
- `sudo timeout 6 aranicy start` while the unit runs → `Error: another aranicy instance is already running (pid 3719); if that pid is stale, remove /run/aranicy.pid`, exit 1. Good single-writer guard.
- `systemctl stop aranicy-collector` → aranicy stops too (Requires propagates); `systemctl mask aranicy-collector` + `start aranicy` → `Failed to start aranicy.service: Unit aranicy-collector.service is masked.`, exit 1. Missing dependency is loud.
- `systemctl restart aranicy-collector` and `kill -9` of the collector → both come back, rules apply again. Recovery works.
- Bad collector socket: `collector_events_socket = /run/aranicy/wrong.sock` → `Error: no collector at /run/aranicy/wrong.sock after 5s: No such file or directory (os error 2) (is aranicy-collector.service running?)`, `status=1/FAILURE`, unit in `activating (auto-restart)`. Exactly what I asked for.
- `bias_protect = openssl` with a stored bias of 3 → live nice was the rule value only (4), no bias. So the protect list gates actuation, not just learning.
- 09:29 **symlinked rules dir**: `/etc/ananicy.d -> /srv/ananicy-rules` → rules load normally. (My Ansible pattern works.)
- `systemd-analyze verify` both units → `VERIFY_EXIT=0`; `systemd-analyze security aranicy.service` → `Overall exposure level … 6.2 MEDIUM`.
- `journalctl -u aranicy -b -o json` → parses; `_PID`, `PRIORITY=6`, `SYSLOG_IDENTIFIER=aranicy`, `MESSAGE`. Logs are JSON-parseable, though everything is in the `MESSAGE` string.
- Idle cost over 30 s: daemon 2 tick, collector 5 tick (100 ticks = 1 core-second) → well under 1 % each.
- 2000-exec storm (`for i in $(seq 1 2000); do /bin/true; done`): 733 ms wall, daemon 2 ticks (20 ms ≈ 2.7 % of one core), collector 4 ticks (≈ 5.5 %). In the ballpark of the README's 5 % budget for the daemon.
- 09:24 **long-running low-priority job**: `xz -9 -c` on a 191 MB file with `nice 19` + `ioclass idle` held `nice=19 ionice=idle` for 100 s of polling, then finished. No drift fight. But **surprise**: the bias engine kept stepping `xz` after the process was gone (`bias changed name=xz bias=2` at 09:26:31 and `bias=3` at 09:27:01 while `pgrep xz` was empty at 09:26:38). Not filed: the documentation says the attack "want" survives deadband excursions, and a name that no longer runs is exactly what the aging law is for. I did not measure how long the entry survives. suspicion: the step is evaluated when the 60 s window closes, so samples taken before the exit are still inside it; I did not verify that from the outside.
- 09:29 removing a rule file + `reload` → `reloaded 1 rules`, `dump rules` no longer lists it. Reload is a real re-read, not a merge.
- Kernel-thread audit (`/proc/*/stat`, flag `0x200000`): 94 kernel threads, the only non-zero nices are the kernel's own defaults (`kworker/R-*` at −20, `ksmd` 5, `khugepaged` 19); no bias entry exists for any `kworker`/`kthread`. No evidence the daemon ever touched one.

## New findings filed

- `issues/ivy-6-uninstall-keeps-learned-bias-state.md` → reinstall re-applies pre-uninstall bias.
- `issues/ivy-7-undefined-cgroup-is-a-silent-noop.md` → rule referencing an undefined cgroup loads, lints green, does nothing, logs nothing.
- `issues/ivy-8-unusable-cpuset-passes-lint-and-logs-applied.md` → `turbo-cores` / `99-100` are dropped silently and the log still says `applied rule`.
- `issues/ivy-9-lint-green-on-empty-corpus.md` → `OK: 0 rules`, exit 0, despite the help text.
- `issues/ivy-10-match-test-strips-the-kworker-prefix.md` → `kworker/0:1-events` reported as `0:1-events`.
- `issues/ivy-11-doc-cpu-quota-period-is-wrong.md` → documented 1 s `cpu.max` period, actual 100 ms.

## State left on the machine

Installed, enabled and running (both units `active`), with the package's own
`/etc/ananicy.d/ananicy.conf` (no `bias_enable`, no `bias_protect` override, the
`collector_events_socket` line I added during the wrong-socket test is gone) and
no rule files:

```
$ sudo ls -la /etc/ananicy.d/
total 4
drwxr-xr-x 1 root root   24 Nov 11 09:30 .
drwxr-xr-x 1 root root 2032 Nov 11 09:30 ..
-rw-r--r-- 1 root root 1694 Nov 11 08:59 ananicy.conf
$ aranicy dump rules
{
  "exact": [],
  "regex": []
}
```

Workload files under `~/work/`, scratch corpora under `~/testrules/`,
`~/emptyrules/`, `~/badrules/`, `~/r2rules/`, a copy of the pre-test bias state
at `/root/bias-state.bak`.

## Would I put this on a box I have to maintain for years

Yes, with one condition: I would keep the rule corpus minimal and the package
pinned, because the failure modes I found this round are all "the rule is
there, the daemon is green, and nothing happened" rather than "it broke". The
engineering that matters to me is now correct: `pacman -R` stops and disables
everything and a process spawned afterwards is left alone; a reboot comes back
with the rules applying; a missing or masked collector is a hard, named failure
instead of an active unit doing nothing; a second instance refuses to start; a
rule-less startup warns; `bias_enable = false` really stops the learned axis
from touching anything, including state restored from a previous installation;
and nothing I could measure moved a kernel thread, `sshd`, `systemd-journald` or
`systemd-resolved`. What still has to change before I trust it unattended is
visibility on the rule side: a rule whose `cgroup` is not defined, whose
`cpuset` cannot be honoured, or a corpus that deployed zero rules must be a
refusal or at least a warning at load and at apply, not a `lint` green light
and a silent pass — and the install must not resurrect priorities I removed the
package to be rid of. Those are all small, and three of them are one code path
(the "field could not be applied" message that today does not exist).

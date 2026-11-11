# Session log — ren, round 3 (adversarial + differential)

Machine: su-dev-r3 (disposable), Arch rolling, kernel 7.2.11-arch1-1, 2 vCPU,
1958 MiB, no display. Package pair under test: `aranicy 0.3.0-1` +
`aranicy-collector 0.3.0-1`. Working dir is the only directory I look at on this
computer; the machine is reached only through `./machine`.

## Entry conditions

```
$ ./machine 'uname -a; nproc; free -m; ls -la ~'
Linux su-dev-r3 7.2.11-arch1-1 #1 SMP PREEMPT_DYNAMIC Fri, 16 Oct 2026 00:28:58 +0000 x86_64 GNU/Linux
2
               total        used        free      shared  buff/cache   available
Mem:            1958         378        1472           4         334        1580
Swap:            511           0         511
...
drwxr-xr-x 1 arch arch 156 Nov 11 09:54 payload
drwxr-xr-x 1 arch arch 156 Nov 11 09:54 previous-build
```

`pacman -Q aranicy aranicy-collector` → `error: package 'aranicy' was not found`
/ `error: package 'aranicy-collector' was not found`; both units `not-found`.
So the machine starts clean, which is what an install/remove/reinstall test
needs.

## Package contents: current vs previous build

`bsdtar -tf` on `~/payload/aranicy-0.3.0-1-x86_64.pkg.tar.zst` lists
`etc/ananicy.d/ananicy.conf`, `usr/bin/aranicy`,
`usr/lib/systemd/system/aranicy.service`,
`usr/share/doc/aranicy/{README.md,docs/BENCHMARKS.md,docs/COMPATIBILITY.md}`,
`usr/share/licenses/aranicy/LICENSE`. The collector package lists
`usr/bin/aranicy-collectord`, its unit and `README.md`.

Surprise: the first `bsdtar -xqf … -C cur` returned exit 0, printed nothing and
created *empty* directories (`sha256sum: cur/usr/bin/aranicy: No such file or
directory`). Re-running with `-v` extracted everything. Recorded as a machine
oddity, not a package finding; every extraction in this log was re-verified by
listing the tree.

`diff -r cur prev` (same version number, both 0.3.0-1):

```
Binary files cur/usr/bin/aranicy and prev/usr/bin/aranicy differ
Binary files ccur/usr/bin/aranicy-collectord and cprev/usr/bin/aranicy-collectord differ
Only in prev/usr/share/doc/aranicy: BENCHMARKS.md
Only in prev/usr/share/doc/aranicy: COMPATIBILITY.md
Only in cur/usr/share/doc/aranicy: docs
diff -r cur/usr/share/doc/aranicy/README.md prev/.../README.md
16,21c16,17
< clean-room rewrite with a hard performance budget for **what the package
< installs — the daemon *and* its collector**: measured **12.3% of one core at
< 2 000 execs/s** (6.1 + 6.2) on the benchmark host, against ananicy-cpp's
< 13.7% for one process. ...
---
> clean-room rewrite with a hard performance budget: **≤ 5% of one core at
> 2 000 execs/s**.
```

and the `.INSTALL` of the current build gained a `post_remove` hook:

```
    # The learned history is derived state, not configuration: leaving it
    # behind means a later reinstall resurrects biases for a machine that has
    # since changed (and the nice values they were applied to are gone).
    rm -f /var/lib/aranicy/bias-state 2>/dev/null || true
    rmdir /var/lib/aranicy 2>/dev/null || true
```

which the previous build does not have. (`diff cur/.INSTALL prev/.INSTALL`.)

`COMPATIBILITY.md` differs too: the current one adds a **Trust model** section
and changes the cgroup sentence from "period 1 s" to "period fixed at the
kernel's 100 ms".

## Install (current build)

```
$ sudo pacman -U --noconfirm ~/payload/aranicy-collector-0.3.0-1-x86_64.pkg.tar.zst ~/payload/aranicy-0.3.0-1-x86_64.pkg.tar.zst
installing aranicy-collector...
installing aranicy...
(1/2) Reloading system manager configuration...
```

No prompts, no post-install action, nothing enabled. Good for a
config-management rollout.

Machine has **no sched_ext scheduler**:

```
$ ls /sys/kernel/sched_ext
enable_seq hotplug_seq nr_rejected state switch_all
$ cat /sys/kernel/sched_ext/root/ops
cat: /sys/kernel/sched_ext/root/ops: No such file or directory
$ systemctl is-active scx_lavd
inactive
```

so the whole learned-bias axis here runs on the collector's own tracepoints
(plain EEVDF), not on `scx_lavd` samples — unlike the setup the previous
round's reports describe.

## CLI surface

```
$ aranicy --help
Auto-nice daemon (eBPF exec events)

Usage: aranicy [OPTIONS] [COMMAND]

Commands:
  start       Run the daemon (root required)
  dump        Print loaded state as JSON (works unprivileged)
  lint        Validate rules and report problems without running
  match-test  Show which rule a process would match, without applying anything
  help        Print this message or the help of the given subcommand(s)

Options:
      --config-dir <CONFIG_DIR>  Rules directory (env: ANANICY_CPP_CONFDIR / ANANICY_RS_CONFDIR)
      --config <CONFIG>          Daemon config file (env: ANANICY_CPP_CONF / ANANICY_RS_CONF)
      --reload                   Reload the running daemon's config AND rules (SIGHUP-equivalent)
  -h, --help                     Print help
  -V, --version                  Print version
$ aranicy --version
aranicy 0.3.0
$ aranicy-collectord --help
...
  run    Serve per-task samples on a UNIX socket until terminated
  probe  Attach, print rows to stdout, and exit (diagnostics; needs root)
```

`aranicy` with no subcommand tries to start the daemon (`Error: aranicy start
requires root (effective uid 1000) …`), which is how ananicy-cpp behaved; noted
only because it is surprising for a `--help`-first user.

## Baseline start (fresh install, empty corpus)

```
$ sudo systemctl enable --now aranicy; sleep 6
$ systemctl is-active aranicy aranicy-collector
active
active
$ sudo ls -la /run/aranicy
srw-------  1 root root 0 collector-events.sock
srwxr-xr-x  1 root root 0 collector-stats.sock
$ sudo journalctl -u aranicy -u aranicy-collector --no-pager -o cat | tail
Started aranicy collector - scheduler-agnostic BPF task statistics.
Starting aranicy - auto-nice daemon...
WARN no rules loaded from /etc/ananicy.d: with an empty rule set only the learned bias changes priorities. The package ships no rule corpus - point ANANICY_CPP_CONFDIR at yours, or write files ending in .rules
WARN kernel does not support latency_nice (sched_setattr LATENCY_NICE); latency_nice policy will be skipped
INFO aranicy_collectord: collector serving sched_samples + exec events socket=/run/aranicy/collector-stats.sock events_socket=/run/aranicy/collector-events.sock cadence_ms=500
INFO stats sampler live prev=degraded: starting detail=bound SchedSample via sched_samples at /run/aranicy/collector-stats.sock
$ aranicy dump rules
{
  "exact": [],
  "regex": []
}
```

`aranicy lint` on the packaged corpus:

```
$ aranicy lint
warning: 0 rules loaded from /etc/ananicy.d: this corpus will apply nothing
lint: warnings found
rc=1
```

## Chronological log (UTC times are the machine's clock; `$` = command run through `./machine`)

| t | command → outcome |
|---|---|
| 09:54 | `$ uname -a; nproc; free -m; ls -la ~` → 7.2.11-arch1-1, 2, 1958 MiB; `payload/` + `previous-build/` present, 2 packages each. |
| 09:54 | `$ pacman -Q aranicy aranicy-collector` → `error: package 'aranicy' was not found` / same for collector; `systemctl is-enabled` → `not-found`. Clean machine. |
| 09:54 | `$ sudo pacman -Qip ~/payload/…zst` vs `~/previous-build/…zst` → same version string 0.3.0-1, `Build Date: Wed Nov 11 09:52:22 2026` vs `08:59:35 2026`. |
| 09:54 | `$ bsdtar -tf … aranicy-0.3.0-1…zst` → 13 entries incl. `etc/ananicy.d/ananicy.conf`, unit, README, `docs/{BENCHMARKS,COMPATIBILITY}.md`. |
| 09:54 | `$ bsdtar -xqf … -C cur` → **surprise**: exit 0, no output, but the tree was empty (`sha256sum: cur/usr/bin/aranicy: No such file or directory`). Re-ran with `-v`: all files extracted. Machine/fs oddity, logged, not a finding. |
| 09:55 | `$ diff -r cur prev` → binaries differ, `docs/` move, README perf paragraph rewritten, `.INSTALL` gained `post_remove` bias-state deletion. `sha256sum` of both binaries recorded in `differential.md`. |
| 09:55 | `$ sudo pacman -U --noconfirm payload/collector payload/aranicy` → installed both, nothing enabled, no prompt. |
| 09:56 | `$ aranicy --help / --version / aranicy-collectord --help` → start/dump/lint/match-test; 0.3.0; run/probe/help. |
| 09:56 | `$ ls /sys/kernel/sched_ext; cat /sys/kernel/sched_ext/root/ops; systemctl is-active scx_lavd` → no root/ops, `inactive`. **No sched_ext scheduler on this box** — every bias number below comes from the collector's own tracepoints. Noted, not a bug. |
| 09:56 | `$ sudo systemctl enable --now aranicy; sleep 6` → active/active, two sockets in `/run/aranicy`, `WARN no rules loaded from /etc/ananicy.d`, `WARN kernel does not support latency_nice`, `INFO stats sampler live`. |
| 09:56 | `$ aranicy lint` (packaged corpus) → `0 rules loaded … this corpus will apply nothing`, `lint: warnings found`, rc=1. |
| 09:56 | Built `~/adv/{empty,trunc,unknown,range,regex,mixed,conf,half,typed}` and linted each → file:line messages, rc=1, no stack traces. |
| 09:56 | `$ aranicy --config-dir ~/adv/good lint` (valid rules, no `ananicy.conf` in the dir) → `config … not found; using defaults` + `lint: warnings found`, rc=1. **First CI trap.** With the conf copied in → `OK: 2 rules…`, rc=0. |
| 09:56 | `$ aranicy --config-dir ~/adv/good match-test renapp / renregex / nosuch` → exact, regex, `no rule matches`, rc 0/0/1. |
| 09:56 | `$ sudo cp … /etc/ananicy.d/zz-test.rules; sudo aranicy --reload` → `reloaded 2 rules; bias reconfigured`; `dump rules` shows both; `~/renapp` → nice 7, `ionice` idle; `dump proc` agrees. |
| 09:57 | `$ sudo kill -9 $(pgrep -x aranicy-collect)` → collector restarted in 5 s, **daemon also restarted** (Requires= propagation), `WARN stats sampler degraded … Broken pipe`, history restored (70), rules live again. 5 s unmanaged window, no failure. |
| 09:57 | `$ aranicy dump bias | head -40` → state file with 70→109 entries; `journalctl | grep -c 'bias changed'` → 0 at that point. |
| 09:57 | Reload with a truncated rule line → `zz-test.rules:2: no JSON object span`, `reloaded 1 rules`, good rule kept. |
| 09:58 | Reload with `apply_nice = fal` → `config: line 2: invalid boolean "fal"`, rules still applied (renapp 7). |
| 09:58 | `exec -a renapp /bin/sleep` → comm `sleep`, nice 0. `prctl(PR_SET_NAME,"renapp")` → comm `renapp`, nice 0. `cp /bin/sleep ~/renapp_copy` → nice 0. Executable identity holds; no rule borrowing. |
| 09:58 | `for i in $(seq 1 2000); do ~/renapp 0; done` → 0.774 s, daemon/collector pids unchanged, journal 31 lines → 31 lines, next long-lived `renapp` niced 7. No crash, no spam. |
| 09:59 | cgroup test: `zz-cg.cgroups` `CPUQuota 50` + rule → `/proc/3820/cgroup` = `0::/rencg`, `/sys/fs/cgroup/rencg` cpu.max = `100000 100000`. **Note**: my check `grep cgroup /proc/$P/cgroup` was wrong (the file's *content* never contains "cgroup"); corrected to `cat` from here on. |
| 10:00 | 225 s bias run: `renbulk`, `sshd9` (protected prefix), `zzunprot` (`yes` copies) → all three learned 0→3 (`bias changed … cause=Evidence`), `sshd9` never applied (nice 0), the other two niced 2-3. Daemon stayed at −5 the whole time. |
| 10:02 | `$ sudo pacman -R --noconfirm aranicy aranicy-collector` (with bias-state present) → `/var/lib/aranicy` gone, `/run/aranicy*` gone, unit + enable symlink gone, no pacsave (config was unmodified). **Current build cleans up.** |
| 10:02 | `$ sudo systemctl reboot` (nothing installed) → after boot: no processes, no `/run/aranicy`, no `/var/lib/aranicy`, `0 loaded units listed` in `systemctl --failed`. Removal leaves nothing behind. (`pgrep -af aranicy` matched my own `bash -c` line — self-match artefact, ignored.) |
| 10:04 | Reinstall current build, `enable --now`, 70 s load with a `yes` copy + `renapp`, `bias-state` 3377 B/22 entries → **reboot mid-work**. |
| 10:06 | After reboot (current): both active, sockets back, `restored history entries=22`, rules loaded, `renapp` nice 7. **New**: `WARN cgroup "rencg" not created: Permission denied (os error 13)`. |
| 10:06 | `stat /sys/fs/cgroup/rencg` → ctime 10:06:05.287602, i.e. the directory *was* created 0.5 ms before the "not created" warning. `cpu.max = max 100000` → **no quota**. Filed ren-13. |
| 10:08 | `zz-cg2` reload → new cgroup created fine; cpu.max for rencg became 100000/100000 — so a reload fixes what the boot start broke. |
| 10:08 | `systemd-run` with the unit's sandbox props doing `mkdir /sys/fs/cgroup/renprobe2` → succeeded, so the sandbox is not the blocker (evidence only, no conclusion about cause). |
| 10:09 | Reboot #2 with the cgroup rule present → same warning, `cpu.max=max 100000`, process moved in (`0::/rencg`). Reproduced. |
| 10:11 | Reboot #3 → same. `cgroup.subtree_control` = `cpu memory pids` after boot. |
| 10:12 | Half-written config at daemon start: first attempt failed because I wrote `/etc/.../ananicy.conf.tmp` **without sudo** (`Permission denied` — my mistake, retried later). |
| 10:12 | Uninstall current (cgroup present) → `/sys/fs/cgroup/rencg` remains with cpu.max 100000/100000; everything else clean. |
| 10:13 | `$ sudo pacman -U … ~/previous-build/*` then `BSET_REMOVE=0 bash ~/bset.sh` → previous build: lint 0-rules corpus exits **0** (`OK: 0 rules…`), everything else identical, cgroup cpu.max `100000 100000`. |
| 10:14 | Removed `/sys/fs/cgroup/rencg` by hand, reload → previous build recreates it correctly with the quota. So the cgroup code is the same; only the docs differ. |
| 10:15 | Previous build, 225 s bias run → identical: `sshd9` learned to 3, not applied; `renbulk`/`zzunprot` niced 3; daemon stays −5. |
| 10:18 | Previous build: half-written config at start → `config: line 4: invalid boolean "tru"`, rules applied. |
| 10:18 | Reboot #4 (previous build) → same cgroup warning, `cpu.max=max 100000`, `restored history entries=58`. |
| 10:19 | `$ sudo pacman -R` previous build → **`/var/lib/aranicy/bias-state` (12 745 B) left behind**. This is the fix in the current build. |
| 10:20 | Reinstall current; half-written config at start → `config: line 4: invalid boolean "tru"` (same). |
| 10:20 | Second `sudo timeout 6 /usr/bin/aranicy-collectord run` → starts, logs "collector serving …", exit 124. Socket inode 2037→ unchanged at first sight (I misread; re-tested later with a before/after pair). |
| 10:20 | `sudo timeout 6 /usr/bin/aranicy start` → `Error: another aranicy instance is already running (pid 986); if that pid is stale, remove /run/aranicy.pid`. Good. |
| 10:20 | `sudo -u nobody socat …` → `socat: command not found` (dead end; used python socket for the probe later). |
| 10:20 | `cat /proc/sys/kernel/pid_max` → 4194304 → **pid recycling cannot be forced on this box**; reported as not tested instead of guessed. |
| 10:20 | `kill -9` the daemon → **restart loop**: `Error: no collector at /run/aranicy/collector-events.sock after 5s: Connection refused … (is aranicy-collector.service running?)`, unit stuck `activating`, 4 restarts. |
| 10:22 | `sudo systemctl restart aranicy` → still fails. `sudo systemctl restart aranicy-collector` → both recover. Element for ren-12. |
| 10:22 | Ran the clean experiment: healthy pair, `kill -9` the daemon only → recovered in 10 s, `client connected`, rules applied. So the loop above was caused by the second collector, not by `kill -9`. |
| 10:23 | `sudo stat -c "%n inode=%i"` before/after `timeout 8 aranicy-collectord run` → 2117 → 2149. Python connect probe → `ConnectionRefusedError [Errno 111]` on both sockets while `systemctl is-active aranicy-collector` = `active`. Then `kill -9` daemon → the restart loop again. Reproduced deliberately. |
| 10:26 | `sudo timeout 30 aranicy-collectord probe` → prints rows (`prio=23 … demand=1017`), pair stays healthy. Documented diagnostic is safe. |
| 10:26 | Zero-byte `ananicy.conf`, restart → defaults used, no complaint, rules still applied. |
| 10:26 | `aranicy --config-dir /etc/ananicy.d lint` → `OK: 3 rules, 0 types, 1 cgroups`, rc 0. |
| 10:29 | `aranicy --config-dir ~/adv/good --config /etc/ananicy.d/ananicy.conf lint` (no conf in the dir) → `OK: 2 rules…`, rc 0 — the workaround for ren-15. |
| 10:29 | `sudo mv /etc/ananicy.d/ananicy.conf …; restart` → daemon stays active with `WARN config: … not found; using defaults`; rules applied. Restored afterwards. |
| 10:30 | Final state: `aranicy 0.3.0-1` + `aranicy-collector 0.3.0-1` installed, both `active`. |

Dead ends worth recording, in order of how much time they cost:

1. `bsdtar -xqf` reporting success with an empty tree (twice before I believed it).
2. `grep cgroup /proc/<pid>/cgroup` — the pattern never appears in that file's
   *contents*, so it printed nothing and made me think the process had not been
   moved. Two cgroup conclusions were nearly wrong because of it; `cat` fixed it.
3. Reading the second-collector damage as a `kill -9` bug. It took one clean
   re-run (healthy pair, kill only the daemon) to separate them.
4. `cp /bin/sleep ~/rencgapp` failing `Text file busy` because the previous test
   process was still alive — the check set now kills by hand before copying.
5. Writing to `/etc` without `sudo` (permission denied), and `socat` not being
   installed. Both retried with the right tool.
6. A load-test shell that kept running after I had read its output; I stopped it
   and re-collected the journal lines directly rather than trusting a partial
   read.

## Verdict

I would roll this out to a colleague's machine — with two exceptions I would fix
first. The parts that matter to me work: rules are data with file:line errors
from both `lint` and the daemon, the exit code is honest on malformed input, a
reload with a broken line keeps the previous rules and says how many it loaded,
executable identity defeats `exec -a` and `prctl` impersonation, non-root,
masked-unit, `kill -9` and removal-then-reboot all behave, and removal now takes
the learned history with it (the previous build did not, and that was the single
worst thing about it for a config-management rollout). What would stop me is
exactly the class of failure this round was looking for: a second
`aranicy-collectord run` — the obvious thing to type while debugging — silently
takes the socket path and leaves a machine where the collector is "active" and
the daemon cannot start, with an error pointing at the wrong service
(`ren-12`); and a cold boot leaves a configured `CPUQuota` unapplied, so the
cap that was supposed to be my safety net is silently absent while processes are
happily moved into the uncapped cgroup (`ren-13`). Both are silent in the sense
that nothing tells me the policy is not what I wrote. Before a colleague gets
this I want the collector to refuse a second instance (or the daemon to retry
into a live one), the cgroup quota verified after creation at boot with a
message that names the missing value, and the two lint exit-code traps gone so
`aranicy lint` can be a CI gate: a `.types`-only or empty-but-valid corpus exits
1 today (`ren-14`), and a rules directory without an `ananicy.conf` beside it
exits 1 even though the rules are fine (`ren-15`). The rest is documentation —
the README points at two files that moved (`ren-16`) and quotes two different
results for the same benchmark (`ren-17`) — which costs trust but not
correctness, and would be trivial to fix in the same pass.

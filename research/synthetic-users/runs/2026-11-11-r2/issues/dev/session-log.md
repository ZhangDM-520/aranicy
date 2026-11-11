# Session log — round 2 (chronological, untidied)

All machine commands went through `./machine '<cmd>'` (ssh to su-dev-r2).
Times are the machine's clock (UTC). Local steps are marked "local:".
Nothing was installed except the two payload packages.

## Inbox

- local: read `card.md` (round-2 brief), `sysinfo.txt`, `machine` wrapper,
  `previous-reports/` (8 files, ren-1 … ren-8), `issues/` (empty).
- local: `card.md` says the dev box has "a Rust toolchain and Node runtime".
  It does not:
  `command -v cargo rustc gcc node npm make` → no output. Also no `which`
  (so `which …` fails with "command not found"). Environment note, not a bug;
  I used `python3` fork/exec work as the bulk workload instead, as the brief
  forbids installing.

## 09:05–09:06 install

- `uname -a` → `Linux su-dev-r2 7.2.11-arch1-1 #1 SMP PREEMPT_DYNAMIC …`
- `ls -la ~/payload` → aranicy-0.3.0-1 (1539682 B), aranicy-collector-0.3.0-1 (1171776 B)
- `/etc/ananicy.d` absent, `systemctl is-active aranicy aranicy-collector` → inactive/inactive
- `.PKGINFO` → `builddate = 1794387575` → `2026-11-11T08:59:35+00:00`
  (i.e. built 6 min before the VM was provisioned; same version string as round 1,
  different build — recorded sha256 of both packages in `retest.md`).
- `sudo pacman -U --noconfirm …` → exit 0, 6.24 MiB.
- `pacman -Ql` → **surprise**: `BENCHMARKS.md` and `COMPATIBILITY.md` now ship
  under `/usr/share/doc/aranicy/` (ren-4 was partly about their absence).
- `aranicy --version` → `aranicy 0.3.0`; `aranicy-collectord --version` → 0.3.0.
- README re-read: name now resolved from `/proc/<pid>/exe` first (ren-3's
  complaint), and `ananicy.conf` now has a `bias_protect` key (ren-7's).
- Units: `aranicy.service` `Requires=aranicy-collector.service`, `Nice=-5`,
  `CapabilityBoundingSet=CAP_SYS_NICE … CAP_SYS_PTRACE`; collector `Nice=-10`.
- `sudo systemctl enable --now aranicy` → both active; pids 1168/1169.

## 09:07 retesting the old reports

- ren-1 (a) glob `cc1*` → `warning: …:1: name "cc1*" is matched exactly; patterns need name_regex`, exit 1. **fixed**
- ren-1 (b) empty name → `warning: …:1: empty name can never match a process`, exit 1. **fixed**
- ren-1 (d) `a.json` → `warning: …/a.json:0: skipped: only *.rules /*.types /*.cgroups are loaded`, exit 1. **fixed** (nit: `:0:`)
- ren-1 (e) duplicate name in two files → `OK: 1 rules` exit 0, later file wins silently. **still**
- ren-1 runtime: rule → undefined `cgroup` → `OK: 3 rules, 0 types, 0 cgroups` exit 0; process not moved; no journal line. **still**
- ren-2 rules-only dir → exit 1 on `config … not found; using defaults`. **still**;
  `lint --help` now says "Exit 1 on any finding" (help/behaviour now agree).
  `--config /dev/null` works as a workaround (undocumented).
- ren-3: `exec -a argvdemo /bin/sleep 20` → nice 0; `exec -a innocuous ~/argvdemo 20`
  → nice 14. **fixed** (exe-based matching).
- ren-7 first probe: rule `{"name":"systemd","nice":12}` loaded, `match-test` shows
  nice 12, `/proc/1/stat` nice stays 0. PID 1 not moved.
- ren-4: `ls /usr/share/doc/aranicy/` → BENCHMARKS.md COMPATIBILITY.md README.md;
  README still links `docs/…` and `contrib/install.sh` (absent). `.rules` schema now documented.
- ren-8: `dump rules` still has no source file/line.

## 09:07–09:10 benchmarks (first dead end)

- Wrote `bench/storm.py` (workers fork in a list comprehension) → printed `0`
  and forked grandchildren; counts lost. Dead end, 2 wasted runs.
- Rewrote as `bench/storm1.py` (single worker, `posix_spawn` + `waitpid`) and
  `bench/stormn.sh` (N workers in parallel). Verified rate: 10 848 execs in 3 s
  ≈ 3 616/s single worker, ≈ 5 500/s at 2 workers (round 1's generator did 1 644/s,
  so my generator is not the same instrument — noted in retest.md).
- `measure.sh` reads `/proc/<pid>/stat` fields 14+15 for both daemons around the load.
- Results: idle 0.0/0.1 %, 50/s 0.2/0.4, 200/s 0.3/1.0, 395/s 0.9/2.0,
  2 000/s 2.2–3.7 / 5.2–5.8, burst 5 500/s 9.1/13.6. Round 1 was 12 % at 200/s.

## 09:09–09:11 **surprise**: the daemon nices itself

- After the storm: `journalctl | grep bias` → `bias changed name=aranicy bias=1/2/3 class=Bulk`.
- `awk '{print $19}' /proc/1169/stat` → `nice=2`, then `3`; `systemctl show -p Nice aranicy` → `Nice=-5`.
- The collector (`comm=aranicy-collect`) stays at -10.
- Filed later as ren-10.

## 09:11–09:15 learned-axis A/B (PID 1 vs a systemd-named process)

- `cp /usr/bin/python3 ~/zzbulk`, `cp /usr/bin/python3 ~/systemd`, both `while True: pass`.
- 09:12:12 / 09:12:42 / 09:13:12 → `bias changed name=systemd bias=1/2/3 class=Bulk`;
  `ps` showed the `systemd` process at nice 0 the whole time (never applied).
  `zzbulk` got `bias=3` and `nice=2`.
- `/proc/1/stat` nice 0 throughout.
- `dump bias` entries: `systemd bias=3 conf=0.96`, `dbus-broker bias=-1`,
  `systemd-journal bias=-1`, `systemd-homed bias=-1` — all names on the shipped
  `bias_protect` list. → ren-9.

## 09:16–09:19 **dead end**: my own pkill

- `pkill -f "/home/arch/zzbulk"` matched my own remote `bash -c` command line and
  killed the session (exit 255). Switched to `kill <pid>` / `pgrep -x`.
- Restarted the A/B with `~/sshd2` (prefix of `sshd` in the list) + `~/zzctl`
  (control): sshd2 → `bias 1/2/3` in the journal, nice 0; zzctl → `bias 3`, later
  nice 3 (confirmed via `aranicy-collectord probe`: `comm=zzctl prio=23`).

## 09:18 **surprise**: restarts and Requires=

- `systemctl restart aranicy-collector` → daemon pid 1169 → 385664 (and again
  396493 → 719785). `systemctl stop aranicy-collector` → both inactive, both gone.
- Collector README claims the daemon "reports a degraded sampler and keeps retrying".
  → ren-11.

## 09:18–09:20 lint/corpus edge cases

- Five lines, four bad: lint prints all four with `file:line` (JSON error, `nice` range,
  unknown key, bad `sched`) → `lint: warnings found` exit 1. Good.
- `name_regex` valid → matched `cc1plus`, not `xcc1` (anchored by my own `^`).
- Invalid regex → parse error with caret, exit 1.
- Non-ASCII name `café` → loads and matches.
- CRLF + trailing garbage → loads (documented parity behaviour).
- `CPUQuota` works; `cpu_quota_pct` now **rejected** ("unknown key") — round 1's
  accepted spelling; behaviour flip, documented key is the one that works.
- `cpuset` in a cgroup definition → "unknown key" (same as round 1).
- Duplicate type (`aaa`=3, `zzz`=17) and duplicate cgroup (10, 90) → silent, later file wins.
- Rule duplicate with different fields → later object replaces outright, earlier
  fields lost (`ioclass: idle` disappeared).
- Recursive `rules/sub/deep.rules` → loaded. `.rules.bak` → skipped with warning.
- `x3d_mode = amd|foo2` → "invalid x3d_mode value"; `cache|frequency` → "accepted for
  compatibility but not applied". `cmdlines` → "accepted but ignored" at lint.

## 09:19–09:21 runtime cgroup + a real build-shaped workload

- Undefined `cgroup` → process not moved, no journal line; define the cgroup →
  `0::/zzgroup` immediately. (Confirms ren-1's runtime half.)
- No compiler on the box, so the "build" is `bench/bulk.py`: 3 workers each
  fork/exec a 400 000-iteration `python3 -c` loop for 130 s; `bench/latprobe.py`
  measures fork+exec→exit latency every 50 ms.
- Idle probe p50 1.22 ms / p95 2.46; under 3-worker load p50 2.63 / p95 4.73 / max 13.19.

## 09:22–09:26 persona scenario with rules (the actual job)

- Rules: `zzbuild` nice 19, `zzedit` nice -5. Build runs as `~/zzbuild`, probe as `~/zzedit`.
- With rules: build at nice 19 (4 procs), probe at nice -5.
  - aranicy active: `n=868 p50=0.78ms p95=4.44ms p99=5.82ms max=12.17ms`
  - `systemctl stop aranicy` then same load: `n=852 p50=2.22ms p95=4.58ms p99=6.12ms max=8.47ms`
  - i.e. p50 ~3× better, p95/p99 within noise of the saturated baseline. The
    "stays responsive" claim holds only at the median on 2 vCPU.
- `systemctl reload aranicy` before/after `dump bias`: 66 → 66 entries, identical
  `(name, bias)` list. Reload does not drop history.
- Live reload with a bad line in `/etc/ananicy.d`: warns `90-mixed.rules:2: nice: 99 out of range`
  and still loads `zzok`. Good.

## 09:26–09:27 removal (ren-6)

- `sudo pacman -Rns --noconfirm aranicy aranicy-collector` → both **stopped**;
  `systemctl is-active` → inactive/inactive; `/run/aranicy` gone; enable symlink gone;
  `/var/lib/aranicy/bias-state` (12 kB) left.
- Cause: both packages now ship `.INSTALL` with `pre_remove() { systemctl --quiet disable --now … }`.
  **fixed**.
- Reinstall + `systemctl start aranicy` → new pids (396492/396493), daemon nice **-5**
  (clean baseline for the self-nice repro).

## 09:29–09:31 self-nice reproduced from a clean start

- t0 `nice=-5`; 150 s `storm1.py`; t+30 `-5`, t+60 **3**, t+90/120/150 `3`, unit still `-5`.
  No new `name=aranicy` journal line: the restored history already held `bias=3`
  (the ramp was logged on the very first start, 09:09–09:10).

## 09:31–09:39 protected-name learning, second attempt (inconclusive)

- `~/sshd9` + later `~/zzbg1` + `~/zzbg2`, 170 s, three busy loops: **no**
  `bias changed` line and no entries for any of them. The engine was demonstrably
  classifying at 09:12–09:25 and not at 09:36; I did not instrument why
  (`WARN observation budget truncated the sample window truncated=403 rows=8192`
  is the only unusual line, at 09:28:52). Recorded as "could not determine" in ren-9.
- At 09:40 the A/B is still valid for the *actuation* half: `zzctl` nice 3,
  `sshd9` nice 0, identical demand (`probe`: prio=23 vs prio=20, demand 1024/1023).
- Session journal accounting: 28 `bias changed` lines, 14 of them for names on
  the `bias_protect` list.

## 09:40 collector restart/stop, clean capture

- restart: daemon 396493 → 719785; stop: both inactive, both none; start again → both active.

## Not done / left behind

- I did not test `kill -9` on the collector (systemd restarts it in 5 s), did not
  test `latency_nice`/`rtprio` application beyond the load-time warning
  (`kernel does not support latency_nice`), and did not measure rule-apply latency.
- Test artefacts left on the machine (disposable): `~/bench/*`, `~/zzbulk`,
  `~/zzctl`, `~/zzbuild`, `~/zzedit`, `~/sshd2`, `~/sshd9`, `~/zzbg1`, `~/zzbg2`,
  `~/zzcg`, `~/zzcl`, `~/argvdemo`, `~/repro*`, `~/t1`, `~/t2`, and unowned rule
  files in `/etc/ananicy.d/{rules,cgroups}` (one of them deliberately broken:
  `90-mixed.rules` line 2, `nice: 99`).

## Would I roll this out?

Yes, but not to a colleague's machine yet — round 1 would have been a flat "no",
and this build earns a conditional yes. The things I actually need are mostly
fixed: rules now match the executable rather than a name the process chooses
(ren-3), `lint` refuses to call a corpus green when a rule cannot apply (ren-1
a/b/d), the documented rule schema ships with the package (ren-4), `pacman -Rns`
really removes the daemon (ren-6), and PID 1 is no longer walked up the nice
scale (ren-7). I also got the workflow I wanted: a build at nice 19 and an editor
at −5, with fork/exec latency at p50 0.78 ms under load versus 2.22 ms with the
daemon stopped, and the build still finished. What stops me is trust, not
throughput: the daemon quietly raises its own nice from the unit's −5 to +3 under
load while `systemctl` still reports −5 (`ren-10`) — that is exactly the
"something is adjusting priorities and I cannot tell what" feeling I came from;
`bias_protect` is documented as "names the learned bias must never move" yet
those names are still classified, logged and relaxed, spending half the global
step budget on no-ops (`ren-9`); a duplicate rule name is still silent and
destructive (`nice`/`ioclass` dropped, no provenance, `ren-1c`, `ren-8`), which
is the single biggest obstacle to version-controlling a corpus and reviewing a
diff; and a rules-only checkout still cannot lint green without copying a daemon
config or passing an undocumented `--config /dev/null` (`ren-2`). Fix those
four — make the bias leave the daemon alone (or make the unit honest), make
`bias_protect` actually skip, warn on duplicate rule names and name the source
file in `dump rules`/`match-test`, and give `lint` a supported "no config"
mode — and I would hand it to a colleague with my rules repo the same day.

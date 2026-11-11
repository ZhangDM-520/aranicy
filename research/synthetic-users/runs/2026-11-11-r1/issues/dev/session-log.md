# session log — ren, round 1 (aranicy 0.3.0-1)

Machine: Arch Linux cloud image, kernel 7.2.11-arch1-1, 2 vCPU, 1958 MiB, no
display. Everything below ran through `./machine '<cmd>'`. Times are the
machine's UTC clock.

## Setup and first contact

- `./machine 'uname -r; id; nproc; free -m; sha256sum ~/payload/*'` → 7.2.11-arch1-1,
  uid 1000 `arch`, `nproc` 2. Package hashes match `sysinfo.txt`
  (`467d6dcf…` / `ba4cf0a1…`).
- `pacman -Qip` on both packages → `aranicy` provides `ananicy-cpp`, conflicts
  `ananicy-cpp`; `aranicy-collector` depends on `libbpf`.
- `tar --zstd -tf` both packages → no rules, no types, no `contrib/`, no `docs/`.
  Only `etc/ananicy.d/ananicy.conf`, two units, two binaries, README + LICENSE.
  First surprise: a package that `Provides: ananicy-cpp` ships no rules and no
  types at all.
- `sudo pacman -U --noconfirm …` → clean install, exit 0.
- `cat /usr/share/doc/aranicy/README.md` → performance budget claim (≤5% of one
  core at 2000 execs/s), links to `docs/COMPATIBILITY.md`, `docs/BENCHMARKS.md`,
  `contrib/install.sh` — none of which are installed. Notes for later.

## Toolchain surprise (environment fault, not a bug)

- `command -v python3 perl bc jq cargo rustc node strace` → only `python3`.
  **`cargo`, `rustc` and `node` are absent**, although the brief says a Rust
  toolchain and a Node runtime are present. `~/.cargo`, `~/.rustup`,
  `pacman -Qs rust nodejs` → nothing. I did not install anything (rule 7); I used
  Python as the bulk workload and spawner instead. `jq` and `strace` are also
  missing, so I switched to `python3 -c` for JSON and gave up on syscall tracing.

## Service and CLI

- `sudo systemctl enable --now aranicy` → both units active (`Requires=` pulls the
  collector).
- `sudo journalctl -u aranicy -u aranicy-collector` → without `sudo` I get "not
  seen by this user"; the daemon's log is INFO-level sparse (one sampler line at
  start). Surprise: `journalctl` as `arch` silently shows nothing for the unit, so
  my first "no output" reading was a permissions artefact.
- `aranicy --help`, `… {start,dump,lint,match-test} --help` → 4 verbs; `match-test`
  help says the name is `basename(argv[0])` — recorded, became ren-3.
- `dump rules|types|cgroups|proc|autogroup|bias` → all emit JSON, exit 0. Empty
  rules corpus out of the box.

## Rule parsing probes

- `aranicy --config-dir ~/rc lint` on a dir with no conf → `warning: config: …
  not found; using defaults`, `lint: warnings found`, **exit 1** (ren-2).
- malformed JSON `{ "name": "x", }` → `r.rules:1: invalid JSON in span: trailing
  comma at line 1 column 16`, exit 1. Good diagnostics, file:line ✓.
- unknown key `"nise"` → `unknown key "nise" ignored`, exit 1 ✓.
- duplicate name in one file → silent, last wins, `OK: 1 rules`, exit 0.
- `"name": "cc1*"` → `OK`, stored as a literal exact name, `match-test cc1plus`
  → no match. Dead end at first: I assumed a glob would be an error; it is not.
- non-ASCII `café` → accepted verbatim ✓.
- `nice: 99` → `nice: 99 out of range -20..=19`, exit 1 ✓.
- `"nice": "5"` → `nice: expected integer, got string` ✓.
- `name_regex: "cc1("` → multi-line regex parse error, correct ✓.
- pretty-printed multi-line rule → four `no JSON object span` warnings, one per
  line. Confusing but exit 1. Format is one object per line; not documented.
- `#` comment line → accepted. CRLF → accepted. No trailing newline → accepted.
- `[{…}]` (one-element array) → accepted as 1 rule; `[{…},{…}]` → `trailing
  characters at line 1 column 27`. Inconsistent, noted but not filed.
- file named `a.json` / `a.rules.bak` → ignored entirely, `OK: 0 rules`, exit 0
  (ren-1).
- duplicate name across two files → later file wins silently, no mention of
  either file (ren-1 + ren-8).
- `"type": "Heavy"` with no type pack → `unknown type … rule keeps its own
  fields`, `match-test` → `"policy": {}` — the canonical ananicy rule shape is a
  no-op (ren-4).
- Type definition shape by trial: `{"type": "Heavy", "nice": 19}` works,
  `{"name": "Heavy", "nice": 19}` does not. Undocumented.
- cgroup definition by trial: `{"cgroup": "zzgroup", "cpu_quota_pct": null}`
  works; `"cpuset"` inside a cgroup definition is rejected as an unknown key.
- config parser probes (unknown key, bad loglevel, bad check_freq, duplicate key,
  line without `=`) → every one reports `config: line 38: …`, exit 1 ✓.
- `--config-dir` and `ANANICY_CPP_CONFDIR`/`ANANICY_RS_CONFDIR` both honoured ✓
  (good for config management).

## Behaviour / apply tests

- rule `{"name": "demoproc", "nice": 10}` + copy of `/bin/sleep` named
  `demoproc` → `/proc/<pid>/stat` field 19 = 10 ✓.
- reload with a *broken* rules file → warns with file:line and keeps the 4 valid
  rules, existing processes keep their nice (`demoproc nice=10`) ✓. This is the
  one thing I was most worried about and it behaves.
- one process with `nice`, `ioclass`, `sched`, `oom_score_adj`, `cpuset` →
  nice 8 ✓, `ionice -p` → `idle` ✓, `policy 3` (SCHED_BATCH) ✓,
  `/proc/<pid>/oom_score_adj` → 500 ✓, `Cpus_allowed_list: 1` ✓.
  Dead end: I first looked for `/proc/<pid>/ionice` (does not exist on Linux) and
  for `oom_score_adj` in `/proc/<pid>/status` (this kernel's `status` does not
  list it, though `/proc/<pid>/oom_score_adj` does). Two false "not applied"
  readings before I used the right interfaces.
- `cgroup: "zzgroup"` with no definition → rule counted, process *not* moved,
  **no log line at all**. After defining `zzgroup` the same rule moves the
  process to `0::/zzgroup`. So the failure was silent (ren-1).
- `bias` state across `systemctl reload` and `systemctl restart` → entries=59,
  threshold identical before/after; restart logs
  `INFO bias: restored history entries=59` ✓.
- corrupt `bias-state` (daemon stopped first) → warns `expected value at line 1
  column 26`, starts with empty history, does not overwrite the bad file ✓;
  `version: 99` → `refusing to downgrade`, same behaviour ✓.
  Dead end: my first attempt at this test corrupted the file *while the daemon was
  running*, and the old daemon rewrote a valid snapshot on shutdown, so all three
  cases "restored 70 entries" and looked like nothing happened. Had to stop the
  service first.
- `aranicy --reload` with the daemon stopped → `Error: no running aranicy daemon
  found (/run/aranicy.pid is missing)`, exit 1 ✓.
- `dump proc` fidelity: 127 rows vs 127 real pids, every `nice` equal to
  `/proc/<pid>/stat` field 19, 0 mismatches ✓.

## Matching identity (ren-3)

- `~/zzsleep` (rule nice 12) → 12. `exec -a innocent ~/zzsleep` → 0 (comm still
  `zzsleep`). `exec -a argvdemo /bin/sleep` → 14 (comm `sleep`).
  Matching is on `basename(argv[0])`, so `exec -a` both evades and fabricates.
  README says `cmdline` is used *because* `exec -a` spoofs `comm` — inverted.

## Workload / responsiveness

- No cargo/node, so: copies of `/usr/bin/python3` named `bigbuild` and `latprobe`;
  `spin.py` burns a core, `probe.py` measures 200 × `sleep(5 ms)` actual elapsed.
- Saturated (2 spinners at nice 0, probe at nice 0): p50 5.09 ms, p99 8.5–12.7 ms,
  max 8.8–19.2 ms.
- With rules (bigbuild 19 + `ioclass idle`, latprobe −5): p99 5.97–8.90 ms, max
  7.3–11.3 ms. Real but modest improvement on 2 vCPUs; no build to time because
  the Rust toolchain is absent.
- Storm (`storm.py`, posix_spawn `/bin/true`): 1 worker ≈ 3 700 exec/s, 2 ≈ 1 650,
  4 ≈ 4 000. Daemon 3.4–5.1%, collector 13.4–14.7% of one core; idle 0.20% /
  0.75%. Paced 50/200/395 exec/s: 1.3/2.6, 4.3/7.7, 7.2/9.7 %. Per-exec cost is
  not monotonic, so I only claim the aggregate vs the documented budget (ren-5).
- Dead end: my first smoke run of `storm.py` printed 912 exec/s; later runs of the
  same script printed 3 731. The spawner competes with itself and with the daemon
  on 2 vCPUs; I stopped treating single runs as stable and reported ranges.

## Learned bias

- 3-minute run with a pinned `bigbuild` + `latprobe`: 43 entries, 38 `unknown`,
  no non-zero bias. Too short — discarded as evidence, not filed.
- 8-minute run with two `spinonly` spinners (no rule): `true` (from the earlier
  storm) got +3, `spinonly` stayed 0 at the 8-minute mark, 52 entries. I was about
  to file "the budget goes to a no-op name", then noticed at 08:41–08:42 that
  `spinonly` did move (+1 then +2) once the other wants were served. Dropped it;
  it was a timing artefact, not a defect.
- Verified bias is applied at exec: a copy of `/bin/sleep` named `true` (bias +3)
  starts at nice 3 with no rule ✓.
- **PID 1 was demoted.** Again at the very end of the session, with every rule
  file deleted (`dump rules` → empty): `bias changed name=systemd bias=1 prev=0`,
  then `bias=2 prev=1`, `/proc/1/stat` nice=2. No rules, still demoting init. `bias changed name=systemd bias=1|2|3` at 08:20:49 →
  08:21:59, `systemd-journal` and `systemd-userwor` too, and `dbus-broker` raised
  to −1. `/proc/1/stat` nice 1, `systemctl show -p Nice systemd` → 1,
  `dump proc` shows it. No rule, no config key, no exemption (ren-7).

## Removal (ren-6)

- `sudo pacman -Rns --noconfirm aranicy aranicy-collector` → clean removal, exit 0,
  binaries gone from `/usr/bin/…`.
- `pgrep -af "/usr/bin/aranicy"` → **1797 aranicy-collectord run** and
  **115963 aranicy start** still alive; `systemctl is-active aranicy-collector`
  → `active`. Both daemons keep working from deleted inodes.
- Proof of residual behaviour: `exec -a argvdemo /bin/sleep 10` after removal →
  nice 14 (rule still in memory).
- Left behind: dangling `aranicy.service` symlink in
  `/etc/systemd/system/multi-user.target.wants/`, `/var/lib/aranicy/bias-state`
  (8 990 B), two sockets in `/run/aranicy`, and my `/etc/ananicy.d/rules`.
- Reinstall + `systemctl start` → same pids (1797/115963); the `start` was a no-op
  because the units were still active. Only `systemctl restart` replaced them.

## Surprises / friction worth recording

- `systemctl restart` ×6 within a minute hit `StartLimitBurst=5` and the unit went
  `failed (Result: start-limit-hit)` — my 8-minute observation run silently lost
  the daemon and burned 3 minutes. Recovered with
  `sudo systemctl reset-failed aranicy aranicy-collector`. Not filed: it is
  systemd doing what the unit file asks, but it does mean iterating on rules with
  `systemctl restart` can wedge the unit and the error is in
  `systemctl status`, not in the CLI.
- `pgrep -a aranicy-collectord` warns "pattern that searches for process name
  longer than 15 characters will result in zero matches" — my own foot-gun.
- `pgrep -f` matched my own `bash -c` wrapper several times, which produced one
  bogus "nice=0" measurement before I switched to `$!`.
- The rules-format discovery cost most of the session: extension filter, one
  object per line, type-file shape, cgroup-file shape, and the fact that `type`
  silently resolves to an empty policy. None of it is documented in the package.
- The daemon logs almost nothing at INFO: start, sampler bound, one line per
  reload ("reloaded N rules"), one line per bias step. There is no "applied rule
  X to pid Y" unless `log_applied_rule = true`, and no per-process explanation.
- `observation budget truncated the sample window truncated=2 rows=8192` appeared
  once in the journal under the storm — the 8 192-row cap is visible in logs,
  which is good.

## Files written

- `issues/ren-1-lint-passes-rules-that-cannot-apply.md` (major, behaviour)
- `issues/ren-2-lint-exits-1-without-daemon-config.md` (minor, behaviour)
- `issues/ren-3-rule-match-uses-argv0.md` (major, behaviour)
- `issues/ren-4-no-shipped-rule-reference.md` (minor, docs)
- `issues/ren-5-overhead-above-documented-budget.md` (minor, docs)
- `issues/ren-6-removal-leaves-daemons-running.md` (major, packaging)
- `issues/ren-7-bias-demotes-pid1.md` (major, safety)
- `issues/ren-8-no-rule-provenance.md` (minor, enhancement)

# session log — kaz, round 1 (su-gamer-r1)

Machine: su-gamer-r1, Arch rolling, kernel 7.2.11-arch1-1, 2 vCPU, 1958 MiB.
All commands were run through `./machine '<cmd>'` from this directory.

## 1. first contact

- `./machine 'uname -r; nproc; free -m | head -3; ls -la ~/payload'`
  → `7.2.11-arch1-1`, `2`, 1958 MiB total. Two packages present in `~/payload`.
- `./machine 'tar --zstd -tf aranicy-0.3.0-1-x86_64.pkg.tar.zst'` (and the collector)
  → daemon: `/usr/bin/aranicy`, `/etc/ananicy.d/ananicy.conf`,
    `/usr/lib/systemd/system/aranicy.service`, `/usr/share/doc/aranicy/README.md`, LICENSE.
  → collector: `/usr/bin/aranicy-collectord`, `.../aranicy-collector.service`, README, LICENSE.

## 2. install

- `./machine 'cd ~/payload && sudo pacman -U --noconfirm aranicy-0.3.0-1-x86_64.pkg.tar.zst aranicy-collector-0.3.0-1-x86_64.pkg.tar.zst'`
  → exit 0, no scriptlet, no message about enabling the service.
- Surprise: after install `systemctl is-active aranicy` → `inactive` (checked implicitly
  by the status output in the next step). Nothing in the package tells you to enable it.

## 3. docs

- `./machine 'cat /usr/share/doc/aranicy/README.md'`
  → README's "Build" and "Install" sections are source-tree instructions
    (`cargo build --release`, `./contrib/install.sh`). Neither exists on a package install.
    The only package-relevant sentence is "After install the daemon lives in systemd ...
    `systemctl status aranicy`, `systemctl reload aranicy`".
- `./machine 'cat /usr/share/doc/aranicy-collector/README.md'`
  → byte-identical to the daemon README (same 4694 bytes); says nothing about the collector.
- `./machine 'ls /usr/share/doc/aranicy/docs'` → no such file (exit 2).
  Every relative link in both READMEs (`docs/BENCHMARKS.md`, `docs/COMPATIBILITY.md`,
  `tools/storm/`, `banner.svg`) is dead; LICENSE is in /usr/share/licenses, not next to the README.

## 4. start

- `./machine 'sudo systemctl enable --now aranicy'` → exit 0
  (my own guess; not in the packaged docs). Both units active:
  `aranicy.service` and `aranicy-collector.service` (pulled in by `Requires=`).
- Journal: `WARN kernel does not support latency_nice (sched_setattr LATENCY_NICE);
  latency_nice policy will be skipped` — environment note for this kernel.
- Journal: `collector serving sched_samples + exec events ... cadence_ms=500`,
  `stats sampler live ... bound SchedSample` → the collector really is feeding the sampler.

## 5. CLI surface

- `aranicy --help` → `start`, `dump {rules,types,cgroups,proc,autogroup,bias}`, `lint`,
  `match-test`, `--reload`. Good: lint/match-test/dump are exactly what I want as a user.
- `aranicy-collectord --help` → `run`, `probe`.
- `man aranicy` → `bash: man: command not found` (no man in the image). My first check
  (`ls /usr/share/man/man* | grep -i aran`) was useless — it matched openssl's
  `guarantee` pages. Re-checked with `find /usr/share/man -iname "*aranicy*"` → nothing,
  so no man page is shipped.

## 6. first rule

- Wrote `/etc/ananicy.d/50-kaz.rules`:
  `{"name": "kazgame", "nice": -5}` / `{"name": "openssl", "nice": 19, "ioclass": "idle"}`
- `aranicy lint` → `OK: 2 rules, 0 types, 0 cgroups`, exit 0.
- `aranicy match-test kazgame` → exact match, `"nice": -5`.
- Started the daemon *before* this rule existed, so:
  `cp /usr/bin/sleep /usr/local/bin/kazgame; /usr/local/bin/kazgame 60 &`
  → `NI 0` — the running daemon had not seen the rule (expected, no reload yet).
- `sudo systemctl reload aranicy` → journal `reloaded 2 rules; bias reconfigured`;
  new `kazgame` → `NI -5`. **This works.** A rule + reload + newly started process is
  applied without reboot, and `dump proc` shows the resulting nice.

## 7. visibility

- `log_applied_rule = true` in /etc/ananicy.d/ananicy.conf + reload:
  journal `INFO applied rule rule="kazgame" pid=1304 bias=0 route="exec"`.
  That is the only per-process status line, and it is off by default ("noisy").
- `dump proc` shows only name/pid/nice — no indication of which rule or how much is bias.
- `dump bias` reads the on-disk snapshot, not the daemon ("may be up to ~60 s newer").

## 8. validation probes (scratch dir ~/t1, `--config-dir ~/t1`)

- unknown rule key → `warning: ...:1: unknown key "bias" ignored` + `lint: warnings found`, exit 1.
- rule with `type: Game` and no types file → `warning: unknown type "Game" referenced;
  rule keeps its own fields`, exit 1.
- truncated JSON → `warning: ...:1: no JSON object span (missing '{' or '}')`, exit 1.
- object with neither name/name_regex/type/cgroup → `warning: ...:1: unclassified object...`, exit 1.
- typo'd key (`nise`) → `warning: ... unknown key "nise" ignored`, exit 1. Good, it does not
  silently accept a mistyped key.
- `{"type":"Game","nice":-7}` + `{"name":"kazgame","type":"Game"}` → policy nice -7;
  adding `"nice": -5` to the rule → nice -5. Precedence is sane (rule beats type).
- `aranicy start` as user → `Error: aranicy start requires root (effective uid 1000); ...`
  exit 1 (my first attempt printed EXIT=0 because I piped through `head` — my error).
- `aranicy --reload` as user → `Error: failed to signal aranicy (pid 1126):
  Operation not permitted (os error 1)`, exit 1.

## 9. does it apply the actions it claims

- `/usr/bin/openssl speed -seconds 40 rsa2048` with `{"name":"openssl","nice":19,"ioclass":"idle"}`
  → `NI 19`, `ionice -p` → `idle`. nice + ionice both applied.

## 10. drift corrector

- kazgame running at -5, `sudo renice -n 10 -p <pid>` → `NI 10`,
  then at t+30 s `NI -5` again (journal `applied rule ... route="drift"`).
  So a hand `renice` is reverted within one `check_freq` (60 s). Consistent with the
  config comment but there is no way to tell it "leave this one alone".

## 11. live rule change on an already-running process

- kazgame running at -5; edited the rule to `nice: 0`; `systemctl reload`.
  → t+25 s `NI -5`, t+50 s `NI 1`.
  Not 0: the drift pass applies `clamp(rule_nice + bias)` and bias was already +1
  (journal `bias changed name=kazgame bias=1 prev=0 class=Bulk cause=Evidence`,
  then `applied rule ... bias=1 route="drift"`).
  So a rule's number is silently not authoritative.

## 12. the surprise: my SSH session gets demoted

- `exec -a kazgame sleep 8` → journal `applied rule rule="kazgame" pid=6885 bias=0`
  (argv[0] is used for matching, as match-test --help says — so the name is spoofable,
  which matches the README's "exec -a spoofing" note).
- While looking at that, `ps` showed my own session at nice **3**:
  `sshd-session: arch [postauth] NI 3`, `sshd-session: arch@notty NI 3`, `bash NI 3`.
  Journal: `bias changed name=sshd-session bias=3 prev=2 class=Bulk cause=Evidence`,
  `applied rule rule="sshd-session" pid=6893 bias=3 route="exec"`.
  No rule for sshd-session or bash exists — the learned bias is applied to names with
  no rule at all.
- `dump bias` also had `systemd-journal` bias 3, `sshd-auth` bias 3, `aranicy` bias 3,
  `systemd` bias 2, `openssl` 3.

## 13. is there an off switch?

- `{"name":"kazgame","nice":-5,"bias":false}` → `unknown key "bias" ignored`.
  No per-rule/per-name opt-out exists.
- Appended `bias_enable = false` and `bogus_key_xyz = 1` to the conf + reload:
  → `WARN config: line 40: unknown key "bogus_key_xyz"` (good) and
    `WARN restart-only keys ignored: bias_enable` — so `bias_enable` exists, is
    restart-only, and is documented nowhere (not in the shipped conf, not in the README).
- After `systemctl restart aranicy` with `bias_enable = false`: new `sshd-session` NI 0,
  `bash` NI 0. The demotion stops. This is the only escape I found.

## 14. clean re-run of the demotion

- Moved `/var/lib/aranicy/bias-state` aside, removed the two extra conf lines,
  `systemctl start aranicy`, started a CPU-bound stand-in for the game
  (`/usr/local/bin/kazgame speed -seconds 420 rsa2048`, i.e. openssl renamed).
- (result appended below)

## 15. load

- 20 s of three parallel `/bin/true` loops: daemon 1.68 % of one core,
  collector 5.25 % of one core (jiffies from /proc/<pid>/stat, getconf CLK_TCK).
  No journal errors during the storm. Not a benchmark, just "it does not spin".

## 14b. clean re-run result

- `ps -o pid,ni,comm -C kazgame` at 08:27:35 → `7056  -2 kazgame`, rule says `-5`.
  Journal: `bias changed name=kazgame bias=1/2/3 ... class=Bulk cause=Evidence`, then
  `applied rule rule="kazgame" pid=7056 bias=2/3 route="drift"`. The rule's number is
  not what the process gets.
- Same session: `python3` has no rule, `dump bias` says bias 3, a fresh `python3`
  ran at `NI 3`, journal `applied rule rule="python3" pid=7199 bias=3 route="exec"`.
  That reproduces every time.

## 16. the thing that made me sit up: PID 1

- `ps -o pid,ni,comm -p 1,205` → `1   2 systemd`, `205   2 systemd-journal`.
  Neither has a rule. Journal (first session):
  `applied rule rule="systemd" pid=1 bias=2 route="drift"` and the same for
  `systemd-journald` pid 205. `dump proc` right after install had shown pid 1 at nice 0.
- reload / stop / start all leave PID 1 at 2. I reniced PID 1 by hand to 0; it stuck
  (bias had decayed to 0). `systemd-journald` is still at 2 — I only fixed the one I
  had noticed.

## 17. trying to reproduce the PID-1 part on demand (failed)

- Wiped `/var/lib/aranicy/bias-state` again, restarted, ran ~7 min with the CPU hog.
  `systemd` stayed `responsive` bias 0; no new `pid=1` applies. Filed as
  non-reproducible-on-demand with the durable effect documented.
- Then ran 2 × a 5-minute churn loop (`bash -c "ls /proc"` ×200, repeatedly) to try to
  shift the classification. Still no `pid=1` apply. But it did produce:
  `WARN observation budget truncated the sample window truncated=57 rows=8192`,
  and bias 3 for `ps`, `pgrep`, `pacman`, `python3` and several `systemd-*-generator`
  names — all with no rule.
- Also: 20 s of three parallel `/bin/true` loops earlier cost the daemon 1.68 % of a
  core and the collector 5.25 % — no error lines. So "it does not spin" holds here.

## 18. dead ends / notes

- `sed -i ... /etc/ananicy.conf` → no such file; the config is
  `/etc/ananicy.d/ananicy.conf` (my mistake, not the package's).
- `aranicy start | head` made me record `EXIT=0` for a root error; re-ran without the
  pipe → `EXIT=1`. My error.
- `pkill -x kazgame` from the user fails on the root-owned copy — noise, not a finding.
- The 08:33 daemon restart in the journal is mine (`disable --now` + reinstall +
  `enable --now`), not a crash.

## 19. state I left the machine in

- Both packages installed, `aranicy` + `aranicy-collector` enabled and running,
  `/etc/ananicy.d/50-kaz.rules` with the two test rules, `log_applied_rule = true`,
  `/usr/local/bin/kazgame` (copy of openssl) present, `/var/lib/aranicy/bias-state`
  populated, `systemd-journald` still at nice 2.

## 20. before filing

- Restored `log_applied_rule = false` in /etc/ananicy.d/ananicy.conf (I had turned it on
  to see the apply lines).
- Re-ran every command I quote in an issue, exactly as written, so the pasted outputs are
  what that command produces on this machine now. Two corrections: `ls /usr/bin | grep -i
  "contrib\|install.sh"` really is empty with `EXIT=1`, and `find /usr/share/man -iname
  "*aranicy*"` is empty — my earlier `ls /usr/share/man/man* | grep -i aran` "hit" was
  openssl's guarantee(3) pages, not a daemon page.
- Left `systemd-journald` at nice 2 on purpose: it is the cleanest demonstration that
  nothing in the package notices or undoes a change it made.

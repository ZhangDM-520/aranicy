# round 1 — triage

Gate applied to every filed issue, in order:

1. **Reproduce** — re-run the cited commands in the same VM, same payload.
   Not reproducible ⇒ `unproven`, no fix, recorded with the evidence.
2. **Intended?** — behaviour documented and deliberate ⇒ documentation fix,
   scored separately from a code defect.
3. **Duplicate** — same root cause across personas ⇒ one row, weight = number
   of independent personas.
4. **Fix** — reproduce, red-first test, fix, falsify the test, then
   `cargo test --workspace` + `cargo clippy --workspace --all-targets`.
5. **Enhancement** — recorded, not fixed, for the end-of-experiment digest.

25 issues filed by 4 personas (5 + 8 + 5 + 7), 28 artefacts with the session
logs. Leak audit: 0 hits (`harness/audit.sh runs/2026-11-11-r1`).

## Verified independently (my own commands, same VM)

| claim | how I checked | result |
|---|---|---|
| uninstall leaves the daemon running | `pacman -R aranicy` in `su-gamer-r1`, then `systemctl is-active` / `pgrep` / `ls /usr/bin/aranicy` | **reproduced**: active, pid 7525, binary gone, unit `not-found` |
| bias drives PID 1 | `awk '{print $19}' /proc/1/stat` in `su-dev-r1` mid-session | **reproduced**: nice 3 with an empty rule set |
| lint exit code vs its help | `aranicy lint` on a rule with an unknown `type` on the host build | **reproduced**: warnings → exit 1, help said "errors" |
| collector ships the daemon's README | `contrib/PKGBUILD` installs `README.md` into both packages | **reproduced** by inspection (byte-identical in the smoke VM) |
| zero rules on a fresh install | smoke VM, `aranicy lint` → `OK: 0 rules` | **reproduced** |

## Table

`W` = independent personas that filed it (duplicate weight).

| # | persona | title | sev | repro | W | disposition |
|---|---|---|---|---|---|---|
| kaz-1 | gamer | daemon re-niced PID 1 and journald, never reverted | major | yes | 2 (ren-7, dana-2) | **fixed** — PID 1 is never a plan target; `bias_protect` list for core services |
| kaz-2 | gamer | learned bias overrides the rule's nice; nices unruled processes; no per-name opt-out | major | yes | 2 (ivy-5) | **partial** — `bias_protect` gives per-name control and the startup log names the bias; `clamp(rule+bias)` is intended and now documented. Off-by-default bias = enhancement |
| kaz-3 | gamer | package install leaves the daemon stopped; docs are source-tree only | major | yes | 1 | **fixed** — packaged quick start in README, installed docs |
| kaz-4 | gamer | collector ships the daemon's README; cross-references dead | minor | yes | 3 (ivy-4, ren-4, dana-6) | **fixed** — real `collector/README.md`; COMPATIBILITY + BENCHMARKS now ship |
| kaz-5 | gamer | no per-process status / "why" | minor | yes | 0 | **enhancement** — recorded |
| ren-1 | developer | `lint` green for rules that can never apply, and for files never read | major | yes | 0 | **fixed** (empty name, glob name, `.json`/`.bak` skipped files) / **open part**: duplicate name across files still silent |
| ren-2 | developer | `lint` exits 1 on a rules-only checkout (missing daemon config), same severity as a broken rule | minor | yes | 0 | **documentation fixed** — help text now says errors *or* warnings; the missing-config warning stays |
| ren-3 | developer | rules matched on `basename(argv[0])`: `exec -a` fabricates and evades a match | major | yes | 2 (dana-3) | **fixed (identity)** — `/proc/<pid>/exe` first, argv0 then comm as fallbacks; divergence documented. See dana-3 for what this does *not* fix |
| ren-4 | developer | no shipped rule/type reference, three dead links | minor | yes | 1 | **fixed** — rule format and type/cgroup semantics ship in COMPATIBILITY.md |
| ren-5 | developer | documented ≤5%-of-a-core budget not reachable (17-19% on 2 vCPU) | minor | yes | 1 (dana-5) | **documentation fixed** — claim now states its conditions and how to measure your own |
| ren-6 | developer | `pacman -Rns` leaves both daemons running | major | yes | 2 (ivy-1) | **fixed** — `.install` scriptlets stop and disable on removal |
| ren-7 | developer | with no rules, bias demotes PID 1 and core services | major | yes | 3 (kaz-1, dana-2) | **fixed** — same as kaz-1 |
| ren-8 | developer | no rule provenance in any command | minor | yes | 0 | **enhancement** — recorded |
| ivy-1 | homelab | `pacman -R` leaves both daemons running and still nicing | blocker | yes | 2 (ren-6) | **fixed** — same as ren-6 |
| ivy-2 | homelab | empty rule set is silently green in the journal and `systemctl status` | major | yes | 1 | **fixed** — startup warning names the directory and the reason |
| ivy-3 | homelab | `lint` exits 1 on warnings although its help promises errors only | minor | yes | 1 (ren-2) | **fixed** — help text |
| ivy-4 | homelab | installed documentation is the wrong document for one package, links dead | minor | yes | 3 | **fixed** — same as kaz-4 |
| ivy-5 | homelab | learned bias changes unruled processes; no way to control it | minor | yes | 2 (kaz-2) | **fixed** — `bias_protect` + documented `bias_enable` |
| dana-1 | skeptic | a *rule* naming `systemd` makes the drift pass renice PID 1 | blocker | yes | 1 | **fixed** — PID 1 is never a target, rule or no rule |
| dana-2 | skeptic | bias re-applied to PID 1 across restarts | blocker | yes | 3 | **fixed** — same as kaz-1, plus the unwind path |
| dana-3 | skeptic | `argv[0]` is a confused deputy: unprivileged code obtained `SCHED_FIFO` 10 from the root daemon | major | yes | 2 (ren-3) | **open** — identity is now the executable, but a user can still name a *file* after a rule. Gating privileged properties (RT, negative nice) on the target's unit/uid is a design decision, not a patch; carried to the digest as the round-1 security item |
| dana-4 | skeptic | the classifier gave a shell and a CPU spinner the same verdict (1011 vs 3.9, both `Bulk` +3) | major | yes | 0 | **open** — matches the recorded ADR 0001 finding (score ranks interaction rate). Needs the two-axis `lean` classifier, not a constant |
| dana-5 | skeptic | measured 9.6% of a core at 1 732 execs/s vs the documented ≤5% | minor | yes | 1 | **documentation fixed** — same as ren-5 |
| dana-6 | skeptic | README claims (`scx_stats` binding, perf) unsubstantiated by anything in the package | minor | yes | 2 | **fixed** — COMPATIBILITY + BENCHMARKS ship; performance paragraph qualified |

## Round-1 outcome

* filed 25, duplicated to 18 root causes, independently reproduced 5 (the
  blocker/major ones that drove the fixes), 14 **fixed**, 1 **partially fixed**
  (ren-1, kaz-2), 3 **open** (dana-3 security, dana-4 classifier, ren-1
  duplicate names), 3 **enhancement** (kaz-5, ren-8, kaz-2's opt-in bias).
* fix loop: every fix carries a red-first test; each new guard was
  mutation-probed (guard removed ⇒ the test fails) before the green run.

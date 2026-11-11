# Synthetic outside-user review — 2026-11-11

Four personas who had never seen the source used the shipped package in
kernel-isolated VMs for three rounds, filing issues; the project triaged,
fixed, and handed a new build back **without telling them what changed**.
Harness: `research/synthetic-users/`; the reusable procedure is the skill
`synthetic-user-review`.

The question was not "does it work". It was: *what does a competent stranger
hit first, and does a fix survive someone who was not told it was fixed?*

## Method

- payload = `git archive` of a commit → `makepkg` → two packages + manifest
  (2.28 MB). No source, no docs tree, no git history, no session state.
- environment = one QEMU/KVM VM per persona per round, own kernel
  (`7.2.11-arch1-1`), 2 vCPU / 1958 MiB, no display, destroyed afterwards.
  A container was rejected: eBPF is not namespaced and `nice` applies to
  resolved pids, so a privileged container would collect and renice the
  *host's* processes.
- personas = gamer, Rust/Node developer, headless homelab admin, and a
  hostile performance reviewer. Cards are capability briefs with hard rules
  (evidence for every claim, a reproduction for every issue, no source
  reading) and an issue schema; they never name a repo path or an internal
  component.
- rounds: **1** cold discovery → fix; **2** blind re-test on the new build →
  fix; **3** adversarial conditions + a differential against the round-2
  build.
- leak audit after every round: every transcript, log and issue grepped for
  tokens that exist only outside the shipped package. 0 hits over 105
  artefacts across the three rounds (falsified with a planted leak before
  use).

## Metrics

| metric | round 1 | round 2 | round 3 |
|---|---|---|---|
| issues filed | 25 | 18 | 23 |
| root causes after dedupe | 18 | 14 | 18 |
| independently reproduced by me | 5 | 6 | 6 |
| **fixed in that round** | 14 | 7 | 5 |
| documented (doc-only fixes) | 4 | 2 | 3 |
| enhancements deferred to the digest | 3 | 4 | 6 |
| **regressions introduced by the previous round's fixes** | — | **3** | **0** |
| "would I keep it installed" | 2/4 | 3/4 | 4/4 |

Cross-persona duplicates were the strongest priority signal: PID 1 being
demoted (3 personas), uninstall leaving daemons running (2), the collector
shipping the daemon's README (4), a reinstall not taking effect (4).

## What changed because of it

**Safety and identity**

* PID 1 is never a policy target, rule or bias; a value an older build left
  there is unwound to 0. The learned axis also skips a `bias_protect` list
  (default `systemd dbus-broker dbus-daemon sshd gdm sddm polkitd elogind`,
  plus the daemon and collector themselves), matched against both the
  executable and the comm.
* Rule matching uses `/proc/<pid>/exe` (then `argv[0]`, then comm). `exec -a`
  and `prctl` both let a task choose a name; the executable it cannot. This
  is a documented divergence from ananicy-cpp, and it also fixes daemons that
  rewrite `argv[0]` (`postgres: checkpointer`) silently losing their rule.
* The daemon never applies a decision to its own pid: the unit's `Nice=-5`
  is the operator's statement, and a corpus entry named `aranicy` was
  overriding it.

**Silence — the dominant defect class (11 of 30 root causes)**

* a fresh install with zero rules warned nothing → startup warning naming
  the directory;
* `lint` was green for rules that can never match (empty name, a glob in
  `name`), for files it never reads (`.json`, `.rules.bak`), for an
  undefined `cgroup`, for a duplicate name, and for an empty corpus;
* `lint` exited 1 on a missing `ananicy.conf` and on a `.types`-only tree,
  while its help promised errors-only — both fixed;
* an unusable cpuset still logged `applied` (open, see digest).

**Lifecycle**

* `.install` scriptlets stop and disable on removal (an uninstall used to
  leave both daemons running from deleted inodes and a reinstall changed
  nothing), restart on upgrade, and drop the learned state file;
* `PartOf=aranicy.service` on the collector: stopping the daemon stops the
  sampler, while `Requires=` still stops the daemon if the collector dies.

**Documentation that could not be checked from the package**

* the collector had no README of its own (it shipped the daemon's, byte for
  byte); `COMPATIBILITY.md` and `BENCHMARKS.md` were cited but not shipped;
* the performance budget described the retired single-process architecture
  (2.28%) while the shipped pair measures 12.3%; the README now leads with
  the number for what the package installs;
* the CPU-quota period was documented as 1 s (code uses the kernel's 100 ms);
* the trust model of a root daemon applying a corpus is now stated.

## Open — needs a decision, not a patch

These were reported, reproduced, and deliberately **not** fixed in a round:

1. **Privileged properties are reachable by any user** (`dana-3`, `dana-7`).
   Identity is the executable now, but a user can `cp /bin/sleep ~/sleeper`
   and receive that rule's `SCHED_FIFO`/negative nice from a root daemon.
   Inherent to "root daemon applies a corpus to matching names"; documented
   as a trust model. A gate (only root-owned, non-world-writable executables
   get elevation) is a policy change: it would also stop a user's own game
   from getting `-5`.
2. **The classifier ranks interaction rate, not CPU demand** (`dana-4`).
   A shell scored 1011 and a CPU spinner 3.9, both `Bulk +3`. This is the
   ADR 0001 finding; the next step there is the two-axis `lean` classifier
   behind `bias_shadow`, not another constant.
3. **A rule edit only reaches running processes for `nice`** (`kaz-8`), and
   deleting a rule hands the process back to the learner (`kaz-11`).
   Reconciling `ioclass`/`cgroup` changes and rule deletion on a reload is
   the same mechanism — a re-plan of live processes — and needs its own
   design.
4. **Cgroup `CPUQuota` is lost across a boot** (`ren-13`, `dana-11`,
   `ivy-15`): the cgroup is created and processes move, the quota does not
   re-apply, and `dump cgroups` still reports it. Restarting the service
   fixes it; the root cause is the re-init ordering, and it is not a one-line
   change.
5. **Lost bias state leaves applied nice with nothing to unwind** (`kaz-7`),
   and **`bias_protect` still lets a protected name be learned** (`ren-9`):
   history-only consequences, both cheap to state as intended once a
   per-name opt-out exists.
6. **`--reload` trusts the pidfile** (`dana-15`): it SIGHUPs whatever pid is
   recorded. It should verify the process is an `aranicy` before signalling.
7. **`cpuset` names are not validated at load** (`ivy-17`): an unknown set
   passes `lint` and logs `applied` while doing nothing.

Deferred enhancements (unanimous or repeated): a per-process "why"
(`dump proc` says name/pid/nice only), rule provenance in `dump rules`, and
an opt-in/per-name switch for the learned axis (`"bias": false` on a rule)
so a game can be pinned without maintaining a second config.

## Follow-up wave (2026-11-11 evening): what shipped from the digest

The owner adopted six of the seven items; the escalation gate was declined as
a policy trade-off and stays documented as the trust model above.

| digest item | commit | behavioural check |
|---|---|---|
| cgroup `CPUQuota` lost across a boot | `dd41fa2` | VM, real reboot: reproduced the `EACCES` at 8 s, `CPUQuota applied after retry` at 68 s, `cpu.max` correct, `dump cgroups` reports `cpu_quota_applied: true` |
| `--reload` trusts the pidfile | `4e9f09a` | VM: pid 1 in the file → refused as stale; a live `sleep` pid → refused as "not aranicy"; the clean reload still signals |
| `cpuset` names unvalidated | `96fe3ad` | VM: `turbo-cores` and `0-4-9` warn with file:line, `lint` exits 1 |
| per-name bias opt-out | `1c2ff18` | VM A/B: `burn` (`"bias": false`, nice 5) learned bias 3 and stayed at 5; the unruled control went 0 → 3 |
| live re-plan on reload | `ea6172f`, `4998e00` | VM: ionice 7→3 landed on reload, a deleted field reset to none, a deleted rule reset its ioclass/ionice and — for a name the learner has no evidence on — nice (`released=1`). A name the learner *does* know keeps the learner's nice by design (ADR 0002); only the rule's dimensions are given back |
| two-axis classifier promotion | `21e7cd4` | ADR 0003: lean inputs persisted, offline replay implemented, first capture 24/2 (7.7 %) on a *saturated* 2 vCPU box — gate not met |

The cgroup boot failure is worth a note: the persona saw it as a race with
boot; the VM shows the kernel did not offer `cpu.max` in the new cgroup until
~68 s into a 63 s userspace startup, so the retry is what makes it
condition-independent. `ivy-15` — the uninstall half of that digest item —
was *not* covered by `dd41fa2`: removing the package used to leave the cgroup
and its quota behind, throttling whatever still ran inside. The daemon now
records the directories it created and the removal hooks release them
(`release_cgroups` in `contrib/aranicy.install` and
`contrib/install.sh --uninstall`).

## Limitations of this method

- Personas ran as subagents on the maintainer's harness, which supplies the
  working directory and project instructions to every subagent. Each was put
  in a directory outside the repo, told it was its whole world, and every
  artefact was audited; independent CLI sessions were unavailable (the
  harness model is not registered for child processes and the account quota
  was exhausted for every selectable model). Treat cross-persona duplicates
  as the credibility signal, not a single persona's prose.
- No display, no GPU, no audio: graphical behaviour, real frame rate and
  power draw are out of scope and were declared so in the cards.
- 2 vCPU is the wrong box for absolute performance; only instructions and
  A/B deltas are meaningful there (and the personas measured the pair at
  8.7-13.8% of a core at 2 800-3 800 execs/s, above the documented 12.3% —
  consistent with a faster exec rate, not with a defect).
- 65 issues over three rounds is a lot of surface for four personas; a fifth
  persona was dropped because power draw cannot be measured in a VM.

## Re-running it

`~/.agents/skills/synthetic-user-review/SKILL.md` for the procedure;
`research/synthetic-users/README.md` for the harness. One round costs four
VMs, four agents, roughly 35-45 minutes of wall clock, and a fix wave.

Round artefacts, including every issue as filed and every persona's
`retest.md` / `adversarial.md` / `differential.md`, are in
`research/synthetic-users/runs/2026-11-11-r{1,2,3}/`.

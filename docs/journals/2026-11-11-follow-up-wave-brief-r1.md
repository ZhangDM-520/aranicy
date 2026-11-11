# External review brief — aranicy follow-up wave

## Context

- Worktree: `/home/zhangdm/Projects/aranicy` (clean), branch `main`.
- Head under review: `e3c76a0`.
- Base: `8f185a5` (`origin/main`); compare with `git diff 8f185a5...e3c76a0` (three-dot).
- The brief itself is a docs-only commit on top of `e3c76a0`; nothing else moved.
- The repository is a Rust workspace: `aranicy-daemon` (runtime), `aranicy-apply`
  (syscalls), `aranicy-rules` (corpus), `aranicy-model` (shared types), plus a
  separate `collector/` for the eBPF side (not part of this diff).

`git diff --stat 8f185a5...e3c76a0`:

```
 .gitignore                                      |   3 +
 AGENTS.md                                       |  29 +-
 contrib/aranicy.install                         |  37 ++
 contrib/install.sh                              |  40 +++
 crates/aranicy-apply/src/lib.rs                 | 369 +++++++++++++------
 crates/aranicy-daemon/src/bias.rs               | 231 +++++++++++-
 crates/aranicy-daemon/src/bias_state.rs         |  56 +++
 crates/aranicy-daemon/src/daemon.rs             | 419 ++++++++++++++++++++--
 crates/aranicy-daemon/src/decision.rs           | 457 +++++++++++++++++++++++-
 crates/aranicy-daemon/src/dump.rs               | 125 ++++++-
 crates/aranicy-daemon/src/load.rs               |   9 +
 crates/aranicy-daemon/src/main.rs               |  12 +-
 crates/aranicy-daemon/src/paths.rs             |  17 +
 crates/aranicy-daemon/src/pidfile.rs            | 173 +++++++++
 crates/aranicy-model/src/lib.rs                 | 103 ++++++
 crates/aranicy-rules/src/lib.rs                 |  67 +++-
 crates/aranicy-rules/src/parse.rs               |  13 +
 crates/aranicy-rules/tests/rules_load.rs        | 106 ++++++
 docs/COMPATIBILITY.md                           |  65 +++-
 docs/JOURNAL.md                                 |   4 +
 docs/adr/0002-live-replan.md                    |  65 ++++
 docs/adr/0003-lean-classifier-promotion-gate.md | 109 ++++++
 docs/reviews/2026-11-11-synthetic-usage.md      |  42 +++
 tools/replay-bias/replay.py                     | 141 ++++++++
 24 files changed, 2534 insertions(+), 158 deletions(-)
```

### What the change does

aranicy is an auto-nice daemon: it matches exec-time process names against an
ananicy-compatible corpus, applies the matched policy (nice, scheduler,
ioclass, oom score, cgroup, cpuset), and additionally learns a per-name nice
bias from scheduler statistics. This branch ships six follow-ups to a field
report, plus the persistence and cleanup machinery they need:

1. **cgroup `CPUQuota` retry.** A `.cgroups` definition's `cpu.max` write can
   fail with `EACCES` when the daemon starts before the kernel exposes the
   controller in the new cgroup. The directory is kept, processes still move
   into it, the write is retried on a short cadence, and `dump cgroups`
   reports the declaration *and* the live value.
2. **`--reload` pid check.** The signal path proves the pidfile is held and the
   pid's executable is this binary before sending SIGHUP; stale and foreign
   pidfiles are refused by name and never signalled.
3. **cpuset validation at load.** Values that cannot resolve (`turbo-cores`,
   `x3d-*`, malformed lists) warn with `file:line`, and `lint` exits non-zero.
4. **Per-name opt-out.** `"bias": false` on a rule pins its base nice against
   the learned axis; learning continues internally.
5. **Live re-plan on reload.** A reload re-applies the new plan to running
   processes, and a deleted rule or a deleted field gives back what it had set
   (cpuset excepted; ADR 0002).
6. **Lean-classifier capture.** With `bias_shadow` on, each history entry keeps
   its two-axis inputs (`burst_ns`, `hop_hz`, `lean_at`) and the snapshot keeps
   a `lean_capture` marker naming the boundary that measured them, so
   `tools/replay-bias/replay.py` can recompute the verdict offline. Actuation
   is unchanged (ADR 0003).

Two invariants tie the persistence to the rest of the daemon: a capture stays
replayable while its marker names the boundary the running binary uses, and a
cgroup the daemon created can be released at uninstall (the daemon keeps a
ledger of the directories it created; `contrib/aranicy.install` and
`contrib/install.sh --uninstall` move survivors back to the root cgroup and
remove them).

## Scope contract

**In scope:** the diff above — daemon runtime, apply layer, rules loader,
persistence (`bias_state`), `dump`, packaging hooks, and the documents that
define the contract (`docs/COMPATIBILITY.md`, `docs/adr/0002-*.md`,
`docs/adr/0003-*.md`, `AGENTS.md`).

**Deliberately out:**

- Promoting the two-axis lean classifier to actuation. It is observation-only
  here; the promotion gate is defined in ADR 0003 and is not met.
- The trust model of a root daemon applying a user-editable corpus (any user
  can copy a binary to a name a rule elevates). It is documented as a policy
  choice, not changed.
- `x3d_mode` (accepted and warned about, never applied).
- The separate `collector/` crate, the BPF event source, and the two-process
  performance harness.
- Deploying this branch to a running host.

## What to check

Answer with concrete scenarios; a finding without an input/sequence that
produces a wrong result is not actionable.

1. Walk the cgroup lifecycle end to end: definition loaded, directory created,
   quota written, process moved in, daemon reloaded with the definition
   changed, removed, or re-added, daemon restarted, package removed. What fails
   first, and what state is left on disk at each step?
2. When `bias_shadow` is toggled, `bias_lean_*` / `bias_deadband_pct` are
   edited, the daemon is restarted rather than reloaded, the entry cap evicts
   names, or the state file is written by a different build: what happens to a
   persisted capture, and what can the offline replay then legitimately
   conclude from it?
3. What does a reload re-apply to a live process, what does it deliberately
   leave, and is the boundary ADR 0002 draws the one the code draws? Where does
   a deleted rule or field hand a dimension back, and where does it not?
4. What does the `--reload` path assume about `/run/aranicy.pid` and
   `/proc/<pid>/exe`, and what happens when each assumption fails (stale file,
   recycled pid, another user's process, a permissions restriction)?
5. Does every configuration key behave as its liveness classification says
   (live, reconfigurable, restart-only, accepted-only)? Where can an operator
   believe a change took effect when it did not?
6. Are the two uninstall hooks safe to run twice, safe when a process cannot be
   moved or a directory cannot be emptied, correct when several cgroups are
   listed, and honest when they fail? How do they decide which cgroups are
   theirs to touch?
7. Which invariants in the documents have no pin that would fail for the
   intended reason, and can any existing pin pass trivially (assertions that
   hold for unrelated reasons)?
8. Where would you expect this to break under a build storm, across a reboot,
   with a corpus whose names are unusual (separators, control characters, very
   long, non-UTF-8), or on a host where `/sys/fs/cgroup` is not a cgroup2
   mount?

## Validation facts

Commands run on this host at `e3c76a0`:

```
$ cargo test --workspace
  14 passed / 162 passed / 2 passed / 18 passed / 4 passed / 22 passed / 11 passed   (0 failed)
$ cargo clippy --workspace --all-targets
  Finished (no warnings)
$ bash -n contrib/aranicy.install && sh -n contrib/aranicy.install && bash -n contrib/install.sh
  clean
$ python3 tools/replay-bias/replay.py <capture>
  anchor 1185 band [1007, 1363]: 11 violations (42.31% of the cohort)
  lean axis: the snapshot carries no capture marker, so its entries are not a capture
```

The uninstall release path was exercised on this host against real cgroupfs,
as root, with a scratch record path so the live state directory was not
touched:

```
$ sudo mkdir /sys/fs/cgroup/aranicy-hooktest-$$
$ sudo sh -c 'echo $$ > /sys/fs/cgroup/aranicy-hooktest-$$/cgroup.procs; exec sleep 300' &
$ printf '%s\n' aranicy-hooktest-$$ > $record
$ sudo env ANANICY_CGROUP_MOUNT=/sys/fs/cgroup ANANICY_CGROUP_RECORD=$record \
      bash -c '. contrib/aranicy.install; release_cgroups'
$ cat /proc/<sleep-pid>/cgroup               # 0::/
$ ls -d /sys/fs/cgroup/aranicy-hooktest-*    # no match: the directory is gone
```

A cgroup with a nested child (so `rmdir` must fail) keeps its ledger line and
prints `aranicy: cgroup cleanup incomplete`.

**Known limits — not claimed:**

- The branch is **not deployed**: the host's `aranicy.service` still runs the
  packaged build. Runtime behaviour here is unit/integration tested only.
- The lean-classifier promotion gate is **not met**. The only capture is 24
  agree / 2 disagree on a deliberately saturated 2 vCPU VM; the code is
  observation-only.
- The uninstall release was measured on a running host with a scratch record
  path, not across a reboot or inside a package transaction.
- `cargo fmt` is not an enforced gate in this repository (rustfmt rewrites
  untouched code and the host's formatter aborts on a glibc assertion), so
  formatting was not machine-checked.
- No fuzzing, no adversarial corpus, no non-x86 hosts, and no
  container/namespace runs were done for this diff.

## Report format

Write the report to `docs/journals/2026-11-11-follow-up-wave-review-r1.md`.

For each finding: severity (`HIGH` / `MEDIUM` / `LOW` / `NIT`), `path:line`, the
failure story (which input or sequence produces which wrong result), and a fix
direction. Declare explicitly when a level is empty, and end with a single
verdict for the whole diff: `PASS` or `FIX-FIRST`.

## Anti-bias note

This brief was written by the change's author. Nothing in it has been
independently verified beyond the commands quoted above, and the author's
testing is not evidence that a defect is absent. Disagreement with the
implementation, the documents, or the scope is the point of this pass; no
hypothesis about where a defect lies is offered here, and none should be read
into the order of the questions.

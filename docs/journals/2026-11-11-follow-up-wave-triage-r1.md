# Triage — external review r1 of the follow-up wave

Report: `docs/journals/2026-11-11-follow-up-wave-review-r1.md`
(reviewed `8f185a5...e3c76a0`; verdict FIX-FIRST: 0 HIGH, 1 MEDIUM, 6 LOW, 2 NIT).

Every finding is closed here. All nine were accepted; none was declined and
none needed more evidence — each carries a concrete failure story that the
code confirms.

| # | report line | decision | close |
|---|---|---|---|
| MED-1 | "`LeanCapture.saved_at` timestamp is not refreshed on snapshot save, breaking offline replay staleness aging" | accept | `saved_at` is now stamped by `BiasSnapshot::mark_saved` at each save (`daemon::save_bias_state`); pin `a_saved_snapshot_dates_its_capture_at_the_save` (mutation: drop the stamp → red) |
| LOW-1 | "`release_cgroups` emits false 'cleanup incomplete' warning when process exits during uninstall" | accept | the pid move no longer latches; the directory is the arbiter (`[ -d "$dir" ]`); proved on real cgroupfs and with a cgroupfs-semantics `rmdir` shim |
| LOW-2 | "`aranicy --reload` fails with `NotAranicy` if the calling CLI binary was deleted/replaced" | accept | one `same_binary` comparison strips ` (deleted)` on both sides; pin `both_sides_of_the_executable_comparison_strip_the_deleted_marker` (mutation: compare raw → red) |
| LOW-3 | "Unprivileged `aranicy --reload` reports process identification failure instead of permission denied" | accept | `ReloadTargetError::PermissionDenied` for both the pidfile and the `/proc/<pid>/exe` link; pin `an_unreadable_pidfile_is_named_as_a_privilege_problem` |
| LOW-4 | "Explicit `cpuset` lists exceeding host CPU count pass load and lint without warning" | accept | `resolve_cpuset` refuses a list with no online CPU (`mask_intersects`); pins `an_explicit_cpuset_with_no_online_cpu_does_not_resolve` (mutation → red) and `a_mask_matches_only_on_shared_bits` |
| LOW-5 | "Rules with path separators in `\"name\"` never match but pass lint cleanly" | accept | loader warns with `file:line`; pin `a_name_with_a_path_separator_is_reported_at_load` |
| LOW-6 | "`PidFile::drop` unlocks before unlinking pidfile (pre-existing)" | accept | unlink first, the fd close releases the lock; pin `dropping_the_pidfile_unlinks_it_and_frees_the_singleton` (the ordering itself is a race and is not pinnable from outside — stated in the test) |
| NIT-1 | "`owed_dims` counts un-revertable `cpuset` in `released` telemetry" | accept | `released` now counts only dimensions actually reset; `kept_cpuset` counts the deliberate keep, and the reload line logs both; pin `a_dropped_cpuset_alone_is_reported_as_kept_not_released` |
| NIT-2 | "`tools/replay-bias/replay.py` has no test automation" | accept | `tools/replay-bias/test_replay.py` (9 stdlib tests) plus `crates/aranicy-daemon/tests/replay_gate.rs`, which runs it from `cargo test` (returns without checking where `python3` is absent; `cargo test` captures the note) |

Two documentation gaps the report raised inside its answers (not filed as
findings) are also closed: the cpuset row in `docs/COMPATIBILITY.md` now says
a well-formed list naming no online CPU warns at load, and the reload section
names the privilege requirement and the `released` / `kept_cpuset` split.

## Round 7 (post-fix re-review) reopened one closure

The Spec axis of round 7 found that LOW-4 was *not* delivered at load: the
loader's `CpuSet::Explicit` arm only parsed syntax, so the host resolver was
unreachable for exactly the values the finding was about —
`aranicy lint` still exited 0 for `"cpuset": "24-27"` on a 24-CPU host. Fixed
by routing a well-formed explicit list through the same
`host_cpuset_warning` the alias arm uses, with a loader-level pin
(`a_well_formed_explicit_cpuset_is_checked_against_the_host`, mutation-probed
red) and an end-to-end check:

```
$ ./target/debug/aranicy lint --config-dir <tmp>   # {"cpuset": "24-27"} + {"cpuset": "0-3"}
warning: .../10-a.rules:1: cpuset "24-27" cannot be resolved on this host
         (none of the listed CPUs is online on this host (24 online)); ...
lint: warnings found
exit=1
```

Round 7 also recorded three non-blocking items, all closed:

- an all-stale cohort reported `disagreement_pct: 0.0`, which literally meets
  ADR 0003's "< 10 % of decided entries" while scoring nothing — now `null`
  plus `gate_cohort_ok` (ADR's 200-entry minimum) and a human line saying the
  cohort is not a measurement;
- `close_window` still wrote `saved_at` with the window-close meaning — now
  `0` (unstamped), so `mark_saved` is the single writer and the field's
  contract holds structurally;
- `released` could count a process the pass never reached — now gated on the
  plan being an `Apply` that issues syscalls, and the doc wording says
  "re-planned processes".

## Round 7 (Standards axis) added one LOW and three NITs, all closed

- LOW: `saved_at` froze when the sampler stream was down — the debounced save
  ran only in the delivered-batch arm, so a collector that stayed down left a
  state file dated at the last pre-outage minute (the MED-1 failure mode in
  the one window where no save happens). The loop iteration is now a function
  (`bias_step`) that both arms call, and it owes the save; pin
  `an_idle_tick_saves_on_the_same_debounce_as_a_batch` drives the iteration
  itself (mutation: save only when a batch arrives → red).
- NIT: a directory (or an empty file) at the pidfile path was reported as
  "no running daemon" — now `ReloadTargetError::PidfileUnreadable`, pin
  `a_path_that_is_not_a_pidfile_is_named_as_such`.
- NIT: the permission test would fail on a root runner (root ignores the
  mode) — the test now returns early as root, which is the outcome its
  comment describes.
- NIT: the `name`-with-`/` warning's rationale named the wrong mechanism and
  the behaviour was undocumented — the message now says the match key is a
  basename and the only separator-bearing comm is a kernel thread, and
  `COMPATIBILITY.md`'s `name` row documents the warning and `lint` exit.

Also closed from the same report's answer to Q2: a cgroup that disappears
between the `-r` test and the `done <` redirect could abort
`install.sh --uninstall` under `set -e`; the read loop is guarded now, and the
two hook copies are byte-identical (checked).

## Round 8: PASS on both axes

Both axes returned **PASS** with no `HIGH`/`MEDIUM` and no repeated blocking
finding, which closes the loop. Two round-8 findings were fixed anyway because
each finished a change this wave had just introduced rather than polishing an
old one:

- the `name`-with-`/` warning fired even for a rule that also carries
  `name_regex`, where the exact key is dead but the definition still applies —
  `lint` would have gone red on a working corpus. It is now guarded on
  `draft.pattern.is_none()`, with the regex case pinned in the same test.
- the outer `done < "$record"` loop had the same `set -e` exposure as the
  inner one fixed in round 7: a ledger that disappears between the `-r` test
  and the redirect aborted `install.sh --uninstall` before the binary was
  removed. Both copies are guarded and still byte-identical.

Three documentation sentences were corrected to match the code they describe:
the ADR 0003 gate list now carries the 200-decided minimum (the tool has
enforced it since round 7), and ADR 0002 / COMPATIBILITY describe `released`
and `kept_cpuset` as what the pass *planned* rather than a count of syscalls.

`deferred: 7 bullets / 9 items at round 8` — recorded, not actioned:

- a marker with `saved_at == 0` (only possible from a hand-edited or
  foreign-build file; every production save stamps) disables the replay's age
  check, while the field doc says the replay "cannot age this";
- the human replay line for an all-deadband cohort blames "no fresh inputs";
- `released` includes a `nice` the learner re-applies at the learned value
  (the ADR wording above now says so, the counter still counts it);
- `an_unidentifiable_process_is_not_counted_as_a_revert` passes through the
  pre-existing non-UTF-8 early return, so it does not exercise the `Apply`
  gate it claims;
- the `comm`-carries-a-separator sentence was broader than the truth (a
  userspace `comm` can contain `/` via `prctl`; it is the key only when `exe`
  and `argv[0]` both yield no basename) — **closed in two halves**: the `name`
  row in round 12's follow-up, the `lint` message in round 13's;
- the privilege pin returns early on a root runner instead of asserting the
  `Stale` outcome root actually gets;
- `PidFile::drop` unlinks by path without comparing the inode it locked, so an
  externally removed pidfile can end up deleting a successor's file;
  a rule whose exact `name` *and* `name_regex` both require a separator loads
  silently — the recorded limit, pinned in
  `a_name_with_a_path_separator_is_reported_at_load`; and the two hook copies
  are byte-identical by inspection only.

## Round 9: one MEDIUM (a sentence I wrote), three LOWs

Standards returned FIX-FIRST on a documentation claim introduced in the
round-8 commit — ADR 0003 said the replay reports `null` for every cohort below
200, which is true only for an empty one (a 150-entry cohort returns a number
with `gate_cohort_ok: false`) — plus three LOWs; Spec returned PASS with the
same doc item. All four are fixed:

- ADR 0003 gate 1 now says "`null` when nothing was decided, and
  `gate_cohort_ok: false` below that count", which is what the tool does and
  what its tests pin.
- `COMPATIBILITY.md`'s `name` row now carries the `name_regex` exception.
- rules: a `name_regex` containing a path separator warned, so a rule whose
  exact name *and* pattern both required one was no longer silent (one warning
  naming the regex, mutation-probed red). **Reverted in round 10**: a pattern
  containing a separator is not a pattern requiring one.
- hooks: `} || { [ ! -e "$record" ] || left=1; }` — a redirect failure that is
  not "the ledger vanished" keeps the ledger instead of letting `rm -f` delete
  the only pointer to still-throttled cgroups.

`deferred: 8 items at round 9` — the round-8 list less the `name_regex` item
this round addressed. The round-8 section is 7 bullets but 9 distinct items;
counted as items, `name_regex` re-deferred by round 10 put the set back to 9,
and the `comm` sentence stayed open until round 13's follow-up finished its
second half, leaving 8 open at this head.

## Round 10: the regex-death warning was unsound and is reverted

The round-9 fix for "a rule whose exact name *and* pattern both require a
separator is silent" tested `pattern.contains('/')`. Both axes flagged that as
wrong in the other direction: a regex may contain a separator and still match a
key (`^(/usr/bin/)?foo$` matches `foo`, `foo/bar|baz` matches `baz`,
`[^/]+` matches everything), so a working corpus would have gone red with a
false "this definition can never apply". Deciding whether a pattern *requires*
a separator needs a language check, not a substring test, so the warning is
gone and the silence is recorded as a known limit instead. What remains is
sound: the exact-`name` warning fires unless the rule has a *compiled*
`name_regex` (an uncompiled one is no escape and the rule already warns about
it), and the contract row says so.

Pins: `a_name_with_a_path_separator_is_reported_at_load` now covers the plain
name, a live regex beside a dead exact key, an uncompiled pattern beside a
dead exact key, and a live pattern that contains a separator.

Round 11 reviewed that commit and returned **PASS on both axes** (Standards
with two NITs, Spec with one LOW and four observations — all about this
record's wording, none about the code). Its findings were all about the
removed check and are closed by the follow-up, so the open set is the round-8
list again.

## Round 12: three wording defects in the contract and the ledger

Round 12 reviewed round 11's follow-up: Standards FIX-FIRST (three MEDIUM),
Spec PASS (three LOW). None is a code defect. The `name` row's new
parenthetical contradicted its own next clause — a `comm` set via `prctl`
*can* reach the key when `exe` and `argv[0]` yield no basename, which is not
"the one separator-bearing comm … is a kernel thread". The `name_regex` row
claimed the rest of the rule still applies (false for a regex-only rule) and
called itself the only exception to "bad lines … are skipped" (a `cpuset` or
`cgroup` that cannot be applied is another). The deferred ledger still counted
the `comm` sentence as open after the row fixed it. The both-dead pin's
message claimed the pattern matched no key while the rule's own path key is
registered.

All are addressed here: the row is rescoped, the `name_regex` row says the
rule's other fields apply when another key matches, the ledger records the
`comm` item, and the pin asserts both that no basename matches and that the
path key does. Two of these did not land as intended: the `lint`-message
rewrite was lost when a mutation probe restored the source with
`git checkout --`, and the row's parenthetical named shapes more narrowly
than `derive_name` reaches (see round 13).

## Round 13: the message fix was lost, and the row was narrower than the code

Round 13 reviewed round 12's follow-up: FIX-FIRST on both axes (Standards one
HIGH, one MEDIUM, two LOW; Spec one MEDIUM, one LOW). The HIGH is mine. The
`lint` message and the comment above it were rewritten, then discarded by the
`git checkout --` that restored `lib.rs` after a mutation probe, so the commit
shipped the corrected contract row beside the old message that told a corpus
author a separator name "can never apply". The row was also narrower than
`derive_name`: `comm` is the key when neither `exe` nor `argv[0]` *yields a
basename* — a non-dumpable task makes both unreadable together, so an empty
`argv[0]` is not required — and a zombie's `exe` link fails rather than being
empty.

Fixed here: the message and its comment state the condition the row states (a
basename from `exe` or `argv[0]`, or the raw `comm` when neither yields one),
the row says a separator name "matches no basename key" and names the
kernel-thread, zombie and hidden-both shapes, the pin's comment no longer
claims the rule is dead for every process and the test now refuses a message
that claims impossibility, and the module brief's key order (`exe`, then
`argv[0]`, then comm) matches the code. The ledger records the `comm` item's
two halves by round, which is what round 12 should have done once the row
landed.

Reported for the record, not actioned (outside the reviewed diff): the
reviewer's Q5 notes that `apply_cgroup=false` does not evacuate processes
already placed in a cgroup, and that drift remains nice-only. Both are
documented behaviours of the current design, not defects of this wave.

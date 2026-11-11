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
| NIT-2 | "`tools/replay-bias/replay.py` has no test automation" | accept | `tools/replay-bias/test_replay.py` (8 stdlib tests) plus `crates/aranicy-daemon/tests/replay_gate.rs`, which runs it from `cargo test` and skips loudly where `python3` is absent |

Two documentation gaps the report raised inside its answers (not filed as
findings) are also closed: the cpuset row in `docs/COMPATIBILITY.md` now says
a well-formed list naming no online CPU warns at load, and the reload section
names the privilege requirement and the `released` / `kept_cpuset` split.

Reported for the record, not actioned (outside the reviewed diff): the
reviewer's Q5 notes that `apply_cgroup=false` does not evacuate processes
already placed in a cgroup, and that drift remains nice-only. Both are
documented behaviours of the current design, not defects of this wave.

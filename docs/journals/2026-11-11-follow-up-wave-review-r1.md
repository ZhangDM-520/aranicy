# External Review Report — aranicy follow-up wave (r1)

- **Diff under review:** `8f185a5...e3c76a0`
- **Target workspace:** `/home/zhangdm/Projects/aranicy`
- **Date:** 2026-11-11
- **Reviewer:** External reviewer (Antigravity)

---

## 1. Context & Scope Verification

This review evaluates the six follow-ups to the round-3 field report and their supporting persistence/cleanup machinery across `8f185a5...e3c76a0`:
1. `CPUQuota` retry on boot-time `EACCES` and `dump cgroups` live comparison.
2. `--reload` pidfile validation (`flock` check + binary identity check) before `SIGHUP`.
3. `cpuset` validation at load time with `file:line` warnings and `lint` gate.
4. Per-name `"bias": false` opt-out from the learned axis.
5. Live re-plan on `--reload` and dimension reversion for deleted rules/fields (ADR 0002).
6. Lean-classifier snapshot capture (`burst_ns`, `hop_hz`, `lean_at`, `lean_capture` marker) and offline replay tool (`tools/replay-bias/replay.py`, ADR 0003).

All tests in `cargo test --workspace` (14/162/2/18/4/22/11 passing) and `cargo clippy --workspace --all-targets` are clean. Packaging scripts pass syntax checks.

---

## 2. Detailed Technical Audit

### Question 1: Cgroup Lifecycle End to End

- **Definition loaded:** Parsed from `*.cgroups` into `CgroupDef`. Names are checked via `valid_cgroup_name` (`!name.is_empty() && name != "." && name != ".." && !name.contains('/') && !name.chars().any(char::is_control)`).
- **Directory created:** `init_at` executes `fs::create_dir(&dir)`.
  - If `Ok(())`: added to `manager.owned` and `manager.created`.
  - If `AlreadyExists`: added to `manager.created`, but **not** to `owned` (managed, not owned).
  - If error: warning logged, cgroup omitted without failing the batch.
- **Quota written:** `write_quota(&dir, pct, ncpu)`. If the kernel returns `EACCES` (e.g., early boot before `cpu` controller enabled at root), added to `manager.pending_quota` and warned. Retried every 5 seconds by `drift_loop`.
- **Process moved in:** Exec/drift/reload calls `CgroupManager::assign(pid, name)`. Writes `pid` to `<mount>/<name>/cgroup.procs`.
- **Daemon reloaded:**
  - *Definition changed:* `AlreadyExists` preserves directory; `write_quota` updates `cpu.max` immediately. `record_cgroups` keeps it in ledger because directory exists.
  - *Definition removed:* `init_cgroups` drops definition. Directory and its existing `cpu.max` **remain on disk**. The entry in `/var/lib/aranicy/cgroups` is **retained** because `mount.join(name).is_dir()` is still true. In `reconcile_live`, processes previously assigned to this cgroup are moved back to the root cgroup (`cgroup = Some("")`). If an unmanaged rule still references this cgroup, processes assigned there remain throttled.
  - *Definition re-added:* Re-enters `manager.created`, quota re-applied.
- **Daemon restarted:** Previous daemon drops `PidFile` (releasing flock and removing `/run/aranicy.pid`). New daemon starts. `create_dir` yields `AlreadyExists`. `record_cgroups` reads existing ledger, sees directory exists, preserves line. Quota is updated. Running processes are **not** re-planned on restart (ADR 0002 boundary).
- **Package removed:** `contrib/aranicy.install:post_remove` runs `release_cgroups()`. Reads `/var/lib/aranicy/cgroups`, writes every PID in `<cgroup>/cgroup.procs` to `/sys/fs/cgroup/cgroup.procs`, then removes the directory via `rmdir`.
- **What fails first and state left on disk:**
  - If a nested child cgroup was created under the cgroup, `rmdir` fails with `ENOTEMPTY`.
  - If a process forks continuously during evacuation, `rmdir` fails with `EBUSY`.
  - In both failure cases, `left=1` is latched: `/var/lib/aranicy/cgroups` is preserved on disk, the un-removable cgroup remains on disk, and a warning is emitted to stderr.
  - **Defect noted:** If any process in `cgroup.procs` terminates naturally while `release_cgroups` is iterating, `echo $pid > cgroup.procs` fails with `ESRCH`, setting `left=1`. Even though `rmdir` subsequently succeeds and the directory is deleted, the ledger is left behind with a false warning (see Finding 2).

---

### Question 2: Lean Capture Persistence & Replay

- **`bias_shadow` toggled (True -> False -> True):**
  - Toggling `bias_shadow` changes `self.cfg.shadow`, but `self.cfg.boundary()` does not change because `Boundary` only contains `(stage_min_ns, hop_min_hz, deadband_pct)`.
  - Existing `self.capture` and entries' `burst_ns`/`hop_hz`/`lean_at` are preserved across the toggle.
  - While shadow is false, `close_window` does not compute lean windows and does not update `self.capture`.
  - **Defect noted:** `LeanCapture.saved_at` is set only in `close_window` when `shadow == true`. It is **never updated on snapshot save**. If the daemon runs for hours with `shadow == false` and then saves a snapshot, `saved_at` is hours old. When `replay.py` evaluates `saved_at - at > max_age_s`, `saved_at - at` evaluates to ~0, completely defeating age-based staleness checks (see Finding 1).
- **`bias_lean_*` or `bias_deadband_pct` edited:**
  - In `reconfigure()`: `self.cfg.boundary() != cfg.boundary()` evaluates to `true`.
  - `self.capture` is set to `None`. Every entry's `burst_ns`, `hop_hz`, and `lean_at` are zeroed.
  - The snapshot immediately loses `lean_capture`. Offline replay refuses to score it (`no capture marker`).
- **Daemon restarted instead of reloaded:**
  - In `restore()`: `self.capture = snapshot.lean_capture.filter(|c| (c.stage_min_ns, c.hop_min_hz, c.deadband_pct) == (b.stage_min_ns, b.hop_min_hz, b.deadband_pct))`.
  - If boundary matches: marker and inputs are restored.
  - If boundary changed while stopped: marker is dropped (`None`) and all entry inputs are zeroed. Stale classifiers are never mixed.
- **Entry cap evicts names:**
  - `enforce_cap()` evicts by LRU `(last_seen, name)`.
  - Evicted names lose their lean inputs. If all entries with `lean_at > 0` are evicted, `snapshot()` drops `lean_capture` to `None`.
  - If only some are evicted, `replay.py` evaluates an incomplete, survivorship-biased subset of the original cohort.
- **State file written by a different build:**
  - Older build (pre-shadow): `burst_ns`, `hop_hz`, `lean_at`, `lean_capture` default to 0 / `None`. Replay refuses with "no capture marker".
  - Future build (`version > BIAS_STATE_VERSION`): `bias_state::load` refuses the file outright.

---

### Question 3: Live Re-plan on Reload vs ADR 0002

- **What reload re-applies to live processes:**
  - Reconcile pass iterates `/proc`. For every non-kthread, non-PID-1, non-self process:
    - Derives name from `/proc/<pid>/exe`.
    - Plans policy against the new `RuleSet`.
    - If matched: applies new policy. If the old rule had dimensions absent in the new rule, `fill_released` injects reset defaults (`nice/latnice: 0`, `sched: Normal`, `rtprio: 0`, `ioclass: None`, `ionice: 0`, `oom_score_adj: 0`, `cgroup: ""`, `autogroup: 0`).
    - If no longer matched, but was matched in old corpus: `release_policy(old)` resets all previous dimensions to defaults.
    - If learner had learned bias: learned nice survives and is applied; other dimensions are released.
    - `latency_nice` is only reset to 0 if both `desired.latency_nice` and `desired.nice` are `None` (preserving fallback semantics).
- **What reload deliberately leaves:**
  - `cpuset`: `release_policy` sets `cpuset: None`. A deleted rule leaves the existing CPU affinity in place.
  - External non-nice changes made outside aranicy between reloads are not corrected by periodic drift (drift remains nice-only).
  - Unmatched processes are completely ignored.
- **Where code and ADR 0002 diverge:**
  - ADR 0002 is faithfully implemented. However, `owed_dims()` increments when `old.cpuset` is dropped, causing `reconcile()` to flag `released = true` and log `released = N` even when `cpuset` was the only dimension dropped and no actual revert was executed on the process (see Finding 8).

---

### Question 4: `--reload` PID and `/proc/<pid>/exe` Assumptions

- **Assumptions:**
  1. `/run/aranicy.pid` exists and contains a valid ASCII PID.
  2. The file is locked with `flock(LOCK_EX)` by a living process.
  3. `/proc/<pid>/exe` is readable via `readlink` and its basename equals `std::env::current_exe().file_name()`.
- **Failure modes:**
  - *Stale file (daemon died without unlinking):* Kernel automatically drops `flock` on fd close. `lock_is_held()` acquires `LOCK_EX | LOCK_NB` successfully, releases it, and returns `false`. Refuses with `ReloadTargetError::Stale`. SIGHUP is never sent.
  - *Recycled PID:* Because no process holds `LOCK_EX` on `/run/aranicy.pid`, `lock_is_held()` returns `false`. Handled safely.
  - *Foreign process:* If a malicious/errant process flock'd `/run/aranicy.pid` and wrote another PID, `executable_name(pid)` reads `/proc/<pid>/exe` and detects `exe != current_exe()`. Refuses with `ReloadTargetError::NotAranicy`.
  - *Permissions restriction:*
    - If an unprivileged user executes `aranicy --reload` while the daemon runs as root: `lock_is_held` succeeds (Linux permits `flock` on read-only fds and reports `EWOULDBLOCK`). But `readlink("/proc/<root_pid>/exe")` fails with `EACCES` due to ptrace access checks. `reload_target` reports `cannot identify pid ... (/proc/.../exe is not readable)` rather than a permissions error (Finding 4).
    - If `/run/aranicy.pid` has mode 0600, `read_pid` returns `None`, falsely reporting that the file is missing (Finding 4).
  - *Deleted/upgraded binary:* If the CLI tool itself was deleted/replaced on disk before `--reload` was run, `std::env::current_exe()` has `" (deleted)"`. `executable_name(pid)` strips `" (deleted)"` from the daemon, but `ours` does not strip it, causing a spurious `NotAranicy` refusal (Finding 3).

---

### Question 5: Configuration Key Liveness

- **`RestartOnly` (7 keys):** `bias_enable`, `bias_cadence_ms`, `bias_state_path`, `bias_collector_socket`, `collector_events_socket`, `collector_wait_ms`, `loglevel`.
  - Behavior: Captured at startup. On `--reload`, `reload()` calls `old.restart_only_changed(&cfg)` and logs:
    `tracing::warn!("restart-only keys ignored: {}", deferred.join(", "))`.
- **`AcceptedOnly` (1 key):** `x3d_mode`. Parsed for ananicy-cpp compatibility; emits warning at load/lint that it is not applied.
- **`Reconfigurable` (14 keys):** `bias_min`, `bias_max`, `bias_window_s`, `bias_deadband_pct`, `bias_min_samples`, `bias_max_entries`, `bias_half_life_s`, `bias_starved_min_demand`, `bias_starved_ratio`, `bias_attack_min_util`, `bias_shadow`, `bias_lean_stage_min_ns`, `bias_lean_hop_hz`, `bias_group.*`.
  - Behavior: Forwarded directly to `BiasEngine::reconfigure()`. Clamps updated, targets re-derived, boundary invalidation applied if `stage_min_ns`, `hop_min_hz`, or `deadband_pct` changed.
- **`Live` (15 keys):** `check_freq`, `apply_*` (9 keys), `cgroup_load`, `type_load`, `rule_load`, `cgroup_realtime_workaround`, `log_applied_rule`, `bias_protect`.
  - Behavior: Read dynamically via `shared.config()` on each event, drift tick, and reload reconcile.
- **Operator confusion traps:**
  1. *Turning off `apply_cgroup`:* Disables cgroup writes in `applier`, but does **not** evacuate existing processes from previously assigned cgroups.
  2. *`systemctl restart` vs `aranicy --reload`:* If an operator restarts rather than reloads, running processes are **not** re-planned.
  3. *Unresolvable explicit cpusets:* Explicit cpuset ranges (e.g. `96` on a 12-core host) pass `lint` without warning and silently fail at apply time (Finding 5).

---

### Question 6: Uninstall Hooks Safety & Ledger Mechanics

- **Idempotency (running twice):**
  - First run clears cgroups and deletes `/var/lib/aranicy/cgroups`.
  - Second run checks `[ -r "$record" ] || return 0` and exits cleanly.
  - If partial failure occurred on first run, surviving directories are re-attempted while already-removed directories are skipped (`[ -d "$dir" ] || continue`).
- **Pinned processes / un-emptied directories:**
  - If a process cannot be moved or directory cannot be `rmdir`'d, `left=1` is set.
  - Script does not crash; ledger `$record` is preserved for future retry.
- **Multiple cgroups:** Handled line-by-line via `while IFS= read -r name; do ... done < "$record"`. Failure on one cgroup does not abort subsequent cgroups.
- **Ownership determination:**
  - Hooks do not guess. They read only `$record`.
  - Daemon writes to `$record` only cgroups where `fs::create_dir` returned `Ok(())` (`cgroups.owned`), plus existing ledger entries whose directories still exist. Pre-existing external cgroups are never recorded.

---

### Question 7: Document Invariants vs Test Pin Coverage

- **Invariants lacking pin or trivially passing:**
  1. `daemon_cpu_count()` in `dump.rs`: reads `/run/aranicy.pid`. In unit tests, this file does not exist, so `daemon_cpu_count()` is always `None` in test runs.
  2. `a_quota_that_cannot_land_is_pending_not_a_creation_failure` (`aranicy-apply/src/lib.rs:1265`): Pre-creates the cgroup directory before calling `init_at`. Consequently, `fs::create_dir` returns `AlreadyExists`, leaving `manager.owned` empty during the test.
  3. `tools/replay-bias/replay.py`: Has zero unit or integration tests in the repository (Finding 9).
  4. Packaging uninstall scripts (`contrib/aranicy.install`, `contrib/install.sh`): Tested manually by author, but lack automated test harnesses.

---

### Question 8: Breakages Under Stress, Reboots & Host Variations

- **Build storm:**
  - Short-lived processes exiting before `apply_policy` completes fail syscalls with `ESRCH`, handled gracefully without crash.
  - In `BiasEngine`, burst of compiler instances causes `enforce_cap()` LRU eviction. Captured entries with lean inputs are evicted, causing `snapshot().lean_capture` to become `None` once all captured entries are evicted.
- **Reboot:**
  - `/sys/fs/cgroup/*` directories are wiped on reboot (cgroup2 is in-memory tmpfs).
  - Next daemon start recreates directories via `init_cgroups`. Old definitions removed from corpus are pruned from ledger on startup because their directories no longer exist.
  - Early-boot quota write `EACCES` is safely retried by `drift_loop`.
- **Unusual corpus names:**
  - Control characters in cgroup names are rejected by `valid_cgroup_name`.
  - Slashes in rule names (e.g. `{"name": "/usr/bin/foo"}`) load without error or warning, but will never match because `derive_name` matches on basename only (Finding 6).
  - Non-UTF-8 process names are skipped with `Plan::Skip { reason: Skip::NonUtf8 }`.
- **Host without cgroup2 mount:**
  - `find_cgroup2_mount()` fails; daemon initializes `CgroupManager::unavailable()` (`/dev/null`).
  - `record_cgroups` aborts safely without erasing previous ledgers.
  - `assign` returns `ActionOutcome::Failed(ENOTDIR)`. Daemon continues running other policies.
  - `dump cgroups` outputs `cpu_max: null`, `cpu_quota_applied: false`.

---

## 3. Findings

### HIGH
*(None)*

---

### MEDIUM

#### `[MED-1]` `LeanCapture.saved_at` timestamp is not refreshed on snapshot save, breaking offline replay staleness aging
- **path:line:** `crates/aranicy-daemon/src/bias.rs:1150-1155` and `tools/replay-bias/replay.py:119`
- **Failure story:**
  In `crates/aranicy-daemon/src/bias.rs`, `self.capture` is constructed in `close_window`:
  ```rust
  self.capture = Some(crate::bias_state::LeanCapture {
      stage_min_ns: b.stage_min_ns,
      hop_min_hz: b.hop_min_hz,
      deadband_pct: b.deadband_pct,
      saved_at: close_t,
  });
  ```
  `saved_at` is set to `close_t` during the shadow window. When `snapshot()` is called, `self.capture` is serialized as-is without updating `saved_at` to the current time. Furthermore, if `bias_shadow` is turned off, `close_window` stops updating `self.capture`.
  When `tools/replay-bias/replay.py` evaluates entry staleness:
  ```python
  if saved_at and at and saved_at - at > max_age_s:
      stale += 1
      continue
  ```
  Because both `saved_at` and `at` reflect the timestamp of the last shadow window, `saved_at - at` remains ~0 even if the daemon ran for days without shadow or across a restart under the same boundary. The offline replay tool evaluates ancient, un-aged data and falsely reports `stale: 0`.
- **Fix direction:** Refresh `saved_at` to the current wall-clock Unix timestamp inside `BiasEngine::snapshot()` before returning the `BiasSnapshot`, or have `tools/replay-bias/replay.py` compare against the snapshot file's mtime / current time when `saved_at` is stale.

---

### LOW

#### `[LOW-1]` `release_cgroups` emits false "cleanup incomplete" warning when process exits during uninstall
- **path:line:** `contrib/aranicy.install:37-47` and `contrib/install.sh:58-68`
- **Failure story:**
  In `release_cgroups()`:
  ```bash
  while IFS= read -r pid; do
      [ -n "$pid" ] || continue
      echo "$pid" > "$mount/cgroup.procs" 2>/dev/null || left=1
  done < "$dir/cgroup.procs"
  rmdir "$dir" 2>/dev/null || left=1
  ```
  If a process exits while `cgroup.procs` is being read, `echo "$pid"` fails (`ESRCH`), latching `left=1`. Because the process exited, `rmdir "$dir"` succeeds and the cgroup is deleted. However, because `left=1`, the script preserves `$record` and warns that a cgroup may still be throttled.
- **Fix direction:** Only set `left=1` if `rmdir "$dir"` fails, or verify whether `$pid` is still alive before latching `left=1`.

#### `[LOW-2]` `aranicy --reload` fails with `NotAranicy` if the calling CLI binary was deleted/replaced
- **path:line:** `crates/aranicy-daemon/src/pidfile.rs:160-168`
- **Failure story:**
  `executable_name(pid)` strips `" (deleted)"` from `/proc/<pid>/exe`. However, `ours` (`std::env::current_exe()`) does not strip `" (deleted)"`. If an administrator runs `aranicy --reload` after a package upgrade that replaced the on-disk binary, `ours` is `"aranicy (deleted)"` while `exe` is `"aranicy"`. Reload aborts with `ReloadTargetError::NotAranicy`.
- **Fix direction:** Strip `" (deleted)"` from `ours` as well before comparison.

#### `[LOW-3]` Unprivileged `aranicy --reload` reports process identification failure instead of permission denied
- **path:line:** `crates/aranicy-daemon/src/pidfile.rs:154-159`
- **Failure story:**
  When invoked by a non-root user, `flock` succeeds, but `readlink("/proc/<root_pid>/exe")` fails with `EACCES`. `executable_name` converts this to `None`, producing `cannot identify pid ... (/proc/.../exe is not readable); refusing to signal it`. If `/run/aranicy.pid` is mode 0600, it falsely reports the pidfile as missing.
- **Fix direction:** Match on `io::ErrorKind::PermissionDenied` and report an explicit error requiring root privileges.

#### `[LOW-4]` Explicit `cpuset` lists exceeding host CPU count pass load and lint without warning
- **path:line:** `crates/aranicy-apply/src/lib.rs:985` & `crates/aranicy-model/src/lib.rs:59-75`
- **Failure story:**
  `parse_cpu_list` only validates syntax and `hi < 1024`. On a host with 12 CPUs, `"cpuset": "16-31"` produces no warning at load time and passes `aranicy lint`. At apply time, `sched_setaffinity` returns `EINVAL` and the cpuset is silently dropped, contradicting the documentation that unresolvable cpusets warn at load.
- **Fix direction:** In `cpuset_check`, verify that explicit CPU masks overlap with online CPUs (`/sys/devices/system/cpu/online`).

#### `[LOW-5]` Rules with path separators in `"name"` never match but pass lint cleanly
- **path:line:** `crates/aranicy-rules/src/lib.rs:325`
- **Failure story:**
  Legacy rules specifying `"name": "/usr/bin/foo"` parse without error. Because `aranicy` always matches against executable basenames, this rule can never match any process. Unlike empty names or wildcard names, `lint` emits no warning for names containing slashes.
- **Fix direction:** In `aranicy-rules`, emit a `LoadWarning` when an exact match rule name contains `/`.

#### `[LOW-6]` `PidFile::drop` unlocks before unlinking pidfile (pre-existing)
- **path:line:** `crates/aranicy-daemon/src/pidfile.rs:76-79`
- **Failure story:**
  `PidFile::drop` calls `flock(LOCK_UN)` before `remove_file(&self.path)`. A concurrently starting instance can acquire the lock and write its PID before the old instance executes `remove_file`, deleting the new instance's pidfile.
- **Fix direction:** Remove explicit `LOCK_UN` and unlink while the descriptor remains open, or rely on descriptor close to release the lock.

---

### NIT

#### `[NIT-1]` `owed_dims` counts un-revertable `cpuset` in `released` telemetry
- **path:line:** `crates/aranicy-daemon/src/decision.rs:344`
- **Failure story:**
  When a rule's `cpuset` is deleted, `owed_dims` increments, causing `reconcile` to report `released = 1`. However, `release_policy` deliberately leaves `cpuset` untouched, so no dimensions were actually reverted.
- **Fix direction:** Clarify telemetry or distinguish between applied reverts and intentionally skipped dimensions.

#### `[NIT-2]` `tools/replay-bias/replay.py` has no test automation
- **path:line:** `tools/replay-bias/replay.py:1`
- **Failure story:**
  The offline replay script serves as the gate for ADR 0003, but is not exercised by any CI script or automated unit test.
- **Fix direction:** Add a test runner exercising `replay.py` against valid, invalid, and empty snapshots.

---

## 4. Overall Verdict

**Verdict:** `FIX-FIRST`

While the core wave (cgroup quota retry, reload validation, live re-plan, opt-out) is sound and passes all compilation and unit checks, `[MED-1]` directly invalidates the ADR 0003 offline replay gate by allowing arbitrarily stale lean captures to masquerade as fresh (`stale: 0`), and `[LOW-1]` leaves leaked ledger files and false warnings during package removal. Both should be resolved before cutting a release.

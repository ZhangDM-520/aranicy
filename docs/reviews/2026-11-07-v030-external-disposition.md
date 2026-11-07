# Disposition — external review of v0.3.0 (2026-11-07)

Every finding in `2026-11-07-v030-external-report.md` was accepted and fixed in
the same working tree, except two sub-items where a cheaper fix achieves the
same effect (noted below). All fixes are covered by tests and by live
measurements on the development host (kernel 7.4.0-rc1-1-zeus, `scx_lavd` 1.1.4).

| # | Finding | Disposition |
|---|---|---|
| 1.1 | `avg_runtime_wall = 0` when `d_runs == 0`, `d_run > 0` | Fixed: `instant_rt` falls back to `d_run` / the in-flight `now - last_run_start`; runtime is seeded on the first window that *has* runtime (`rt_init`), not on the first window a pid appears. Tests: `uninterrupted_run_reports_runtime`, `zero_score_compiler_batch_is_bulk`. Live: `rustc` rows now read `rt=810 ms`, `rt=730 ms` with `d_runs == 0`. |
| 1.2 | pid-reuse / thread comm lifecycle | Fixed all three: `sched_process_fork` deletes the child's pid entry; exec resets every counter (`run_ns`, `runnable_ns`, `runs=1`, `sleeps`, `wakes_issued`, `last_enqueue`, `last_tick`); `comm` is refreshed on every switch and tick; `Aggregator::ingest` drops the pid's `Ema` when the comm changes. |
| 1.3 | IRQ-context wake attribution | Fixed: `in_interrupt()` via `bpf_this_cpu_ptr(&__preempt_count) & 0x00ffff00`, plus self/idle-wake exclusion. Live: a pure spinloop reports `wake_freq ≈ 2` on a quiet CPU (was 22–45); on a busy CPU the gate is best-effort — this kernel is `CONFIG_PREEMPT_LAZY`, where `__preempt_count` does not always carry hardirq nesting, so some IRQ wakes still land (measured up to ~45/s with the CPU fully loaded). With the wake gate removed from `sample_score` this is a log2 multiplier on the score, not a classification break. |
| 1.4.1 | first window of a new pid discarded | Fixed: a pid absent from the baseline is treated as a zeroed baseline (its map entry starts at zero). Test: `mid_window_task_is_not_discarded`. |
| 1.4.2 | in-flight `last_run_start` / `last_tick` never folded | **Partially accepted**: folding into the cumulative delta double-counts against the switch flush, so the fold is confined to the *metric* (`avg_runtime_wall`), which is where the reported breakage was. Cumulative `run_ns` stays exact on both kernel shapes via the tick + switch residual. |
| 1.4.3 | switch/wakeup edge cases | Fixed: early return on `prev->pid == next->pid`, `last_tick = 0` on switch-in, wakee enqueue only when `last_run_start == 0`, unconditional `last_enqueue = now` on preemption, `TASK_DEAD` is not a sleep. |
| 1.5.1 | non-fatal event-socket bind | Fixed: `events::bind()` runs on the main thread before the serve thread starts and aborts startup on failure. |
| 1.5.2 | first frame lost on reconnect | Fixed: `EventQueue::push_front` requeues the frame after a failed write, and an EOF probe (`recv(MSG_PEEK\|MSG_DONTWAIT)`, since `UnixStream::peek` is unstable) returns to `accept` promptly. Test: `requeued_frame_survives_a_failed_write`. |
| 1.5.3 | snapshot cost / 500 Hz pump polling | **Alternative fix**: `MapCore::lookup_batch` (1024 entries per call, per-key fallback kept) cuts the drain from ~32k `bpf()` calls to ~16, and a condvar replaces the 2 ms poll loop. Measured: collector CPU 47.6 s / 20 min (≈4 % of a core) → ≈0.5–1 %. A separate snapshot thread was not needed: `libbpf-rs::Object` is `Send` but not `Sync`, so sharing it would re-serialize on a lock anyway. |
| 1.6 | `Some(0)` vs `score_ema == 0.0` sentinel, test gaps | Fixed: `Entry::score_init` (restored snapshots mark it `true`). Tests: 32-sample unscored window, `zero_score_compiler_batch_is_bulk`, `starved_gate_honours_custom_thresholds`. |
| 1.7 | config validation, reload warnings, caps, stale docs | Fixed: `bias_starved_min_demand < 1024`, empty socket/state paths warn and keep the default, `reload()` logs `cfg.warnings`, both units gain `CAP_SYS_PTRACE`, the redundant `Wants=` is gone, and `COMPATIBILITY.md` no longer claims the daemon globs `/run/scx/*/stats` (new keys listed, `aranicy start` described correctly). Test: `collector_keys_are_validated`. |

## Gates after the fixes

- `cargo test --workspace`: 87 passed (daemon) + helpers, 0 failed.
- `cd collector && cargo test`: 19 passed.
- `cargo clippy --workspace --all-targets` and `cd collector && cargo clippy --all-targets`: clean.
- Red-first checks: each new test was run against the pre-fix code and failed
  (runtime fallback, first-sighting, score sentinel, config floor).
- Live: daemon stop 58 ms, collector stop 231 ms, spinloop `wake_freq 0`,
  `rustc` `rt` 730–810 ms under an uninterrupted run, exec stream
  `unresolved=0 dropped=0`, bias stream still changing.

## Known residuals (documented, not defects)

- Snapshot and ring-buffer poll still share one thread; the batch read bounds
  the stall to well under 100 µs, far below the `/proc` exit race window.
- `wake_freq` still counts wakes from kernel threads (kworkers legitimately
  wake others); only IRQ-context attribution is filtered.

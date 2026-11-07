# External review report — aranicy v0.3.0 (scheduler-agnostic collector)

Date: 2026-11-07
Target: uncommitted working tree on top of `HEAD` (`v0.2.3`), 36 changed files
Gates verified: `cargo test --workspace` (136 passed), `cargo clippy --workspace --all-targets` (clean), `cd collector && cargo test && cargo clippy --all-targets` (15 passed, clean), root `Cargo.lock` and `cargo tree --workspace -e normal,build` free of `scx_stats*` and `libbpf*`.

---

## 1. Ranked findings

### 1.1 `[blocker]` Unpreempted CPU-bound tasks (`d_runs == 0`, `d_run > 0`) emit `avg_runtime_wall = 0` and are dropped by `sample_score`

- **Location:** `collector/src/agg.rs:202-223` (interacting with `crates/aranicy-daemon/src/bias.rs:83-85` and `collector/bpf/collector.bpf.c:156-200`)
- **What is wrong:**
  `on_sched_stat_runtime` (`collector/bpf/collector.bpf.c:163-177`) and `on_sched_process_exec_account` (`collector/bpf/collector.bpf.c:186-200`) were added so a CPU-bound task that runs across drain windows without context-switching out still accumulates `run_ns`. However, `runs` is only incremented in `on_sched_switch(next)` and `on_sched_process_exec_account`. When a task is already running at the start of a window (its switch-in or `exec` was captured in `self.prev`) and runs through the entire window without switching out, `d_run > 0` (from `sched_stat_runtime` ticks) while `d_runs == 0`.
  In `Aggregator::ingest` (`collector/src/agg.rs:202-223`):
  ```rust
  let instant_rt = if d_runs > 0 {
      d_run as f64 / d_runs as f64
  } else {
      0.0
  };
  ...
  if seen_before {
      e.wait += (inst_wait - e.wait) * alpha;
      e.wake += (inst_wake - e.wake) * alpha;
      if d_runs > 0 {
          e.runtime += (instant_rt - e.runtime) * alpha;
      }
  } else {
      e.wait = inst_wait;
      e.wake = inst_wake;
      e.runtime = instant_rt;
  }
  ```
  - On the first active window after baseline (`!seen_before`), `d_runs == 0` seeds `e.runtime = 0.0`, reporting `avg_runtime_wall = 0`.
  - On every subsequent uninterrupted window (`d_runs == 0`, `d_run > 0`), `if d_runs > 0` is false, so `e.runtime` stays `0.0` (or freezes at whatever short pre-burn runtime was recorded before the uninterrupted loop).
  - If a task's first window in `self.ema` had `d_run == 0` (e.g. only `d_wakes > 0` or `d_sleeps > 0`), `e.runtime` is seeded to `0.0` with `seen_before = true`; when it later runs (`d_runs > 0`), `e.runtime` is smoothed from `0.0` (`alpha ≈ 0.0165` at 500 ms) instead of being seeded with the first non-zero runtime observation.
- **Why it matters:**
  `bias::sample_score` (`crates/aranicy-daemon/src/bias.rs:84-85`) gates on `if s.avg_runtime_wall == 0 { return None; }`. Any sample with `avg_runtime_wall == 0` is discarded as unscored.
  Reproduced live on the host via `sudo /usr/bin/aranicy-collectord probe --interval-ms 500 --iterations 2 --rows 6`:
  ```text
  pid=2334340 comm=burnx  prio=23 wait=0 wake=38 rt=0 demand=1024 deliv=1024
  pid=2339082 comm=burnx2 prio=23 wait=0 wake=0  rt=0 demand=1022 deliv=1022
  ```
  The unit test `utilization_is_clamped_to_one_cpu` (`collector/src/agg.rs:315-324`) exhibits the same bug (`c.run_ns = 2 * ONE_SEC`, `c.runs = 0` → `rows[0].avg_runtime_wall == 0`).
- **Concrete fix:**
  When `d_run > 0`, the task ran for at least one schedule in the window (`d_runs.max(1)`):
  ```rust
  let instant_rt = if d_run > 0 {
      d_run as f64 / d_runs.max(1) as f64
  } else {
      0.0
  };
  ```
  Track whether `e.runtime` has been seeded (`rt_seeded: bool` on `Ema`, or `e.runtime == 0.0`) so that `e.runtime` is seeded on the first window where `d_run > 0` and EWMA-updated on every subsequent window where `d_run > 0`.

---

### 1.2 `[blocker]` PID-reuse and thread `comm` lifecycle broken after removing `sched_process_exit`

- **Location:** `collector/bpf/collector.bpf.c:76-93, 139-151, 186-200, 238-243` and `collector/src/agg.rs:169-172, 181, 237`
- **What is wrong:**
  With `sched_process_exit` removed, exited tasks remain in the 16,384-entry `tasks` LRU map until evicted, and the design relies on userspace catching PID reuse via `if p.comm != c.comm` (`collector/bpf/collector.bpf.c:242`, `collector/src/agg.rs:170`). Three bugs break this mechanism:
  1. **`a->comm` is never updated for non-exec tasks (`collector/bpf/collector.bpf.c:76-93, 139-151, 163-177`):**
     `a->comm` is written only on `BPF_NOEXIST` insertion (`acc_get_or_create`) and in `on_sched_process_exec_account`. When an exited PID still present in `tasks` is recycled by the kernel for a thread (`pthread_create` / `clone` without `execve`, e.g. `rustc`, `ld.lld`, browser/compositor worker threads, kworkers) or a forked child that does not `exec`, `acc_get_or_create` returns the dead task's `task_acc` entry without updating `a->comm`. Likewise, a brand-new thread created via `clone` is first recorded in `on_sched_wakeup` before it can call `prctl(PR_SET_NAME)` / `pthread_setname_np`, locking in the parent's `comm` permanently.
  2. **`on_sched_process_exec_account` does not reset counters or `last_tick` (`collector/bpf/collector.bpf.c:186-200`):**
     On `execve`, `on_sched_process_exec_account` updates `comm`, `static_prio`, `last_run_start = now`, `runs += 1`, and `last_seen = now`, leaving `run_ns`, `runnable_ns`, `sleeps`, `wakes_issued`, `last_enqueue`, and `last_tick` intact. If a reused PID execs the same binary name (`comm`) as the previous occupant (common under build storms after PID wrap), `p.comm == c.comm` holds and the old counters bleed into the new task. Furthermore, if `on_sched_stat_runtime` fired between `fork` and `exec`, `a->last_tick` (`< a->last_run_start`) remains non-zero, so the next `on_sched_switch` (`collector/bpf/collector.bpf.c:115`) flushes `now - a->last_tick` instead of `now - a->last_run_start`, double-counting the pre-exec interval.
  3. **`Aggregator::ingest` retains the old task's `Ema` across `p.comm != c.comm` (`collector/src/agg.rs:170-172, 181, 237`):**
     When `p.comm != c.comm` triggers, `ingest` executes `continue;` without removing `pid` from `self.ema`. Line 237 (`self.ema.retain(|pid, _| cur.contains_key(pid))`) keeps `self.ema[pid]`, and line 238 sets `self.prev = cur`. On the next window (`p.comm == c.comm`), `seen_before = self.ema.contains_key(pid)` is `true`, so the new task **inherits the previous task's `Ema { wait, wake, runtime }`**.
- **Why it matters:**
  Non-exec threads reusing a PID in `tasks` have all activity attributed to the dead task's `comm`; threads setting their name via `prctl(PR_SET_NAME)` stay under their parent's `comm`; and exec'd tasks on a reused PID inherit the prior occupant's smoothed `wait_freq`, `wake_freq`, and `avg_runtime_wall`.
- **Concrete fix:**
  1. In `collector/bpf/collector.bpf.c`:
     - Attach `SEC("tp_btf/sched_process_fork")` (or `sched_wakeup_new`) to overwrite/delete `tasks[child->pid]` when the kernel allocates a PID to a new task (after the dead task's final window has already been drained).
     - In `on_sched_process_exec_account`, zero the accumulator (`run_ns = 0`, `runnable_ns = 0`, `runs = 1`, `sleeps = 0`, `wakes_issued = 0`, `last_run_start = now`, `last_enqueue = 0`, `last_tick = 0`, `last_seen = now`).
     - In `on_sched_switch(next)` and `on_sched_stat_runtime`, copy `p->comm` into `a->comm` so `prctl(PR_SET_NAME)` updates are observed.
  2. In `collector/src/agg.rs:170`:
     Call `self.ema.remove(pid);` before `continue;` when `p.comm != c.comm`.

---

### 1.3 `[major]` `on_sched_waking` attributes hardirq/softirq timer and I/O wakeups to the interrupted task on that CPU

- **Location:** `collector/bpf/collector.bpf.c:220-235`
- **What is wrong:**
  ```c
  SEC("tp_btf/sched_waking")
  int BPF_PROG(on_sched_waking, struct task_struct *p)
  {
      unsigned long long now = bpf_ktime_get_ns();
      struct task_struct *waker = (struct task_struct *)bpf_get_current_task_btf();
  ```
  `sched_waking` (`try_to_wake_up`) runs in the waking task's context only when called from task context (`in_task()`). When `try_to_wake_up` is invoked from a hardirq or softirq—such as `hrtimer_wakeup` (expiring `nanosleep`, `epoll_wait`, `poll`, or `futex` timers for other tasks) or device IRQ handlers—Linux does not switch `current`; `bpf_get_current_task_btf()` returns whichever task happened to be interrupted on that CPU.
- **Why it matters:**
  A CPU-bound task (`cc1plus`, `rustc`, `burnx`) occupies its CPU ~100% of the time and therefore absorbs every timer and IRQ wakeup occurring on that CPU.
  Verified live on the host: `burnx` (PID 2334340) and `burnx2` (PID 2339082) are pure userspace spinloops (`/proc/2334340/status` reports `voluntary_ctxt_switches: 0`), yet `/run/aranicy/collector-stats.sock` reports `wake_freq: 45` for `burnx` and `wake_freq: 22` for `burnx2`. A false `wake_freq` of 22–45/s raises `wake_ft` from `1` (`ilog2 = 0`) to `23..46` (`ilog2 = 4..5`), inflating `S0` and `score = S0²` of CPU-bound tasks on CPUs servicing timers or IRQs.
- **Concrete fix:**
  Gate `on_sched_waking` on task context (`!in_interrupt()`) and exclude self/idle wakes (`waker->pid == 0 || waker->pid == p->pid`). On x86_64 CO-RE, read `extern const int __preempt_count __ksym;` via `bpf_this_cpu_ptr(&__preempt_count)` (verified present in `/sys/kernel/btf/vmlinux` as `[71455] VAR '__preempt_count'`) and return `0` when `(*pc & 0x00ffff00) != 0` (`HARDIRQ_MASK | SOFTIRQ_OFFSET | NMI_MASK`); on arm64, check `waker->thread_info.preempt.count & 0x00ffff00`.

---

### 1.4 `[major]` Mid-window task appearances, in-flight slices under `sched_ext`, and `on_sched_switch` / `on_sched_wakeup` accounting edge cases

- **Location:** `collector/src/agg.rs:163-167, 173-177` and `collector/bpf/collector.bpf.c:105, 127-133, 139, 208-209`
- **What is wrong:**
  1. **Tasks appearing after the initial baseline (`self.prev.get(pid) == None`) are unconditionally skipped (`collector/src/agg.rs:163-167`):**
     Once `self.prev_at` is `Some(prev_at)`, any `pid` absent from `self.prev` was inserted into the BPF map `tasks` *during* `[prev_at, now_ns]` with zero-initialized counters (`struct task_acc zero = {};`), not kernel-lifetime counters. Skipping `self.prev.get(pid) == None` discards the first window (up to 500 ms) of every newly spawned process. Any short-lived build tool (`cc1plus`, `as`, `ld`, `collect2`) that spawns and exits within a single 500 ms drain window is skipped on its first snapshot (`prev.get(pid)` is `None`) and skipped on the next snapshot (`d_run == 0 && ...`), producing **zero samples** across its entire life.
  2. **In-flight `last_run_start`/`last_tick` and `last_enqueue` are parsed into `RawAcc` (`collector/src/agg.rs:53-56, 89-92`) but never used (`collector/src/agg.rs:173-174`):**
     On upstream kernels where `sched_stat_runtime` is FAIR-only and does not fire under `sched_ext`, `a->run_ns` only updates in `on_sched_switch(prev)` and `a->runnable_ns` only updates in `on_sched_switch(next)`. A task that runs continuously across a 500 ms drain boundary without switching out (or a starved task queued across a 500 ms boundary without switching in) reports `d_run == 0` (or `d_runnable == 0`) during the windows it is running/waiting, then dumps the multi-window total into the single window where it finally switches (where `util()` clamps it to `MAX_UTIL = 1024`, losing the earlier windows).
  3. **BPF edge cases in `on_sched_switch` and `on_sched_wakeup` (`collector/bpf/collector.bpf.c:105, 127-133, 139, 208-209`):**
     - **Asymmetric `prev_pid != next_pid` check (`collector/bpf/collector.bpf.c:105` vs `139`):** `if (prev)` checks `if (prev_pid != next_pid)`, but `if (next)` at line 139 does not. When `prev_pid == next_pid`, `prev` does not flush `now - last_run_start`, while `next` overwrites `a->last_run_start = now` and increments `a->runs += 1`, losing the elapsed runtime slice.
     - **`on_sched_wakeup` while running (`collector/bpf/collector.bpf.c:208-209`):** When `try_to_wake_up` takes the `ttwu_runnable` path on a currently running task (`last_run_start != 0`) that set `TASK_INTERRUPTIBLE` before checking a satisfied wait condition, `trace_sched_wakeup` fires without the task ever switching out. Because `on_sched_wakeup` only checks `wakee->last_enqueue == 0` (not `wakee->last_run_start == 0`), it stamps `last_enqueue = now` while the task is running; when the task is later preempted (`prev_state == 0`), `else if (a->last_enqueue == 0)` at line 130 keeps that old timestamp, counting the running interval in both `run_ns` and `runnable_ns` (inflating `task_util_est`).
     - **`TASK_DEAD` final switch increments `sleeps` (`collector/bpf/collector.bpf.c:127-128`):** On task exit (`do_task_dead`), the final `sched_switch` has `prev_state = TASK_DEAD (0x80) != 0`, counting process exit as a sleep.
- **Concrete fix:**
  - In `collector/src/agg.rs`: once `self.prev_at.is_some()`, default `self.prev.get(pid)` to `&RawAcc::default()` (with finding 1.2 resetting `task_acc` on fork/exec), and fold in-flight runtime (`if acc.last_run_start > 0 { now_ns.saturating_sub(acc.last_tick.max(acc.last_run_start)) }`) and runnable time (`if acc.last_enqueue > 0 && acc.last_run_start == 0 { now_ns.saturating_sub(acc.last_enqueue) }`) when computing effective window totals.
  - In `collector/bpf/collector.bpf.c`: return early at the top of `on_sched_switch` if `prev && next && prev->pid == next->pid`, clear `a->last_tick = 0` in `next`, check `wakee->last_run_start == 0` in `on_sched_wakeup`, set `a->last_enqueue = now` unconditionally on preemption (`prev_state == 0`), and only increment `a->sleeps` when `prev_state != 0 && !(prev_state & 0x80 /* TASK_DEAD */)`.

---

### 1.5 `[major]` Exec-event server: non-fatal bind failure, lost frame on client reconnect, and `snapshot()` blocking capture-time `/proc` resolution

- **Location:** `collector/src/main.rs:124-160`, `collector/src/events.rs:96-130`, `collector/src/bpf.rs:98-121`
- **What is wrong:**
  1. **`serve_events` bind failure is non-fatal (`collector/src/main.rs:152-160`):**
     `serve_events(&path, queue, stop)` is called inside a spawned thread that only logs `tracing::error!(error = %e, "exec-event server stopped")` on error and exits. The main thread still logs `"collector serving sched_samples + exec events"` and blocks in `wait_for_termination()`, leaving `aranicy-collector.service` `active (running)` without an event socket while `aranicy.service` fails repeatedly.
  2. **First `ExecFrame` after a client disconnect/restart is popped and lost (`collector/src/events.rs:111-129`):**
     `pump()` only touches `stream` when `queue.pop()` returns `Some(frame)`. When `aranicy` disconnects or restarts while no exec events are occurring, `pump()` stays in its `sleep(2ms)` loop on the dead `UnixStream`. When the next `exec` event arrives, `pump()` pops the `ExecFrame` from `queue` and calls `stream.write_all(&frame.encode())?` on the dead socket, failing with `EPIPE` (`Broken pipe`) and **losing the popped frame (`dropped` is not incremented)** before returning to `accept()` for the new client connection. Confirmed in live `journalctl -u aranicy-collector`:
     `18:09:51 ... exec-event client disconnected error=Broken pipe (os error 32) dropped=0`
  3. **Synchronous 16k-key `snapshot()` on the ring-buffer thread stalls capture-time `/proc` resolution and burns ~3–4% of a core (`collector/src/main.rs:128-149`, `collector/src/bpf.rs:98-121`):**
     Every 500 ms, the single `worker` thread stops polling `exec_events` to run `collector.snapshot()`, performing up to 16,384 `BPF_MAP_GET_NEXT_KEY` + 16,384 `BPF_MAP_LOOKUP_ELEM` syscalls (~1.2–1.5 ms per drain measured live on thread `2339880`: `47.6s` CPU time in 20 min, 14× `aranicy`'s own CPU time). Any short-lived process that `exec`s during `snapshot()` + `agg.ingest()` sits in the ring buffer until the map dump completes, re-introducing the `/proc/<pid>` exit race. Meanwhile `pump()` (`collector/src/events.rs:126`) busy-polls `queue.pop()` at 500 Hz (`sleep(2ms)`).
- **Concrete fix:**
  1. Bind and `chmod 0600` the `UnixListener` on the main thread in `main::run()` before spawning `events_thread`, passing the `UnixListener` into `serve_events` so any bind failure terminates `aranicy-collectord` with an error.
  2. Replace the 2 ms sleep loop in `EventQueue`/`pump()` with a `Condvar` (notified on `push` and shutdown), push the frame back to the front of the queue (or peek before popping) if `write_all` fails, and check for peer EOF (`MSG_PEEK | MSG_DONTWAIT`) so client disconnects are detected immediately.
  3. Run `poll_events` on a dedicated thread separate from the 500 ms `snapshot()` + `Aggregator` loop (and optionally use `bpf_map_lookup_batch` in `snapshot()`).

---

### 1.6 `[minor]` `sample_score` returning `Some(0)` collides with `score_ema == 0.0` sentinel; gate test coverage gaps

- **Location:** `crates/aranicy-daemon/src/bias.rs:83-91, 374-378, 924-938, 1225-1248, 1699-1717`
- **What is wrong:**
  1. **`Some(0)` score collides with `self.score_ema == 0.0` initialization check (`crates/aranicy-daemon/src/bias.rs:83-91, 374-378`):**
     With `wait_freq == 0` and `wake_freq == 0` now allowed, a sample with `wait_freq == 0`, `wake_freq == 0`, and `avg_runtime_wall > 500_000_000` (e.g. a 1 s window or `bias.rs:1017`) yields `wait_ft = 1, wake_ft = 1, runtime_ft = 1`, so `sample_score` returns `Some(0)`. In `Entry::close_window` (`bias.rs:374-378`), `self.score_ema == 0.0` is used as the "uninitialized" sentinel (unlike `util_init: bool` on line 356). After a window with `mean == 0.0`, `self.score_ema` stays `0.0`, so the next window with `mean > 0.0` takes the `if self.score_ema == 0.0` branch and overwrites `self.score_ema = mean` instead of EWMA-smoothing from `0.0`.
  2. **Gate test assertions (`crates/aranicy-daemon/src/bias.rs:924-938, 1225-1248, 1699-1717`):**
     - `unscored_active_windows_hold_confidence` (`bias.rs:1704`) sends `10` samples (`< min_samples = 30`). For any non-decisive score, the test would pass via the sparse-window branch (`win_score_n < need` at line 435) even if `sample_score` did not return `None`. Sending `>= 30` samples (`0..32`) with `wait = 0, wake = 0, rt = 0` isolates the `win_score_n == 0` gate.
     - Neither `bulk_batch` nor `decisive_bulk_batch` (`bias.rs:924-938`) tests `wait_freq = 0, wake_freq = 0` through `BiasEngine::record_batch`, and `starved_gate_only_low_minus_one` (`bias.rs:1225-1248`) only tests default `starved_min_demand`/`starved_ratio` thresholds.
- **Concrete fix:**
  Add `score_init: bool` to `Entry` (matching `util_init: bool`), change `unscored_active_windows_hold_confidence` to send `32` samples, and add `BiasEngine` tests covering a `(wait = 0, wake = 0)` compiler batch and custom `starved_min_demand`/`starved_ratio` values.

---

### 1.7 `[minor]` Config validation for new keys, SIGHUP reload warnings, `CAP_SYS_PTRACE` for `/proc/<pid>/exe`, and stale `docs/COMPATIBILITY.md` claims

- **Location:** `crates/aranicy-daemon/src/config.rs:188-201`, `crates/aranicy-daemon/src/daemon.rs:479-499`, `contrib/aranicy-collector.service:31`, `contrib/ananicy.service:5-6, 36`, `docs/COMPATIBILITY.md:102-106, 153-157, 169-170`
- **What is wrong:**
  1. **Config validation (`crates/aranicy-daemon/src/config.rs:188-201`):**
     - `bias_starved_min_demand` accepts any `u64` without checking `< 1024` (or `<= 1024`). Because `task_util_est` is clamped to `MAX_UTIL = 1024` (`collector/src/agg.rs:199`) and `bias.rs:364` checks `self.demand_ewma > cfg.starved_min_demand as f64`, any value `>= 1024` silently disables the starved gate forever without a warning.
     - `bias_collector_socket`, `collector_events_socket`, and `bias_state_path` accept empty values (`key =`) without warning, overwriting defaults with `""` (whereas `bias_group.<tag>` warns and skips empty values at line 207).
  2. **`SIGHUP` reload (`crates/aranicy-daemon/src/daemon.rs:479-499`):**
     `reload()` logs `rules.warnings` and cgroup warnings, but drops `cfg.warnings` silently.
  3. **Missing `CAP_SYS_PTRACE` for `/proc/<pid>/exe` fallback (`contrib/aranicy-collector.service:31`, `contrib/ananicy.service:36`):**
     `ProcResolver::derive_name` (`crates/aranicy-procname/src/lib.rs:229`) falls back to `readlinkat("/proc/<pid>/exe")` when `/proc/<pid>/cmdline` is empty. In Linux (`fs/proc/base.c:proc_exe_link`), reading `/proc/<pid>/exe` of a non-dumpable (`PR_SET_DUMPABLE=0`) or non-root process checks `ptrace_may_access(PTRACE_MODE_READ_FSCREDS)`, which requires `CAP_SYS_PTRACE` (verified live on this host: `sudo capsh --drop=cap_sys_ptrace -- -c 'readlink /proc/10716/exe'` fails with `EACCES`). Also `contrib/ananicy.service:5-6` has both `Wants=aranicy-collector.service` and `Requires=aranicy-collector.service` (`Wants=` is redundant).
  4. **Stale statements in `docs/COMPATIBILITY.md`:**
     - `docs/COMPATIBILITY.md:102-106` still states that the sampler *"re-globs `/run/scx/*/stats` on every reconnect, so switching schedulers (or a mavd `--cgroup` sub-scheduler appearing) needs no daemon restart"*.
     - `docs/COMPATIBILITY.md:153-157` lists `"Config keys (all bias_*)"` but omits the 5 new v0.3.0 keys (`bias_collector_socket`, `collector_events_socket`, `collector_wait_ms`, `bias_starved_min_demand`, `bias_starved_ratio`).
     - `docs/COMPATIBILITY.md:169-170` still describes `aranicy start` as *"eBPF-only: hard failure with a clear message when BTF/tracepoint support is missing"*.
- **Concrete fix:**
  - In `crates/aranicy-daemon/src/config.rs`: reject empty strings for socket/state path keys and values `>= 1024` for `bias_starved_min_demand` via `bad(&mut cfg, ...)`.
  - In `crates/aranicy-daemon/src/daemon.rs:484`: log `for w in &cfg.warnings { tracing::warn!("config: {w}"); }`.
  - In `contrib/aranicy-collector.service:31` and `contrib/ananicy.service:36`: add `CAP_SYS_PTRACE` to `CapabilityBoundingSet`; remove redundant `Wants=aranicy-collector.service` in `contrib/ananicy.service:5`.
  - In `docs/COMPATIBILITY.md`: remove the stale `/run/scx/*/stats` paragraph, update the `aranicy start` CLI bullet, and list the 5 new config keys.

---

## 2. Areas checked and considered correct

- **Licensing & workspace isolation (`Cargo.toml`, `Cargo.lock`, `collector/Cargo.toml`, `collector/Cargo.lock`, `collector/LICENSE`):**
  `collector` is properly excluded from the root MIT workspace (`exclude = ["collector"]`); `cargo tree --workspace -e normal,build` and root `Cargo.lock` contain no `scx_stats*` or `libbpf*` packages; `aranicy-wire` and `aranicy-procname` are MIT (`license.workspace = true`) with zero GPL dependencies; `collector` carries `license = "GPL-2.0-only"`, `collector/LICENSE`, and `// SPDX-License-Identifier: GPL-2.0-only` on `collector/bpf/collector.bpf.c`.
- **BPF struct layout & CO-RE portability (`collector/bpf/collector.bpf.c:25-57`, `collector/src/agg.rs:42-94`, `collector/src/bpf.rs:24-33`):**
  `struct task_acc` (96 bytes, explicit `pad`, 8-byte aligned `u64` fields) matches `RawAcc` byte-for-byte; `StatsCollector` field drop order (`rb` → `links` → `obj`) guarantees ring-buffer fd release and `bpf_link__destroy` before `Object` unload.
- **Exec wire framing (`crates/aranicy-wire/src/lib.rs`, `crates/aranicy-procname/src/lib.rs`):**
  280-byte fixed frame (`u32 pid`, `u8 comm_len`, `u8 reserved`, `u16 name_len`, `[u8; 16] comm`, `[u8; 256] name`) bounds `comm_len <= 16` and `name_len <= 256` on both encode and decode, cannot panic on arbitrary 280-byte inputs, and preserves the full `NAME_CAP` without mid-codepoint truncation.
- **`scx_stats` server & client protocol (`collector/src/stats_server.rs`, `crates/aranicy-daemon/src/sampler.rs`):**
  `SchedSample` exposes the 8 required fields with matching types/names; `nr_samples` string parsing clamps to `[1, cap]`; `sampler.rs` handles both map and array `stats_meta` shapes, `errno != 0` envelopes, preemption slow-retry, exponential backoff with jitter, and immediate `shutdown(Shutdown::Both)` wakeup on `SIGTERM`.
- **Window arithmetic overflow & division safety (`collector/src/agg.rs:149-205`):**
  `dt == 0` returns early while preserving the prior baseline; `per_sec` and `util` promote `u64` deltas to `u128` before multiplying by `1_000_000_000` / `1024`, preventing overflow; `util` clamps to `MAX_UTIL` (`1024`).

---

## 3. What could not be verified

- **Cross-scheduler live behavior (plain EEVDF and non-`lavd` `sched_ext` schedulers):**
  Per §4 constraint, `scx_lavd` on the development host is owner-managed and was not stopped or switched. In particular, the upstream kernel behavior where `sched_stat_runtime` does not fire under `sched_ext` could only be verified statically (which surfaced finding 1.4.2), because the host kernel (`7.4.0-rc1-1-zeus`) does emit `sched_stat_runtime` under `scx_lavd`.
- ** older-kernel verifier compatibility (< 5.5 without `tp_btf`):**
  Verified on the running `7.4.0-rc1-1-zeus` kernel only; no older kernel VM was booted. (`tp_btf` requires `CONFIG_DEBUG_INFO_BTF`, standard on Linux ≥ 5.5.)
- **`makepkg` split-package link behavior:**
  Deferred out of scope per §4.

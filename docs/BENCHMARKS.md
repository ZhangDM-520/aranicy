# Benchmarks — Phase 0 (baseline + candidate audit), 2026-10-22

Method: `tools/storm/` (fork+exec storm generator, deadline-paced, achieved
rate recorded per run) + `tools/storm/run_bench.py` (daemon CPU from
`/proc/<pid>/stat` utime+stime deltas; 100% = one full core; 1 Hz peak window;
RSS). 5 s warm-up discarded; runs 30–60 s; medians over ≥2–3 repetitions.
Instrument calibration: a known busy-loop process measured **99.97%**.

Host: Ryzen AI 9 HX 370 (12C/24T), CachyOS kernel `7.3.0-rc6-1-cachyos-bore-lto`,
BTF present (`/sys/kernel/btf/vmlinux`), `sched_autogroup_enabled=1`,
sched_ext (`scx_pandemonium`) enabled — scheduler confounder, constant across
all runs. Rule corpus: the live 15 832-line corpus (15 830 unique names, 15
types, 0 `name_regex` rules). Unruled storm name: `gsa-storm-probe`; ruled:
`ffmpeg` → `Heavy_CPU` (nice 4 + ioclass best-effort + ionice 7).

## ananicy-cpp 1.2.0 (the daemon being replaced)

| condition | median CPU (% of one core) | max 1 s window | notes |
|---|---|---|---|
| idle | 0.17 | 1.0 | |
| 2 000 execs/s, unruled | **13.7** | 24.0 | reps 12.57 / 18.63 / 13.70 |
| 2 000 execs/s, ruled | **16.2** | 27.0 | reps 15.42 / 16.15 / 17.92 |
| 4 000 execs/s, unruled | 20.1 | 25.0 | |
| 8 000 execs/s, unruled | 30.9 | 40.0 | |
| 12 000 execs/s, unruled | **44.9** | 57.5 | top *cleanly paced* point |
| 16 000 / 24 000 target | 28.8 / 28.4 | 33 | generator saturates ≈15 k execs/s; measured CPU is starvation-limited, not demand-limited — do not compare these rows |

The user-reported **110% symptom was not reproducible** with steady synthetic
storms up to 12 000 paced execs/s (max 44.9%). It likely needs bursty
workloads, thread-churn event volume (COMM/FORK renames), or reflected a
loaded-system snapshot. Sublinear scaling 2 k→12 k is consistent with silent
netlink event drops at high rates.

## JohnRTitor/ananicy-rs 0.1.0 @395da3f (adoption candidate — failed)

| mode | idle CPU | 2 000 execs/s CPU | applies at spawn? |
|---|---|---|---|
| netlink build (default) | **99.9** | 99.9 | **no** — `epoll_wait` EINVAL hot loop, blind; wrote 10.7 GB of error logs in one 2-minute run |
| bpf build, event mode | 0.17 | **12.6** | **no** — BPF program never loads (`bpftool prog show` empty), netlink fallback also broken; strace shows zero `/proc/<pid>` touches and zero `setpriority` attempts for spawned targets |
| bpf build, `--manual-scanning` | — | 13.2 | scan-only (strace-verified `setpriority`+`ioprio_set` succeed) — short-lived processes are never adjusted |

Additional defects observed: stale-singleton lockouts requiring manual
cleanup; `--force-remove-semaphore start` exits silently (it is a standalone
action); BPF poll loop treats EINTR as an error; docs contradict code on
rules hot-reload. Parser is fine (loads all 15 830 unique rules).

## Design consequences for aranicy

1. **Match lookup must be O(1) and regexes compiled at load** — ananicy-cpp
   recompiles every `name_regex` per unmatched event (inactive on this corpus,
   active for users with regex rules).
2. **No per-event `/proc` reads unless necessary** — a comm prescreen skips
   name resolution for unruled names (the common case in storms).
3. **Subscribe to `sched_process_exec` only** — fork/exit are wasted volume
   (ananicy-cpp pays ~3 netlink messages per spawn).
4. **No full-rescan amplification on overrun** — ananicy-cpp destroys the
   socket and rescans every process on ENOBUFS.
5. **No dead probes per event** — ananicy-cpp parses `/proc/<pid>/status`
   into JSON per event and discards the result.
6. **Silent success, rate-limited failure** — per-ESRCH logging was real
   production noise (2026-10-06 `set_priority` spam incident).

## Acceptance target

≤ **5% of one core** median at 2 000 execs/s in *both* storm variants
(unruled + ruled), measured with `tools/storm/` on this host, with the full
corpus loaded — and applied-rule correctness proven via strace-level
verification (see Phase-0 protocol: a long-lived spoofed-named target must
end up with the rule's nice value).

## Results — aranicy v0.1.0 (2026-10-22, same host/protocol)

| condition | ananicy-cpp 1.2.0 | **aranicy** | target | verdict |
|---|---|---|---|---|
| idle | 0.17% | **0.07%** | — | 2.4× lower |
| 2 000 execs/s, unruled | 13.7% | **2.28%** | ≤5% | **pass**, 6.0× lower |
| 2 000 execs/s, ruled | 16.2% | **4.10%** | ≤5% | **pass**, 3.9× lower |
| RSS under load | 13.8 MB | 11.5 MB | — | lower |

Reps (unruled): 2.43 / 2.28 / 2.27; (ruled): 3.57 / 4.10 / 4.32. Max 1 s
windows: 4–5% unruled, 8–11% ruled — even the spikes stay near the baseline's
*medians*. Functional proof in the same run: a long-lived process with
`argv[0] = gsa-smoke-uniq` received its rule at spawn (`nice=7`,
best-effort prio 3), i.e. the eBPF → name-resolution → match → apply chain
works end to end, verified via the observed target state.

## Packaged-binary measurements + profile (2026-10-23, `aranicy-0.1.0-1`)

The gsa tree recipe builds a binary identical in behavior; two runs of the
same package on the same host differ by **3× in wall-% — purely
environmental**:

| condition | 03:29 run (3.1 GHz, IPC 0.9) | 21:33 run (powersave 1.8 GHz, IPC 0.4) |
|---|---|---|
| idle | 0.0% | 0.0% |
| 2 000 execs/s, unruled | **2.05%** | 6.55% |
| 2 000 execs/s, ruled | **3.92%** | 8.40% |

The **instruction count per run is constant** (1.22 G vs 1.32 G for the
30 s `perf stat` window): the work per exec event is ≈ **20–22 k
instructions** and does not depend on load, host power state, or clock.
Wall-% is a function of the CPU governor/frequency — benchmark comparisons
must pin the governor (or compare instruction counts), and the ≤5% target
holds at normal boost frequencies (3.1 GHz measured).

Symbol-resolved `perf record` (2 000 execs/s storm, `kptr_restrict=1`
during capture) — cycle shares:

| share | where | what |
|---|---|---|
| ~19% | kernel VFS | `path_openat`, `proc_root_lookup`, `d_alloc_parallel`, `proc_alloc_inode`, `new_inode`, `kmem_cache_alloc_lru*`, `memcg_post_alloc_hook` — the per-event `/proc/<pid>/cmdline` open |
| ~7% | wakeups | `schedule`, `bpf_map_poll`, `do_epoll_wait`, `epoll_wait` — one context-switch per event (1.7 k/s) |
| ~3% | userspace | daemon hot functions (profile captured on the stripped package binary) |
| ~3% | misc | `memset`, `__vdso_clock_gettime`, `task_exec_state_rcu` (kernel exec-side) |

### Optimization targets (ranked, for the next round)

1. **Event-name fast path** — the `/proc/<pid>/cmdline` open+read is the
   dominant cost. Exact-compat matching needs `basename(argv[0])`, so the
   read is irreducible *by default*; an opt-in `name_source = execfile`
   mode (trust the tracepoint's `filename` basename + comm, zero `/proc`
   reads) would cut ~19% of cycles for users whose rules don't rely on
   argv[0]-spoofed names (the `exec -a` case).
2. **Batch the ringbuf drain + short busy-poll** — one wakeup per event
   costs ~7%; draining batches and spinning briefly (e.g. 25–50 µs) after
   each wakeup collapses context-switches under storms. Sound, no compat
   impact.
3. **Cheaper `/proc` read (default path)** — openat on a cached `/proc`
   dirfd, single pread into the stack buffer; trims path-walk constants.
4. **Profile the unstripped binary** — 3%+ of cycles sit in unsymbolized
   daemon functions; capture with debug symbols before optimizing
   userspace.

## Optimization round OPT2+OPT3 (2026-10-23, A/B same session)

Method: pristine-HEAD binary (A) vs changed build (B) back-to-back in one
session, governor pinned `performance` on all 24 CPUs (restored after),
`ananicy-cpp` stopped for the windows, full corpus, storm at 2000 execs/s.
Instructions/event = perf instructions over a 30 s window (60 000 events).
Raw data: `~/Workspace/aranicy-bench/results-opt23.jsonl`; perf captures,
strace proof and runner scripts in the worktree's `scratch/`.

| metric | A (HEAD) | B spin=30 µs (brief default) | B spin=0 (**shipped**) |
|---|---|---|---|
| wall-% idle | 0.00 | 0.03 | 0.03 |
| wall-% unruled, median [range] | 4.83 [4.52–5.03] | **12.42** [11.12–13.37] | 7.60 [4.45–9.05] |
| wall-% ruled, median [range] | 8.75 [6.12–9.72] | **12.37** [12.03–14.87] | 8.75 [5.93–8.85] |
| instructions/event (3 captures) | 24.4k / 24.1k / 25.1k | **111.5k** | 24.3k / 23.3k / 25.3k |
| cs/s @2000 execs/s | 2006 / 2082 / 2167 | 2014 | 2035 / 2056 / 2159 |
| cycles_frequency | 1.7–2.0 GHz | 2.4 GHz | 1.8 GHz |

Verdicts (deltas called only where they clear noise):

- **30 µs busy-spin: clear regression, expected effect absent.** +157% wall
  (unruled), +357% instructions/event, cs/s unchanged (2006→2014). The paced
  generator's arrivals are ~uniform (1.0 cs/event): the spin rides its window
  empty (~30 µs CPU per event) and collapses nothing. Shipped default is
  therefore **0 µs** (consume-all-before-blocking drain stays unconditional);
  `ANANICY_RS_BUSY_SPIN_US` raises the window for genuinely bursty workloads.
- **B spin=0 vs A: within noise at the invariants.** instructions/event means
  24.5k vs 24.3k (paired capture deltas −0.04% / −3.7% / +1.0%), cs/s
  identical (~1.0/event). Wall-% deltas are not callable: run-to-run spread
  reached ±35% and IPC drifted 0.87→0.67 across the session (same-session
  pairing mitigates but does not remove this).
- OPT3 mechanism verified by strace: `openat(fd, "NNN/cmdline")` /
  `readlinkat(fd, "NNN/exe")` on the startup-opened `/proc` dirfd — 3 534
  dirfd-relative resolver opens vs 0 absolute. The `/proc` root component is
  a cached dentry lookup; skipping it saves less than the profile's 19%
  open-path share suggests (the per-`<pid>` and per-file dentry/inode allocs
  remain). Functional spoofed-argv[0] proof green (nice 7 + best-effort 3).

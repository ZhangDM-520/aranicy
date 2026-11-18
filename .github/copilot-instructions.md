# Copilot instructions — aranicy

Auto-nice pair: applies nice/ionice/sched/cgroup/cpuset/oom/autogroup policy at exec time from `sched_process_exec`, plus optional learned nice bias from per-task scheduler samples. Process rules and the pitfall index live in `AGENTS.md`.

## Build, test, lint

Root workspace: MIT crates, `edition 2024`, MSRV 1.85. `collector/` is a **separate nested workspace**: GPL-2.0-only, MSRV 1.91, excluded from root; it needs clang + the distro `libbpf`.

```sh
cargo build --release && cargo test --workspace && cargo clippy --workspace --all-targets
cd collector && cargo test && cargo clippy --all-targets
python3 -m unittest discover -s tools/replay-bias -p 'test_*.py'
bash -n contrib/install.sh contrib/aranicy.install && (cd contrib && makepkg --printsrcinfo >/dev/null) && ./contrib/install.sh --dry-run
```

- Targeted tests: `cargo test -p <crate> [test_or_mod::]`, `cargo test -p aranicy-rules --test rules_load`, `cargo test -p aranicy-daemon --test replay_gate`, `sudo cargo test -p aranicy-apply -- --ignored` (root-gated), `unittest ... -k <name>`.
- `cargo fmt` is **not** a gate: it rewrites untouched code and aborts on a host glibc assertion.
- Unprivileged inspection: `aranicy lint` (exit 1 on any finding, including 0 rules), `aranicy match-test <name>`, `aranicy dump {rules,types,cgroups,proc,autogroup,bias}`. `sudo aranicy-collectord probe` exercises live BPF attachment and the sample hooks. `aranicy --reload` requires root, verifies the pidfile pid is this binary, and honours `ANANICY_{RS,CPP}_CONFDIR`/`_CONF` (`RS` wins).

## Architecture

Two processes over `/run/aranicy/collector-{stats,events}.sock`: the collector serves, the daemon reconnects with backoff, and a down stream degrades without crashing.

- `aranicy-collectord` (`collector/`, GPL-2.0-only): owns every BPF program (`sched_process_exec` plus per-task accounting) and serves `scx_stats` and the exec-event socket.
- `aranicy` (MIT, `crates/`): `aranicy-model` (no-alloc `Policy`/`CgroupDef`), `aranicy-rules` (JSONL load, regex compiled once), `aranicy-procname` (stack-buffer `/proc/<pid>/stat` and name derivation), `aranicy-wire` (`Sample`, `MAX_UTIL`, `NICE_OFFSET`, `SAMPLE_CAP`, 280-byte exec frames), `aranicy-apply` (syscalls only), `aranicy-daemon` (wiring).

Data flows:

1. Exec: collector resolves the **match name at capture time** → fixed 280-byte frame → daemon worker batches → `decision::plan_locked` → `decision::execute`.
2. Bias: collector aggregates → daemon `sampler` (`aranicy_wire::Sample`) → `daemon::bias_pass` (`BiasEngine::ingest` + `::tick`) → `decision::plan_locked`/`execute`.
3. Drift: periodic `check_freq` (60 s) re-asserts drifted `nice` through `decision::settle`.
4. Reload (SIGHUP): the signal thread swaps `RwLock<Arc<RuleSet>>` and runs `reconcile_live` over `/proc` for non-`nice` attributes; `cpuset` is never released and `kept_cpuset` counts those.

File-spanning invariants:

- `decision.rs` is the only policy synthesis: exec, drift and reload hand plans to `execute`; never duplicate rule/bias/eff-nice logic in `daemon.rs`.
- PID 1 and the daemon's own pid are handled inside `decision::plan`/`reconcile`; `PF_KTHREAD` is skipped on absorb, in the drift scan and in `reconcile_live`.
- Match identity is `/proc/<pid>/exe` → `basename(argv[0])` → kernel `comm`; bias history keys by **comm** (`bias_group` regexes), so the two namespaces never merge.
- A new `Sample` field touches `aranicy-wire`, `collector/src/agg.rs`, `UNSIGNED_FIELDS` in `sampler.rs`, and `every_contract_field_survives_the_lenient_parse`.
- Every `Config` field needs a `CONFIG_KEYS` row (`every_config_field_has_one_row`); a reload logs restart-only keys instead of pretending.
- Persistence: `bias_state::Durable` owns the on-disk shape (`Live` has no `Deserialize`); additive fields omit bumping `BIAS_STATE_VERSION`; `load` refuses a newer version.
- Units: `static_prio = nice + 20` (0..39); `task_util_est`/`cpu_util_wall` 0..1024 = one CPU; `wait_freq`/`wake_freq` are per **active** second (`avg_runtime_wall == 0` is the only "no evidence").
- Bias loop: `bias_pass` runs `ingest` then `tick` every iteration; priority-lowering gates on aggregate window demand (`bias_attack_min_util`, default 512), releases and starved steps do not.
- Classifier: `classify.rs` is pure and shared with `tools/replay-bias/replay.py`; boundary changes drop the cached `lean_capture`.

## Conventions

- Zero-alloc hot path: stack buffers (`StackPath`, `FrameBuf`), no `format!`, no per-event logging (count `ESRCH`/`EPERM`).
- Tests: sentence-style names; fixtures under `CARGO_TARGET_TMPDIR`/`target/` with pid-scoped sockets (never `/tmp`); fake socket servers share one reply cursor across connections.
- Behaviour/data changes update `docs/COMPATIBILITY.md`; classifier boundary changes must pass the `replay.py` gate.
- Packaging moves in lockstep: `Cargo.toml`, `collector/Cargo.toml`, and the split `contrib/PKGBUILD` (`aranicy-collector=$pkgver`, distro `libbpf`). Rule corpora stay out of the repo (GPL data); `contrib/install.sh` installs only the daemon.
- Benchmarks: build `tools/storm/storm`, drive with `run_bench.py`, judge on `read/exec` and `write/exec` medians across paired A/B cells — never live `instr/exec`.
- Commits are area-prefixed (`fix(review):`, `docs:`).

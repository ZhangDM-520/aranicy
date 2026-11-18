# AGENTS.md — aranicy

Last reviewed: 2026-11-18. This is the agent-facing README: what this repo is, the mental model, how to work here, and a lean index of the durable rules and pitfalls. Durable memory — action logs, feature/research notes, lesson bodies, plans, decisions, reviews and the live working note — lives outside this public repository in the maintainer's private warehouse, never in a tracked file.

## What this repository is

An auto-nice pair for Linux. `aranicy` (MIT, `crates/`) is a daemon that applies ananicy-compatible JSONL rules at exec time — nice, ionice, sched, cgroups, cpuset, oom score, autogroup — plus an optional learned nice bias from per-task scheduler samples; `aranicy-collectord` (`collector/`, GPL-2.0-only, its own workspace) owns every BPF program and serves the two sockets under `/run/aranicy/`. The daemon is a pure client: it loads no BPF, reconnects with backoff, and degrades stream by stream instead of crashing while the collector is down.

The package ships **no rule corpus** (ananicy rule data is GPL and not bundled): `provides=ananicy-cpp`, and `aranicy lint` reports `0 rules` until the user supplies `/etc/ananicy.d/` or points `ANANICY_CPP_CONFDIR` at a corpus.

## Mental model

- **Two processes, two sockets.** `/run/aranicy/collector-events.sock` carries exec frames (280 bytes fixed, name resolved at capture time); `/run/aranicy/collector-stats.sock` carries `scx_stats` per-task samples. Either stream can be down without taking the other with it.
- **One policy synthesis.** `decision.rs` plans for exec, drift and reload alike and hands plans to `execute`; `daemon.rs` only wires. Never re-derive rule/bias/effective-nice logic in the daemon.
- **Match identity is `/proc/<pid>/exe` → `basename(argv[0])` → kernel `comm`**, in that fallback order, resolved microseconds after the exec. `argv[0]` and `comm` are writable by the task itself, so a name taken from them is a name the process chose. Bias history keys by **comm** (`bias_group` regexes), so the two namespaces never merge.
- **The crate split is measured, not stylistic**: `aranicy-model` (no-alloc `Policy`/`CgroupDef`), `aranicy-rules` (JSONL load, regexes compiled once), `aranicy-procname` (stack-buffer `/proc/<pid>/stat`), `aranicy-wire` (`Sample`, `MAX_UTIL`, `NICE_OFFSET`, `SAMPLE_CAP`), `aranicy-apply` (syscalls only), `aranicy-daemon` (wiring).
- **Zero-alloc hot path**: stack buffers (`StackPath`, `FrameBuf`), no `format!`, no per-event logging — failures are counted (`ESRCH`/`EPERM`), not printed.

## How to work here

Domain vocabulary and units: `CONTEXT.md`; the behavioural contract: `docs/COMPATIBILITY.md`; measurements: `docs/BENCHMARKS.md`.

1. Read the pitfalls index below before touching the classifier/bias control law, the collector or wire format, or any benchmark claim.
2. Build, test and lint with the commands in `.github/copilot-instructions.md`; run the narrowest check first and stop at the first red.
3. Reproduce a defect before editing it, and prove every new check red-first — revert the fix, watch it fail, then put it back. Judge by the target artifact and read output, never by an exit code or a pipe's status.
4. Keep the tree dirty and hand disposition to the owner: no commit, push, or branch operation without explicit approval.

## Rules

- **Silence is the defect class this project fights.** A rule that can never match, an undefined cgroup, a fresh install with zero rules: each must warn and fail `lint`. An unusable `cpuset` reports `denied`, never `applied`.
- **`decision.rs` is the only policy synthesis.** Exec, drift and reload hand plans to `execute`.
- **PID 1 and the daemon's own pid are guarded inside `decision::plan`/`reconcile`**, and `PF_KTHREAD` is skipped on absorb, in the drift scan and in `reconcile_live` (a restored `kworker/*` entry must not keep applying). `scx_lavd` is owner-managed and never touched.
- **A new `Sample` field touches four places**: `aranicy-wire`, `collector/src/agg.rs`, `UNSIGNED_FIELDS` in `sampler.rs`, and `every_contract_field_survives_the_lenient_parse`; the lenient parse drops unlisted fields silently and has already shipped one flag dead.
- **Every `Config` field needs a `CONFIG_KEYS` row** (`every_config_field_has_one_row`); a reload logs restart-only keys instead of pretending they took.
- **Persistence belongs to `bias_state::Durable`** (`Live` has no `Deserialize`); additive fields omit bumping `BIAS_STATE_VERSION`; `load` refuses a newer version.
- **Packaging moves in lockstep**: `Cargo.toml`, `collector/Cargo.toml` and the split `contrib/PKGBUILD` (`aranicy-collector=$pkgver`, distro `libbpf`). Rule corpora stay out of the repo.
- **Behaviour and data-format changes update `docs/COMPATIBILITY.md`**; classifier boundary changes drop the cached `lean_capture` and must pass the `replay.py` gate.
- **`cargo fmt` is not a gate**: rustfmt rewrites untouched code and aborts on a host glibc assertion.
- **Never `git checkout --` over uncommitted work.** Restore a mutation probe by re-applying the inverse edit, and copy without `-p` (or `touch` it) so Cargo cannot reuse a mutated artifact's mtime.
- **Test runs have side effects**: `cargo test` writes libtest thread names into the live `/var/lib/aranicy/bias-state`; ignore bias entry counts right after a run and filter test threads in any calibration.
- **No daemons or schedulers in agent shells.** Services run under systemd only.

## Pitfalls index

Mechanisms and bodies live in the maintainer's private warehouse; the arrow names the entry.

- **eBPF accounting and transport** — `RingBuffer` must drop before `Object`; three hooks (exec seed, `stat_runtime` tick, `sched_switch` residual); never delete in `sched_process_exit`; `LRU_HASH` or inserts vanish silently; reset the batch buffer on reconnect or a partial tail invents pids → lesson `ebpf-accounting-and-transport`.
- **Bias control law** — rates divide by active time (`run + runnable`), priority-lowering gates on summed window demand (`bias_attack_min_util`, default 512), releases and starved steps do not; `bias_pass` runs `ingest` then `tick` every iteration → lesson `bias-signal-and-control-law`.
- **Benchmarking on a live host** — judge on `read/exec` and `write/exec` medians across paired A/B cells in one session, never live `instr/exec`; a busy-spin costs +357% instructions/event and buys nothing → lesson `benchmarking-on-a-live-host`.
- **Identity and silent failures** — exe-based match identity, comm vs match name, and the no-silent-no-op bar → lesson `identity-and-silent-failures`.
- **Agent worktree hygiene** — `git checkout --` losses, shared-worktree commits, probe restoration, silently disabled tests → lesson `agent-worktree-hygiene`.
- **A no-op attack cycle is not progress**: `attack_ready` returns false while `confidence <= 0.0`, because aging would zero the step anyway and the FIFO would re-arm.
- **The class boundary is not a constant**: the population mean `T` stays, with its deadband; the two-axis `lean` classifier runs in shadow only, and a threshold that is the mean of the population being classified deadbands whenever one name dominates.

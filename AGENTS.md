# AGENTS.md — aranicy

Rust auto-nice pair. `aranicy` (MIT crates under `crates/`) applies ananicy-compatible JSONL rules at exec time and learns a per-name nice bias; `aranicy-collectord` (`collector/`, GPL-2.0-only, its own workspace) owns every BPF program and serves both sockets under `/run/aranicy/`. The daemon is a pure client: it loads no BPF, reconnects with backoff, and degrades stream by stream instead of crashing when the collector is down.

Operational detail lives elsewhere and stays there: build/test/lint gates, crate map, data flows and file-spanning invariants in `.github/copilot-instructions.md`; domain vocabulary and units in `CONTEXT.md`; the behavioural contract in `docs/COMPATIBILITY.md`; measurements in `docs/BENCHMARKS.md`; decisions in `docs/adr/`.

## Memory

- **Project memory lives in the maintainer's private warehouse**, not in this repo: one log per action plus an index, feature/research/lesson notes, artifacts (plans, decisions, reviews, briefs) and the live working note. Read the matching lessons note before touching an area (eBPF transport, bias control law, benchmarking, identity/silence); write each action's log as it happens, not in a batch.
- **In-repo history is retired to the private warehouse**: the action journal, the dated journal entries, the reviews and the scratchpad all live there. `docs/reviews/2026-11-08-wave2-design.md` alone stays in the repo because it is code-cited by section (`classify.rs:1` §7.4/§7.5, `replay.py:7` §7.7) — do not move or renumber it. ADRs are mirrored in the warehouse; edit the warehouse copy when a fact changes.
- Recall by meaning before grepping code: `bm grep --semantic "<question>" --project aranicy`.

## Durable rules

- **Ask before committing, pushing, or replying.** Work stays dirty until the owner settles disposition.
- **Never `git checkout --` over uncommitted work.** Restoring a mutation-probe file that way has already erased a finished rewrite. Restore by re-applying the inverse edit, and copy without `-p` (or `touch` restored files) so Cargo cannot reuse a mutated artifact's mtime.
- **No daemons or schedulers in agent shells.** Services run under systemd only; `scx_lavd` is owner-managed and never touched.
- **Verify and falsify.** Prove every new check red-first without weakening assertions, and judge by target artifacts and read output — never by exit codes or a pipe's status.
- **Silence is the defect class this project fights.** A rule that can never match, an undefined cgroup, a fresh install with zero rules: each must warn and fail `lint`; an unusable `cpuset` must report `denied`, never `applied`. Match identity is `/proc/<pid>/exe` → `basename(argv[0])` → `comm`; never key rules on a name the task can rewrite.
- **One policy synthesis.** `decision.rs` plans for exec, drift and reload alike; never re-derive rule/bias/eff-nice logic in `daemon.rs`.
- **Test runs have side effects.** `cargo test` writes libtest thread names into the live `/var/lib/aranicy/bias-state`; ignore bias entry counts right after a run and filter test threads in any calibration.
- **`cargo fmt` is not a gate.** rustfmt rewrites untouched code and aborts on a glibc malloc assertion on this host.

Done-gate: `cargo test --workspace` + `cargo clippy --workspace --all-targets` + `cd collector && cargo test && cargo clippy --all-targets` (the collector needs clang and the distro `libbpf`).

<p align="center">
  <img src="banner.svg" alt="aranicy banner" width="100%">
</p>

# aranicy

Auto-nice daemon in Rust: applies nice / ionice / sched / cgroup / cpuset
policies to processes at exec time, driven by ananicy-compatible JSONL rules.

Why it exists: ananicy-cpp burns more than one core under heavy process
spawning (measured: 13.7% of one core at 2 000 execs/s and 44.9% at 12 000,
with algorithmic amplifiers waiting at higher rates — see
[docs/BENCHMARKS.md](docs/BENCHMARKS.md)). The measured Rust candidate
(`JohnRTitor/ananicy-rs`) is worse on this kernel: its netlink build spins a
full core at idle and its eBPF path never delivers events. aranicy is a
clean-room rewrite with a hard performance budget: **≤ 5% of one core at
2 000 execs/s**.

## Design in one paragraph

A single `sched_process_exec` eBPF tracepoint (fork/exit are never
subscribed); every event resolves its name from `/proc/<pid>/exe` (one
stack-buffered `readlink`) followed by an O(1) `HashMap` lookup. The
executable, not `argv[0]` or `comm`: both of those are writable by the task
itself (`exec -a`, `prctl(PR_SET_NAME)`), so a name taken from them is a
name the process chose — it can evade its own rule or borrow someone else's.
`basename(argv[0])` and the kernel comm remain as fallbacks for when
`/proc/<pid>/exe` is unreadable. Rules are
flattened into small `Copy`-friendly policy structs at load and `name_regex`
patterns are compiled once at load; the apply layer walks `/proc/<pid>/task`
once per process and issues syscalls only; failures are counted, not logged
per event.

Optional superset: **dynamic nice bias** — per-name nice deltas learned from
`sched_ext` per-task scheduler samples (`scx_lavd`/`scx_mavd` today; the
sampler schema-binds whatever per-task `scx_stats` surface the running
scheduler exposes) — bulk work nudged nicer, latency-sensitive work less
nice, applied as `clamp(rule_nice + bias, -20, 19)` with closed-loop
verification against the observed nice and half-life-capped history.
No per-task stats → no learning,
but learned biases keep applying and age out gracefully
([docs/COMPATIBILITY.md](docs/COMPATIBILITY.md)).

## Status

Implemented and benchmarked (2026-10-22): **6.0× cheaper than ananicy-cpp
under a 2 000 execs/s spawn storm (2.28% vs 13.7% of one core)**, applying
policies at exec time. Read that number with its conditions: a uniform paced
storm on an otherwise idle host, daemon only, no rule corpus. Real workloads
differ — a parallel build delivers 2 000-5 000 execs/s in clustered bursts,
the bias engine's window work scales with the observed task count, and on a
2-vCPU box the same absolute cost is a larger share of a core. The budget is
a target for the storm protocol, not a guarantee for your machine; measure
your own with `tools/storm/` (source tree) or `pidstat -p $(pgrep aranicy)`. 112 unit/integration tests (2026-10-28). See
[docs/BENCHMARKS.md](docs/BENCHMARKS.md) for the full measurement protocol
and results.

- Compatibility surface: [docs/COMPATIBILITY.md](docs/COMPATIBILITY.md)
- Measurements and acceptance target: [docs/BENCHMARKS.md](docs/BENCHMARKS.md)
- Benchmark harness: [tools/storm/](tools/storm/)

## Build

```sh
cargo build --release
sudo ./target/release/aranicy start        # root required
./target/release/aranicy dump rules        # unprivileged inspection
./target/release/aranicy dump bias         # dynamic-nice bias state (superset)
```

## Packages

Two packages are built from this tree:

| package | license | role |
|---|---|---|
| `aranicy` | MIT | the auto-nice daemon (rules, apply, bias engine) |
| `aranicy-collector` | GPL-2.0-only | `aranicy-collectord`, the scheduler-agnostic BPF statistics collector |

The collector owns every BPF program. `aranicy.service` declares
`Requires=`/`After=aranicy-collector.service`, so starting aranicy summons the
collector automatically; aranicy itself then needs only `CAP_SYS_NICE` (no
`CAP_BPF`). The license boundary is the package boundary: nothing GPL lives
under `crates/`.

`contrib/PKGBUILD` builds both as one split package: `aranicy` depends on
`aranicy-collector=$pkgver` (the socket contract is internal, so the pair moves
in lockstep). The collector links the **distro** `libbpf` rather than a vendored
static copy: makepkg's global `lto` option adds `-flto=auto` to CFLAGS/LDFLAGS,
which turns a vendored `libbpf.a` into an LTO archive that the final `ld.lld`
link cannot consume. `libbpf` is therefore a runtime dependency; `cargo`, `clang`
and `llvm` are build dependencies for the BPF object.

## Install

### From the package (Arch)

```sh
sudo pacman -U aranicy-collector-0.3.0-1-x86_64.pkg.tar.zst \
                aranicy-0.3.0-1-x86_64.pkg.tar.zst
sudo systemctl enable --now aranicy      # Requires= pulls the collector in
aranicy lint                             # what the daemon loaded
systemctl status aranicy                 # active (running) + the startup log
```

The package ships **no rule corpus**: rule data from the ananicy ecosystem is
GPL and is not bundled (`provides=ananicy-cpp`, so an old `ananicy-cpp`
install is replaced and its package-owned rules go with it). Put your own
files in `/etc/ananicy.d/` (`*.rules`, `*.types`, `*.cgroups`, scanned
recursively) or point `ANANICY_CPP_CONFDIR` at a corpus; `aranicy lint`
reports `0 rules` until you do, and the daemon warns about it at startup.

The packaged documentation is `/usr/share/doc/aranicy/README.md`,
`COMPATIBILITY.md` and `BENCHMARKS.md`.

### From the tree

Generic installer (binary + systemd unit + config; keeps any existing
`/etc/ananicy.d` rules corpus; replaces `ananicy-cpp` unless told otherwise):

```sh
./contrib/install.sh                  # build if needed, install, enable --now
./contrib/install.sh --dry-run        # show actions only
./contrib/install.sh --keep-ananicy-cpp --no-enable
./contrib/install.sh --uninstall      # remove binary + unit, keep config
```

Arch packaging: `contrib/PKGBUILD` (or the curated gsa-tree recipe
`packages/stable/aranicy`). After install the daemon lives in systemd like
ananicy-cpp did: `systemctl status aranicy`, `systemctl reload aranicy`
(rules + config reload).

## License

MIT — see [LICENSE](LICENSE). Rule *data* from the ananicy ecosystem (e.g.
CachyOS rule packs) is GPL and is never bundled with this repository.

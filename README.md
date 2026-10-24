<p align="center">
  <img src="./banner.svg" alt="aranicy banner" width="100%">
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
subscribed); a comm prescreen skips `/proc` reads for unruled names; rules are
flattened into small `Copy`-friendly policy structs at load; exact-name lookup
is a `HashMap`, `name_regex` patterns are compiled once at load; the apply
layer walks `/proc/<pid>/task` once per process and issues syscalls only;
failures are counted, not logged per event.

## Status

Implemented and benchmarked (2026-10-22): **6.0× cheaper than ananicy-cpp
under a 2 000 execs/s spawn storm (2.28% vs 13.7% of one core)**, applying
policies at exec time — the acceptance target of ≤5% is met in both storm
variants. 77 unit/integration tests. See
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
```

## Install

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

# The README still advertises "≤ 5% of one core at 2 000 execs/s" as the product's hard budget, while the shipped product is two processes and the shipped BENCHMARKS puts the pair at 12.3% on the same protocol

- severity: minor
- category: docs
- environment: Linux 7.2.11-arch1-1 (Arch rolling), aranicy 0.3.0-1 + aranicy-collector 0.3.0-1, 2 vCPU x86_64, 1958 MiB, CLK_TCK 100

## What I was trying to do

Re-check round 1's dana-5 (the documented performance budget) against this
build, and decide which number a user is supposed to believe.

## Steps to reproduce

```
bsdtar -xOf ~/payload/aranicy-0.3.0-1-x86_64.pkg.tar.zst usr/share/doc/aranicy/README.md | sed -n '10,18p'
bsdtar -xOf ~/payload/aranicy-0.3.0-1-x86_64.pkg.tar.zst usr/share/doc/aranicy/BENCHMARKS.md | sed -n '308,316p'
bsdtar -xOf ~/payload/aranicy-0.3.0-1-x86_64.pkg.tar.zst usr/share/doc/aranicy/BENCHMARKS.md | sed -n '366,374p'
bsdtar -xOf ~/payload/aranicy-collector-0.3.0-1-x86_64.pkg.tar.zst usr/lib/systemd/system/aranicy.service
# then measure the installed pair: utime+stime of both pids around a fork/exec loop
```

## Expected

One budget number, stated for what the package installs. The README's opening
claim is "a hard performance budget: **≤ 5% of one core at 2 000 execs/s**" and
its Status section offers "6.0× cheaper than ananicy-cpp under a 2 000 execs/s
spawn storm (2.28% vs 13.7% of one core)". A user installs two packages
(`aranicy` `Requires=` `aranicy-collector.service`), so the budget has to
describe the pair.

## Actual

The README's number describes the retired single-process architecture; the
shipped `BENCHMARKS.md` says so and gives a different figure for the pair
(daemon 6.1 % + collector 6.2 % = "total 12.3" wall at 2 000 execs/s, on a
12-core host), then states that the ≤ 5 % / 24.4 k instructions-per-exec
invariants "describe the retired single-process architecture and are not
directly comparable". So the two shipped documents disagree about the shipped
product's cost by more than 2×, and the user-facing one (README, which is what
`pacman -Qi` and the package doc dir show first) is the optimistic one.

My own measurement of the installed pair on this 2 vCPU box is between the two
stories and above the budget at any rate I could produce:

```
execs=60000 wall=21.00s rate=2858/s daemon=2.33% coll=6.38% sum=8.72% of one core
idle daemon=0.00% coll=0.50%
execs=60000 wall=15.85s rate=3786/s daemon=3.22% coll=10.54% sum=13.76% of one core
```

The collector is 2.7–3.3× the daemon's cost in both cells, which is the part
a user reading only the README would not expect at all — round 1's dana-5 said
"the README's acceptance sentence never mentions that a second process is doing
most of the work"; that is still true of the README, though BENCHMARKS now
documents it.

## Evidence

```
$ bsdtar -xOf aranicy-0.3.0-1-x86_64.pkg.tar.zst usr/share/doc/aranicy/README.md | sed -n '10,18p'
Why it exists: ananicy-cpp burns more than one core under heavy process
spawning (measured: 13.7% of one core at 2 000 execs/s and 44.9% at 12 000,
with algorithmic amplifiers waiting at higher rates — see
[docs/BENCHMARKS.md](docs/BENCHMARKS.md)). The measured Rust candidate
(`JohnRTitor/ananicy-rs`) is worse on this kernel: its netlink build spins a
full core at idle and its eBPF path never delivers events. aranicy is a
clean-room rewrite with a hard performance budget: **≤ 5% of one core at
2 000 execs/s**.
```

```
$ bsdtar -xOf aranicy-0.3.0-1-x86_64.pkg.tar.zst usr/share/doc/aranicy/BENCHMARKS.md | sed -n '308,316p'
## v0.3.0 split baseline — 2026-11-09

v0.3.0 moved every BPF program into `aranicy-collectord`, so per-exec cost is
no longer one process's bill: the collector captures, the daemon resolves and
applies, and the two talk over a Unix socket. The historical ≤5 % / 24.4k
instructions-per-exec / cs ≈ 1.0 invariants describe the retired single-process
architecture and are not directly comparable. `tools/storm/run_bench.py` is now
a two-process harness: `--pid` is repeatable, `--perf` measures each pid
separately (idle window subtracted), and the storm generator can cluster

$ bsdtar -xOf aranicy-0.3.0-1-x86_64.pkg.tar.zst usr/share/doc/aranicy/BENCHMARKS.md | sed -n '366,374p'
`powersave`; marginal cost per exec = (storm − idle) ÷ executed execs.

| metric (per exec event) | daemon | collector | total |
|---|---|---|---|
| instructions | **28–32k** (27.9 / 28.8 / 31.7k) | **25–27k** (25.3 / 25.9 / 26.5k) | **54–58k** vs 24.4k single-process → ≈2.2× |
| context switches | 0.79 | 0.83 | 1.6 |
| `read` syscalls | 0.996 (41 848 / 42 000) | — | — |
| wall-% of one core | 6.1 | 6.2 | 12.3 |
| idle instructions/s | 1.2–2.3M | 15–25M | — |
```

```
$ dp=$(systemctl show -p MainPID --value aranicy); cp=$(systemctl show -p MainPID --value aranicy-collector)
$ getcpu() { awk '{print $14+$15}' /proc/$1/stat; }; a0=$(getcpu $dp); b0=$(getcpu $cp); s0=$(date +%s%N)
$ for j in $(seq 1 60000); do /bin/true; done; s1=$(date +%s%N); a1=$(getcpu $dp); b1=$(getcpu $cp)
$ python3 -c "w=($s1-$s0)/1e9; d=($a1-$a0)/100; c=($b1-$b0)/100; print(f'...')"
execs=60000 wall=21.00s rate=2858/s daemon=2.33% coll=6.38% sum=8.72% of one core
execs=60000 wall=15.85s rate=3786/s daemon=3.22% coll=10.54% sum=13.76% of one core
```

```
$ bsdtar -xOf aranicy-0.3.0-1-x86_64.pkg.tar.zst usr/lib/systemd/system/aranicy.service | grep -E 'Requires|After'
After=local-fs.target aranicy-collector.service
Requires=aranicy-collector.service
```

## What I could not determine

- The 2 000 execs/s point exactly: my loop produced 2 858/s and 3 786/s, and
  `perf` is not installed (`perf: command not found`), so I cannot report
  instructions/event — the invariant the documentation itself says to trust.
  One sample per cell, `/bin/true` as the cheapest possible exec.
- Whether the "6.0× cheaper than ananicy-cpp" comparison still holds for the
  split: ananicy-cpp is not installed and I did not install it, so I make no
  claim about the ratio, only that the number quoted is a single-process v0.1.0
  measurement and the shipped product is not single-process.

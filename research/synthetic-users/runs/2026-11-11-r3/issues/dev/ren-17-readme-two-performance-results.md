# The README states two different results for the same benchmark — 12.3 % of a core vs ananicy-cpp's 13.7 %, and "6.0× cheaper … 2.28 % vs 13.7 %" — in one document

- severity: minor
- category: docs
- environment: Arch Linux cloud image, kernel 7.2.11-arch1-1, aranicy 0.3.0-1 +
  aranicy-collector 0.3.0-1 (build 2026-11-11 09:52 UTC), 2 vCPU / 1958 MiB / no display

## What I was trying to do

Decide whether this is cheap enough to run on a 2 vCPU build box, from the
shipped README. The package is a replacement for something I already ran, so the
comparison against ananicy-cpp is the number I care about.

## Steps to reproduce

```sh
sudo pacman -U --noconfirm ~/payload/aranicy-collector-0.3.0-1-x86_64.pkg.tar.zst \
                            ~/payload/aranicy-0.3.0-1-x86_64.pkg.tar.zst
grep -n "12.3% of one core\|6.0× cheaper" /usr/share/doc/aranicy/README.md
sed -n '10,24p' /usr/share/doc/aranicy/README.md
sed -n '46,56p' /usr/share/doc/aranicy/README.md
```

## Expected

One number for one measurement, or two clearly labelled measurements with their
conditions.

## Actual

The intro says the package (daemon *and* collector) measures 12.3 % of one core
at 2 000 execs/s against ananicy-cpp's 13.7 % — about 1.1× cheaper, and it says
the older 2.28 % figure "describe[s] the retired single-process architecture and
[is] kept for comparison only". The Status section, twenty lines later, still
says "**6.0× cheaper than ananicy-cpp under a 2 000 execs/s spawn storm (2.28 %
vs 13.7 % of one core)**".

Those two sentences cannot both be current: 2.28 % and 12.3 % are the same
measurement of the same thing at the same rate.

## Evidence

```
$ grep -n "12.3% of one core\|6.0× cheaper" /usr/share/doc/aranicy/README.md
17:installs — the daemon *and* its collector**: measured **12.3% of one core at
50:Implemented and benchmarked (2026-10-22): **6.0× cheaper than ananicy-cpp
```

```
$ sed -n '10,24p' /usr/share/doc/aranicy/README.md
--- lines 10-24:
Why it exists: ananicy-cpp burns more than one core under heavy process
spawning (measured: 13.7% of one core at 2 000 execs/s and 44.9% at 12 000,
with algorithmic amplifiers waiting at higher rates — see
[docs/BENCHMARKS.md](docs/BENCHMARKS.md)). The measured Rust candidate
(`JohnRTitor/ananicy-rs`) is worse on this kernel: its netlink build spins a
full core at idle and its eBPF path never delivers events. aranicy is a
clean-room rewrite with a hard performance budget for **what the package
installs — the daemon *and* its collector**: measured **12.3% of one core at
2 000 execs/s** (6.1 + 6.2) on the benchmark host, against ananicy-cpp's
13.7% for one process. The 2.28% and 24.4 k instructions-per-exec figures in
BENCHMARKS.md describe the retired single-process architecture and are kept
for comparison only.
```

```
$ sed -n '46,56p' /usr/share/doc/aranicy/README.md
--- lines 46,56:
([docs/COMPATIBILITY.md](docs/COMPATIBILITY.md)).

## Status

Implemented and benchmarked (2026-10-22): **6.0× cheaper than ananicy-cpp
under a 2 000 execs/s spawn storm (2.28% vs 13.7% of one core)**, applying
policies at exec time. Read that number with its conditions: a uniform paced
storm on an otherwise idle host, daemon only, no rule corpus. Real workloads
differ — a parallel build delivers 2 000-5 000 execs/s in clustered bursts,
the bias engine's window work scales with the observed task count, and on a
2-vCPU box the same absolute cost is a larger share of a core. The budget is
```

(The `--- lines …` markers are the `echo` labels from the command line I ran;
the text between them is the file's own output.)

The previous build did not contradict itself — it stated a budget and a result
that fit inside it — so the contradiction came in with the sentence that quotes
12.3 %:

```
$ grep -n "5% of one core\|13.7%" ~/x/prev/usr/share/doc/aranicy/README.md
11:spawning (measured: 13.7% of one core at 2 000 execs/s and 44.9% at 12 000,
16:clean-room rewrite with a hard performance budget: **≤ 5% of one core at
47:under a 2 000 execs/s spawn storm (2.28% vs 13.7% of one core)**, applying
```

## What I could not determine

- Which number is true. I did not benchmark either build (2 vCPU box, and the
  measurement protocol in `BENCHMARKS.md` asks for conditions I cannot
  reproduce here: a paced storm on an otherwise idle host). I am reporting that
  the document contradicts itself, not which half is wrong.
- Whether `BENCHMARKS.md` (installed at `docs/BENCHMARKS.md` under the current
  build) resolves the ambiguity; it is 25 kB of protocol and results and I did
  not audit it.
- Whether the 12.3 % figure is inside a budget. The current README calls it "a
  hard performance budget" in the same sentence without stating a number, and
  the number the previous build stated (≤ 5 % of one core) is not met by
  12.3 %. I cannot tell whether the budget moved or the claim did.

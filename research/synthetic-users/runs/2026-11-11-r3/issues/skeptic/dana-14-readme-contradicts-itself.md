# The shipped README still headlines "6.0× cheaper ... 2.28% of one core" in its Status section, 30 lines after its own opening paragraph calls that same 2.28% a retired-architecture figure

- severity: minor
- category: docs
- environment: Linux 7.2.11-arch1-1 (Arch rolling), aranicy 0.3.0-1 + aranicy-collector 0.3.0-1, 2 vCPU x86_64, 1958 MiB

## What I was trying to do

Read the installed README end to end to find the number I am supposed to hold
the package to (round 2 filed this as dana-10 against the previous build, whose
README claimed "≤ 5% of one core at 2 000 execs/s"). The opening paragraph has
been rewritten in this build, so I followed it to the Status section.

## Steps to reproduce

```
sed -n '13,21p' /usr/share/doc/aranicy/README.md
sed -n '49,60p' /usr/share/doc/aranicy/README.md
```

## Expected

One number per claim, or an explicit statement of which number applies to the
shipped package. The new opening paragraph does the right thing: it names
12.3 % for "what the package installs — the daemon *and* its collector" and
says the 2.28 % / 24.4 k figures "describe the retired single-process
architecture and are kept for comparison only".

## Actual

The Status section, in the same file, still leads with the retired figure and
labels it as the product's result:

```
Implemented and benchmarked (2026-10-22): **6.0× cheaper than ananicy-cpp
under a 2 000 execs/s spawn storm (2.28% vs 13.7% of one core)**, applying
policies at exec time.
```

A reader who skips the header (which is what a Status section invites) takes
2.28 % as the number for the thing they installed; every other shipped surface
(`BENCHMARKS.md`, the new header) now says 12.3 %. That is the same defect as
dana-10 with the polarity reversed: the honest number was added at the top and
the optimistic one was left standing below it, so the document argues with
itself.

## Evidence

```
$ grep -n -E "5%|2\.28|12\.3|6\.0" /usr/share/doc/aranicy/README.md
17:installs — the daemon *and* its collector**: measured **12.3% of one core at
19:13.7% for one process. The 2.28% and 24.4 k instructions-per-exec figures in
50:Implemented and benchmarked (2026-10-22): **6.0× cheaper than ananicy-cpp
51:under a 2 000 execs/s spawn storm (2.28% vs 13.7% of one core)**, applying
```

```
$ sed -n "50,54p" /usr/share/doc/aranicy/README.md
Implemented and benchmarked (2026-10-22): **6.0× cheaper than ananicy-cpp
under a 2 000 execs/s spawn storm (2.28% vs 13.7% of one core)**, applying
policies at exec time. Read that number with its conditions: a uniform paced
storm on an otherwise idle host, daemon only, no rule corpus. Real workloads
```

(Those five lines are the first five of the `## Status` section; I quoted from
line 50 to keep the heading out of this report's own heading structure.)

Note the qualifier "daemon only" does appear in the Status paragraph — but the
sentence it qualifies is still written as a competitive result against
ananicy-cpp's whole process, which is the comparison `BENCHMARKS.md` now says
is not directly comparable.

## What I could not determine

- Which of the two numbers the release is actually gated on. `BENCHMARKS.md`
  says the ≤5 %/24.4 k/`cs ≈ 1.0` invariants are historical and that the pair
  costs 12.3 %; the README agrees in one place and not the other.
- The 2 000 execs/s figure itself: I could not produce a 2 000/s paced storm
  from a shell loop (I measured 2 858/s and 3 786/s in round 2, and `perf` is
  not installed, so I have no instructions-per-exec number of my own). I make
  no claim about the cost, only about two shipped documents disagreeing.

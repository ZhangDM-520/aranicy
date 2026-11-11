# The documented "≤ 5% of one core at 2 000 execs/s" budget is not reachable on a 2 vCPU box: the pair costs 17–19% of a core

- severity: minor
- category: docs
- environment: Arch Linux cloud image, kernel 7.2.11-arch1-1, aranicy 0.3.0-1 +
  aranicy-collector 0.3.0-1 (local/pacman), 2 vCPU / 1958 MiB / no display

## What I was trying to do

Decide whether I can leave this running while a `-j6` build saturates both cores.
The README states the budget explicitly: "**≤ 5% of one core at 2 000 execs/s**",
and "6.0× cheaper than ananicy-cpp under a 2 000 execs/s spawn storm (2.28% vs
13.7% of one core)". On a 2 vCPU machine that number is the whole argument for
using this over ananicy-cpp, so I measured it.

## Steps to reproduce

Measure the two daemons' CPU time from `/proc` while a process-spawning load runs.
`storm.py` forks/execs `/bin/true` in batches of 32; `rate.sh` paces execs to a
fixed rate. Both read `/proc/<pid>/stat` fields 14+15 before and after.

```sh
# idle reference
tick=$(getconf CLK_TCK); da=$(pgrep -f "^/usr/bin/aranicy start"); dc=$(pgrep -f aranicy-collectord)
a0=$(awk '{print $14+$15}' /proc/$da/stat); c0=$(awk '{print $14+$15}' /proc/$dc/stat); sleep 20
a1=$(awk '{print $14+$15}' /proc/$da/stat); c1=$(awk '{print $14+$15}' /proc/$dc/stat)
# then the same subtraction around a 10 s storm

# storm: two workers, ~1 650 execs/s
for i in 1 2; do python3 ~/bench/storm.py 10 & done; wait
# paced: 200 execs/s
python3 ~/bench/storm_rate.py 10 200
```

## Expected

The whole package, at the documented rate, inside the documented budget (≤ 5% of
one core at 2 000 execs/s), or a README that states the honest number for the
two-process v0.3.0 layout.

## Actual

Idle is cheap (daemon 0.20%, collector 0.75% of one core), but as soon as exec
activity exists the pair costs 17–19% of one core, and **the collector alone
exceeds the whole documented budget** at every non-idle rate I measured,
reaching ~13–15% of a core around the documented 2 000/s.

Measured (10 s windows, `elapsed` from wall clock, CPU from `/proc/stat` ticks):

| load | daemon | collector |
|---|---|---|
| idle | 0.20% | 0.75% |
| 50 execs/s (paced) | 1.30% | 2.60% |
| 200 execs/s (paced) | 4.30% | 7.70% |
| 395 execs/s (paced) | 7.20% | 9.70% |
| 1 644 execs/s (2 workers) | 5.10% | 13.40% |
| 3 731 execs/s (1 worker) | 4.10% | 14.70% |
| 4 041 execs/s (4 workers) | 3.40% | 13.70% |

## Evidence

```
$ ~/bench/storms.sh 10 2
822
822
elapsed=10.05s daemon_cpu=0.510s (5.1% of one core) collector_cpu=1.350s (13.4% of one core)

$ ~/bench/storms.sh 10 1
3731
elapsed=10.02s daemon_cpu=0.410s (4.1% of one core) collector_cpu=1.470s (14.7% of one core)

$ ~/bench/storms.sh 10 4
1011
1030
989
1011
elapsed=10.06s daemon_cpu=0.340s (3.4% of one core) collector_cpu=1.380s (13.7% of one core)

$ ~/bench/rate.sh 10 50
rate=50/s daemon=1.30% collector=2.60% of one core

$ ~/bench/rate.sh 10 200
rate=200/s daemon=4.30% collector=7.70% of one core

$ ~/bench/rate.sh 10 395
rate=395/s daemon=7.20% collector=9.70% of one core

$ idle reference (20 s)
idle 20s: daemon 0.040s (0.20%), collector 0.150s (0.75%)
```

The collector is not optional: `aranicy.service` declares
`Requires=aranicy-collector.service`, so its cost is always part of the
package's cost.

## What I could not determine

- Whether the 2.28% figure was measured on a machine with more cores, where the
  daemon and collector do not fight the spawner for CPU. My machine has 2 vCPUs
  and my spawner is a Python loop, not the project's `tools/storm`, so these
  absolute percentages are not the project's measurement protocol and I am not
  claiming a regression in the code — only that the documented budget is not
  reproducible on this machine, in the shape the package ships today.
- Why the per-exec cost is not monotonic in rate (the 395/s run costs more per
  exec than the 1 644/s run did). I did not instrument the internals; with 2
  vCPUs the CPU-time attribution of the samplers and the storm is confounded.
- Whether the collector can be run disabled or replaced by nothing for a
  rules-only user; no documentation mentions it as optional, and the unit
  requires it.

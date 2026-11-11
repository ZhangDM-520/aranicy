# The shipped two-service system costs 9.6% of one core at 1 732 execs/s, about twice the documented "≤ 5% at 2 000 execs/s" budget

- severity: minor
- category: docs
- environment: Linux 7.2.11-arch1-1, aranicy 0.3.0-1 + aranicy-collector 0.3.0-1, 2 vCPU x86_64, 1958 MiB, CLK_TCK=100, one rule loaded

## What I was trying to do

Reproduce the README's hard performance budget: "≤ 5% of one core at 2 000 execs/s" and
"6.0× cheaper than ananicy-cpp under a 2 000 execs/s spawn storm (2.28% vs 13.7% of one
core)". A user cannot install the daemon without the collector (`Requires=`), so the
budget has to hold for both processes.

## Steps to reproduce

```
sudo pacman -U --noconfirm ~/payload/*.pkg.tar.zst && sudo systemctl enable --now aranicy
cat > /etc/ananicy.d/t.rules <<< '{"name": "skeptictest", "nice": 12}' && sudo systemctl reload aranicy
# measure: busiest single-worker fork/exec loop of /bin/true, CPU taken from /proc/<pid>/stat utime+stime
getcpu() { awk '{print $14+$15}' /proc/$1/stat; }
dp=$(pgrep -x aranicy); cp=$(pgrep -x aranicy-collect)
a0=$(getcpu $dp); b0=$(getcpu $cp); s0=$(date +%s%N)
for j in $(seq 1 60000); do /bin/true; done
s1=$(date +%s%N); a1=$(getcpu $dp); b1=$(getcpu $cp)
```

## Expected

At ~2 000 execs/s the two services together stay at or under 5% of one core, and the
2.28% figure is recognisable.

## Actual

1 732 execs/s: daemon 2.40%, collector 7.22%, **9.61% of one core** in total.
3 316 execs/s: daemon 3.43%, collector 12.22%, **15.64%** in total.
The daemon alone is in the documented range; the collector — which the package forces
into the install — is three times more expensive than the daemon and takes the pair to
about twice the stated budget. The README's acceptance sentence never mentions that a
second process is doing most of the work.

## Evidence

```
--- single worker 60000 execs ---
execs=60000 wall=34.65s rate=1732/s daemon=2.40% coll=7.22% sum=9.61% of one core
--- two workers 30000 each ---
execs=60000 wall=18.09s rate=3316/s daemon=3.43% coll=12.22% sum=15.64% of one core
```

Idle baseline for the same pids (20 s, same method): `daemon_ticks=1 coll_ticks=4` at
`HZ=100`, i.e. 0.05% and 0.2% — the numbers above are the spawn storm, not a constant cost.

## What I could not determine

- Spread. I ran each cell once (the box is 2 vCPU and each cell is 20-35 s of saturation).
  "2× over budget" is far outside tick quantisation (±0.01 s over 35 s = ±0.03%), but a
  single sample per cell is a single sample; treat the ratio, not the third digit, as the
  finding.
- The 13.7% ananicy-cpp comparison: ananicy-cpp is not installed and I did not install it,
  so I have no comparison and make no claim about the "6×" ratio.
- Whether my storm is representative: `/bin/true` is the cheapest possible exec, one rule
  is a small corpus, and both processes share a saturated 2-vCPU box, which distorts
  wall-clock-adjacent behaviour (though not the utime+stime ratios being reported).

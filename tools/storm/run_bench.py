#!/usr/bin/env python3
"""run_bench.py — benchmark orchestrator for auto-nice daemon comparisons.

Runs the storm generator for a warm-up plus N repetitions while sampling a
daemon's CPU consumption from /proc/<pid>/stat (utime+stime), reporting:
  - cpu_pct: mean over the whole rep window (100 = one full core)
  - max_1s_pct: worst 1-second window (spike visibility)
  - rss_kb: peak VmRSS observed
  - achieved storm rate from the generator's own RESULT line

A --pid of "none" runs a no-daemon control (harness overhead anchor).
"""
import argparse
import json
import os
import re
import statistics
import subprocess
import sys
import threading
import time

CLK = os.sysconf("SC_CLK_TCK")


def read_ticks(pid):
    with open(f"/proc/{pid}/stat") as f:
        data = f.read()
    fields = data[data.rfind(")") + 2:].split()
    return int(fields[11]) + int(fields[12])  # utime + stime


def read_rss_kb(pid):
    try:
        with open(f"/proc/{pid}/status") as f:
            for line in f:
                if line.startswith("VmRSS:"):
                    return int(line.split()[1])
    except FileNotFoundError:
        return 0
    return 0


class Sampler(threading.Thread):
    """1 Hz tick/RSS sampler running alongside the storm."""

    def __init__(self, pid):
        super().__init__(daemon=True)
        self.pid = pid
        self.ticks = []
        self.rss_peak = 0
        self.lost = False

    def run(self):
        try:
            self.ticks.append((time.monotonic(), read_ticks(self.pid)))
            while True:
                time.sleep(1.0)
                self.ticks.append((time.monotonic(), read_ticks(self.pid)))
                self.rss_peak = max(self.rss_peak, read_rss_kb(self.pid))
        except (FileNotFoundError, ProcessLookupError):
            self.lost = True


def run_storm(args, duration):
    cmd = [args.storm,
           "--rate", str(args.rate),
           "--duration", str(duration),
           "--workers", str(args.workers),
           "--argv0", args.argv0,
           "--target", args.target]
    out = subprocess.run(cmd, capture_output=True, text=True, check=True).stdout
    m = re.search(r"execs=(\d+) elapsed=([\d.]+) rate=([\d.]+)", out)
    if not m:
        raise RuntimeError(f"unexpected storm output: {out!r}")
    return {"execs": int(m.group(1)), "elapsed": float(m.group(2)),
            "rate": float(m.group(3))}


def one_rep(args):
    pid = None if args.pid == "none" else int(args.pid)
    sampler = Sampler(pid) if pid else None
    t0 = time.monotonic()
    t0_ticks = read_ticks(pid) if pid else 0
    if sampler:
        sampler.start()
    storm = run_storm(args, args.duration)
    t1 = time.monotonic()
    t1_ticks = read_ticks(pid) if pid else 0
    if sampler:
        sampler.join(timeout=2)

    rep = {"storm": storm}
    if pid:
        wall = t1 - t0
        cpu_pct = (t1_ticks - t0_ticks) / CLK / wall * 100.0 if wall > 0 else 0.0
        # 1 Hz deltas from the sampler for spike visibility
        per_1s = []
        ticks = sampler.ticks
        for (ta, ca), (tb, cb) in zip(ticks, ticks[1:]):
            dt = tb - ta
            if dt > 0:
                per_1s.append((cb - ca) / CLK / dt * 100.0)
        rep.update({
            "cpu_pct": round(cpu_pct, 2),
            "max_1s_pct": round(max(per_1s), 2) if per_1s else None,
            "rss_kb": sampler.rss_peak or read_rss_kb(pid),
            "sampler_lost": sampler.lost,
            "wall_s": round(wall, 3),
        })
    return rep


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--pid", required=True, help='daemon pid, or "none" for control')
    ap.add_argument("--rate", type=int, default=2000)
    ap.add_argument("--duration", type=int, default=60)
    ap.add_argument("--warmup", type=int, default=5)
    ap.add_argument("--reps", type=int, default=3)
    ap.add_argument("--workers", type=int, default=4)
    ap.add_argument("--argv0", default="gsa-storm-probe")
    ap.add_argument("--target", default="/bin/true")
    ap.add_argument("--storm", default=os.path.join(os.path.dirname(
        os.path.abspath(__file__)), "storm"))
    ap.add_argument("--label", default="run")
    ap.add_argument("--out", help="also append the summary JSON to this file")
    args = ap.parse_args()

    if args.pid != "none":
        if not os.path.exists(f"/proc/{args.pid}"):
            sys.exit(f"error: pid {args.pid} not found")

    if args.warmup > 0:
        w = run_storm(args, args.warmup)
        print(f"warmup: {w['rate']:.0f} execs/s (discarded)", file=sys.stderr)

    reps = []
    for i in range(args.reps):
        rep = one_rep(args)
        rep["rep"] = i + 1
        reps.append(rep)
        print(f"rep {i + 1}/{args.reps}: " + json.dumps(rep), file=sys.stderr)

    summary = {"label": args.label, "pid": args.pid, "rate_target": args.rate,
               "argv0": args.argv0, "target": args.target,
               "reps": reps}
    if args.pid != "none":
        cpus = [r["cpu_pct"] for r in reps]
        maxs = [r["max_1s_pct"] for r in reps if r["max_1s_pct"] is not None]
        summary["median_cpu_pct"] = round(statistics.median(cpus), 2)
        summary["median_max_1s_pct"] = round(statistics.median(maxs), 2) if maxs else None
        summary["rss_kb"] = max(r["rss_kb"] for r in reps)
    summary["median_rate"] = round(statistics.median(
        r["storm"]["rate"] for r in reps), 1)

    line = json.dumps(summary, sort_keys=True)
    print(line)
    if args.out:
        with open(args.out, "a") as f:
            f.write(line + "\n")


if __name__ == "__main__":
    main()

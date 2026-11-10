#!/usr/bin/env python3
"""run_bench.py — benchmark orchestrator for the two-process aranicy split.

A "cell" is one (idle window, storm window) pair measured over N reps:

  * idle window   — duration+pad seconds with NO storm, perf counting each pid
  * storm window  — the same length, with the fork+exec storm running
  * per-event cost = (storm counter − idle counter) ÷ achieved execs

so per-process marginal cost is separable even though the work is spread over
`aranicyd` and `aranicy-collectord`. Wall-% (utime+stime deltas) and RSS come
from /proc/<pid>/. Without --perf the script is the historical wall-only
runner (no idle window, no perf, no sudo).

Modes
  default     measure the --pid(s) given (repeatable; --pid none = control)
  --perf      add instructions / sched_switch / sys_enter_read /
              sys_enter_write counters per pid (needs `perf` on PATH; uses
              `sudo -n perf` when not root)
  --compare   install a prev and a next binary pair, restart both units between
              the cells, report both cells plus the delta. --compare --dry-run
              prints the exact command sequence and touches nothing.

Output: one aligned table per cell on stdout, followed by a single-line JSON
summary. Reference invariants are printed with every table so a cell is
reproducible without consulting docs/BENCHMARKS.md.
"""
import argparse
import json
import os
import re
import shlex
import shutil
import statistics
import subprocess
import sys
import threading
import time

CLK = os.sysconf("SC_CLK_TCK")

#: Measured per pid, in table order: (column key, perf events to SUM). The
#: idle window is subtracted from the storm window for every one of them.
#:
#: Rust's `UnixStream` read/write go through `recvfrom`/`sendto`, not
#: `read`/`write` (measured: 18 763 recvfrom vs 1 082 read on the daemon, and
#: 18 050 sendto vs 0 write on the collector for 18 000 execs). Counting only
#: `read`/`write` reports 0.00 for the very syscalls this harness exists to
#: watch, so each column sums the file-descriptor flavour and the socket one.
#: A batched transfer is `writev` (write_vectored), so the write column sums
#: three: with only `sendto` counted, a batched collector reports 0.00 writes
#: per exec and the improvement looks like a regression to zero traffic.
PERF_COLS = [
    ("instr", ["instructions"]),
    ("cs", ["sched:sched_switch"]),
    ("read", ["syscalls:sys_enter_read", "syscalls:sys_enter_recvfrom"]),
    ("write", ["syscalls:sys_enter_write", "syscalls:sys_enter_sendto",
               "syscalls:sys_enter_writev"]),
]

#: Advertised single-process invariants (docs/BENCHMARKS.md, 2026-10-23):
#: instructions, context switches and reads per exec event. Wall-% is
#: deliberately absent — it moves with the governor/frequency, these do not.
REFERENCE = {"instr": 24400.0, "cs": 1.0, "read": 1.0}
REFERENCE_WALL_PCT = 3.88  # same round, governor `performance` pinned

#: (label, binary name) in reporting order.
BINARIES = [("daemon", "aranicy"), ("collector", "aranicy-collectord")]
#: (label, systemd unit) in reporting order.
UNITS = [("daemon", "aranicy"), ("collector", "aranicy-collector")]
#: The collector recreates /run/aranicy on start; it must be up before the
#: daemon connects to the events socket.
RESTART_ORDER = ["aranicy-collector", "aranicy"]


# --------------------------------------------------------------------------
# /proc sampling


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


def read_paranoid():
    try:
        with open("/proc/sys/kernel/perf_event_paranoid") as f:
            return f.read().strip()
    except OSError:
        return "?"


def governor():
    try:
        with open("/sys/devices/system/cpu/cpu0/cpufreq/scaling_governor") as f:
            return f.read().strip()
    except OSError:
        return "unknown"


class Sampler(threading.Thread):
    """1 Hz tick/RSS sampler for one pid, running alongside the storm."""

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


def peak_1s(ticks):
    peaks = []
    for (ta, ca), (tb, cb) in zip(ticks, ticks[1:]):
        dt = tb - ta
        if dt > 0:
            peaks.append((cb - ca) / CLK / dt * 100.0)
    return max(peaks) if peaks else None


# --------------------------------------------------------------------------
# perf


def sudo_prefix():
    return [] if os.geteuid() == 0 else ["sudo", "-n"]


def perf_argv(pid, seconds):
    cmd = sudo_prefix() + ["perf", "stat", "-x,", "-p", str(pid)]
    for _, events in PERF_COLS:
        for event in events:
            cmd.extend(["-e", event])
    cmd.extend(["--", "sleep", f"{seconds:g}"])
    return cmd


def parse_perf_csv(text):
    """Map perf event name -> integer counter (None when `<not counted>`)."""
    counters = {}
    for line in text.splitlines():
        parts = line.split(",")
        if len(parts) < 3:
            continue
        event = parts[2].strip()
        if not event:
            continue
        raw = parts[0].strip()
        if raw.startswith("<"):
            counters.setdefault(event, None)
            continue
        try:
            counters[event] = int(raw)
        except ValueError:
            counters.setdefault(event, None)
    return counters


def start_perf(pids, seconds):
    if shutil.which("perf") is None:
        sys.exit("error: --perf needs `perf` on PATH (install linux-tools)")
    return {pid: subprocess.Popen(perf_argv(pid, seconds),
                                  stdout=subprocess.PIPE,
                                  stderr=subprocess.PIPE, text=True)
            for pid in pids}


def wait_perf(procs, phase):
    counters = {}
    errors = []
    for pid, proc in procs.items():
        out, err = proc.communicate()
        if proc.returncode != 0:
            errors.append(f"pid {pid}: perf exited {proc.returncode}: "
                          + ((err or out).strip() or "<no output>"))
            continue
        # perf stat writes its summary to stderr (stdout stays empty); accept
        # either stream so a future perf version cannot silently break us.
        parsed = parse_perf_csv(err + "\n" + out)
        if not parsed:
            errors.append(f"pid {pid}: perf produced no counters: "
                          + ((err or out).strip() or "<no output>"))
            continue
        counters[pid] = parsed
    if errors:
        sys.exit(f"error: perf counters unavailable ({phase} window, "
                 f"perf_event_paranoid={read_paranoid()}):\n  "
                 + "\n  ".join(errors))
    return counters


def perf_idle(pids, seconds):
    """Idle window: identical perf invocation, no storm. Returns (counters, s)."""
    procs = start_perf(pids, seconds)
    t0 = time.monotonic()
    counters = wait_perf(procs, "idle")
    return counters, max(time.monotonic() - t0, 1e-9)


# --------------------------------------------------------------------------
# storm


def storm_argv(args, duration):
    cmd = [args.storm, "--rate", str(args.rate), "--duration", str(duration),
           "--workers", str(args.workers), "--argv0", args.argv0,
           "--target", args.target]
    if args.burst != 1:
        cmd.extend(["--burst", str(args.burst)])
    return cmd


def run_storm(args, duration):
    try:
        out = subprocess.run(storm_argv(args, duration), capture_output=True,
                             text=True, check=True).stdout
    except FileNotFoundError:
        sys.exit(f"error: storm binary {args.storm} not found "
                 f"(gcc -O2 -o {args.storm} tools/storm/storm.c)")
    except subprocess.CalledProcessError as exc:
        sys.exit(f"error: storm failed ({exc.returncode}): {exc.stderr.strip()}")
    match = re.search(r"execs=(\d+) elapsed=([\d.]+) rate=([\d.]+)", out)
    if not match:
        sys.exit(f"error: unexpected storm output: {out!r}")
    return {"execs": int(match.group(1)), "elapsed": float(match.group(2)),
            "rate": float(match.group(3))}


# --------------------------------------------------------------------------
# one rep = idle window + storm window


def idle_floor(args, specs):
    """Measure the idle floor once per cell.

    It is a property of the build, not of a rep: measured per rep (i.e. right
    after a storm) it captured the previous storm's bias catch-up and swung
    between 22M and 70M instr/s on the same daemon, which made the subtracted
    marginal cost swing by 30%. One measurement, before the first storm, after
    a settle.
    """
    pids = [spec["pid"] for spec in specs if spec["pid"]]
    seconds = args.duration + args.perf_pad
    if not (args.perf and pids):
        return None, None
    if args.settle > 0:
        time.sleep(args.settle)
    # The idle floor sits at ~20M instr/s, so a single 18 s window on a live
    # desktop measured 38M and 73M for the same build on consecutive runs — a
    # +-30% error on a number that is then subtracted from every per-event
    # value. Take three windows and use the per-counter median; report the
    # spread so a delta smaller than it can be called noise.
    samples = []
    for index in range(3):
        if index:
            time.sleep(args.idle_gap)
        print(f"  idle floor {seconds}s (#{index + 1}/3, no storm) ...",
              file=sys.stderr)
        counters, elapsed = perf_idle(pids, seconds)
        samples.append((counters, elapsed))
    merged = {}
    for pid in pids:
        merged[pid] = {}
        for _, events in PERF_COLS:
            for event in events:
                values = [s[0][pid].get(event) for s in samples]
                values = [v for v in values if v is not None]
                if not values:
                    continue
                merged[pid][event] = statistics.median(values)
        spread = []
        for _, events in PERF_COLS:
            for event in events:
                values = [s[0][pid].get(event) for s in samples]
                values = [v for v in values if v]
                if len(values) > 1 and max(values) > 0:
                    spread.append(max(values) / min(values))
        if spread:
            print(f"  idle spread pid {pid}: worst counter varies "
                  f"{max(spread):.2f}x across the three windows", file=sys.stderr)
    return merged, statistics.median([s[1] for s in samples])


def one_rep(args, specs, idle, idle_seconds):
    pids = [spec["pid"] for spec in specs if spec["pid"]]
    seconds = args.duration + args.perf_pad

    samplers = {pid: Sampler(pid) for pid in pids}
    procs = start_perf(pids, seconds) if (args.perf and pids) else {}
    window_start = time.monotonic()
    if procs:
        # Let perf attach before the first event so the window covers it all.
        time.sleep(args.perf_pad / 2.0)

    for sampler in samplers.values():
        sampler.start()
    # Read the tick baseline on the same edge as t0: the numerator is
    # utime+stime over [ticks0, ticks1] and the denominator over [t0, t1].
    # Sampling ticks before the perf attach made wall-% include pad/2 seconds
    # the divisor did not.
    ticks0 = {pid: read_ticks(pid) for pid in pids}
    t0 = time.monotonic()
    storm = run_storm(args, args.duration)
    t1 = time.monotonic()
    ticks1 = {pid: read_ticks(pid) for pid in pids}
    for sampler in samplers.values():
        sampler.join(timeout=2.0)

    storm_counters = None
    if procs:
        spare = seconds - (time.monotonic() - window_start)
        if spare > 0:
            time.sleep(spare)
        storm_counters = wait_perf(procs, "storm")

    if storm["execs"] <= 0:
        sys.exit("error: the storm executed 0 events (nothing to divide by)")

    wall = max(t1 - t0, 1e-9)
    rep = {"storm": storm, "pids": []}
    for spec in specs:
        pid = spec["pid"]
        row = {"label": spec["label"], "pid": pid or "none"}
        if pid:
            row["cpu_pct"] = (ticks1[pid] - ticks0[pid]) / CLK / wall * 100.0
            row["max_1s_pct"] = peak_1s(samplers[pid].ticks)
            row["rss_kb"] = samplers[pid].rss_peak or read_rss_kb(pid)
            row["sampler_lost"] = samplers[pid].lost
            if storm_counters is not None and pid in storm_counters:
                storm_c = storm_counters[pid]
                idle_c = idle[pid]
                for key, events in PERF_COLS:
                    storm_val = 0
                    idle_val = 0
                    for event in events:
                        got = storm_c.get(event)
                        if got is None:
                            sys.exit(f"error: perf could not count {event} on pid {pid} "
                                     f"during the storm window "
                                     f"(perf_event_paranoid={read_paranoid()})")
                        storm_val += got
                        idle_val += idle_c.get(event) or 0
                    row[f"{key}_per_event"] = (storm_val - idle_val) / storm["execs"]
                    row[f"idle_{key}_per_s"] = idle_val / idle_seconds
        rep["pids"].append(row)
    return rep


# --------------------------------------------------------------------------
# aggregation + rendering


def median_of(rows, key):
    values = [row[key] for row in rows if row.get(key) is not None]
    return statistics.median(values) if values else None


MEDIAN_KEYS = [f"{key}_per_event" for key, _ in PERF_COLS] + \
              [f"idle_{key}_per_s" for key, _ in PERF_COLS] + \
              ["cpu_pct", "max_1s_pct", "rss_kb"]


def cell_rows(reps, specs):
    rows = []
    for index, spec in enumerate(specs):
        per = [rep["pids"][index] for rep in reps]
        row = {"label": spec["label"], "pid": spec["pid"] or "none"}
        for key in MEDIAN_KEYS:
            row[key] = median_of(per, key)
        rows.append(row)
    return rows


def total_row(rows):
    total = {"label": "total", "pid": "-"}
    for key in ("instr_per_event", "cs_per_event", "read_per_event",
                "write_per_event", "cpu_pct", "max_1s_pct", "rss_kb"):
        values = [row[key] for row in rows if row.get(key) is not None]
        total[key] = sum(values) if values else None
    return total


def summarize(args, specs, reps):
    measured = [spec for spec in specs if spec["pid"]]
    rows = cell_rows(reps, specs)
    if len(measured) > 1:
        rows.append(total_row(rows))
    return {
        "cell": args.cell_label,
        "mode": "perf" if args.perf else "wall",
        "burst": args.burst,
        "rate_target": args.rate,
        "workers": args.workers,
        "argv0": args.argv0,
        "target": args.target,
        "duration_s": args.duration,
        "reps": len(reps),
        "governor": governor(),
        "median_rate": round(statistics.median(r["storm"]["rate"] for r in reps), 1),
        "execs_median": statistics.median(r["storm"]["execs"] for r in reps),
        "rows": rows,
        "detail": reps,
    }


TABLE_HEADER = (
    f"{'label':<11} {'pid':>8} {'instr/exec':>11} {'cs/exec':>8} "
    f"{'read/exec':>10} {'write/exec':>10} {'wall-%':>7} {'max1s%':>7} "
    f"{'rss_kB':>8} {'idle instr/s':>12}")


def _num(value, width, prec=0, signed=False):
    if value is None:
        return f"{'-':>{width}}"
    spec = f"{'+' if signed else ''}{width},.{prec}f"
    return f"{value:{spec}}"


def _si(value, width):
    if value is None:
        return f"{'-':>{width}}"
    for divisor, suffix in ((1e9, "G"), (1e6, "M"), (1e3, "k")):
        if abs(value) >= divisor:
            return f"{value / divisor:.1f}{suffix}".rjust(width)
    return f"{value:.0f}".rjust(width)


def table_row(row, signed=False):
    return (f"{row['label']:<11} {str(row['pid']):>8} "
            f"{_num(row.get('instr_per_event'), 11, 0, signed)} "
            f"{_num(row.get('cs_per_event'), 8, 2, signed)} "
            f"{_num(row.get('read_per_event'), 10, 2, signed)} "
            f"{_num(row.get('write_per_event'), 10, 2, signed)} "
            f"{_num(row.get('cpu_pct'), 7, 2, signed)} "
            f"{_num(row.get('max_1s_pct'), 7, 1, signed)} "
            f"{_num(row.get('rss_kb'), 8, 0, signed)} "
            f"{_si(row.get('idle_instr_per_s'), 12)}")


def cell_title(cell):
    return (f"== {cell['cell']}  burst={cell['burst']}  rate={cell['rate_target']}/s  "
            f"workers={cell['workers']}  argv0={cell['argv0']}  target={cell['target']}  "
            f"execs={cell['execs_median']:.0f}  achieved={cell['median_rate']:.0f}/s  "
            f"reps={cell['reps']}  governor={cell['governor']}")


def print_cell(cell, signed=False):
    print()
    print(cell_title(cell))
    print(TABLE_HEADER)
    for row in cell["rows"]:
        print(table_row(row, signed))


def print_reference():
    print(f"reference invariants (single-process HEAD, 2026-10-23): "
          f"instr/exec {REFERENCE['instr']:,.0f} | cs/exec {REFERENCE['cs']:.2f} | "
          f"read/exec {REFERENCE['read']:.2f} | wall-% {REFERENCE_WALL_PCT:.2f} "
          f"@ governor performance")
    print("note: instr/exec, cs/exec, read/exec and the idle floors are "
          "governor-independent; wall-% is not.")


def delta_rows(prev_rows, next_rows):
    rows = []
    for prev, nxt in zip(prev_rows, next_rows):
        row = {"label": nxt["label"], "pid": nxt["pid"]}
        for key in ("instr_per_event", "cs_per_event", "read_per_event",
                    "write_per_event", "cpu_pct", "max_1s_pct", "rss_kb",
                    "idle_instr_per_s"):
            before, after = prev.get(key), nxt.get(key)
            row[key] = None if before is None or after is None else after - before
        rows.append(row)
    return rows


# --------------------------------------------------------------------------
# compare mode: install a binary pair, restart the units, measure, repeat


class Runner:
    """Runs system commands, or only prints them in --dry-run mode."""

    def __init__(self, dry):
        self.dry = dry

    def run(self, cmd, check=True):
        if self.dry:
            print("+ " + shlex.join(cmd))
            return subprocess.CompletedProcess(cmd, 0, "", "")
        proc = subprocess.run(cmd, capture_output=True, text=True)
        if check and proc.returncode != 0:
            raise RuntimeError(f"{shlex.join(cmd)} failed ({proc.returncode}): "
                               f"{(proc.stderr or proc.stdout).strip()}")
        return proc


def systemctl_argv():
    return sudo_prefix() + ["systemctl"]


def journal_argv():
    return sudo_prefix() + ["journalctl"]


def pair_files(directory, suffix):
    return {binary: os.path.join(directory, binary + suffix)
            for _, binary in BINARIES}


def install_pair(runner, pair):
    for binary, src in pair.items():
        dest = os.path.join("/usr/bin", binary)
        if runner.dry:
            print("+ " + shlex.join(sudo_prefix() + ["install", "-m", "0755",
                                                     src, dest])
                  + ("    # already in place, skipped at run time"
                     if os.path.realpath(src) == os.path.realpath(dest) else ""))
            continue
        if not os.path.exists(src):
            raise RuntimeError(f"binary pair file not found: {src}")
        if os.path.realpath(src) == os.path.realpath(dest):
            continue
        runner.run(sudo_prefix() + ["install", "-m", "0755", src, dest])


def wait_active(runner, unit, timeout):
    cmd = systemctl_argv() + ["is-active", "--quiet", unit]
    if runner.dry:
        print("+ " + shlex.join(cmd) + f"    # poll up to {timeout:g} s")
        return
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if subprocess.run(cmd, capture_output=True).returncode == 0:
            return
        time.sleep(0.25)
    raise RuntimeError(f"{unit} did not become active within {timeout:g} s")


def wait_sampler_live(runner, since, timeout):
    cmd = journal_argv() + ["-u", "aranicy", "--since", since,
                            "--no-pager", "-o", "cat"]
    if runner.dry:
        print("+ " + shlex.join(cmd) +
              " | grep -q 'stats sampler live'    # poll up to "
              f"{timeout:g} s")
        return
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        out = subprocess.run(cmd, capture_output=True, text=True).stdout
        if "stats sampler live" in out:
            return
        time.sleep(0.5)
    raise RuntimeError(f"daemon did not log 'stats sampler live' within "
                       f"{timeout:g} s of the restart")


def restart_and_wait(runner, args):
    since = time.strftime("%Y-%m-%d %H:%M:%S")
    for unit in RESTART_ORDER:
        runner.run(systemctl_argv() + ["restart", unit])
    for _, unit in UNITS:
        wait_active(runner, unit, args.active_timeout)
    wait_sampler_live(runner, since, args.sampler_timeout)


def resolve_unit_pids(args):
    specs = []
    for label, unit in UNITS:
        proc = subprocess.run(systemctl_argv() + ["show", "-p", "MainPID",
                                                  "--value", unit],
                              capture_output=True, text=True)
        pid = int((proc.stdout or "0").strip() or 0)
        if proc.returncode != 0 or pid <= 0:
            raise RuntimeError(f"{unit} has no MainPID (is it running?)")
        specs.append({"pid": pid, "label": label})
    return specs


def measure_installed_cell(runner, args, pair, name):
    print(f"-- {name} pair: {pair['aranicy']} + {pair['aranicy-collectord']}",
          file=sys.stderr)
    if not runner.dry:
        for path in pair.values():
            if not os.path.exists(path):
                raise RuntimeError(f"binary pair file not found: {path}")
    install_pair(runner, pair)
    restart_and_wait(runner, args)
    if not runner.dry and args.restart_settle > 0:
        # A fresh daemon is not idle: it restores history, re-applies the rule
        # corpus to every live pid and fills its first windows. Measuring the
        # idle floor during that burst subtracted 52M instr/s instead of the
        # real ~20M, which understated every per-event number by ~25%.
        print(f"  post-restart settle {args.restart_settle:g}s ...",
              file=sys.stderr)
        time.sleep(args.restart_settle)
    if runner.dry:
        show = systemctl_argv() + ["show", "-p", "MainPID", "--value",
                                   "aranicy", "aranicy-collector"]
        print("# pids   : " + shlex.join(show) + "    # resolved at run time")
        if args.warmup > 0:
            print("# warmup : " + shlex.join(storm_argv(args, args.warmup)))
        if args.perf:
            for label, _ in UNITS:
                print("# perf   : " + shlex.join(
                    perf_argv(f"<{label}-pid>", args.duration + args.perf_pad)))
        print("# storm  : " + shlex.join(storm_argv(args, args.duration)))
        if args.reps > 1:
            print(f"# ... repeated {args.reps} times")
        return None

    specs = resolve_unit_pids(args)
    args.cell_label = name
    reps = (measure_slope_reps(args, specs) if args.slope
            else measure_reps(args, specs))
    cell = summarize(args, specs, reps)
    cell["cell"] = name
    cell["pair"] = pair
    return cell


def run_compare(args):
    runner = Runner(args.dry_run)
    prev_pair = pair_files(args.prev_dir, args.prev_suffix)
    next_pair = pair_files(args.next_dir, "")
    # Both cells install into /usr/bin, so a candidate read from there would be
    # overwritten by the prev install and the "next" cell would silently
    # measure prev (and destroy the build it was about to compare).
    for binary, src in next_pair.items():
        dest = os.path.join("/usr/bin", binary)
        if os.path.realpath(src) == os.path.realpath(dest):
            sys.exit(
                f"error: --next-dir {args.next_dir} is the install destination; "
                f"installing the prev pair would overwrite the candidate before "
                f"it is measured. Stage the build output elsewhere (e.g. "
                f"/tmp/aranicy-next) and pass that as --next-dir."
            )
    print(f"== compare: prev={prev_pair['aranicy']}  next={next_pair['aranicy']}",
          file=sys.stderr)
    print(f"== compare: prev={prev_pair['aranicy-collectord']}  "
          f"next={next_pair['aranicy-collectord']}", file=sys.stderr)
    cells = {}
    try:
        cells["prev"] = measure_installed_cell(runner, args, prev_pair, "prev")
        cells["next"] = measure_installed_cell(runner, args, next_pair, "next")
    except Exception as exc:  # noqa: BLE001 - abort loudly, restore, exit 1
        print(f"\nerror: compare aborted: {exc}", file=sys.stderr)
        if not args.dry_run:
            print("restoring the prev pair and restarting the units ...",
                  file=sys.stderr)
            try:
                install_pair(runner, prev_pair)
                restart_and_wait(runner, args)
            except Exception as restore_exc:  # noqa: BLE001
                print(f"error: restore FAILED: {restore_exc}", file=sys.stderr)
                print("UNITS MAY BE DOWN — check: systemctl status aranicy "
                      "aranicy-collector", file=sys.stderr)
        sys.exit(1)

    if args.dry_run:
        return

    print_reference()
    print_cell(cells["prev"])
    print_cell(cells["next"])
    delta = {"cell": "delta (next - prev)",
             "burst": args.burst, "rate_target": args.rate,
             "workers": args.workers, "argv0": args.argv0,
             "target": args.target,
             "execs_median": cells["next"]["execs_median"],
             "median_rate": cells["next"]["median_rate"],
             "reps": args.reps, "governor": cells["next"]["governor"],
             "rows": delta_rows(cells["prev"]["rows"], cells["next"]["rows"])}
    print_cell(delta, signed=True)
    emit_json(args, {"compare": {"prev": cells["prev"], "next": cells["next"],
                                 "delta": delta}})


# --------------------------------------------------------------------------
# wiring


def parse_specs(args):
    labels = {}
    cell_label = "run"
    for raw in args.label:
        if "=" in raw:
            pid_key, name = raw.split("=", 1)
            labels[pid_key.strip()] = name.strip()
        else:
            cell_label = raw.strip()
    specs = []
    for raw in args.pid:
        raw = raw.strip()
        if raw == "none":
            specs.append({"pid": None, "label": "none"})
            continue
        try:
            pid = int(raw)
        except ValueError:
            sys.exit(f"error: --pid {raw!r} is neither a pid nor 'none'")
        specs.append({"pid": pid,
                      "label": labels.get(raw) or labels.get(str(pid))
                      or f"pid{pid}"})
    return specs, cell_label



# --------------------------------------------------------------------------
# slope mode: per-event cost without an idle window
#
# The idle floor is the fragile part of the measurement: it moved 46-64M
# instr/s across three consecutive 18 s windows on the same build (a live
# desktop plus the post-storm row tail), and it is subtracted from every
# per-event number. Two storm rates in the same session remove it entirely:
# the ambient load and the row tail are present in both windows, so
#
#     per_event = (instr(rate_high) - instr(rate_low)) / (execs_high - execs_low)
#
# is a marginal cost with everything constant cancelled.


def slope_argv(args, rate, duration):
    saved = args.rate
    try:
        args.rate = rate
        return storm_argv(args, duration)
    finally:
        args.rate = saved


def run_storm_at(args, rate, duration):
    saved = args.rate
    try:
        args.rate = rate
        return run_storm(args, duration)
    finally:
        args.rate = saved


def measure_slope_rep(args, specs, low_rate, high_rate):
    pids = [spec["pid"] for spec in specs if spec["pid"]]
    seconds = args.duration + args.perf_pad
    windows = {}
    for rate in (low_rate, high_rate):
        time.sleep(1.0)
        procs = start_perf(pids, seconds)
        started = time.monotonic()
        time.sleep(args.perf_pad / 2.0)
        storm = run_storm_at(args, rate, args.duration)
        spare = seconds - (time.monotonic() - started)
        if spare > 0:
            time.sleep(spare)
        windows[rate] = (wait_perf(procs, f"slope@{rate}"), storm, started)
    (low_c, low_s, _), (high_c, high_s, high_start) = windows[low_rate], windows[high_rate]
    wall = max(time.monotonic() - high_start, 1e-9)
    d_events = high_s["execs"] - low_s["execs"]
    if d_events <= 0:
        sys.exit("error: slope mode needs the two rates to deliver different "
                 "exec counts (the storm is probably saturating)")
    rep = {"storm": high_s, "pids": []}
    for spec in specs:
        pid = spec["pid"]
        row = {"label": spec["label"], "pid": pid or "none"}
        if pid:
            storm_c = high_c[pid]
            idle_c = low_c[pid]
            # Wall-% over the high-rate window only; it is informational.
            t1 = read_ticks(pid)
            row["cpu_pct"] = None
            row["max_1s_pct"] = None
            row["rss_kb"] = read_rss_kb(pid)
            row["sampler_lost"] = False
            row["ticks_note"] = t1
            for key, events in PERF_COLS:
                total = 0
                for event in events:
                    high = storm_c.get(event)
                    low = idle_c.get(event)
                    if high is None or low is None:
                        sys.exit(f"error: perf could not count {event} on pid {pid} "
                                 f"in the slope windows "
                                 f"(perf_event_paranoid={read_paranoid()})")
                    total += high - low
                row[f"{key}_per_event"] = total / d_events
        rep["pids"].append(row)
    return rep


def measure_slope_reps(args, specs):
    low_rate = max(1, args.rate // args.slope_divisor)
    if args.warmup > 0 and any(spec["pid"] for spec in specs):
        warm = run_storm(args, args.warmup)
        print(f"warmup: {warm['rate']:.0f} execs/s (discarded)", file=sys.stderr)
    print(f"  slope: {low_rate}/s vs {args.rate}/s, no idle window",
          file=sys.stderr)
    reps = []
    for index in range(args.reps):
        rep = measure_slope_rep(args, specs, low_rate, args.rate)
        rep["rep"] = index + 1
        reps.append(rep)
        print(f"rep {index + 1}/{args.reps}: {json.dumps(rep)}", file=sys.stderr)
    return reps

def measure_reps(args, specs):
    # Idle floor FIRST: a storm leaves thousands of recently-exited tasks in
    # the collector's table, so the observation budget keeps carrying rows for
    # minutes afterwards, and an "idle" window taken after the warmup charges
    # that tail to the floor.
    idle, idle_seconds = idle_floor(args, specs)
    if args.warmup > 0 and any(spec["pid"] for spec in specs):
        warm = run_storm(args, args.warmup)
        print(f"warmup: {warm['rate']:.0f} execs/s (discarded)", file=sys.stderr)
    reps = []
    for index in range(args.reps):
        rep = one_rep(args, specs, idle, idle_seconds)
        rep["rep"] = index + 1
        reps.append(rep)
        print(f"rep {index + 1}/{args.reps}: {json.dumps(rep)}", file=sys.stderr)
    return reps


def emit_json(args, summary):
    line = json.dumps(summary, sort_keys=True)
    print(line)
    if args.out:
        with open(args.out, "a") as handle:
            handle.write(line + "\n")
    if args.json:
        text = json.dumps(summary, sort_keys=True, indent=2)
        if args.json == "-":
            print(text)
        else:
            with open(args.json, "w") as handle:
                handle.write(text + "\n")


def main():
    ap = argparse.ArgumentParser(
        description="two-process storm benchmark for the aranicy split")
    ap.add_argument("--pid", action="append", default=[], metavar="PID",
                    help='pid to measure; repeatable; "none" = no-daemon control')
    ap.add_argument("--label", action="append", default=[], metavar="PID=NAME",
                    help="label a pid (default pid<id>); a lone value sets the "
                         "cell label")
    ap.add_argument("--rate", type=int, default=2000)
    ap.add_argument("--duration", type=int, default=60)
    ap.add_argument("--warmup", type=int, default=5)
    ap.add_argument("--reps", type=int, default=3)
    ap.add_argument("--workers", type=int, default=4)
    ap.add_argument("--argv0", default="gsa-storm-probe")
    ap.add_argument("--target", default="/bin/true")
    ap.add_argument("--burst", type=int, default=1,
                    help="children per burst slot (storm --burst; the average "
                         "rate is unchanged, only the arrivals cluster)")
    ap.add_argument("--storm", default=os.path.join(os.path.dirname(
        os.path.abspath(__file__)), "storm"))
    ap.add_argument("--perf", action="store_true",
                    help="measure instructions/cs/read/write per event "
                         "(idle window subtracted)")
    ap.add_argument("--perf-pad", type=float, default=3.0, metavar="S",
                    help="extra seconds around the storm so perf always covers "
                         "it (default 3)")
    ap.add_argument("--slope", action="store_true",
                    help="derive per-event cost from two storm rates instead of "
                         "subtracting an idle window (more robust on a live host)")
    ap.add_argument("--slope-divisor", type=int, default=4, metavar="N",
                    help="--slope: the low rate is --rate/N (default 4)")
    ap.add_argument("--idle-gap", type=float, default=3.0, metavar="S",
                    help="seconds between the three idle-floor windows (default 3)")
    ap.add_argument("--restart-settle", type=float, default=30.0, metavar="S",
                    help="--compare only: quiet seconds after a restart before the "
                         "idle window, so the re-apply/first-window burst is not "
                         "counted as the idle floor (default 30)")
    ap.add_argument("--settle", type=float, default=6.0, metavar="S",
                    help="quiet seconds before each idle window so the last "
                         "storm's catch-up work is not counted as idle (default 6: "
                         "2 s measured 12-43M instr/s on the same daemon, i.e. a "
                         "still-draining backlog)")
    ap.add_argument("--compare", action="store_true",
                    help="install prev + next binary pairs, restart the units "
                         "between the cells, report both and the delta")
    ap.add_argument("--next-dir", default="/usr/bin")
    ap.add_argument("--prev-dir", default="/usr/bin")
    ap.add_argument("--prev-suffix", default=".prev")
    ap.add_argument("--dry-run", action="store_true",
                    help="--compare only: print the command sequence")
    ap.add_argument("--active-timeout", type=float, default=15.0)
    ap.add_argument("--sampler-timeout", type=float, default=20.0)
    ap.add_argument("--out", help="also append the summary JSON to this file")
    ap.add_argument("--json", metavar="PATH",
                    help='write the whole cell set as pretty JSON ("-" = stdout)')
    args = ap.parse_args()

    if args.dry_run and not args.compare:
        sys.exit("error: --dry-run only applies to --compare")
    if args.burst < 1:
        sys.exit("error: --burst must be >= 1")
    if args.perf_pad <= 0:
        sys.exit("error: --perf-pad must be > 0")
    if args.compare:
        if args.pid:
            print("note: --compare resolves pids from systemd; --pid ignored",
                  file=sys.stderr)
        run_compare(args)
        return

    if not args.pid:
        sys.exit("error: --pid is required (or --compare)")
    specs, cell_label = parse_specs(args)
    args.cell_label = cell_label
    for spec in specs:
        if spec["pid"] and not os.path.exists(f"/proc/{spec['pid']}"):
            sys.exit(f"error: pid {spec['pid']} not found")

    reps = measure_reps(args, specs)
    summary = summarize(args, specs, reps)
    print_reference()
    print_cell(summary)
    emit_json(args, summary)


if __name__ == "__main__":
    main()

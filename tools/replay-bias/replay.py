#!/usr/bin/env python3
"""replay-bias — replay a persisted bias snapshot against a candidate class boundary.

Loads a `bias-state` file (v1 JSON), joins stored `score_ema`/`class` labels
with a candidate boundary, and reports whether an absolute anchor would
preserve them. This is the stage-0 gate from
`docs/reviews/2026-11-08-wave2-design.md` section 7.7, written down so the next
classifier change is measurable instead of asserted.

The labels in the file were produced by the *population* boundary, so the
sweep only shows compatibility, never accuracy — the best threshold recovers
today's `T` almost exactly. The tool says so in its output on purpose.

Usage:
    tools/replay-bias/replay.py [SNAPSHOT] [--anchor N] [--max-violations N]
                                [--keep-tests] [--json]

The host's state file is root-owned: run with sudo (read-only) or pass a copy.

TEST-THREAD POLLUTION. `cargo test` names its threads after the test path, and
libtest truncates that to TASK_COMM_LEN-1, so the history fills with entries
like `a_pass_ages_wh` / `bias::tests:`. They classify BULK at scores ~200 and
drag `T` down. They are filtered by default; `--keep-tests` keeps them so the
effect can be shown.
"""

import argparse
import json
import math
import statistics
import sys

# The two-axis boundary, mirrored from crates/aranicy-daemon/src/classify.rs so
# the replay and the live shadow call the same arithmetic (ADR 0003).
STAGE_MIN_NS = 64_000  # cake SEAT_BURST_MIN_NS
HOP_MIN_HZ = 100  # hop floor
BURST_FLOOR_NS = 1_464  # cake handoff floor

# libtest thread names are the test path truncated to 15 characters; the
# detectable shapes are a `::` path, a `tests:` segment, or a 15-char token
# with an underscore that was cut mid-identifier.
TASK_COMM_LEN = 15


def looks_like_test(name: str) -> bool:
    if "::" in name or "tests:" in name:
        return True
    return len(name) >= TASK_COMM_LEN and ("_" in name or name.endswith(":"))


def pct(values, p):
    values = sorted(values)
    if not values:
        return 0.0
    return values[min(len(values) - 1, int(p * (len(values) - 1)))]


def sweep(bulk, responsive, deadband_pct):
    """Best hard threshold and best threshold with the configured deadband."""
    d = deadband_pct / 100
    best_hard = best_band = None
    for t in range(1, 4001):
        hard = sum(1 for x in bulk if x >= t) + sum(1 for x in responsive if x <= t)
        if best_hard is None or hard < best_hard[1]:
            best_hard = (t, hard)
        lo, hi = t * (1 - d), t * (1 + d)
        band = sum(1 for x in bulk if x >= hi) + sum(1 for x in responsive if x <= lo)
        if best_band is None or band < best_band[1]:
            best_band = (t, band)
    return best_hard, best_band


def lean_value(burst_ns, hop_hz):
    """Mirror of `classify::lean`: positive = bulk, negative = responsive."""
    if burst_ns == 0:
        return None
    burst = max(burst_ns, BURST_FLOOR_NS) / STAGE_MIN_NS
    rate = max(hop_hz, HOP_MIN_HZ) / HOP_MIN_HZ
    return math.log2(burst) - math.log2(rate)


def lean_report(cohort, deadband_pct):
    """Re-run the two-axis verdict over a captured population.

    The stored `class` is the *actuated* (post-hysteresis) label, so a
    disagreement here is the same shape the live `bias shadow` counters report
    but not the same denominator: live compares against the pre-hysteresis
    `cand`, some of which is `None` (deadband) and shows up as unilateral.
    """
    deadband = math.log2(1 + deadband_pct / 100)
    agree = disagree = unilateral = no_evidence = 0
    examples = []
    for e in cohort:
        burst = e.get("burst_ns", 0)
        hop = e.get("hop_hz", 0)
        value = lean_value(burst, hop)
        if value is None:
            no_evidence += 1
            continue
        if abs(value) <= deadband:
            continue
        lean_cls = "bulk" if value > 0 else "responsive"
        pop = e.get("class")
        if pop in ("bulk", "responsive"):
            if pop == lean_cls:
                agree += 1
            else:
                disagree += 1
                if len(examples) < 8:
                    examples.append(
                        {
                            "name": e.get("name"),
                            "lean": round(value, 3),
                            "lean_class": lean_cls,
                            "class": pop,
                            "burst_ns": burst,
                            "hop_hz": hop,
                        }
                    )
        else:
            unilateral += 1
    decided = agree + disagree + unilateral
    return {
        "cohort_with_lean_inputs": decided,
        "no_evidence": no_evidence,
        "agree": agree,
        "disagree": disagree,
        "unilateral": unilateral,
        "disagreement_pct": round(100 * disagree / decided, 1) if decided else 0.0,
        "examples": examples,
    }


def main() -> int:
    ap = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter
    )
    ap.add_argument("snapshot", nargs="?", default="/var/lib/aranicy/bias-state")
    ap.add_argument(
        "--anchor",
        type=float,
        default=1185.0,
        help="absolute anchor to test (default 1185, the wave-2 candidate)",
    )
    ap.add_argument("--deadband-pct", type=float, default=15.0)
    ap.add_argument(
        "--max-violations",
        type=int,
        default=None,
        help="exit 1 when the anchor violates more labels than this",
    )
    ap.add_argument("--keep-tests", action="store_true", help="do not filter test threads")
    ap.add_argument("--json", action="store_true", help="machine-readable output")
    args = ap.parse_args()

    try:
        with open(args.snapshot) as f:
            raw = json.load(f)
    except OSError as e:
        print(f"replay-bias: cannot read {args.snapshot}: {e}", file=sys.stderr)
        return 2

    entries = raw.get("entries", [])
    tests = sum(1 for e in entries if looks_like_test(e.get("name", "")))
    kept = entries if args.keep_tests else [e for e in entries if not looks_like_test(e["name"])]
    cohort = [
        e
        for e in kept
        if e.get("confidence", 0.0) > 0.5 and e.get("class") in ("bulk", "responsive")
    ]
    bulk = [e["score_ema"] for e in cohort if e["class"] == "bulk"]
    resp = [e["score_ema"] for e in cohort if e["class"] == "responsive"]

    lo = args.anchor * (1 - args.deadband_pct / 100)
    hi = args.anchor * (1 + args.deadband_pct / 100)
    viol_bulk = sum(1 for x in bulk if x >= hi)
    viol_resp = sum(1 for x in resp if x <= lo)
    viol = viol_bulk + viol_resp
    best_hard, best_band = sweep(bulk, resp, args.deadband_pct)
    lean = lean_report(cohort, args.deadband_pct)

    report = {
        "snapshot": args.snapshot,
        "entries": len(entries),
        "test_like_entries": tests,
        "entries_considered": len(kept),
        "threshold_ema": raw.get("threshold_ema"),
        "cohort": len(cohort),
        "bulk": {
            "n": len(bulk),
            "p10": pct(bulk, 0.1),
            "p50": statistics.median(bulk) if bulk else 0,
            "p90": pct(bulk, 0.9),
        },
        "responsive": {
            "n": len(resp),
            "p10": pct(resp, 0.1),
            "p50": statistics.median(resp) if resp else 0,
            "p90": pct(resp, 0.9),
        },
        "anchor": {
            "value": args.anchor,
            "band": [round(lo, 1), round(hi, 1)],
            "violations_bulk_above_band": viol_bulk,
            "violations_responsive_below_band": viol_resp,
            "violations": viol,
            "violation_pct": round(100 * viol / len(cohort), 2) if cohort else 0.0,
        },
        "best": {
            "hard": {"threshold": best_hard[0], "errors": best_hard[1]},
            "band": {"threshold": best_band[0], "errors": best_band[1]},
        },
        "lean": lean,
    }

    if args.json:
        print(json.dumps(report, indent=2))
    else:
        print(f"{args.snapshot}")
        print(
            f"  entries {len(entries)} (test-like {tests}) -> considered {len(kept)}; "
            f"population T {report['threshold_ema']}"
        )
        print(f"  cohort (confidence > 0.5): {len(cohort)}")
        print(
            f"    bulk       n={len(bulk):4d}  p10={report['bulk']['p10']:.0f} "
            f"p50={report['bulk']['p50']:.0f} p90={report['bulk']['p90']:.0f}"
        )
        print(
            f"    responsive n={len(resp):4d}  p10={report['responsive']['p10']:.0f} "
            f"p50={report['responsive']['p50']:.0f} p90={report['responsive']['p90']:.0f}"
        )
        print(
            f"  anchor {args.anchor:.0f} band [{lo:.0f}, {hi:.0f}]: "
            f"{viol} violations ({report['anchor']['violation_pct']}% of the cohort)"
        )
        print(
            f"  best hard threshold {best_hard[0]} ({best_hard[1]} errors); "
            f"best banded {best_band[0]} ({best_band[1]} errors)"
        )
        print(
            "  note: labels came from the population boundary, so 'best' recovers T; "
            "this is a compatibility check, not accuracy."
        )
        if lean["cohort_with_lean_inputs"] == 0:
            print(
                "  lean axis: no captured burst/hop inputs — run with "
                "bias_shadow = true so the snapshot records them (ADR 0003)."
            )
        else:
            print(
                f"  lean axis: {lean['agree']} agree / {lean['disagree']} disagree "
                f"({lean['disagreement_pct']}%) / {lean['unilateral']} unilateral; "
                f"{lean['no_evidence']} without runtime evidence"
            )
            for ex in lean["examples"]:
                print(
                    f"    {ex['name']}: lean {ex['lean']:+.2f} = {ex['lean_class']}, "
                    f"actuated {ex['class']} (burst {ex['burst_ns']} ns, {ex['hop_hz']} hops/s)"
                )
            print(
                "  note: the stored class is post-hysteresis; the live counters compare "
                "against the pre-hysteresis verdict, so the percentages differ."
            )

    if args.max_violations is not None and viol > args.max_violations:
        print(
            f"replay-bias: {viol} violations > allowed {args.max_violations}",
            file=sys.stderr,
        )
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())

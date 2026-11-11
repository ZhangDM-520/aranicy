#!/usr/bin/env python3
"""Tests for replay-bias — the ADR 0003 offline gate.

Run from the repository root:

    python3 -m unittest discover -s tools/replay-bias -p 'test_*.py'

The gate decides whether a captured population may be scored at all, so the
refusals matter as much as the arithmetic: no marker, a marker with no dated
inputs, and a cohort older than `--max-lean-age` must each be reported, never
silently scored.
"""

import json
import os
import subprocess
import sys
import tempfile
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)

import replay  # noqa: E402


def entry(name, cls, score, burst=0, hop=0, at=0, confidence=0.9):
    return {
        "name": name,
        "class": cls,
        "score_ema": score,
        "confidence": confidence,
        "burst_ns": burst,
        "hop_hz": hop,
        "lean_at": at,
    }


def marker(saved_at, stage=64_000, hop=100, deadband=15):
    return {
        "stage_min_ns": stage,
        "hop_min_hz": hop,
        "deadband_pct": deadband,
        "saved_at": saved_at,
    }


class LeanAxis(unittest.TestCase):
    def test_a_marker_scores_the_cohort(self):
        cohort = [
            entry("cc1", "bulk", 90, burst=5_000_000, hop=1, at=1_000),
            entry("pipewire", "responsive", 900, burst=1_500, hop=4_000, at=1_000),
        ]
        r = replay.lean_report(cohort, marker(1_100), 3_600)
        self.assertEqual(r["cohort_with_lean_inputs"], 2)
        self.assertEqual(r["agree"], 2)
        self.assertEqual(r["disagree"], 0)
        self.assertEqual(r["stale"], 0)
        self.assertEqual(r["disagreement_pct"], 0.0)
        self.assertFalse(r["gate_cohort_ok"], "2 entries is not 200")

    def test_a_disagreement_names_the_entry(self):
        cohort = [entry("odd", "bulk", 90, burst=1_500, hop=4_000, at=1_000)]
        r = replay.lean_report(cohort, marker(1_100), 3_600)
        self.assertEqual(r["disagree"], 1)
        self.assertEqual([e["name"] for e in r["examples"]], ["odd"])

    def test_inputs_older_than_the_max_age_are_excluded(self):
        # The defect this pins: `saved_at` must be the *save* time. With both
        # stamps from the same window the age is 0 and nothing is ever stale.
        cohort = [entry("ancient", "bulk", 90, burst=5_000_000, hop=1, at=1_000)]
        fresh = replay.lean_report(cohort, marker(1_000 + 10), 3_600)
        self.assertEqual(fresh["stale"], 0, "a fresh capture is scored")
        self.assertEqual(fresh["agree"], 1)
        aged = replay.lean_report(cohort, marker(1_000 + 7_200), 3_600)
        self.assertEqual(aged["stale"], 1, "a two-hour-old capture is stale")
        self.assertEqual(aged["cohort_with_lean_inputs"], 0)
        self.assertEqual(aged["agree"], 0)

    def test_a_marker_without_dated_inputs_reports_no_evidence(self):
        cohort = [entry("undated", "bulk", 90, burst=5_000_000, hop=1, at=0)]
        r = replay.lean_report(cohort, marker(2_000), 3_600)
        self.assertEqual(r["no_evidence"], 1)
        self.assertEqual(r["cohort_with_lean_inputs"], 0)

    def test_an_all_stale_cohort_is_not_a_gate_measurement(self):
        # A 0.0 % ratio on a cohort that scored nothing would satisfy ADR
        # 0003's "< 10 % of decided entries" literally, so the tool must say
        # the cohort is empty instead.
        cohort = [entry("old", "bulk", 90, burst=5_000_000, hop=1, at=1_000)]
        r = replay.lean_report(cohort, marker(1_000 + 7_200), 3_600)
        self.assertEqual(r["stale"], 1)
        self.assertIsNone(r["disagreement_pct"], "no decided entries, no ratio")
        self.assertFalse(r["gate_cohort_ok"])

    def test_a_deadband_of_zero_is_a_boundary_not_an_absent_value(self):
        # What this pins is the *echo*: `deadband_pct` is read with an
        # explicit membership test, so 0 survives as 0 instead of falling back
        # to the 15 default (which would silently re-score the capture under a
        # boundary the daemon never ran). The decision band itself is not
        # asserted here — lean = +1.0 is outside any plausible band.
        cohort = [entry("marginal", "bulk", 90, burst=128_000, hop=100, at=1_000)]
        r = replay.lean_report(cohort, marker(1_100, deadband=0), 3_600)
        self.assertEqual(r["boundary"]["deadband_pct"], 0)
        self.assertEqual(r["agree"] + r["disagree"], 1)


class CommandLine(unittest.TestCase):
    def run_replay(self, snapshot, *extra):
        with tempfile.NamedTemporaryFile("w", suffix=".json", delete=False) as f:
            json.dump(snapshot, f)
            path = f.name
        try:
            proc = subprocess.run(
                [sys.executable, os.path.join(HERE, "replay.py"), path, "--json", *extra],
                capture_output=True,
                text=True,
            )
        finally:
            os.unlink(path)
        self.assertEqual(proc.returncode, 0, proc.stderr)
        return json.loads(proc.stdout)

    def snapshot(self, capture, entries):
        return {"version": 1, "threshold_ema": 1000.0, "lean_capture": capture, "entries": entries}

    def test_no_marker_is_reported_as_not_a_capture(self):
        report = self.run_replay(
            self.snapshot(None, [entry("cc1", "bulk", 90, burst=5_000_000, hop=1, at=1_000)])
        )
        self.assertIsNone(report["lean"], "entries without a marker are not scored")

    def test_a_marker_is_scored_through_the_cli(self):
        report = self.run_replay(
            self.snapshot(
                marker(1_100), [entry("cc1", "bulk", 90, burst=5_000_000, hop=1, at=1_000)]
            )
        )
        self.assertIsNotNone(report["lean"])
        self.assertEqual(report["lean"]["agree"], 1)

    def test_the_max_age_flag_reaches_the_report(self):
        snap = self.snapshot(
            marker(1_000 + 7_200), [entry("old", "bulk", 90, burst=5_000_000, hop=1, at=1_000)]
        )
        self.assertEqual(self.run_replay(snap)["lean"]["stale"], 1)
        self.assertEqual(self.run_replay(snap, "--max-lean-age", "10000")["lean"]["stale"], 0)


if __name__ == "__main__":
    unittest.main()

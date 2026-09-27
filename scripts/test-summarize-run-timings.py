#!/usr/bin/env python3
import importlib.util
from pathlib import Path
import unittest

spec = importlib.util.spec_from_file_location("timings", Path(__file__).with_name("summarize-run-timings.py"))
timings = importlib.util.module_from_spec(spec)
spec.loader.exec_module(timings)


class TimingSummaryTests(unittest.TestCase):
    def test_clocks_and_origins_are_not_subtracted_or_combined(self):
        report = timings.summarize([{
            "run_timings": {"available": True, "dropped": 3, "observations": [
                {"phase": "admission", "elapsed_us": n * 1000, "observed_at_ms": 999999} for n in range(1, 21)
            ]},
            "renderer_run_timings": {"observations": [
                {"phase": "admission", "elapsedUs": 3000},
                {"phase": "admission", "elapsedUs": -1},
            ]},
        }])
        host, renderer = report["phases"]
        self.assertEqual((host["p50_ms"], host["p95_ms"]), (10, 19))
        self.assertEqual(renderer["observations"], 1)
        self.assertEqual(renderer["p95_ms"], 3)
        self.assertEqual(report["host_dropped"], 3)
        self.assertEqual(report["host_unavailable_snapshots"], 0)

    def test_missing_observations_remain_unknown(self):
        report = timings.summarize([{}])
        self.assertEqual(report["phases"], [])
        self.assertEqual(report["host_unavailable_snapshots"], 1)

    def test_overlapping_exports_do_not_reweight_the_same_observation(self):
        earlier = {"run_timings": {"process_instance": "p", "available": True, "dropped": 2,
            "observations": [{"sequence": 1, "phase": "admission", "elapsed_us": 1000}]},
            "renderer_run_timings": {"observations": [{"submissionKey": "s", "runKey": "local", "phase": "admission", "elapsedUs": 2000}]}}
        later = {"run_timings": {"process_instance": "p", "available": True, "dropped": 3,
            "observations": [{"sequence": 1, "phase": "admission", "elapsed_us": 1000}, {"sequence": 2, "phase": "admission", "elapsed_us": 100000}]},
            "renderer_run_timings": {"observations": [{"submissionKey": "s", "runKey": "admitted", "phase": "admission", "elapsedUs": 2000}]}}
        report = timings.summarize([earlier, later])
        self.assertEqual(report["host_dropped"], 3)
        self.assertEqual(report["duplicate_observations_removed"], 2)
        self.assertEqual([phase["observations"] for phase in report["phases"]], [2, 1])


if __name__ == "__main__":
    unittest.main()

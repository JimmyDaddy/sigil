#!/usr/bin/env python3
"""Offline tests for the paid DeepSeek cache campaign wrapper."""

from __future__ import annotations

import importlib.util
from pathlib import Path
import sys
import tempfile
import unittest


SCRIPT = Path(__file__).with_name("deepseek-real-cache-conformance.py")
SPEC = importlib.util.spec_from_file_location("deepseek_real_cache_conformance", SCRIPT)
assert SPEC is not None and SPEC.loader is not None
MODULE = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = MODULE
SPEC.loader.exec_module(MODULE)


class DeepSeekRealCacheConformanceTests(unittest.TestCase):
    def test_metrics_parser_requires_and_decodes_one_complete_record(self) -> None:
        record = (
            "DeepSeek cache conformance: requests=6, reservation_usd=0.05397504, "
            "pricing_snapshot=deepseek-v4-flash-usd-2026-07-28, "
            "within_tool_turn=0.9910, cross_turn_resume=0.9920, "
            "warm_pre_compaction=0.9930, second_resume=0.9940, "
            "cumulative_pre_compaction=0.9925, "
            "first_post_compaction=0.0100"
        )

        metrics = MODULE.parse_metrics(record)

        self.assertEqual(metrics["request_count"], 6)
        self.assertEqual(metrics["conservative_reservation_usd"], "0.05397504")
        self.assertEqual(metrics["within_tool_turn_ratio"], 0.991)
        self.assertEqual(metrics["second_resume_ratio"], 0.994)
        with self.assertRaises(MODULE.AdmissionError):
            MODULE.parse_metrics("")
        with self.assertRaises(MODULE.AdmissionError):
            MODULE.parse_metrics(f"{record}\n{record}")

    def test_output_directory_is_confined_below_repo_local_dev(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            allowed = root / ".repo-local-dev" / "campaign"
            self.assertEqual(MODULE.resolve_output_dir(root, allowed), allowed.resolve())
            with self.assertRaises(MODULE.AdmissionError):
                MODULE.resolve_output_dir(root, root / "outside")


if __name__ == "__main__":
    unittest.main()

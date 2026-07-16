#!/usr/bin/env python3
"""Unit tests for the long-session evidence collector contract."""

from __future__ import annotations

import importlib.util
import json
import sys
import unittest
from pathlib import Path


SCRIPT_PATH = Path(__file__).with_name("long-session-evidence.py")
SPEC = importlib.util.spec_from_file_location("long_session_evidence", SCRIPT_PATH)
assert SPEC is not None and SPEC.loader is not None
evidence = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = evidence
SPEC.loader.exec_module(evidence)


def record(scenario: str) -> dict[str, object]:
    return {
        "schema_version": 1,
        "scenario": scenario,
        "scale": 10,
        "elapsed_ms": 2,
        "facts": {"count": 10},
    }


class LongSessionEvidenceTests(unittest.TestCase):
    def test_marker_records_parse_and_validate_in_stable_order(self) -> None:
        values = [record(scenario) for scenario in reversed(sorted(evidence.EXPECTED_SCENARIOS))]
        output = "noise\n" + "\n".join(
            f"test output {evidence.MARKER}{json.dumps(value)}" for value in values
        )

        parsed = evidence.parse_records(output)
        validated = evidence.validate_records(parsed)

        self.assertEqual(
            [value["scenario"] for value in validated],
            sorted(evidence.EXPECTED_SCENARIOS),
        )

    def test_duplicate_missing_and_unknown_scenarios_fail(self) -> None:
        complete = [record(scenario) for scenario in evidence.EXPECTED_SCENARIOS]
        with self.assertRaisesRegex(ValueError, "duplicate evidence scenario"):
            evidence.validate_records(complete + [record(complete[0]["scenario"])])
        with self.assertRaisesRegex(ValueError, "evidence scenarios differ"):
            evidence.validate_records(complete[:-1])
        with self.assertRaisesRegex(ValueError, "evidence scenarios differ"):
            evidence.validate_records(complete + [record("unknown")])

    def test_invalid_schema_measurements_and_facts_fail(self) -> None:
        complete = [record(scenario) for scenario in evidence.EXPECTED_SCENARIOS]
        invalid_schema = [dict(value) for value in complete]
        invalid_schema[0]["schema_version"] = 2
        with self.assertRaisesRegex(ValueError, "schema_version"):
            evidence.validate_records(invalid_schema)

        invalid_measurement = [dict(value) for value in complete]
        invalid_measurement[0]["elapsed_ms"] = -1
        with self.assertRaisesRegex(ValueError, "elapsed_ms"):
            evidence.validate_records(invalid_measurement)

        invalid_facts = [dict(value) for value in complete]
        invalid_facts[0]["facts"] = {"count": "10"}
        with self.assertRaisesRegex(ValueError, "facts"):
            evidence.validate_records(invalid_facts)

    def test_non_object_payload_is_rejected(self) -> None:
        with self.assertRaisesRegex(ValueError, "JSON object"):
            evidence.parse_records(f"{evidence.MARKER}[]")


if __name__ == "__main__":
    unittest.main()

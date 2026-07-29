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
    facts: dict[str, int]
    scale: int
    elapsed_ms = 2
    if scenario == "active_projection_10k":
        scale = 10_000
        facts = {
            "startup_full_scan_count": 1,
            "steady_state_full_scan_count": 0,
            "incremental_append_count": scale,
            "projection_notice_count": scale,
            "changed_family_count": scale,
            "durable_session_entry_count": scale,
            "durable_bytes": 1_000,
            "projection_estimated_bytes": 100,
            "snapshot_count": 2,
            "full_rebuild_count": 1,
            "incremental_apply_count": scale,
            "invalidation_count": 0,
            "coordinator_writer_lock_attempt_count": scale,
            "os_shared_lock_attempt_count": 0,
            "os_exclusive_lock_attempt_count": scale,
            "os_lock_contention_count": 0,
            "os_lock_failure_count": 0,
            "forced_invalidation_rebuild_count": 1,
        }
    elif scenario == "session_writer_10k":
        scale = 10_000
        facts = {
            "append_elapsed_ms": 1,
            "replay_elapsed_ms": 1,
            "full_scan_count": 1,
            "record_count": scale,
            "file_bytes": 1_000,
        }
    elif scenario == "portable_compaction_1k_turns":
        scale = 1_000
        facts = {
            "source_record_count": 2_000,
            "folded_event_count": 1_000,
            "raw_file_bytes_before": 1_000,
            "raw_file_bytes_after": 1_000,
        }
    elif scenario == "timeline_render_5k":
        scale = 5_000
        facts = {
            "full_rebuild_ms": 1,
            "sequential_append_ms": 1,
            "tail_rerender_count": 10,
            "tail_rerender_ms": 1,
            "total_lines": 5_000,
            "prefix_hash_count": 5_000,
        }
    elif scenario == "worker_reactor_idle_10mib_30s":
        scale = 10 * 1024 * 1024
        elapsed_ms = 30_000
        facts = {
            "durable_bytes": scale,
            "durable_entry_count": 430,
            "prompt_tokens": 216_803,
            "context_window_tokens": 985_468,
            "context_utilization_percent": 22,
            "idle_event_wake_count": 0,
            "idle_deadline_count": 0,
            "idle_advancement_count": 0,
            "idle_shared_lock_attempt_count": 0,
            "idle_exclusive_lock_attempt_count": 0,
            "idle_lock_contention_count": 0,
            "idle_lock_failure_count": 0,
            "teardown_event_count": 1,
        }
    else:
        scale = 10
        facts = {"count": 10}
    return {
        "schema_version": 1,
        "scenario": scenario,
        "scale": scale,
        "elapsed_ms": elapsed_ms,
        "facts": facts,
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

        invalid_acceptance = [record(scenario) for scenario in evidence.EXPECTED_SCENARIOS]
        active = next(
            value
            for value in invalid_acceptance
            if value["scenario"] == "active_projection_10k"
        )
        active["facts"] = dict(active["facts"])
        active["facts"]["steady_state_full_scan_count"] = 1
        with self.assertRaisesRegex(ValueError, "acceptance facts failed"):
            evidence.validate_records(invalid_acceptance)

    def test_non_object_payload_is_rejected(self) -> None:
        with self.assertRaisesRegex(ValueError, "JSON object"):
            evidence.parse_records(f"{evidence.MARKER}[]")


if __name__ == "__main__":
    unittest.main()

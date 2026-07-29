#!/usr/bin/env python3
"""Collect release-profile long-session evidence from ignored Rust tests."""

from __future__ import annotations

import argparse
import json
import subprocess
import sys
from pathlib import Path
from typing import Any


MARKER = "SIGIL_LONG_SESSION_EVIDENCE "
SCHEMA_VERSION = 1
EXPECTED_SCENARIOS = {
    "active_projection_10k",
    "session_writer_10k",
    "portable_compaction_1k_turns",
    "timeline_render_5k",
    "worker_reactor_idle_10mib_30s",
}
REQUIRED_FACTS = {
    "active_projection_10k": {
        "startup_full_scan_count",
        "steady_state_full_scan_count",
        "incremental_append_count",
        "projection_notice_count",
        "changed_family_count",
        "durable_session_entry_count",
        "durable_bytes",
        "projection_estimated_bytes",
        "snapshot_count",
        "full_rebuild_count",
        "incremental_apply_count",
        "invalidation_count",
        "coordinator_writer_lock_attempt_count",
        "os_shared_lock_attempt_count",
        "os_exclusive_lock_attempt_count",
        "os_lock_contention_count",
        "os_lock_failure_count",
        "forced_invalidation_rebuild_count",
    },
    "session_writer_10k": {
        "append_elapsed_ms",
        "replay_elapsed_ms",
        "full_scan_count",
        "record_count",
        "file_bytes",
    },
    "portable_compaction_1k_turns": {
        "source_record_count",
        "folded_event_count",
        "raw_file_bytes_before",
        "raw_file_bytes_after",
    },
    "timeline_render_5k": {
        "full_rebuild_ms",
        "sequential_append_ms",
        "tail_rerender_count",
        "tail_rerender_ms",
        "total_lines",
        "prefix_hash_count",
    },
    "worker_reactor_idle_10mib_30s": {
        "durable_bytes",
        "durable_entry_count",
        "prompt_tokens",
        "context_window_tokens",
        "context_utilization_percent",
        "idle_event_wake_count",
        "idle_deadline_count",
        "idle_advancement_count",
        "idle_shared_lock_attempt_count",
        "idle_exclusive_lock_attempt_count",
        "idle_lock_contention_count",
        "idle_lock_failure_count",
        "teardown_event_count",
    },
}
COMMANDS = (
    (
        "active_projection_10k",
        (
            "cargo",
            "test",
            "--locked",
            "--release",
            "-p",
            "sigil-kernel",
            "active_projection_long_session_evidence",
            "--",
            "--ignored",
            "--nocapture",
        ),
    ),
    (
        "session_writer_10k",
        (
            "cargo",
            "test",
            "--locked",
            "--release",
            "-p",
            "sigil-kernel",
            "session_writer_long_session_evidence",
            "--",
            "--ignored",
            "--nocapture",
        ),
    ),
    (
        "portable_compaction_1k_turns",
        (
            "cargo",
            "test",
            "--locked",
            "--release",
            "-p",
            "sigil-kernel",
            "portable_compaction_long_session_evidence",
            "--",
            "--ignored",
            "--nocapture",
        ),
    ),
    (
        "timeline_render_5k",
        (
            "cargo",
            "test",
            "--locked",
            "--release",
            "-p",
            "sigil-tui",
            "timeline_render_store_long_session_evidence",
            "--",
            "--ignored",
            "--nocapture",
        ),
    ),
    (
        "worker_reactor_idle_10mib_30s",
        (
            "cargo",
            "test",
            "--locked",
            "--release",
            "-p",
            "sigil-tui",
            "worker_reactor_idle_long_session_evidence",
            "--",
            "--ignored",
            "--nocapture",
        ),
    ),
)


def parse_records(output: str) -> list[dict[str, Any]]:
    """Parse marker-prefixed JSON evidence records from command output."""
    records: list[dict[str, Any]] = []
    for line in output.splitlines():
        marker_index = line.find(MARKER)
        if marker_index < 0:
            continue
        payload = line[marker_index + len(MARKER) :]
        value = json.loads(payload)
        if not isinstance(value, dict):
            raise ValueError("evidence payload must be a JSON object")
        records.append(value)
    return records


def validate_records(records: list[dict[str, Any]]) -> list[dict[str, Any]]:
    """Validate and deterministically order one complete evidence set."""
    by_scenario: dict[str, dict[str, Any]] = {}
    for record in records:
        if record.get("schema_version") != SCHEMA_VERSION:
            raise ValueError("evidence schema_version must be 1")
        scenario = record.get("scenario")
        if not isinstance(scenario, str) or not scenario:
            raise ValueError("evidence scenario must be a non-empty string")
        if scenario in by_scenario:
            raise ValueError(f"duplicate evidence scenario: {scenario}")
        for field in ("scale", "elapsed_ms"):
            value = record.get(field)
            if not isinstance(value, int) or isinstance(value, bool) or value < 0:
                raise ValueError(f"{scenario} {field} must be a non-negative integer")
        facts = record.get("facts")
        if not isinstance(facts, dict) or not facts:
            raise ValueError(f"{scenario} facts must be a non-empty object")
        if any(
            not isinstance(value, int) or isinstance(value, bool) or value < 0
            for value in facts.values()
        ):
            raise ValueError(f"{scenario} facts must contain non-negative integers")
        missing_facts = REQUIRED_FACTS.get(scenario, set()).difference(facts)
        if missing_facts:
            raise ValueError(
                f"{scenario} facts missing required fields: {sorted(missing_facts)}"
            )
        by_scenario[scenario] = record

    actual = set(by_scenario)
    if actual != EXPECTED_SCENARIOS:
        raise ValueError(
            "evidence scenarios differ: "
            f"expected={sorted(EXPECTED_SCENARIOS)} actual={sorted(actual)}"
        )
    validate_acceptance_facts(by_scenario)
    return [by_scenario[scenario] for scenario in sorted(by_scenario)]


def validate_acceptance_facts(records: dict[str, dict[str, Any]]) -> None:
    """Reject complete-looking evidence that does not prove the release invariants."""
    active = records["active_projection_10k"]
    active_facts = active["facts"]
    if (
        active["scale"] < 10_000
        or active_facts["startup_full_scan_count"] != 1
        or active_facts["steady_state_full_scan_count"] != 0
        or active_facts["incremental_append_count"] != active["scale"]
        or active_facts["projection_notice_count"] != active["scale"]
        or active_facts["changed_family_count"] < active["scale"]
        or active_facts["durable_session_entry_count"] != active["scale"]
        or active_facts["durable_bytes"] <= active_facts["projection_estimated_bytes"]
        or active_facts["full_rebuild_count"] != 1
        or active_facts["incremental_apply_count"] != active["scale"]
        or active_facts["invalidation_count"] != 0
        or active_facts["coordinator_writer_lock_attempt_count"] < active["scale"]
        or active_facts["os_shared_lock_attempt_count"] != 0
        or active_facts["os_exclusive_lock_attempt_count"] < active["scale"]
        or active_facts["os_lock_contention_count"] != 0
        or active_facts["os_lock_failure_count"] != 0
        or active_facts["forced_invalidation_rebuild_count"] != 1
    ):
        raise ValueError("active_projection_10k acceptance facts failed")

    writer = records["session_writer_10k"]
    writer_facts = writer["facts"]
    if (
        writer["scale"] < 10_000
        or writer_facts["full_scan_count"] != 1
        or writer_facts["record_count"] != writer["scale"]
        or writer_facts["file_bytes"] <= 0
    ):
        raise ValueError("session_writer_10k acceptance facts failed")

    compaction = records["portable_compaction_1k_turns"]
    compaction_facts = compaction["facts"]
    if (
        compaction["scale"] < 1_000
        or not (
            0
            < compaction_facts["folded_event_count"]
            < compaction_facts["source_record_count"]
        )
        or compaction_facts["raw_file_bytes_before"]
        != compaction_facts["raw_file_bytes_after"]
    ):
        raise ValueError("portable_compaction_1k_turns acceptance facts failed")

    timeline = records["timeline_render_5k"]
    timeline_facts = timeline["facts"]
    if (
        timeline["scale"] < 5_000
        or timeline_facts["tail_rerender_count"] <= 0
        or timeline_facts["total_lines"] <= 0
        or timeline_facts["prefix_hash_count"] <= 0
    ):
        raise ValueError("timeline_render_5k acceptance facts failed")

    idle = records["worker_reactor_idle_10mib_30s"]
    idle_facts = idle["facts"]
    if (
        idle["scale"] < 10 * 1024 * 1024
        or idle_facts["durable_bytes"] != idle["scale"]
        or idle_facts["durable_entry_count"] < 400
        or idle_facts["prompt_tokens"] != 216_803
        or idle_facts["context_utilization_percent"] != 22
        or idle["elapsed_ms"] < 29_000
        or idle_facts["idle_event_wake_count"] != 0
        or idle_facts["idle_deadline_count"] != 0
        or idle_facts["idle_advancement_count"] != 0
        or idle_facts["idle_shared_lock_attempt_count"] != 0
        or idle_facts["idle_exclusive_lock_attempt_count"] != 0
        or idle_facts["idle_lock_contention_count"] != 0
        or idle_facts["idle_lock_failure_count"] != 0
        or idle_facts["teardown_event_count"] != 1
    ):
        raise ValueError("worker_reactor_idle_10mib_30s acceptance facts failed")


def collect(root: Path) -> list[dict[str, Any]]:
    """Run all release-profile evidence tests and return validated records."""
    records: list[dict[str, Any]] = []
    for scenario, command in COMMANDS:
        print(f"collecting {scenario}: {' '.join(command)}", flush=True)
        completed = subprocess.run(
            command,
            cwd=root,
            check=False,
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
            text=True,
        )
        print(completed.stdout, end="")
        if completed.returncode != 0:
            raise RuntimeError(f"{scenario} evidence command failed")
        command_records = parse_records(completed.stdout)
        if len(command_records) != 1:
            raise ValueError(
                f"{scenario} command emitted {len(command_records)} evidence records"
            )
        records.extend(command_records)
    return validate_records(records)


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    root = Path(__file__).resolve().parent.parent
    try:
        records = collect(root)
        output = args.output if args.output.is_absolute() else root / args.output
        output.parent.mkdir(parents=True, exist_ok=True)
        output.write_text(
            json.dumps(
                {"schema_version": SCHEMA_VERSION, "records": records},
                indent=2,
                sort_keys=True,
            )
            + "\n",
            encoding="utf-8",
        )
    except (OSError, ValueError, RuntimeError, json.JSONDecodeError) as error:
        print(f"long-session evidence failed: {error}", file=sys.stderr)
        return 1
    print(f"long-session evidence written: {output}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

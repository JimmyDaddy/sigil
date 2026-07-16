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
    "session_writer_10k",
    "portable_compaction_1k_turns",
    "timeline_render_5k",
}
COMMANDS = (
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
        by_scenario[scenario] = record

    actual = set(by_scenario)
    if actual != EXPECTED_SCENARIOS:
        raise ValueError(
            "evidence scenarios differ: "
            f"expected={sorted(EXPECTED_SCENARIOS)} actual={sorted(actual)}"
        )
    return [by_scenario[scenario] for scenario in sorted(by_scenario)]


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

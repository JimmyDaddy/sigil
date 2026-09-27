#!/usr/bin/env python3
"""Summarize explicit support exports without mixing timing origins or process clocks."""
from __future__ import annotations

import argparse
from collections import defaultdict
import json
import math
from pathlib import Path


def summarize(bundles: list[dict]) -> dict:
    groups: dict[tuple[str, str], list[float]] = defaultdict(list)
    dropped_by_process: dict[object, int] = {}
    observations_by_identity: dict[tuple, tuple[str, str, float]] = {}
    unavailable = 0
    duplicates = 0
    for bundle_index, bundle in enumerate(bundles):
        native = bundle.get("run_timings", {})
        instance = native.get("process_instance")
        process = instance if instance is not None else ("unidentified", bundle_index)
        dropped_by_process[process] = max(dropped_by_process.get(process, 0), native.get("dropped", 0))
        unavailable += not native.get("available", False)
        for clock, observations, duration in (
            ("host_phase", native.get("observations", []), "elapsed_us"),
            ("renderer_from_input", bundle.get("renderer_run_timings", {}).get("observations", []), "elapsedUs"),
        ):
            for entry_index, entry in enumerate(observations):
                value = entry.get(duration)
                if isinstance(value, (int, float)) and not isinstance(value, bool) and math.isfinite(value) and value >= 0:
                    if clock == "host_phase" and instance is not None and "sequence" in entry:
                        identity = (clock, instance, entry["sequence"])
                    elif clock == "renderer_from_input" and entry.get("submissionKey"):
                        identity = (clock, entry["submissionKey"], entry["phase"])
                    else:
                        # Legacy exports lack stable observation identities. Preserve their
                        # samples without pretending a duration/run label uniquely identifies one.
                        identity = (clock, bundle_index, entry_index)
                    duplicates += identity in observations_by_identity
                    observations_by_identity[identity] = (clock, entry["phase"], value / 1_000)
    for clock, phase, value in observations_by_identity.values():
        groups[(clock, phase)].append(value)
    rows = []
    for (clock, phase), values in sorted(groups.items()):
        values.sort()
        rows.append({
            "clock_and_origin": clock, "phase": phase, "observations": len(values),
            "p50_ms": values[math.ceil(len(values) * .50) - 1],
            "p95_ms": values[math.ceil(len(values) * .95) - 1],
        })
    return {"host_dropped": sum(dropped_by_process.values()), "host_unavailable_snapshots": unavailable, "duplicate_observations_removed": duplicates, "phases": rows}


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("bundles", nargs="+", type=Path)
    args = parser.parse_args()
    bundles = []
    for path in args.bundles:
        if path.stat().st_size > 384 * 1024:
            parser.error("support bundle exceeds 384 KiB")
        bundles.append(json.loads(path.read_text()))
    print(json.dumps(summarize(bundles), indent=2))


if __name__ == "__main__":
    main()

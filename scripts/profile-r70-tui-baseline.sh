#!/usr/bin/env bash
# RFC-0070 R70.0: collect the current TUI phase timing baseline.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
OUTPUT_DIR="${SIGIL_R70_PROFILE_DIR:-$ROOT/.repo-local-dev/r70-baseline/profile}"
mkdir -p "$OUTPUT_DIR"

tests=(
  "timed_frame_path_uses_the_production_present_helper"
  "layout_snapshot_reserves_disclosure_rows_before_timeline_hit_areas"
)

for test_name in "${tests[@]}"; do
  log_path="$OUTPUT_DIR/${test_name}.log"
  echo "profiling $test_name -> $log_path"
  SIGIL_TUI_PHASE_TIMINGS=1 cargo test -p sigil-tui --lib "$test_name" -- --nocapture >"$log_path" 2>&1
done

python3 - "$OUTPUT_DIR" <<'PY'
import re
import sys
from pathlib import Path

output_dir = Path(sys.argv[1])
samples = {}
pattern = re.compile(r"SIGIL_R70_PHASE name=([^ ]+) elapsed_ns=(\d+)")
for path in sorted(output_dir.glob("*.log")):
    for match in pattern.finditer(path.read_text(encoding="utf-8")):
        samples.setdefault(match.group(1), []).append(int(match.group(2)))

report = output_dir / "phase-timing-baseline.md"
with report.open("w", encoding="utf-8") as handle:
    handle.write("# RFC-0070 R70.0 TUI Phase Timing Baseline\n\n")
    handle.write("Instrumentation is opt-in via `SIGIL_TUI_PHASE_TIMINGS=1`; `terminal_present` includes the ratatui draw callback and backend present/flush.\n\n")
    handle.write("| Phase | Samples | Min (us) | Median (us) | Max (us) |\n| --- | ---: | ---: | ---: | ---: |\n")
    for name in sorted(samples):
        values = sorted(samples[name])
        median = values[len(values) // 2]
        handle.write(f"| `{name}` | {len(values)} | {values[0] / 1000:.3f} | {median / 1000:.3f} | {values[-1] / 1000:.3f} |\n")
    if not samples:
        raise SystemExit("phase profiler captured no instrumentation samples")
print(f"wrote {report}")
PY

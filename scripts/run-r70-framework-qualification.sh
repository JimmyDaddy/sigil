#!/usr/bin/env bash
# RFC-0070 R70.2/R70.7: deterministic public-framework qualification workloads.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
OUTPUT_DIR="${SIGIL_R70_BENCH_DIR:-$ROOT/.repo-local-dev/r70-baseline/qualification}"
mkdir -p "$OUTPUT_DIR"
LOG_PATH="$OUTPUT_DIR/framework-qualification.log"
REPORT_PATH="$OUTPUT_DIR/framework-qualification.md"
cd "$ROOT"

cargo test --locked -p sigil-tui --test r70_qualification --release -- --nocapture 2>&1 | tee "$LOG_PATH"

python3 - "$LOG_PATH" "$REPORT_PATH" <<'PY'
from __future__ import annotations

import re
import subprocess
import sys
from pathlib import Path

log_path = Path(sys.argv[1])
report_path = Path(sys.argv[2])
log = log_path.read_text(encoding="utf-8")
benchmarks = re.findall(r"R70_BENCH ([^\n]+)", log)
if not benchmarks:
    raise SystemExit("framework qualification captured no benchmark counters")

commit = subprocess.check_output(
    ["git", "rev-parse", "HEAD"], text=True, cwd=Path.cwd()
).strip()
with report_path.open("w", encoding="utf-8") as report:
    report.write("# RFC-0070 public framework qualification\n\n")
    report.write(f"Implementation commit: `{commit}`\n\n")
    report.write("The release-profile fixture asserts bounded 100k materialization, Fenwick height lookup, dense committed hit testing, resize/theme/Unicode input contracts, and same-target mouse flood dispatch.\n\n")
    report.write("| Counter | Value |\n| --- | --- |\n")
    for benchmark in benchmarks:
        name, _, values = benchmark.partition(" ")
        report.write(f"| `{name}` | {values} |\n")
    report.write("\nNo filesystem, network, or application projection work is performed by the framework hot-path fixture.\n")
PY

echo "r70 framework qualification: release-profile 100k, mouse flood, resize/theme/Unicode and dense-hit workloads passed"

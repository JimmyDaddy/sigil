#!/usr/bin/env bash
# RFC-0071 R71.3: sandbox conformance runner.
# Verifies reserved-environment construction, requested-vs-effective enforcement verification,
# closed backend declarations and toolchain CAS. Zero-case and non-zero exit are refused.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

usage() {
  cat <<'EOF'
Usage: scripts/run-r71-sandbox-conformance.sh
EOF
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    -h|--help) usage; exit 0 ;;
    *) echo "unknown argument: $1" >&2; usage >&2; exit 2 ;;
  esac
done

run_suite() {
  local label="$1"
  shift
  local output
  output=$("$@" 2>&1) || {
    echo "FAIL(conformance/$label): cargo exited non-zero" >&2
    echo "$output" | tail -20 >&2
    exit 1
  }
  local summary
  summary=$(echo "$output" | grep -E 'test result: ok\.' | tail -1 || true)
  if [[ -z "$summary" ]] || echo "$summary" | grep -qE '0 passed'; then
    echo "FAIL(conformance/$label): zero tests ran or missing summary" >&2
    exit 1
  fi
  echo "PASS(conformance/$label): $summary"
}

run_suite sandbox-environment cargo test -p sigil-sandbox --lib environment -- --format terse
run_suite sandbox-receipt cargo test -p sigil-sandbox --lib receipt -- --format terse
run_suite sandbox-backends cargo test -p sigil-sandbox --lib backends -- --format terse
run_suite sandbox-toolchain cargo test -p sigil-sandbox --lib toolchain -- --format terse
run_suite sandbox-local cargo test -p sigil-sandbox --lib local -- --format terse
run_suite sandbox-launch-plan cargo test -p sigil-sandbox --lib launch_plan -- --format terse
run_suite sandbox-principal cargo test -p sigil-sandbox --lib principal -- --format terse
run_suite sandbox-output cargo test -p sigil-sandbox --lib output -- --format terse
echo "r71-sandbox-conformance: all fixtures passed"

#!/usr/bin/env bash
# RFC-0071 R71.4: consumer conformance runner.
# Verifies the frozen mandatory-consumer matrix (family -> exact seam) and cross-purpose
# rejection fixtures. Zero-case and missing summary are refused.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

usage() {
  cat <<'EOF'
Usage: scripts/run-r71-consumer-conformance.sh
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
  if [[ -z "$summary" ]] || echo "$summary" | grep -qE '([^0-9]|^)0 passed'; then
    echo "FAIL(conformance/$label): zero tests ran or missing summary" >&2
    exit 1
  fi
  echo "PASS(conformance/$label): $summary"
}

run_suite consumer-matrix cargo test -p sigil-resource-authority --lib consumer_ports -- --format terse
run_suite process-observer cargo test -p sigil-process-observer --lib -- --format terse
run_suite release-tools-lib cargo test -p sigil-release-tools --lib -- --format terse
echo "r71-consumer-conformance: all fixtures passed"

#!/usr/bin/env bash
# RFC-0071 R71.2: authority conformance runner.
# Runs the isolated-harness authority fixtures (bootstrap fail-closed, lifecycle matrix,
# journal genesis/chain, quota atomic reservation). Refuses zero-case success.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

usage() {
  cat <<'EOF'
Usage: scripts/run-r71-authority-conformance.sh
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
    echo "$output" | tail -30 >&2
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

run_suite authority-bootstrap cargo test -p sigil-resource-authority --lib bootstrap -- --format terse
run_suite authority-arena cargo test -p sigil-resource-authority --lib arena -- --format terse
run_suite authority-lifecycle cargo test -p sigil-resource-authority --lib lease -- --format terse
run_suite authority-journal cargo test -p sigil-resource-authority --lib journal -- --format terse
run_suite authority-quota cargo test -p sigil-resource-authority --lib quota -- --format terse
run_suite authority-identity cargo test -p sigil-resource-authority --lib identity -- --format terse
run_suite authority-maintenance cargo test -p sigil-resource-authority --lib maintenance -- --format terse
run_suite authority-allocator cargo test -p sigil-resource-authority --lib allocator -- --format terse
run_suite authority-borrowed cargo test -p sigil-resource-authority --lib borrowed -- --format terse
run_suite authority-reconcile cargo test -p sigil-resource-authority --lib reconcile -- --format terse
run_suite authority-storage cargo test -p sigil-resource-authority --lib storage -- --format terse
run_suite authority-factory cargo test -p sigil-resource-authority --lib factory -- --format terse
run_suite authority-file-access cargo test -p sigil-resource-authority --lib file_access -- --format terse
echo "r71-authority-conformance: all fixtures passed"

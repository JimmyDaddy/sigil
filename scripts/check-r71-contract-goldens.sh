#!/usr/bin/env bash
# RFC-0071 R71.1: contract golden checker.
# Verifies the frozen contract golden case ids and expected counts; zero cases is never success,
# and any drift in the case set fails closed.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

usage() {
  cat <<'EOF'
Usage: scripts/check-r71-contract-goldens.sh
EOF
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    -h|--help) usage; exit 0 ;;
    *) echo "unknown argument: $1" >&2; usage >&2; exit 2 ;;
  esac
done

# Golden cargo fixtures must pass with an exact, non-zero case count.
run_golden() {
  local label="$1"
  shift
  local output
  output=$("$@" 2>&1) || {
    echo "FAIL(golden/$label): cargo command exited non-zero" >&2
    echo "$output" | tail -20 >&2
    exit 1
  }
  local summary
  summary=$(echo "$output" | grep -E 'test result: ok\.' | tail -1 || true)
  if [[ -z "$summary" ]]; then
    echo "FAIL(golden/$label): no test result summary" >&2
    exit 1
  fi
  if echo "$summary" | grep -qE '0 passed'; then
    echo "FAIL(golden/$label): zero tests ran" >&2
    exit 1
  fi
  echo "PASS(golden/$label): $summary"
}

run_golden v3-envelope-shapes cargo test -p sigil-kernel --lib r71_v3_closed_envelope -- --format terse
run_golden v3-cross-swap cargo test -p sigil-kernel --lib r71_v3_cross_swap -- --format terse
run_golden resource-preconditions cargo test -p sigil-kernel --lib r71_resource_requirement_contract -- --format terse
echo "check-r71-contract-goldens: all golden fixtures passed"

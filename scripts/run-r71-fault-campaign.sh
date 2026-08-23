#!/usr/bin/env bash
# RFC-0071 R71.5: typed fault campaign runner.
# Executes the deterministic recovery fixtures of each campaign family with a zero-test guard.
# Each family must report a non-zero exact test count and zero failures.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

usage() {
  cat <<'EOF'
Usage: scripts/run-r71-fault-campaign.sh
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
    echo "FAIL(campaign/$label): cargo exited non-zero" >&2
    echo "$output" | tail -20 >&2
    exit 1
  }
  local summary
  summary=$(echo "$output" | grep -E 'test result: ok\.' | tail -1 || true)
  if [[ -z "$summary" ]] || echo "$summary" | grep -qE '([^0-9]|^)0 passed'; then
    echo "FAIL(campaign/$label): zero tests ran or missing summary" >&2
    exit 1
  fi
  echo "PASS(campaign/$label): $summary"
}

run_suite recovery-gate cargo test -p sigil-kernel --lib resource_recovery -- --format terse
run_suite fault-jrn cargo test -p sigil-resource-authority --lib r71_f_jrn -- --format terse
run_suite fault-boot cargo test -p sigil-resource-authority --lib r71_f_boot -- --format terse
run_suite fault-rec cargo test -p sigil-resource-authority --lib r71_f_rec -- --format terse
run_suite fault-abr cargo test -p sigil-resource-authority --lib r71_f_abr -- --format terse
run_suite contract-goldens bash scripts/check-r71-contract-goldens.sh
run_suite authority-fixtures bash scripts/run-r71-authority-conformance.sh
echo "r71-fault-campaign: all fixtures passed"

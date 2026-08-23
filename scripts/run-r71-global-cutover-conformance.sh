#!/usr/bin/env bash
# RFC-0071 R71.6: application-global cutover conformance runner.
# The startup cutover manifest is content-addressed; exactly one epoch is selected per
# application instance; a NewCurrentSchema manifest with any failed adapter probe fails
# closed; old-schema sessions are explicitly unavailable; the boot seam refuses partial
# startup. Zero-test and non-zero exit are refused.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

usage() {
  cat <<'EOF'
Usage: scripts/run-r71-global-cutover-conformance.sh
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
    echo "FAIL(conformance/$label): command exited non-zero" >&2
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

# Kernel: cutover manifest contract + current-schema-only session boundary.
run_suite kernel-cutover-manifest cargo test -p sigil-kernel --lib cutover_manifest -- --format terse
run_suite kernel-current-schema-only cargo test -p sigil-kernel --lib current_schema_only -- --format terse
# Sandbox: managed execution service (spawn, truthful Local none, bounded output, fail closed).
run_suite sandbox-managed-execution cargo test -p sigil-sandbox --lib r71_managed -- --format terse
# Runtime: application-global cutover coordinator + boot seam fail-closed behavior.
run_suite runtime-global-cutover cargo test -p sigil-runtime --lib resource_global_cutover -- --format terse
echo "r71-global-cutover-conformance: all fixtures passed"

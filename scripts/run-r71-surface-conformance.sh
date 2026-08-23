#!/usr/bin/env bash
# RFC-0071 R71.5: four-surface conformance runner.
# The same canonical ResourceRecoverySurfaceContractV1 fixture must project losslessly through
# the runtime facade and the generated wire adapters; transport-private state machines or hash
# recomputation are prohibited. Zero-test and non-zero exit are refused.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

usage() {
  cat <<'EOF'
Usage: scripts/run-r71-surface-conformance.sh [--epoch legacy|current]
  --epoch current   additionally runs the R71.6 full-composition gate (red until every
                    mandatory adapter is composed; no partial cutover claim).
EOF
}

EPOCH="legacy"
while [[ $# -gt 0 ]]; do
  case "$1" in
    -h|--help) usage; exit 0 ;;
    --epoch)
      if [[ $# -lt 2 ]]; then echo "--epoch requires a value" >&2; exit 2; fi
      EPOCH="$2"; shift 2 ;;
    --epoch=*) EPOCH="${1#--epoch=}"; shift ;;
    *) echo "unknown argument: $1" >&2; usage >&2; exit 2 ;;
  esac
done
if [[ "$EPOCH" != "legacy" && "$EPOCH" != "current" ]]; then
  echo "--epoch must be legacy or current" >&2; exit 2
fi

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

run_suite surface-facade cargo test -p sigil-runtime --lib r71_facade -- --format terse
run_suite kernel-recovery-surface cargo test -p sigil-kernel --lib resource_recovery_surface -- --format terse
if [[ "$EPOCH" == "current" ]]; then
  echo "--epoch current: running the R71.6 full-composition gate (red until every adapter is wired)"
  run_suite full-composition-gate cargo test -p sigil-runtime --lib r71_full_composition_gate -- --ignored --format terse
fi
echo "r71-surface-conformance: all fixtures passed (epoch=$EPOCH)"

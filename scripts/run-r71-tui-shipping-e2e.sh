#!/usr/bin/env bash
# RFC-0071 R71.9l: shipping TUI bootstrap/current-schema E2E gate.
# The first fixture enters the real runtime boot owner; the authority fixture then exercises the
# same activated workspace through list/grep/read and checks that each result carries a receipt.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

run() {
  local label="$1"
  shift
  local output
  output=$("$@" 2>&1) || {
    echo "FAIL(tui-shipping/$label): command exited non-zero" >&2
    echo "$output" | tail -40 >&2
    exit 1
  }
  local summary
  summary=$(echo "$output" | grep -E 'test result: ok\.' | tail -1 || true)
  if [[ -z "$summary" ]] || echo "$summary" | grep -qE '([^0-9]|^)0 passed'; then
    echo "FAIL(tui-shipping/$label): zero tests ran or missing summary" >&2
    exit 1
  fi
  echo "PASS(tui-shipping/$label): $summary"
}

run current-schema-boot cargo test --locked -p sigil-tui --test r71_shipping_e2e \
  production_boot_current_schema_uses_real_authority_transaction -- --format terse
run launcher-production-replacement cargo test --locked -p sigil-tui --test r71_shipping_e2e \
  production_launcher_replacement_keeps_session_runtime_config -- --format terse
run delete-reconciliation cargo test --locked -p sigil-resource-authority --lib r71_f_del -- --format terse
echo "r71-tui-shipping-e2e: current-schema boot, authority file receipts, replacement and delete fixtures passed"

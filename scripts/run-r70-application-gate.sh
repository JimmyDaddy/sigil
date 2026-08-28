#!/usr/bin/env bash
# RFC-0070 R70.4: application-port exit gate.
#
# This is intentionally a small, deterministic gate.  It exercises the shared application
# contract first, then the runtime projection/page path, the five-surface fixture, and the
# production HTTP/TUI adapters.  A successful unit test in one surface is not allowed to stand
# in for the shared receipt/frontier or production-adapter checks.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

run_suite() {
  local label="$1"
  shift
  local output
  output=$("$@" 2>&1) || {
    echo "FAIL(r70.4/$label): command exited non-zero" >&2
    echo "$output" | tail -40 >&2
    exit 1
  }
  local summary
  summary=$(echo "$output" | grep -E 'test result: ok\.' | tail -1 || true)
  if [[ -z "$summary" ]] || echo "$summary" | grep -qE '([^0-9]|^)0 passed'; then
    echo "FAIL(r70.4/$label): zero tests ran or missing summary" >&2
    exit 1
  fi
  echo "PASS(r70.4/$label): $summary"
}

run_check() {
  local label="$1"
  shift
  local output
  output=$("$@" 2>&1) || {
    echo "FAIL(r70.4/$label): command exited non-zero" >&2
    echo "$output" | tail -40 >&2
    exit 1
  }
  echo "PASS(r70.4/$label): $output"
}

run_check manifest python3 scripts/check-r70-migration-manifest.py
run_check manifest-tests python3 -m unittest scripts/test-r70-migration-manifest.py
run_check package-topology python3 scripts/check-r70-package-topology.py
run_suite application-contract cargo test --locked -p sigil-application --lib -- --format terse
run_suite runtime-projection cargo test --locked -p sigil-runtime --lib application_projection -- --format terse
run_suite runtime-service cargo test --locked -p sigil-runtime --lib application_service -- --format terse
run_suite five-surface cargo test --locked -p sigil-application --lib five_surface_clients_share_frontier_page_and_domain_receipt -- --format terse
run_suite http-production cargo test --locked -p sigil-http --lib production_http_application_client_uses_runtime_projection_page_and_reservation -- --format terse
run_suite tui-production cargo test --locked -p sigil-tui-host --test r71_shipping_e2e -- --format terse
run_suite cold-cache cargo test --locked -p sigil-runtime --lib cold_cache_transcript_page_100k_keeps_the_resident_page_bounded -- --ignored --format terse

echo "r70.4 application gate: all contract, projection, five-surface, production-adapter and cold-cache fixtures passed"

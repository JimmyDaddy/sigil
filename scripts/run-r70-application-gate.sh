#!/usr/bin/env bash
# RFC-0070 R70.4: application-port exit gate.
#
# This is intentionally a small, deterministic gate.  Registration must prove real production
# enum/caller anchors and a ready shared fixture before any qualification command can run.  A
# successful unit test in one surface is not allowed to stand in for the shared receipt/frontier
# or production-adapter checks.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

run_suite() {
  local label="$1"
  shift
  local output
  output=$(python3 "${ROOT}/scripts/run-isolated-tests.py" -- "$@" 2>&1) || {
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
run_check real-surface-registration-tests python3 scripts/run-r70-application-gate.py --self-test
run_check real-surface-registration python3 scripts/run-r70-application-gate.py --check
run_check manifest-tests python3 -m unittest scripts/test-r70-migration-manifest.py
run_check package-topology python3 scripts/check-r70-package-topology.py
run_suite application-unit-contract cargo test --locked -p sigil-application --lib -- --format terse
run_suite runtime-projection cargo test --locked -p sigil-runtime --lib application_projection -- --format terse
run_suite runtime-service cargo test --locked -p sigil-runtime --lib application_service -- --format terse
run_suite cli-entry-evidence cargo test --locked -p sigil --test machine_output_tests json_process_stdout_is_one_parseable_result_and_exit_zero -- --format terse
run_suite http-entry-evidence cargo test --locked -p sigil-http --lib production_http_application_client_uses_runtime_projection_page_and_reservation -- --format terse
run_suite desktop-entry-evidence cargo test --locked -p sigil --test serve_process_tests desktop_typed_client_streams_and_replays_real_run_events -- --format terse
run_suite tui-keyboard-entry-evidence cargo test --locked -p sigil --test session_lifecycle_tui_process_tests real_tui_first_run_without_model_catalog_completes_the_first_request -- --format terse
run_suite cold-cache cargo test --locked -p sigil-runtime --lib cold_cache_transcript_page_100k_keeps_the_resident_page_bounded -- --ignored --format terse

echo "r70.4 application gate: real-surface registration, entry evidence, contract, projection and cold-cache fixtures passed"

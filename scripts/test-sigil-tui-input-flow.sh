#!/usr/bin/env bash
set -euo pipefail

if [[ "$#" -eq 0 ]]; then
  set -- input_flow_tests
fi

export SIGIL_TUI_TEST_SLICE_APP_INPUT_FLOW=1
exec cargo test -p sigil-tui --lib "$@"

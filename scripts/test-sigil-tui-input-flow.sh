#!/usr/bin/env bash
set -euo pipefail

if [[ "$#" -eq 0 ]]; then
  set -- input_flow_tests
fi

repo_root="$(git rev-parse --show-toplevel 2>/dev/null || pwd)"
exec python3 "${repo_root}/scripts/run-isolated-tests.py" -- \
  env SIGIL_TUI_TEST_SLICE_APP_INPUT_FLOW=1 cargo test -p sigil-tui-host --lib "$@"

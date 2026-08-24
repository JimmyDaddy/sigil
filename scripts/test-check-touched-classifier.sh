#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
# shellcheck source=scripts/check-touched-classifier.sh
source "${ROOT}/scripts/check-touched-classifier.sh"

assert_true() {
  "$1" "$2"
}

assert_false() {
  if "$1" "$2"; then
    echo "expected false: $1 $2" >&2
    exit 1
  fi
}

assert_true is_high_risk_path crates/sigil-mcp/src/process.rs
assert_true is_high_risk_path crates/sigil-tui/src/runner/worker_loop.rs
assert_true is_high_risk_path crates/sigil-tools-builtin/src/shell.rs
assert_false is_high_risk_path dev/docs/rfcs/0071.md
assert_false is_high_risk_path crates/sigil-runtime/src/paths.rs

assert_true is_desktop_path apps/desktop/src-tauri/src/commands.rs
assert_true is_desktop_path crates/sigil-http/src/openapi.rs
assert_true is_desktop_path scripts/generate-desktop-contract.sh
assert_false is_desktop_path crates/sigil-runtime/src/paths.rs

echo "check-touched classifier tests: 8 passed"

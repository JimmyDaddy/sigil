#!/usr/bin/env bash
set -euo pipefail

repo_root="$(git rev-parse --show-toplevel 2>/dev/null || pwd)"
cd "${repo_root}"

cargo test -p sigil-tui render_docs_screenshot_assets -- --ignored --nocapture

echo "generated TUI renderer screenshots in site/assets/screenshots"

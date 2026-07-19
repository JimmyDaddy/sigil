#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
tauri="$repo_root/apps/desktop/src-tauri/src/lib.rs"

node "$repo_root/apps/desktop/scripts/check-ui-system.mjs"
rg -q '\.min_inner_size\(320\.0, 480\.0\)' "$tauri"

echo "desktop semantic-token, theme, catalog and 320px reflow checks passed"

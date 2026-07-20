#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
tauri="$repo_root/apps/desktop/src-tauri/src/lib.rs"

node "$repo_root/apps/desktop/scripts/check-ui-system.mjs"
rg -q '\.inner_size\(1280\.0, 820\.0\)' "$tauri"
rg -q '\.min_inner_size\(900\.0, 640\.0\)' "$tauri"

echo "desktop semantic-token, theme, catalog and usable-window checks passed"

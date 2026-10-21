#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
tauri="$repo_root/apps/desktop/src-tauri/src/lib.rs"

contains_pattern() {
  local pattern="$1"
  local path="$2"
  if command -v rg >/dev/null 2>&1; then
    rg -q "${pattern}" "${path}"
  else
    grep -Eq "${pattern}" "${path}"
  fi
}

node "$repo_root/apps/desktop/scripts/check-ui-system.mjs"
contains_pattern '\.inner_size\(1440\.0, 900\.0\)' "$tauri"
contains_pattern '\.min_inner_size\(f64::from\(MIN_WINDOW_WIDTH\), f64::from\(MIN_WINDOW_HEIGHT\)\)' "$tauri"
contains_pattern 'MIN_WINDOW_WIDTH: u32 = 840;' "$repo_root/apps/desktop/src-tauri/src/window_state.rs"
contains_pattern 'MIN_WINDOW_HEIGHT: u32 = 560;' "$repo_root/apps/desktop/src-tauri/src/window_state.rs"

echo "desktop semantic-token, theme, catalog and usable-window checks passed"

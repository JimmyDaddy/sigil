#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
styles="$repo_root/apps/desktop/src/styles.css"
tauri="$repo_root/apps/desktop/src-tauri/src/lib.rs"

component_css="$(sed -n '/^\* { box-sizing/,$p' "$styles")"
if printf '%s\n' "$component_css" | rg -n '#[0-9a-fA-F]{3,8}|rgb\(' >/dev/null; then
  echo "desktop UI check failed: component styles contain raw color values" >&2
  exit 1
fi

rg -q -- '--color-success:' "$styles"
rg -q '@media \(prefers-color-scheme: light\)' "$styles"
rg -q '@media \(forced-colors: active\)' "$styles"
rg -q '@media \(prefers-reduced-motion: reduce\)' "$styles"
rg -q 'min-width: 320px' "$styles"
rg -q '\.min_inner_size\(320\.0, 480\.0\)' "$tauri"

echo "desktop semantic-token, theme, motion and 320px reflow checks passed"

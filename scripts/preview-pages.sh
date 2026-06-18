#!/usr/bin/env bash
set -euo pipefail

repo_root="$(git rev-parse --show-toplevel 2>/dev/null || pwd)"
cd "${repo_root}"

port="${1:-8787}"
stage_root="$(mktemp -d)"
cleanup() {
  rm -rf "${stage_root}"
}
trap cleanup EXIT

stage_dir="${stage_root}/public"
scripts/build-pages-site.sh "${stage_dir}" >/dev/null

echo "Serving Sigil Pages preview:"
echo "  http://127.0.0.1:${port}/"
echo
echo "Press Ctrl-C to stop."
python3 -m http.server "${port}" --bind 127.0.0.1 --directory "${stage_dir}"

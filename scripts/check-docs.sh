#!/usr/bin/env bash
set -euo pipefail

repo_root="$(git rev-parse --show-toplevel 2>/dev/null || pwd)"
cd "${repo_root}"

scripts/check-docs-links.rb
scripts/check-docs-mirror.rb
scripts/check-docs-command-metadata.rb

echo "docs checks passed"

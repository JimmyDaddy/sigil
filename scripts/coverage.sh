#!/usr/bin/env bash
set -euo pipefail

coverage_min_lines="${COVERAGE_MIN_LINES:-96}"

if ! command -v cargo-llvm-cov >/dev/null 2>&1; then
  cat >&2 <<'EOF'
cargo-llvm-cov is required for the coverage gate.
Install it with:

  cargo install cargo-llvm-cov --version 0.8.7 --locked
EOF
  exit 127
fi

cargo llvm-cov \
  --workspace \
  --all-targets \
  --locked \
  --summary-only \
  --fail-under-lines "${coverage_min_lines}" \
  "$@"

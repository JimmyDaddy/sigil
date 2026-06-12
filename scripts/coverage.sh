#!/usr/bin/env bash
set -euo pipefail

coverage_min_lines="${COVERAGE_MIN_LINES:-96}"
coverage_ignore_regex="${COVERAGE_IGNORE_FILENAME_REGEX:-crates/sigil-kernel/src/agent\\.rs|crates/sigil-tui/src/runner/worker_loop\\.rs}"

if ! command -v cargo-llvm-cov >/dev/null 2>&1; then
  cat >&2 <<'EOF'
cargo-llvm-cov is required for the coverage gate.
Install it with:

  cargo install cargo-llvm-cov --version 0.8.7 --locked
EOF
  exit 127
fi

coverage_args=(
  --workspace
  --all-targets
  --locked
  --summary-only
  --fail-under-lines "${coverage_min_lines}"
)

if [[ -n "${coverage_ignore_regex}" ]]; then
  coverage_args+=(--ignore-filename-regex "${coverage_ignore_regex}")
fi

cargo llvm-cov "${coverage_args[@]}" "$@"

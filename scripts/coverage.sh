#!/usr/bin/env bash
set -euo pipefail

coverage_min_lines="${COVERAGE_MIN_LINES:-96}"
coverage_ignore_regex="${COVERAGE_IGNORE_FILENAME_REGEX:-crates/sigil-kernel/src/agent\\.rs|crates/sigil-runtime/src/agent_tools\\.rs|crates/sigil-tui/src/launcher\\.rs|crates/sigil-tui/src/runner/(spawn|worker_loop)\\.rs}"
coverage_summary_only="${COVERAGE_SUMMARY_ONLY:-1}"
coverage_packages="${COVERAGE_PACKAGES:-}"

if ! command -v cargo-llvm-cov >/dev/null 2>&1; then
  cat >&2 <<'EOF'
cargo-llvm-cov is required for the coverage gate.
Install it with:

  cargo install cargo-llvm-cov --version 0.8.7 --locked
EOF
  exit 127
fi

coverage_args=(
  --all-targets
  --locked
  --fail-under-lines "${coverage_min_lines}"
)

if [[ -n "${coverage_packages}" ]]; then
  read -r -a coverage_package_list <<<"${coverage_packages}"
  for package in "${coverage_package_list[@]}"; do
    coverage_args+=(-p "${package}")
  done
else
  coverage_args+=(--workspace)
fi

if [[ "${coverage_summary_only}" != "0" ]]; then
  coverage_args+=(--summary-only)
fi

if [[ -n "${coverage_ignore_regex}" ]]; then
  coverage_args+=(--ignore-filename-regex "${coverage_ignore_regex}")
fi

cargo llvm-cov "${coverage_args[@]}" "$@"

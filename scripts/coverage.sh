#!/usr/bin/env bash
set -euo pipefail

coverage_min_lines="${COVERAGE_MIN_LINES:-}"
coverage_ignore_regex="${COVERAGE_IGNORE_FILENAME_REGEX:-crates/sigil-kernel/src/agent\\.rs|crates/sigil-runtime/src/agent_tools\\.rs|crates/sigil-tui/src/launcher\\.rs|crates/sigil-tui/src/runner/(spawn|worker_loop)\\.rs}"
coverage_summary_only="${COVERAGE_SUMMARY_ONLY:-1}"
coverage_packages="${COVERAGE_PACKAGES:-}"
coverage_no_report="${COVERAGE_NO_REPORT:-0}"
coverage_report_only="${COVERAGE_REPORT_ONLY:-0}"

if [[ "${coverage_no_report}" != "0" && "${coverage_report_only}" != "0" ]]; then
  echo "COVERAGE_NO_REPORT and COVERAGE_REPORT_ONLY cannot both be set" >&2
  exit 2
fi

if ! command -v cargo-llvm-cov >/dev/null 2>&1; then
  cat >&2 <<'EOF'
cargo-llvm-cov is required for the coverage gate.
Install it with:

  cargo install cargo-llvm-cov --version 0.8.7 --locked
EOF
  exit 127
fi

coverage_command=(cargo llvm-cov)
coverage_args=(--locked)

if [[ "${coverage_report_only}" != "0" ]]; then
  coverage_command+=(report)
else
  coverage_args+=(--all-targets)
fi

if [[ "${coverage_no_report}" != "0" ]]; then
  coverage_args+=(--no-report)
fi

if [[ "${coverage_no_report}" == "0" && -n "${coverage_min_lines}" ]]; then
  coverage_args+=(--fail-under-lines "${coverage_min_lines}")
fi

if [[ -n "${coverage_packages}" ]]; then
  read -r -a coverage_package_list <<<"${coverage_packages}"
  for package in "${coverage_package_list[@]}"; do
    coverage_args+=(-p "${package}")
  done
elif [[ "${coverage_report_only}" == "0" ]]; then
  coverage_args+=(--workspace)
fi

if [[ "${coverage_no_report}" == "0" && "${coverage_summary_only}" != "0" ]]; then
  coverage_args+=(--summary-only)
fi

if [[ "${coverage_no_report}" == "0" && -n "${coverage_ignore_regex}" ]]; then
  coverage_args+=(--ignore-filename-regex "${coverage_ignore_regex}")
fi

"${coverage_command[@]}" "${coverage_args[@]}" "$@"

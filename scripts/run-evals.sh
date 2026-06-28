#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'USAGE'
Usage: scripts/run-evals.sh --deterministic [--output-dir DIR]

Runs Sigil deterministic eval cases with fake provider/tool plumbing only.
The script does not call real models or network-backed providers.

Options:
  --deterministic   Run deterministic conformance evals.
  --output-dir DIR  Directory for generated JSONL, summary, and retained artifacts.
  -h, --help        Show this help.
USAGE
}

mode=""
output_dir=""

while [[ $# -gt 0 ]]; do
  case "$1" in
    --deterministic)
      mode="deterministic"
      shift
      ;;
    --output-dir)
      if [[ $# -lt 2 ]]; then
        echo "missing value for --output-dir" >&2
        exit 2
      fi
      output_dir="$2"
      shift 2
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      echo "unknown argument: $1" >&2
      usage >&2
      exit 2
      ;;
  esac
done

if [[ "${mode}" != "deterministic" ]]; then
  echo "missing required --deterministic mode" >&2
  usage >&2
  exit 2
fi

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo_root="$(cd "${script_dir}/.." && pwd)"
cd "${repo_root}"

if [[ -z "${output_dir}" ]]; then
  timestamp="$(date +%Y%m%d-%H%M%S)"
  output_dir=".repo-local-dev/evals/deterministic-${timestamp}"
fi

case "${output_dir}" in
  /*) ;;
  *) output_dir="${repo_root}/${output_dir}" ;;
esac

mkdir -p "${output_dir}"

SIGIL_DETERMINISTIC_EVAL_REPORT_DIR="${output_dir}" \
  cargo test -p sigil-kernel eval_report_writes_deterministic_artifacts -- --nocapture

echo "wrote ${output_dir}/results.jsonl"
echo "wrote ${output_dir}/summary.md"

#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'USAGE'
Usage: scripts/run-context-quality.sh [--output-dir DIR]

Runs the deterministic Context V0 quality evidence sweep.
The script does not call real models, network-backed providers, embeddings, or vector indexes.

Options:
  --output-dir DIR  Directory for generated JSONL, summary, and manifest artifacts.
  -h, --help        Show this help.
USAGE
}

output_dir=""

while [[ $# -gt 0 ]]; do
  case "$1" in
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

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo_root="$(cd "${script_dir}/.." && pwd)"
cd "${repo_root}"

if [[ -z "${output_dir}" ]]; then
  timestamp="$(date +%Y%m%d-%H%M%S)"
  output_dir=".repo-local-dev/context-quality/context-v0-${timestamp}"
fi

case "${output_dir}" in
  /*) ;;
  *) output_dir="${repo_root}/${output_dir}" ;;
esac

mkdir -p "${output_dir}"

SIGIL_CONTEXT_QUALITY_REPORT_DIR="${output_dir}" \
  cargo test -p sigil-kernel context_quality_report_writes_evidence_artifacts -- --nocapture

for artifact in context-quality.jsonl summary.md manifest.json; do
  if [[ ! -s "${output_dir}/${artifact}" ]]; then
    echo "context quality sweep did not produce non-empty ${artifact}" >&2
    exit 1
  fi
done

echo "wrote ${output_dir}/context-quality.jsonl"
echo "wrote ${output_dir}/summary.md"
echo "wrote ${output_dir}/manifest.json"

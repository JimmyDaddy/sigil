#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'USAGE'
Usage:
  scripts/run-evals.sh --deterministic [--output-dir DIR]
  scripts/run-evals.sh --model --config FILE --case ID [--case ID ...]
    --repetitions N --max-cost-usd USD [--timeout-secs N] [--output-dir DIR]

Deterministic mode uses fake provider/tool plumbing only. Model mode is an explicit,
cost-bounded provider-backed acceptance campaign and may perform network requests.

Options:
  --deterministic   Run deterministic conformance evals.
  --model           Run explicit provider-backed model evals.
  --config FILE     Sigil config used by model mode.
  --case ID         Committed model fixture id; repeat for multiple cases.
  --repetitions N   Repetitions per model fixture.
  --max-cost-usd N  Local admission budget; not a provider-side billing cap.
  --timeout-secs N  Campaign wall deadline (default: 300).
  --output-dir DIR  Directory for generated JSONL, summary, and retained artifacts.
  -h, --help        Show this help.
USAGE
}

mode=""
output_dir=""
config_path=""
repetitions=""
max_cost_usd=""
timeout_secs="300"
cases=()

while [[ $# -gt 0 ]]; do
  case "$1" in
    --deterministic)
      [[ -z "${mode}" || "${mode}" == "deterministic" ]] || { echo "eval modes are mutually exclusive" >&2; exit 2; }
      mode="deterministic"
      shift
      ;;
    --model)
      [[ -z "${mode}" || "${mode}" == "model" ]] || { echo "eval modes are mutually exclusive" >&2; exit 2; }
      mode="model"
      shift
      ;;
    --config)
      [[ $# -ge 2 ]] || { echo "missing value for --config" >&2; exit 2; }
      config_path="$2"
      shift 2
      ;;
    --case)
      [[ $# -ge 2 ]] || { echo "missing value for --case" >&2; exit 2; }
      cases+=("$2")
      shift 2
      ;;
    --repetitions)
      [[ $# -ge 2 ]] || { echo "missing value for --repetitions" >&2; exit 2; }
      repetitions="$2"
      shift 2
      ;;
    --max-cost-usd)
      [[ $# -ge 2 ]] || { echo "missing value for --max-cost-usd" >&2; exit 2; }
      max_cost_usd="$2"
      shift 2
      ;;
    --timeout-secs)
      [[ $# -ge 2 ]] || { echo "missing value for --timeout-secs" >&2; exit 2; }
      timeout_secs="$2"
      shift 2
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

if [[ "${mode}" != "deterministic" && "${mode}" != "model" ]]; then
  echo "missing required --deterministic or --model mode" >&2
  usage >&2
  exit 2
fi

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo_root="$(cd "${script_dir}/.." && pwd)"
cd "${repo_root}"

if [[ -z "${output_dir}" ]]; then
  timestamp="$(date +%Y%m%d-%H%M%S)"
  output_dir=".repo-local-dev/evals/${mode}-${timestamp}"
fi

case "${output_dir}" in
  /*) ;;
  *) output_dir="${repo_root}/${output_dir}" ;;
esac

if [[ "${mode}" == "deterministic" ]]; then
  mkdir -p "${output_dir}"
  SIGIL_DETERMINISTIC_EVAL_REPORT_DIR="${output_dir}" \
    cargo test -p sigil-kernel eval_report_writes_deterministic_artifacts -- --nocapture
else
  if [[ -z "${config_path}" || ${#cases[@]} -eq 0 || -z "${repetitions}" || -z "${max_cost_usd}" ]]; then
    echo "model mode requires --config, --case, --repetitions, and --max-cost-usd" >&2
    usage >&2
    exit 2
  fi
  case "${config_path}" in
    /*) ;;
    *) config_path="${repo_root}/${config_path}" ;;
  esac
  model_args=(
    --config "${config_path}"
    model-eval
    --repetitions "${repetitions}"
    --max-cost-usd "${max_cost_usd}"
    --timeout-secs "${timeout_secs}"
    --output-dir "${output_dir}"
  )
  for case_id in "${cases[@]}"; do
    model_args+=(--case "${case_id}")
  done
  if [[ -n "${SIGIL_BIN:-}" ]]; then
    "${SIGIL_BIN}" "${model_args[@]}"
  else
    cargo run --quiet -p sigil -- "${model_args[@]}"
  fi
fi

for artifact in results.jsonl summary.md manifest.json; do
  if [[ ! -s "${output_dir}/${artifact}" ]]; then
    echo "deterministic eval did not produce non-empty ${artifact}" >&2
    exit 1
  fi
done

if [[ "${mode}" == "model" ]]; then
  grep -q '"report_schema_version": 3' "${output_dir}/manifest.json"
  if grep -v '"report_schema_version":3' "${output_dir}/results.jsonl" >/dev/null; then
    echo "model eval results contain a non-V3 record" >&2
    exit 1
  fi
fi

echo "wrote ${output_dir}/results.jsonl"
echo "wrote ${output_dir}/summary.md"
echo "wrote ${output_dir}/manifest.json"

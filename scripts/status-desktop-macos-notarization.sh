#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'USAGE'
Usage: scripts/status-desktop-macos-notarization.sh \
  --artifact-dir PATH \
  [--summary] \
  [--submit-pending] \
  [--resubmit ARTIFACT_KEY]

Query each non-terminal Apple submission at most once and persist the observed
status. --summary is offline. --resubmit explicitly orphans one missing/failed
attempt and creates a new Apple submission for the same immutable bytes.
--submit-pending continues artifacts that were never submitted because an
earlier upload process was interrupted.
USAGE
}

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
artifact_dir=""
summary_only=false
submit_pending=false
resubmit_key=""

while [[ $# -gt 0 ]]; do
  case "$1" in
    --artifact-dir)
      artifact_dir="${2-}"
      shift 2
      ;;
    --summary)
      summary_only=true
      shift
      ;;
    --submit-pending)
      submit_pending=true
      shift
      ;;
    --resubmit)
      resubmit_key="${2-}"
      shift 2
      ;;
    -h | --help)
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

if [[ -z "$artifact_dir" ]]; then
  usage >&2
  exit 2
fi
if [[ "$artifact_dir" != /* ]]; then
  artifact_dir="$repo_root/$artifact_dir"
fi
if [[ ! -d "$artifact_dir" || -L "$artifact_dir" ]]; then
  echo "artifact directory must be a non-symlink directory: $artifact_dir" >&2
  exit 1
fi

if [[ -n "$resubmit_key" ]]; then
  node "$repo_root/scripts/desktop-notarization-state.mjs" resubmit \
    "$artifact_dir" "$resubmit_key"
  exit 0
fi
if [[ "$submit_pending" == true ]]; then
  node "$repo_root/scripts/desktop-notarization-state.mjs" submit "$artifact_dir"
  exit 0
fi
if [[ "$summary_only" == true ]]; then
  node "$repo_root/scripts/desktop-notarization-state.mjs" summary "$artifact_dir"
  exit 0
fi

if node "$repo_root/scripts/desktop-notarization-state.mjs" status "$artifact_dir"; then
  exit 0
else
  status_code=$?
  node "$repo_root/scripts/desktop-notarization-state.mjs" summary "$artifact_dir" || true
  exit "$status_code"
fi

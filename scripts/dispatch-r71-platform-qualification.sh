#!/usr/bin/env bash
# RFC-0071 R71.8: exact-candidate GitHub qualification dispatch.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

usage() {
  cat <<'EOF'
Usage: scripts/dispatch-r71-platform-qualification.sh \
  --candidate-sha SHA --base-sha SHA --local-evidence PATH [--wait]
EOF
}

candidate_sha=""
base_sha=""
local_evidence=""
wait_for_run=0
while [[ $# -gt 0 ]]; do
  case "$1" in
    --candidate-sha) candidate_sha="${2:-}"; shift 2 ;;
    --base-sha) base_sha="${2:-}"; shift 2 ;;
    --local-evidence) local_evidence="${2:-}"; shift 2 ;;
    --wait) wait_for_run=1; shift ;;
    -h|--help) usage; exit 0 ;;
    *) echo "unknown argument: $1" >&2; usage >&2; exit 2 ;;
  esac
done
[[ -n "$candidate_sha" && -n "$base_sha" && -n "$local_evidence" ]] || { usage >&2; exit 2; }
command -v gh >/dev/null || { echo "gh CLI is required" >&2; exit 1; }

python3 - "$ROOT" "$candidate_sha" "$base_sha" "$local_evidence" <<'PY'
import json
import sys
from pathlib import Path

sys.path.insert(0, str(Path(sys.argv[1]) / "scripts"))
from r71_qualification_common import load_conformance_manifest, load_platform_manifest, manifest_digests, validate_git_identity

root = Path(sys.argv[1])
candidate = sys.argv[2].lower()
base = sys.argv[3].lower()
evidence = Path(sys.argv[4])
identity = validate_git_identity(root, candidate, base)
load_conformance_manifest()
load_platform_manifest()
identity.update(manifest_digests())
payload = json.loads(evidence.read_text(encoding="utf-8"))
for key in ("candidate_sha", "base_sha", "conformance_manifest_sha256", "platform_manifest_sha256"):
    if payload.get(key) != identity[key]:
        raise SystemExit(f"local qualification evidence mismatch for {key}")
if payload.get("result") != "passed" or payload.get("suite") != "full":
    raise SystemExit("local qualification evidence is not a passed full suite")
print(f"local qualification evidence verified for candidate {candidate}")
PY

remote_candidate="$(git ls-remote --heads origin r71-release-candidate | awk '{print $1}')"
if [[ "$remote_candidate" != "$candidate_sha" ]]; then
  echo "origin/r71-release-candidate resolves to ${remote_candidate:-<missing>}, expected $candidate_sha" >&2
  exit 1
fi

gh workflow run sandbox-conformance.yml \
  --ref r71-release-candidate \
  -f candidate_sha="$candidate_sha" \
  -f base_sha="$base_sha" \
  -f require_conformance=true

run_json="$(gh run list \
  --workflow sandbox-conformance.yml \
  --branch r71-release-candidate \
  --event workflow_dispatch \
  --limit 20 \
  --json databaseId,headSha,status,createdAt,url)"
run_id="$(python3 - "$candidate_sha" "$run_json" <<'PY'
import json
import sys

candidate, raw = sys.argv[1], sys.argv[2]
runs = json.loads(raw)
matches = [run for run in runs if run.get("headSha") == candidate]
if not matches:
    raise SystemExit("no dispatched workflow run has the exact candidate SHA")
matches.sort(key=lambda item: item.get("createdAt", ""), reverse=True)
print(matches[0]["databaseId"])
PY
)"
echo "r71 platform qualification run: $run_id"

if [[ "$wait_for_run" != "1" ]]; then
  exit 0
fi

gh run watch "$run_id" --exit-status
run_view="$(gh run view "$run_id" --json databaseId,headSha,conclusion,url,jobs)"
evidence_dir="${SIGIL_R71_EVIDENCE_DIR:-$(mktemp -d -t sigil-r71-dispatch-XXXXXX)}"
mkdir -p "$evidence_dir"
printf '%s\n' "$run_view" > "$evidence_dir/run.json"
gh run download "$run_id" --dir "$evidence_dir/artifacts"

python3 - "$ROOT" "$candidate_sha" "$base_sha" "$run_view" "$evidence_dir" <<'PY'
import json
import sys
from pathlib import Path

sys.path.insert(0, str(Path(sys.argv[1]) / "scripts"))
from r71_qualification_common import REQUIRED_PLATFORM_JOBS, manifest_digests

candidate, base, raw, evidence_dir = sys.argv[2], sys.argv[3], sys.argv[4], Path(sys.argv[5])
run = json.loads(raw)
if run.get("headSha") != candidate or run.get("conclusion") != "success":
    raise SystemExit("platform workflow did not finish successfully at the exact candidate SHA")
jobs = {job.get("name"): job for job in run.get("jobs", [])}
if set(jobs) != REQUIRED_PLATFORM_JOBS:
    raise SystemExit(f"required platform job set drifted: {sorted(jobs)}")
for name, job in jobs.items():
    if job.get("conclusion") != "success":
        raise SystemExit(f"required job {name} did not conclude success")

qualification_files = sorted(evidence_dir.glob("artifacts/**/qualification.json"))
if len(qualification_files) != len(REQUIRED_PLATFORM_JOBS):
    raise SystemExit("platform evidence does not contain one qualification.json per required job")
digests = manifest_digests()
for path in qualification_files:
    payload = json.loads(path.read_text(encoding="utf-8"))
    for key, expected in {"candidate_sha": candidate, "base_sha": base, **digests}.items():
        if payload.get(key) != expected:
            raise SystemExit(f"{path} mismatches {key}")
    if payload.get("result") != "passed":
        raise SystemExit(f"{path} is not passed evidence")
print(f"r71 platform qualification passed: {len(REQUIRED_PLATFORM_JOBS)} required jobs")
PY

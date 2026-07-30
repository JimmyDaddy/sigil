#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'USAGE'
Usage: scripts/notarize-desktop-macos.sh \
  --dmg PATH \
  --expected-arch arm64|x86_64 \
  --team-id TEAM_ID \
  [--notary-profile PROFILE] \
  [--submission-id UUID] \
  [--timeout 30m]

Submit a Developer ID-signed Sigil DMG to Apple, wait for acceptance, staple
the ticket, and run the complete distribution verification. If waiting is
interrupted, rerun with the submission ID persisted beside the DMG.
USAGE
}

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
dmg_path=""
expected_arch=""
team_id=""
notary_profile="${SIGIL_NOTARY_PROFILE:-Sigil-Notary}"
submission_id=""
wait_timeout="${SIGIL_NOTARY_TIMEOUT:-30m}"

while [[ $# -gt 0 ]]; do
  case "$1" in
    --dmg)
      dmg_path="${2-}"
      shift 2
      ;;
    --expected-arch)
      expected_arch="${2-}"
      shift 2
      ;;
    --team-id)
      team_id="${2-}"
      shift 2
      ;;
    --notary-profile)
      notary_profile="${2-}"
      shift 2
      ;;
    --submission-id)
      submission_id="${2-}"
      shift 2
      ;;
    --timeout)
      wait_timeout="${2-}"
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

if [[ -z "$dmg_path" || -z "$expected_arch" || -z "$team_id" ]]; then
  usage >&2
  exit 2
fi
if [[ ! -f "$dmg_path" || -L "$dmg_path" ]]; then
  echo "DMG must be a regular non-symlink file: $dmg_path" >&2
  exit 1
fi
if [[ -z "$notary_profile" ]]; then
  echo "notary profile must not be empty" >&2
  exit 2
fi

xcrun notarytool history \
  --keychain-profile "$notary_profile" \
  --output-format json >/dev/null

submit_path="${dmg_path}.notary-submit.json"
wait_path="${dmg_path}.notary-wait.json"
log_path="${dmg_path}.notary-log.json"

json_field() {
  local path=$1
  local field=$2
  node -e '
    const fs = require("node:fs");
    const value = JSON.parse(fs.readFileSync(process.argv[1], "utf8"));
    const result = value[process.argv[2]];
    if (typeof result !== "string" || result.length === 0) process.exit(1);
    process.stdout.write(result);
  ' "$path" "$field"
}

if [[ -z "$submission_id" ]]; then
  xcrun notarytool submit "$dmg_path" \
    --keychain-profile "$notary_profile" \
    --output-format json >"$submit_path"
  submission_id="$(json_field "$submit_path" id)"
  echo "Apple notarization submission: $submission_id"
fi

if ! xcrun notarytool wait "$submission_id" \
  --keychain-profile "$notary_profile" \
  --timeout "$wait_timeout" \
  --output-format json >"$wait_path"; then
  echo "notarization wait did not complete; resume with:" >&2
  printf '%q ' "$repo_root/scripts/notarize-desktop-macos.sh" \
    --dmg "$dmg_path" \
    --expected-arch "$expected_arch" \
    --team-id "$team_id" \
    --notary-profile "$notary_profile" \
    --submission-id "$submission_id"
  printf '\n' >&2
  exit 1
fi

status="$(json_field "$wait_path" status)"
if [[ "$status" != "Accepted" ]]; then
  xcrun notarytool log "$submission_id" \
    --keychain-profile "$notary_profile" \
    "$log_path" || true
  echo "Apple notarization ended with status $status; log: $log_path" >&2
  exit 1
fi

xcrun stapler staple "$dmg_path"
"$repo_root/scripts/verify-desktop-macos.sh" \
  --dmg "$dmg_path" \
  --expected-arch "$expected_arch" \
  --team-id "$team_id" \
  --notarized

echo "notarized and stapled Sigil DMG: $dmg_path"

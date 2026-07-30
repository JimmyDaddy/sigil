#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'USAGE'
Usage: scripts/verify-desktop-update-signature.sh \
  --archive PATH \
  --signature PATH \
  [--config PATH]

Verify a Tauri updater archive against the public key embedded in the checked
out Desktop configuration. Exit 1 means the signature is cryptographically
invalid. Exit 2 means the verifier could not establish the verification input.
USAGE
}

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
config="$repo_root/apps/desktop/src-tauri/tauri.conf.json"
archive=""
signature=""

while [[ $# -gt 0 ]]; do
  case "$1" in
    --archive)
      archive="${2-}"
      shift 2
      ;;
    --signature)
      signature="${2-}"
      shift 2
      ;;
    --config)
      config="${2-}"
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

if [[ -z "$archive" || -z "$signature" ]]; then
  usage >&2
  exit 2
fi

exec cargo run \
  --quiet \
  --locked \
  -p sigil-updater \
  --features release-verifier \
  --bin sigil-verify-tauri-update \
  -- \
  --config "$config" \
  --archive "$archive" \
  --signature "$signature"

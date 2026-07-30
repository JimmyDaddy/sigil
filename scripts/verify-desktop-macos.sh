#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'USAGE'
Usage: scripts/verify-desktop-macos.sh \
  --dmg PATH \
  --expected-arch arm64|x86_64 \
  --team-id TEAM_ID \
  [--notarized]

Verify the exact Sigil DMG that will be distributed. Developer ID signatures,
Hardened Runtime, secure timestamps, bundle identity, nested sidecar identity,
architecture, and optional notarization/Gatekeeper state are checked.
USAGE
}

dmg_path=""
expected_arch=""
team_id=""
notarized=false

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
    --notarized)
      notarized=true
      shift
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

if [[ ! -f "$dmg_path" || -L "$dmg_path" ]]; then
  echo "DMG must be a regular non-symlink file: $dmg_path" >&2
  exit 1
fi
if [[ "$expected_arch" != "arm64" && "$expected_arch" != "x86_64" ]]; then
  echo "expected architecture must be arm64 or x86_64" >&2
  exit 2
fi
if [[ ! "$team_id" =~ ^[A-Z0-9]{10}$ ]]; then
  echo "Apple Team ID must contain exactly 10 uppercase letters or digits" >&2
  exit 2
fi

for command_name in codesign hdiutil lipo plutil spctl xcrun; do
  command -v "$command_name" >/dev/null || {
    echo "missing required command: $command_name" >&2
    exit 1
  }
done

assert_developer_id_signature() {
  local signed_path=$1
  local description=$2
  local details

  codesign --verify --strict --verbose=4 "$signed_path"
  details="$(codesign -dv --verbose=4 "$signed_path" 2>&1)"
  grep -Fq "Authority=Developer ID Application:" <<<"$details" || {
    echo "$description is not signed by Developer ID Application" >&2
    exit 1
  }
  grep -Fq "TeamIdentifier=$team_id" <<<"$details" || {
    echo "$description does not belong to Apple Team $team_id" >&2
    exit 1
  }
  grep -Fq "Timestamp=" <<<"$details" || {
    echo "$description is missing a secure signing timestamp" >&2
    exit 1
  }
}

assert_hardened_executable() {
  local executable_path=$1
  local description=$2
  local details

  assert_developer_id_signature "$executable_path" "$description"
  details="$(codesign -dv --verbose=4 "$executable_path" 2>&1)"
  grep -Fq "Runtime Version=" <<<"$details" || {
    echo "$description does not enable Hardened Runtime" >&2
    exit 1
  }
  if codesign -d --entitlements :- "$executable_path" 2>&1 |
    grep -A1 -Fq "com.apple.security.get-task-allow"; then
    echo "$description carries the forbidden get-task-allow entitlement" >&2
    exit 1
  fi
}

hdiutil verify "$dmg_path"
assert_developer_id_signature "$dmg_path" "Sigil DMG"

mount_point="$(mktemp -d "${TMPDIR:-/tmp}/sigil-desktop-mount.XXXXXX")"
mounted=false
cleanup_mount() {
  if [[ "$mounted" == true ]]; then
    hdiutil detach "$mount_point" -quiet || true
  fi
  rmdir "$mount_point" 2>/dev/null || true
}
trap cleanup_mount EXIT

hdiutil attach -readonly -nobrowse -mountpoint "$mount_point" "$dmg_path" >/dev/null
mounted=true

app_path="$mount_point/Sigil.app"
main_executable="$app_path/Contents/MacOS/sigil-desktop-app"
runtime_executable="$app_path/Contents/MacOS/sigil-runtime"
info_plist="$app_path/Contents/Info.plist"

test -d "$app_path"
test -x "$main_executable"
test -x "$runtime_executable"
test -f "$info_plist"

codesign --verify --deep --strict --verbose=4 "$app_path"
assert_developer_id_signature "$app_path" "Sigil app bundle"
assert_hardened_executable "$main_executable" "Sigil desktop executable"
assert_hardened_executable "$runtime_executable" "Sigil runtime sidecar"

test "$(plutil -extract CFBundleIdentifier raw "$info_plist")" = "dev.sigil.desktop"
test "$(lipo -archs "$main_executable")" = "$expected_arch"
test "$(lipo -archs "$runtime_executable")" = "$expected_arch"
"$runtime_executable" --version

if [[ "$notarized" == true ]]; then
  xcrun stapler validate "$dmg_path"
  spctl --assess --type open --context context:primary-signature --verbose=4 "$dmg_path"
  spctl --assess --type execute --verbose=4 "$app_path"
fi

cleanup_mount
trap - EXIT
echo "verified Sigil macOS package: $dmg_path"

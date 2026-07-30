#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'USAGE'
Usage: scripts/verify-desktop-macos-app.sh \
  --app PATH \
  --expected-arch arm64|x86_64 \
  --team-id TEAM_ID \
  [--expected-version VERSION] \
  [--expected-commit COMMIT] \
  [--notarized]

Verify the exact Sigil app bundle used to create a Desktop updater archive.
Developer ID signatures, Hardened Runtime, secure timestamps, bundle identity,
nested sidecar identity, architecture, and optional notarization state are
checked before the archive is signed.
USAGE
}

app_path=""
expected_arch=""
team_id=""
expected_version=""
expected_commit=""
notarized=false

while [[ $# -gt 0 ]]; do
  case "$1" in
    --app)
      app_path="${2-}"
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
    --expected-version)
      expected_version="${2-}"
      shift 2
      ;;
    --expected-commit)
      expected_commit="${2-}"
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

if [[ ! -d "$app_path" || -L "$app_path" ]]; then
  echo "app must be a non-symlink directory: $app_path" >&2
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
if [[ -n "$expected_version" && -z "$expected_commit" ]] ||
  [[ -z "$expected_version" && -n "$expected_commit" ]]; then
  echo "expected version and expected commit must be supplied together" >&2
  exit 2
fi
if [[ -n "$expected_version" ]]; then
  if [[ ! "$expected_version" =~ ^[0-9]+\.[0-9]+\.[0-9]+([.-][0-9A-Za-z.-]+)?$ ]]; then
    echo "expected version must be a bounded release version" >&2
    exit 2
  fi
  if [[ ! "$expected_commit" =~ ^[0-9a-f]{7,40}$ ]]; then
    echo "expected commit must contain 7 to 40 lowercase hexadecimal characters" >&2
    exit 2
  fi
fi

for command_name in codesign lipo plutil spctl xcrun; do
  command -v "$command_name" >/dev/null || {
    echo "missing required command: $command_name" >&2
    exit 1
  }
done

main_executable="$app_path/Contents/MacOS/sigil-desktop-app"
runtime_executable="$app_path/Contents/MacOS/sigil-runtime"
info_plist="$app_path/Contents/Info.plist"

test -x "$main_executable"
test -x "$runtime_executable"
test -f "$info_plist"

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

codesign --verify --deep --strict --verbose=4 "$app_path"
assert_developer_id_signature "$app_path" "Sigil app bundle"
assert_hardened_executable "$main_executable" "Sigil desktop executable"
assert_hardened_executable "$runtime_executable" "Sigil runtime sidecar"

test "$(plutil -extract CFBundleIdentifier raw "$info_plist")" = "dev.sigil.desktop"
test "$(lipo -archs "$main_executable")" = "$expected_arch"
test "$(lipo -archs "$runtime_executable")" = "$expected_arch"
runtime_version_output="$("$runtime_executable" --version)"
printf '%s\n' "$runtime_version_output"
if [[ -n "$expected_version" ]]; then
  actual_version="$(sed -n 's/^sigil //p' <<<"$runtime_version_output")"
  actual_commit="$(sed -n 's/^commit: //p' <<<"$runtime_version_output")"
  if [[ "$actual_version" != "$expected_version" ]]; then
    echo "Sigil runtime version mismatch: expected $expected_version, found ${actual_version:-missing}" >&2
    exit 1
  fi
  if [[ "$actual_commit" != "$expected_commit" ]]; then
    echo "Sigil runtime commit mismatch: expected $expected_commit, found ${actual_commit:-missing}" >&2
    exit 1
  fi
fi

if [[ "$notarized" == true ]]; then
  xcrun stapler validate "$app_path"
  spctl --assess --type execute --verbose=4 "$app_path"
fi

echo "verified Sigil macOS app: $app_path"

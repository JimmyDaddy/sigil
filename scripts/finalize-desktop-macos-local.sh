#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'USAGE'
Usage: scripts/finalize-desktop-macos-local.sh \
  --artifact-dir PATH \
  [--updater-key PATH]

Finalize an asynchronously notarized Desktop candidate without contacting
Apple. Every submission must already have an Accepted status recorded by
status-desktop-macos-notarization.sh. The command is safe to rerun after an
interruption: stapled outputs and signed archives are reverified before reuse.
USAGE
}

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

artifact_dir=""
updater_key="${SIGIL_DESKTOP_UPDATER_KEY:-$HOME/Library/Application Support/sigil/release/desktop-updater.key}"
updater_key_password="${SIGIL_DESKTOP_UPDATER_KEY_PASSWORD:-}"

while [[ $# -gt 0 ]]; do
  case "$1" in
    --artifact-dir)
      artifact_dir="${2-}"
      shift 2
      ;;
    --updater-key)
      updater_key="${2-}"
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
for command_name in codesign ditto git node pnpm shasum tar xcrun; do
  command -v "$command_name" >/dev/null || {
    echo "missing required command: $command_name" >&2
    exit 1
  }
done
if [[ ! -f "$updater_key" || -L "$updater_key" ]] ||
  [[ ! -f "${updater_key}.pub" || -L "${updater_key}.pub" ]]; then
  echo "updater key pair must contain regular non-symlink files: $updater_key" >&2
  exit 1
fi
if [[ "$(stat -f '%Lp' "$updater_key")" != "600" ]]; then
  echo "Tauri updater private key must have mode 600: $updater_key" >&2
  exit 1
fi

state_tool="$repo_root/scripts/desktop-notarization-state.mjs"
node "$state_tool" assert-accepted "$artifact_dir" >/dev/null
tag="$(node "$state_tool" get-release "$artifact_dir" tag)"
version="$(node "$state_tool" get-release "$artifact_dir" version)"
commit="$(node "$state_tool" get-release "$artifact_dir" commit)"
team_id="$(node "$state_tool" get-release "$artifact_dir" teamId)"
expected_commit="${commit:0:12}"

if [[ "$(git rev-parse HEAD)" != "$commit" ]]; then
  echo "HEAD must match the notarization ledger commit: $commit" >&2
  exit 1
fi
if [[ -n "$(git status --porcelain)" ]]; then
  echo "the worktree must be clean before finalizing Desktop release assets" >&2
  exit 1
fi
node scripts/release-doctor.mjs \
  --tag "$tag" \
  --require-clean \
  --updater-public-key "${updater_key}.pub"

targets=()
while IFS= read -r target; do
  [[ -n "$target" ]] && targets+=("$target")
done < <(node "$state_tool" list-targets "$artifact_dir")
if [[ "${#targets[@]}" -eq 0 ]]; then
  echo "notarization ledger contains no Desktop targets" >&2
  exit 1
fi

temporary_root="$(mktemp -d "$artifact_dir/.finalize.XXXXXX")"
trap 'rm -rf "$temporary_root"' EXIT
declare -a release_assets=()

write_checksum() {
  local path=$1
  local checksum_path="${path}.sha256"
  local temporary_path
  temporary_path="$temporary_root/$(basename "$checksum_path").tmp"
  (
    cd "$(dirname "$path")"
    shasum -a 256 "$(basename "$path")"
  ) >"$temporary_path"
  mv -f "$temporary_path" "$checksum_path"
}

for target in "${targets[@]}"; do
  case "$target" in
    aarch64-apple-darwin)
      expected_arch="arm64"
      key_prefix="arm64"
      ;;
    x86_64-apple-darwin)
      expected_arch="x86_64"
      key_prefix="x86_64"
      ;;
    *)
      echo "unsupported notarization target: $target" >&2
      exit 1
      ;;
  esac

  work_root="$artifact_dir/.work/$target"
  submission_dmg="$work_root/Sigil_${version}_${target}.submission.dmg"
  app_submission="$work_root/Sigil_${version}_${target}.app.notary.zip"
  updater_app="$work_root/Sigil.app"
  output_dmg="$artifact_dir/Sigil_${version}_${target}.dmg"

  if ! xcrun stapler validate "$output_dmg" >/dev/null 2>&1; then
    temporary_dmg="$temporary_root/$(basename "$output_dmg")"
    ditto "$submission_dmg" "$temporary_dmg"
    xcrun stapler staple "$temporary_dmg"
    "$repo_root/scripts/verify-desktop-macos.sh" \
      --dmg "$temporary_dmg" \
      --expected-arch "$expected_arch" \
      --team-id "$team_id" \
      --expected-version "$version" \
      --expected-commit "$expected_commit" \
      --notarized
    mv -f "$temporary_dmg" "$output_dmg"
  fi
  "$repo_root/scripts/verify-desktop-macos.sh" \
    --dmg "$output_dmg" \
    --expected-arch "$expected_arch" \
    --team-id "$team_id" \
    --expected-version "$version" \
    --expected-commit "$expected_commit" \
    --notarized
  node "$state_tool" record-finalized \
    "$artifact_dir" "${key_prefix}_dmg" "$output_dmg" >/dev/null

  if ! xcrun stapler validate "$updater_app" >/dev/null 2>&1; then
    extracted_root="$temporary_root/extract-$target"
    mkdir -p "$extracted_root"
    ditto -x -k "$app_submission" "$extracted_root"
    if [[ ! -d "$extracted_root/Sigil.app" || -L "$extracted_root/Sigil.app" ]]; then
      echo "notarization app archive did not contain Sigil.app: $app_submission" >&2
      exit 1
    fi
    xcrun stapler staple "$extracted_root/Sigil.app"
    "$repo_root/scripts/verify-desktop-macos-app.sh" \
      --app "$extracted_root/Sigil.app" \
      --expected-arch "$expected_arch" \
      --team-id "$team_id" \
      --expected-version "$version" \
      --expected-commit "$expected_commit" \
      --notarized
    rm -rf "$updater_app"
    mv "$extracted_root/Sigil.app" "$updater_app"
  fi
  "$repo_root/scripts/verify-desktop-macos-app.sh" \
    --app "$updater_app" \
    --expected-arch "$expected_arch" \
    --team-id "$team_id" \
    --expected-version "$version" \
    --expected-commit "$expected_commit" \
    --notarized
  node "$state_tool" record-finalized \
    "$artifact_dir" "${key_prefix}_app" "$updater_app" >/dev/null

  updater_archive="$artifact_dir/Sigil_${version}_${target}.app.tar.gz"
  if [[ ! -s "$updater_archive" || ! -s "${updater_archive}.sig" ]] ||
    ! "$repo_root/scripts/verify-desktop-update-signature.sh" \
      --archive "$updater_archive" \
      --signature "${updater_archive}.sig" >/dev/null 2>&1; then
    temporary_archive="$temporary_root/$(basename "$updater_archive")"
    tar -czf "$temporary_archive" -C "$work_root" Sigil.app
    roundtrip_root="$temporary_root/roundtrip-$target"
    mkdir -p "$roundtrip_root"
    tar -xzf "$temporary_archive" -C "$roundtrip_root"
    "$repo_root/scripts/verify-desktop-macos-app.sh" \
      --app "$roundtrip_root/Sigil.app" \
      --expected-arch "$expected_arch" \
      --team-id "$team_id" \
      --expected-version "$version" \
      --expected-commit "$expected_commit" \
      --notarized
    TAURI_SIGNING_PRIVATE_KEY_PATH="$updater_key" \
    TAURI_SIGNING_PRIVATE_KEY_PASSWORD="$updater_key_password" \
      pnpm --dir apps/desktop tauri signer sign "$temporary_archive"
    "$repo_root/scripts/verify-desktop-update-signature.sh" \
      --archive "$temporary_archive" \
      --signature "${temporary_archive}.sig"
    mv -f "$temporary_archive" "$updater_archive"
    mv -f "${temporary_archive}.sig" "${updater_archive}.sig"
  fi
  "$repo_root/scripts/verify-desktop-update-signature.sh" \
    --archive "$updater_archive" \
    --signature "${updater_archive}.sig"
  write_checksum "$output_dmg"
  write_checksum "$updater_archive"
  release_assets+=(
    "$output_dmg"
    "${output_dmg}.sha256"
    "$updater_archive"
    "${updater_archive}.sha256"
    "${updater_archive}.sig"
  )
done

if [[ "${#targets[@]}" -eq 2 ]]; then
  arm_archive="$artifact_dir/Sigil_${version}_aarch64-apple-darwin.app.tar.gz"
  intel_signature="$artifact_dir/Sigil_${version}_x86_64-apple-darwin.app.tar.gz.sig"
  if "$repo_root/scripts/verify-desktop-update-signature.sh" \
    --archive "$arm_archive" \
    --signature "$intel_signature"; then
    echo "swapped Desktop updater signature unexpectedly verified" >&2
    exit 1
  elif [[ "$?" -ne 1 ]]; then
    echo "swapped-signature negative control could not complete verification" >&2
    exit 1
  fi
fi

identity="$(
  codesign -dv --verbose=4 "$updater_app" 2>&1 |
    sed -n 's/^Authority=//p' |
    head -n 1
)"
if [[ -z "$identity" ]]; then
  echo "unable to read Developer ID authority from finalized app" >&2
  exit 1
fi
targets_text="$(printf '%s ' "${targets[@]}")"
build_metadata="$artifact_dir/build.txt"
temporary_metadata="$temporary_root/build.txt"
cat >"$temporary_metadata" <<EOF
version=$version
commit=$commit
team_id=$team_id
identity=$identity
targets=$targets_text
notarized=true
updater_public_key=$(shasum -a 256 "${updater_key}.pub" | awk '{print $1}')
EOF
mv -f "$temporary_metadata" "$build_metadata"
release_assets+=("$build_metadata")

node "$state_tool" complete "$artifact_dir" "${release_assets[@]}" >/dev/null
node "$state_tool" verify-finalized "$artifact_dir" >/dev/null
echo "signed, notarized, stapled Desktop packages are ready at $artifact_dir"

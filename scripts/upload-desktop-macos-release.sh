#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'USAGE'
Usage: scripts/upload-desktop-macos-release.sh \
  --tag v<version> \
  --artifact-dir PATH \
  [--repository OWNER/REPO] \
  [--updater-key PATH] \
  [--replace] \
  [--dry-run]

Verify and upload the complete signed/notarized Apple Silicon + Intel Desktop
candidate to the matching draft GitHub Release. Existing identical assets are
kept. Existing different assets fail closed unless --replace is explicit.
USAGE
}

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "${repo_root}"

release_tag=""
artifact_dir=""
repository="${GITHUB_REPOSITORY:-JimmyDaddy/sigil}"
updater_key="${SIGIL_DESKTOP_UPDATER_KEY:-$HOME/Library/Application Support/sigil/release/desktop-updater.key}"
replace=false
dry_run=false

while [[ $# -gt 0 ]]; do
  case "$1" in
    --tag)
      release_tag="${2-}"
      shift 2
      ;;
    --artifact-dir)
      artifact_dir="${2-}"
      shift 2
      ;;
    --repository)
      repository="${2-}"
      shift 2
      ;;
    --updater-key)
      updater_key="${2-}"
      shift 2
      ;;
    --replace)
      replace=true
      shift
      ;;
    --dry-run)
      dry_run=true
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

if [[ ! "${release_tag}" =~ ^v[0-9]+\.[0-9]+\.[0-9]+(-beta\.[0-9]+)?$ ]] ||
  [[ -z "${artifact_dir}" ]] ||
  [[ ! "${repository}" =~ ^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+$ ]]; then
  usage >&2
  exit 2
fi
if [[ "$(uname -s)" != "Darwin" ]]; then
  echo "public macOS Desktop verification and upload must run on macOS" >&2
  exit 1
fi
for command_name in gh git jq node python3 shasum; do
  command -v "${command_name}" >/dev/null || {
    echo "missing required command: ${command_name}" >&2
    exit 1
  }
done

if [[ "${artifact_dir}" != /* ]]; then
  artifact_dir="${repo_root}/${artifact_dir}"
fi
if [[ ! -d "${artifact_dir}" || -L "${artifact_dir}" ]]; then
  echo "artifact directory must be a regular directory: ${artifact_dir}" >&2
  exit 1
fi

node scripts/release-doctor.mjs \
  --tag "${release_tag}" \
  --repository "${repository}" \
  --require-clean \
  --require-head-tag \
  --require-origin-main \
  --require-remote-tag \
  --require-ci \
  --require-workflow CI \
  --require-workflow "Desktop Package" \
  --require-public-channel \
  --updater-public-key "${updater_key}.pub"

version="${release_tag#v}"
commit="$(git rev-parse HEAD)"
expected_commit="${commit:0:12}"
team_id="${APPLE_TEAM_ID:-9Q6HK7T78S}"
build_metadata="${artifact_dir}/build.txt"
if [[ ! -f "${build_metadata}" || -L "${build_metadata}" ]]; then
  echo "missing regular build metadata: ${build_metadata}" >&2
  exit 1
fi

read_build_field() {
  local field="$1"
  sed -n "s/^${field}=//p" "${build_metadata}" | head -n 1
}

test "$(read_build_field version)" = "${version}"
test "$(read_build_field commit)" = "${commit}"
test "$(read_build_field team_id)" = "${team_id}"
test "$(read_build_field notarized)" = "true"
test "$(node scripts/desktop-notarization-state.mjs get-release "${artifact_dir}" tag)" = "${release_tag}"
test "$(node scripts/desktop-notarization-state.mjs get-release "${artifact_dir}" commit)" = "${commit}"
test "$(node scripts/desktop-notarization-state.mjs get-release "${artifact_dir}" teamId)" = "${team_id}"
node scripts/desktop-notarization-state.mjs verify-finalized "${artifact_dir}" >/dev/null
targets="$(read_build_field targets)"
for target in aarch64-apple-darwin x86_64-apple-darwin; do
  if ! grep -Eq "(^|[[:space:]])${target}([[:space:]]|$)" <<<"${targets}"; then
    echo "build metadata is missing target ${target}" >&2
    exit 1
  fi
done

temporary_root="$(mktemp -d)"
trap 'rm -rf "${temporary_root}"' EXIT
declare -a upload_assets=()

for target in aarch64-apple-darwin x86_64-apple-darwin; do
  case "${target}" in
    aarch64-apple-darwin) expected_architecture="arm64" ;;
    x86_64-apple-darwin) expected_architecture="x86_64" ;;
  esac
  dmg="${artifact_dir}/Sigil_${version}_${target}.dmg"
  archive="${artifact_dir}/Sigil_${version}_${target}.app.tar.gz"
  required=(
    "${dmg}"
    "${dmg}.sha256"
    "${archive}"
    "${archive}.sha256"
    "${archive}.sig"
  )
  for asset in "${required[@]}"; do
    if [[ ! -f "${asset}" || -L "${asset}" || ! -s "${asset}" ]]; then
      echo "Desktop candidate asset must be a non-empty regular file: ${asset}" >&2
      exit 1
    fi
    upload_assets+=("${asset}")
  done
  (
    cd "${artifact_dir}"
    shasum -a 256 --check "$(basename "${dmg}").sha256"
    shasum -a 256 --check "$(basename "${archive}").sha256"
  )
  scripts/verify-desktop-update-signature.sh \
    --archive "${archive}" \
    --signature "${archive}.sig"
  scripts/verify-desktop-macos.sh \
    --dmg "${dmg}" \
    --expected-arch "${expected_architecture}" \
    --team-id "${team_id}" \
    --expected-version "${version}" \
    --expected-commit "${expected_commit}" \
    --notarized
  extract_dir="${temporary_root}/extract-${target}"
  python3 scripts/extract-verified-desktop-updater-app.py \
    --archive "${archive}" \
    --destination "${extract_dir}"
  scripts/verify-desktop-macos-app.sh \
    --app "${extract_dir}/Sigil.app" \
    --expected-arch "${expected_architecture}" \
    --team-id "${team_id}" \
    --expected-version "${version}" \
    --expected-commit "${expected_commit}" \
    --notarized
done

release_json="$(
  gh release view "${release_tag}" \
    --repo "${repository}" \
    --json assets,isDraft,tagName
)"
test "$(jq -r '.tagName' <<<"${release_json}")" = "${release_tag}"
if [[ "$(jq -r '.isDraft' <<<"${release_json}")" != "true" ]]; then
  echo "Desktop assets can only be uploaded to a draft release: ${release_tag}" >&2
  exit 1
fi
existing_names="$(jq -r '.assets[].name' <<<"${release_json}")"

for asset in "${upload_assets[@]}"; do
  name="$(basename "${asset}")"
  if grep -Fxq -- "${name}" <<<"${existing_names}"; then
    existing_dir="${temporary_root}/existing-${name}"
    mkdir -p "${existing_dir}"
    gh release download "${release_tag}" \
      --repo "${repository}" \
      --pattern "${name}" \
      --dir "${existing_dir}"
    if cmp -s "${asset}" "${existing_dir}/${name}"; then
      echo "keeping identical draft asset: ${name}"
      continue
    fi
    if [[ "${replace}" != true ]]; then
      echo "draft asset ${name} has different bytes; inspect it or rerun with explicit --replace" >&2
      exit 1
    fi
    if [[ "${dry_run}" == true ]]; then
      echo "would replace different draft asset: ${name}"
      continue
    fi
    gh release delete-asset "${release_tag}" "${name}" \
      --repo "${repository}" \
      --yes
  elif [[ "${dry_run}" == true ]]; then
    echo "would upload draft asset: ${name}"
    continue
  fi
  gh release upload "${release_tag}" "${asset}" --repo "${repository}"
done

if [[ "${dry_run}" == true ]]; then
  echo "Desktop candidate verification passed; no assets were changed"
  exit 0
fi

remote_dir="${temporary_root}/remote"
mkdir -p "${remote_dir}"
for asset in "${upload_assets[@]}"; do
  name="$(basename "${asset}")"
  gh release download "${release_tag}" \
    --repo "${repository}" \
    --pattern "${name}" \
    --dir "${remote_dir}"
  cmp "${asset}" "${remote_dir}/${name}"
done
echo "uploaded and re-downloaded the complete Desktop candidate for ${release_tag}"

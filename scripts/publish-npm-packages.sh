#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'USAGE'
Usage: scripts/publish-npm-packages.sh --version <version> (--packages-dir <dir> | --package-tarballs-dir <dir>) [--tag <tag>] [--dry-run]

Publish generated @sigil-ai platform packages before the root launcher package.
Already-published package versions are skipped so a failed release workflow can
resume safely, but only when the requested dist-tag already points at that exact
version. Every registry read and channel monotonicity check fails closed.

Options:
  --version VERSION      Exact package version to publish.
  --packages-dir DIR     Directory created by prepare-npm-packages.sh.
  --package-tarballs-dir DIR
                         Directory containing its npm pack .tgz outputs.
  --tag TAG              npm dist-tag. Defaults to alpha/beta for matching prereleases and latest otherwise.
  --dry-run              Validate package metadata and print the publish plan without registry access.
  -h, --help             Show this help.
USAGE
}

repo_root="$(git rev-parse --show-toplevel 2>/dev/null || pwd)"
version=""
packages_dir=""
package_tarballs_dir=""
dist_tag=""
dry_run=false
publish_verify_attempts="${SIGIL_NPM_PUBLISH_VERIFY_ATTEMPTS:-12}"
publish_verify_delay_seconds="${SIGIL_NPM_PUBLISH_VERIFY_DELAY_SECONDS:-5}"

while [[ $# -gt 0 ]]; do
  case "$1" in
    --version)
      version="${2-}"
      shift 2
      ;;
    --packages-dir)
      packages_dir="${2-}"
      shift 2
      ;;
    --package-tarballs-dir)
      package_tarballs_dir="${2-}"
      shift 2
      ;;
    --tag)
      dist_tag="${2-}"
      shift 2
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

if [[ -z "${version}" ]] ||
  [[ -z "${packages_dir}" && -z "${package_tarballs_dir}" ]] ||
  [[ -n "${packages_dir}" && -n "${package_tarballs_dir}" ]]; then
  usage >&2
  exit 2
fi

if [[ ! "${publish_verify_attempts}" =~ ^[1-9][0-9]*$ ]]; then
  echo "SIGIL_NPM_PUBLISH_VERIFY_ATTEMPTS must be a positive integer" >&2
  exit 2
fi
if [[ ! "${publish_verify_delay_seconds}" =~ ^[0-9]+$ ]]; then
  echo "SIGIL_NPM_PUBLISH_VERIFY_DELAY_SECONDS must be a non-negative integer" >&2
  exit 2
fi

if [[ -z "${dist_tag}" ]]; then
  case "${version}" in
    *-alpha | *-alpha.*) dist_tag="alpha" ;;
    *-beta | *-beta.*) dist_tag="beta" ;;
    *-*)
      echo "unsupported prerelease channel in ${version}; pass --tag explicitly" >&2
      exit 2
      ;;
    *) dist_tag="latest" ;;
  esac
fi

case "${dist_tag}" in
  alpha | beta) release_channel="${dist_tag}" ;;
  latest) release_channel="stable" ;;
  *)
    echo "unsupported npm dist-tag for release publication: ${dist_tag}" >&2
    exit 2
    ;;
esac

declare -a platform_package_sources=()
declare -a platform_package_metadata=()
root_package_source=""
root_package_metadata=""
metadata_root=""

if [[ -n "${packages_dir}" ]]; then
  if [[ "${packages_dir}" != /* ]]; then
    packages_dir="${repo_root}/${packages_dir}"
  fi
  root_package_source="${packages_dir}/sigil"
  root_package_metadata="${root_package_source}/package.json"
  if [[ ! -d "${root_package_source}" ]]; then
    echo "missing root npm package directory: ${root_package_source}" >&2
    exit 1
  fi
  shopt -s nullglob
  platform_package_sources=("${packages_dir}"/sigil-*)
  shopt -u nullglob
  for package_source in "${platform_package_sources[@]}"; do
    platform_package_metadata+=("${package_source}/package.json")
  done
else
  if [[ "${package_tarballs_dir}" != /* ]]; then
    package_tarballs_dir="${repo_root}/${package_tarballs_dir}"
  fi
  root_package_source="${package_tarballs_dir}/sigil-ai-sigil-${version}.tgz"
  if [[ ! -f "${root_package_source}" || -L "${root_package_source}" ]]; then
    echo "missing root npm package tarball: ${root_package_source}" >&2
    exit 1
  fi
  shopt -s nullglob
  # This intentionally expands the package tarball inventory.
  # shellcheck disable=SC2206
  platform_package_sources=("${package_tarballs_dir}"/sigil-ai-sigil-*-${version}.tgz)
  shopt -u nullglob
  metadata_root="$(mktemp -d "${TMPDIR:-/tmp}/sigil-npm-metadata.XXXXXX")"
  trap 'rm -rf "${metadata_root}"' EXIT
  for index in "${!platform_package_sources[@]}"; do
    package_source="${platform_package_sources[$index]}"
    if [[ ! -f "${package_source}" || -L "${package_source}" ]]; then
      echo "npm package tarball must be a regular non-symlink file: ${package_source}" >&2
      exit 1
    fi
    package_json="${metadata_root}/platform-${index}.json"
    tar -xOf "${package_source}" package/package.json >"${package_json}"
    platform_package_metadata+=("${package_json}")
  done
  root_package_metadata="${metadata_root}/root.json"
  tar -xOf "${root_package_source}" package/package.json >"${root_package_metadata}"
fi

if [[ "${#platform_package_sources[@]}" -eq 0 ]]; then
  echo "no platform npm packages found" >&2
  exit 1
fi

wait_for_published_dist_tag() {
  local package_name="$1"
  local expected_version="$2"
  local attempt
  local observed_version
  local npm_error

  for ((attempt = 1; attempt <= publish_verify_attempts; attempt++)); do
    npm_error="$(mktemp "${TMPDIR:-/tmp}/sigil-npm-view.XXXXXX")"
    if observed_version="$(
      npm view "${package_name}@${dist_tag}" version 2>"${npm_error}"
    )"; then
      rm -f "${npm_error}"
      if [[ "${observed_version}" == "${expected_version}" ]]; then
        echo "npm ${package_name}@${dist_tag} converged to ${expected_version}"
        return
      fi
      echo "waiting for npm ${package_name}@${dist_tag}: observed ${observed_version}" >&2
    elif grep -Fq "E404" "${npm_error}"; then
      rm -f "${npm_error}"
      echo "waiting for npm ${package_name}@${dist_tag}: not visible yet" >&2
    else
      cat "${npm_error}" >&2
      rm -f "${npm_error}"
      echo "cannot prove npm convergence for ${package_name}@${dist_tag}" >&2
      return 1
    fi

    if ((attempt < publish_verify_attempts)); then
      sleep "${publish_verify_delay_seconds}"
    fi
  done

  echo "npm ${package_name}@${dist_tag} did not converge to ${expected_version} after ${publish_verify_attempts} attempts" >&2
  return 1
}

publish_package() {
  local package_source="$1"
  local package_json="$2"
  local package_name
  local package_version
  local published_version
  local current_tag_version
  local dist_tags_json
  local npm_error

  if [[ ! -f "${package_json}" ]]; then
    echo "missing npm package metadata: ${package_json}" >&2
    exit 1
  fi

  package_name="$(node -e 'const value = require(process.argv[1]); process.stdout.write(value.name || "")' "${package_json}")"
  package_version="$(node -e 'const value = require(process.argv[1]); process.stdout.write(value.version || "")' "${package_json}")"

  if [[ "${package_name}" != "@sigil-ai/sigil" && "${package_name}" != @sigil-ai/sigil-* ]]; then
    echo "unexpected npm package name in ${package_json}: ${package_name}" >&2
    exit 1
  fi
  if [[ "${package_version}" != "${version}" ]]; then
    echo "npm package version mismatch for ${package_name}: expected ${version}, found ${package_version}" >&2
    exit 1
  fi

  if [[ "${dry_run}" == true ]]; then
    echo "would publish ${package_name}@${version} with tag ${dist_tag}"
    return
  fi

  npm_error="$(mktemp "${TMPDIR:-/tmp}/sigil-npm-view.XXXXXX")"
  if dist_tags_json="$(npm view "${package_name}" dist-tags --json 2>"${npm_error}")"; then
    current_tag_version="$(
      node -e '
        const tags = JSON.parse(process.argv[1]);
        const value = tags?.[process.argv[2]];
        if (typeof value === "string") process.stdout.write(value);
      ' "${dist_tags_json}" "${dist_tag}"
    )"
  elif grep -Fq "E404" "${npm_error}"; then
    current_tag_version=""
  else
    cat "${npm_error}" >&2
    rm -f "${npm_error}"
    echo "cannot prove npm dist-tag state for ${package_name}" >&2
    exit 1
  fi
  rm -f "${npm_error}"

  if [[ -n "${current_tag_version}" ]]; then
    node "${repo_root}/scripts/release-channel-version.mjs" \
      --candidate "${version}" \
      --channel "${release_channel}" \
      --current "${current_tag_version}" \
      --surface "npm ${package_name}@${dist_tag}"
  fi

  npm_error="$(mktemp "${TMPDIR:-/tmp}/sigil-npm-view.XXXXXX")"
  if published_version="$(npm view "${package_name}@${version}" version 2>"${npm_error}")"; then
    if [[ "${published_version}" != "${version}" ]]; then
      cat "${npm_error}" >&2
      rm -f "${npm_error}"
      echo "npm returned an unexpected exact version for ${package_name}@${version}" >&2
      exit 1
    fi
    rm -f "${npm_error}"
    if [[ "${current_tag_version}" != "${version}" ]]; then
      echo "already published ${package_name}@${version}, but ${dist_tag} points to ${current_tag_version:-nothing}" >&2
      exit 1
    fi
    echo "already published ${package_name}@${version}; desired dist-tag state verified"
    return
  elif ! grep -Fq "E404" "${npm_error}"; then
    cat "${npm_error}" >&2
    rm -f "${npm_error}"
    echo "cannot prove exact npm version state for ${package_name}@${version}" >&2
    exit 1
  fi
  rm -f "${npm_error}"

  echo "publishing ${package_name}@${version} with tag ${dist_tag}"
  npm publish "${package_source}" --access public --tag "${dist_tag}"
  wait_for_published_dist_tag "${package_name}" "${version}"
}

for index in "${!platform_package_sources[@]}"; do
  package_source="${platform_package_sources[$index]}"
  if [[ -d "${package_source}" || -f "${package_source}" ]]; then
    publish_package "${package_source}" "${platform_package_metadata[$index]}"
  fi
done

publish_package "${root_package_source}" "${root_package_metadata}"

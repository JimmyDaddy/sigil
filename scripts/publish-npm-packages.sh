#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'USAGE'
Usage: scripts/publish-npm-packages.sh --version <version> --packages-dir <dir> [--tag <tag>] [--dry-run]

Publish generated @sigil-ai platform packages before the root launcher package.
Already-published package versions are skipped so a failed release workflow can
resume safely.

Options:
  --version VERSION      Exact package version to publish.
  --packages-dir DIR     Directory created by prepare-npm-packages.sh.
  --tag TAG              npm dist-tag. Defaults to alpha for prereleases and latest otherwise.
  --dry-run              Validate package metadata and print the publish plan without registry access.
  -h, --help             Show this help.
USAGE
}

repo_root="$(git rev-parse --show-toplevel 2>/dev/null || pwd)"
version=""
packages_dir=""
dist_tag=""
dry_run=false

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

if [[ -z "${version}" || -z "${packages_dir}" ]]; then
  usage >&2
  exit 2
fi

if [[ -z "${dist_tag}" ]]; then
  dist_tag="latest"
  if [[ "${version}" == *-* ]]; then
    dist_tag="alpha"
  fi
fi

if [[ "${packages_dir}" != /* ]]; then
  packages_dir="${repo_root}/${packages_dir}"
fi

root_package_dir="${packages_dir}/sigil"
if [[ ! -d "${root_package_dir}" ]]; then
  echo "missing root npm package directory: ${root_package_dir}" >&2
  exit 1
fi

shopt -s nullglob
platform_package_dirs=("${packages_dir}"/sigil-*)
shopt -u nullglob

if [[ "${#platform_package_dirs[@]}" -eq 0 ]]; then
  echo "no platform npm package directories found in ${packages_dir}" >&2
  exit 1
fi

publish_package() {
  local package_dir="$1"
  local package_json="${package_dir}/package.json"
  local package_name
  local package_version
  local published_version

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

  if published_version="$(npm view "${package_name}@${version}" version 2>/dev/null)" \
    && [[ "${published_version}" == "${version}" ]]; then
    echo "already published ${package_name}@${version}; skipping"
    return
  fi

  echo "publishing ${package_name}@${version} with tag ${dist_tag}"
  npm publish "${package_dir}" --access public --tag "${dist_tag}"
}

for package_dir in "${platform_package_dirs[@]}"; do
  if [[ -d "${package_dir}" ]]; then
    publish_package "${package_dir}"
  fi
done

publish_package "${root_package_dir}"

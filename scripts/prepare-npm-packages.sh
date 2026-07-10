#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'USAGE'
Usage: scripts/prepare-npm-packages.sh --version <version> [--dist-dir <dir>] [--out-dir <dir>] [--pack-destination <dir>]

Generate npm package directories for @sigil-ai/sigil from Sigil release
archives. The root package is a Node.js launcher; platform-specific optional
packages carry the built Rust binaries.

Options:
  --version VERSION        Release version without the leading v, for example 0.1.0.
  --dist-dir DIR          Directory containing sigil-<version>-<target>.tar.gz archives. Defaults to dist.
  --out-dir DIR           Directory for generated package folders. Defaults to dist/npm-packages.
  --pack-destination DIR  Also run npm pack for each generated package and write .tgz files here.
  -h, --help              Show this help.
USAGE
}

repo_root="$(git rev-parse --show-toplevel 2>/dev/null || pwd)"
cd "${repo_root}"

version=""
dist_dir="dist"
out_dir="dist/npm-packages"
pack_destination=""
scope="@sigil-ai"

while [[ $# -gt 0 ]]; do
  case "$1" in
    --version)
      version="${2-}"
      shift 2
      ;;
    --dist-dir)
      dist_dir="${2-}"
      shift 2
      ;;
    --out-dir)
      out_dir="${2-}"
      shift 2
      ;;
    --pack-destination)
      pack_destination="${2-}"
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

if [[ -z "${version}" ]]; then
  usage >&2
  exit 2
fi

target_to_node_platform() {
  case "$1" in
    aarch64-apple-darwin) echo "darwin arm64 darwin-arm64" ;;
    x86_64-apple-darwin) echo "darwin x64 darwin-x64" ;;
    aarch64-unknown-linux-gnu) echo "linux arm64 linux-arm64" ;;
    x86_64-unknown-linux-gnu) echo "linux x64 linux-x64" ;;
    aarch64-pc-windows-msvc) echo "win32 arm64 win32-arm64" ;;
    x86_64-pc-windows-msvc) echo "win32 x64 win32-x64" ;;
    *) return 1 ;;
  esac
}

rm -rf "${out_dir}"
mkdir -p "${out_dir}"

declare -a platform_package_names=()
declare -a package_dirs=()

shopt -s nullglob
archives=("${dist_dir}/sigil-${version}-"*.tar.gz)
shopt -u nullglob

if [[ "${#archives[@]}" -eq 0 ]]; then
  echo "no release archives found in ${dist_dir} for version ${version}" >&2
  exit 1
fi

for archive in "${archives[@]}"; do
  archive_name="$(basename "${archive}")"
  target_triple="${archive_name#sigil-${version}-}"
  target_triple="${target_triple%.tar.gz}"

  if ! platform_info="$(target_to_node_platform "${target_triple}")"; then
    echo "skipping unsupported npm target: ${target_triple}" >&2
    continue
  fi

  read -r os cpu package_suffix <<<"${platform_info}"
  package_name="${scope}/sigil-${package_suffix}"
  package_dir="${out_dir}/sigil-${package_suffix}"
  binary_name="sigil"
  if [[ "${os}" == "win32" ]]; then
    binary_name="sigil.exe"
  fi

  tmp_dir="$(mktemp -d)"
  trap 'rm -rf "${tmp_dir}"' EXIT
  tar -xzf "${archive}" -C "${tmp_dir}"
  payload_dir="${tmp_dir}/sigil-${version}-${target_triple}"
  binary_path="${payload_dir}/${binary_name}"
  if [[ ! -f "${binary_path}" ]]; then
    echo "missing binary in ${archive}: ${binary_name}" >&2
    exit 1
  fi

  mkdir -p "${package_dir}/bin"
  cp "${binary_path}" "${package_dir}/bin/${binary_name}"
  cp "${payload_dir}/LICENSE" "${package_dir}/LICENSE"
  if [[ "${os}" != "win32" ]]; then
    chmod 0755 "${package_dir}/bin/${binary_name}"
  fi

  cat >"${package_dir}/package.json" <<JSON
{
  "name": "${package_name}",
  "version": "${version}",
  "description": "Platform binary for Sigil",
  "license": "MIT",
  "repository": {
    "type": "git",
    "url": "git+https://github.com/JimmyDaddy/sigil.git"
  },
  "homepage": "https://jimmydaddy.github.io/sigil/",
  "os": ["${os}"],
  "cpu": ["${cpu}"],
  "files": ["bin", "LICENSE"]
}
JSON

  platform_package_names+=("${package_name}")
  package_dirs+=("${package_dir}")
  rm -rf "${tmp_dir}"
  trap - EXIT
done

if [[ "${#platform_package_names[@]}" -eq 0 ]]; then
  echo "no supported npm platform packages were generated" >&2
  exit 1
fi

root_package_dir="${out_dir}/sigil"
mkdir -p "${root_package_dir}/bin"
cp npm/sigil/bin/sigil.js "${root_package_dir}/bin/sigil.js"
cp npm/sigil/README.md "${root_package_dir}/README.md"
cp LICENSE "${root_package_dir}/LICENSE"
chmod 0755 "${root_package_dir}/bin/sigil.js"

optional_dependencies=""
for package_name in "${platform_package_names[@]}"; do
  if [[ -n "${optional_dependencies}" ]]; then
    optional_dependencies+=$',\n'
  fi
  optional_dependencies+="    \"${package_name}\": \"${version}\""
done

cat >"${root_package_dir}/package.json" <<JSON
{
  "name": "${scope}/sigil",
  "version": "${version}",
  "description": "TUI-first Rust AI coding agent",
  "license": "MIT",
  "repository": {
    "type": "git",
    "url": "git+https://github.com/JimmyDaddy/sigil.git"
  },
  "homepage": "https://jimmydaddy.github.io/sigil/",
  "bin": {
    "sigil": "bin/sigil.js"
  },
  "files": ["bin", "README.md", "LICENSE"],
  "engines": {
    "node": ">=18"
  },
  "optionalDependencies": {
${optional_dependencies}
  }
}
JSON

package_dirs+=("${root_package_dir}")

if [[ -n "${pack_destination}" ]]; then
  pack_destination_path="${pack_destination}"
  if [[ "${pack_destination}" != /* ]]; then
    pack_destination_path="${repo_root}/${pack_destination}"
  fi
  mkdir -p "${pack_destination_path}"
  for package_dir in "${package_dirs[@]}"; do
    (cd "${package_dir}" && npm pack --pack-destination "${pack_destination_path}" >/dev/null)
  done
fi

echo "npm packages staged at ${out_dir}"

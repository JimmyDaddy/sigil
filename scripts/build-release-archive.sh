#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'USAGE'
Usage: scripts/build-release-archive.sh [--target <triple>] [--out-dir <dir>] [--skip-smoke]

Build the release `sigil` binary, run install-oriented smoke checks, and write a
versioned tar.gz archive plus a sha256 checksum file.

Options:
  --target TRIPLE  Build for an explicit Rust target triple.
  --out-dir DIR    Output directory. Defaults to dist.
  --skip-smoke     Do not run `sigil --version` and `sigil doctor` on the built binary.
  -h, --help       Show this help.
USAGE
}

repo_root="$(git rev-parse --show-toplevel 2>/dev/null || pwd)"
cd "${repo_root}"

out_dir="dist"
target_triple=""
run_smoke=1

while [[ $# -gt 0 ]]; do
  case "$1" in
    --target)
      if [[ $# -lt 2 ]]; then
        echo "missing value for --target" >&2
        exit 2
      fi
      target_triple="$2"
      shift 2
      ;;
    --out-dir)
      if [[ $# -lt 2 ]]; then
        echo "missing value for --out-dir" >&2
        exit 2
      fi
      out_dir="$2"
      shift 2
      ;;
    --skip-smoke)
      run_smoke=0
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

if [[ -z "${target_triple}" ]]; then
  target_triple="$(rustc -vV | sed -n 's/^host: //p')"
fi
if [[ -z "${target_triple}" ]]; then
  echo "unable to determine Rust target triple" >&2
  exit 1
fi

version="$(cargo pkgid -p sigil | sed 's/.*#//')"
git_hash="$(git rev-parse --short=12 HEAD 2>/dev/null || printf 'unknown')"
binary_name="sigil"
case "${target_triple}" in
  *windows*) binary_name="sigil.exe" ;;
esac

target_args=()
binary_path="target/release/${binary_name}"
if [[ -n "${target_triple}" ]]; then
  target_args=(--target "${target_triple}")
  binary_path="target/${target_triple}/release/${binary_name}"
fi

echo "building sigil ${version} for ${target_triple}"
SIGIL_BUILD_GIT_HASH="${git_hash}" \
  SIGIL_BUILD_TARGET="${target_triple}" \
  SIGIL_BUILD_PROFILE="release" \
  cargo build -p sigil --release --locked "${target_args[@]}"

if [[ ! -x "${binary_path}" ]]; then
  echo "built binary is missing or not executable: ${binary_path}" >&2
  exit 1
fi

if [[ "${run_smoke}" -eq 1 ]]; then
  echo "smoke: ${binary_path} --version"
  "${binary_path}" --version
  echo "smoke: ${binary_path} doctor"
  "${binary_path}" doctor >/dev/null
fi

archive_base="sigil-${version}-${target_triple}"
mkdir -p "${out_dir}"
stage_dir="$(mktemp -d)"
trap 'rm -rf "${stage_dir}"' EXIT
payload_dir="${stage_dir}/${archive_base}"
mkdir -p "${payload_dir}"

cp "${binary_path}" "${payload_dir}/${binary_name}"
cp LICENSE README.md README.zh-CN.md "${payload_dir}/"
mkdir -p "${payload_dir}/assets"
mkdir -p "${payload_dir}/assets/logo"
cp assets/logo/*.png assets/logo/*.svg assets/logo/README.md "${payload_dir}/assets/logo/"
mkdir -p "${payload_dir}/docs/en" "${payload_dir}/docs/zh-CN"
cp docs/en/installation.md "${payload_dir}/docs/en/"
cp docs/zh-CN/installation.md "${payload_dir}/docs/zh-CN/"

archive_path="${out_dir}/${archive_base}.tar.gz"
tar -C "${stage_dir}" -czf "${archive_path}" "${archive_base}"

checksum_path="${archive_path}.sha256"
if command -v sha256sum >/dev/null 2>&1; then
  sha256sum "${archive_path}" >"${checksum_path}"
else
  shasum -a 256 "${archive_path}" >"${checksum_path}"
fi

echo "archive: ${archive_path}"
echo "checksum: ${checksum_path}"

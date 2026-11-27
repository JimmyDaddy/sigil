#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'USAGE'
Usage: scripts/build-release-archive.sh [--target <triple>] [--out-dir <dir>] [--binary <path>] [--orchestration-eval-manifest <path>] [--skip-smoke]

Build the release `sigil` binary, run install-oriented smoke checks, and write a
versioned tar.gz archive plus a sha256 checksum file.

Options:
  --target TRIPLE  Build for an explicit Rust target triple.
  --out-dir DIR    Output directory. Defaults to dist.
  --binary PATH    Reuse an already-built `sigil` binary instead of running `cargo build --release`.
  --orchestration-eval-manifest PATH
                   Qualified RFC-0053 report for this exact commit. The built
                   binary validates it and derives the release rollout sidecar.
  --skip-smoke     Do not run `sigil --version` and `sigil doctor` on the built binary.
  -h, --help       Show this help.
USAGE
}

repo_root="$(git rev-parse --show-toplevel 2>/dev/null || pwd)"
cd "${repo_root}"

out_dir="dist"
target_triple=""
prebuilt_binary=""
run_smoke=1
orchestration_eval_manifest=""

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
    --binary)
      if [[ $# -lt 2 ]]; then
        echo "missing value for --binary" >&2
        exit 2
      fi
      prebuilt_binary="$2"
      shift 2
      ;;
    --orchestration-eval-manifest)
      if [[ $# -lt 2 ]]; then
        echo "missing value for --orchestration-eval-manifest" >&2
        exit 2
      fi
      orchestration_eval_manifest="$2"
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

if [[ -n "${prebuilt_binary}" ]]; then
  binary_path="${prebuilt_binary}"
  echo "packaging prebuilt sigil ${version} for ${target_triple}: ${binary_path}"
else
  target_args=()
  binary_path="target/release/${binary_name}"
  if [[ -n "${target_triple}" ]]; then
    target_args=(--target "${target_triple}")
    binary_path="target/${target_triple}/release/${binary_name}"
  fi

  echo "building sigil ${version} for ${target_triple}"
  SIGIL_BUILD_GIT_HASH="${git_hash}" \
    SIGIL_RUNTIME_BUILD_GIT_HASH="${git_hash}" \
    SIGIL_BUILD_TARGET="${target_triple}" \
    SIGIL_BUILD_PROFILE="release" \
    SIGIL_BUILD_DISTRIBUTION="github-release" \
    cargo build -p sigil --release --locked "${target_args[@]}"
fi

if [[ ! -x "${binary_path}" ]]; then
  echo "built binary is missing or not executable: ${binary_path}" >&2
  exit 1
fi

stage_dir="$(mktemp -d)"
trap 'rm -rf "${stage_dir}"' EXIT

if [[ "${run_smoke}" -eq 1 ]]; then
  echo "smoke: ${binary_path} --version"
  "${binary_path}" --version
  echo "smoke: ${binary_path} doctor --output json"
  smoke_home="${stage_dir}/smoke-home"
  mkdir -p "${smoke_home}"
  smoke_config="${stage_dir}/smoke-sigil.toml"
  smoke_doctor_json="${stage_dir}/smoke-doctor.json"
  cat >"${smoke_config}" <<'EOF'
config_version = 2

[workspace]
root = "."

[storage]
credential_store = "file"

[agent]
connection = "smoke-default"
model = "deepseek-chat"

[connections.smoke-default]
label = "Smoke"
provider = "deepseek"
protocol = "deepseek"
base_url = "https://api.deepseek.com"
credential.source = "environment"
credential.name = "SIGIL_API_KEY"
EOF
  HOME="${smoke_home}" \
    USERPROFILE="${smoke_home}" \
    APPDATA="${smoke_home}/AppData/Roaming" \
    LOCALAPPDATA="${smoke_home}/AppData/Local" \
    XDG_CONFIG_HOME="${smoke_home}/.config" \
    XDG_STATE_HOME="${smoke_home}/.local/state" \
    XDG_CACHE_HOME="${smoke_home}/.cache" \
    SIGIL_STATE_HOME="${smoke_home}/.local/state" \
    SIGIL_CACHE_HOME="${smoke_home}/.cache" \
    "${binary_path}" --config "${smoke_config}" doctor --output json >"${smoke_doctor_json}"
  python3 - "${smoke_doctor_json}" "${smoke_home}" <<'PY'
import json
import os
import sys

report_path, smoke_home = sys.argv[1], sys.argv[2]
with open(report_path, encoding="utf-8") as fh:
    report = json.load(fh)
assert report.get("schema_version", 0) >= 1, f"invalid schema_version: {report}"
checks = {item["name"]: item for item in report.get("checks", [])}
assert checks.get("config:load", {}).get("status") == "ok", f"config:load failed: {checks.get('config:load')}"
assert report.get("cutover", {}).get("blockers") == [], f"unexpected cutover blockers: {report.get('cutover')}"
for name in ("cutover:epoch", "cutover:authority", "cutover:blocker"):
    assert checks.get(name, {}).get("status") != "error", f"{name} reported error: {checks.get(name)}"
assert not os.path.exists(os.path.join(smoke_home, ".sigil")), "doctor smoke must stay read-only"
PY
fi

archive_base="sigil-${version}-${target_triple}"
mkdir -p "${out_dir}"
payload_dir="${stage_dir}/${archive_base}"
mkdir -p "${payload_dir}"

cp "${binary_path}" "${payload_dir}/${binary_name}"
if [[ -n "${orchestration_eval_manifest}" ]]; then
  echo "deriving qualified orchestration rollout sidecar"
  "${binary_path}" model-eval-rollout-manifest \
    --report "${orchestration_eval_manifest}" \
    --output "${payload_dir}/sigil-orchestration-rollout-v1.json"
fi
cp LICENSE README.md README.zh-CN.md "${payload_dir}/"
mkdir -p "${payload_dir}/assets"
mkdir -p "${payload_dir}/assets/logo"
cp assets/logo/*.png assets/logo/*.svg assets/logo/README.md "${payload_dir}/assets/logo/"
mkdir -p "${payload_dir}/site/assets/screenshots"
cp site/assets/screenshots/tui-session.svg "${payload_dir}/site/assets/screenshots/"
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

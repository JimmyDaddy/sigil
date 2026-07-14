#!/usr/bin/env bash
set -euo pipefail

repo_root="$(git rev-parse --show-toplevel)"
tmp_dir="$(mktemp -d)"
trap 'rm -rf "${tmp_dir}"' EXIT

packages_dir="${tmp_dir}/packages"
fake_bin="${tmp_dir}/bin"
log_file="${tmp_dir}/npm.log"
mkdir -p "${packages_dir}" "${fake_bin}"

make_package() {
  local dir_name="$1"
  local package_name="$2"
  mkdir -p "${packages_dir}/${dir_name}"
  printf '{"name":"%s","version":"1.2.3-alpha.1"}\n' "${package_name}" \
    >"${packages_dir}/${dir_name}/package.json"
}

make_package sigil-darwin-arm64 @sigil-ai/sigil-darwin-arm64
make_package sigil-linux-x64 @sigil-ai/sigil-linux-x64
make_package sigil-win32-x64 @sigil-ai/sigil-win32-x64
make_package sigil @sigil-ai/sigil

cat >"${fake_bin}/npm" <<'FAKE_NPM'
#!/usr/bin/env bash
set -euo pipefail
printf '%s\n' "$*" >>"${FAKE_NPM_LOG}"
if [[ "${1-}" == "view" ]]; then
  if [[ "${2-}" == "@sigil-ai/sigil-darwin-arm64@1.2.3-alpha.1" ]]; then
    echo "1.2.3-alpha.1"
    exit 0
  fi
  exit 1
fi
if [[ "${1-}" == "publish" ]]; then
  exit 0
fi
exit 2
FAKE_NPM
chmod +x "${fake_bin}/npm"

FAKE_NPM_LOG="${log_file}" PATH="${fake_bin}:${PATH}" \
  "${repo_root}/scripts/publish-npm-packages.sh" \
  --version 1.2.3-alpha.1 \
  --packages-dir "${packages_dir}" \
  --tag alpha

if grep -Fq "publish ${packages_dir}/sigil-darwin-arm64" "${log_file}"; then
  echo "already-published platform package was published again" >&2
  exit 1
fi

grep -Fqx "publish ${packages_dir}/sigil-linux-x64 --access public --tag alpha" "${log_file}"
grep -Fqx "publish ${packages_dir}/sigil-win32-x64 --access public --tag alpha" "${log_file}"

expected_last="publish ${packages_dir}/sigil --access public --tag alpha"
actual_last="$(tail -n 1 "${log_file}")"
if [[ "${actual_last}" != "${expected_last}" ]]; then
  echo "root package was not published last: ${actual_last}" >&2
  exit 1
fi

dry_run_output="$("${repo_root}/scripts/publish-npm-packages.sh" \
  --version 1.2.3-alpha.1 \
  --packages-dir "${packages_dir}" \
  --dry-run)"
grep -Fq "would publish @sigil-ai/sigil@1.2.3-alpha.1 with tag alpha" <<<"${dry_run_output}"

echo "publish npm packages tests passed"

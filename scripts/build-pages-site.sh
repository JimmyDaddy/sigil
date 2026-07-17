#!/usr/bin/env bash
set -euo pipefail

repo_root="$(git rev-parse --show-toplevel 2>/dev/null || pwd)"
cd "${repo_root}"

out_dir="${1:-_site}"
if [[ -z "${out_dir}" || "${out_dir}" == "/" ]]; then
  echo "invalid output directory: ${out_dir}" >&2
  exit 2
fi

case "${out_dir}" in
  _site | ./_site | .site | ./.site | /tmp/* | /var/folders/*) ;;
  *)
    echo "refusing to overwrite non-temporary output directory: ${out_dir}" >&2
    echo "use _site, .site, /tmp/*, or /var/folders/*" >&2
    exit 2
    ;;
esac

rm -rf "${out_dir}"
mkdir -p "${out_dir}/assets/logo" "${out_dir}/assets/social"
cp -R site/. "${out_dir}/"
rm -f "${out_dir}/README.md"
cp assets/logo/*.{png,svg} "${out_dir}/assets/logo/"
cp assets/social/*.{png,svg} "${out_dir}/assets/social/"
mkdir -p "${out_dir}/examples"
cp -R docs/examples/. "${out_dir}/examples/"
scripts/build-docs-site.rb "${out_dir}" >/dev/null

echo "pages site staged at ${out_dir}"

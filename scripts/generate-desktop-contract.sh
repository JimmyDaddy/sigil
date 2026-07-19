#!/usr/bin/env bash
set -euo pipefail

usage() {
  echo "Usage: scripts/generate-desktop-contract.sh --write|--check" >&2
}

if [[ "$#" -ne 1 ]]; then
  usage
  exit 2
fi

mode="$1"
case "${mode}" in
  --write|--check) ;;
  *)
    usage
    exit 2
    ;;
esac

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
desktop_root="${repo_root}/apps/desktop"
snapshot="${desktop_root}/contracts/sigil-openapi.json"
generated="${desktop_root}/src/generated/http-schema.ts"
tmp_dir="$(mktemp -d)"
trap 'rm -rf "${tmp_dir}"' EXIT

generated_snapshot="${tmp_dir}/sigil-openapi.json"
generated_types="${tmp_dir}/http-schema.ts"

cd "${repo_root}"
cargo run --quiet -p sigil-http --example export_openapi >"${generated_snapshot}"
pnpm --dir "${desktop_root}" exec openapi-typescript \
  "${generated_snapshot}" \
  --output "${generated_types}"

if [[ "${mode}" == "--write" ]]; then
  mkdir -p "$(dirname "${snapshot}")" "$(dirname "${generated}")"
  cp "${generated_snapshot}" "${snapshot}"
  cp "${generated_types}" "${generated}"
  echo "desktop contract generated"
  exit 0
fi

if [[ ! -f "${snapshot}" || ! -f "${generated}" ]]; then
  echo "desktop contract artifacts are missing; run pnpm contract:generate" >&2
  exit 1
fi

if ! cmp -s "${generated_snapshot}" "${snapshot}"; then
  echo "desktop OpenAPI snapshot drifted; run pnpm contract:generate" >&2
  diff -u "${snapshot}" "${generated_snapshot}" || true
  exit 1
fi

if ! cmp -s "${generated_types}" "${generated}"; then
  echo "desktop TypeScript DTOs drifted; run pnpm contract:generate" >&2
  diff -u "${generated}" "${generated_types}" || true
  exit 1
fi

echo "desktop contract drift check passed"

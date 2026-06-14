#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'USAGE'
Usage: scripts/generate-release-notes.sh <tag-or-ref>

Generate Markdown release notes from Conventional Commit subjects between the
previous tag and the given tag/ref. The script writes to stdout.
USAGE
}

if [[ "${1-}" == "-h" || "${1-}" == "--help" ]]; then
  usage
  exit 0
fi

if [[ $# -ne 1 ]]; then
  usage >&2
  exit 2
fi

target_ref="$1"
version="${target_ref#v}"

if ! git rev-parse --verify --quiet "${target_ref}^{commit}" >/dev/null; then
  echo "unknown tag or ref: ${target_ref}" >&2
  exit 1
fi

previous_tag="$(git describe --tags --abbrev=0 "${target_ref}^" 2>/dev/null || true)"
if [[ -n "${previous_tag}" ]]; then
  log_range="${previous_tag}..${target_ref}"
else
  log_range="${target_ref}"
fi

tmp_dir="$(mktemp -d)"
trap 'rm -rf "${tmp_dir}"' EXIT
features="${tmp_dir}/features.md"
fixes="${tmp_dir}/fixes.md"
docs="${tmp_dir}/docs.md"
maintenance="${tmp_dir}/maintenance.md"
other="${tmp_dir}/other.md"
: >"${features}"
: >"${fixes}"
: >"${docs}"
: >"${maintenance}"
: >"${other}"

while IFS=$'\t' read -r short_hash subject; do
  [[ -z "${short_hash}" ]] && continue
  entry="- ${subject} (${short_hash})"
  case "${subject}" in
    feat* | "feat("*)
      echo "${entry}" >>"${features}"
      ;;
    fix* | "fix("*)
      echo "${entry}" >>"${fixes}"
      ;;
    docs* | "docs("*)
      echo "${entry}" >>"${docs}"
      ;;
    build* | "build("* | chore* | "chore("* | ci* | "ci("* | refactor* | "refactor("* | test* | "test("*)
      echo "${entry}" >>"${maintenance}"
      ;;
    *)
      echo "${entry}" >>"${other}"
      ;;
  esac
done < <(git log --format='%h%x09%s' "${log_range}")

echo "# Sigil ${version}"
echo
if [[ -n "${previous_tag}" ]]; then
  echo "Changes since \`${previous_tag}\`."
else
  echo "Initial tagged release notes."
fi

print_section() {
  local title="$1"
  local file="$2"
  if [[ -s "${file}" ]]; then
    echo
    echo "## ${title}"
    echo
    cat "${file}"
  fi
}

print_section "Features" "${features}"
print_section "Fixes" "${fixes}"
print_section "Documentation" "${docs}"
print_section "Maintenance" "${maintenance}"
print_section "Other changes" "${other}"

echo
echo "## Verification"
echo
echo "- Release archives include SHA-256 checksum files."
echo "- GitHub artifact provenance attestations are generated for archive artifacts."
echo "- Run \`sigil --version\` and \`sigil doctor\` after install."

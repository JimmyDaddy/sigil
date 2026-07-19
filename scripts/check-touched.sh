#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'EOF'
Usage: scripts/check-touched.sh [--tier quick|standard|full] [--scope dirty|staged|base] [--base REF] [--dry-run]

Runs a risk-scaled local gate for the current change set.

Tiers:
  quick     docs whitespace check, rustfmt, workspace cargo check, touched crate tests
  standard  quick + touched crate clippy
  full      rustfmt, workspace cargo check, workspace cargo test, workspace clippy

Scopes:
  dirty     tracked changes against HEAD plus untracked files (default)
  staged    staged changes only
  base      changes against --base REF plus untracked files
EOF
}

tier="quick"
scope="dirty"
base_ref="origin/main"
dry_run=0

while [[ $# -gt 0 ]]; do
  case "$1" in
    --tier)
      tier="${2:-}"
      shift 2
      ;;
    --scope)
      scope="${2:-}"
      shift 2
      ;;
    --base)
      base_ref="${2:-}"
      shift 2
      ;;
    --dry-run)
      dry_run=1
      shift
      ;;
    -h|--help)
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

case "${tier}" in
  quick|standard|full) ;;
  *)
    echo "invalid tier: ${tier}" >&2
    usage >&2
    exit 2
    ;;
esac

case "${scope}" in
  dirty|staged|base) ;;
  *)
    echo "invalid scope: ${scope}" >&2
    usage >&2
    exit 2
    ;;
esac

run_cmd() {
  printf '+'
  printf ' %q' "$@"
  printf '\n'
  if [[ "${dry_run}" == "0" ]]; then
    "$@"
  fi
}

changed_files() {
  case "${scope}" in
    dirty)
      {
        git diff --name-only HEAD --
        git ls-files --others --exclude-standard
      } | sort -u
      ;;
    staged)
      git diff --cached --name-only -- | sort -u
      ;;
    base)
      {
        git diff --name-only "${base_ref}" --
        git ls-files --others --exclude-standard
      } | sort -u
      ;;
  esac
}

workspace_packages() {
  {
    find crates -mindepth 2 -maxdepth 2 -name Cargo.toml -print \
      | sed -E 's#^crates/([^/]+)/Cargo.toml$#\1#'
    printf '%s\n' "sigil-desktop-app"
  } | sort
}

tmp_dir="$(mktemp -d)"
trap 'rm -rf "${tmp_dir}"' EXIT
files_file="${tmp_dir}/changed-files"
packages_file="${tmp_dir}/packages"
: >"${packages_file}"
changed_files >"${files_file}"

if [[ ! -s "${files_file}" ]]; then
  echo "no changed files for scope=${scope}"
  exit 0
fi

rust_changed=0
docs_changed=0
workspace_manifest_changed=0
high_risk_changed=0
desktop_changed=0

while IFS= read -r path; do
  case "${path}" in
    crates/*/*)
      crate="${path#crates/}"
      crate="${crate%%/*}"
      printf '%s\n' "${crate}" >>"${packages_file}"
      ;;
    apps/desktop/src-tauri/*|apps/desktop/src-tauri/**/*)
      printf '%s\n' "sigil-desktop-app" >>"${packages_file}"
      ;;
  esac

  case "${path}" in
    *.rs|Cargo.toml|*/Cargo.toml|Cargo.lock|rust-toolchain.toml)
      rust_changed=1
      ;;
  esac

  case "${path}" in
    Cargo.toml|Cargo.lock|rust-toolchain.toml)
      workspace_manifest_changed=1
      ;;
  esac

  case "${path}" in
    README.md|README.*.md|docs/*|docs/**/*|dev/docs/*|dev/docs/**/*|dev/governance/*|dev/governance/**/*|*.md)
      docs_changed=1
      ;;
  esac

  case "${path}" in
    crates/sigil-kernel/src/agent.rs|\
    crates/sigil-kernel/src/event.rs|\
    crates/sigil-kernel/src/session.rs|\
    crates/sigil-kernel/src/mutation.rs|\
    crates/sigil-kernel/src/verification.rs|\
    crates/sigil-kernel/src/permission.rs|\
    crates/sigil-kernel/src/task_orchestrator.rs|\
    crates/sigil-kernel/src/tool.rs|\
    crates/sigil-tui/src/runner/*|\
    crates/sigil-tui/src/app/worker_bridge.rs|\
    crates/sigil-mcp/src/*|\
    crates/sigil-tools-builtin/src/*)
      high_risk_changed=1
      ;;
  esac

  case "${path}" in
    apps/desktop/*|apps/desktop/**/*|crates/sigil-http/src/openapi.rs|scripts/generate-desktop-contract.sh)
      desktop_changed=1
      ;;
  esac
done <"${files_file}"

sort -u "${packages_file}" -o "${packages_file}"
packages=()
while IFS= read -r package; do
  [[ -n "${package}" ]] || continue
  packages+=("${package}")
done <"${packages_file}"

echo "scope: ${scope}"
if [[ "${scope}" == "base" ]]; then
  echo "base: ${base_ref}"
fi
echo "tier: ${tier}"
printf 'changed files: %s\n' "$(wc -l <"${files_file}" | tr -d ' ')"
if [[ "${#packages[@]}" -gt 0 ]]; then
  printf 'touched packages: %s\n' "${packages[*]}"
fi
if [[ "${high_risk_changed}" == "1" && "${tier}" == "quick" ]]; then
  echo "note: high-risk paths changed; prefer --tier standard before commit and --tier full before release"
fi

run_cmd git diff --check --

if [[ "${desktop_changed}" == "1" ]]; then
  run_cmd pnpm --dir apps/desktop check
fi

if [[ "${docs_changed}" == "1" && "${rust_changed}" == "0" ]]; then
  if [[ "${tier}" == "standard" || "${tier}" == "full" ]]; then
    run_cmd ./scripts/check-docs.sh
  fi
  exit 0
fi

if [[ "${rust_changed}" == "0" ]]; then
  exit 0
fi

run_cmd cargo fmt --all --check
run_cmd cargo check

if [[ "${tier}" == "full" ]]; then
  run_cmd cargo test
  run_cmd cargo clippy --all-targets -- -D warnings
  exit 0
fi

if [[ "${workspace_manifest_changed}" == "1" && "${#packages[@]}" -eq 0 ]]; then
  workspace_packages >"${packages_file}"
  packages=()
  while IFS= read -r package; do
    [[ -n "${package}" ]] || continue
    packages+=("${package}")
  done <"${packages_file}"
fi

for package in "${packages[@]}"; do
  run_cmd cargo test -p "${package}"
done

if [[ "${tier}" == "standard" ]]; then
  for package in "${packages[@]}"; do
    run_cmd cargo clippy -p "${package}" --all-targets -- -D warnings
  done
fi

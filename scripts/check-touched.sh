#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
# shellcheck source=scripts/check-touched-classifier.sh
source "${ROOT}/scripts/check-touched-classifier.sh"

usage() {
  cat <<'EOF'
Usage: scripts/check-touched.sh [--tier quick|standard|full] [--scope dirty|staged|base] [--base REF] [--dry-run]

Runs a risk-scaled local gate for the current change set.

Tiers:
  quick     policy/static checks, docs whitespace check, rustfmt, workspace cargo check, touched crate tests
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

run_isolated_test_cmd() {
  run_cmd python3 "${ROOT}/scripts/run-isolated-tests.py" -- "$@"
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

tmp_dir="$(mktemp -d)"
files_file="${tmp_dir}/changed-files"
packages_file="${tmp_dir}/packages"
: >"${packages_file}"
changed_files >"${files_file}"

if [[ ! -s "${files_file}" ]]; then
  echo "no changed files for scope=${scope}"
  exit 0
fi

# A staged gate must validate the exact index tree. Running Cargo in the dirty
# checkout would allow unstaged follow-up edits to make a broken staged tree
# appear healthy. Materialize the index into an isolated candidate directory;
# the candidate has no .git directory, so the checks cannot accidentally read
# the caller's worktree or index.
execution_root="${ROOT}"
candidate_tree=""
candidate_root=""
candidate_worktree=0
cleanup() {
  if [[ "${candidate_worktree}" == "1" ]]; then
    git -C "${ROOT}" worktree remove --force "${candidate_root}" >/dev/null 2>&1 || true
  fi
  rm -rf "${tmp_dir}"
}
trap cleanup EXIT
if [[ "${scope}" == "staged" && "${dry_run}" == "0" ]]; then
  candidate_tree="$(git -C "${ROOT}" write-tree)"
  candidate_root="${tmp_dir}/candidate"
  git -C "${ROOT}" worktree add --detach --no-checkout "${candidate_root}" HEAD >/dev/null
  candidate_worktree=1
  git -C "${candidate_root}" read-tree "${candidate_tree}"
  git -C "${candidate_root}" checkout-index --all --force
  execution_root="${candidate_root}"
  echo "candidate tree: ${candidate_tree}"
  echo "candidate parent: $(git -C "${ROOT}" rev-parse HEAD)"
fi

# This is the only check that intentionally inspects the caller's Git index.
run_cmd git -C "${ROOT}" diff --check --
if [[ "${candidate_worktree}" == "1" ]]; then
  # The caller may have supplied an alternate GIT_INDEX_FILE to describe the staged
  # candidate. Once the candidate worktree is materialized, inherited index state would
  # make Cargo tests and nested Git probes read the caller's temporary index instead of
  # the candidate worktree's own index.
  unset GIT_INDEX_FILE
fi
cd "${execution_root}"

rust_changed=0
docs_changed=0
high_risk_changed=0
desktop_changed=0

while IFS= read -r path; do
  case "${path}" in
    *.rs|Cargo.toml|*/Cargo.toml|Cargo.lock|rust-toolchain.toml)
      rust_changed=1
      ;;
  esac

  case "${path}" in
    README.md|README.*.md|docs/*|docs/**/*|dev/docs/*|dev/docs/**/*|dev/governance/*|dev/governance/**/*|*.md)
      docs_changed=1
      ;;
  esac

  if is_high_risk_path "${path}"; then
    high_risk_changed=1
  fi

  if is_desktop_path "${path}"; then
    desktop_changed=1
  fi
done <"${files_file}"

if [[ "${rust_changed}" == "1" ]]; then
  python3 "${ROOT}/scripts/check-touched-packages.py" \
    --changed-files "${files_file}" \
    --root "${execution_root}" >"${packages_file}"
fi
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

run_cmd scripts/test-check-touched-classifier.sh
run_cmd python3 scripts/test-check-touched-packages.py
run_cmd python3 scripts/test-check-no-prompt-phrase-routing.py
run_cmd python3 scripts/check-no-prompt-phrase-routing.py
run_cmd python3 "${ROOT}/scripts/check-isolated-test-entrypoints.py"
run_cmd python3 "${ROOT}/scripts/test-run-isolated-tests.py"
run_cmd python3 "${ROOT}/scripts/test-isolated-test-entrypoints.py"

if [[ "${desktop_changed}" == "1" ]]; then
  # Keep Corepack in the desktop package context so it honors the checked-in
  # `pnpm@10.30.3` declaration. A root-level invocation has no package
  # manifest and can select a different ambient pnpm version.
  run_isolated_test_cmd bash -c 'cd apps/desktop && pnpm check'
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
  run_isolated_test_cmd cargo test
  run_cmd cargo clippy --all-targets -- -D warnings
  exit 0
fi

for package in "${packages[@]}"; do
  run_isolated_test_cmd cargo test -p "${package}"
done

if [[ "${tier}" == "standard" ]]; then
  for package in "${packages[@]}"; do
    run_cmd cargo clippy -p "${package}" --all-targets -- -D warnings
  done
fi

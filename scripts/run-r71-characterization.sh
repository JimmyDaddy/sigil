#!/usr/bin/env bash
# RFC-0071 R71.0 characterization runner.
#
# Session 5ff39a6d-5225-4533-8c1f-b64c0c81abb7 causal edges are locked by deterministic
# assertions. The runner:
#   0. verifies the frozen conformance inventory (exact, non-zero case count);
#   1. injects an explicitly isolated fixture root (SIGIL_TEST_ISOLATED_ROOT) that is never the
#      active SessionScratch, and fails closed if it resolves into one;
#   2. runs the explicit-fixture isolation and poisoning fixtures first;
#   3. only then runs the original feedback-flow suite;
#   4. verifies each cargo invocation reports a non-zero, exact assertion count (zero tests is
#      never success) and uses direct exit-code checks so `cargo test ... | tail` cannot
#      produce a false green;
#   5. snapshots the active SessionScratch (when present) before and after and fails on any new
#      entry -- characterization itself must never poison the current execution environment.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

usage() {
  cat <<'EOF'
Usage: scripts/run-r71-characterization.sh
EOF
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    -h|--help) usage; exit 0 ;;
    *) echo "unknown argument: $1" >&2; usage >&2; exit 2 ;;
  esac
done

# 0. Frozen conformance inventory: exact, non-zero case list, all required.
python3 - "$ROOT/dev/governance/r71-conformance-inventory-v1.toml" <<'INVCHECK'
import sys, tomllib
from pathlib import Path
doc = tomllib.loads(Path(sys.argv[1]).read_text(encoding="utf-8"))
cases = doc.get("cases", [])
if not cases:
    print("FAIL: r71-conformance-inventory must not be empty", file=sys.stderr)
    sys.exit(1)
ids = [c.get("case_id") for c in cases]
if len(ids) != len(set(ids)):
    print("FAIL: duplicate case ids in conformance inventory", file=sys.stderr)
    sys.exit(1)
for c in cases:
    if not c.get("required", False):
        print(f"FAIL: characterization case {c.get('case_id')} must be required", file=sys.stderr)
        sys.exit(1)
print(f"PASS(inventory): {len(cases)} exact characterization cases verified")
INVCHECK

# Explicitly isolated fixture root. Never derive from TMPDIR/TMP/TEMP (restricted execution
# binds them to the session scratch) and never inside the workspace.
FIXTURE_BASE=$(mktemp -d -t sigil-r71-characterization-XXXXXX)
trap 'rm -rf -- "$FIXTURE_BASE"' EXIT
export SIGIL_TEST_ISOLATED_ROOT="$FIXTURE_BASE/fixtures"
mkdir -p "$SIGIL_TEST_ISOLATED_ROOT"

assert_fixture_root_not_scratch() {
  python3 - "$SIGIL_TEST_ISOLATED_ROOT" <<'PYEOF'
import os, sys
root = os.path.abspath(sys.argv[1])
# SIGIL_SCRATCH_DIR is the authoritative active SessionScratch identity (restricted execution
# always injects it). A bare user TMPDIR is a normal OS temp directory and is NOT scratch.
scratch = os.environ.get("SIGIL_SCRATCH_DIR")
if scratch:
    scratch = os.path.abspath(scratch)
    if root == scratch or root.startswith(scratch + os.sep):
        print("FAIL: injected fixture root resolves into active SessionScratch: " + root, file=sys.stderr)
        sys.exit(1)
print("PASS(isolation): fixture root isolated from active SessionScratch: " + root)
PYEOF
}

assert_fixture_root_not_scratch

# Snapshot any active SessionScratch so we can prove this run wrote nothing into it.
if [[ -n "${SIGIL_SCRATCH_DIR:-}" ]] && [[ -d "$SIGIL_SCRATCH_DIR" ]]; then
  SCRATCH_BEFORE=$(find "$SIGIL_SCRATCH_DIR" -mindepth 1 -maxdepth 3 -print | sort)
fi

# Helper: run cargo test with a suffix filter and require at least 1 test to run and 0 failures.
# Direct exit-code checks (never `| tail` swallowing a failure) keep false green impossible.
run_cargo_fixture() {
  local label="$1"
  shift
  local output
  output=$("$@" 2>&1) || {
    echo "FAIL($label): cargo command exited non-zero" >&2
    echo "$output" | tail -30 >&2
    exit 1
  }
  local summary
  summary=$(echo "$output" | grep -E 'test result: ok\.' | tail -1 || true)
  if [[ -z "$summary" ]]; then
    echo "FAIL($label): no 'test result: ok' summary in output" >&2
    echo "$output" | tail -30 >&2
    exit 1
  fi
  if echo "$summary" | grep -qE '([^0-9]|^)0 passed'; then
    echo "FAIL($label): zero tests ran (zero test is not success)" >&2
    echo "$output" | tail -30 >&2
    exit 1
  fi
  echo "PASS($label): $summary"
}

# 1. Explicit-fixture isolation and causal-edge characterization.
#    These run first so the original suite below is only executed once isolation is proven.
run_cargo_fixture "r71-causal-edges" cargo test -p sigil-tools-builtin --lib r71_ -- --format terse

# 2. Original feedback flow suite (only after fixture isolation passes).
run_cargo_fixture "feedback-flow" cargo test -p sigil-tui-app --lib feedback_ -- --format terse

# 3. Paths fail-closed: missing HOME/XDG must not create cwd .sigil-state/.sigil-cache (the former
#    pollution causal edge); the current resolver behavior is asserted by the existing test.
run_cargo_fixture "paths-fail-closed" cargo test -p sigil-runtime --lib paths -- --format terse

# 4. No-follow cleanup assertion is enforced inside the Rust fixtures (TestFixtureRoot RAII);
#    the injected fixture root is trap-cleaned below.

# 5. Characterization itself must not poison the current execution environment: an active
#    SessionScratch must not gain any new entry during this run.
if [[ -n "${SIGIL_SCRATCH_DIR:-}" ]] && [[ -d "$SIGIL_SCRATCH_DIR" ]]; then
  SCRATCH_AFTER=$(find "$SIGIL_SCRATCH_DIR" -mindepth 1 -maxdepth 3 -print | sort)
  if [[ "$SCRATCH_BEFORE" != "$SCRATCH_AFTER" ]]; then
    echo "FAIL: active SessionScratch changed during characterization run" >&2
    diff <(echo "$SCRATCH_BEFORE") <(echo "$SCRATCH_AFTER") >&2 || true
    exit 1
  fi
  echo "PASS(scratch-purity): active SessionScratch untouched by characterization"
else
  echo "PASS(scratch-purity): no active SessionScratch in this environment (fixture root isolated by construction)"
fi

echo "r71-characterization: all fixtures passed"

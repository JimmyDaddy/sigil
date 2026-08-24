#!/usr/bin/env bash
# RFC-0071 R71.5: typed fault campaign runner.
# Executes the deterministic recovery fixtures of each campaign family with a zero-test guard.
# Each family must report a non-zero exact test count and zero failures.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

usage() {
  cat <<'EOF'
Usage: scripts/run-r71-fault-campaign.sh
EOF
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    -h|--help) usage; exit 0 ;;
    *) echo "unknown argument: $1" >&2; usage >&2; exit 2 ;;
  esac
done

# Verify the frozen 200-case manifest before running any fixture and bind every manifest row to
# exactly one discovered Rust test. The family-level cargo filters below are only accepted after
# this bijection has been proven; a missing, skipped, duplicated, or extra fault test fails here.
python3 - "$ROOT/dev/governance/r71-conformance-inventory-v1.toml" <<'MANIFEST_CHECK'
import re, subprocess, sys, tomllib
from pathlib import Path
doc = tomllib.loads(Path(sys.argv[1]).read_text(encoding="utf-8"))
cases = doc.get("cases", [])
if len(cases) != 200:
    print(f"FAIL: manifest must contain exactly 200 required fault cases, found {len(cases)}", file=sys.stderr)
    sys.exit(1)
ids = [c.get("case_id") for c in cases]
if len(ids) != len(set(ids)):
    print("FAIL: duplicate case ids", file=sys.stderr)
    sys.exit(1)
for c in cases:
    if not c.get("required", False):
        print(f"FAIL: case {c.get('case_id')} must be required", file=sys.stderr)
        sys.exit(1)
    if c.get("expected_assertion_count", 0) != 1:
        print(f"FAIL: case {c.get('case_id')} expected assertion count must be 1", file=sys.stderr)
        sys.exit(1)
print(f"PASS(manifest): {len(cases)} exact fault cases frozen")

expected = {}
for case in cases:
    _, _, family, number = case["case_id"].split("-")
    prefix = f"r71_f_{family.lower()}_{number}"
    package = "sigil-sandbox" if family == "SPN" else "sigil-resource-authority"
    expected.setdefault(package, {})[prefix] = case["case_id"]

discovered = {}
for package, package_expected in expected.items():
    families = sorted({prefix.rsplit("_", 1)[0] for prefix in package_expected})
    for family_prefix in families:
        result = subprocess.run(
            ["cargo", "test", "-p", package, "--lib", family_prefix, "--", "--list"],
            check=False, capture_output=True, text=True,
        )
        if result.returncode != 0:
            print(f"FAIL: could not enumerate {package}/{family_prefix}\n{result.stderr}", file=sys.stderr)
            sys.exit(1)
        names = []
        for line in result.stdout.splitlines():
            match = re.search(r"(r71_f_[a-z]+_[0-9]+)[^:]*: test$", line.strip())
            if match:
                names.append(match.group(0).rsplit(":", 1)[0])
        expected_prefixes = {
            prefix for prefix in package_expected if prefix.startswith(family_prefix + "_")
        }
        matched = {}
        for prefix in expected_prefixes:
            hits = [name for name in names if name == prefix or name.startswith(prefix + "_")]
            if len(hits) != 1:
                print(
                    f"FAIL: {package}/{prefix} maps to {len(hits)} discovered tests: {hits}",
                    file=sys.stderr,
                )
                sys.exit(1)
            matched[prefix] = hits[0]
        unexpected = sorted(set(names) - set(matched.values()))
        if unexpected:
            print(
                f"FAIL: {package}/{family_prefix} has unregistered fault tests: {unexpected}",
                file=sys.stderr,
            )
            sys.exit(1)
        discovered.update({name: package_expected[prefix] for prefix, name in matched.items()})

if len(discovered) != len(cases):
    print(
        f"FAIL: manifest/test bijection observed {len(discovered)} tests for {len(cases)} cases",
        file=sys.stderr,
    )
    sys.exit(1)
print(f"PASS(manifest): {len(discovered)} manifest rows bind to unique Rust tests")
MANIFEST_CHECK

run_suite() {
  local label="$1"
  local expected="$2"
  shift 2
  local output
  output=$("$@" 2>&1) || {
    echo "FAIL(campaign/$label): cargo exited non-zero" >&2
    echo "$output" | tail -20 >&2
    exit 1
  }
  local summary
  summary=$(echo "$output" | grep -E 'test result: ok\.' | tail -1 || true)
  if [[ -z "$summary" ]]; then
    echo "FAIL(campaign/$label): zero tests ran or missing summary" >&2
    exit 1
  fi
  if [[ "$expected" != "-" ]]; then
    local passed failed ignored
    passed=$(echo "$summary" | sed -nE 's/.* ([0-9]+) passed; ([0-9]+) failed; ([0-9]+) ignored;.*/\1/p')
    failed=$(echo "$summary" | sed -nE 's/.* ([0-9]+) passed; ([0-9]+) failed; ([0-9]+) ignored;.*/\2/p')
    ignored=$(echo "$summary" | sed -nE 's/.* ([0-9]+) passed; ([0-9]+) failed; ([0-9]+) ignored;.*/\3/p')
    if [[ -z "$passed" || "$passed" -ne "$expected" || "$failed" -ne 0 || "$ignored" -ne 0 ]]; then
      echo "FAIL(campaign/$label): expected $expected passed, 0 failed, 0 ignored; got: $summary" >&2
      exit 1
    fi
  fi
  echo "PASS(campaign/$label): $summary"
}

run_suite recovery-gate - cargo test -p sigil-kernel --lib resource_recovery -- --format terse
run_suite fault-jrn 8 cargo test -p sigil-resource-authority --lib r71_f_jrn -- --format terse
run_suite fault-boot 10 cargo test -p sigil-resource-authority --lib r71_f_boot -- --format terse
run_suite fault-rec 10 cargo test -p sigil-resource-authority --lib r71_f_rec -- --format terse
run_suite fault-abr 8 cargo test -p sigil-resource-authority --lib r71_f_abr -- --format terse
run_suite fault-key 10 cargo test -p sigil-resource-authority --lib r71_f_key -- --format terse
run_suite fault-ret 8 cargo test -p sigil-resource-authority --lib r71_f_ret -- --format terse
run_suite fault-brg 12 cargo test -p sigil-resource-authority --lib r71_f_brg -- --format terse
run_suite fault-child 8 cargo test -p sigil-resource-authority --lib r71_f_child -- --format terse
run_suite fault-upd 6 cargo test -p sigil-resource-authority --lib r71_f_upd -- --format terse
run_suite fault-bor 18 cargo test -p sigil-resource-authority --lib r71_f_bor -- --format terse
run_suite fault-mut 22 cargo test -p sigil-resource-authority --lib r71_f_mut -- --format terse
run_suite fault-cat 10 cargo test -p sigil-resource-authority --lib r71_f_cat -- --format terse
run_suite fault-att 14 cargo test -p sigil-resource-authority --lib r71_f_att -- --format terse
run_suite fault-exp 24 cargo test -p sigil-resource-authority --lib r71_f_exp -- --format terse
run_suite fault-spn 32 cargo test -p sigil-sandbox --lib r71_f_spn -- --format terse
run_suite contract-goldens - bash scripts/check-r71-contract-goldens.sh
run_suite authority-fixtures - bash scripts/run-r71-authority-conformance.sh
echo "r71-fault-campaign: all fixtures passed"

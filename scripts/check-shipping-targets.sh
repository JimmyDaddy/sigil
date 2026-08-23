#!/usr/bin/env bash
# RFC-0071 R71.0: shipping targets checker (Cargo metadata + command AST scan).
# Baseline mode verifies the manifest is consistent with the current workspace and records
# migration blockers (release-only commands still inside the shipping binary) without failing.
# Enforce mode fails when any migration blocker remains or a release-only command is found in
# the shipping sigil binary.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
MANIFEST="$ROOT/dev/governance/shipping-targets-v1.toml"
MODE="baseline"

usage() {
  cat <<'EOF'
Usage: scripts/check-shipping-targets.sh [--mode baseline|enforce]
EOF
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --mode) MODE="${2:-}"; shift 2 ;;
    -h|--help) usage; exit 0 ;;
    *) echo "unknown argument: $1" >&2; usage >&2; exit 2 ;;
  esac
done

case "$MODE" in
  baseline|enforce) ;;
  *) echo "invalid mode: $MODE" >&2; usage >&2; exit 2 ;;
esac

if [[ ! -f "$MANIFEST" ]]; then
  echo "missing manifest: $MANIFEST" >&2
  exit 2
fi

python3 - "$ROOT" "$MANIFEST" "$MODE" <<'PYEOF'
import json, re, subprocess, sys, tomllib
from pathlib import Path

root, manifest_path, mode = sys.argv[1], sys.argv[2], sys.argv[3]
root = Path(root)
manifest = tomllib.loads(Path(manifest_path).read_text(encoding="utf-8"))

meta = subprocess.run(
    ["cargo", "metadata", "--no-deps", "--format-version", "1"],
    cwd=str(root), capture_output=True, text=True, check=False,
)
if meta.returncode != 0:
    print("cargo metadata failed: " + meta.stderr[-2000:], file=sys.stderr)
    sys.exit(2)
metadata = json.loads(meta.stdout)
packages = {p["name"]: p for p in metadata["packages"]}

targets = []
for package in packages.values():
    for t in package["targets"]:
        if any(k in t["kind"] for k in ("bin", "lib")):
            targets.append((package["name"], t["name"], t["kind"][0]))

def has_target(package: str, bin_name=None) -> bool:
    for pkg, name, _kind in targets:
        if pkg == package and (bin_name is None or name == bin_name):
            return True
    return False

errors = []
for cargo_target in manifest.get("shipping_roots", {}).get("cargo_targets", []):
    if not has_target(cargo_target["package"], cargo_target.get("bin")):
        errors.append(
            f"shipping root missing: {cargo_target['package']}:{cargo_target.get('bin', 'any')}"
        )

blockers = manifest.get("migration_blockers", [])
if mode == "enforce" and blockers:
    for blocker in blockers:
        errors.append(f"migration blocker remains: {blocker['kind']} ({blocker['package']})")

if mode == "enforce":
    main_rs = root / "crates" / "sigil" / "src" / "main.rs"
    if main_rs.exists():
        text = main_rs.read_text(encoding="utf-8")
        for blocker in blockers:
            for command in blocker.get("commands", []):
                pattern = re.compile(r'command\(name = \"' + re.escape(command) + r'\"')
                if pattern.search(text):
                    errors.append(f"release-only command still ships in sigil binary: {command}")

if errors:
    print("shipping-targets check failed:", file=sys.stderr)
    for error in errors:
        print(f"  {error}", file=sys.stderr)
    sys.exit(2)

blocker_count = len(blockers)
print(f"shipping-targets {mode}: manifest verified, {blocker_count} migration blocker(s) recorded")
PYEOF

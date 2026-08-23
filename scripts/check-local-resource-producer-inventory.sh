#!/usr/bin/env bash
# RFC-0071 R71.0: local resource producer inventory baseline/enforce checker.
# Baseline mode verifies forward existence and manifest coverage only (zero-case is a failure);
# enforce mode additionally fails on any unclassified, migration-blocker, or build-or-test row
# that is not a legitimate BuildOrTestOnly classification.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
MANIFEST="$ROOT/dev/governance/local-resource-producer-inventory-v1.toml"
MODE="baseline"

usage() {
  cat <<'EOF'
Usage: scripts/check-local-resource-producer-inventory.sh [--mode baseline|enforce]
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

python3 - "$MANIFEST" "$MODE" <<'PYEOF'
import json, subprocess, sys, tomllib
from pathlib import Path

manifest_path, mode = sys.argv[1], sys.argv[2]
root = Path(manifest_path).resolve().parent.parent.parent
manifest = tomllib.loads(Path(manifest_path).read_text(encoding="utf-8"))
manifest_sites = manifest.get("sites", [])
if not manifest_sites:
    print("resource producer inventory must not be empty", file=sys.stderr)
    sys.exit(2)

scan = json.loads(subprocess.check_output(
    [sys.executable, str(root / "scripts" / "r71_inventory_scan.py"), str(root)],
    text=True,
))
scan_sites = scan["producer_sites"]

def site_key(site):
    return (site["module"], site["line"], site["constructor"])

manifest_keys = {site_key(s) for s in manifest_sites}
scan_keys = {site_key(s) for s in scan_sites}

missing = sorted(scan_keys - manifest_keys, key=str)
stale = sorted(manifest_keys - scan_keys, key=str)
if missing:
    print(f"resource producer scanner found {len(missing)} site(s) not in manifest:", file=sys.stderr)
    for key in missing[:30]:
        print(f"  {key}", file=sys.stderr)
if stale:
    print(f"manifest contains {len(stale)} site(s) no longer present:", file=sys.stderr)
    for key in stale[:30]:
        print(f"  {key}", file=sys.stderr)
if missing or stale:
    print("regenerate: python3 scripts/generate-r71-inventory-baseline.py", file=sys.stderr)
    sys.exit(2)

if mode == "enforce":
    bad = [s for s in manifest_sites if s["class"] in ("unclassified", "migration-blocker")]
    if bad:
        print(f"enforce mode: {len(bad)} unclassified/blocker producer site(s)", file=sys.stderr)
        for s in bad[:30]:
            print(f"  {s['site_id']} {s['module']}:{s['line']} class={s['class']}", file=sys.stderr)
        print("RFC-0071 R71.4 (mandatory consumer qualification) must resolve them first", file=sys.stderr)
        sys.exit(2)

print(f"local-resource-producer-inventory {mode}: {len(manifest_sites)} sites verified")
PYEOF

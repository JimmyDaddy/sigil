#!/usr/bin/env bash
# RFC-0070 R70.6: host ownership, package boundary and transitional-edge gate.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

python3 scripts/check-r70-host-ownership.py
python3 -m unittest scripts/test-r70-host-ownership.py
python3 scripts/check-r70-package-topology.py
python3 scripts/check-r70-migration-manifest.py
cargo check --locked -p sigil-tui-app --lib
cargo check --locked -p sigil-tui --lib
cargo check --locked -p sigil-tui-host --lib
cargo check --locked -p sigil --bin sigil
cargo check --locked -p sigil-http --lib
cargo test --locked -p sigil-tui-host --lib --quiet

echo "r70.6 host ownership gate: application package, public framework, host wiring and migration edges passed"

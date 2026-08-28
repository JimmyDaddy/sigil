#!/usr/bin/env bash
# RFC-0070 R70.8: public compatibility retirement and application-facade gate.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

python3 scripts/check-r70-legacy-retirement.py
python3 -m unittest scripts/test-r70-legacy-retirement.py
python3 scripts/check-r70-package-topology.py
python3 scripts/check-r70-migration-manifest.py
cargo fmt --all --check
cargo check --locked -p sigil-application --lib
cargo check --locked -p sigil-runtime --lib
cargo check --locked -p sigil-tui-host --lib
cargo test --locked -p sigil-application --lib resource_recovery --quiet
cargo test --locked -p sigil-runtime --lib r71_global_cutover --quiet
cargo test --locked -p sigil-tui-host --test r71_shipping_e2e --quiet
git diff --check

echo "r70.8 legacy retirement gate: public compatibility paths and runtime recovery facade passed"

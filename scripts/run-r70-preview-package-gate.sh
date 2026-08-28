#!/usr/bin/env bash
# RFC-0070 R70.7: public preview package qualification.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

FEATURES=(crossterm serde syntax image tracing test-util)

python3 scripts/check-r70-package-topology.py
python3 scripts/check-r70-preview-metadata.py
cargo fmt --all --check
bash scripts/run-r70-framework-qualification.sh

for package in sigil-tui-core sigil-tui-ratatui sigil-tui; do
  cargo package --locked -p "$package" --allow-dirty --list >/dev/null
done

cargo package --locked -p sigil-tui-core --allow-dirty
cargo package --locked -p sigil-tui-ratatui --allow-dirty \
  --config 'patch.crates-io.sigil-tui-core.path="crates/sigil-tui-core"'
cargo package --locked -p sigil-tui --allow-dirty \
  --config 'patch.crates-io.sigil-tui-core.path="crates/sigil-tui-core"' \
  --config 'patch.crates-io.sigil-tui-ratatui.path="crates/sigil-tui-ratatui"'

cargo test --locked --manifest-path target/package/sigil-tui-core-0.1.0/Cargo.toml --all-targets --quiet
cargo test --locked --manifest-path target/package/sigil-tui-ratatui-0.1.0/Cargo.toml --all-targets --quiet \
  --config "patch.crates-io.sigil-tui-core.path=\"$ROOT/crates/sigil-tui-core\""
cargo test --locked --manifest-path target/package/sigil-tui-0.1.0/Cargo.toml --all-targets --quiet \
  --config "patch.crates-io.sigil-tui-core.path=\"$ROOT/crates/sigil-tui-core\"" \
  --config "patch.crates-io.sigil-tui-ratatui.path=\"$ROOT/crates/sigil-tui-ratatui\""

cargo doc --locked -p sigil-tui --no-deps
for ((mask = 0; mask < (1 << ${#FEATURES[@]}); mask++)); do
  selected=()
  for ((index = 0; index < ${#FEATURES[@]}; index++)); do
    if ((mask & (1 << index))); then
      selected+=("${FEATURES[index]}")
    fi
  done
  if ((${#selected[@]} == 0)); then
    cargo check --locked -p sigil-tui --no-default-features
  else
    cargo check --locked -p sigil-tui --no-default-features --features "$(IFS=,; echo "${selected[*]}")"
  fi
done

cargo publish --locked --dry-run -p sigil-tui-core --allow-dirty
cargo publish --locked --dry-run -p sigil-tui-ratatui --allow-dirty \
  --config 'patch.crates-io.sigil-tui-core.path="crates/sigil-tui-core"'
cargo publish --locked --dry-run -p sigil-tui --allow-dirty \
  --config 'patch.crates-io.sigil-tui-core.path="crates/sigil-tui-core"' \
  --config 'patch.crates-io.sigil-tui-ratatui.path="crates/sigil-tui-ratatui"'

echo "r70.7 preview package gate: package lists, verified tarballs, empty-package tests, docs and feature profiles passed"

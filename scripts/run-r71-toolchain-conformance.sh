#!/usr/bin/env bash
# RFC-0071 R71.8: real warm/offline Rust, Node and Git toolchain qualification.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

usage() {
  cat <<'EOF'
Usage: scripts/run-r71-toolchain-conformance.sh --candidate-sha SHA --base-sha SHA
EOF
}

candidate_sha=""
base_sha=""
while [[ $# -gt 0 ]]; do
  case "$1" in
    --candidate-sha) candidate_sha="${2:-}"; shift 2 ;;
    --base-sha) base_sha="${2:-}"; shift 2 ;;
    -h|--help) usage; exit 0 ;;
    *) echo "unknown argument: $1" >&2; usage >&2; exit 2 ;;
  esac
done

[[ -n "$candidate_sha" && -n "$base_sha" ]] || { usage >&2; exit 2; }

qualification_json="$(python3 - "$ROOT" "$candidate_sha" "$base_sha" <<'PY'
import json
import sys
from pathlib import Path

sys.path.insert(0, str(Path(sys.argv[1]) / "scripts"))
from r71_qualification_common import load_conformance_manifest, load_platform_manifest, manifest_digests, validate_git_identity

root = Path(sys.argv[1])
identity = validate_git_identity(root, sys.argv[2], sys.argv[3])
load_conformance_manifest()
load_platform_manifest()
identity.update(manifest_digests())
print(json.dumps(identity, sort_keys=True))
PY
)"

echo "R71 toolchain qualification identity: $qualification_json"

for command in rustc cargo node git; do
  command -v "$command" >/dev/null || {
    echo "required toolchain executable is missing: $command" >&2
    exit 1
  }
done

# Keep the already-installed Rust toolchain store warm while isolating user configuration. Hiding
# RUSTUP_HOME would make rustup attempt a network install under the fresh HOME, which is neither a
# warm profile nor an offline proof.
rustup_home="${RUSTUP_HOME:-}"
if command -v rustup >/dev/null; then
  rustup_home="$(rustup show home 2>/dev/null || true)"
fi

fixture_home="$(mktemp -d -t sigil-r71-toolchain-XXXXXX)"
trap 'rm -rf -- "$fixture_home"' EXIT
mkdir -p "$fixture_home/.cargo" "$fixture_home/.npm"

export HOME="$fixture_home"
if [[ -n "$rustup_home" ]]; then
  export RUSTUP_HOME="$rustup_home"
fi
export CARGO_NET_OFFLINE=true
export CARGO_TERM_COLOR=never
export GIT_TERMINAL_PROMPT=0
export GIT_CONFIG_NOSYSTEM=1
export npm_config_offline=true

echo "Rust: $(rustc --version)"
echo "Cargo: $(cargo --version)"
echo "Node: $(node --version)"
echo "Git: $(git --version)"

# Warm toolchain profile: existing binaries and the checked-in workspace metadata are usable
# without resolving a new toolchain or contacting a registry.
cargo metadata --locked --offline --no-deps --format-version 1 >/dev/null
node --eval 'if (process.version.length < 2) process.exit(1)'
git -C "$ROOT" status --porcelain=v1 >/dev/null

# The sandbox binding fixture proves the typed CAS agreement. The command above proves the
# actual Rust/Node/Git executables remain usable with a fresh HOME and offline policy.
cargo test --locked -p sigil-sandbox --lib toolchain -- --format terse
echo "r71-toolchain-conformance: Rust/Node/Git warm offline profile passed"

#!/usr/bin/env bash
# RFC-0070 R70.5: public package identity and dependency allowlist gate.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
exec python3 "$ROOT/scripts/check-r70-package-topology.py"

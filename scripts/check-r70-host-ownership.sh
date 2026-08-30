#!/usr/bin/env bash
# RFC-0070 R70.6: application/host ownership and transitional-edge gate.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
exec python3 "$ROOT/scripts/check-r70-host-ownership.py"

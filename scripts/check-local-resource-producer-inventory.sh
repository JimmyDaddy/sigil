#!/usr/bin/env bash
# RFC-0071 R71.0: resource-producer-specific entrypoint for the shared inventory checker.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
exec python3 "$ROOT/scripts/check-r71-inventories.py" "$@" --kind producer

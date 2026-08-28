#!/usr/bin/env bash
# RFC-0070 R70.0: post-R71 baseline, frozen contract and command/event manifest gate.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
python3 "$ROOT/scripts/check-r70-migration-manifest.py"

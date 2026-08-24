#!/usr/bin/env bash
# RFC-0071 R71.7: fail-closed Cargo graph and production-source negative gate.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
exec python3 "${ROOT}/scripts/r71_negative_dependency_scan.py"

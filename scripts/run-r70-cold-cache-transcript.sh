#!/usr/bin/env bash
set -euo pipefail

# RFC-0070 R70.4 opt-in cold-cache qualification. The ignored test creates its fixture below
# the test runner's TempDir and exercises the same streaming page function used by runtime.
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"
cargo test --locked -p sigil-runtime --lib cold_cache_transcript_page_100k_keeps_the_resident_page_bounded -- --ignored --nocapture

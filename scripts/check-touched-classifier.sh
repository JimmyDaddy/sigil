#!/usr/bin/env bash

is_high_risk_path() {
  case "$1" in
    crates/sigil-kernel/src/agent.rs|\
    crates/sigil-kernel/src/event.rs|\
    crates/sigil-kernel/src/session.rs|\
    crates/sigil-kernel/src/mutation.rs|\
    crates/sigil-kernel/src/verification.rs|\
    crates/sigil-kernel/src/permission.rs|\
    crates/sigil-kernel/src/task_orchestrator.rs|\
    crates/sigil-kernel/src/tool.rs|\
    crates/sigil-tui/src/runner/*|\
    crates/sigil-tui/src/app/worker_bridge.rs|\
    crates/sigil-mcp/src/*|\
    crates/sigil-tools-builtin/src/*)
      return 0
      ;;
    *)
      return 1
      ;;
  esac
}

is_desktop_path() {
  case "$1" in
    apps/desktop/*|apps/desktop/**/*|crates/sigil-http/src/openapi.rs|scripts/generate-desktop-contract.sh)
      return 0
      ;;
    *)
      return 1
      ;;
  esac
}

#!/usr/bin/env python3
"""Enforce R70.6 host ownership and transitional-edge removal."""

from __future__ import annotations

import json
import sys
import tomllib
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]


def main() -> int:
    errors: list[str] = []

    tui_lib = (ROOT / "crates/sigil-tui/src/lib.rs").read_text(encoding="utf-8")
    if "pub mod runner;" in tui_lib:
        errors.append("sigil-tui-host exposes runner as a public module")
    if "pub(crate) mod runner;" not in tui_lib:
        errors.append("sigil-tui-host must keep runner behind a crate-private module boundary")

    production_edges = {
        "crates/sigil-tui/src": "sigil_runtime::r71_",
        "crates/sigil-http/src/production_driver.rs": "sigil_runtime::r71_",
        "crates/sigil/src/main.rs": "sigil_runtime::r71_",
    }
    for relative, marker in production_edges.items():
        path = ROOT / relative
        paths = path.rglob("*.rs") if path.is_dir() else (path,)
        for source in paths:
            if "tests" in source.relative_to(ROOT).parts:
                continue
            if marker in source.read_text(encoding="utf-8"):
                errors.append(f"transitional runtime edge remains in {source.relative_to(ROOT)}")

    app_manifest = (ROOT / "crates/sigil-tui-app/Cargo.toml").read_text(encoding="utf-8")
    for forbidden in ("sigil-runtime", "sigil-kernel", "sigil-tools-builtin", "sigil-updater"):
        if forbidden in app_manifest:
            errors.append(f"sigil-tui-app declares forbidden host dependency {forbidden}")

    host_manifest = tomllib.loads(
        (ROOT / "crates/sigil-tui/Cargo.toml").read_text(encoding="utf-8")
    )
    host_dependencies = host_manifest.get("dependencies", {})
    if "sigil-tui-app" not in host_dependencies:
        errors.append("production TUI host must depend on sigil-tui-app")

    application_bridge = (ROOT / "crates/sigil-tui/src/application_bridge.rs").read_text(
        encoding="utf-8"
    )
    if "sigil_tui_app::TuiApplicationAdapter" not in application_bridge:
        errors.append("production TUI application bridge must use TuiApplicationAdapter")
    if "TuiApplicationAdapter::from_port" not in application_bridge:
        errors.append("production TUI application bridge must create its adapter from the host port")
    if "ApplicationClient::new" in application_bridge:
        errors.append("production TUI application bridge must not construct a parallel ApplicationClient")

    application_sources = ROOT / "crates/sigil-application/src"
    for source in application_sources.rglob("*.rs"):
        text = source.read_text(encoding="utf-8")
        for forbidden in ("sigil_resource_authority", "sigil_sandbox", "std::process", "std::fs"):
            if forbidden in text:
                errors.append(
                    f"application contract contains physical/host marker {forbidden}: "
                    f"{source.relative_to(ROOT)}"
                )

    managed_adapters = (ROOT / "crates/sigil-runtime/src/managed_resource_adapters.rs").read_text(
        encoding="utf-8"
    )
    if "ProductUpdaterState" not in managed_adapters:
        errors.append("runtime managed adapters no longer show the independent ProductUpdaterState owner")

    if errors:
        for error in errors:
            print(f"r70 host ownership: {error}", file=sys.stderr)
        return 2

    print(json.dumps({"status": "pass", "transitional_edges": 0, "runner": "crate-private"}))
    return 0


if __name__ == "__main__":
    sys.exit(main())

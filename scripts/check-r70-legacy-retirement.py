#!/usr/bin/env python3
"""Enforce the R70.8 public compatibility boundary.

R70.8 retires the old public package paths after the preview release.  The Sigil product host
may still contain implementation-only renderer and worker code until the release-cycle proof is
recorded, but none of those types may leak through a publishable package or a runtime recovery
facade.  This checker makes that distinction explicit instead of treating a private module as a
completed public migration.
"""

from __future__ import annotations

import json
import sys
import tomllib
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
MANIFEST = ROOT / "dev/governance/r70-command-event-migration-v1.toml"
PUBLIC_ROOTS = (
    ROOT / "crates/sigil-tui-core/src",
    ROOT / "crates/sigil-tui-ratatui/src",
    ROOT / "crates/sigil-tui-framework/src",
    ROOT / "crates/sigil-tui-app/src",
)
PUBLIC_LEGACY_MARKERS = (
    "AppState",
    "LayoutSnapshot",
    "WorkerProtocol",
    "WorkerCommand",
    "WorkerMessage",
    "WorkerApprovalCommand",
    "ThemePalette",
    "InputKeyCode",
    "UiCommand",
)
REQUIRED_RETIREMENTS = {
    "sigil-tui-host::runner",
    "sigil-tui-host::ui::LayoutSnapshot",
    "sigil-tui-host::app::AppState",
    "sigil-tui-host::theme::ThemePalette",
    "sigil-runtime::resource_recovery_surface",
}


def _package_is_private() -> bool:
    manifest = tomllib.loads(
        (ROOT / "crates/sigil-tui/Cargo.toml").read_text(encoding="utf-8")
    )
    return manifest.get("package", {}).get("name") == "sigil-tui-host" and manifest.get(
        "package", {}
    ).get("publish") is False


def main() -> int:
    errors: list[str] = []
    if not _package_is_private():
        errors.append("sigil-tui-host must remain an explicitly non-publishable package")

    host_lib = (ROOT / "crates/sigil-tui/src/lib.rs").read_text(encoding="utf-8")
    for module in ("app", "launcher", "mouse", "ui", "runner"):
        if f"pub mod {module};" in host_lib:
            errors.append(f"sigil-tui-host exposes {module} as a public module")
        if f"pub(crate) mod {module};" not in host_lib:
            errors.append(f"sigil-tui-host must keep {module} behind a crate-private boundary")

    runtime_facade = ROOT / "crates/sigil-runtime/src/resource_recovery_surface.rs"
    runtime_lib = (ROOT / "crates/sigil-runtime/src/lib.rs").read_text(encoding="utf-8")
    if runtime_facade.exists() or "pub mod resource_recovery_surface;" in runtime_lib:
        errors.append("R71 runtime resource recovery facade is still present")
    application_recovery = ROOT / "crates/sigil-application/src/resource_recovery.rs"
    if not application_recovery.is_file():
        errors.append("application recovery facade replacement is missing")

    for root in PUBLIC_ROOTS:
        for source in root.rglob("*.rs"):
            text = source.read_text(encoding="utf-8")
            for marker in PUBLIC_LEGACY_MARKERS:
                if marker in text:
                    errors.append(
                        f"public package contains retired compatibility marker {marker}: "
                        f"{source.relative_to(ROOT)}"
                    )

    try:
        document = tomllib.loads(MANIFEST.read_text(encoding="utf-8"))
    except (OSError, tomllib.TOMLDecodeError) as error:
        errors.append(f"cannot load migration manifest: {error}")
        document = {}
    retirements = document.get("legacy_retirement", [])
    observed = {
        row.get("symbol")
        for row in retirements
        if isinstance(row, dict) and row.get("status") == "verified-retire"
    }
    missing = sorted(REQUIRED_RETIREMENTS - observed)
    if missing:
        errors.append(f"migration manifest lacks verified retirements: {', '.join(missing)}")

    if errors:
        for error in errors:
            print(f"r70 legacy retirement: {error}", file=sys.stderr)
        return 2
    print(
        json.dumps(
            {
                "status": "pass",
                "public_legacy_markers": 0,
                "runtime_recovery_facade": "retired",
                "verified_retirements": len(REQUIRED_RETIREMENTS),
            },
            sort_keys=True,
        )
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())

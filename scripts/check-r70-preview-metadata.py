#!/usr/bin/env python3
"""Check the public preview package metadata and release order."""

from __future__ import annotations

import json
import sys
import tomllib
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
PACKAGES = {
    "sigil-tui-core": "crates/sigil-tui-core/Cargo.toml",
    "sigil-tui-ratatui": "crates/sigil-tui-ratatui/Cargo.toml",
    "sigil-tui": "crates/sigil-tui-framework/Cargo.toml",
}


def main() -> int:
    errors: list[str] = []
    for expected_name, relative in PACKAGES.items():
        path = ROOT / relative
        try:
            document = tomllib.loads(path.read_text(encoding="utf-8"))
            package = document["package"]
        except (OSError, tomllib.TOMLDecodeError, KeyError) as error:
            errors.append(f"{relative}: cannot load package metadata: {error}")
            continue
        for key, expected in {
            "name": expected_name,
            "version": "0.1.0",
            "rust-version": "1.85",
            "repository": "https://github.com/JimmyDaddy/sigil",
            "changelog": "CHANGELOG.md",
        }.items():
            if package.get(key) != expected:
                errors.append(f"{relative}: {key} must be {expected!r}")
        readme = ROOT / path.parent / str(package.get("readme", ""))
        if not readme.is_file():
            errors.append(f"{relative}: readme is missing: {readme}")
        documentation = package.get("documentation")
        if documentation != f"https://docs.rs/{expected_name}":
            errors.append(f"{relative}: documentation must point to docs.rs/{expected_name}")

    workflow = ROOT / ".github/workflows/sigil-tui-preview.yml"
    workflow_text = workflow.read_text(encoding="utf-8") if workflow.is_file() else ""
    if not workflow_text:
        errors.append("preview release workflow is missing")
    for package in PACKAGES:
        if f"-p {package}" not in workflow_text:
            errors.append(f"preview release workflow is missing package {package}")

    if errors:
        for error in errors:
            print(f"r70 preview metadata: {error}", file=sys.stderr)
        return 2
    print(json.dumps({"status": "pass", "preview_version": "0.1.0", "publish_order": list(PACKAGES)}))
    return 0


if __name__ == "__main__":
    sys.exit(main())

#!/usr/bin/env python3
"""Enforce the public R70 TUI package identity and dependency allowlist."""

from __future__ import annotations

import json
import subprocess
import sys
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
ALLOWED = {
    "sigil-tui-core": set(),
    "sigil-tui-ratatui": {"sigil-tui-core"},
    "sigil-tui": {"sigil-tui-core", "sigil-tui-ratatui"},
}
PUBLIC_ROOTS = {
    "sigil-tui-core": ROOT / "crates/sigil-tui-core/src",
    "sigil-tui-ratatui": ROOT / "crates/sigil-tui-ratatui/src",
    "sigil-tui": ROOT / "crates/sigil-tui-framework/src",
}
FORBIDDEN_SOURCE_MARKERS = (
    "sigil_kernel",
    "sigil_runtime",
    "sigil_application",
    "std::fs",
    "std::process",
    "tokio::",
)


def metadata() -> dict[str, object]:
    result = subprocess.run(
        ["cargo", "metadata", "--format-version", "1", "--all-features", "--no-deps"],
        cwd=ROOT,
        capture_output=True,
        text=True,
        check=False,
    )
    if result.returncode != 0:
        raise RuntimeError(result.stderr.strip() or "cargo metadata failed")
    return json.loads(result.stdout)


def main() -> int:
    try:
        packages = {
            package["name"]: package
            for package in metadata().get("packages", [])
            if package["name"] in ALLOWED
        }
    except (RuntimeError, json.JSONDecodeError, TypeError, KeyError) as error:
        print(f"r70 package topology cannot be loaded: {error}", file=sys.stderr)
        return 2

    errors: list[str] = []
    for package_name, allowed in ALLOWED.items():
        package = packages.get(package_name)
        if package is None:
            errors.append(f"missing package: {package_name}")
            continue
        dependencies = {
            dependency["name"]
            for dependency in package.get("dependencies", [])
            if dependency.get("kind") in (None, "normal", "build", "dev")
        }
        sigil_dependencies = {name for name in dependencies if name.startswith("sigil-")}
        unexpected = sorted(sigil_dependencies - allowed)
        if unexpected:
            errors.append(
                f"{package_name} has disallowed sigil dependencies: {', '.join(unexpected)}"
            )

        source_root = PUBLIC_ROOTS[package_name]
        for path in source_root.rglob("*.rs"):
            text = path.read_text(encoding="utf-8")
            for marker in FORBIDDEN_SOURCE_MARKERS:
                if marker in text:
                    errors.append(f"{package_name} public source contains {marker}: {path}")

    if errors:
        for error in errors:
            print(f"r70 package topology: {error}", file=sys.stderr)
        return 2

    print(
        json.dumps(
            {
                "status": "pass",
                "packages": {
                    name: sorted(ALLOWED[name]) for name in sorted(ALLOWED)
                },
            },
            sort_keys=True,
        )
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())


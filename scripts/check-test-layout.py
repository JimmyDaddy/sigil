#!/usr/bin/env python3
"""Reject inline Rust test modules in production source files."""

from __future__ import annotations

import re
import sys
from pathlib import Path


INLINE_TEST_MODULE = re.compile(r"^[ \t]*mod[ \t]+tests[ \t]*\{", re.MULTILINE)


def is_test_source(path: Path) -> bool:
    """Return whether a Rust source path belongs to a physical test surface."""
    return "tests" in path.parts or path.name.endswith(("_tests.rs", "_test_support.rs"))


def inline_test_modules(root: Path) -> list[tuple[Path, int]]:
    """Return production Rust files and line numbers containing inline tests."""
    violations: list[tuple[Path, int]] = []
    crates = root / "crates"
    if not crates.is_dir():
        raise ValueError(f"crates directory is missing: {crates}")

    for path in sorted(crates.glob("*/src/**/*.rs")):
        relative = path.relative_to(root)
        if is_test_source(relative):
            continue
        text = path.read_text(encoding="utf-8")
        for match in INLINE_TEST_MODULE.finditer(text):
            line = text.count("\n", 0, match.start()) + 1
            violations.append((relative, line))
    return violations


def main() -> int:
    root = Path(__file__).resolve().parent.parent
    try:
        violations = inline_test_modules(root)
    except (OSError, UnicodeError, ValueError) as error:
        print(f"test layout check failed: {error}", file=sys.stderr)
        return 1

    if violations:
        print(
            "test layout check failed: move inline tests to a sibling tests directory",
            file=sys.stderr,
        )
        for path, line in violations:
            print(f"  {path}:{line}: inline mod tests", file=sys.stderr)
        return 1

    print("test layout check passed: no inline production test modules")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

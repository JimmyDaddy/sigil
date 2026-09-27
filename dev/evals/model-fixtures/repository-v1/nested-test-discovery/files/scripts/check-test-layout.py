#!/usr/bin/env python3
"""Reject inline Rust test modules in production source files."""

from __future__ import annotations

import re
import os
import subprocess
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

    for path in sorted(crates.glob("*/src/*.rs")):
        relative = path.relative_to(root)
        if is_test_source(relative):
            continue
        text = path.read_text(encoding="utf-8")
        for match in INLINE_TEST_MODULE.finditer(text):
            line = text.count("\n", 0, match.start()) + 1
            violations.append((relative, line))
    return violations


def staged_inline_test_modules(root: Path) -> list[tuple[Path, int]]:
    """Inspect the staged blob instead of the worktree for pre-commit checks."""
    result = subprocess.run(
        ["git", "diff", "--cached", "--name-only", "-z", "--diff-filter=ACMRTUXB"],
        cwd=root,
        check=True,
        capture_output=True,
    )
    violations: list[tuple[Path, int]] = []
    for raw_path in result.stdout.split(b"\0"):
        if not raw_path:
            continue
        relative = Path(os.fsdecode(raw_path))
        if relative.suffix != ".rs" or "crates" not in relative.parts:
            continue
        try:
            source = subprocess.run(
                ["git", "show", f":{relative.as_posix()}"],
                cwd=root,
                check=True,
                capture_output=True,
            ).stdout.decode("utf-8")
        except (subprocess.CalledProcessError, UnicodeDecodeError):
            continue
        if is_test_source(relative):
            continue
        for match in INLINE_TEST_MODULE.finditer(source):
            line = source.count("\n", 0, match.start()) + 1
            violations.append((relative, line))
    return violations


def main() -> int:
    root = Path(__file__).resolve().parent.parent
    try:
        staged = len(sys.argv) == 2 and sys.argv[1] == "--staged"
        if len(sys.argv) > 1 and not staged:
            print("usage: check-test-layout.py [--staged]", file=sys.stderr)
            return 2
        violations = staged_inline_test_modules(root) if staged else inline_test_modules(root)
    except (OSError, UnicodeError, ValueError, subprocess.CalledProcessError) as error:
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

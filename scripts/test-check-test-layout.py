#!/usr/bin/env python3
"""Unit tests for the Rust test-layout gate."""

from __future__ import annotations

import importlib.util
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path


SCRIPT_PATH = Path(__file__).with_name("check-test-layout.py")
SPEC = importlib.util.spec_from_file_location("check_test_layout", SCRIPT_PATH)
assert SPEC is not None and SPEC.loader is not None
check_layout = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = check_layout
SPEC.loader.exec_module(check_layout)


class TestLayoutTests(unittest.TestCase):
    def test_inline_production_test_module_is_reported(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source = root / "crates/example/src/lib.rs"
            source.parent.mkdir(parents=True)
            source.write_text("fn live() {}\n\nmod tests {\n}\n", encoding="utf-8")

            self.assertEqual(
                check_layout.inline_test_modules(root),
                [(Path("crates/example/src/lib.rs"), 3)],
            )

    def test_physical_test_module_declaration_and_test_file_pass(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source = root / "crates/example/src/lib.rs"
            tests = root / "crates/example/src/tests/lib_tests.rs"
            tests.parent.mkdir(parents=True)
            source.write_text(
                '#[cfg(test)]\n#[path = "tests/lib_tests.rs"]\nmod tests;\n',
                encoding="utf-8",
            )
            tests.write_text("mod tests {\n}\n", encoding="utf-8")

            self.assertEqual(check_layout.inline_test_modules(root), [])

    def test_missing_crates_directory_is_rejected(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            with self.assertRaisesRegex(ValueError, "crates directory is missing"):
                check_layout.inline_test_modules(Path(directory))

    def test_staged_mode_reads_index_blob_and_ignores_unstaged_fix(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source = root / "crates/example/src/lib.rs"
            source.parent.mkdir(parents=True)
            source.write_text("fn live() {}\n", encoding="utf-8")
            subprocess.run(["git", "init", "-q"], cwd=root, check=True)
            subprocess.run(["git", "config", "user.email", "test@example.com"], cwd=root, check=True)
            subprocess.run(["git", "config", "user.name", "Test"], cwd=root, check=True)
            subprocess.run(["git", "add", "."], cwd=root, check=True)
            subprocess.run(["git", "commit", "-qm", "base"], cwd=root, check=True)
            source.write_text("fn live() {}\n\nmod tests {\n}\n", encoding="utf-8")
            subprocess.run(["git", "add", str(source)], cwd=root, check=True)
            source.write_text("fn live() {}\n", encoding="utf-8")

            self.assertEqual(
                check_layout.staged_inline_test_modules(root),
                [(Path("crates/example/src/lib.rs"), 3)],
            )


if __name__ == "__main__":
    unittest.main()

#!/usr/bin/env python3
"""Regression tests for the R70.6 host ownership policy."""

from __future__ import annotations

import importlib.util
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
MODULE_PATH = ROOT / "scripts/check-r70-host-ownership.py"
SPEC = importlib.util.spec_from_file_location("r70_host_ownership", MODULE_PATH)
if SPEC is None or SPEC.loader is None:
    raise RuntimeError(f"cannot load {MODULE_PATH}")
MODULE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(MODULE)


class R70HostOwnershipTests(unittest.TestCase):
    def test_current_host_boundary_is_closed(self) -> None:
        self.assertEqual(MODULE.main(), 0)

    def test_runner_is_not_publicly_exported(self) -> None:
        source = (ROOT / "crates/sigil-tui/src/lib.rs").read_text(encoding="utf-8")
        self.assertIn("pub(crate) mod runner;", source)
        self.assertNotIn("pub mod runner;", source)


if __name__ == "__main__":
    unittest.main()

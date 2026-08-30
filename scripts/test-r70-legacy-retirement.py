#!/usr/bin/env python3
"""Regression tests for the R70.8 compatibility boundary."""

from __future__ import annotations

import importlib.util
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
MODULE_PATH = ROOT / "scripts/check-r70-legacy-retirement.py"
SPEC = importlib.util.spec_from_file_location("r70_legacy_retirement", MODULE_PATH)
if SPEC is None or SPEC.loader is None:
    raise RuntimeError(f"cannot load {MODULE_PATH}")
MODULE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(MODULE)


class R70LegacyRetirementTests(unittest.TestCase):
    def test_public_compatibility_boundary_is_closed(self) -> None:
        self.assertEqual(MODULE.main(), 0)

    def test_runtime_facade_replacement_is_application_owned(self) -> None:
        self.assertTrue(
            (ROOT / "crates/sigil-application/src/resource_recovery.rs").is_file()
        )
        self.assertFalse(
            (ROOT / "crates/sigil-runtime/src/resource_recovery_surface.rs").exists()
        )


if __name__ == "__main__":
    unittest.main()

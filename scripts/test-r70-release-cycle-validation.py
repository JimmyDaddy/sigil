#!/usr/bin/env python3
"""Regression tests for the fail-closed R70.8 external evidence gate."""

from __future__ import annotations

import importlib.util
import unittest
from pathlib import Path


MODULE_PATH = Path(__file__).resolve().with_name("check-r70-release-cycle-validation.py")
SPEC = importlib.util.spec_from_file_location("r70_release_cycle_validation", MODULE_PATH)
if SPEC is None or SPEC.loader is None:
    raise RuntimeError(f"cannot load {MODULE_PATH}")
MODULE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(MODULE)


class R70ReleaseCycleValidationTests(unittest.TestCase):
    def test_missing_evidence_fails_closed(self) -> None:
        self.assertTrue(MODULE.validate_evidence({}))

    def test_complete_evidence_passes(self) -> None:
        evidence = {
            "schema_version": "r70-release-cycle-validation-v1",
            "preview_version": "0.1.0",
            "release_tag": "sigil-tui-v0.1.0",
            "published_at": "2026-08-28T00:00:00Z",
            "published": True,
            "published_packages": ["sigil-tui-core", "sigil-tui-ratatui", "sigil-tui"],
            "release_cycle_id": "cycle-1",
            "release_cycle_completed": True,
            "user_validation_id": "validation-1",
            "user_validation_status": "passed",
            "implementation_commit": "a" * 40,
        }
        self.assertEqual(MODULE.validate_evidence(evidence), [])


if __name__ == "__main__":
    unittest.main()

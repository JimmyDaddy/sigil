#!/usr/bin/env python3
"""Unit tests for the closed R70.0 enum discovery boundary."""

from __future__ import annotations

import importlib.util
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
MODULE_PATH = ROOT / "scripts/check-r70-migration-manifest.py"
SPEC = importlib.util.spec_from_file_location("r70_migration_manifest", MODULE_PATH)
if SPEC is None or SPEC.loader is None:
    raise RuntimeError(f"cannot load {MODULE_PATH}")
MODULE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(MODULE)


class R70MigrationManifestTests(unittest.TestCase):
    def test_closed_scope_discovers_all_frozen_production_variants(self) -> None:
        rows = MODULE.discover()
        keys = {(row["source_enum"], row["source_variant"]) for row in rows}
        self.assertEqual(len(rows), 274)
        self.assertEqual(len(keys), 274)
        self.assertIn(("WorkerCommand", "SubmitPrompt"), keys)
        self.assertIn(("WorkerMessage", "RunFailed"), keys)
        self.assertIn(("AppAction", "ConfigSaved"), keys)
        self.assertIn(("ResourceRecoveryActionV1", "SelectFreshAuthorityEpoch"), keys)
        self.assertIn(("DesktopRecoveryAction", "ShowDetails"), keys)

    def test_comment_stripping_preserves_enum_line_boundaries(self) -> None:
        source = "/* comment\n   still comment */\nenum Example {\n    #[doc = \"x\"]\n    One, // trailing\n    Two { value: u8 },\n}\n"
        clean = MODULE._strip_comments_preserving_lines(source)
        self.assertEqual(len(clean.splitlines()), len(source.splitlines()))
        self.assertEqual(MODULE._discover_variants_from_text(clean, "Example"), ["One", "Two"])

    def test_manifest_has_no_wildcard_or_duplicate_rows(self) -> None:
        import tomllib

        manifest = tomllib.loads(
            (ROOT / "dev/governance/r70-command-event-migration-v1.toml").read_text(
                encoding="utf-8"
            )
        )
        rows = manifest["mapping"]
        keys = [(row["source_enum"], row["source_variant"]) for row in rows]
        self.assertEqual(len(keys), len(set(keys)))
        self.assertNotIn("*", [variant for _enum, variant in keys])


if __name__ == "__main__":
    unittest.main()

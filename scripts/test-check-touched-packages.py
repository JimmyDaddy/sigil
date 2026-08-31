#!/usr/bin/env python3
"""Regression tests for package identity, scope expansion and deleted source mapping."""

from __future__ import annotations

import importlib.util
from pathlib import Path
import unittest


SCRIPT = Path(__file__).with_name("check-touched-packages.py")
SPEC = importlib.util.spec_from_file_location("check_touched_packages", SCRIPT)
assert SPEC is not None and SPEC.loader is not None
MODULE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(MODULE)


def workspace_metadata() -> dict:
    root = Path("/workspace").resolve()
    locations = {
        "sigil-tui-host": "crates/sigil-tui",
        "sigil-tui": "crates/sigil-tui-framework",
        "sigil-kernel": "crates/sigil-kernel",
        "sigil-desktop-app": "apps/desktop/src-tauri",
    }
    return {
        "workspace_root": str(root),
        "workspace_members": list(locations),
        "packages": [
            {"id": name, "name": name, "manifest_path": str(root / location / "Cargo.toml")}
            for name, location in locations.items()
        ],
    }


class TouchedPackagesTests(unittest.TestCase):
    def select(self, *paths: str) -> list[str]:
        return MODULE.touched_packages(workspace_metadata(), list(paths))

    def test_host_uses_manifest_identity(self):
        self.assertEqual(self.select("crates/sigil-tui/src/launcher.rs"), ["sigil-tui-host"])

    def test_framework_uses_public_package_identity(self):
        self.assertEqual(self.select("crates/sigil-tui-framework/src/lib.rs"), ["sigil-tui"])

    def test_both_tui_packages_are_distinct(self):
        self.assertEqual(
            self.select("crates/sigil-tui/src/lib.rs", "crates/sigil-tui-framework/src/lib.rs"),
            ["sigil-tui", "sigil-tui-host"],
        )

    def test_deleted_source_does_not_need_to_exist(self):
        self.assertEqual(self.select("crates/sigil-tui/src/deleted.rs"), ["sigil-tui-host"])

    def test_desktop_native_is_a_workspace_member(self):
        self.assertEqual(self.select("apps/desktop/src-tauri/src/main.rs"), ["sigil-desktop-app"])

    def test_non_cargo_files_select_no_package(self):
        self.assertEqual(self.select("apps/desktop/src/App.tsx", "dev/docs/guide.md"), [])

    def test_package_manifest_selects_its_real_name(self):
        self.assertEqual(self.select("crates/sigil-tui/Cargo.toml"), ["sigil-tui-host"])

    def test_all_workspace_inputs_expand_even_with_a_touched_member(self):
        expected = sorted(workspace_metadata()["workspace_members"])
        for item in ("Cargo.toml", "Cargo.lock", "rust-toolchain.toml"):
            with self.subTest(item=item):
                self.assertEqual(self.select(item, "crates/sigil-kernel/src/lib.rs"), expected)

    def test_removed_member_with_workspace_change_runs_remaining_members(self):
        self.assertEqual(
            self.select("Cargo.toml", "crates/removed/src/lib.rs"),
            sorted(workspace_metadata()["workspace_members"]),
        )

    def test_unknown_crate_fails_without_silent_directory_name_fallback(self):
        with self.assertRaisesRegex(ValueError, "no workspace package"):
            self.select("crates/sigil-tui-framework-extra/src/lib.rs")

    def test_duplicates_select_one_package(self):
        self.assertEqual(
            self.select("crates/sigil-kernel/src/lib.rs", "crates/sigil-kernel/src/tool.rs"),
            ["sigil-kernel"],
        )

    def test_dependencies_not_in_workspace_are_excluded(self):
        metadata = workspace_metadata()
        metadata["packages"].append(
            {"id": "external", "name": "external", "manifest_path": "/outside/Cargo.toml"}
        )
        self.assertEqual(
            MODULE.touched_packages(metadata, ["Cargo.lock"]), sorted(metadata["workspace_members"])
        )

    def test_missing_member_metadata_fails_closed(self):
        metadata = workspace_metadata()
        metadata["packages"].pop()
        with self.assertRaisesRegex(ValueError, "incomplete"):
            MODULE.touched_packages(metadata, ["Cargo.lock"])

    def test_empty_workspace_fails_closed(self):
        metadata = workspace_metadata()
        metadata["workspace_members"] = []
        with self.assertRaisesRegex(ValueError, "incomplete"):
            MODULE.touched_packages(metadata, ["Cargo.lock"])

    def test_path_escape_is_rejected_before_global_expansion(self):
        for path in ("../crates/sigil-kernel/src/lib.rs", "/outside/source.rs"):
            with self.subTest(path=path), self.assertRaisesRegex(ValueError, "workspace-relative"):
                self.select("Cargo.lock", path)

    def test_nested_member_owns_its_files(self):
        metadata = workspace_metadata()
        root = Path(metadata["workspace_root"])
        metadata["workspace_members"].append("parent")
        metadata["packages"].append(
            {"id": "parent", "name": "parent", "manifest_path": str(root / "apps/desktop/Cargo.toml")}
        )
        self.assertEqual(
            MODULE.touched_packages(metadata, ["apps/desktop/src-tauri/src/main.rs"]),
            ["sigil-desktop-app"],
        )


if __name__ == "__main__":
    unittest.main()

#!/usr/bin/env python3
"""Deterministic unit tests for the R71 inventory scanner's production boundary."""

from __future__ import annotations

import importlib.util
import re
from pathlib import Path


ROOT = Path(__file__).resolve().parent.parent
SPEC = importlib.util.spec_from_file_location(
    "r71_inventory_scan", ROOT / "scripts" / "r71_inventory_scan.py"
)
assert SPEC and SPEC.loader
scanner = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(scanner)


def test_cfg_test_block_is_excluded_beyond_twelve_lines() -> None:
    lines = ["#[cfg(test)]", "mod tests {"] + ["    // filler"] * 20
    lines += ["    let _ = File::create(\"test-only\");", "}"]
    context = scanner._Ctx("\n".join(lines))
    assert context.cfg_test_or_mod_tests(22)


def test_cfg_test_string_does_not_open_test_scope() -> None:
    context = scanner._Ctx(
        'let text = "#[cfg(test)] mod tests {";\nFile::create(path);'
    )
    assert not context.cfg_test_or_mod_tests(1)


def test_standalone_cfg_test_item_is_excluded() -> None:
    context = scanner._Ctx(
        "#[cfg(test)]\n"
        "fn test_only_writer() {\n"
        "    let _ = File::create(\"test-only\");\n"
        "}\n"
        "File::create(\"shipping\");"
    )
    assert context.cfg_test_or_mod_tests(1)
    assert context.cfg_test_or_mod_tests(2)
    assert not context.cfg_test_or_mod_tests(4)


def test_test_support_cfg_item_is_excluded() -> None:
    context = scanner._Ctx(
        '#[cfg(any(test, feature = "test-support"))]\n'
        "fn test_support_writer() {\n"
        "    let _ = File::create(\"test-only\");\n"
        "}\n"
        "File::create(\"shipping\");"
    )
    assert context.cfg_test_or_mod_tests(1)
    assert context.cfg_test_or_mod_tests(2)
    assert not context.cfg_test_or_mod_tests(4)


def test_non_test_feature_cfg_item_remains_shipping() -> None:
    context = scanner._Ctx(
        '#[cfg(feature = "other-feature")]\n'
        "fn shipping_feature_writer() {\n"
        "    let _ = File::create(\"shipping\");\n"
        "}"
    )
    assert not context.cfg_test_or_mod_tests(1)
    assert not context.cfg_test_or_mod_tests(2)


def test_filename_does_not_count_as_database_producer() -> None:
    sites = scanner.scan_sites(
        ROOT
    )["producer_sites"]
    assert not any(
        site["module"] == "crates/sigil-runtime/src/paths.rs"
        and site["constructor"] == "DatabaseOrSidecar"
        for site in sites
    )


def test_process_imports_are_not_spawn_sites() -> None:
    sites = scanner.scan_sites(ROOT)["process_sites"]
    assert all(site["constructor"] not in {"ProcessCommand", "TokioProcess"} for site in sites)


def test_read_only_open_options_are_not_producers() -> None:
    context = scanner._Ctx(
        "let mut options = OpenOptions::new();\n"
        "options.read(true);\n"
        "let file = options.open(path);"
    )
    assert not context.is_writable_open_options(0)
    writable = scanner._Ctx(
        "let mut options = OpenOptions::new();\n"
        "options.write(true).create_new(true);\n"
        "let file = options.open(path);"
    )
    assert writable.is_writable_open_options(0)


def test_function_declaration_named_persist_is_not_a_write_site() -> None:
    context = scanner._Ctx("pub async fn persist(record: &Record) -> Result<()> {")
    assert not any(
        re.search(pattern, context.lines[0])
        for pattern, constructor in scanner.PRODUCER_PATTERNS
        if constructor == "DirectWriteOrAtomicReplace"
    )


if __name__ == "__main__":
    for name, function in sorted(globals().items()):
        if name.startswith("test_"):
            function()
    print("r71 inventory scanner tests: 9 passed")

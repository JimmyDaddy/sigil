#!/usr/bin/env python3
"""Regression tests for RFC-0071 shipping registration ownership invariants."""

from __future__ import annotations

import importlib.util
import sys
import tempfile
from pathlib import Path


ROOT = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(ROOT / "scripts"))
SPEC = importlib.util.spec_from_file_location(
    "r71_negative_dependency_scan", ROOT / "scripts" / "r71_negative_dependency_scan.py"
)
assert SPEC and SPEC.loader
scanner = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(scanner)


def write_registry(root: Path, terminal_binding: str, terminal_type: str) -> None:
    registry = root / "crates/sigil-tools-builtin/src/registry.rs"
    registry.parent.mkdir(parents=True)
    registry.write_text(
        """
pub fn register_builtin_tools_with_unavailable_managed_execution(
    registry: &mut ToolRegistry,
    paths: BuiltinToolPaths,
) -> BuiltinToolHandles {
    register_builtin_tools_with_managed_execution_and_terminal_config_and_managed_terminal(
        registry,
        paths,
        Arc::new(UnavailableManagedCommandExecutionPortV1),
        TerminalExecutionConfig::default(),
        None,
        None,
        TERMINAL_BINDING,
    )
}

pub fn register_builtin_tools_with_managed_execution_and_terminal_config_and_managed_terminal(
    managed_terminal: TERMINAL_TYPE,
) {}
""".replace("TERMINAL_BINDING", terminal_binding).replace(
            "TERMINAL_TYPE", terminal_type
        ),
        encoding="utf-8",
    )


def test_registration_invariant_rejects_optional_terminal_and_none_binding() -> None:
    with tempfile.TemporaryDirectory() as directory:
        root = Path(directory)
        write_registry(root, "None", "Option<Arc<dyn ManagedTerminalExecutionPortV1>>")
        findings = scanner.check_registration_invariants(root)
        assert any("does not bind" in finding for finding in findings)
        assert any("optional managed terminal" in finding for finding in findings)


def test_registration_invariant_accepts_required_fail_closed_terminal() -> None:
    with tempfile.TemporaryDirectory() as directory:
        root = Path(directory)
        write_registry(
            root,
            "Arc::new(crate::managed_execution::UnavailableManagedCommandExecutionPortV1)",
            "Arc<dyn ManagedTerminalExecutionPortV1>",
        )
        assert scanner.check_registration_invariants(root) == []


if __name__ == "__main__":
    for name, function in sorted(globals().items()):
        if name.startswith("test_"):
            function()
    print("r71 negative dependency scanner tests: 2 passed")

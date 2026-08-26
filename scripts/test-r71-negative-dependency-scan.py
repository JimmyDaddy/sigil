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
    manager = root / "crates/sigil-tools-builtin/src/terminal_process/manager.rs"
    manager.parent.mkdir(parents=True, exist_ok=True)
    manager.write_text(
        """
struct TerminalProcessManager { execution_owner: TerminalExecutionOwnerV1 }
impl TerminalProcessManager {
    pub fn new_with_artifact_root_and_terminal_execution() -> Self {
        Self { execution_owner: TerminalExecutionOwnerV1::Managed(Unavailable) }
    }
    /// Binds the production terminal lifecycle.
    pub fn with_managed_execution(self) -> Self { self }
}
""",
        encoding="utf-8",
    )
    plugins = root / "crates/sigil-runtime/src/plugins.rs"
    plugins.parent.mkdir(parents=True, exist_ok=True)
    plugins.write_text(
        """
pub trait ManagedPluginHookExecutionPortV1 {}
struct ManagedPluginHookExecutionRequestV1 {
    config_grant_ref: GrantRef,
    config_grant_hash: Hash,
    durable_scope: Scope,
}
impl PluginHookExecutionRunner {
    pub fn new(executor: Arc<dyn ManagedPluginHookExecutionPortV1>) -> Self { todo!() }
    fn execute() {
        let purpose = purpose: sigil_kernel::managed_execution::ExecutionPurposeV1::ExtensionProcess;
    }
}
""",
        encoding="utf-8",
    )
    composition = root / "crates/sigil-runtime/src/r71_authority_composition.rs"
    composition.write_text(
        "pub plugin_hook_execution: Route\nfn plugin_hook_runner() {}\nlet channel = Ch::ApplicationControlLog;\n",
        encoding="utf-8",
    )
    (root / "crates/sigil-runtime/src/managed_resource_adapters.rs").write_text(
        "materialize_plugin_extension_admission\n"
        "authorize_extension(&decision, &plan)\n"
        "StorageWriterChannelV1::ApplicationControlLog\n"
        "seal_extension_execution_proof\n"
        "ProcessCancelReasonV1::UserCancelled\n"
        '"extension_process_settled"\n',
        encoding="utf-8",
    )
    (root / "crates/sigil-runtime/Cargo.toml").write_text(
        "[features]\ntest-support = []\n",
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


def test_registration_invariant_rejects_transitive_legacy_test_feature() -> None:
    with tempfile.TemporaryDirectory() as directory:
        root = Path(directory)
        write_registry(
            root,
            "Arc::new(crate::managed_execution::UnavailableManagedCommandExecutionPortV1)",
            "Arc<dyn ManagedTerminalExecutionPortV1>",
        )
        (root / "crates/sigil-runtime/Cargo.toml").write_text(
            '[features]\ntest-support = ["sigil-tools-builtin/test-support"]\n',
            encoding="utf-8",
        )
        findings = scanner.check_registration_invariants(root)
        assert any("must not activate legacy dependency features" in finding for finding in findings)


def test_registration_invariant_rejects_legacy_public_manager_constructor() -> None:
    with tempfile.TemporaryDirectory() as directory:
        root = Path(directory)
        write_registry(
            root,
            "Arc::new(crate::managed_execution::UnavailableManagedCommandExecutionPortV1)",
            "Arc<dyn ManagedTerminalExecutionPortV1>",
        )
        manager = root / "crates/sigil-tools-builtin/src/terminal_process/manager.rs"
        manager.write_text(
            manager.read_text(encoding="utf-8").replace(
                "TerminalExecutionOwnerV1::Managed(Unavailable)",
                "TerminalExecutionOwnerV1::LegacyDirect",
            ),
            encoding="utf-8",
        )
        findings = scanner.check_registration_invariants(root)
        assert any("selects legacy direct execution" in finding for finding in findings)


def test_registration_invariant_rejects_normal_build_direct_spawn() -> None:
    with tempfile.TemporaryDirectory() as directory:
        root = Path(directory)
        write_registry(
            root,
            "Arc::new(crate::managed_execution::UnavailableManagedCommandExecutionPortV1)",
            "Arc<dyn ManagedTerminalExecutionPortV1>",
        )
        manager = root / "crates/sigil-tools-builtin/src/terminal_process/manager.rs"
        manager.write_text(
            manager.read_text(encoding="utf-8")
            + "\nfn shipping_path() { Command::new(\"sh\").spawn().unwrap(); }\n",
            encoding="utf-8",
        )
        findings = scanner.check_registration_invariants(root)
        assert any("direct spawn route" in finding for finding in findings)


def test_registration_invariant_rejects_normal_build_plugin_backend() -> None:
    with tempfile.TemporaryDirectory() as directory:
        root = Path(directory)
        write_registry(
            root,
            "Arc::new(crate::managed_execution::UnavailableManagedCommandExecutionPortV1)",
            "Arc<dyn ManagedTerminalExecutionPortV1>",
        )
        plugins = root / "crates/sigil-runtime/src/plugins.rs"
        plugins.write_text(
            plugins.read_text(encoding="utf-8")
            + "\npub fn legacy_hook(executor: Arc<dyn ExecutionBackend>) { let _ = executor; }\n",
            encoding="utf-8",
        )
        findings = scanner.check_source(root)
        assert any("legacy ExecutionBackend seam" in finding for finding in findings)


if __name__ == "__main__":
    for name, function in sorted(globals().items()):
        if name.startswith("test_"):
            function()
    print("r71 negative dependency scanner tests: 6 passed")

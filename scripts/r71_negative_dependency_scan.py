#!/usr/bin/env python3
"""RFC-0071 R71.7 negative dependency and legacy-source gate.

This is intentionally a structural source scan rather than a diagnostic grep. Cargo metadata
provides the workspace dependency graph; Rust source is token-masked and evaluated only outside
cfg(test)/test modules. Any parser/import failure or unclassified inventory result is a gate
failure, including an empty source set.
"""

from __future__ import annotations

import json
import re
import subprocess
import sys
from pathlib import Path

import r71_inventory_scan


ROOT = Path(__file__).resolve().parent.parent

FORBIDDEN_DEPENDENCIES = {
    "sigil-kernel": {"sigil-runtime", "sigil-resource-authority", "sigil-sandbox"},
    "sigil-resource-authority": {"sigil-runtime", "sigil-sandbox"},
    "sigil-sandbox": {"sigil-runtime"},
    "sigil-process": {"sigil-runtime", "sigil-resource-authority", "sigil-sandbox"},
    "sigil-mcp": {"sigil-runtime", "sigil-resource-authority", "sigil-sandbox"},
    "sigil-desktop": {"sigil-runtime", "sigil-resource-authority", "sigil-sandbox"},
}

PHYSICAL_IMPORT_ALLOWLIST = {
    "sigil-runtime": {
        "crates/sigil-runtime/src/managed_resource_adapters.rs",
        "crates/sigil-runtime/src/managed_storage_writer.rs",
        "crates/sigil-runtime/src/r71_authority_composition.rs",
        "crates/sigil-runtime/src/r71_global_cutover.rs",
        "crates/sigil-runtime/src/session_scratch.rs",
    },
    "sigil-http": {
        "crates/sigil-http/src/listener.rs",
        "crates/sigil-http/src/production_driver.rs",
        "crates/sigil-http/src/support.rs",
    },
    "sigil-release-tools": {"crates/sigil-release-tools/src/lib.rs"},
    "sigil-sandbox": {
        "crates/sigil-sandbox/src/lib.rs",
        "crates/sigil-sandbox/src/managed.rs",
        "crates/sigil-sandbox/src/provider.rs",
    },
    "sigil-resource-authority": set(),
}

LEGACY_PATTERNS = (
    (re.compile(r"\bMcpProcessLaunch::owned\s*\("), "production MCP raw child launch"),
    (re.compile(r"\blaunch_planned_mcp_process\s*\("), "production planned MCP child launch"),
    (re.compile(r"\bLegacyBackendCommandExecutionPortV1\b"), "production legacy execution backend adapter"),
    (
        re.compile(r"\bregister_builtin_tools_with_paths(?:_and_execution_backend|_execution_backend)"),
        "production builtin registration exposes the legacy execution backend seam",
    ),
    (re.compile(r"\bFileProjectionStore\b"), "production path-rooted FileProjectionStore"),
    (re.compile(r'PathBuf::from\("\.sigil-(?:state|cache)"\)'), "cwd-relative legacy writable root"),
    (re.compile(r'Path::new\("\.sigil-(?:state|cache)"\)'), "cwd-relative legacy writable root"),
    (
        re.compile(r"RuntimePlanReviewChildResourceProvisionerV1::new\s*\("),
        "plan-review child provisioner may not invent an authority generation",
    ),
    (
        re.compile(r"PlanReviewCoordinator::run_plan_review\s*\("),
        "production plan review must use the current-schema child resource provisioner",
    ),
)

KERNEL_ARTIFACT_LEGACY_PATTERNS = (
    (re.compile(r"\bToolArtifactStore::for_session_path(?:_with_roots)?\s*\("),
     "path-rooted kernel artifact store constructor"),
    (re.compile(r"\b(?:root|staging_root):\s*Option<PathBuf>"),
     "kernel artifact store retains a physical root"),
    (re.compile(r"\bfn\s+legacy_(?:staging_)?root\s*\("),
     "kernel artifact store exposes a legacy physical root"),
    (re.compile(r"\b(?:OpenOptions::new|File::open|fs::(?:create_dir_all|hard_link|read|read_dir|remove_file|rename|symlink_metadata))\b"),
     "kernel artifact store performs direct filesystem IO"),
)

PLUGIN_LEGACY_PATTERNS = (
    (
        re.compile(r"\bExecutionBackend\b"),
        "production plugin hook retains the legacy ExecutionBackend seam",
    ),
    (
        re.compile(r"\bManagedCommandExecutionPortV1\b"),
        "production plugin hook retains the generic one-shot command seam",
    ),
)


def package_for_path(relative: str) -> str:
    if relative.startswith("crates/"):
        return relative.split("/", 2)[1]
    if relative.startswith("apps/desktop/src-tauri/"):
        return "sigil-desktop-app"
    return relative.split("/", 1)[0]


def production_lines(root: Path):
    count = 0
    for path in r71_inventory_scan.rust_sources(root):
        relative = path.relative_to(root).as_posix()
        context = r71_inventory_scan._Ctx(path.read_text(encoding="utf-8"))
        for index, (line, code) in enumerate(zip(context.lines, context.code_lines)):
            if context.cfg_test_or_mod_tests(index):
                continue
            count += 1
            yield relative, package_for_path(relative), index + 1, line, code
    if count == 0:
        raise RuntimeError("Rust production source scan returned zero lines")


def run_metadata(root: Path) -> tuple[dict, list[str]]:
    result = subprocess.run(
        ["cargo", "metadata", "--no-deps", "--format-version", "1"],
        cwd=root,
        capture_output=True,
        text=True,
        check=False,
    )
    if result.returncode != 0:
        raise RuntimeError(f"cargo metadata failed: {result.stderr[-2000:]}")
    metadata = json.loads(result.stdout)
    packages = {package["name"]: package for package in metadata["packages"]}
    findings: list[str] = []
    for package in sorted(packages.values(), key=lambda item: item["name"]):
        name = package["name"]
        workspace_dependencies = sorted(
            dependency["name"]
            for dependency in package["dependencies"]
            if dependency["name"] in packages
        )
        for dependency in workspace_dependencies:
            if dependency in FORBIDDEN_DEPENDENCIES.get(name, set()):
                findings.append(f"dependency {name} -> {dependency} is forbidden")
    return metadata, findings


def check_source(root: Path) -> list[str]:
    findings: list[str] = []
    for relative, package, line_number, original, code in production_lines(root):
        for pattern, description in LEGACY_PATTERNS:
            if pattern.search(code) or (
                description == "cwd-relative legacy writable root" and pattern.search(original)
            ):
                findings.append(f"{relative}:{line_number}: {description}: {original.strip()}")

        if relative == "crates/sigil-kernel/src/session/tool_artifact.rs":
            for pattern, description in KERNEL_ARTIFACT_LEGACY_PATTERNS:
                if pattern.search(code):
                    findings.append(
                        f"{relative}:{line_number}: {description}: {original.strip()}"
                    )

        if relative == "crates/sigil-runtime/src/plugins.rs":
            for pattern, description in PLUGIN_LEGACY_PATTERNS:
                if pattern.search(code):
                    findings.append(
                        f"{relative}:{line_number}: {description}: {original.strip()}"
                    )

        if "sigil_resource_authority::" in code or "sigil_sandbox::" in code:
            if relative not in PHYSICAL_IMPORT_ALLOWLIST.get(package, set()):
                findings.append(
                    f"{relative}:{line_number}: concrete authority/sandbox import outside allowlist: "
                    f"{original.strip()}"
                )

        if relative in {
            "crates/sigil-kernel/src/resource.rs",
            "crates/sigil-kernel/src/managed_execution.rs",
        } and re.search(r"\b(?:PathBuf|std::path::|\bPath\b)", code):
            findings.append(
                f"{relative}:{line_number}: kernel public execution/resource contract contains a path type: "
                f"{original.strip()}"
            )

        if (
            relative != "crates/sigil-kernel/src/cutover_manifest.rs"
            and re.search(r"\bstruct\s+CutoverSurfaceStatusV1\b", code)
        ):
            findings.append(
                f"{relative}:{line_number}: runtime/surface duplicate of kernel CutoverSurfaceStatusV1"
            )

        if re.search(r"sigil[-_]application|RFC[-_]?0070", code, re.IGNORECASE):
            findings.append(
                f"{relative}:{line_number}: RFC-0070 package split/cutover marker in R71 production source: "
                f"{original.strip()}"
            )
    return findings


def check_registration_invariants(root: Path) -> list[str]:
    """Check shipping registration ownership beyond legacy-symbol matching.

    The terminal manager still contains a test fixture fallback, so this gate verifies the
    production boundary that prevents shipping registration from constructing that fallback.
    It also makes the plugin hook's managed-only constructor contract explicit.
    """

    findings: list[str] = []
    registry_path = root / "crates/sigil-tools-builtin/src/registry.rs"
    try:
        registry_source = registry_path.read_text(encoding="utf-8")
    except OSError as error:
        return [f"failed to read terminal registration source: {error}"]

    unavailable_start = registry_source.find(
        "pub fn register_builtin_tools_with_unavailable_managed_execution"
    )
    if unavailable_start < 0:
        findings.append("shipping terminal registration helper is missing")
    else:
        next_function = registry_source.find("\npub fn ", unavailable_start + 1)
        helper = registry_source[
            unavailable_start : next_function if next_function >= 0 else len(registry_source)
        ]
        expected_terminal_binding = re.compile(
            r"TerminalExecutionConfig::default\(\),\s*"
            r"None,\s*None,\s*"
            r"Arc::new\(\s*crate::managed_execution::"
            r"UnavailableManagedCommandExecutionPortV1\s*\)",
            re.DOTALL,
        )
        if not expected_terminal_binding.search(helper):
            findings.append(
                "shipping unavailable builtin registration does not bind an unavailable managed terminal port"
            )

    full_signature = re.search(
        r"pub fn register_builtin_tools_with_managed_execution_and_terminal_config_and_managed_terminal"
        r"[\s\S]*?managed_terminal:\s*([^,\n]+)",
        registry_source,
    )
    if not full_signature or "Option" in full_signature.group(1):
        findings.append(
            "shipping builtin registration still exposes an optional managed terminal port"
        )

    manager_path = root / "crates/sigil-tools-builtin/src/terminal_process/manager.rs"
    manager_source = manager_path.read_text(encoding="utf-8")
    public_constructor = re.search(
        r"pub fn new_with_artifact_root_and_terminal_execution[\s\S]*?"
        r"\n\s*/// Binds the production terminal lifecycle",
        manager_source,
    )
    if public_constructor is None:
        findings.append("public TerminalProcessManager constructor invariant is missing")
    elif "LegacyDirect" in public_constructor.group(0):
        findings.append(
            "public TerminalProcessManager constructor selects legacy direct execution"
        )
    manager_context = r71_inventory_scan._Ctx(manager_source)
    for index, code in enumerate(manager_context.code_lines):
        if manager_context.cfg_test_or_mod_tests(index):
            continue
        if re.search(r"\bmanaged_execution\s*:\s*Option\s*<", code):
            findings.append(
                "normal-build TerminalProcessManager still stores an optional managed owner"
            )
        if re.search(r"\bCommand::new\s*\(|\bspawn_pty_runtime\s*\(", code):
            findings.append(
                f"normal-build TerminalProcessManager retains a direct spawn route at line {index + 1}"
            )

    plugins_path = root / "crates/sigil-runtime/src/plugins.rs"
    plugins_source = plugins_path.read_text(encoding="utf-8")
    if "pub trait ManagedPluginHookExecutionPortV1" not in plugins_source:
        findings.append("purpose-specific managed plugin hook port is missing")
    if not re.search(
        r"pub fn new\(executor:\s*Arc<dyn ManagedPluginHookExecutionPortV1>\)",
        plugins_source,
    ):
        findings.append(
            "production plugin hook runner is not bound to its purpose-specific managed port"
        )
    for required in (
        "config_grant_ref:",
        "config_grant_hash:",
        "durable_scope:",
        "purpose: sigil_kernel::managed_execution::ExecutionPurposeV1::ExtensionProcess",
    ):
        if required not in plugins_source:
            findings.append(
                f"managed plugin hook request is missing current-schema admission binding: {required}"
            )

    composition_source = (
        root / "crates/sigil-runtime/src/r71_authority_composition.rs"
    ).read_text(encoding="utf-8")
    activation = re.search(
        r"pub fn activate_workspace\([\s\S]*?(?=\n\s*}\n\s*})",
        composition_source,
    )
    if activation and "AuthorityGeneration {" in activation.group(0):
        findings.append(
            "workspace activation must reuse the composition authority generation"
        )
    if activation and "self.authority_generation" not in activation.group(0):
        findings.append(
            "workspace activation does not source its generation from the authority composition"
        )
    if "pub plugin_hook_execution:" not in composition_source or "plugin_hook_runner" not in composition_source:
        findings.append(
            "production authority composition does not expose the managed plugin hook route"
        )
    if "Ch::ApplicationControlLog" not in composition_source:
        findings.append(
            "production authority composition does not declare ApplicationControlLog before plugin activation"
        )

    child_provisioner_path = root / "crates/sigil-runtime/src/plan_review_coordinator.rs"
    if child_provisioner_path.exists():
        child_provisioner_source = child_provisioner_path.read_text(encoding="utf-8")
        legacy_runner = re.search(
            r"pub async fn run_plan_review<H, A>\s*\(", child_provisioner_source
        )
        if legacy_runner:
            prefix = child_provisioner_source[: legacy_runner.start()]
            if not re.search(
                r"#\[cfg\(\s*(?:test|any\(\s*test\s*,\s*feature\s*=\s*\"test-support\"\s*\))\s*\)\]",
                prefix[-400:],
            ):
                findings.append(
                    "legacy plan-review runner must be test/test-support-only; production must use the current-schema entry point"
                )
        if re.search(
            r"impl RuntimePlanReviewChildResourceProvisionerV1[\s\S]*?pub fn new\s*\(",
            child_provisioner_source,
        ):
            findings.append(
                "plan-review child provisioner exposes a default constructor without composition generation"
            )

    adapters_source = (
        root / "crates/sigil-runtime/src/managed_resource_adapters.rs"
    ).read_text(encoding="utf-8")
    for required in (
        "materialize_plugin_extension_admission",
        "authorize_extension(&decision, &plan)",
        "StorageWriterChannelV1::ApplicationControlLog",
        "seal_extension_execution_proof",
        "ProcessCancelReasonV1::UserCancelled",
        '"extension_process_settled"',
    ):
        if required not in adapters_source:
            findings.append(
                f"managed plugin Extension admission/control invariant is missing: {required}"
            )

    runtime_manifest = (root / "crates/sigil-runtime/Cargo.toml").read_text(encoding="utf-8")
    if not re.search(r"^test-support\s*=\s*\[\s*\]$", runtime_manifest, re.MULTILINE):
        findings.append(
            "sigil-runtime test-support must not activate legacy dependency features"
        )
    if re.search(
        r'^sigil-tools-builtin\s*=\s*\{[^\n]*features\s*=\s*\[[^\]]*"test-support"',
        runtime_manifest,
        re.MULTILINE,
    ):
        findings.append(
            "sigil-runtime dependency activates sigil-tools-builtin/test-support"
        )
    return findings


def check_inventory(root: Path) -> list[str]:
    findings: list[str] = []
    for checker in (
        "check-local-process-inventory.sh",
        "check-local-resource-producer-inventory.sh",
    ):
        result = subprocess.run(
            [str(root / "scripts" / checker), "--mode", "enforce"],
            cwd=root,
            capture_output=True,
            text=True,
            check=False,
        )
        if result.returncode != 0:
            detail = (result.stderr or result.stdout).strip().splitlines()
            findings.append(f"{checker} enforce failed")
            findings.extend(f"  {line}" for line in detail[:40])
    return findings


def main() -> int:
    findings: list[str] = []
    try:
        _metadata, dependency_findings = run_metadata(ROOT)
        findings.extend(dependency_findings)
        findings.extend(check_source(ROOT))
        findings.extend(check_registration_invariants(ROOT))
        findings.extend(check_inventory(ROOT))
    except Exception as error:  # parser/metadata failures are fail-closed gate findings
        findings.append(f"negative dependency scanner error: {error}")

    findings = sorted(set(findings))
    if findings:
        print("R71 negative dependency gate: FAILED", file=sys.stderr)
        for finding in findings:
            print(f"  {finding}", file=sys.stderr)
        return 2
    print("R71 negative dependency gate: passed")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

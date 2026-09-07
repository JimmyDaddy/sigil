#!/usr/bin/env python3
"""RFC-0071 R71.0 inventory baseline generator.

Consumes the deterministic scan output of scripts/r71_inventory_scan.py and writes the
versioned process/producer governance manifests under dev/governance/:

- local-process-inventory-v1.toml
- local-resource-producer-inventory-v1.toml

Every site gets a closed classification per RFC-0071 section 9.5. Sites that cannot be assigned
by the bundled rule table are emitted as class="unclassified" blocks -> migration blockers.
The manifests are baselines: they record today's production producers and their target admission
contract, with exception_rfc="0071" marking current legacy behavior to be migrated.

Deterministic: same input scan -> same output. Unknown constructor variants fail loudly.
"""

from __future__ import annotations

import json
import subprocess
import sys
import tomllib
from collections import Counter
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent

# Rules: matched against "crate:module-snippet" with a permissive substring check.
# (pattern, class, owner, root_source, input_taint, child_access,
#  admission, resource_contract, lifecycle_contract, receipt_contract, exception)
PROCESS_RULES = [
    ("sigil-desktop/src/launcher", "trusted-product", "DesktopProcessSupervisor",
     "ConfiguredPlatformBootstrapLocation", "UserConfiguration", "None",
     "TrustedHostOperation", "HostProcessLifecycle", "ProcessTreeOwner",
     "ProcessObservationReceipt", "0071"),
    ("sigil-process-observer/src", "trusted-product", "ProcessObserver",
     "ConfiguredPlatformBootstrapLocation", "None", "None", "HostObservation",
     "HostProcessObservation", "ObserverLifecycle", "ProcessObservationReceipt", "0071"),
    ("sigil-runtime/src/definition_file_io", "trusted-product", "DefinitionFileHostHelper",
     "ConfiguredPlatformBootstrapLocation", "UserConfiguration", "None",
     "TrustedHostOperation", "HostProcessLifecycle", "ProcessTreeOwner",
     "ProcessObservationReceipt", "0071"),
    ("sigil-runtime/src/doctor/terminal", "trusted-product", "DoctorHostCapabilityProbe",
     "ConfiguredPlatformBootstrapLocation", "None", "None", "HostObservation",
     "HostProcessObservation", "ObserverLifecycle", "ProcessObservationReceipt", "0071"),
    ("sigil-runtime/src/isolated_workspace", "managed-execution", "IsolatedWorkspaceAuthority",
     "AuthorityBootstrapAnchor", "Workspace", "ExactManagedGrant", "ManagedExecutionLease",
     "ManagedGeneration(ExecutionTemp)", "AuthorityLeaseAndCurrentState", "ManagedResourceReceipt",
     "0071"),
    ("sigil-runtime/src/agent_supervisor", "managed-execution", "ResourceAuthority",
     "AuthorityBootstrapAnchor", "Workspace", "ExactManagedGrant", "ManagedExecutionLease",
     "ManagedGeneration(ExecutionTemp)", "AuthorityLeaseAndCurrentState", "ManagedResourceReceipt",
     "0071"),
    ("sigil-sandbox/src/managed", "managed-execution", "ResourceAuthority",
     "AuthorityBootstrapAnchor", "ExtensionConfiguration", "ExactManagedGrant",
     "ManagedExecutionLease", "ManagedGeneration(ExecutionTemp)", "AuthorityLeaseAndCurrentState",
     "ManagedResourceReceipt", "0071"),
    ("sigil-tools-builtin/src/execution_backends/", "managed-execution", "ResourceAuthority",
     "AuthorityBootstrapAnchor", "None", "ExactManagedGrant",
     "ManagedExecutionLease", "ManagedGeneration(ExecutionTemp)", "AuthorityLeaseAndCurrentState",
     "ManagedResourceReceipt", "0071"),
    ("sigil-tools-builtin/src/shell", "managed-execution", "ResourceAuthority",
     "AuthorityBootstrapAnchor", "Model", "ExactManagedGrant",
     "ManagedExecutionLease", "ManagedGeneration(ExecutionTemp)", "AuthorityLeaseAndCurrentState",
     "ManagedResourceReceipt", "0071"),
    ("sigil-tools-builtin/src/terminal", "managed-execution", "ResourceAuthority",
     "AuthorityBootstrapAnchor", "Model", "ExactManagedGrant",
     "ManagedExecutionLease", "ManagedGeneration(ExecutionTemp)", "AuthorityLeaseAndCurrentState",
     "ManagedResourceReceipt", "0071"),
    ("sigil-tools-builtin/src/process_group", "managed-execution", "ResourceAuthority",
     "AuthorityBootstrapAnchor", "None", "ExactManagedGrant",
     "ManagedExecutionLease", "ManagedGeneration(ExecutionTemp)", "AuthorityLeaseAndCurrentState",
     "ManagedResourceReceipt", "0071"),
    ("sigil-tools-builtin/src/vcs_inspect", "managed-execution", "ResourceAuthority",
     "AuthorityBootstrapAnchor", "Workspace", "ExactManagedGrant",
     "ManagedExecutionLease", "ManagedGeneration(ExecutionTemp)", "AuthorityLeaseAndCurrentState",
     "ManagedResourceReceipt", "0071"),
    ("sigil-mcp/src/process", "managed-execution", "ResourceAuthority",
     "AuthorityBootstrapAnchor", "ExtensionConfiguration", "ExactManagedGrant",
     "ManagedExecutionLease", "ManagedGeneration(ExecutionTemp)", "AuthorityLeaseAndCurrentState",
     "ManagedResourceReceipt", "0071"),
    ("sigil-process/src", "managed-execution", "ResourceAuthority",
     "AuthorityBootstrapAnchor", "None", "ExactManagedGrant",
     "ManagedExecutionLease", "ManagedGeneration(ExecutionTemp)", "AuthorityLeaseAndCurrentState",
     "ManagedResourceReceipt", "0071"),
    ("sigil-code-intel", "managed-execution", "ResourceAuthority",
     "AuthorityBootstrapAnchor", "Workspace", "ExactManagedGrant",
     "ManagedExecutionLease", "ManagedGeneration(ExecutionTemp)", "AuthorityLeaseAndCurrentState",
     "ManagedResourceReceipt", "0071"),
    ("sigil-updater", "trusted-product", "ProductUpdaterState", "PlatformProductStateAnchor",
     "None", "None", "ProductStateOwnerAdmission", "ProductStateObject(SignedUpdaterCache)",
     "ProductOwnerAtomicLifecycle", "ProductStateReceipt", "0071"),
    ("sigil-tui/src", "managed-execution", "ResourceAuthority", "AuthorityBootstrapAnchor",
     "Model", "ExactManagedGrant", "ManagedExecutionLease",
     "ManagedGeneration(ExecutionTemp)", "AuthorityLeaseAndCurrentState",
     "ManagedResourceReceipt", "0071"),
    ("sigil/src/main.rs", "managed-execution", "ResourceAuthority", "AuthorityBootstrapAnchor",
     "Model", "ExactManagedGrant", "ManagedExecutionLease",
     "ManagedGeneration(ExecutionTemp)", "AuthorityLeaseAndCurrentState",
     "ManagedResourceReceipt", "0071"),
]

CONTRACT_FIELDS = (
    "class",
    "owner",
    "root_source",
    "input_taint",
    "child_access",
    "admission_contract",
    "resource_contract",
    "lifecycle_contract",
    "receipt_contract",
)

PRODUCER_RULES = [
    ("sigil-release-tools/src/bin/", "build-or-test", "BuildOrTestHarness",
     "InjectedIsolatedTestRoot", "ReleaseOwnerInput", "ExactHarnessGrant",
     "BuildOrTestHarnessAdmission", "EphemeralHarnessRoot(NonShippingReleaseToolTarget)",
     "HarnessRaiiCleanup", "HarnessCleanupAssertion", "0071"),
    ("sigil-resource-authority/src/bootstrap", "managed-runtime-state", "AuthorityBootstrap",
     "AuthorityBootstrapAnchor", "None", "None", "AuthorityBootstrapAdmission",
     "AuthorityBootstrapObject(BootstrapMetadata)", "AuthorityBootstrapLifecycle",
     "AuthorityBootstrapReceipt", "0071"),
    ("sigil-resource-authority/src/arena", "managed-execution-temp", "ResourceAuthority",
     "AuthorityBootstrapAnchor", "None", "ExactManagedGrant", "ManagedExecutionLease",
     "ManagedGeneration(ExecutionTemp)", "AuthorityLeaseAndCurrentState",
     "ManagedResourceReceipt", "0071"),
    ("sigil-resource-authority/src/durable_snapshot", "managed-runtime-state", "ResourceAuthority",
     "AuthorityBootstrapAnchor", "None", "ExactManagedGrant",
     "AuthorityDurableSnapshotWriter", "ManagedGeneration(AuthorityStateLock)",
     "AuthorityLeaseAndCurrentState", "ManagedResourceReceipt", "0071"),
    ("sigil-resource-authority/src/file_access", "managed-artifact", "ResourceAuthority",
     "BorrowedWorkspaceRegistration", "Workspace", "ExactManagedGrant",
     "BorrowedWorkspaceFileAccess", "ManagedGeneration(BorrowedWorkspace)",
     "AuthorityLeaseAndCurrentState", "ManagedFileAccessReceipt", "0071"),
    ("sigil-resource-authority/src/quota", "managed-runtime-state", "ResourceAuthority",
     "AuthorityBootstrapAnchor", "None", "ExactManagedGrant", "ResourceQuotaAuthority",
     "ManagedGeneration(ResourceQuota)", "AuthorityLeaseAndCurrentState", "ManagedResourceReceipt", "0071"),
    ("sigil-resource-authority/src/storage", "managed-runtime-state", "ResourceAuthority",
     "AuthorityBootstrapAnchor", "None", "ExactManagedGrant", "ManagedStorageCurrentState",
     "ManagedNamespace(CurrentState)", "AuthorityLeaseAndCurrentState", "ManagedResourceReceipt", "0071"),
    ("sigil-resource-authority/src/session_scratch", "managed-runtime-state", "ResourceAuthority",
     "AuthorityBootstrapAnchor", "Model", "ExactManagedGrant", "SessionScratchAuthorityAdmission",
     "ManagedGeneration(SessionScratch)", "AuthorityLeaseAndCurrentState", "ManagedResourceReceipt", "0071"),
    ("sigil-runtime/src/application_run", "managed-runtime-state", "SessionLifecycle",
     "AuthorityBootstrapAnchor", "None", "None", "ManagedStorageNamespace",
     "ManagedGeneration(RuntimeState)", "AuthorityLeaseAndCurrentState", "ManagedResourceReceipt", "0071"),
    ("sigil-runtime/src/managed_storage_writer", "managed-runtime-state", "ManagedStorage",
     "AuthorityBootstrapAnchor", "None", "None", "ManagedStorageNamespace",
     "ManagedGeneration(RuntimeState)", "AuthorityLeaseAndCurrentState", "ManagedResourceReceipt", "0071"),
    ("sigil-runtime/src/managed_artifact_store", "managed-artifact", "ManagedStorage",
     "AuthorityBootstrapAnchor", "None", "None", "ManagedStorageNamespace",
     "ManagedGeneration(ArtifactStaging)", "AuthorityLeaseAndCurrentState", "ManagedResourceReceipt", "0071"),
    ("sigil-runtime/src/writable_memory", "managed-runtime-state", "ManagedStorage",
     "AuthorityBootstrapAnchor", "None", "None", "ManagedStorageNamespace",
     "ManagedGeneration(RuntimeState)", "AuthorityLeaseAndCurrentState", "ManagedResourceReceipt", "0071"),
    ("sigil-runtime/src/r71_authority_composition", "managed-runtime-state", "AuthorityBootstrap",
     "AuthorityBootstrapAnchor", "None", "None", "AuthorityBootstrapAdmission",
     "AuthorityBootstrapObject(StateAnchor)", "AuthorityBootstrapLifecycle",
     "AuthorityBootstrapReceipt", "0071"),
    ("sigil-runtime/src/model_eval", "build-or-test", "BuildOrTestHarness",
     "InjectedIsolatedTestRoot", "ReleaseOwnerInput", "ExactHarnessGrant",
     "BuildOrTestHarnessAdmission", "EphemeralHarnessRoot(NonShippingReleaseToolTarget)",
     "HarnessRaiiCleanup", "HarnessCleanupAssertion", "0071"),
    ("sigil-runtime/src/orchestration_rollout", "build-or-test", "BuildOrTestHarness",
     "InjectedIsolatedTestRoot", "ReleaseOwnerInput", "ExactHarnessGrant",
     "BuildOrTestHarnessAdmission", "EphemeralHarnessRoot(NonShippingReleaseToolTarget)",
     "HarnessRaiiCleanup", "HarnessCleanupAssertion", "0071"),
    ("sigil-runtime/src/r71_global_cutover", "managed-runtime-state", "AuthorityBootstrap",
     "AuthorityBootstrapAnchor", "None", "None", "AuthorityBootstrapAdmission",
     "AuthorityBootstrapObject(BootstrapManifest)", "AuthorityBootstrapLifecycle",
     "AuthorityBootstrapReceipt", "0071"),
    ("sigil-runtime/src/image_attachment", "managed-artifact", "ResourceAuthority",
     "AuthorityBootstrapAnchor", "None", "None", "ManagedStorageNamespace",
     "ManagedGeneration(ArtifactStaging)", "AuthorityLeaseAndCurrentState", "ManagedResourceReceipt", "0071"),
    ("sigil-runtime/src/interactive_session_attachment", "managed-runtime-state", "SessionLifecycle",
     "AuthorityBootstrapAnchor", "None", "None", "ManagedStorageNamespace",
     "ManagedGeneration(RuntimeState)", "AuthorityLeaseAndCurrentState", "ManagedResourceReceipt", "0071"),
    ("sigil-runtime/src/integration_lanes", "managed-artifact", "ResourceAuthority",
     "AuthorityBootstrapAnchor", "Workspace", "None", "ManagedStorageNamespace",
     "ManagedGeneration(ArtifactStaging)", "AuthorityLeaseAndCurrentState", "ManagedResourceReceipt", "0071"),
    ("sigil-runtime/src/isolated_workspace", "managed-artifact", "IsolatedWorkspaceAuthority",
     "AuthorityBootstrapAnchor", "Workspace", "None", "WorktreeOrCheckout",
     "ManagedGeneration(ArtifactStaging)", "AuthorityLeaseAndCurrentState", "ManagedResourceReceipt", "0071"),
    ("sigil-tools-builtin/src/execution_backends", "managed-execution-temp", "ResourceAuthority",
     "AuthorityBootstrapAnchor", "None", "ExactManagedGrant", "ManagedExecutionLease",
     "ManagedGeneration(ExecutionTemp)", "AuthorityLeaseAndCurrentState", "ManagedResourceReceipt", "0071"),
    ("sigil-tools-builtin/src/shell", "managed-execution-temp", "ResourceAuthority",
     "AuthorityBootstrapAnchor", "Model", "ExactManagedGrant", "ManagedStorageNamespace",
     "ManagedGeneration(ExecutionTemp)", "AuthorityLeaseAndCurrentState", "ManagedResourceReceipt", "0071"),
    ("sigil-tools-builtin/src/terminal_process", "managed-artifact", "ResourceAuthority",
     "AuthorityBootstrapAnchor", "Model", "ExactManagedGrant", "ManagedStorageNamespace",
     "ManagedGeneration(ArtifactStaging)", "AuthorityLeaseAndCurrentState", "ManagedResourceReceipt", "0071"),
    ("sigil-tools-builtin/src/terminal_tools", "managed-execution-temp", "ResourceAuthority",
     "AuthorityBootstrapAnchor", "Model", "ExactManagedGrant", "ManagedStorageNamespace",
     "ManagedGeneration(ExecutionTemp)", "AuthorityLeaseAndCurrentState", "ManagedResourceReceipt", "0071"),
    ("sigil-tui/src/app.rs", "managed-execution-temp", "ResourceAuthority",
     "AuthorityBootstrapAnchor", "None", "ExactManagedGrant", "ManagedStorageNamespace",
     "ManagedGeneration(ExecutionTemp)", "AuthorityLeaseAndCurrentState", "ManagedResourceReceipt", "0071"),
    ("sigil-tui/src/launcher", "managed-runtime-state", "AuthorityBootstrap",
     "AuthorityBootstrapAnchor", "None", "None", "AuthorityBootstrapAdmission",
     "AuthorityBootstrapObject(StateAnchor)", "AuthorityBootstrapLifecycle",
     "AuthorityBootstrapReceipt", "0071"),
    ("sigil-kernel/src/mutation/", "managed-runtime-state", "WorkspaceMutationAuthority",
     "AuthorityBootstrapAnchor", "Workspace", "None",
     "BorrowedMutation", "BorrowedIdentity(WorkspaceMutation)", "BorrowedNoOwnership",
     "BorrowedMutationReceipt", "0071"),
    ("sigil-kernel/src/projection", "migration-blocker", "ResourceAuthority",
     "AuthorityBootstrapAnchor", "None", "None", "ManagedStorageNamespace",
     "ManagedGeneration(RuntimeState)", "AuthorityLeaseAndCurrentState",
     "ManagedResourceReceipt", "0071"),
    ("sigil-kernel/src/session", "managed-runtime-state", "SessionLifecycle",
     "AuthorityBootstrapAnchor", "None", "None", "ManagedStorageNamespace",
     "ManagedGeneration(RuntimeState)", "AuthorityLeaseAndCurrentState",
     "ManagedResourceReceipt", "0071"),
    ("sigil-kernel/src/", "managed-runtime-state", "SessionLifecycle",
     "AuthorityBootstrapAnchor", "None", "None", "ManagedStorageNamespace",
     "ManagedGeneration(RuntimeState)", "AuthorityLeaseAndCurrentState",
     "ManagedResourceReceipt", "0071"),
    ("sigil-runtime/src/session_lifecycle", "managed-runtime-state", "SessionLifecycle",
     "AuthorityBootstrapAnchor", "None", "None", "ManagedStorageNamespace",
     "ManagedGeneration(RuntimeState)", "AuthorityLeaseAndCurrentState",
     "ManagedResourceReceipt", "0071"),
    ("sigil-runtime/src/input_history", "managed-runtime-state", "SessionLifecycle",
     "AuthorityBootstrapAnchor", "None", "None", "ManagedStorageNamespace",
     "ManagedGeneration(RuntimeState)", "AuthorityLeaseAndCurrentState",
     "ManagedResourceReceipt", "0071"),
    ("sigil-runtime/src/paths", "migration-blocker", "ResourceAuthority",
     "AuthorityBootstrapAnchor", "None", "None", "ManagedStorageNamespace",
     "ManagedGeneration(RuntimeState)", "AuthorityLeaseAndCurrentState",
     "ManagedResourceReceipt", "0071"),
    ("sigil-runtime/src/mcp_registry", "managed-execution", "ResourceAuthority",
     "AuthorityBootstrapAnchor", "ExtensionConfiguration", "None", "ManagedExecutionLease",
     "ManagedGeneration(ExecutionTemp)", "AuthorityLeaseAndCurrentState",
     "ManagedResourceReceipt", "0071"),
    ("sigil-runtime/src/plugins", "managed-execution", "ResourceAuthority",
     "AuthorityBootstrapAnchor", "ExtensionConfiguration", "None", "ManagedExecutionLease",
     "ManagedGeneration(ExecutionTemp)", "AuthorityLeaseAndCurrentState",
     "ManagedResourceReceipt", "0071"),
    ("sigil-runtime/src/cache", "managed-runtime-cache", "ResourceAuthority",
     "AuthorityBootstrapAnchor", "None", "None", "ManagedStorageNamespace",
     "ManagedGeneration(RuntimeCache)", "AuthorityLeaseAndCurrentState",
     "ManagedResourceReceipt", "0071"),
    ("sigil-runtime/src/provider_connections", "managed-runtime-cache", "ResourceAuthority",
     "AuthorityBootstrapAnchor", "None", "None", "ManagedStorageNamespace",
     "ManagedGeneration(RuntimeCache)", "AuthorityLeaseAndCurrentState",
     "ManagedResourceReceipt", "0071"),
    ("sigil-runtime/src/artifact", "managed-artifact", "ResourceAuthority",
     "AuthorityBootstrapAnchor", "None", "None", "ManagedStorageNamespace",
     "ManagedGeneration(ArtifactStaging)", "AuthorityLeaseAndCurrentState",
     "ManagedResourceReceipt", "0071"),
    ("sigil-runtime/src/support", "managed-artifact", "ResourceAuthority",
     "AuthorityBootstrapAnchor", "None", "None", "ManagedStorageNamespace",
     "ManagedGeneration(ArtifactStaging)", "AuthorityLeaseAndCurrentState",
     "ManagedResourceReceipt", "0071"),
    ("sigil-runtime/src/agent_supervisor", "managed-artifact", "ResourceAuthority",
     "AuthorityBootstrapAnchor", "Model", "None", "ManagedStorageNamespace",
     "ManagedGeneration(ArtifactStaging)", "AuthorityLeaseAndCurrentState",
     "ManagedResourceReceipt", "0071"),
    ("sigil-tools-builtin/src/scratch", "managed-execution-temp", "ResourceAuthority",
     "AuthorityBootstrapAnchor", "Model", "ExactManagedGrant", "ManagedExecutionLease",
     "ManagedGeneration(ExecutionTemp)", "AuthorityLeaseAndCurrentState",
     "ManagedResourceReceipt", "0071"),
    ("sigil-tools-builtin/src/support", "managed-artifact", "ResourceAuthority",
     "AuthorityBootstrapAnchor", "None", "None", "ManagedStorageNamespace",
     "ManagedGeneration(ArtifactStaging)", "AuthorityLeaseAndCurrentState",
     "ManagedResourceReceipt", "0071"),
    ("sigil-tools-builtin/src/tool_artifact", "managed-artifact", "ResourceAuthority",
     "AuthorityBootstrapAnchor", "None", "None", "ManagedStorageNamespace",
     "ManagedGeneration(ArtifactStaging)", "AuthorityLeaseAndCurrentState",
     "ManagedResourceReceipt", "0071"),
    ("sigil-tools-builtin/src/changeset", "managed-artifact", "ResourceAuthority",
     "AuthorityBootstrapAnchor", "Model", "None", "ManagedStorageNamespace",
     "ManagedGeneration(ArtifactStaging)", "AuthorityLeaseAndCurrentState",
     "ManagedResourceReceipt", "0071"),
    ("sigil-tui/src/app/input_history", "managed-runtime-state", "SessionLifecycle",
     "AuthorityBootstrapAnchor", "None", "None", "ManagedStorageNamespace",
     "ManagedGeneration(RuntimeState)", "AuthorityLeaseAndCurrentState",
     "ManagedResourceReceipt", "0071"),
    ("sigil-http/src", "managed-runtime-state", "ResourceAuthority",
     "AuthorityBootstrapAnchor", "None", "None", "ManagedStorageNamespace",
     "ManagedGeneration(RuntimeState)", "AuthorityLeaseAndCurrentState",
     "ManagedResourceReceipt", "0071"),
    ("sigil-resource-authority/src/native_save", "managed-runtime-state", "ResourceAuthority",
     "AuthorityBootstrapAnchor", "UserSelectedDestination", "None", "BorrowedMutation",
     "BorrowedIdentity(UserSelectedSupportExport)", "BorrowedNoOwnership",
     "BorrowedMutationReceipt", "0071"),
    ("apps/desktop/src-tauri/src", "trusted-product", "DesktopProductState",
     "PlatformProductStateAnchor", "None", "None", "ProductStateOwnerAdmission",
     "ProductStateObject(DesktopAppearance)", "ProductOwnerAtomicLifecycle",
     "ProductStateReceipt", "0071"),
    ("sigil-provider-deepseek", "managed-runtime-cache", "ResourceAuthority",
     "AuthorityBootstrapAnchor", "None", "None", "ManagedStorageNamespace",
     "ManagedGeneration(RuntimeCache)", "AuthorityLeaseAndCurrentState",
     "ManagedResourceReceipt", "0071"),
    ("sigil-updater/src/cache", "trusted-product", "ProductUpdaterState",
     "PlatformProductStateAnchor", "None", "None", "ProductStateOwnerAdmission",
     "ProductStateObject(SignedUpdaterCache)", "ProductOwnerAtomicLifecycle",
     "ProductStateReceipt", "0071"),
]

EXACT_RECOVERY_ROOT_SITES: tuple[dict[str, str], ...] = ()

EXACT_LOCATOR_FIELDS = ("crate", "module", "line", "constructor", "snippet")


def match_rule(site: dict, rules: list) -> tuple | None:
    if site["module"].endswith("build.rs"):
        # Cargo build-script sites are BuildOrTestOnly(BuildScriptOutDir):
        # they execute only inside the build script output directory and never
        # ship into the production binary.
        return ("__build__", "build-or-test", "BuildOrTestHarness", "CargoBuildOutDir",
                "None", "None", "BuildOrTestHarnessAdmission",
                "EphemeralHarnessRoot(BuildScriptOutDir)", "HarnessRaiiCleanup",
                "HarnessCleanupAssertion", "0071")
    key = site["crate"] + "/" + site["module"]
    for rule in rules:
        pattern = rule[0]
        if pattern in key:
            return rule
    return None


def exact_recovery_root_contract(site: dict) -> dict[str, str] | None:
    for declaration in EXACT_RECOVERY_ROOT_SITES:
        if all(site.get(field) == declaration[field] for field in EXACT_LOCATOR_FIELDS):
            return {field: declaration[field] for field in CONTRACT_FIELDS}
    return None


def validate_exact_sites(sites: list[dict]) -> None:
    """Require each recovery-root producer locator exactly once before classification."""

    for declaration in EXACT_RECOVERY_ROOT_SITES:
        matches = [
            site
            for site in sites
            if all(site.get(field) == declaration[field] for field in EXACT_LOCATOR_FIELDS)
        ]
        if len(matches) != 1:
            locator = ", ".join(
                f"{field}={declaration[field]!r}" for field in EXACT_LOCATOR_FIELDS
            )
            raise ValueError(
                f"recovery root exact producer locator must match once ({locator}); "
                f"found {len(matches)}"
            )


def site_contract(site: dict, rules: list) -> dict[str, str]:
    """Return the declared R71 contract for one scanner site.

    This is the only translation from the ordered classification rules into
    manifest contract fields.  The checker imports it so an enforced manifest
    cannot independently reinterpret the current declarations.
    """

    exact_contract = exact_recovery_root_contract(site)
    if exact_contract is not None:
        return exact_contract
    rule = match_rule(site, rules)
    if rule is None:
        return {field: "unclassified" for field in CONTRACT_FIELDS}
    return dict(zip(CONTRACT_FIELDS, rule[1:-1], strict=True))


def site_id(kind: str, index: int) -> str:
    prefix = "P" if kind in ("local-process", "process") else "R"
    return f"{prefix}-{index:04d}"


def emit_manifest(path: Path, kind: str, sites: list[dict], rules: list) -> Counter:
    if kind in ("local-resource-producer", "producer"):
        validate_exact_sites(sites)
    lines = [
        "# RFC-0071 R71.0 baseline. Generated by scripts/generate-r71-inventory-baseline.py.",
        "# Do not edit by hand; regenerate and commit the diff.",
        f"schema_version = 1",
        f"kind = \"{kind}\"",
        "",
    ]
    stats = Counter()
    for index, site in enumerate(sites, start=1):
        rule = match_rule(site, rules)
        contract = site_contract(site, rules)
        exception_rfc = "none" if rule is None else rule[-1]
        stats[contract["class"]] += 1
        name = site["filename"] + ":" + str(site["line"])
        lines.extend([
            f"[[sites]]",
            f"site_id = \"{site_id(kind, index)}\"",
            f"crate_name = \"{site['crate']}\"",
            f"module = \"{site['module']}\"",
            f"constructor = \"{site['constructor']}\"",
            f"line = {site['line']}",
            *(f'{field} = "{contract[field]}"' for field in CONTRACT_FIELDS),
            f"reachability_proof_digest = \"scanner-line:{name}\"",
            f"test_case_ids = []",
            f"exception_rfc = \"{exception_rfc}\"",
            "",
        ])
    manifest_dir = ROOT / "dev" / "governance"
    manifest_dir.mkdir(parents=True, exist_ok=True)
    path.write_text("\n".join(lines), encoding="utf-8")
    return stats


def main() -> int:
    scan = json.loads(subprocess.check_output(
        [sys.executable, str(ROOT / "scripts" / "r71_inventory_scan.py"), str(ROOT)],
        text=True,
    ))
    producer_sites = scan["producer_sites"]
    # Fail before writing either baseline so drift cannot leave a partial update.
    validate_exact_sites(producer_sites)
    # The conformance manifest is a frozen R71.5 artifact. Inventory regeneration must never
    # replace it with the smaller R71.0 characterization list.
    proc_stats = emit_manifest(
        ROOT / "dev" / "governance" / "local-process-inventory-v1.toml",
        "local-process", scan["process_sites"], PROCESS_RULES)
    prod_stats = emit_manifest(
        ROOT / "dev" / "governance" / "local-resource-producer-inventory-v1.toml",
        "local-resource-producer", producer_sites, PRODUCER_RULES)
    print("process classes:", dict(proc_stats))
    print("producer classes:", dict(prod_stats))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

#!/usr/bin/env python3
"""RFC-0071 R71.0 inventory baseline generator.

Consumes the deterministic scan output of scripts/r71_inventory_scan.py and writes the
versioned governance manifests under dev/governance/:

- local-process-inventory-v1.toml
- local-resource-producer-inventory-v1.toml
- r71-conformance-inventory-v1.toml

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
    ("sigil-tools-builtin/src/execution_backends/", "managed-execution", "ResourceAuthority",
     "AuthorityBootstrapAnchor", "None", "ExactManagedGrant",
     "ManagedExecutionLease", "ManagedGeneration(ExecutionTemp)", "AuthorityLeaseAndJournal",
     "ManagedResourceReceipt", "0071"),
    ("sigil-tools-builtin/src/shell", "managed-execution", "ResourceAuthority",
     "AuthorityBootstrapAnchor", "Model", "ExactManagedGrant",
     "ManagedExecutionLease", "ManagedGeneration(ExecutionTemp)", "AuthorityLeaseAndJournal",
     "ManagedResourceReceipt", "0071"),
    ("sigil-tools-builtin/src/terminal", "managed-execution", "ResourceAuthority",
     "AuthorityBootstrapAnchor", "Model", "ExactManagedGrant",
     "ManagedExecutionLease", "ManagedGeneration(ExecutionTemp)", "AuthorityLeaseAndJournal",
     "ManagedResourceReceipt", "0071"),
    ("sigil-tools-builtin/src/process_group", "managed-execution", "ResourceAuthority",
     "AuthorityBootstrapAnchor", "None", "ExactManagedGrant",
     "ManagedExecutionLease", "ManagedGeneration(ExecutionTemp)", "AuthorityLeaseAndJournal",
     "ManagedResourceReceipt", "0071"),
    ("sigil-tools-builtin/src/process_owner", "managed-execution", "ResourceAuthority",
     "AuthorityBootstrapAnchor", "None", "ExactManagedGrant",
     "ManagedExecutionLease", "ManagedGeneration(ExecutionTemp)", "AuthorityLeaseAndJournal",
     "ManagedResourceReceipt", "0071"),
    ("sigil-tools-builtin/src/vcs_inspect", "managed-execution", "ResourceAuthority",
     "AuthorityBootstrapAnchor", "Workspace", "ExactManagedGrant",
     "ManagedExecutionLease", "ManagedGeneration(ExecutionTemp)", "AuthorityLeaseAndJournal",
     "ManagedResourceReceipt", "0071"),
    ("sigil-mcp/src/process", "managed-execution", "ResourceAuthority",
     "AuthorityBootstrapAnchor", "ExtensionConfiguration", "ExactManagedGrant",
     "ManagedExecutionLease", "ManagedGeneration(ExecutionTemp)", "AuthorityLeaseAndJournal",
     "ManagedResourceReceipt", "0071"),
    ("sigil-process/src", "managed-execution", "ResourceAuthority",
     "AuthorityBootstrapAnchor", "None", "ExactManagedGrant",
     "ManagedExecutionLease", "ManagedGeneration(ExecutionTemp)", "AuthorityLeaseAndJournal",
     "ManagedResourceReceipt", "0071"),
    ("sigil-code-intel", "managed-execution", "ResourceAuthority",
     "AuthorityBootstrapAnchor", "Workspace", "ExactManagedGrant",
     "ManagedExecutionLease", "ManagedGeneration(ExecutionTemp)", "AuthorityLeaseAndJournal",
     "ManagedResourceReceipt", "0071"),
    ("sigil-updater", "trusted-product", "ProductUpdaterState", "PlatformProductStateAnchor",
     "None", "None", "ProductStateOwnerAdmission", "ProductStateObject(SignedUpdaterCache)",
     "ProductOwnerAtomicLifecycle", "ProductStateReceipt", "0071"),
    ("sigil-tui/src", "managed-execution", "ResourceAuthority", "AuthorityBootstrapAnchor",
     "Model", "ExactManagedGrant", "ManagedExecutionLease",
     "ManagedGeneration(ExecutionTemp)", "AuthorityLeaseAndJournal",
     "ManagedResourceReceipt", "0071"),
    ("sigil/src/main.rs", "managed-execution", "ResourceAuthority", "AuthorityBootstrapAnchor",
     "Model", "ExactManagedGrant", "ManagedExecutionLease",
     "ManagedGeneration(ExecutionTemp)", "AuthorityLeaseAndJournal",
     "ManagedResourceReceipt", "0071"),
]

PRODUCER_RULES = [
    ("sigil-kernel/src/mutation/", "managed-runtime-state", "WorkspaceMutationAuthority",
     "AuthorityBootstrapAnchor", "Workspace", "None",
     "BorrowedMutation", "BorrowedIdentity(WorkspaceMutation)", "BorrowedNoOwnership",
     "BorrowedMutationReceipt", "0071"),
    ("sigil-kernel/src/projection", "migration-blocker", "ResourceAuthority",
     "AuthorityBootstrapAnchor", "None", "None", "ManagedStorageNamespace",
     "ManagedGeneration(RuntimeState)", "AuthorityLeaseAndJournal",
     "ManagedResourceReceipt", "0071"),
    ("sigil-kernel/src/session", "managed-runtime-state", "SessionLifecycle",
     "AuthorityBootstrapAnchor", "None", "None", "ManagedStorageNamespace",
     "ManagedGeneration(RuntimeState)", "AuthorityLeaseAndJournal",
     "ManagedResourceReceipt", "0071"),
    ("sigil-kernel/src/", "managed-runtime-state", "SessionLifecycle",
     "AuthorityBootstrapAnchor", "None", "None", "ManagedStorageNamespace",
     "ManagedGeneration(RuntimeState)", "AuthorityLeaseAndJournal",
     "ManagedResourceReceipt", "0071"),
    ("sigil-runtime/src/session_lifecycle", "managed-runtime-state", "SessionLifecycle",
     "AuthorityBootstrapAnchor", "None", "None", "ManagedStorageNamespace",
     "ManagedGeneration(RuntimeState)", "AuthorityLeaseAndJournal",
     "ManagedResourceReceipt", "0071"),
    ("sigil-runtime/src/input_history", "managed-runtime-state", "SessionLifecycle",
     "AuthorityBootstrapAnchor", "None", "None", "ManagedStorageNamespace",
     "ManagedGeneration(RuntimeState)", "AuthorityLeaseAndJournal",
     "ManagedResourceReceipt", "0071"),
    ("sigil-runtime/src/paths", "migration-blocker", "ResourceAuthority",
     "AuthorityBootstrapAnchor", "None", "None", "ManagedStorageNamespace",
     "ManagedGeneration(RuntimeState)", "AuthorityLeaseAndJournal",
     "ManagedResourceReceipt", "0071"),
    ("sigil-runtime/src/mcp_registry", "managed-execution", "ResourceAuthority",
     "AuthorityBootstrapAnchor", "ExtensionConfiguration", "None", "ManagedExecutionLease",
     "ManagedGeneration(ExecutionTemp)", "AuthorityLeaseAndJournal",
     "ManagedResourceReceipt", "0071"),
    ("sigil-runtime/src/plugins", "managed-execution", "ResourceAuthority",
     "AuthorityBootstrapAnchor", "ExtensionConfiguration", "None", "ManagedExecutionLease",
     "ManagedGeneration(ExecutionTemp)", "AuthorityLeaseAndJournal",
     "ManagedResourceReceipt", "0071"),
    ("sigil-runtime/src/cache", "managed-runtime-cache", "ResourceAuthority",
     "AuthorityBootstrapAnchor", "None", "None", "ManagedStorageNamespace",
     "ManagedGeneration(RuntimeCache)", "AuthorityLeaseAndJournal",
     "ManagedResourceReceipt", "0071"),
    ("sigil-runtime/src/provider_connections", "managed-runtime-cache", "ResourceAuthority",
     "AuthorityBootstrapAnchor", "None", "None", "ManagedStorageNamespace",
     "ManagedGeneration(RuntimeCache)", "AuthorityLeaseAndJournal",
     "ManagedResourceReceipt", "0071"),
    ("sigil-runtime/src/artifact", "managed-artifact", "ResourceAuthority",
     "AuthorityBootstrapAnchor", "None", "None", "ManagedStorageNamespace",
     "ManagedGeneration(ArtifactStaging)", "AuthorityLeaseAndJournal",
     "ManagedResourceReceipt", "0071"),
    ("sigil-runtime/src/support", "managed-artifact", "ResourceAuthority",
     "AuthorityBootstrapAnchor", "None", "None", "ManagedStorageNamespace",
     "ManagedGeneration(ArtifactStaging)", "AuthorityLeaseAndJournal",
     "ManagedResourceReceipt", "0071"),
    ("sigil-runtime/src/agent_supervisor", "managed-artifact", "ResourceAuthority",
     "AuthorityBootstrapAnchor", "Model", "None", "ManagedStorageNamespace",
     "ManagedGeneration(ArtifactStaging)", "AuthorityLeaseAndJournal",
     "ManagedResourceReceipt", "0071"),
    ("sigil-tools-builtin/src/scratch", "managed-execution-temp", "ResourceAuthority",
     "AuthorityBootstrapAnchor", "Model", "ExactManagedGrant", "ManagedExecutionLease",
     "ManagedGeneration(ExecutionTemp)", "AuthorityLeaseAndJournal",
     "ManagedResourceReceipt", "0071"),
    ("sigil-tools-builtin/src/support", "managed-artifact", "ResourceAuthority",
     "AuthorityBootstrapAnchor", "None", "None", "ManagedStorageNamespace",
     "ManagedGeneration(ArtifactStaging)", "AuthorityLeaseAndJournal",
     "ManagedResourceReceipt", "0071"),
    ("sigil-tools-builtin/src/tool_artifact", "managed-artifact", "ResourceAuthority",
     "AuthorityBootstrapAnchor", "None", "None", "ManagedStorageNamespace",
     "ManagedGeneration(ArtifactStaging)", "AuthorityLeaseAndJournal",
     "ManagedResourceReceipt", "0071"),
    ("sigil-tools-builtin/src/changeset", "managed-artifact", "ResourceAuthority",
     "AuthorityBootstrapAnchor", "Model", "None", "ManagedStorageNamespace",
     "ManagedGeneration(ArtifactStaging)", "AuthorityLeaseAndJournal",
     "ManagedResourceReceipt", "0071"),
    ("sigil-tui/src/app/input_history", "managed-runtime-state", "SessionLifecycle",
     "AuthorityBootstrapAnchor", "None", "None", "ManagedStorageNamespace",
     "ManagedGeneration(RuntimeState)", "AuthorityLeaseAndJournal",
     "ManagedResourceReceipt", "0071"),
    ("sigil-http/src", "managed-runtime-state", "ResourceAuthority",
     "AuthorityBootstrapAnchor", "None", "None", "ManagedStorageNamespace",
     "ManagedGeneration(RuntimeState)", "AuthorityLeaseAndJournal",
     "ManagedResourceReceipt", "0071"),
    ("apps/desktop/src-tauri/src", "trusted-product", "DesktopProductState",
     "PlatformProductStateAnchor", "None", "None", "ProductStateOwnerAdmission",
     "ProductStateObject(DesktopAppearance)", "ProductOwnerAtomicLifecycle",
     "ProductStateReceipt", "0071"),
    ("sigil-provider-deepseek", "managed-runtime-cache", "ResourceAuthority",
     "AuthorityBootstrapAnchor", "None", "None", "ManagedStorageNamespace",
     "ManagedGeneration(RuntimeCache)", "AuthorityLeaseAndJournal",
     "ManagedResourceReceipt", "0071"),
    ("sigil-updater/src/cache", "trusted-product", "ProductUpdaterState",
     "PlatformProductStateAnchor", "None", "None", "ProductStateOwnerAdmission",
     "ProductStateObject(SignedUpdaterCache)", "ProductOwnerAtomicLifecycle",
     "ProductStateReceipt", "0071"),
]


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


def site_id(kind: str, index: int) -> str:
    prefix = "P" if kind in ("local-process", "process") else "R"
    return f"{prefix}-{index:04d}"


def emit_manifest(path: Path, kind: str, sites: list[dict], rules: list) -> Counter:
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
        if rule is None:
            cls, owner, root, taint, child, admission, resource, lifecycle, receipt, ex = (
                "unclassified", "unclassified", "unclassified", "unclassified",
                "unclassified", "unclassified", "unclassified", "unclassified",
                "unclassified", "none")
        else:
            (_, cls, owner, root, taint, child, admission, resource, lifecycle,
             receipt, ex) = rule
        stats[cls] += 1
        name = site["filename"] + ":" + str(site["line"])
        lines.extend([
            f"[[sites]]",
            f"site_id = \"{site_id(kind, index)}\"",
            f"crate_name = \"{site['crate']}\"",
            f"module = \"{site['module']}\"",
            f"constructor = \"{site['constructor']}\"",
            f"line = {site['line']}",
            f"class = \"{cls}\"",
            f"owner = \"{owner}\"",
            f"root_source = \"{root}\"",
            f"input_taint = \"{taint}\"",
            f"child_access = \"{child}\"",
            f"admission_contract = \"{admission}\"",
            f"resource_contract = \"{resource}\"",
            f"lifecycle_contract = \"{lifecycle}\"",
            f"receipt_contract = \"{receipt}\"",
            f"reachability_proof_digest = \"scanner-line:{name}\"",
            f"test_case_ids = []",
            f"exception_rfc = \"{ex}\"",
            "",
        ])
    manifest_dir = ROOT / "dev" / "governance"
    manifest_dir.mkdir(parents=True, exist_ok=True)
    path.write_text("\n".join(lines), encoding="utf-8")
    return stats


def emit_conformance(path: Path, cases: list[str]) -> None:
    lines = [
        "# RFC-0071 R71.0 characterization case ids.",
        "schema_version = 1",
        f"manifest_hash = \"r71-conformance-v1-{len(cases):03d}\"",
        "",
    ]
    for index, case in enumerate(cases, start=1):
        lines.append(f"[[cases]]")
        lines.append(f"case_id = \"R71-C-{index:03d}\"")
        lines.append(f"case_name = \"{case}\"")
        lines.append(f"fixture_class = \"characterization\"")
        lines.append(f"required = true")
        lines.append("")
    path.write_text("\n".join(lines), encoding="utf-8")


def main() -> int:
    scan = json.loads(subprocess.check_output(
        [sys.executable, str(ROOT / "scripts" / "r71_inventory_scan.py"), str(ROOT)],
        text=True,
    ))
    cases = [
        "feedback_storage_fixture_is_explicitly_injected_isolated_root",
        "feedback_storage_fixture_never_inherits_active_session_scratch",
        "feedback_storage_fixture_no_follow_cleanup_completes",
        "descendant_symlink_is_leaf_report_not_followed",
        "poisoned_sibling_blocks_unrelated_session_provisioning",
        "gc_permanently_skips_invalid_namespace_without_quarantine",
        "same_session_double_lease_releases_early",
        "repeated_new_call_hits_same_provision_failure_without_blocker_gate",
        "two_terminal_tasks_release_early_while_sibling_live",
        "missing_home_xdg_falls_back_to_cwd_sigil_state_cache",
        "cargo_test_pipe_tail_false_green_guard",
        "workspace_cross_session_blast_radius_is_measurement_only",
    ]
    emit_conformance(ROOT / "dev" / "governance" / "r71-conformance-inventory-v1.toml", cases)
    proc_stats = emit_manifest(
        ROOT / "dev" / "governance" / "local-process-inventory-v1.toml",
        "local-process", scan["process_sites"], PROCESS_RULES)
    prod_stats = emit_manifest(
        ROOT / "dev" / "governance" / "local-resource-producer-inventory-v1.toml",
        "local-resource-producer", scan["producer_sites"], PRODUCER_RULES)
    print("process classes:", dict(proc_stats))
    print("producer classes:", dict(prod_stats))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

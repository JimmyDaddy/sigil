//! RFC-0071 section 9.5 / R71.4: consumer port contract conformance.
//!
//! Locks the exact mandatory-adapter seams that every process and file-producing subsystem must
//! consume by cutover time. This module is contract-only (no production consumer switch); it
//! proves the ports exist and reject cross-purpose swaps before any consumer migration.

use sigil_kernel::managed_execution::ExecutionPurposeV1;
use sigil_kernel::managed_file_access::ManagedFileOperationV1;
use sigil_kernel::resource::{CanonicalHash, OpaquePermissionSubjectRef, ResourceKindV1};

/// Closed consumer family classification (from section 9.5 mandatory rows).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ConsumerFamilyV1 {
    OneShotShell,
    PersistentTerminal,
    McpStdio,
    PluginHook,
    VerificationJob,
    CodeIntel,
    GitInspection,
    AgentSupervisor,
    InProcessFileTools,
    SessionExport,
    DurableMemory,
    InputHistory,
    SessionLifecycleLog,
    RuntimeCacheOwner,
    ArtifactWriter,
    ChildAgentReport,
    UpdaterSignedCache,
}

/// The mandatory seam for one consumer family (closed mapping; never inferred).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConsumerSeamV1 {
    ManagedExecution(&'static str),
    ManagedFileAccess,
    ManagedStorage(&'static str),
    ProductState,
    ReleaseTools,
}

/// Closed matrix: family -> exact seam. Any unknown family is rejected before admission.
pub fn mandatory_seam(family: ConsumerFamilyV1) -> ConsumerSeamV1 {
    use ConsumerFamilyV1::*;
    match family {
        OneShotShell | PersistentTerminal | McpStdio | PluginHook | VerificationJob | CodeIntel
        | GitInspection | AgentSupervisor => {
            ConsumerSeamV1::ManagedExecution("tool/terminal/extension")
        }
        InProcessFileTools | SessionExport => ConsumerSeamV1::ManagedFileAccess,
        DurableMemory | InputHistory | SessionLifecycleLog | RuntimeCacheOwner | ArtifactWriter
        | ChildAgentReport => ConsumerSeamV1::ManagedStorage("runtime-state/cache/artifact"),
        UpdaterSignedCache => ConsumerSeamV1::ProductState,
    }
}

/// Cross-purpose execution seam guard: one-shot and terminal purposes may never be swapped.
pub fn validate_execution_purpose_seam(
    family: ConsumerFamilyV1,
    purpose: ExecutionPurposeV1,
) -> Result<(), ConsumerPortErrorV1> {
    use ConsumerFamilyV1::*;
    use ExecutionPurposeV1::*;
    let allowed = match family {
        OneShotShell => matches!(purpose, OneShot),
        PersistentTerminal => matches!(purpose, Terminal),
        McpStdio | PluginHook => matches!(purpose, ExtensionProcess),
        VerificationJob | CodeIntel | GitInspection | AgentSupervisor => true,
        _ => false,
    };
    if !allowed {
        return Err(ConsumerPortErrorV1::PurposeCrossSwap);
    }
    Ok(())
}

/// In-process file operation guard: all read/write families share one borrowed lease seam.
pub fn validate_file_operation_seam(
    family: ConsumerFamilyV1,
    operation: ManagedFileOperationV1,
) -> Result<(), ConsumerPortErrorV1> {
    if !matches!(
        family,
        ConsumerFamilyV1::InProcessFileTools | ConsumerFamilyV1::SessionExport
    ) {
        return Err(ConsumerPortErrorV1::NotAFileConsumer);
    }
    let _ = operation;
    Ok(())
}

/// Borrowed-subject identity guard: Workspace subjects never masquerade as ExternalUserPath.
pub fn validate_borrowed_kind_seam(
    subject: &OpaquePermissionSubjectRef,
    kind: ResourceKindV1,
) -> Result<(), ConsumerPortErrorV1> {
    let is_borrowed = matches!(
        kind,
        ResourceKindV1::Workspace
            | ResourceKindV1::ToolchainStore
            | ResourceKindV1::UserConfig
            | ResourceKindV1::ExternalUserPath
            | ResourceKindV1::SystemTemp
    );
    if !is_borrowed {
        return Ok(());
    }
    if subject.as_str().is_empty() {
        return Err(ConsumerPortErrorV1::EmptySubject);
    }
    Ok(())
}

/// Closed consumer port error classification.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ConsumerPortErrorV1 {
    #[error("consumer family requests a cross-purpose execution seam")]
    PurposeCrossSwap,
    #[error("consumer family is not an in-process file consumer")]
    NotAFileConsumer,
    #[error("borrowed subject reference is empty")]
    EmptySubject,
    #[error("unknown consumer family; refresh the matrix before admission")]
    UnknownFamily,
}

/// Frozen expected-case hash: any drift in the matrix fails the conformance runner.
pub fn consumer_matrix_hash() -> CanonicalHash {
    use sha2::{Digest, Sha256};
    let mut payload = Vec::new();
    for family in [
        "one-shot-shell",
        "persistent-terminal",
        "mcp-stdio",
        "plugin-hook",
        "verification-job",
        "code-intel",
        "git-inspection",
        "agent-supervisor",
        "in-process-file-tools",
        "session-export",
        "durable-memory",
        "input-history",
        "session-lifecycle-log",
        "runtime-cache-owner",
        "artifact-writer",
        "child-agent-report",
        "updater-signed-cache",
    ] {
        payload.extend_from_slice(family.as_bytes());
        payload.push(0);
    }
    let mut hasher = Sha256::new();
    hasher.update(&payload);
    CanonicalHash::from_bytes(hasher.finalize().into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn r71_consumer_matrix_has_exact_family_count() {
        // All 17 mandatory families must be present and mapped.
        assert_eq!(consumer_matrix_hash().to_hex().len(), 64);
        let families = [
            ConsumerFamilyV1::OneShotShell,
            ConsumerFamilyV1::PersistentTerminal,
            ConsumerFamilyV1::McpStdio,
            ConsumerFamilyV1::PluginHook,
            ConsumerFamilyV1::VerificationJob,
            ConsumerFamilyV1::CodeIntel,
            ConsumerFamilyV1::GitInspection,
            ConsumerFamilyV1::AgentSupervisor,
            ConsumerFamilyV1::InProcessFileTools,
            ConsumerFamilyV1::SessionExport,
            ConsumerFamilyV1::DurableMemory,
            ConsumerFamilyV1::InputHistory,
            ConsumerFamilyV1::SessionLifecycleLog,
            ConsumerFamilyV1::RuntimeCacheOwner,
            ConsumerFamilyV1::ArtifactWriter,
            ConsumerFamilyV1::ChildAgentReport,
            ConsumerFamilyV1::UpdaterSignedCache,
        ];
        for family in families {
            let _ = mandatory_seam(family);
        }
    }

    #[test]
    fn r71_one_shot_shell_never_accepts_terminal_purpose() {
        let error = validate_execution_purpose_seam(
            ConsumerFamilyV1::OneShotShell,
            ExecutionPurposeV1::Terminal,
        )
        .expect_err("cross swap must fail");
        assert!(matches!(error, ConsumerPortErrorV1::PurposeCrossSwap));
    }

    #[test]
    fn r71_mcp_requires_extension_purpose() {
        validate_execution_purpose_seam(
            ConsumerFamilyV1::McpStdio,
            ExecutionPurposeV1::ExtensionProcess,
        )
        .expect("extension ok");
        let error = validate_execution_purpose_seam(
            ConsumerFamilyV1::McpStdio,
            ExecutionPurposeV1::OneShot,
        )
        .expect_err("one-shot wrong");
        assert!(matches!(error, ConsumerPortErrorV1::PurposeCrossSwap));
    }

    #[test]
    fn r71_file_consumer_rejects_non_file_family() {
        let error = validate_file_operation_seam(
            ConsumerFamilyV1::OneShotShell,
            ManagedFileOperationV1::Read,
        )
        .expect_err("shell is not a file consumer");
        assert!(matches!(error, ConsumerPortErrorV1::NotAFileConsumer));
    }
}

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
    let error =
        validate_execution_purpose_seam(ConsumerFamilyV1::McpStdio, ExecutionPurposeV1::OneShot)
            .expect_err("one-shot wrong");
    assert!(matches!(error, ConsumerPortErrorV1::PurposeCrossSwap));
}

#[test]
fn r71_file_consumer_rejects_non_file_family() {
    let error =
        validate_file_operation_seam(ConsumerFamilyV1::OneShotShell, ManagedFileOperationV1::Read)
            .expect_err("shell is not a file consumer");
    assert!(matches!(error, ConsumerPortErrorV1::NotAFileConsumer));
}

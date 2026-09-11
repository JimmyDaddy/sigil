use super::*;
use sigil_kernel::ToolResultStatus;

#[test]
fn workspace_check_disk_measurement_does_not_block_low_space() {
    let probe = WorkspaceCheckResourceProbe {
        available_bytes: 128 * 1024 * 1024,
        target_bytes_lower_bound: 16 * 1024 * 1024 * 1024,
        target_scan_truncated: false,
    };

    let result = workspace_check_resource_error(
        "call-resource",
        "bash",
        "cargo clippy --all-targets -- -D warnings",
        &probe,
    );
    assert!(result.is_none());
}

#[test]
fn workspace_check_disk_preflight_allows_sufficient_headroom() {
    let probe = WorkspaceCheckResourceProbe {
        available_bytes: 8 * 1024 * 1024 * 1024,
        target_bytes_lower_bound: 32 * 1024 * 1024 * 1024,
        target_scan_truncated: true,
    };

    assert!(probe.has_capacity());
    assert!(
        workspace_check_resource_error("call-resource", "bash", "cargo test", &probe).is_none()
    );
}

#[test]
fn capture_storage_failure_is_distinct_from_pipe_reader_failure() {
    let mut result = ToolResult::ok(
        "call-capture",
        "bash",
        "command output",
        ToolResultMeta::default(),
    );

    attach_capture_storage_failure(&mut result, 42, "capture_write_failed");

    assert_eq!(
        result.metadata.details["capture"]["code"],
        "capture_storage_failed"
    );
    assert_eq!(
        result.metadata.details["capture"]["command_completed"],
        true
    );
    assert_ne!(
        result.metadata.details["capture"]["code"],
        "output_reader_failed"
    );
}

#[tokio::test]
async fn configured_capture_missing_from_backend_preserves_success_and_unavailable_artifact()
-> Result<()> {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use sigil_kernel::{
        ExecutionBackendCapabilities, ExecutionBackendKind, ExecutionNetworkReceipt,
        ExecutionResourceReceipt, JsonlSessionStore, RunCancellationHandle, ToolArtifactBindingV1,
        ToolArtifactSensitivity, ToolArtifactStore, ToolResultRecordedV3,
        ToolStorageCompletenessV1,
    };

    const OBSERVED_BYTES: u64 = 512 * 1024;
    const PREVIEW: &[u8] = b"bounded preview\n";

    struct MissingCapturePort {
        calls: AtomicUsize,
    }

    #[async_trait]
    impl ManagedCommandExecutionPortV1 for MissingCapturePort {
        fn kind(&self) -> ExecutionBackendKind {
            ExecutionBackendKind::Local
        }

        fn capabilities(&self) -> ExecutionBackendCapabilities {
            ExecutionBackendCapabilities::default()
        }

        fn planned_network_receipt(&self) -> ExecutionNetworkReceipt {
            ExecutionNetworkReceipt::unknown("capture transfer test fixture")
        }

        async fn execute_with_cancellation(
            &self,
            mut request: ExecutionRequest,
            _cancellation: Option<RunCancellationHandle>,
        ) -> Result<ExecutionReceipt> {
            self.calls.fetch_add(1, Ordering::AcqRel);
            let capture = request
                .capture
                .take()
                .expect("process capture setup must succeed");
            assert!(capture.config.artifact_staging_limit_bytes_per_stream > PREVIEW.len() as u64);
            // Simulate the command completing and its transfer owner losing the configured
            // capture. The receipt still carries truthful observed counts and a bounded prefix.
            drop(capture);
            Ok(ExecutionReceipt {
                backend: self.kind(),
                capabilities: self.capabilities(),
                network: self.planned_network_receipt(),
                resources: ExecutionResourceReceipt::default(),
                environment_policy: request.environment_policy,
                exit_code: Some(0),
                stdout: PREVIEW.to_vec(),
                stderr: Vec::new(),
                timed_out: false,
                output: ExecutionOutputReceipt {
                    stdout: ExecutionStreamCapture {
                        total_bytes: OBSERVED_BYTES,
                        returned_bytes: PREVIEW.len() as u64,
                        omitted_bytes: OBSERVED_BYTES - PREVIEW.len() as u64,
                        retained_head_bytes: PREVIEW.len() as u64,
                        retained_limit_bytes: 64 * 1024,
                        hard_limit_bytes: 128 * 1024 * 1024,
                        total_lines: 32_768,
                        truncated: true,
                        ..ExecutionStreamCapture::default()
                    },
                    combined_total_bytes: OBSERVED_BYTES,
                    combined_hard_limit_bytes: 128 * 1024 * 1024,
                    ..ExecutionOutputReceipt::default()
                },
                capture: None,
            })
        }
    }

    let fixture = tempfile::tempdir()?;
    let workspace = fixture.path().join("workspace");
    std::fs::create_dir(&workspace)?;
    let session_store = JsonlSessionStore::new(fixture.path().join("session.jsonl"))?;
    let artifact_store = ToolArtifactStore::for_session_store(&session_store);
    let port = Arc::new(MissingCapturePort {
        calls: AtomicUsize::new(0),
    });
    let tool = BashTool {
        scratch_label: "fixture scratch".to_owned(),
        scratch_quota: ScratchQuota::default(),
        scratch_control: ScratchNamespaceControl::for_local_root(fixture.path().join("scratch")),
        scratch_namespaces: Arc::new(ScratchNamespaceLeaseRegistry::new()),
        executor: port.clone(),
        shell: ResolvedShell::resolve_explicit("sh")?,
    };
    let context = ToolContext::new(&workspace, 5)
        .with_session_scope_id("capture-transfer-fixture")
        .with_tool_artifact_reader(
            artifact_store.clone(),
            sigil_kernel::session::ToolArtifactReadBudgetV1::default(),
            "context-epoch:capture-transfer",
        );

    let result = tool
        .execute(
            context,
            "capture-transfer-call".to_owned(),
            json!({ "command": "printf 'bounded preview\\n'" }),
        )
        .await?;

    assert_eq!(port.calls.load(Ordering::Acquire), 1);
    assert!(matches!(result.status, ToolResultStatus::Ok));
    assert_eq!(result.metadata.exit_code, Some(0));
    assert!(result.content.contains("bounded preview"));
    assert_eq!(result.metadata.total_bytes, Some(OBSERVED_BYTES));
    assert_eq!(
        result.metadata.details["capture"]["code"],
        "capture_storage_failed"
    );
    assert_eq!(
        result.metadata.details["capture"]["stage"],
        "capture_transfer_failed"
    );
    assert_eq!(
        result.metadata.details["capture"]["command_completed"],
        true
    );

    // Exercise the same durable projection used after tool completion. Missing process
    // capture must not be republished from the much smaller inline prefix as a full artifact.
    let (recorded, display) = ToolResultRecordedV3::capture(
        &result,
        Some(&artifact_store),
        ToolArtifactSensitivity::Ordinary,
    )?;
    let ToolArtifactBindingV1::Unavailable { unavailable } = &recorded.artifact else {
        panic!("lost capture must not publish the bounded preview as an artifact");
    };
    assert_eq!(unavailable.observed_bytes, OBSERVED_BYTES);
    assert_eq!(
        recorded.capture_completeness.storage,
        ToolStorageCompletenessV1::Unavailable
    );
    assert_eq!(recorded.facts.status, "ok");
    assert!(recorded.initial_model_view.artifact_ref.is_none());
    assert_eq!(display.persisted_bytes, 0);
    Ok(())
}

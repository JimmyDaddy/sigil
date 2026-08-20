use super::*;
use sigil_kernel::ToolResultStatus;

#[test]
fn workspace_check_disk_preflight_blocks_low_space_with_actionable_error() {
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
    )
    .expect("low disk space should pause the validation command");
    let ToolResultStatus::Error(error) = result.status else {
        panic!("resource preflight should return an error result");
    };
    assert_eq!(error.kind, ToolErrorKind::ResourceExhausted);
    assert!(error.retryable);
    assert_eq!(error.details["code"], "disk_space_exhausted");
    assert_eq!(error.details["resource"], "disk_space");
    assert!(
        error.details["required_available_bytes"]
            .as_u64()
            .expect("required bytes should be numeric")
            > probe.available_bytes
    );
    assert!(error.details["action"].as_str().is_some_and(|action| {
        action.contains("free disk space") && action.contains("resume the Task")
    }));
    assert!(error.details.get("command").is_none());
    assert!(error.details["command_sha256"].as_str().is_some());
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

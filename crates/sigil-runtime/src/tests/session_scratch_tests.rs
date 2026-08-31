use super::*;

#[cfg(unix)]
use std::{fs, os::unix::fs::symlink};

#[cfg(unix)]
use serde_json::json;
#[cfg(unix)]
use sigil_kernel::{ToolCall, ToolContext, ToolErrorKind, ToolRegistry, ToolResultStatus};
#[cfg(unix)]
use sigil_tools_builtin::{
    BuiltinToolPaths, ManagedCommandExecutionPortV1, ManagedTerminalExecutionPortV1,
    ScratchNamespaceControl, ScratchQuota, TerminalExecutionConfig,
    register_builtin_tools_with_managed_execution_and_terminal_config_and_managed_terminal,
};

#[test]
fn authority_provider_lease_blocks_gc_for_active_tool() {
    let temp = tempfile::tempdir().expect("temp");
    let control = authority_scratch_control(temp.path().join("scratch"));
    control
        .ensure_session_scratch(
            Some("session-a"),
            &sigil_tools_builtin::ScratchQuota {
                per_session_bytes: 1024,
                workspace_hard_bytes: 4096,
            },
        )
        .expect("provision");
    let _lease = control.namespaces.acquire("session-a").expect("lease");
    let report = control
        .gc_scratch_namespaces(
            &sigil_tools_builtin::ScratchGcConfig { ttl_ms: 0 },
            u64::MAX,
        )
        .expect("gc");
    assert_eq!(report.skipped_leased, 1);
    assert_eq!(report.deleted, 0);
}

#[test]
fn authority_entry_limit_keeps_counts_in_the_existing_tool_diagnostic() {
    let error = authority_error(
        sigil_resource_authority::session_scratch::SessionScratchErrorV1::EntryLimitExceeded {
            limit: 250_000,
            observed: 250_001,
        },
    );
    assert_eq!(
        error.downcast_ref::<sigil_tools_builtin::ScratchMeasurementError>(),
        Some(
            &sigil_tools_builtin::ScratchMeasurementError::EntryLimitExceeded {
                limit: 250_000,
                observed_entries: 250_001,
            }
        )
    );
    assert!(
        error
            .downcast_ref::<sigil_kernel::resource::ScratchQuotaExceededError>()
            .is_none()
    );
}

#[cfg(unix)]
fn tool_call(name: &str, args: serde_json::Value) -> ToolCall {
    ToolCall {
        id: format!("call-{name}"),
        name: name.to_owned(),
        args_json: serde_json::to_string(&args).expect("tool args serialize"),
    }
}

#[cfg(unix)]
fn registered_authority_tools(
    workspace: &std::path::Path,
    scratch_root: std::path::PathBuf,
    execution_temp_root: std::path::PathBuf,
    quota: ScratchQuota,
) -> (ToolRegistry, ScratchNamespaceControl) {
    fs::create_dir_all(&execution_temp_root).expect("execution temp root");
    let route = std::sync::Arc::new(
        crate::managed_resource_adapters::RuntimeManagedCommandExecutionRouteV1::new(
            std::sync::Arc::new(crate::r71_shadow_planner::ShadowPlannerV1::new(
                crate::r71_shadow_planner::ShadowPlannerConfigV1::default(),
            )),
            std::sync::Arc::new(sigil_kernel::capability_issuer::KernelCapabilityBrokerV1::new()),
            execution_temp_root,
        )
        .with_process_inventory(std::sync::Arc::new(
            sigil_resource_authority::InMemoryAuthorityProcessInventoryV1::default(),
        )),
    );
    let scratch = authority_scratch_control(scratch_root.clone());
    let mut registry = ToolRegistry::new();
    let command_port: std::sync::Arc<dyn ManagedCommandExecutionPortV1> = route.clone();
    let terminal_port: std::sync::Arc<dyn ManagedTerminalExecutionPortV1> = route;
    register_builtin_tools_with_managed_execution_and_terminal_config_and_managed_terminal(
        &mut registry,
        BuiltinToolPaths {
            changesets_root: workspace.join("state/artifacts/changesets"),
            changesets_label_root: std::path::PathBuf::from("state/artifacts/changesets"),
            terminal_tasks_root: workspace.join("state/artifacts/tasks"),
            terminal_tasks_label_root: std::path::PathBuf::from("state/artifacts/tasks"),
            scratch_root,
            scratch_label: "cache/tmp".to_owned(),
            scratch_quota: quota,
        },
        command_port,
        TerminalExecutionConfig::default(),
        None,
        Some(scratch.clone()),
        terminal_port,
    );
    (registry, scratch)
}

#[cfg(unix)]
fn assert_quota_error(
    result: &sigil_kernel::ToolResult,
    scope: &str,
    usage_bytes: u64,
    quota_bytes: u64,
    hidden_path: &std::path::Path,
) {
    let ToolResultStatus::Error(error) = &result.status else {
        panic!("scratch quota must fail before spawning the command");
    };
    assert_eq!(error.kind, ToolErrorKind::ScratchQuotaExceeded);
    assert_eq!(error.details["scope"], scope);
    assert_eq!(error.details["usage_bytes"], usage_bytes);
    assert_eq!(error.details["quota_bytes"], quota_bytes);
    assert!(!error.retryable);
    assert!(error.message.contains("reset scratch storage"));
    assert!(!error.message.contains(&hidden_path.display().to_string()));
    assert!(!result.content.contains(&hidden_path.display().to_string()));
    assert_eq!(error.details["recovery"]["automatic"], false);
    assert_eq!(error.details["recovery"]["requires_confirmation"], true);
}

#[cfg(unix)]
#[tokio::test]
async fn registered_authority_provider_carries_scratch_quota_across_bash_and_terminal()
-> anyhow::Result<()> {
    let temp = tempfile::tempdir()?;
    let workspace = temp.path().join("workspace");
    fs::create_dir(&workspace)?;
    let session = "quota-session-0000-0000-0000-000000000301";
    let context = ToolContext::new(workspace.clone(), 5).with_session_scope_id(session);

    let session_scratch = temp.path().join("session-scratch");
    let (registry, scratch) = registered_authority_tools(
        &workspace,
        session_scratch,
        temp.path().join("execution-temp-session"),
        ScratchQuota {
            per_session_bytes: 16,
            workspace_hard_bytes: 64,
        },
    );
    let first = registry
        .execute(
            context.clone(),
            tool_call(
                "bash",
                json!({ "command": "printf 123456789012345678901234 > \"$SIGIL_SCRATCH_DIR/payload\"" }),
            ),
        )
        .await?;
    assert!(matches!(first.status, ToolResultStatus::Ok));
    let session_dir = scratch.session_scratch_dir(Some(session));
    assert_eq!(fs::metadata(session_dir.join("payload"))?.len(), 24);
    let bash_command =
        "printf session-recovered > quota-session-sentinel; printf session-recovered";

    let rejected = registry
        .execute(
            context.clone(),
            tool_call("bash", json!({ "command": bash_command })),
        )
        .await?;
    assert_quota_error(&rejected, "session", 24, 16, temp.path());
    assert!(
        !workspace.join("quota-session-sentinel").exists(),
        "a quota rejection must occur before Bash is spawned"
    );

    fs::remove_file(session_dir.join("payload"))?;
    let recovered = registry
        .execute(
            context,
            tool_call("bash", json!({ "command": bash_command })),
        )
        .await?;
    assert!(matches!(recovered.status, ToolResultStatus::Ok));
    assert_eq!(recovered.content, "session-recovered");
    assert_eq!(
        fs::read_to_string(workspace.join("quota-session-sentinel"))?,
        "session-recovered"
    );

    let workspace_scratch = temp.path().join("workspace-scratch");
    let (registry, scratch) = registered_authority_tools(
        &workspace,
        workspace_scratch,
        temp.path().join("execution-temp-workspace"),
        ScratchQuota {
            per_session_bytes: 64,
            workspace_hard_bytes: 16,
        },
    );
    let writer_context = ToolContext::new(workspace.clone(), 5)
        .with_session_scope_id("workspace-writer-0000-0000-0000-000000000302");
    let terminal_context = ToolContext::new(workspace.clone(), 5)
        .with_session_scope_id("workspace-terminal-0000-0000-0000-000000000303");
    let write = registry
        .execute(
            writer_context.clone(),
            tool_call(
                "bash",
                json!({ "command": "printf 123456789012345678901234 > \"$SIGIL_SCRATCH_DIR/workspace-payload\"" }),
            ),
        )
        .await?;
    assert!(matches!(write.status, ToolResultStatus::Ok));
    let workspace_payload = scratch
        .session_scratch_dir(writer_context.session_scope_id())
        .join("workspace-payload");
    assert_eq!(fs::metadata(&workspace_payload)?.len(), 24);
    let terminal_command = concat!(
        "printf terminal-sentinel > \"$SIGIL_SCRATCH_DIR/terminal-sentinel\"; ",
        "printf terminal-recovered; ",
        "i=0; while [ \"$i\" -lt 60 ]; do i=$((i + 1)); sleep 1; done"
    );

    let terminal_rejected = registry
        .execute(
            terminal_context.clone(),
            tool_call(
                "terminal_start",
                json!({
                    "task_id": "quota-terminal",
                    "command": terminal_command,
                    "mode": "background",
                    "shell": "sh"
                }),
            ),
        )
        .await?;
    assert_quota_error(&terminal_rejected, "workspace", 24, 16, temp.path());
    assert!(
        !scratch
            .session_scratch_dir(terminal_context.session_scope_id())
            .join("terminal-sentinel")
            .exists(),
        "a quota rejection must occur before the managed terminal launcher is called"
    );

    fs::remove_file(workspace_payload)?;
    let started = registry
        .execute(
            terminal_context.clone(),
            tool_call(
                "terminal_start",
                json!({
                    "task_id": "quota-terminal",
                    "command": terminal_command,
                    "mode": "background",
                    "shell": "sh"
                }),
            ),
        )
        .await?;
    assert!(matches!(started.status, ToolResultStatus::Ok));
    let generation = started.metadata.details["generation"]
        .as_u64()
        .expect("terminal start generation");
    let waited = registry
        .execute(
            terminal_context.clone(),
            tool_call(
                "terminal_wait",
                json!({
                    "task_id": "quota-terminal",
                    "after_generation": generation,
                    "until": "output_contains",
                    "value": "terminal-recovered",
                    "timeout_secs": 30
                }),
            ),
        )
        .await?;
    assert!(matches!(waited.status, ToolResultStatus::Ok));
    let output = registry
        .execute(
            terminal_context.clone(),
            tool_call(
                "terminal_read",
                json!({
                    "task_id": "quota-terminal",
                    "offset": 0,
                    "limit_bytes": 64,
                    "include_content": true
                }),
            ),
        )
        .await?;
    assert!(matches!(output.status, ToolResultStatus::Ok));
    assert_eq!(output.content, "terminal-recovered");
    assert_eq!(
        fs::read_to_string(
            scratch
                .session_scratch_dir(terminal_context.session_scope_id())
                .join("terminal-sentinel"),
        )?,
        "terminal-sentinel"
    );
    let cancelled = registry
        .execute(
            terminal_context.clone(),
            tool_call("terminal_cancel", json!({ "task_id": "quota-terminal" })),
        )
        .await?;
    assert!(matches!(cancelled.status, ToolResultStatus::Ok));
    assert_eq!(cancelled.metadata.details["status"], "cancelled");

    let io_scratch = temp.path().join("io-scratch");
    let (registry, scratch) = registered_authority_tools(
        &workspace,
        io_scratch,
        temp.path().join("execution-temp-io"),
        ScratchQuota {
            per_session_bytes: 64,
            workspace_hard_bytes: 64,
        },
    );
    let io_context = ToolContext::new(workspace, 5)
        .with_session_scope_id("io-session-0000-0000-0000-000000000304");
    let initial = registry
        .execute(
            io_context.clone(),
            tool_call("bash", json!({ "command": "printf ready" })),
        )
        .await?;
    assert!(matches!(initial.status, ToolResultStatus::Ok));
    symlink(
        temp.path().join("outside"),
        scratch
            .session_scratch_dir(io_context.session_scope_id())
            .join("escape"),
    )?;
    let io_failure = registry
        .execute(
            io_context,
            tool_call(
                "bash",
                json!({ "command": "printf should-not-run > io-failure-sentinel" }),
            ),
        )
        .await?;
    let ToolResultStatus::Error(error) = &io_failure.status else {
        panic!("unsafe scratch namespace must fail before spawning the command");
    };
    assert_eq!(error.kind, ToolErrorKind::Io);
    assert_ne!(error.kind, ToolErrorKind::ScratchQuotaExceeded);
    assert!(error.details.get("usage_bytes").is_none());
    assert!(!error.message.contains(&temp.path().display().to_string()));
    assert!(!error.retryable);
    assert!(
        !temp
            .path()
            .join("workspace")
            .join("io-failure-sentinel")
            .exists(),
        "a non-quota scratch failure must not be classified after a child starts"
    );
    Ok(())
}

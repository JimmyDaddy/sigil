use std::{
    path::Path,
    sync::{Arc, Mutex},
};

use anyhow::{Context, Result};
use serde_json::{Value, json};
use sigil_kernel::{
    ToolCall, ToolContext, ToolErrorKind, ToolProgressEvent, ToolProgressSink, ToolRegistry,
    ToolResultStatus,
};

use crate::{BuiltinToolPaths, register_builtin_tools_with_paths};

fn registry(workspace: &Path) -> ToolRegistry {
    let mut registry = ToolRegistry::new();
    register_builtin_tools_with_paths(
        &mut registry,
        BuiltinToolPaths::workspace_defaults(workspace),
    );
    registry
}

fn call(name: &str, args: Value) -> ToolCall {
    ToolCall {
        id: format!("{name}-call"),
        name: name.to_owned(),
        args_json: args.to_string(),
    }
}

#[derive(Default)]
struct Progress(Mutex<Vec<ToolProgressEvent>>);

impl ToolProgressSink for Progress {
    fn emit(&self, event: ToolProgressEvent) -> Result<()> {
        self.0
            .lock()
            .map_err(|_| anyhow::anyhow!("progress lock poisoned"))?
            .push(event);
        Ok(())
    }
}

#[cfg(unix)]
#[tokio::test]
async fn exec_silent_process_yields_then_waits_on_the_same_execution() -> Result<()> {
    let workspace = tempfile::tempdir()?;
    let registry = registry(workspace.path());
    let progress = Arc::new(Progress::default());
    let context = ToolContext::new(workspace.path(), 5).with_progress_sink(progress.clone());
    let start = registry
        .execute(
            context.clone(),
            call(
                "exec_command",
                json!({
                    "command": "sleep 0.2", "shell": "sh", "yield_time_ms": 0,
                }),
            ),
        )
        .await?;
    assert!(!start.is_error(), "{start:?}");
    let execution_id = start.metadata.details["execution_id"]
        .as_str()
        .context("execution id")?;
    assert_eq!(start.metadata.details["verdict"], "running");
    assert_eq!(start.metadata.details["cleanup_complete"], false);
    assert!(start.metadata.details["exit_code"].is_null());
    assert_eq!(
        start.metadata.details["call"]["summary"],
        "command=sleep 0.2"
    );
    {
        let events = progress
            .0
            .lock()
            .map_err(|_| anyhow::anyhow!("progress lock poisoned"))?;
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].tool_name, "exec_command");
        assert_eq!(events[0].execution_id.as_str(), execution_id);
        assert_eq!(events[0].status, "running");
        assert!(events[0].output_preview.is_none());
    }
    let poll = registry
        .execute(
            context.clone(),
            call(
                "exec_wait",
                json!({
                    "execution_id": execution_id, "yield_time_ms": 0,
                }),
            ),
        )
        .await?;
    assert_eq!(poll.metadata.details["outcome"], "timeout");
    assert_eq!(poll.metadata.details["verdict"], "running");
    let completed = registry
        .execute(
            context,
            call(
                "exec_wait",
                json!({
                    "execution_id": execution_id, "yield_time_ms": 5000,
                }),
            ),
        )
        .await?;
    assert!(!completed.is_error(), "{completed:?}");
    assert_eq!(completed.metadata.details["execution_id"], execution_id);
    assert_eq!(completed.metadata.details["outcome"], "condition_met");
    assert_eq!(completed.metadata.details["verdict"], "success");
    assert_eq!(completed.metadata.details["exit_code"], 0);
    assert_eq!(completed.metadata.details["cleanup_complete"], true);
    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn exec_nonzero_exit_is_a_structured_failure_with_saved_output() -> Result<()> {
    let workspace = tempfile::tempdir()?;
    let registry = registry(workspace.path());
    let result = registry
        .execute(
            ToolContext::new(workspace.path(), 5),
            call(
                "exec_command",
                json!({
                    "command": "printf diagnostic; exit 7", "shell": "sh", "yield_time_ms": 5000,
                }),
            ),
        )
        .await?;
    let ToolResultStatus::Error(error) = &result.status else {
        anyhow::bail!("nonzero exit accepted: {result:?}");
    };
    assert_eq!(error.kind, ToolErrorKind::ExitStatus);
    assert_eq!(result.metadata.details["exit_code"], 7);
    assert_eq!(result.metadata.details["verdict"], "failed");
    assert_eq!(result.metadata.details["cleanup_complete"], true);
    assert!(result.content.contains("diagnostic"));
    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn exec_cancel_returns_cleanup_receipt_without_claiming_command_success() -> Result<()> {
    let workspace = tempfile::tempdir()?;
    let registry = registry(workspace.path());
    let context = ToolContext::new(workspace.path(), 5);
    let started = registry
        .execute(
            context.clone(),
            call(
                "exec_command",
                json!({
                    "command": "sleep 30", "shell": "sh", "yield_time_ms": 0,
                }),
            ),
        )
        .await?;
    let execution_id = &started.metadata.details["execution_id"];
    let cancelled = registry
        .execute(
            context.clone(),
            call("exec_cancel", json!({"execution_id": execution_id})),
        )
        .await?;
    assert!(!cancelled.is_error(), "{cancelled:?}");
    assert_eq!(cancelled.metadata.details["verdict"], "interrupted");
    assert_eq!(cancelled.metadata.details["status"], "cancelled");
    assert_eq!(cancelled.metadata.details["cleanup_complete"], true);
    let observed = registry
        .execute(
            context,
            call("exec_wait", json!({"execution_id": execution_id})),
        )
        .await?;
    assert!(
        observed.is_error(),
        "interrupted command must not become execution success"
    );
    assert_eq!(observed.metadata.details["verdict"], "interrupted");
    Ok(())
}

struct RejectingProgress;

impl ToolProgressSink for RejectingProgress {
    fn emit(&self, _event: ToolProgressEvent) -> Result<()> {
        anyhow::bail!("display sink has closed")
    }
}

#[cfg(unix)]
#[tokio::test]
async fn exec_retains_the_execution_when_its_progress_display_closes() -> Result<()> {
    let workspace = tempfile::tempdir()?;
    let registry = registry(workspace.path());
    let context =
        ToolContext::new(workspace.path(), 5).with_progress_sink(Arc::new(RejectingProgress));
    let start = registry
        .execute(
            context.clone(),
            call(
                "exec_command",
                json!({
                    "command": "sleep 0.1; printf completed", "shell": "sh", "yield_time_ms": 0,
                }),
            ),
        )
        .await?;
    assert!(!start.is_error(), "{start:?}");
    let execution_id = start.metadata.details["execution_id"]
        .as_str()
        .context("execution id survives projection failure")?;
    assert!(
        start.metadata.details["started_at_ms"]
            .as_u64()
            .is_some_and(|value| value > 0)
    );
    let result = registry
        .execute(
            context,
            call(
                "exec_wait",
                json!({"execution_id": execution_id, "yield_time_ms": 5000}),
            ),
        )
        .await?;
    assert!(!result.is_error(), "{result:?}");
    assert_eq!(result.metadata.details["verdict"], "success");
    assert_eq!(result.metadata.details["cleanup_complete"], true);
    assert_eq!(
        result.metadata.details["started_at_ms"],
        start.metadata.details["started_at_ms"]
    );
    assert!(result.content.contains("completed"));
    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn exec_prepared_permission_plan_matches_the_actual_execution_binding() -> Result<()> {
    let workspace = tempfile::tempdir()?;
    let registry = registry(workspace.path());
    let context = ToolContext::new(workspace.path(), 5);
    let command = call(
        "exec_command",
        json!({"command": "printf approved", "shell": "sh", "yield_time_ms": 5000}),
    );
    let plan = registry.permission_plan(&context, &command)?;
    assert_eq!(
        plan.analysis_bindings["terminal_execution_class"],
        "managed"
    );
    let result = registry
        .execute(context.with_prepared_permission_plan(plan), command)
        .await?;
    assert!(!result.is_error(), "{result:?}");
    assert_eq!(result.metadata.details["verdict"], "success");
    assert_eq!(result.metadata.details["cleanup_complete"], true);
    assert_eq!(result.metadata.exit_code, Some(0));
    assert!(result.content.contains("approved"));
    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn exec_rejects_prepared_environment_drift_before_spawning() -> Result<()> {
    let workspace = tempfile::tempdir()?;
    let registry = registry(workspace.path());
    let context = ToolContext::new(workspace.path(), 5);
    let command = call(
        "exec_command",
        json!({"command": "printf spawned > spawned.txt", "shell": "sh", "yield_time_ms": 5000}),
    );
    let mut plan = registry.permission_plan(&context, &command)?;
    plan.analysis_bindings.insert(
        "environment_binding".to_owned(),
        "shell-env-v2:changed-after-approval".to_owned(),
    );
    let error = registry
        .execute(context.with_prepared_permission_plan(plan), command)
        .await
        .expect_err("environment drift must be rejected before starting the command");
    assert!(
        error
            .to_string()
            .contains("prepared terminal environment binding changed"),
        "{error:#}"
    );
    assert!(!workspace.path().join("spawned.txt").exists());
    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn exec_control_results_carry_complete_owner_receipts() -> Result<()> {
    use sigil_kernel::TerminalTaskEntry;

    let workspace = tempfile::tempdir()?;
    let registry = registry(workspace.path());
    let context = ToolContext::new(workspace.path(), 5);
    for pty in [false, true] {
        let start = registry
            .execute(
                context.clone(),
                call(
                    "exec_command",
                    json!({
                        "command": "sleep 30", "shell": "sh", "pty": pty, "yield_time_ms": 0,
                    }),
                ),
            )
            .await?;
        assert!(!start.is_error(), "{start:?}");
        let started = TerminalTaskEntry::from_tool_result_details(&start.metadata.details)?
            .context("start owner receipt")?;
        let id = started.handle.task_id.as_str();
        for (name, args) in [
            (
                "exec_read",
                json!({"execution_id": id, "offset": 0, "include_content": true}),
            ),
            ("exec_input", json!({"execution_id": id, "input": ""})),
            (
                "exec_resize",
                json!({"execution_id": id, "rows": 30, "cols": 90}),
            ),
            ("exec_cancel", json!({"execution_id": id})),
            ("exec_wait", json!({"execution_id": id, "yield_time_ms": 0})),
        ] {
            let result = registry.execute(context.clone(), call(name, args)).await?;
            let entry = TerminalTaskEntry::from_tool_result_details(&result.metadata.details)
                .with_context(|| format!("{name} pty={pty}"))?
                .context("owner receipt")?;
            assert_eq!(entry.handle, started.handle);
            assert!(entry.generation >= started.generation);
            entry.validate_durable()?;
            if name == "exec_read" {
                let nested = TerminalTaskEntry::from_tool_result_details(
                    &result.metadata.details["terminal_task"],
                )?;
                assert_eq!(nested, Some(entry));
                assert_eq!(result.metadata.details["offset"], 0);
            } else if matches!(name, "exec_input" | "exec_resize") {
                assert_eq!(result.metadata.details["supported"], pty);
                assert_eq!(result.is_error(), !pty);
            }
        }
    }
    Ok(())
}

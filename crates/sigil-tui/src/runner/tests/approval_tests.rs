use std::{fs, sync::Arc, time::Duration};

use anyhow::Result;
use sigil_kernel::{
    Agent, ApprovalHandler, ProviderChunk, ReasoningEffort, RunEvent, SessionLogEntry,
    ToolApproval, ToolCall, ToolCategory, ToolPreviewCapability, ToolRegistry, ToolSpec,
};
use sigil_runtime::register_agent_tools_with_workspace;
use tempfile::tempdir;

use super::{
    super::{
        WorkerCommand, WorkerMessage,
        approval_bridge::{ApprovalSignal, ChannelApprovalHandler},
        protocol::{WorkerApprovalCommand, WorkerCommandEnvelope},
    },
    common::{
        ApprovalFlowProvider, PlannedProvider, StreamPlan, WriteTool, spawn_test_worker,
        test_root_config,
    },
};

#[test]
fn approval_decision_is_forwarded_to_active_run() -> Result<()> {
    let temp = tempdir()?;
    let workspace_root = temp.path().to_path_buf();
    let session_log_path = temp.path().join(".sigil/sessions/session-approval.jsonl");
    let root_config = test_root_config(&workspace_root, "approval-flow", "approval-model");
    let mut registry = ToolRegistry::new();
    registry.register(Arc::new(WriteTool));
    let agent = Agent::new(ApprovalFlowProvider, registry);
    let worker = spawn_test_worker(root_config, session_log_path, agent, workspace_root)?;

    worker.send(WorkerCommand::SubmitPrompt {
        prompt: "write".to_owned(),
        reasoning_effort: ReasoningEffort::Max,
    })?;
    let _ = worker.recv_until(|message| matches!(message, WorkerMessage::RunStarted { .. }))?;
    let approval_request = worker.recv_until(|message| {
        matches!(
            message,
            WorkerMessage::Event(event)
                if matches!(event.as_ref(), RunEvent::ToolApprovalRequested { call, .. } if call.id == "call-1")
        )
    })?;
    assert!(matches!(
        approval_request,
        WorkerMessage::Event(event)
            if matches!(event.as_ref(), RunEvent::ToolApprovalRequested { call, .. } if call.id == "call-1")
    ));

    worker.send(WorkerCommand::ApprovalDecision {
        call_id: "call-1".to_owned(),
        approved: true,
    })?;

    let approval_resolved = worker.recv_until(|message| {
        matches!(
            message,
            WorkerMessage::Event(event)
                if matches!(event.as_ref(), RunEvent::ToolApprovalResolved { call_id, approved, .. } if call_id == "call-1" && *approved)
        )
    })?;
    assert!(matches!(
        approval_resolved,
        WorkerMessage::Event(event)
            if matches!(event.as_ref(), RunEvent::ToolApprovalResolved { call_id, approved, .. } if call_id == "call-1" && *approved)
    ));

    let finished =
        worker.recv_until(|message| matches!(message, WorkerMessage::RunFinished { .. }))?;
    let WorkerMessage::RunFinished { result, entries } = finished else {
        panic!("expected run finished");
    };
    assert_eq!(result.final_text, "approved run finished");
    assert_eq!(result.tool_calls, 1);
    let tool_result_message = entries
        .iter()
        .find_map(|entry| match entry {
            SessionLogEntry::ToolResult(message) => message.content.as_deref(),
            _ => None,
        })
        .expect("expected tool result session message");
    let envelope: serde_json::Value = serde_json::from_str(tool_result_message)?;
    assert_eq!(envelope["status"], "ok");
    assert_eq!(envelope["content"], "wrote file");

    worker.shutdown()?;
    Ok(())
}

#[test]
fn approval_command_envelope_ignores_duplicate_command_ids() -> Result<()> {
    let temp = tempdir()?;
    let workspace_root = temp.path().to_path_buf();
    let session_log_path = temp
        .path()
        .join(".sigil/sessions/session-approval-command.jsonl");
    let root_config = test_root_config(&workspace_root, "approval-flow", "approval-model");
    let mut registry = ToolRegistry::new();
    registry.register(Arc::new(WriteTool));
    let agent = Agent::new(ApprovalFlowProvider, registry);
    let worker = spawn_test_worker(root_config, session_log_path, agent, workspace_root)?;

    worker.send(WorkerCommand::SubmitPrompt {
        prompt: "write".to_owned(),
        reasoning_effort: ReasoningEffort::Max,
    })?;
    let _ = worker.recv_until(|message| matches!(message, WorkerMessage::RunStarted { .. }))?;
    let _ = worker.recv_until(|message| {
        matches!(
            message,
            WorkerMessage::Event(event)
                if matches!(event.as_ref(), RunEvent::ToolApprovalRequested { call, .. } if call.id == "call-1")
        )
    })?;

    worker.send(approval_command("command-approval-1"))?;
    worker.send(approval_command("command-approval-1"))?;

    let duplicate_notice = worker.recv_until(|message| {
        matches!(message, WorkerMessage::Notice(notice) if notice.contains("duplicate command command-approval-1 ignored"))
    })?;
    assert!(matches!(duplicate_notice, WorkerMessage::Notice(_)));

    let finished =
        worker.recv_until(|message| matches!(message, WorkerMessage::RunFinished { .. }))?;
    let WorkerMessage::RunFinished { result, .. } = finished else {
        panic!("expected run finished");
    };
    assert_eq!(result.tool_calls, 1);

    worker.shutdown()?;
    Ok(())
}

#[test]
fn spawn_agent_tool_request_surfaces_approval_preview_in_worker() -> Result<()> {
    let temp = tempdir()?;
    let workspace_root = temp.path().to_path_buf();
    let agent_dir = workspace_root
        .join(".sigil")
        .join("agents")
        .join("review-required");
    fs::create_dir_all(&agent_dir)?;
    fs::write(
        agent_dir.join("agent.toml"),
        r#"
description = "Review-required test agent."
instructions = "Inspect the workspace."
invocation_policy = "model_allowed"
allowed_tools = ["grep"]
"#,
    )?;
    let session_log_path = temp
        .path()
        .join(".sigil/sessions/session-agent-approval.jsonl");
    let root_config = test_root_config(&workspace_root, "planned", "planned-model");
    let mut registry = ToolRegistry::new();
    register_agent_tools_with_workspace(&mut registry, &root_config, &workspace_root)?;
    let agent = Agent::new(
        PlannedProvider::new(vec![
            StreamPlan::Chunks(vec![
                ProviderChunk::ToolCallComplete(ToolCall {
                    id: "call-spawn-agent".to_owned(),
                    name: sigil_runtime::SPAWN_AGENT_TOOL_NAME.to_owned(),
                    args_json: serde_json::json!({
                        "profile_id": "review-required",
                        "objective": "inspect tui worker",
                        "prompt": "inspect tui worker",
                        "mode": "join_before_final"
                    })
                    .to_string(),
                }),
                ProviderChunk::Done,
            ]),
            StreamPlan::Chunks(vec![
                ProviderChunk::TextDelta("spawn denied and handled".to_owned()),
                ProviderChunk::Done,
            ]),
        ]),
        registry,
    );
    let worker = spawn_test_worker(root_config, session_log_path, agent, workspace_root)?;

    worker.send(WorkerCommand::SubmitPrompt {
        prompt: "use a sub agent".to_owned(),
        reasoning_effort: ReasoningEffort::Max,
    })?;
    let _ = worker.recv_until(|message| matches!(message, WorkerMessage::RunStarted { .. }))?;
    let approval_request = worker.recv_until(|message| {
        matches!(
            message,
            WorkerMessage::Event(event)
                if matches!(
                    event.as_ref(),
                    RunEvent::ToolApprovalRequested { call, preview, .. }
                        if call.id == "call-spawn-agent"
                            && call.name == sigil_runtime::SPAWN_AGENT_TOOL_NAME
                            && preview.as_ref().is_some_and(|preview| preview.body.contains("budget:"))
                )
        )
    })?;
    assert!(matches!(
        approval_request,
        WorkerMessage::Event(event)
            if matches!(
                event.as_ref(),
                RunEvent::ToolApprovalRequested { call, preview, .. }
                    if call.id == "call-spawn-agent"
                        && preview.as_ref().is_some_and(|preview| preview.body.contains("mode: join_before_final"))
            )
    ));

    worker.send(WorkerCommand::ApprovalDecision {
        call_id: "call-spawn-agent".to_owned(),
        approved: false,
    })?;
    let finished =
        worker.recv_until(|message| matches!(message, WorkerMessage::RunFinished { .. }))?;
    let WorkerMessage::RunFinished { result, entries } = finished else {
        panic!("expected run finished");
    };
    assert_eq!(result.final_text, "spawn denied and handled");
    assert!(entries.iter().any(|entry| {
        matches!(
            entry,
            SessionLogEntry::ToolResult(message)
                if message.tool_call_id.as_deref() == Some("call-spawn-agent")
                    && message.content.as_deref().is_some_and(|content| {
                        content.contains("approval_denied")
                            || content.contains("tool execution denied by user")
                    })
        )
    }));

    worker.shutdown()?;
    Ok(())
}

fn approval_command(command_id: &str) -> WorkerCommand {
    WorkerCommand::ApprovalCommand(WorkerCommandEnvelope::new(
        command_id,
        "sigil-tui-test",
        "session-test",
        WorkerApprovalCommand::Decision {
            call_id: "call-1".to_owned(),
            approved: true,
        },
    ))
}

#[test]
fn approval_handler_denies_when_decision_channel_stays_idle() -> Result<()> {
    let (_tx, rx) = std::sync::mpsc::channel::<ApprovalSignal>();
    let mut handler = ChannelApprovalHandler::with_timeout(rx, Duration::from_millis(1));
    let approval = handler.approve_tool_call(
        &ToolCall {
            id: "call-1".to_owned(),
            name: "write_file".to_owned(),
            args_json: "{}".to_owned(),
        },
        &ToolSpec {
            name: "write_file".to_owned(),
            description: "write".to_owned(),
            input_schema: serde_json::json!({"type":"object"}),
            category: ToolCategory::File,
            access: sigil_kernel::ToolAccess::Write,
            preview: ToolPreviewCapability::Required,
        },
    )?;

    assert!(matches!(
        approval,
        ToolApproval::Deny { reason } if reason.contains("approval timed out")
    ));
    Ok(())
}

#[test]
fn approval_handler_with_zero_timeout_denies_immediately() -> Result<()> {
    let (_tx, rx) = std::sync::mpsc::channel::<ApprovalSignal>();
    let mut handler = ChannelApprovalHandler::with_timeout(rx, Duration::ZERO);
    let approval = handler.approve_tool_call(
        &ToolCall {
            id: "call-1".to_owned(),
            name: "write_file".to_owned(),
            args_json: "{}".to_owned(),
        },
        &ToolSpec {
            name: "write_file".to_owned(),
            description: "write".to_owned(),
            input_schema: serde_json::json!({"type":"object"}),
            category: ToolCategory::File,
            access: sigil_kernel::ToolAccess::Write,
            preview: ToolPreviewCapability::Required,
        },
    )?;

    assert!(matches!(
        approval,
        ToolApproval::Deny { reason } if reason == "approval timed out after 0 seconds"
    ));
    Ok(())
}

#[test]
fn approval_handler_ignores_other_call_ids_until_matching_decision_arrives() -> Result<()> {
    let (tx, rx) = std::sync::mpsc::channel::<ApprovalSignal>();
    let mut handler = ChannelApprovalHandler::with_timeout(rx, Duration::from_secs(1));
    tx.send(ApprovalSignal::Decision {
        call_id: "other-call".to_owned(),
        approval: ToolApproval::Deny {
            reason: "wrong call".to_owned(),
        },
    })?;
    tx.send(ApprovalSignal::Decision {
        call_id: "call-1".to_owned(),
        approval: ToolApproval::Approve,
    })?;

    let approval = handler.approve_tool_call(
        &ToolCall {
            id: "call-1".to_owned(),
            name: "write_file".to_owned(),
            args_json: "{}".to_owned(),
        },
        &ToolSpec {
            name: "write_file".to_owned(),
            description: "write".to_owned(),
            input_schema: serde_json::json!({"type":"object"}),
            category: ToolCategory::File,
            access: sigil_kernel::ToolAccess::Write,
            preview: ToolPreviewCapability::Required,
        },
    )?;

    assert!(matches!(approval, ToolApproval::Approve));
    Ok(())
}

#[test]
fn approval_handler_forwards_approved_argument_overrides() -> Result<()> {
    let (tx, rx) = std::sync::mpsc::channel::<ApprovalSignal>();
    let mut handler = ChannelApprovalHandler::with_timeout(rx, Duration::from_secs(1));
    tx.send(ApprovalSignal::Decision {
        call_id: "call-spawn".to_owned(),
        approval: ToolApproval::ApproveWithArgs {
            args_json: r#"{"mode":"background"}"#.to_owned(),
        },
    })?;

    let approval = handler.approve_tool_call(
        &ToolCall {
            id: "call-spawn".to_owned(),
            name: sigil_runtime::SPAWN_AGENT_TOOL_NAME.to_owned(),
            args_json: "{}".to_owned(),
        },
        &ToolSpec {
            name: sigil_runtime::SPAWN_AGENT_TOOL_NAME.to_owned(),
            description: "spawn".to_owned(),
            input_schema: serde_json::json!({"type":"object"}),
            category: ToolCategory::Agent,
            access: sigil_kernel::ToolAccess::Execute,
            preview: ToolPreviewCapability::Required,
        },
    )?;

    assert!(matches!(
        approval,
        ToolApproval::ApproveWithArgs { args_json } if args_json.contains("background")
    ));
    Ok(())
}

#[test]
fn approval_denial_is_forwarded_to_active_run() -> Result<()> {
    let temp = tempdir()?;
    let workspace_root = temp.path().to_path_buf();
    let session_log_path = temp
        .path()
        .join(".sigil/sessions/session-approval-deny.jsonl");
    let root_config = test_root_config(&workspace_root, "approval-flow", "approval-model");
    let mut registry = ToolRegistry::new();
    registry.register(Arc::new(WriteTool));
    let agent = Agent::new(ApprovalFlowProvider, registry);
    let worker = spawn_test_worker(root_config, session_log_path, agent, workspace_root)?;

    worker.send(WorkerCommand::SubmitPrompt {
        prompt: "write".to_owned(),
        reasoning_effort: ReasoningEffort::Max,
    })?;
    let _ = worker.recv_until(|message| matches!(message, WorkerMessage::RunStarted { .. }))?;
    let _ = worker.recv_until(|message| {
        matches!(
            message,
            WorkerMessage::Event(event)
                if matches!(event.as_ref(), RunEvent::ToolApprovalRequested { call, .. } if call.id == "call-1")
        )
    })?;

    worker.send(WorkerCommand::ApprovalDecision {
        call_id: "call-1".to_owned(),
        approved: false,
    })?;

    let denied = worker.recv_until(|message| {
        matches!(
            message,
            WorkerMessage::Event(event)
                if matches!(event.as_ref(), RunEvent::ToolApprovalResolved { call_id, approved, reason } if call_id == "call-1" && !approved && reason.as_deref() == Some("denied in TUI"))
        )
    })?;
    assert!(matches!(
        denied,
        WorkerMessage::Event(event)
            if matches!(event.as_ref(), RunEvent::ToolApprovalResolved { call_id, approved, reason } if call_id == "call-1" && !approved && reason.as_deref() == Some("denied in TUI"))
    ));

    let tool_result = worker.recv_until(|message| {
        matches!(
            message,
            WorkerMessage::Event(event)
                if matches!(event.as_ref(), RunEvent::ToolResult(result) if result.is_error())
        )
    })?;
    assert!(matches!(
        tool_result,
        WorkerMessage::Event(event)
            if matches!(event.as_ref(), RunEvent::ToolResult(result) if result.is_error())
    ));

    worker.shutdown()?;
    Ok(())
}

#[test]
fn approval_handler_returns_cancel_denial() -> Result<()> {
    let (tx, rx) = std::sync::mpsc::channel::<ApprovalSignal>();
    let mut handler = ChannelApprovalHandler::with_timeout(rx, Duration::from_secs(1));
    tx.send(ApprovalSignal::Cancel)?;

    let approval = handler.approve_tool_call(
        &ToolCall {
            id: "call-1".to_owned(),
            name: "write_file".to_owned(),
            args_json: "{}".to_owned(),
        },
        &ToolSpec {
            name: "write_file".to_owned(),
            description: "write".to_owned(),
            input_schema: serde_json::json!({"type":"object"}),
            category: ToolCategory::File,
            access: sigil_kernel::ToolAccess::Write,
            preview: ToolPreviewCapability::Required,
        },
    )?;

    assert!(matches!(
        approval,
        ToolApproval::Deny { reason } if reason == "run cancelled from TUI"
    ));
    Ok(())
}

#[test]
fn approval_handler_errors_when_channel_closes() {
    let (tx, rx) = std::sync::mpsc::channel::<ApprovalSignal>();
    drop(tx);
    let mut handler = ChannelApprovalHandler::with_timeout(rx, Duration::from_secs(1));

    let error = handler
        .approve_tool_call(
            &ToolCall {
                id: "call-1".to_owned(),
                name: "write_file".to_owned(),
                args_json: "{}".to_owned(),
            },
            &ToolSpec {
                name: "write_file".to_owned(),
                description: "write".to_owned(),
                input_schema: serde_json::json!({"type":"object"}),
                category: ToolCategory::File,
                access: sigil_kernel::ToolAccess::Write,
                preview: ToolPreviewCapability::Required,
            },
        )
        .expect_err("closed decision channel should fail");

    assert!(error.to_string().contains("approval channel closed"));
}

#[test]
fn approval_decision_without_active_run_reports_error() -> Result<()> {
    let temp = tempdir()?;
    let workspace_root = temp.path().to_path_buf();
    let session_log_path = temp
        .path()
        .join(".sigil/sessions/session-stray-approval.jsonl");
    let root_config = test_root_config(&workspace_root, "planned", "planned-model");
    let provider = PlannedProvider::new(vec![]);
    let agent = Agent::new(provider, ToolRegistry::new());
    let worker = spawn_test_worker(root_config, session_log_path, agent, workspace_root)?;

    worker.send(WorkerCommand::ApprovalDecision {
        call_id: "missing-call".to_owned(),
        approved: true,
    })?;
    let error = worker.recv_until(|message| matches!(message, WorkerMessage::RunFailed(_)))?;
    assert!(matches!(
        error,
        WorkerMessage::RunFailed(ref text)
            if text == "received stray approval decision without pending approval"
    ));

    worker.shutdown()?;
    Ok(())
}

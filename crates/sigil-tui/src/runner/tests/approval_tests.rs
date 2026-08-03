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
    let session_id = session_log_path.display().to_string();
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
        &approval_request,
        WorkerMessage::Event(event)
            if matches!(event.as_ref(), RunEvent::ToolApprovalRequested { call, .. } if call.id == "call-1")
    ));

    worker.send(approval_command(
        "command-approval-once",
        &session_id,
        "call-1",
        approval_request_id(&approval_request),
        true,
    ))?;

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
    let tool_result = entries
        .iter()
        .find_map(|entry| match entry {
            SessionLogEntry::ToolResultV2(result) => Some(result),
            _ => None,
        })
        .expect("expected tool result session message");
    assert_eq!(tool_result.facts.status, "ok");
    assert_eq!(tool_result.initial_model_view.preview, "wrote file");

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
    let session_id = session_log_path.display().to_string();
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
    let request = worker.recv_until(|message| {
        matches!(
            message,
            WorkerMessage::Event(event)
                if matches!(event.as_ref(), RunEvent::ToolApprovalRequested { call, .. } if call.id == "call-1")
        )
    })?;
    let approval_request_id = approval_request_id(&request).to_owned();

    worker.send(approval_command(
        "command-approval-1",
        &session_id,
        "call-1",
        &approval_request_id,
        true,
    ))?;
    worker.send(approval_command(
        "command-approval-1",
        &session_id,
        "call-1",
        &approval_request_id,
        true,
    ))?;

    let replayed_receipt = worker.recv_until(|message| {
        matches!(
            message,
            WorkerMessage::ApprovalCommandReceipt(receipt)
                if receipt.command_id == "command-approval-1" && receipt.replayed
        )
    })?;
    assert!(matches!(
        replayed_receipt,
        WorkerMessage::ApprovalCommandReceipt(receipt)
            if receipt.approval_request_id == approval_request_id
                && receipt.route_state
                    == crate::runner::WorkerApprovalRouteState::DecisionAccepted
    ));

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
    let session_id = session_log_path.display().to_string();
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
        &approval_request,
        WorkerMessage::Event(event)
            if matches!(
                event.as_ref(),
                RunEvent::ToolApprovalRequested { call, preview, .. }
                    if call.id == "call-spawn-agent"
                        && preview.as_ref().is_some_and(|preview| preview.body.contains("mode: join_before_final"))
            )
    ));

    worker.send(approval_command(
        "command-deny-spawn-agent",
        &session_id,
        "call-spawn-agent",
        approval_request_id(&approval_request),
        false,
    ))?;
    let finished =
        worker.recv_until(|message| matches!(message, WorkerMessage::RunFinished { .. }))?;
    let WorkerMessage::RunFinished { result, entries } = finished else {
        panic!("expected run finished");
    };
    assert_eq!(result.final_text, "spawn denied and handled");
    assert!(entries.iter().any(|entry| {
        matches!(
            entry,
            SessionLogEntry::ToolResultV2(result)
                if result.call_id == "call-spawn-agent"
                    && result.facts.error.as_ref().is_some_and(|error| {
                        error.kind == sigil_kernel::ToolErrorKind::ApprovalDenied
                            || error.message.contains("tool execution denied by user")
                    })
        )
    }));

    worker.shutdown()?;
    Ok(())
}

fn approval_command(
    command_id: &str,
    session_id: &str,
    call_id: &str,
    approval_request_id: &str,
    approved: bool,
) -> WorkerCommand {
    WorkerCommand::ApprovalCommand(WorkerCommandEnvelope::new(
        command_id,
        "sigil-tui-test",
        session_id,
        WorkerApprovalCommand::Decision {
            call_id: call_id.to_owned(),
            approval_request_id: approval_request_id.to_owned(),
            approved,
        },
    ))
}

fn approval_request_id(message: &WorkerMessage) -> &str {
    let WorkerMessage::Event(event) = message else {
        panic!("expected approval request event");
    };
    let RunEvent::ToolApprovalRequested {
        approval_identity, ..
    } = event.as_ref()
    else {
        panic!("expected approval request event");
    };
    &approval_identity.approval_request_id
}

#[test]
fn approval_handler_waits_for_an_explicit_decision_without_an_idle_timeout() -> Result<()> {
    let (tx, rx) = std::sync::mpsc::channel::<ApprovalSignal>();
    let mut handler = ChannelApprovalHandler::new(rx);
    let sender = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(20));
        let (acknowledgement_tx, _acknowledgement_rx) = std::sync::mpsc::sync_channel(1);
        tx.send(ApprovalSignal::Decision {
            call_id: "call-1".to_owned(),
            approval_request_id: "approval-1".to_owned(),
            approval: ToolApproval::Approve,
            acknowledgement_tx,
        })
    });
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
            network_effect: None,
            preview: ToolPreviewCapability::Required,
        },
    )?;

    sender
        .join()
        .expect("approval sender thread should not panic")?;
    assert!(matches!(approval, ToolApproval::Approve));
    Ok(())
}

#[test]
fn approval_handler_ignores_other_call_ids_until_matching_decision_arrives() -> Result<()> {
    let (tx, rx) = std::sync::mpsc::channel::<ApprovalSignal>();
    let mut handler = ChannelApprovalHandler::new(rx);
    let (wrong_ack_tx, _wrong_ack_rx) = std::sync::mpsc::sync_channel(1);
    tx.send(ApprovalSignal::Decision {
        call_id: "other-call".to_owned(),
        approval_request_id: "approval-other".to_owned(),
        approval: ToolApproval::Deny {
            reason: "wrong call".to_owned(),
        },
        acknowledgement_tx: wrong_ack_tx,
    })?;
    let (matching_ack_tx, _matching_ack_rx) = std::sync::mpsc::sync_channel(1);
    tx.send(ApprovalSignal::Decision {
        call_id: "call-1".to_owned(),
        approval_request_id: "approval-1".to_owned(),
        approval: ToolApproval::Approve,
        acknowledgement_tx: matching_ack_tx,
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
            network_effect: None,
            preview: ToolPreviewCapability::Required,
        },
    )?;

    assert!(matches!(approval, ToolApproval::Approve));
    Ok(())
}

#[test]
fn approval_handler_forwards_approved_argument_overrides() -> Result<()> {
    let (tx, rx) = std::sync::mpsc::channel::<ApprovalSignal>();
    let mut handler = ChannelApprovalHandler::new(rx);
    let (acknowledgement_tx, _acknowledgement_rx) = std::sync::mpsc::sync_channel(1);
    tx.send(ApprovalSignal::Decision {
        call_id: "call-spawn".to_owned(),
        approval_request_id: "approval-spawn".to_owned(),
        approval: ToolApproval::ApproveWithArgs {
            args_json: r#"{"mode":"background"}"#.to_owned(),
        },
        acknowledgement_tx,
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
            network_effect: None,
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
fn approval_handler_rejects_stale_request_id_before_accepting_exact_decision() -> Result<()> {
    let (tx, rx) = std::sync::mpsc::channel::<ApprovalSignal>();
    let mut handler = ChannelApprovalHandler::new(rx);
    let (stale_ack_tx, stale_ack_rx) = std::sync::mpsc::sync_channel(1);
    tx.send(ApprovalSignal::Decision {
        call_id: "call-1".to_owned(),
        approval_request_id: "approval-stale".to_owned(),
        approval: ToolApproval::Approve,
        acknowledgement_tx: stale_ack_tx,
    })?;
    let (exact_ack_tx, exact_ack_rx) = std::sync::mpsc::sync_channel(1);
    tx.send(ApprovalSignal::Decision {
        call_id: "call-1".to_owned(),
        approval_request_id: "approval-current".to_owned(),
        approval: ToolApproval::Approve,
        acknowledgement_tx: exact_ack_tx,
    })?;
    let call = ToolCall {
        id: "call-1".to_owned(),
        name: "write_file".to_owned(),
        args_json: "{}".to_owned(),
    };
    let spec = ToolSpec {
        name: "write_file".to_owned(),
        description: "write".to_owned(),
        input_schema: serde_json::json!({"type":"object"}),
        category: ToolCategory::File,
        access: sigil_kernel::ToolAccess::Write,
        network_effect: None,
        preview: ToolPreviewCapability::Required,
    };
    let context = sigil_kernel::ToolApprovalContext {
        identity: sigil_kernel::ApprovalRequestIdentityV2 {
            session_id: "session-1".to_owned(),
            run_id: "run-1".to_owned(),
            call_id: call.id.clone(),
            approval_request_id: "approval-current".to_owned(),
            plan_hash: "plan-1".to_owned(),
            policy_version: "policy-1".to_owned(),
            execution_binding_hash: "binding-1".to_owned(),
            expires_at_ms: u64::MAX,
        },
        permission_signature: "permission-1".to_owned(),
        policy_fingerprint: "policy-1".to_owned(),
        requested_at_ms: 1,
        expires_at_ms: u64::MAX,
    };

    let approval = handler.approve_tool_call_with_context(&call, &spec, &context)?;

    assert!(matches!(approval, ToolApproval::Approve));
    assert!(!stale_ack_rx.recv()?.accepted);
    assert!(exact_ack_rx.recv()?.accepted);
    Ok(())
}

#[test]
fn approval_denial_is_forwarded_to_active_run() -> Result<()> {
    let temp = tempdir()?;
    let workspace_root = temp.path().to_path_buf();
    let session_log_path = temp
        .path()
        .join(".sigil/sessions/session-approval-deny.jsonl");
    let session_id = session_log_path.display().to_string();
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

    worker.send(approval_command(
        "command-deny-write",
        &session_id,
        "call-1",
        approval_request_id(&approval_request),
        false,
    ))?;

    let denied = worker.recv_until(|message| {
        matches!(
            message,
            WorkerMessage::Event(event)
                if matches!(event.as_ref(), RunEvent::ToolApprovalResolved { call_id, approved, reason, .. } if call_id == "call-1" && !approved && reason.as_deref() == Some("denied in TUI"))
        )
    })?;
    assert!(matches!(
        denied,
        WorkerMessage::Event(event)
            if matches!(event.as_ref(), RunEvent::ToolApprovalResolved { call_id, approved, reason, .. } if call_id == "call-1" && !approved && reason.as_deref() == Some("denied in TUI"))
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
    let mut handler = ChannelApprovalHandler::new(rx);
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
            network_effect: None,
            preview: ToolPreviewCapability::Required,
        },
    )?;

    assert!(matches!(
        approval,
        ToolApproval::Cancelled { reason } if reason == "run cancelled from TUI"
    ));
    Ok(())
}

#[test]
fn approval_handler_errors_when_channel_closes() {
    let (tx, rx) = std::sync::mpsc::channel::<ApprovalSignal>();
    drop(tx);
    let mut handler = ChannelApprovalHandler::new(rx);

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
                network_effect: None,
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
    let session_id = session_log_path.display().to_string();
    let root_config = test_root_config(&workspace_root, "planned", "planned-model");
    let provider = PlannedProvider::new(vec![]);
    let agent = Agent::new(provider, ToolRegistry::new());
    let worker = spawn_test_worker(root_config, session_log_path, agent, workspace_root)?;

    worker.send(approval_command(
        "command-stray",
        &session_id,
        "missing-call",
        "approval-missing",
        true,
    ))?;
    let error =
        worker.recv_until(|message| matches!(message, WorkerMessage::ApprovalCommandReceipt(_)))?;
    assert!(matches!(
        error,
        WorkerMessage::ApprovalCommandReceipt(ref receipt)
            if receipt.call_id == "missing-call"
                && receipt.route_state == crate::runner::WorkerApprovalRouteState::Rejected
    ));

    worker.shutdown()?;
    Ok(())
}

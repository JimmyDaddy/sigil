use std::{sync::Arc, time::Duration};

use anyhow::Result;
use sigil_kernel::{
    Agent, ApprovalHandler, ReasoningEffort, RunEvent, SessionLogEntry, ToolApproval, ToolCall,
    ToolCategory, ToolPreviewCapability, ToolRegistry, ToolSpec,
};
use tempfile::tempdir;

use super::{
    super::{
        WorkerCommand, WorkerMessage,
        approval_bridge::{ApprovalSignal, ChannelApprovalHandler},
    },
    common::{
        ApprovalFlowProvider, PlannedProvider, WriteTool, spawn_test_worker, test_root_config,
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

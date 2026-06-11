use std::time::Duration;

use anyhow::Result;
use sigil_kernel::{
    Agent, McpServerConfig, McpServerStartup, ProviderChunk, ReasoningEffort, RunEvent,
    SessionLogEntry, ToolRegistry,
};
use tempfile::tempdir;

use super::{
    super::{McpActivationStatus, WorkerCommand, WorkerMessage, spawn_agent_worker},
    common::{PlannedProvider, StreamPlan, spawn_test_worker, test_root_config},
};
#[test]
fn submit_prompt_emits_started_event_and_finished_messages() -> Result<()> {
    let temp = tempdir()?;
    let workspace_root = temp.path().to_path_buf();
    let session_log_path = temp.path().join(".sigil/sessions/session-worker.jsonl");
    let root_config = test_root_config(&workspace_root, "planned", "planned-model");
    let provider = PlannedProvider::new(vec![StreamPlan::Chunks(vec![
        ProviderChunk::TextDelta("hello from worker".to_owned()),
        ProviderChunk::Done,
    ])]);
    let agent = Agent::new(provider, ToolRegistry::new());
    let worker = spawn_test_worker(root_config, session_log_path, agent, workspace_root)?;

    worker.send(WorkerCommand::SubmitPrompt {
        prompt: "hello".to_owned(),
        reasoning_effort: ReasoningEffort::Max,
    })?;
    let started = worker.recv()?;
    assert!(matches!(
        started,
        WorkerMessage::RunStarted { ref prompt } if prompt == "hello"
    ));

    let text_event = worker.recv_until(|message| {
        matches!(
            message,
            WorkerMessage::Event(event)
                if matches!(event.as_ref(), RunEvent::TextDelta(delta) if delta == "hello from worker")
        )
    })?;
    assert!(matches!(
        text_event,
        WorkerMessage::Event(event)
            if matches!(event.as_ref(), RunEvent::TextDelta(delta) if delta == "hello from worker")
    ));

    let finished =
        worker.recv_until(|message| matches!(message, WorkerMessage::RunFinished { .. }))?;
    assert!(matches!(
        finished,
        WorkerMessage::RunFinished { ref result, ref entries }
            if result.final_text == "hello from worker"
                && result.tool_calls == 0
                && entries.iter().any(|entry| matches!(entry, SessionLogEntry::User(message) if message.content.as_deref() == Some("hello")))
    ));

    worker.shutdown()?;
    Ok(())
}

#[test]
fn spawn_agent_worker_reports_provider_configuration_error() -> Result<()> {
    let temp = tempdir()?;
    let workspace_root = temp.path().to_path_buf();
    let session_log_path = temp.path().join(".sigil/sessions/session-worker.jsonl");
    let root_config = test_root_config(&workspace_root, "deepseek", "deepseek-v4-flash");

    let (_command_tx, message_rx) = spawn_agent_worker(
        root_config,
        session_log_path,
        workspace_root,
        sigil_kernel::InteractionMode::Interactive,
    )?;

    let message = message_rx.recv_timeout(Duration::from_secs(3))?;

    assert!(matches!(
        message,
        WorkerMessage::RunFailed(ref error)
            if error.contains("missing [providers.deepseek] in sigil.toml")
    ));
    Ok(())
}

#[test]
fn spawn_agent_worker_reports_required_eager_mcp_startup_error() -> Result<()> {
    let temp = tempdir()?;
    let workspace_root = temp.path().to_path_buf();
    let session_log_path = temp.path().join(".sigil/sessions/session-worker.jsonl");
    let mut root_config = test_root_config(&workspace_root, "deepseek", "deepseek-v4-flash");
    root_config.providers.insert(
        "deepseek".to_owned(),
        serde_json::json!({
            "api_key": "test-key",
            "model": "deepseek-v4-flash"
        }),
    );
    root_config.mcp_servers.push(McpServerConfig {
        name: "required-eager".to_owned(),
        command: "/definitely/missing/sigil-mcp-server".to_owned(),
        startup: McpServerStartup::Eager,
        ..McpServerConfig::default()
    });

    let (_command_tx, message_rx) = spawn_agent_worker(
        root_config,
        session_log_path,
        workspace_root,
        sigil_kernel::InteractionMode::Interactive,
    )?;

    let message = message_rx.recv_timeout(Duration::from_secs(3))?;

    assert!(matches!(
        message,
        WorkerMessage::RunFailed(ref error)
            if error.contains("failed to spawn MCP server required-eager")
    ));
    Ok(())
}

#[test]
fn activate_lazy_mcp_reports_notice_when_no_lazy_servers_match() -> Result<()> {
    let temp = tempdir()?;
    let workspace_root = temp.path().to_path_buf();
    let session_log_path = temp.path().join(".sigil/sessions/session-worker.jsonl");
    let root_config = test_root_config(&workspace_root, "planned", "planned-model");
    let provider = PlannedProvider::new(Vec::new());
    let agent = Agent::new(provider, ToolRegistry::new());
    let worker = spawn_test_worker(root_config, session_log_path, agent, workspace_root)?;

    worker.send(WorkerCommand::ActivateLazyMcp {
        server_name: Some("missing".to_owned()),
    })?;
    let status = worker.recv_until(|message| {
        matches!(
            message,
            WorkerMessage::McpActivationStatus {
                status: McpActivationStatus::Deferred,
                ..
            }
        )
    })?;
    assert!(matches!(
        status,
        WorkerMessage::McpActivationStatus {
            server_name: Some(ref server_name),
            status: McpActivationStatus::Deferred,
        } if server_name == "missing"
    ));
    let notice = worker.recv_until(|message| matches!(message, WorkerMessage::Notice(_)))?;

    assert!(matches!(
        notice,
        WorkerMessage::Notice(ref text) if text == "no lazy MCP tools activated for missing"
    ));

    worker.shutdown()?;
    Ok(())
}

#[test]
fn activate_lazy_mcp_is_rejected_while_run_is_active() -> Result<()> {
    let temp = tempdir()?;
    let workspace_root = temp.path().to_path_buf();
    let session_log_path = temp.path().join(".sigil/sessions/session-worker.jsonl");
    let root_config = test_root_config(&workspace_root, "planned", "planned-model");
    let provider = PlannedProvider::new(vec![StreamPlan::Pending]);
    let agent = Agent::new(provider, ToolRegistry::new());
    let worker = spawn_test_worker(root_config, session_log_path, agent, workspace_root)?;

    worker.send(WorkerCommand::SubmitPrompt {
        prompt: "hold".to_owned(),
        reasoning_effort: ReasoningEffort::Max,
    })?;
    let _ = worker.recv_until(|message| matches!(message, WorkerMessage::RunStarted { .. }))?;
    worker.send(WorkerCommand::ActivateLazyMcp { server_name: None })?;
    let failure = worker.recv_until(|message| matches!(message, WorkerMessage::RunFailed(_)))?;

    assert!(matches!(
        failure,
        WorkerMessage::RunFailed(ref error)
            if error == "cannot activate MCP while the agent is running"
    ));

    worker.shutdown()?;
    Ok(())
}

#[test]
fn activate_lazy_mcp_reports_failed_status_for_required_server_error() -> Result<()> {
    let temp = tempdir()?;
    let workspace_root = temp.path().to_path_buf();
    let session_log_path = temp.path().join(".sigil/sessions/session-worker.jsonl");
    let mut root_config = test_root_config(&workspace_root, "planned", "planned-model");
    root_config.mcp_servers.push(McpServerConfig {
        name: "required-lazy".to_owned(),
        command: "/definitely/missing/sigil-mcp-server".to_owned(),
        startup: McpServerStartup::Lazy,
        ..McpServerConfig::default()
    });
    let provider = PlannedProvider::new(Vec::new());
    let agent = Agent::new(provider, ToolRegistry::new());
    let worker = spawn_test_worker(root_config, session_log_path, agent, workspace_root)?;

    worker.send(WorkerCommand::ActivateLazyMcp {
        server_name: Some("required-lazy".to_owned()),
    })?;
    let status = worker.recv_until(|message| {
        matches!(
            message,
            WorkerMessage::McpActivationStatus {
                status: McpActivationStatus::Failed { .. },
                ..
            }
        )
    })?;

    assert!(matches!(
        status,
        WorkerMessage::McpActivationStatus {
            server_name: Some(ref server_name),
            status: McpActivationStatus::Failed { ref error },
        } if server_name == "required-lazy" && error.contains("failed to spawn MCP server required-lazy")
    ));

    worker.shutdown()?;
    Ok(())
}

#[test]
fn check_changed_files_diagnostics_is_rejected_while_run_is_active() -> Result<()> {
    let temp = tempdir()?;
    let workspace_root = temp.path().to_path_buf();
    let session_log_path = temp.path().join(".sigil/sessions/session-worker.jsonl");
    let root_config = test_root_config(&workspace_root, "planned", "planned-model");
    let provider = PlannedProvider::new(vec![StreamPlan::Pending]);
    let agent = Agent::new(provider, ToolRegistry::new());
    let worker = spawn_test_worker(root_config, session_log_path, agent, workspace_root)?;

    worker.send(WorkerCommand::SubmitPrompt {
        prompt: "hold".to_owned(),
        reasoning_effort: ReasoningEffort::Max,
    })?;
    let _ = worker.recv_until(|message| matches!(message, WorkerMessage::RunStarted { .. }))?;
    worker.send(WorkerCommand::CheckChangedFilesDiagnostics)?;
    let failure = worker.recv_until(|message| matches!(message, WorkerMessage::RunFailed(_)))?;

    assert!(matches!(
        failure,
        WorkerMessage::RunFailed(ref error)
            if error == "cannot check changes while the agent is running"
    ));

    worker.shutdown()?;
    Ok(())
}

#[test]
fn spawn_agent_worker_reports_provider_build_failure() -> Result<()> {
    let temp = tempdir()?;
    let workspace_root = temp.path().to_path_buf();
    let session_log_path = temp
        .path()
        .join(".sigil/sessions/session-spawn-failure.jsonl");
    let root_config = test_root_config(&workspace_root, "missing-provider", "planned-model");

    let (_command_tx, message_rx) = spawn_agent_worker(
        root_config,
        session_log_path,
        workspace_root,
        sigil_kernel::InteractionMode::Interactive,
    )?;
    let failure = message_rx.recv_timeout(std::time::Duration::from_secs(3))?;

    assert!(matches!(
        failure,
        WorkerMessage::RunFailed(ref error)
            if error.contains("unsupported provider missing-provider")
    ));
    Ok(())
}

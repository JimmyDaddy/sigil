use std::{fs, time::Duration};

use anyhow::Result;
use sigil_kernel::{
    Agent, AgentRole, McpServerConfig, McpServerStartup, ProviderChunk, ReasoningEffort, RunEvent,
    SessionLogEntry, SkillDescriptor, SkillRunMode, SkillSource, SkillTrustState, ToolCall,
    ToolErrorKind, ToolRegistry, ToolResultStatus,
};
use tempfile::tempdir;

use super::{
    super::{
        McpActivationStatus, WorkerCommand, WorkerMessage, spawn_agent_worker,
        worker_loop::skill_child_agent_role,
    },
    common::{PlannedProvider, StreamPlan, WriteTool, spawn_test_worker, test_root_config},
};

fn write_workspace_skill(workspace_root: &std::path::Path, id: &str, body: &str) -> Result<()> {
    let path = workspace_root
        .join(".sigil")
        .join("skills")
        .join(id)
        .join("SKILL.md");
    fs::create_dir_all(path.parent().expect("skill path should have parent"))?;
    fs::write(path, body)?;
    Ok(())
}

fn test_child_session_skill(agent: Option<&str>) -> SkillDescriptor {
    SkillDescriptor {
        id: "reviewer".to_owned(),
        name: "Reviewer".to_owned(),
        description: "Review current changes.".to_owned(),
        when_to_use: None,
        root: ".sigil/agents/reviewer.md".into(),
        entrypoint: ".sigil/agents/reviewer.md".into(),
        source: SkillSource::Workspace,
        sha256: "hash".to_owned(),
        enabled: true,
        trust: SkillTrustState::Trusted,
        model_invocable: true,
        user_invocable: true,
        run_as: SkillRunMode::ChildSession,
        agent: agent.map(str::to_owned),
        argument_hint: None,
        allowed_tools: Default::default(),
        disallowed_tools: Default::default(),
        path_patterns: Vec::new(),
    }
}

fn write_fake_server_script(path: &std::path::Path) -> Result<()> {
    fs::write(
        path,
        r#"#!/usr/bin/env python3
import json, sys

def read_message():
    headers = {}
    while True:
        line = sys.stdin.buffer.readline()
        if not line:
            return None
        if line in (b"\r\n", b"\n"):
            break
        key, value = line.decode().split(":", 1)
        headers[key.lower()] = value.strip()
    length = int(headers["content-length"])
    body = sys.stdin.buffer.read(length)
    return json.loads(body.decode())

def write_message(obj):
    body = json.dumps(obj).encode()
    sys.stdout.buffer.write(f"Content-Length: {len(body)}\r\n\r\n".encode())
    sys.stdout.buffer.write(body)
    sys.stdout.buffer.flush()

while True:
    message = read_message()
    if message is None:
        break
    method = message.get("method")
    if method == "initialize":
        write_message({"jsonrpc":"2.0","id":message["id"],"result":{"protocolVersion":"2024-11-05","serverInfo":{"name":"fake","version":"1.0.0"},"capabilities":{}}})
    elif method == "notifications/initialized":
        pass
    elif method == "tools/list":
        write_message({"jsonrpc":"2.0","id":message["id"],"result":{"tools":[{"name":"echo","description":"Echo","inputSchema":{"type":"object","properties":{"value":{"type":"string"}},"required":["value"]}}]}})
"#,
    )?;
    Ok(())
}

#[test]
fn child_session_skill_agent_hint_defaults_to_read_role() {
    assert_eq!(
        skill_child_agent_role(&test_child_session_skill(None)),
        AgentRole::SubagentRead
    );
    assert_eq!(
        skill_child_agent_role(&test_child_session_skill(Some("reviewer"))),
        AgentRole::SubagentRead
    );
    assert_eq!(
        skill_child_agent_role(&test_child_session_skill(Some("subagent-write"))),
        AgentRole::SubagentWrite
    );
    assert_eq!(
        skill_child_agent_role(&test_child_session_skill(Some("writer"))),
        AgentRole::SubagentWrite
    );
}
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
fn inline_skill_invocation_applies_skill_tool_scope() -> Result<()> {
    let temp = tempdir()?;
    let workspace_root = temp.path().to_path_buf();
    write_workspace_skill(
        &workspace_root,
        "readonly",
        r#"---
name: readonly
description: Read only skill.
trust: trusted
user-invocable: true
run-as: inline
disallowed-tools: [write_file]
---

# Readonly
"#,
    )?;
    let session_log_path = temp
        .path()
        .join(".sigil/sessions/session-inline-skill.jsonl");
    let root_config = test_root_config(&workspace_root, "planned", "planned-model");
    let provider = PlannedProvider::new(vec![
        StreamPlan::Chunks(vec![
            ProviderChunk::ToolCallStart {
                id: "call-write".to_owned(),
                name: "write_file".to_owned(),
            },
            ProviderChunk::ToolCallArgsDelta {
                id: "call-write".to_owned(),
                delta: r#"{"path":"note.txt"}"#.to_owned(),
            },
            ProviderChunk::ToolCallComplete(ToolCall {
                id: "call-write".to_owned(),
                name: "write_file".to_owned(),
                args_json: r#"{"path":"note.txt"}"#.to_owned(),
            }),
            ProviderChunk::Done,
        ]),
        StreamPlan::Chunks(vec![
            ProviderChunk::TextDelta("done".to_owned()),
            ProviderChunk::Done,
        ]),
    ]);
    let mut registry = ToolRegistry::new();
    registry.register(std::sync::Arc::new(WriteTool));
    let agent = Agent::new(provider, registry);
    let worker = spawn_test_worker(root_config, session_log_path, agent, workspace_root)?;

    worker.send(WorkerCommand::InvokeInlineSkill {
        skill_id: "readonly".to_owned(),
        arguments: "target".to_owned(),
        reasoning_effort: ReasoningEffort::Medium,
    })?;
    let _ = worker.recv_until(|message| matches!(message, WorkerMessage::RunStarted { .. }))?;
    let tool_error = worker.recv_until(|message| {
        matches!(
            message,
            WorkerMessage::Event(event)
                if matches!(
                    event.as_ref(),
                    RunEvent::ToolResult(result)
                        if matches!(
                            &result.status,
                            ToolResultStatus::Error(error)
                                if error.kind == ToolErrorKind::Internal
                                    && error.message.contains("not available in this role scope")
                        )
                )
        )
    })?;
    assert!(matches!(tool_error, WorkerMessage::Event(_)));
    let _ = worker.recv_until(|message| matches!(message, WorkerMessage::RunFinished { .. }))?;

    worker.shutdown()?;
    Ok(())
}

#[test]
fn worker_revalidates_skill_run_mode_after_loading() -> Result<()> {
    let temp = tempdir()?;
    let workspace_root = temp.path().to_path_buf();
    write_workspace_skill(
        &workspace_root,
        "child-only",
        r#"---
name: child-only
description: Child only skill.
trust: trusted
user-invocable: true
run-as: child-session
---

# Child Only
"#,
    )?;
    let session_log_path = temp.path().join(".sigil/sessions/session-skill-mode.jsonl");
    let root_config = test_root_config(&workspace_root, "planned", "planned-model");
    let provider = PlannedProvider::new(Vec::new());
    let agent = Agent::new(provider, ToolRegistry::new());
    let worker = spawn_test_worker(root_config, session_log_path, agent, workspace_root)?;

    worker.send(WorkerCommand::InvokeInlineSkill {
        skill_id: "child-only".to_owned(),
        arguments: String::new(),
        reasoning_effort: ReasoningEffort::Medium,
    })?;
    let failure = worker.recv_until(|message| matches!(message, WorkerMessage::RunFailed(_)))?;

    match failure {
        WorkerMessage::RunFailed(error) => {
            assert!(
                error.contains("child_session mode, not inline mode"),
                "{error}"
            );
        }
        other => panic!("expected run failure, got {other:?}"),
    }

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
fn spawn_agent_worker_reports_eager_mcp_failure_without_stopping_worker() -> Result<()> {
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

    let (command_tx, message_rx) = spawn_agent_worker(
        root_config,
        session_log_path,
        workspace_root,
        sigil_kernel::InteractionMode::Interactive,
    )?;

    let failure = loop {
        let message = message_rx.recv_timeout(Duration::from_secs(3))?;
        if matches!(
            message,
            WorkerMessage::McpActivationStatus {
                status: McpActivationStatus::Failed { .. },
                ..
            }
        ) {
            break message;
        }
    };

    assert!(matches!(
        failure,
        WorkerMessage::McpActivationStatus {
            server_name: Some(ref server_name),
            status: McpActivationStatus::Failed { ref error },
        } if server_name == "required-eager"
            && error.contains("failed to spawn MCP server required-eager")
    ));

    command_tx.send(WorkerCommand::CancelRun)?;
    let still_running = loop {
        let message = message_rx.recv_timeout(Duration::from_secs(3))?;
        if matches!(message, WorkerMessage::RunFailed(_)) {
            break message;
        }
    };
    assert!(matches!(
        still_running,
        WorkerMessage::RunFailed(ref error) if error == "no active run to cancel"
    ));
    let _ = command_tx.send(WorkerCommand::Shutdown);
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
    let activating = worker.recv()?;
    assert!(matches!(
        activating,
        WorkerMessage::McpActivationStatus {
            server_name: Some(ref server_name),
            status: McpActivationStatus::Activating,
        } if server_name == "missing"
    ));
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
fn activate_lazy_mcp_reports_ready_status_when_server_registers_tools() -> Result<()> {
    let temp = tempdir()?;
    let workspace_root = temp.path().to_path_buf();
    let script_path = temp.path().join("fake_mcp_server.py");
    write_fake_server_script(&script_path)?;

    let session_log_path = temp.path().join(".sigil/sessions/session-ready-lazy.jsonl");
    let mut root_config = test_root_config(&workspace_root, "planned", "planned-model");
    root_config.mcp_servers.push(McpServerConfig {
        name: "ready-lazy".to_owned(),
        command: "python3".to_owned(),
        args: vec![script_path.to_string_lossy().to_string()],
        startup: McpServerStartup::Lazy,
        startup_timeout_secs: 5,
        ..McpServerConfig::default()
    });

    let provider = PlannedProvider::new(Vec::new());
    let agent = Agent::new(provider, ToolRegistry::new());
    let worker = spawn_test_worker(root_config, session_log_path, agent, workspace_root)?;

    worker.send(WorkerCommand::ActivateLazyMcp {
        server_name: Some("ready-lazy".to_owned()),
    })?;
    let activating = worker.recv()?;
    assert!(matches!(
        activating,
        WorkerMessage::McpActivationStatus {
            server_name: Some(ref server_name),
            status: McpActivationStatus::Activating,
        } if server_name == "ready-lazy"
    ));

    let ready = worker.recv_until(|message| {
        matches!(
            message,
            WorkerMessage::McpActivationStatus {
                server_name: Some(server_name),
                status: McpActivationStatus::Ready { added_tools: 1 },
            } if server_name == "ready-lazy"
        )
    })?;
    assert!(matches!(
        ready,
        WorkerMessage::McpActivationStatus {
            server_name: Some(ref server_name),
            status: McpActivationStatus::Ready { added_tools: 1 },
        } if server_name == "ready-lazy"
    ));

    let notice = worker.recv_until(|message| matches!(message, WorkerMessage::Notice(_)))?;
    assert!(matches!(
        notice,
        WorkerMessage::Notice(ref text)
            if text == "activated 1 lazy MCP tools for ready-lazy"
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

#[test]
fn submit_prompt_rejects_second_run_while_agent_is_active() -> Result<()> {
    let temp = tempdir()?;
    let workspace_root = temp.path().to_path_buf();
    let session_log_path = temp.path().join(".sigil/sessions/session-busy.jsonl");
    let root_config = test_root_config(&workspace_root, "planned", "planned-model");
    let provider = PlannedProvider::new(vec![StreamPlan::Pending]);
    let agent = Agent::new(provider, ToolRegistry::new());
    let worker = spawn_test_worker(root_config, session_log_path, agent, workspace_root)?;

    worker.send(WorkerCommand::SubmitPrompt {
        prompt: "first".to_owned(),
        reasoning_effort: ReasoningEffort::Max,
    })?;
    let _ = worker.recv_until(|message| matches!(message, WorkerMessage::RunStarted { .. }))?;

    worker.send(WorkerCommand::SubmitPrompt {
        prompt: "second".to_owned(),
        reasoning_effort: ReasoningEffort::Max,
    })?;
    let failure = worker.recv_until(|message| matches!(message, WorkerMessage::RunFailed(_)))?;

    assert!(matches!(
        failure,
        WorkerMessage::RunFailed(ref error) if error == "agent is already running"
    ));

    worker.shutdown()?;
    Ok(())
}

#[test]
fn submit_prompt_surfaces_provider_startup_errors() -> Result<()> {
    let temp = tempdir()?;
    let workspace_root = temp.path().to_path_buf();
    let session_log_path = temp.path().join(".sigil/sessions/session-error.jsonl");
    let root_config = test_root_config(&workspace_root, "planned", "planned-model");
    let provider = PlannedProvider::new(vec![StreamPlan::Fail("provider startup failed")]);
    let agent = Agent::new(provider, ToolRegistry::new());
    let worker = spawn_test_worker(root_config, session_log_path, agent, workspace_root)?;

    worker.send(WorkerCommand::SubmitPrompt {
        prompt: "hello".to_owned(),
        reasoning_effort: ReasoningEffort::Max,
    })?;
    let _ = worker.recv_until(|message| matches!(message, WorkerMessage::RunStarted { .. }))?;
    let failure = worker.recv_until(|message| matches!(message, WorkerMessage::RunFailed(_)))?;

    assert!(matches!(
        failure,
        WorkerMessage::RunFailed(ref error) if error.contains("provider startup failed")
    ));

    worker.shutdown()?;
    Ok(())
}

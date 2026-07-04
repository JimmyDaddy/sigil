use std::{collections::BTreeMap, path::PathBuf, sync::mpsc, time::Duration};

use anyhow::Result;
use serde_json::json;
use sigil_kernel::{
    AgentConfig, DurableEventType, JsonlSessionStore, McpServerConfig, McpServerStartup,
    MemoryConfig, PermissionConfig, RootConfig, SessionConfig, SessionStreamRecord,
    WorkspaceConfig,
};
use std::fs;
use tempfile::tempdir;

use super::super::{
    McpActivationStatus, WorkerCommand, WorkerMessage, spawn::report_runtime_build_result,
    spawn_agent_worker,
};

fn deepseek_root_config(workspace_root: &std::path::Path) -> RootConfig {
    RootConfig {
        workspace: WorkspaceConfig {
            root: workspace_root.display().to_string(),
        },
        storage: Default::default(),
        session: SessionConfig {
            log_dir: Some(".sigil/sessions".to_owned()),
        },
        agent: AgentConfig {
            provider: "deepseek".to_owned(),
            model: "deepseek-v4-flash".to_owned(),
            max_turns: None,
            tool_timeout_secs: 30,
        },
        model_request: Default::default(),
        permission: PermissionConfig::default(),
        memory: MemoryConfig { enabled: false },
        skills: Default::default(),
        compaction: Default::default(),
        code_intelligence: Default::default(),
        terminal: Default::default(),
        execution: Default::default(),
        verification: Default::default(),
        appearance: Default::default(),
        task: Default::default(),
        providers: BTreeMap::from([(
            "deepseek".to_owned(),
            json!({
                "base_url": "https://example.com",
                "beta_base_url": "https://example.com/beta",
                "anthropic_base_url": "https://example.com/anthropic",
                "fim_model": "deepseek-v4-pro",
                "api_key": "test-key",
                "strict_tools_mode": "auto"
            }),
        )]),
        mcp_servers: Vec::new(),
    }
}

fn recv_message(message_rx: &mpsc::Receiver<WorkerMessage>) -> Result<WorkerMessage> {
    message_rx
        .recv_timeout(Duration::from_secs(3))
        .map_err(|error| anyhow::anyhow!("timed out waiting for worker message: {error}"))
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
fn report_runtime_build_result_forwards_runtime_build_failures() -> Result<()> {
    let (message_tx, message_rx) = mpsc::channel();
    let runtime = report_runtime_build_result(
        Err(std::io::Error::other("runtime unavailable")),
        &message_tx,
    );

    assert!(runtime.is_none());
    assert!(matches!(
        recv_message(&message_rx)?,
        WorkerMessage::RunFailed(ref message) if message.contains("runtime unavailable")
    ));
    Ok(())
}

#[test]
fn spawn_agent_worker_reports_provider_build_failures_from_worker_thread() -> Result<()> {
    let temp = tempdir()?;
    let workspace_root = temp.path().to_path_buf();
    let session_log_path = temp
        .path()
        .join(".sigil/sessions/session-spawn-provider.jsonl");
    let mut root_config = deepseek_root_config(&workspace_root);
    root_config.agent.provider = "other".to_owned();

    let (_command_tx, message_rx) = spawn_agent_worker(
        root_config,
        session_log_path,
        workspace_root,
        sigil_kernel::InteractionMode::Interactive,
    )?;
    let failure = recv_message(&message_rx)?;

    assert!(matches!(
        failure,
        WorkerMessage::RunFailed(ref error) if error.contains("unsupported provider other")
    ));
    Ok(())
}

#[test]
fn spawn_agent_worker_starts_and_accepts_shutdown_for_valid_config() -> Result<()> {
    let temp = tempdir()?;
    let workspace_root = temp.path().to_path_buf();
    let session_log_path = temp.path().join(".sigil/sessions/session-spawn-ok.jsonl");
    let root_config = deepseek_root_config(&workspace_root);

    let (command_tx, message_rx) = spawn_agent_worker(
        root_config,
        session_log_path.clone(),
        workspace_root,
        sigil_kernel::InteractionMode::Interactive,
    )?;
    command_tx.send(WorkerCommand::Shutdown)?;

    assert!(message_rx.recv_timeout(Duration::from_millis(250)).is_err());
    Ok(())
}

#[test]
fn spawn_agent_worker_keeps_running_when_eager_mcp_startup_fails() -> Result<()> {
    let temp = tempdir()?;
    let workspace_root = temp.path().to_path_buf();
    let session_log_path = temp
        .path()
        .join(".sigil/sessions/session-spawn-registry.jsonl");
    let mut root_config = deepseek_root_config(&workspace_root);
    root_config.mcp_servers.push(McpServerConfig {
        name: "required-eager".to_owned(),
        command: "/definitely/missing/sigil-mcp-server".to_owned(),
        startup: McpServerStartup::Eager,
        ..McpServerConfig::default()
    });

    let (command_tx, message_rx) = spawn_agent_worker(
        root_config,
        session_log_path,
        PathBuf::from(&workspace_root),
        sigil_kernel::InteractionMode::Interactive,
    )?;
    let failure = loop {
        let message = recv_message(&message_rx)?;
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
    if let Ok(message) = message_rx.recv_timeout(Duration::from_millis(100)) {
        assert!(
            !matches!(
                message,
                WorkerMessage::Notice(ref notice) if notice.contains("MCP startup failed")
            ),
            "background eager MCP startup failure should stay in lifecycle status"
        );
    }

    command_tx.send(WorkerCommand::CancelRun)?;
    let response = loop {
        let message = recv_message(&message_rx)?;
        if matches!(message, WorkerMessage::RunFailed(_)) {
            break message;
        }
    };
    assert!(matches!(
        response,
        WorkerMessage::RunFailed(ref error) if error == "no active run to cancel"
    ));
    let _ = command_tx.send(WorkerCommand::Shutdown);
    Ok(())
}

#[test]
fn spawn_agent_worker_reports_ready_for_eager_mcp_startup() -> Result<()> {
    let temp = tempdir()?;
    let workspace_root = temp.path().to_path_buf();
    let script_path = temp.path().join("fake_mcp_server.py");
    write_fake_server_script(&script_path)?;
    let session_log_path = temp
        .path()
        .join(".sigil/sessions/session-spawn-eager-ready.jsonl");
    let mut root_config = deepseek_root_config(&workspace_root);
    root_config.mcp_servers.push(McpServerConfig {
        name: "ready-eager".to_owned(),
        command: "python3".to_owned(),
        args: vec![script_path.to_string_lossy().to_string()],
        startup: McpServerStartup::Eager,
        startup_timeout_secs: 5,
        ..McpServerConfig::default()
    });

    let (command_tx, message_rx) = spawn_agent_worker(
        root_config,
        session_log_path.clone(),
        workspace_root,
        sigil_kernel::InteractionMode::Interactive,
    )?;
    let ready = loop {
        let message = recv_message(&message_rx)?;
        if matches!(
            message,
            WorkerMessage::McpActivationStatus {
                status: McpActivationStatus::Ready { .. },
                ..
            }
        ) {
            break message;
        }
    };

    assert!(matches!(
        ready,
        WorkerMessage::McpActivationStatus {
            server_name: Some(ref server_name),
            status: McpActivationStatus::Ready {
                added_tools: 1,
                process_coverage: Some(ref process_coverage),
            },
        } if server_name == "ready-eager"
            && process_coverage == "local stdio outside local sandbox"
    ));
    let lifecycle_mutations = JsonlSessionStore::read_event_records(&session_log_path)?
        .into_iter()
        .filter(|record| {
            matches!(
                record,
                SessionStreamRecord::Stored(event)
                    if DurableEventType::from_event_type(&event.event_type)
                        == Some(DurableEventType::WorkspaceMutationDetected)
            )
        })
        .count();
    assert_eq!(
        lifecycle_mutations, 0,
        "clean eager MCP startup must not stale workspace verification"
    );
    let _ = command_tx.send(WorkerCommand::Shutdown);
    Ok(())
}

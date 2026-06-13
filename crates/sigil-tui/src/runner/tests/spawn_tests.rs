use std::{collections::BTreeMap, path::PathBuf, sync::mpsc, time::Duration};

use anyhow::Result;
use serde_json::json;
use sigil_kernel::{
    AgentConfig, McpServerConfig, McpServerStartup, MemoryConfig, PermissionConfig, RootConfig,
    SessionConfig, WorkspaceConfig,
};
use tempfile::tempdir;

use super::super::{
    WorkerCommand, WorkerMessage, spawn::report_runtime_build_result, spawn_agent_worker,
};

fn deepseek_root_config(workspace_root: &std::path::Path) -> RootConfig {
    RootConfig {
        workspace: WorkspaceConfig {
            root: workspace_root.display().to_string(),
        },
        session: SessionConfig {
            log_dir: ".sigil/sessions".to_owned(),
        },
        agent: AgentConfig {
            provider: "deepseek".to_owned(),
            model: "deepseek-v4-flash".to_owned(),
            max_turns: None,
            tool_timeout_secs: 30,
        },
        permission: PermissionConfig::default(),
        memory: MemoryConfig { enabled: false },
        compaction: Default::default(),
        code_intelligence: Default::default(),
        terminal: Default::default(),
        providers: BTreeMap::from([(
            "deepseek".to_owned(),
            json!({
                "base_url": "https://example.com",
                "beta_base_url": "https://example.com/beta",
                "anthropic_base_url": "https://example.com/anthropic",
                "model": "deepseek-v4-flash",
                "fim_model": "deepseek-v4-pro",
                "api_key": "test-key",
                "request_timeout_secs": 15,
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
        session_log_path,
        workspace_root,
        sigil_kernel::InteractionMode::Interactive,
    )?;
    command_tx.send(WorkerCommand::Shutdown)?;

    assert!(message_rx.recv_timeout(Duration::from_millis(250)).is_err());
    Ok(())
}

#[test]
fn spawn_agent_worker_reports_registry_build_failures_from_worker_thread() -> Result<()> {
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

    let (_command_tx, message_rx) = spawn_agent_worker(
        root_config,
        session_log_path,
        PathBuf::from(&workspace_root),
        sigil_kernel::InteractionMode::Interactive,
    )?;
    let failure = recv_message(&message_rx)?;

    assert!(matches!(
        failure,
        WorkerMessage::RunFailed(ref error)
            if error.contains("failed to spawn MCP server required-eager")
    ));
    Ok(())
}

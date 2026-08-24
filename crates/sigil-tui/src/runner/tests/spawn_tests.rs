use std::{
    collections::BTreeMap,
    path::PathBuf,
    sync::{Arc, mpsc},
    time::Duration,
};

use anyhow::Result;
use serde_json::json;
use sigil_kernel::{
    AgentConfig, ConnectionId, ControlEntry, DurableEventType, JsonlSessionStore, McpServerConfig,
    McpServerStartup, MemoryConfig, PermissionConfig, RootConfig, SessionConfig, SessionLogEntry,
    SessionStreamRecord, WorkspaceConfig, WorkspaceTrust, WorkspaceTrustDecisionEntry,
    stable_workspace_id,
};
use std::fs;
use tempfile::tempdir;

use super::super::{
    McpActivationStatus, WorkerCommand, WorkerMessage,
    spawn::load_session_entries_with_workspace_trust, spawn::report_runtime_build_result,
    spawn_agent_worker,
};

fn deepseek_root_config(workspace_root: &std::path::Path) -> RootConfig {
    RootConfig {
        config_version: 2,
        workspace: WorkspaceConfig {
            root: workspace_root.display().to_string(),
        },
        storage: Default::default(),
        session: SessionConfig {
            log_dir: Some(".sigil/sessions".to_owned()),
            retention: Default::default(),
        },
        agent: AgentConfig {
            runtime_provider: "deepseek".to_owned(),
            connection: Some(ConnectionId::new("deepseek-default").expect("connection id")),
            model: "deepseek-v4-flash".to_owned(),
            max_turns: None,
            tool_timeout_secs: 30,
        },
        model_request: Default::default(),
        permission: PermissionConfig::default(),
        memory: MemoryConfig::with_enabled(false),
        skills: Default::default(),
        compaction: Default::default(),
        code_intelligence: Default::default(),
        terminal: Default::default(),
        execution: Default::default(),
        verification: Default::default(),
        appearance: Default::default(),
        task: Default::default(),
        connections: BTreeMap::from([(
            "deepseek-default".to_owned(),
            json!({
                "label": "DeepSeek",
                "provider": "deepseek",
                "protocol": "deepseek",
                "base_url": "https://example.com",
                "credential": {"source": "environment", "name": "SIGIL_API_KEY"},
                "options": {
                    "beta_base_url": "https://example.com/beta",
                    "anthropic_base_url": "https://example.com/anthropic",
                    "fim_model": "deepseek-v4-pro",
                    "strict_tools_mode": "auto"
                }
            }),
        )]),
        web: Default::default(),
        mcp_servers: Vec::new(),
    }
}

fn v2_loopback_root_config(workspace_root: &std::path::Path) -> Result<RootConfig> {
    let connection_id = ConnectionId::new("orchestration-fixture")?;
    let mut root_config = deepseek_root_config(workspace_root);
    root_config.config_version = 2;
    root_config.agent.runtime_provider.clear();
    root_config.agent.connection = Some(connection_id.clone());
    root_config.agent.model = "fixture-model".to_owned();
    root_config.connections.insert(
        connection_id.to_string(),
        json!({
            "label": "Orchestration fixture",
            "provider": "custom",
            "protocol": "chat_completions",
            "base_url": "http://127.0.0.1:43123/v1",
            "credential": { "source": "none" }
        }),
    );
    Ok(root_config)
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
    line = sys.stdin.buffer.readline()
    if not line:
        return None
    return json.loads(line.decode())

def write_message(obj):
    body = json.dumps(obj).encode()
    sys.stdout.buffer.write(body + b"\n")
    sys.stdout.buffer.flush()

while True:
    message = read_message()
    if message is None:
        break
    method = message.get("method")
    if method == "initialize":
        write_message({"jsonrpc":"2.0","id":message["id"],"result":{"protocolVersion":"2025-06-18","serverInfo":{"name":"fake","version":"1.0.0"},"capabilities":{}}})
    elif method == "notifications/initialized":
        pass
    elif method == "tools/list":
        write_message({"jsonrpc":"2.0","id":message["id"],"result":{"tools":[{"name":"echo","description":"Echo","inputSchema":{"type":"object","properties":{"value":{"type":"string"}},"required":["value"]}}]}})
"#,
    )?;
    Ok(())
}

fn test_authority_composition(
    root: &std::path::Path,
) -> Result<Arc<sigil_runtime::r71_authority_composition::RuntimeAuthorityCompositionV1>> {
    let state_root = root.join("authority-state");
    fs::create_dir_all(state_root.join("cache"))?;
    let execution_temp_root = root.join("authority-execution-temp");
    fs::create_dir_all(&execution_temp_root)?;
    Ok(Arc::new(
        sigil_runtime::r71_authority_composition::compose_runtime_authority(
            &state_root,
            &execution_temp_root,
            sigil_kernel::resource::CanonicalHash::from_bytes([0x71; 32]),
            Arc::new(sigil_runtime::r71_shadow_planner::ShadowPlannerV1::new(
                sigil_runtime::r71_shadow_planner::ShadowPlannerConfigV1::default(),
            )),
            &[],
        )?,
    ))
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
        WorkerMessage::SessionRouteRecoveryRequired {
            code: sigil_kernel::PublicRouteRecoveryCode::ProviderUnavailable,
            retryable: true,
            ..
        }
    ));
    assert!(message_rx.try_recv().is_err());
    Ok(())
}

#[test]
fn worker_loads_workspace_trust_from_the_active_session_before_registry_build() -> Result<()> {
    let temp = tempdir()?;
    let workspace_root = temp.path().to_path_buf();
    let session_log_path = temp.path().join("session-trust.jsonl");
    JsonlSessionStore::new(&session_log_path)?.append(&SessionLogEntry::Control(
        ControlEntry::WorkspaceTrustDecision(WorkspaceTrustDecisionEntry {
            workspace_id: stable_workspace_id(&workspace_root)?,
            workspace_trust_snapshot_id: "workspace-trust:test".to_owned(),
            trust: WorkspaceTrust::Trusted,
            decided_by_event_id: None,
            reason: Some("test trust decision".to_owned()),
        }),
    ))?;

    let (entries, trust) =
        load_session_entries_with_workspace_trust(&session_log_path, &workspace_root)?;

    assert_eq!(entries.len(), 1);
    assert_eq!(trust, WorkspaceTrust::Trusted);
    Ok(())
}

#[test]
fn spawn_agent_worker_rejects_unconfigured_route_before_worker_thread() -> Result<()> {
    let temp = tempdir()?;
    let workspace_root = temp.path().to_path_buf();
    let session_log_path = temp
        .path()
        .join(".sigil/sessions/session-spawn-provider.jsonl");
    let mut root_config = deepseek_root_config(&workspace_root);
    root_config.agent.connection = Some(ConnectionId::new("missing")?);

    let error = spawn_agent_worker(
        root_config,
        workspace_root.join("sigil.toml"),
        session_log_path,
        workspace_root,
        sigil_kernel::InteractionMode::Interactive,
    )
    .expect_err("route admission must fail synchronously");

    assert!(format!("{error:#}").contains("model_route_not_configured"));
    Ok(())
}

#[test]
fn spawn_agent_worker_starts_and_accepts_shutdown_for_valid_config() -> Result<()> {
    let _environment_lock = crate::test_env::lock();
    let _api_key = crate::test_env::EnvScope::set("SIGIL_API_KEY", "test-key");
    let temp = tempdir()?;
    let workspace_root = temp.path().to_path_buf();
    let session_log_path = temp.path().join(".sigil/sessions/session-spawn-ok.jsonl");
    let root_config = deepseek_root_config(&workspace_root);

    let (command_tx, message_rx) = spawn_agent_worker(
        root_config,
        workspace_root.join("sigil.toml"),
        session_log_path.clone(),
        workspace_root,
        sigil_kernel::InteractionMode::Interactive,
    )?;
    let ready = recv_message(&message_rx)?;
    assert!(matches!(ready, WorkerMessage::WorkerReady));
    command_tx.send(WorkerCommand::Shutdown)?;
    Ok(())
}

#[test]
fn second_worker_for_the_same_session_reports_attachment_busy() -> Result<()> {
    let _environment_lock = crate::test_env::lock();
    let _api_key = crate::test_env::EnvScope::set("SIGIL_API_KEY", "test-key");
    let temp = tempdir()?;
    let workspace_root = temp.path().to_path_buf();
    let session_log_path = temp.path().join(".sigil/sessions/session-exclusive.jsonl");
    let root_config = deepseek_root_config(&workspace_root);
    let (owner_tx, owner_rx) = spawn_agent_worker(
        root_config.clone(),
        workspace_root.join("sigil.toml"),
        session_log_path.clone(),
        workspace_root.clone(),
        sigil_kernel::InteractionMode::Interactive,
    )?;
    assert!(matches!(
        recv_message(&owner_rx)?,
        WorkerMessage::WorkerReady
    ));

    let contender = spawn_agent_worker(
        root_config,
        workspace_root.join("sigil.toml"),
        session_log_path,
        workspace_root,
        sigil_kernel::InteractionMode::Interactive,
    )
    .expect_err("second attachment must be rejected before a worker starts");
    assert!(format!("{contender:#}").contains("session_attachment_busy"));
    owner_tx.send(WorkerCommand::Shutdown)?;
    Ok(())
}

#[test]
fn worker_rebinds_same_origin_route_after_endpoint_correction() -> Result<()> {
    let _environment_lock = crate::test_env::lock();
    let _api_key = crate::test_env::EnvScope::set("SIGIL_API_KEY", "test-key");
    let temp = tempdir()?;
    let workspace_root = temp.path().to_path_buf();
    let session_log_path = temp.path().join(".sigil/sessions/session-corrected.jsonl");
    let mut wrong_config = deepseek_root_config(&workspace_root);
    wrong_config
        .connections
        .get_mut("deepseek-default")
        .expect("connection")["base_url"] = json!("https://example.com/wrong-endpoint");
    let (_, wrong_route) =
        sigil_runtime::provider_connections::resolve_default_model_route(&wrong_config)?;
    let initial = sigil_runtime::provider_connections::load_session_for_route_resume(
        &wrong_config,
        &wrong_route,
        JsonlSessionStore::new(&session_log_path)?,
    )?;
    let wrong_fingerprint = initial
        .resolved_model_route()
        .expect("initial route")
        .semantic_fingerprint
        .clone();
    drop(initial);

    let corrected_config = deepseek_root_config(&workspace_root);
    let (command_tx, message_rx) = spawn_agent_worker(
        corrected_config,
        workspace_root.join("sigil.toml"),
        session_log_path.clone(),
        workspace_root,
        sigil_kernel::InteractionMode::Interactive,
    )?;
    let mut saw_rebind_notice = false;
    loop {
        match recv_message(&message_rx)? {
            WorkerMessage::Notice(message) if message.contains("连接配置已更新") => {
                saw_rebind_notice = true;
            }
            WorkerMessage::WorkerReady => break,
            message => anyhow::bail!("unexpected worker startup message: {message:?}"),
        }
    }
    assert!(saw_rebind_notice);
    let entries = JsonlSessionStore::read_entries(&session_log_path)?;
    assert!(entries.iter().any(|entry| matches!(
        entry,
        SessionLogEntry::Control(ControlEntry::SessionRouteRebound {
            resolved_model_route,
            ..
        }) if resolved_model_route.semantic_fingerprint != wrong_fingerprint
    )));
    command_tx.send(WorkerCommand::Shutdown)?;
    Ok(())
}

#[test]
fn spawn_agent_worker_initializes_v2_route_after_workspace_trust_prelude() -> Result<()> {
    let temp = tempdir()?;
    let workspace_root = temp.path().to_path_buf();
    let session_log_path = temp
        .path()
        .join(".sigil/sessions/session-spawn-after-trust.jsonl");
    JsonlSessionStore::new(&session_log_path)?.append(&SessionLogEntry::Control(
        ControlEntry::WorkspaceTrustDecision(WorkspaceTrustDecisionEntry {
            workspace_id: stable_workspace_id(&workspace_root)?,
            workspace_trust_snapshot_id: "workspace-trust:test".to_owned(),
            trust: WorkspaceTrust::Trusted,
            decided_by_event_id: None,
            reason: Some("trusted before worker startup".to_owned()),
        }),
    ))?;

    let (command_tx, message_rx) = spawn_agent_worker(
        v2_loopback_root_config(&workspace_root)?,
        workspace_root.join("sigil.toml"),
        session_log_path.clone(),
        workspace_root,
        sigil_kernel::InteractionMode::Interactive,
    )?;

    assert!(matches!(
        recv_message(&message_rx)?,
        WorkerMessage::WorkerReady
    ));
    let entries = JsonlSessionStore::read_entries(&session_log_path)?;
    let route = entries.iter().find_map(|entry| match entry {
        SessionLogEntry::Control(ControlEntry::SessionIdentity {
            resolved_model_route,
            ..
        }) => resolved_model_route.as_ref(),
        _ => None,
    });
    assert_eq!(
        route.map(|route| route.model_ref.connection_id.as_str()),
        Some("orchestration-fixture")
    );
    assert_eq!(
        route.map(|route| route.model_ref.model_id.as_str()),
        Some("fixture-model")
    );
    command_tx.send(WorkerCommand::Shutdown)?;
    Ok(())
}

#[test]
fn spawn_agent_worker_keeps_running_when_eager_mcp_startup_fails() -> Result<()> {
    let _environment_lock = crate::test_env::lock();
    let _api_key = crate::test_env::EnvScope::set("SIGIL_API_KEY", "test-key");
    let temp = tempdir()?;
    let workspace_root = temp.path().to_path_buf();
    let session_log_path = temp
        .path()
        .join(".sigil/sessions/session-spawn-registry.jsonl");
    let mut root_config = deepseek_root_config(&workspace_root);
    root_config.mcp_servers.push(mcp_server_config! {
        name: "required-eager".to_owned(),
        command: "/definitely/missing/sigil-mcp-server".to_owned(),
        startup: McpServerStartup::Eager,
        ..McpServerConfig::default()
    });

    let (command_tx, message_rx) = spawn_agent_worker(
        root_config,
        workspace_root.join("sigil.toml"),
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
            && error.contains("mcp_command_resolution_failed")
            && error.contains("stdio command does not resolve to an existing file")
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
    let _environment_lock = crate::test_env::lock();
    let _api_key = crate::test_env::EnvScope::set("SIGIL_API_KEY", "test-key");
    let temp = tempdir()?;
    let workspace_root = temp.path().to_path_buf();
    let script_path = temp.path().join("fake_mcp_server.py");
    write_fake_server_script(&script_path)?;
    let session_log_path = temp
        .path()
        .join(".sigil/sessions/session-spawn-eager-ready.jsonl");
    let mut root_config = deepseek_root_config(&workspace_root);
    root_config.mcp_servers.push(mcp_server_config! {
        name: "ready-eager".to_owned(),
        command: "python3".to_owned(),
        args: vec![script_path.to_string_lossy().to_string()],
        startup: McpServerStartup::Eager,
        startup_timeout_secs: 5,
        ..McpServerConfig::default()
    });

    let spawned = super::super::spawn::spawn_agent_worker_with_route_directive_and_attachment(
        root_config,
        workspace_root.join("sigil.toml"),
        session_log_path.clone(),
        workspace_root,
        sigil_kernel::InteractionMode::Interactive,
        super::super::spawn::WorkerSessionRouteDirective::default(),
        Some(test_authority_composition(temp.path())?),
        None,
    )?;
    let command_tx = spawned.command_tx;
    let message_rx = spawned.message_rx;
    drop(spawned.join_handle);
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

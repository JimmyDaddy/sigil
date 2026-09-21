//! Actual Core execution and cancellation settlement across the worker boundary.
#![cfg(unix)]

use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use anyhow::{Context, Result};
use sigil_kernel::{
    Agent, JsonlSessionStore, ProviderChunk, ReasoningEffort, ToolCall, ToolRegistry,
};

use super::common::{
    PlannedProvider, StreamPlan, failing_role_provider_builder,
    routed_unauthenticated_test_root_config, spawn_test_worker_with_owned_terminal,
    test_authority_composition,
};
use crate::runner::{WorkerCommand, WorkerMessage};

#[test]
fn shutdown_settles_foreground_execution_after_physical_spawn() -> Result<()> {
    check_real_execution_shutdown(false)
}

#[test]
fn shutdown_settles_background_execution_after_run_finished() -> Result<()> {
    check_real_execution_shutdown(true)
}

fn check_real_execution_shutdown(background: bool) -> Result<()> {
    let runtime = tokio::runtime::Runtime::new()?;
    let _runtime_guard = runtime.enter();
    let temp = tempfile::tempdir()?;
    let workspace = temp.path().to_path_buf();
    let session_path = workspace.join("sessions/real-shutdown.jsonl");
    let mut config = routed_unauthenticated_test_root_config(&workspace, "planned-model");
    config.composition = sigil_kernel::RuntimeCompositionConfig::core();
    config.permission.mode = sigil_kernel::PermissionMode::DangerFullAccess;
    config.storage.state_root =
        sigil_kernel::StorageRoot::Path(workspace.join("state").display().to_string());
    config.storage.cache_root =
        sigil_kernel::StorageRoot::Path(workspace.join("cache").display().to_string());
    config.save(&workspace.join("sigil.toml"))?;
    let config = config.with_effective_composition()?;
    let (provider_name, route) =
        sigil_runtime::provider_connections::resolve_default_model_route(&config)?;
    let mut session = sigil_kernel::Session::new_with_route(provider_name, route)
        .with_store(JsonlSessionStore::new(&session_path)?);
    session.ensure_identity_entry()?;

    let (authority, _authority_root) = test_authority_composition(&workspace)?;
    let paths = sigil_tools_builtin::BuiltinToolPaths::workspace_defaults(&workspace);
    let scratch = sigil_runtime::authority_scratch_control(paths.scratch_root.clone());
    let (event_tx, event_rx) = std::sync::mpsc::channel();
    let lifecycle = crate::runner::terminal_lifecycle_bridge::ChannelTerminalLifecycleRouter::new(
        event_tx.clone(),
    );
    let mut registry = ToolRegistry::new();
    let handles = sigil_tools_builtin::register_builtin_tools_with_selection(
        &mut registry,
        paths,
        Arc::clone(&authority.command_execution)
            as Arc<dyn sigil_tools_builtin::ManagedCommandExecutionPortV1>,
        sigil_tools_builtin::BuiltinToolSelection::core(),
        Some(scratch),
        || sigil_tools_builtin::BuiltinTerminalOptions {
            execution_config: sigil_tools_builtin::TerminalExecutionConfig::from_execution_config(
                &config.execution,
            ),
            lifecycle_route: Some(sigil_tools_builtin::TerminalLifecycleRoute::Factory(
                Arc::new(lifecycle),
            )),
            executor: Arc::clone(&authority.command_execution)
                as Arc<dyn sigil_tools_builtin::ManagedTerminalExecutionPortV1>,
        },
    );
    let control = handles.terminal.context("Core execution control")?;
    let args_json = serde_json::json!({
        "command": "printf ready > shutdown-child-ready; sleep 20",
        "yield_time_ms": if background { 0 } else { 60000 },
        "max_runtime_secs": 20,
    })
    .to_string();
    let call = ToolCall {
        id: "shutdown-real-command".to_owned(),
        name: "exec_command".to_owned(),
        args_json: args_json.clone(),
    };
    let provider = PlannedProvider::new(vec![
        StreamPlan::Chunks(vec![
            ProviderChunk::ToolCallStart {
                id: call.id.clone(),
                name: call.name.clone(),
            },
            ProviderChunk::ToolCallArgsDelta {
                id: call.id.clone(),
                delta: args_json,
            },
            ProviderChunk::ToolCallComplete(call),
            ProviderChunk::Done,
        ]),
        if background {
            StreamPlan::Chunks(vec![
                ProviderChunk::TextDelta("Command is running.".to_owned()),
                ProviderChunk::Done,
            ])
        } else {
            StreamPlan::Pending
        },
    ]);
    let worker = spawn_test_worker_with_owned_terminal(
        config,
        session_path.clone(),
        Agent::new(provider, registry),
        workspace.clone(),
        failing_role_provider_builder(),
        Arc::clone(&authority),
        None,
        Some(control.clone()),
        Some((event_tx, event_rx)),
    )?;
    worker.recv_until(|message| matches!(message, WorkerMessage::WorkerReady))?;
    worker.send(WorkerCommand::SubmitPrompt {
        prompt: "Run the command until cancellation.".to_owned(),
        reasoning_effort: ReasoningEffort::Medium,
    })?;
    let ready_deadline = Instant::now() + Duration::from_secs(10);
    while !std::fs::read_to_string(workspace.join("shutdown-child-ready"))
        .is_ok_and(|contents| contents == "ready")
    {
        anyhow::ensure!(
            Instant::now() < ready_deadline,
            "actual managed child did not start"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(
        std::fs::read_to_string(workspace.join("shutdown-child-ready"))?,
        "ready"
    );
    if background {
        worker.recv_until(|message| matches!(message, WorkerMessage::RunFinished { .. }))?;
    }
    let sender = worker.command_sender();
    // The ready marker proves a physical spawn. The original owner must stop and settle it.
    sender.begin_shutdown();
    worker.shutdown()?;
    assert!(
        sender.cleanup_complete(),
        "{}",
        sender.shutdown_diagnostic("real-core-worker")
    );

    let records = JsonlSessionStore::read_event_records(&session_path)?;
    let requests = records
        .iter()
        .map(|record| &record.stored_event().payload)
        .filter(|payload| payload["record"] == "requested")
        .collect::<Vec<_>>();
    let terminals = records
        .iter()
        .map(|record| &record.stored_event().payload)
        .filter(|payload| payload["record"] == "finalized")
        .collect::<Vec<_>>();
    if background {
        assert!(requests.is_empty(), "the foreground run already finished");
        assert!(terminals.is_empty());
    } else {
        assert_eq!(requests.len(), 1);
        assert_eq!(terminals.len(), 1);
        assert_eq!(terminals[0]["request_id"], requests[0]["request_id"]);
        assert_eq!(terminals[0]["outcome"], "cancelled");
        assert_eq!(terminals[0]["cleanup_complete"], true);
        assert_eq!(terminals[0]["active_effects"], 0);
        assert_eq!(terminals[0]["active_tasks"], 0);
    }

    let result = records
        .iter()
        .map(|record| &record.stored_event().payload)
        .filter_map(|payload| payload.get("session_log_entry")?.get("tool_result_v3"))
        .find(|result| result["call_id"] == "shutdown-real-command")
        .context("cancelled tool result must remain durable")?;
    let execution_id = result["facts"]["tool_specific"]["execution_id"]
        .as_str()
        .context("real execution identity must survive cancellation")?;
    let execution = runtime.block_on(control.status(
        &workspace,
        &sigil_kernel::TerminalTaskId::new(execution_id)?,
    ))?;
    assert!(matches!(
        execution.status,
        sigil_kernel::TerminalTaskStatus::Cancelled
    ));
    assert!(
        execution.cleanup.as_ref().is_some_and(
            |cleanup| cleanup.status == sigil_kernel::ExecutionCleanupStatus::Completed
        )
    );
    let durable_entries = JsonlSessionStore::read_entries(&session_path)?;
    assert!(durable_entries.iter().any(|entry| matches!(entry,
        sigil_kernel::SessionLogEntry::Control(sigil_kernel::ControlEntry::TerminalTask(task))
            if task.handle.task_id == execution.handle.task_id
                && task.status == sigil_kernel::TerminalTaskStatus::Cancelled
                && task.cleanup.as_ref().is_some_and(|cleanup| cleanup.status == sigil_kernel::ExecutionCleanupStatus::Completed)
    )), "the exact execution terminal must be durable before its owner joins");
    Ok(())
}

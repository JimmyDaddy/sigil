use std::{fs, sync::Arc, time::Duration};

#[cfg(unix)]
use anyhow::Context;
use anyhow::Result;
#[cfg(unix)]
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use sigil_kernel::{
    Agent, ApprovalHandler, ProviderChunk, ReasoningEffort, RunEvent, SessionLogEntry,
    ToolApproval, ToolCall, ToolCategory, ToolPreviewCapability, ToolRegistry, ToolSpec,
};
#[cfg(unix)]
use sigil_kernel::{ControlEntry, JsonlSessionStore};
use sigil_runtime::register_agent_tools_with_workspace;
use tempfile::tempdir;

#[cfg(unix)]
use super::common::{
    failing_role_provider_builder, spawn_test_worker_with_owned_terminal,
    test_authority_composition,
};
use super::{
    super::{
        WorkerCommand, WorkerMessage,
        approval_bridge::{ApprovalSignal, ChannelApprovalHandler},
        protocol::{WorkerApprovalCommand, WorkerCommandEnvelope},
    },
    common::{
        ApprovalFlowProvider, PlannedProvider, StreamPlan, WriteTool,
        routed_unauthenticated_test_root_config, spawn_test_worker, test_root_config,
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
        &approval_request,
        WorkerMessage::Event(event)
            if matches!(event.as_ref(), RunEvent::ToolApprovalRequested { call, .. } if call.id == "call-1")
    ));

    worker.send(approval_command(
        "command-approval-once",
        approval_session_id(&approval_request),
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
            SessionLogEntry::ToolResultV3(result) => Some(result),
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
    let session_id = approval_session_id(&request);

    worker.send(approval_command(
        "command-approval-1",
        session_id,
        "call-1",
        &approval_request_id,
        true,
    ))?;
    worker.send(approval_command(
        "command-approval-1",
        session_id,
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
fn approval_command_requires_the_durable_session_scope_before_delivery_or_replay() -> Result<()> {
    let temp = tempdir()?;
    let workspace_root = temp.path().to_path_buf();
    let session_log_path = temp.path().join(".sigil/sessions/approval-scope.jsonl");
    let path_alias = session_log_path.display().to_string();
    let root_config = test_root_config(&workspace_root, "approval-flow", "approval-model");
    let mut registry = ToolRegistry::new();
    registry.register(Arc::new(WriteTool));
    let worker = spawn_test_worker(
        root_config,
        session_log_path,
        Agent::new(ApprovalFlowProvider, registry),
        workspace_root,
    )?;
    worker.send(WorkerCommand::SubmitPrompt {
        prompt: "write".to_owned(),
        reasoning_effort: ReasoningEffort::Max,
    })?;
    let request = worker.recv_until(|message| {
        matches!(
            message,
            WorkerMessage::Event(event)
                if matches!(event.as_ref(), RunEvent::ToolApprovalRequested { call, .. } if call.id == "call-1")
        )
    })?;
    let session_id = approval_session_id(&request);
    uuid::Uuid::parse_str(session_id)?;
    assert_ne!(session_id, path_alias);
    let foreign_session_id = uuid::Uuid::new_v4().to_string();
    let request_id = approval_request_id(&request);
    let command_id = "approval-scope-command";

    for invalid_scope in [&path_alias, &foreign_session_id] {
        worker.send(approval_command(
            command_id,
            invalid_scope,
            "call-1",
            request_id,
            true,
        ))?;
        let rejected = worker.recv_until(|message| {
            matches!(message, WorkerMessage::ApprovalCommandReceipt(receipt) if receipt.command_id == command_id)
        })?;
        assert!(matches!(
            rejected,
            WorkerMessage::ApprovalCommandReceipt(receipt)
                if receipt.route_state == crate::runner::WorkerApprovalRouteState::Rejected
                    && !receipt.replayed
        ));
    }

    // A rejected envelope must leave both the approval and the command ID available.
    worker.send(approval_command(
        command_id, session_id, "call-1", request_id, true,
    ))?;
    let mut accepted = false;
    let mut finished = false;
    while !accepted || !finished {
        match worker.recv_with_timeout(Duration::from_secs(10))? {
            WorkerMessage::ApprovalCommandReceipt(receipt) if receipt.command_id == command_id => {
                assert_eq!(
                    receipt.route_state,
                    crate::runner::WorkerApprovalRouteState::DecisionAccepted
                );
                assert!(!receipt.replayed);
                accepted = true;
            }
            WorkerMessage::RunFinished { result, entries } => {
                assert_eq!(result.tool_calls, 1);
                assert_eq!(
                    entries
                        .iter()
                        .filter(|entry| matches!(entry, SessionLogEntry::ToolResultV3(_)))
                        .count(),
                    1
                );
                finished = true;
            }
            WorkerMessage::RunFailed(error) => anyhow::bail!("approval run failed: {error}"),
            _ => {}
        }
    }

    for (scope, expected_state, expected_replay) in [
        (
            foreign_session_id.as_str(),
            crate::runner::WorkerApprovalRouteState::Rejected,
            false,
        ),
        (
            session_id,
            crate::runner::WorkerApprovalRouteState::DecisionAccepted,
            true,
        ),
    ] {
        worker.send(approval_command(
            command_id, scope, "call-1", request_id, true,
        ))?;
        let replay = worker.recv_until(|message| {
            matches!(message, WorkerMessage::ApprovalCommandReceipt(receipt) if receipt.command_id == command_id)
        })?;
        let WorkerMessage::ApprovalCommandReceipt(receipt) = replay else {
            unreachable!("receipt predicate selected the message");
        };
        assert_eq!(receipt.route_state, expected_state);
        assert_eq!(receipt.replayed, expected_replay);
    }

    worker.shutdown()?;
    Ok(())
}

#[test]
fn rejected_family_without_active_run_preserves_persisted_permissions() -> Result<()> {
    let temp = tempdir()?;
    let workspace = temp.path().to_path_buf();
    let config_path = workspace.join("sigil.toml");
    let session_path = workspace.join(".sigil/sessions/rejected-family-idle.jsonl");
    let config = routed_unauthenticated_test_root_config(&workspace, "planned-model");
    config.save(&config_path)?;
    sigil_kernel::RootConfig::load_persisted(&config_path)?;
    let before = fs::read(&config_path)?;
    let worker = spawn_test_worker(
        config,
        session_path.clone(),
        Agent::new(PlannedProvider::new(vec![]), ToolRegistry::new()),
        workspace,
    )?;
    worker.recv_until(|message| matches!(message, WorkerMessage::WorkerReady))?;
    let session = sigil_kernel::Session::load_from_store(
        "planned",
        "planned-model",
        sigil_kernel::JsonlSessionStore::open_existing(&session_path)?,
    )?;
    worker.send(WorkerCommand::ApprovalCommand(WorkerCommandEnvelope::new(
        "idle-rejected-family",
        "sigil-tui-test",
        session.session_scope_id(),
        WorkerApprovalCommand::DecisionForFamily {
            call_id: "missing-call".to_owned(),
            approval_request_id: "missing-request".to_owned(),
            pattern: "cargo test*".to_owned(),
        },
    )))?;
    let receipt = worker.recv_until(|message| {
        matches!(message, WorkerMessage::ApprovalCommandReceipt(receipt)
            if receipt.command_id == "idle-rejected-family")
    })?;
    assert!(matches!(
        receipt,
        WorkerMessage::ApprovalCommandReceipt(receipt)
            if receipt.route_state == crate::runner::WorkerApprovalRouteState::Rejected
                && receipt.decision == crate::runner::WorkerApprovalDecision::ApproveForFamily
                && !receipt.replayed
    ));

    // Joining the worker also waits for its runtime's blocking persistence tasks; inspecting
    // the file immediately after the receipt could miss a rejected rule written later.
    worker.shutdown()?;
    assert!(
        fs::read(&config_path)? == before,
        "a rejected family decision must not change persisted config bytes"
    );
    assert!(
        sigil_kernel::RootConfig::load_persisted(&config_path)?
            .permission
            .commands
            .allow
            .is_empty()
    );
    Ok(())
}

#[test]
fn rejected_family_with_mismatched_approval_preserves_persisted_permissions() -> Result<()> {
    let temp = tempdir()?;
    let workspace = temp.path().to_path_buf();
    let config_path = workspace.join("sigil.toml");
    let session_path = workspace.join(".sigil/sessions/rejected-family-active.jsonl");
    let config = routed_unauthenticated_test_root_config(&workspace, "approval-model");
    config.save(&config_path)?;
    sigil_kernel::RootConfig::load_persisted(&config_path)?;
    let before = fs::read(&config_path)?;
    let mut registry = ToolRegistry::new();
    registry.register(Arc::new(WriteTool));
    let worker = spawn_test_worker(
        config,
        session_path,
        Agent::new(ApprovalFlowProvider, registry),
        workspace,
    )?;
    worker.send(WorkerCommand::SubmitPrompt {
        prompt: "request one write approval".to_owned(),
        reasoning_effort: ReasoningEffort::Max,
    })?;
    let request = worker.recv_until(|message| {
        matches!(message, WorkerMessage::Event(event)
            if matches!(event.as_ref(), RunEvent::ToolApprovalRequested { call, .. }
                if call.id == "call-1"))
    })?;
    let session_id = approval_session_id(&request);
    let request_id = approval_request_id(&request);
    for (command_id, call_id, candidate_request_id, pattern) in [
        ("family-wrong-call", "wrong-call", request_id, "cargo test*"),
        (
            "family-wrong-request",
            "call-1",
            "wrong-request",
            "cargo check*",
        ),
    ] {
        worker.send(WorkerCommand::ApprovalCommand(WorkerCommandEnvelope::new(
            command_id,
            "sigil-tui-test",
            session_id,
            WorkerApprovalCommand::DecisionForFamily {
                call_id: call_id.to_owned(),
                approval_request_id: candidate_request_id.to_owned(),
                pattern: pattern.to_owned(),
            },
        )))?;
        let receipt = worker.recv_until(|message| {
            matches!(message, WorkerMessage::ApprovalCommandReceipt(receipt)
                if receipt.command_id == command_id)
        })?;
        assert!(matches!(
            receipt,
            WorkerMessage::ApprovalCommandReceipt(receipt)
                if receipt.route_state == crate::runner::WorkerApprovalRouteState::Rejected
                    && receipt.decision == crate::runner::WorkerApprovalDecision::ApproveForFamily
                    && receipt.call_id == call_id
                    && receipt.approval_request_id == candidate_request_id
                    && !receipt.replayed
        ));
    }

    // Both negative acknowledgements must leave the original exact request available.
    worker.send(approval_command(
        "deny-original-after-rejected-family",
        session_id,
        "call-1",
        request_id,
        false,
    ))?;
    let finished =
        worker.recv_until(|message| matches!(message, WorkerMessage::RunFinished { .. }))?;
    let WorkerMessage::RunFinished { entries, .. } = finished else {
        unreachable!("run-finished predicate selected the message");
    };
    assert!(entries.iter().any(|entry| {
        matches!(entry, SessionLogEntry::ToolResultV3(result)
            if result.call_id == "call-1"
                && result.facts.error.as_ref().is_some_and(|error|
                    error.kind == sigil_kernel::ToolErrorKind::ApprovalDenied))
    }));
    worker.shutdown()?;
    assert!(
        fs::read(&config_path)? == before,
        "negative approval acknowledgements must not change persisted config bytes"
    );
    assert!(
        sigil_kernel::RootConfig::load_persisted(&config_path)?
            .permission
            .commands
            .allow
            .is_empty()
    );
    Ok(())
}

#[cfg(unix)]
#[test]
fn worker_rejected_family_receipt_retries_through_app_state_and_executes_once() -> Result<()> {
    command_family_receipt_through_app_state(false)
}

#[cfg(unix)]
#[test]
fn worker_rejected_family_after_cancellation_clears_the_delayed_approval_view() -> Result<()> {
    command_family_receipt_through_app_state(true)
}

#[cfg(unix)]
fn command_family_receipt_through_app_state(cancel_before_delivery: bool) -> Result<()> {
    let runtime = tokio::runtime::Runtime::new()?;
    let _runtime_guard = runtime.enter();
    let temp = tempdir()?;
    let workspace = temp.path().to_path_buf();
    let config_path = workspace.join("sigil.toml");
    let session_path = workspace.join(".sigil/sessions/family-app-state.jsonl");
    let command_fixture = workspace.join("approval-fixture");
    let marker = command_fixture.join("approval-family-marker");
    fs::create_dir_all(command_fixture.join("src"))?;
    fs::write(
        command_fixture.join("Cargo.toml"),
        "[package]\nname = \"approval-family-fixture\"\nversion = \"0.0.0\"\nedition = \"2024\"\n[workspace]\n",
    )?;
    fs::write(
        command_fixture.join("src/lib.rs"),
        r#"#[test]
fn record_execution() {
    use std::io::Write;
    let marker = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("approval-family-marker");
    std::fs::OpenOptions::new().create(true).append(true).open(marker)
        .expect("open fixture marker").write_all(b"executed\n")
        .expect("record actual execution");
}
"#,
    )?;
    let mut config = routed_unauthenticated_test_root_config(&workspace, "planned-model");
    config.composition = sigil_kernel::RuntimeCompositionConfig::core();
    config.storage.state_root =
        sigil_kernel::StorageRoot::Path(workspace.join("state").display().to_string());
    config.storage.cache_root =
        sigil_kernel::StorageRoot::Path(workspace.join("cache").display().to_string());
    config.save(&config_path)?;
    let before_bytes = fs::read(&config_path)?;
    let before_config = sigil_kernel::RootConfig::load_persisted(&config_path)?;
    let config = config.with_effective_composition()?;
    let (provider_name, route) =
        sigil_runtime::provider_connections::resolve_default_model_route(&config)?;
    let mut session = sigil_kernel::Session::new_with_route(provider_name, route)
        .with_store(JsonlSessionStore::new(&session_path)?);
    session.ensure_identity_entry()?;
    let session_id = session.session_scope_id().to_owned();
    drop(session);

    let (authority, authority_root) = test_authority_composition(&workspace)?;
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
    let call = ToolCall {
        id: "family-real-command".to_owned(),
        name: "exec_command".to_owned(),
        args_json: serde_json::json!({
            "command": "cargo test --offline --manifest-path approval-fixture/Cargo.toml --target-dir approval-fixture/target",
            "yield_time_ms": 30000,
            "max_runtime_secs": 30,
        })
        .to_string(),
    };
    let provider = PlannedProvider::new(vec![
        StreamPlan::Chunks(vec![
            ProviderChunk::ToolCallStart {
                id: call.id.clone(),
                name: call.name.clone(),
            },
            ProviderChunk::ToolCallArgsDelta {
                id: call.id.clone(),
                delta: call.args_json.clone(),
            },
            ProviderChunk::ToolCallComplete(call),
            ProviderChunk::Done,
        ]),
        StreamPlan::Chunks(vec![
            ProviderChunk::TextDelta("command finished".to_owned()),
            ProviderChunk::Done,
        ]),
    ]);
    let worker = spawn_test_worker_with_owned_terminal(
        config.clone(),
        session_path.clone(),
        Agent::new(provider, registry),
        workspace,
        failing_role_provider_builder(),
        authority,
        Some(authority_root),
        handles.terminal,
        Some((event_tx, event_rx)),
    )?;
    let mut app = crate::AppState::from_root_config(&config_path, &config);
    app.session_id = session_id.clone();
    app.session_log_path = session_path.clone();
    app.set_terminal_size(80, 12);
    worker.send(WorkerCommand::SubmitPrompt {
        prompt: "request the fixture command".to_owned(),
        reasoning_effort: ReasoningEffort::Medium,
    })?;
    loop {
        let message = worker.recv_with_timeout(Duration::from_secs(10))?;
        let requested = matches!(&message, WorkerMessage::Event(event)
            if matches!(event.as_ref(), RunEvent::ToolApprovalRequested { call, .. }
                if call.id == "family-real-command"));
        if let WorkerMessage::RunFailed(error) = &message {
            anyhow::bail!("real command failed before approval: {error}");
        }
        app.handle_worker_message(message)?;
        if requested {
            break;
        }
    }
    assert!(!marker.exists());
    let approval = app
        .approval_modal_view()
        .context("real worker approval must be reviewable")?;
    assert!(
        approval.session_grant_available,
        "real grant metadata: reason={:?}; analysis={:?}; effects={:?}",
        approval.session_grant_unavailable_reason, approval.analysis, approval.effects
    );
    assert_eq!(
        approval.command_family_allow_pattern.as_deref(),
        Some("cargo test*")
    );
    for _ in 0..4 {
        if app
            .approval_modal_view()
            .context("approval actions must remain visible")?
            .selected_action
            == crate::approval::ApprovalAction::AllowFamily
        {
            break;
        }
        assert!(
            app.handle_key_event(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE))?
                .is_none()
        );
    }
    let action = app
        .handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))?
        .context("selecting family permission must emit an action")?;
    let crate::app::AppAction::ApprovalFamilyDecision {
        call_id,
        approval_request_id,
        pattern,
    } = action
    else {
        anyhow::bail!("expected the selected family action");
    };
    assert_eq!(call_id, "family-real-command");
    let family_command = |scope: &str| {
        WorkerCommand::ApprovalCommand(WorkerCommandEnvelope::new(
            "family-app-state-command",
            "sigil-tui-test",
            scope,
            WorkerApprovalCommand::DecisionForFamily {
                call_id: call_id.clone(),
                approval_request_id: approval_request_id.clone(),
                pattern: pattern.clone(),
            },
        ))
    };

    let mut delayed_terminal_messages = Vec::new();
    if cancel_before_delivery {
        worker.send(WorkerCommand::CancelRun)?;
        loop {
            let message = worker.recv_with_timeout(Duration::from_secs(10))?;
            let settled = matches!(message, WorkerMessage::RunCancelled { .. });
            if matches!(
                message,
                WorkerMessage::RunFailed(_) | WorkerMessage::RunInterrupted { .. }
            ) {
                anyhow::bail!("fixture cancellation did not settle: {message:?}");
            }
            // Hold actual transport output, not fabricated state: the UI still displays the
            // request until it receives these real cancellation messages.
            delayed_terminal_messages.push(message);
            if settled {
                break;
            }
        }
        worker.send(family_command(&session_id))?;
    } else {
        worker.send(family_command(&uuid::Uuid::new_v4().to_string()))?;
    }
    let rejected = worker.recv_until(|message| {
        matches!(message, WorkerMessage::ApprovalCommandReceipt(receipt)
            if receipt.command_id == "family-app-state-command")
    })?;
    assert!(
        matches!(&rejected, WorkerMessage::ApprovalCommandReceipt(receipt)
        if receipt.route_state == crate::runner::WorkerApprovalRouteState::Rejected
            && receipt.call_id == call_id
            && receipt.approval_request_id == approval_request_id
            && !receipt.replayed)
    );
    app.handle_worker_message(rejected)?;
    assert_eq!(
        app.approval_modal_view()
            .context("a rejected route must preserve the reviewable request")?
            .delivery_feedback,
        Some("Decision not delivered; review and retry")
    );
    let (rejected_text, rejected_surface) = worker_approval_frame(&app)?;
    assert!(
        rejected_text.contains("Decision not delivered"),
        "{rejected_text}"
    );
    assert!(rejected_text.contains("Enter retry"), "{rejected_text}");
    assert_eq!(
        rejected_surface.layout.approval_request_identity,
        Some((call_id.clone(), approval_request_id.clone()))
    );
    assert!(!marker.exists());

    if cancel_before_delivery {
        for message in delayed_terminal_messages {
            app.handle_worker_message(message)?;
        }
        assert!(app.approval_modal_view().is_none());
        let (cancelled_text, cancelled_surface) = worker_approval_frame(&app)?;
        assert!(cancelled_surface.approval.is_none());
        assert!(cancelled_surface.layout.approval_request_identity.is_none());
        assert!(
            !cancelled_text.contains("Decision not delivered"),
            "{cancelled_text}"
        );
        assert!(!cancelled_text.contains("Enter retry"), "{cancelled_text}");
        assert!(cancelled_text.contains("Enter send"), "{cancelled_text}");
        assert!(
            app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))?
                .is_none(),
            "the settled cancellation must remove the old approval action"
        );
        worker.shutdown()?;
        assert!(!marker.exists());
        assert!(
            fs::read(&config_path)? == before_bytes,
            "a cancelled approval must not persist its family permission"
        );
        let entries = JsonlSessionStore::read_entries(&session_path)?;
        assert!(!entries.iter().any(|entry| {
            matches!(entry, SessionLogEntry::ToolResultV3(result)
                if result.call_id == call_id && result.facts.status == "ok")
        }));
    } else {
        assert!(matches!(
            app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))?,
            Some(crate::app::AppAction::ApprovalFamilyDecision {
                call_id: retry_call,
                approval_request_id: retry_request,
                pattern: retry_pattern,
            }) if retry_call == call_id
                && retry_request == approval_request_id
                && retry_pattern == pattern
        ));
        worker.send(family_command(&session_id))?;
        let mut accepted = false;
        loop {
            let message = worker.recv_with_timeout(Duration::from_secs(10))?;
            if let WorkerMessage::ApprovalCommandReceipt(receipt) = &message {
                assert_eq!(
                    receipt.route_state,
                    crate::runner::WorkerApprovalRouteState::DecisionAccepted
                );
                assert!(!receipt.replayed);
                accepted = true;
            }
            let finished = matches!(message, WorkerMessage::RunFinished { .. });
            if let WorkerMessage::RunFailed(error) = &message {
                anyhow::bail!("real approved command failed: {error}");
            }
            app.handle_worker_message(message)?;
            if finished {
                break;
            }
        }
        assert!(accepted);
        assert!(app.approval_modal_view().is_none());
        assert_eq!(fs::read(&marker)?, b"executed\n");
        worker.send(family_command(&session_id))?;
        let replay = worker.recv_until(|message| {
            matches!(message, WorkerMessage::ApprovalCommandReceipt(receipt)
                if receipt.command_id == "family-app-state-command")
        })?;
        assert!(
            matches!(replay, WorkerMessage::ApprovalCommandReceipt(receipt)
            if receipt.route_state == crate::runner::WorkerApprovalRouteState::DecisionAccepted
                && receipt.replayed)
        );
        worker.shutdown()?;
        assert_eq!(fs::read(&marker)?, b"executed\n");

        let after = sigil_kernel::RootConfig::load_persisted(&config_path)?;
        let mut expected = before_config;
        expected.permission.commands.allow.push(pattern);
        assert!(
            serde_json::to_value(after)? == serde_json::to_value(expected)?,
            "the accepted family must be the only persisted permission change"
        );
        let entries = JsonlSessionStore::read_entries(&session_path)?;
        assert_eq!(
            entries
                .iter()
                .filter(|entry| {
                    matches!(entry, SessionLogEntry::Control(ControlEntry::ToolExecution(execution))
                    if execution.call_id == call_id
                        && execution.status == sigil_kernel::ToolExecutionStatus::Started)
                })
                .count(),
            1
        );
        assert_eq!(
            entries
                .iter()
                .filter(|entry| {
                    matches!(entry, SessionLogEntry::ToolResultV3(result)
                    if result.call_id == call_id && result.facts.status == "ok")
                })
                .count(),
            1
        );
    }
    Ok(())
}

#[cfg(unix)]
fn worker_approval_frame(app: &crate::AppState) -> Result<(String, crate::surface::SurfaceModel)> {
    use ratatui::{Terminal, backend::TestBackend, layout::Rect};

    let surface = crate::surface_adapter::build_surface_model(
        Rect::new(0, 0, 80, 12),
        app,
        crate::surface::SurfaceState {
            frame_generation: 1,
            terminal_epoch: 1,
        },
    );
    let mut terminal = Terminal::new(TestBackend::new(80, 12))?;
    terminal.draw(|frame| crate::ui::render_surface(frame, &surface))?;
    let buffer = terminal.backend().buffer();
    let text = (0..12)
        .map(|row| {
            (0..80)
                .map(|column| buffer[(column, row)].symbol())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n");
    Ok((text, surface))
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
        approval_session_id(&approval_request),
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
            SessionLogEntry::ToolResultV3(result)
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

fn approval_session_id(message: &WorkerMessage) -> &str {
    let WorkerMessage::Event(event) = message else {
        panic!("expected approval request event");
    };
    let RunEvent::ToolApprovalRequested {
        approval_identity, ..
    } = event.as_ref()
    else {
        panic!("expected approval request event");
    };
    &approval_identity.session_id
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
        approval_session_id(&approval_request),
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
    let root_config = test_root_config(&workspace_root, "planned", "planned-model");
    let provider = PlannedProvider::new(vec![]);
    let agent = Agent::new(provider, ToolRegistry::new());
    let worker = spawn_test_worker(root_config, session_log_path.clone(), agent, workspace_root)?;
    worker.recv_until(|message| matches!(message, WorkerMessage::WorkerReady))?;
    let session = sigil_kernel::Session::load_from_store(
        "planned",
        "planned-model",
        sigil_kernel::JsonlSessionStore::open_existing(&session_log_path)?,
    )?;

    worker.send(approval_command(
        "command-stray",
        session.session_scope_id(),
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

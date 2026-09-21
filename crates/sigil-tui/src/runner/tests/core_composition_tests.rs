use std::{fs, sync::Arc, time::Duration};

use anyhow::{Context, Result, bail};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use sigil_kernel::EventHandler;
use sigil_kernel::{
    Agent, AssistantMessageKind, ControlEntry, ConversationInputKind, ConversationInputStatus,
    ConversationInputTarget, JsonlSessionStore, ProviderChunk, ReasoningEffort, RootConfig,
    RunEvent, RuntimeCompositionConfig, SessionLogEntry, ToolCall, ToolRegistry,
};
use tempfile::tempdir;

use super::{
    super::{WorkerCommand, WorkerMessage},
    common::{
        PlannedProvider, StreamPlan, failing_role_provider_builder,
        routed_unauthenticated_test_root_config,
        spawn_test_worker_with_existing_authority_composition,
        spawn_test_worker_with_role_provider_builder, test_authority_composition,
    },
};

fn core_root_config(workspace_root: &std::path::Path) -> Result<RootConfig> {
    let mut config = routed_unauthenticated_test_root_config(workspace_root, "planned-model");
    config.composition = RuntimeCompositionConfig::core();
    // This unselected role is deliberately invalid and must not prevent ordinary conversation.
    config.task.planner.model = Some("unselected-role-model".to_owned());
    config.storage.state_root =
        sigil_kernel::StorageRoot::Path(workspace_root.join("state").display().to_string());
    config.storage.cache_root =
        sigil_kernel::StorageRoot::Path(workspace_root.join("cache").display().to_string());
    config.save(&workspace_root.join("sigil.toml"))?;
    config.with_effective_composition()
}

#[test]
fn core_worker_approves_and_executes_file_write_and_one_shot_command() -> Result<()> {
    let runtime = tokio::runtime::Runtime::new()?;
    let _runtime_guard = runtime.enter();
    let temp = tempdir()?;
    let workspace_root = temp.path().to_path_buf();
    let session_log_path = temp.path().join(".sigil/sessions/core-tools.jsonl");
    let config = core_root_config(&workspace_root)?;
    let (provider_name, route) =
        sigil_runtime::provider_connections::resolve_default_model_route(&config)?;
    let mut routed_session = sigil_kernel::Session::new_with_route(provider_name, route)
        .with_store(JsonlSessionStore::new(&session_log_path)?);
    routed_session.ensure_identity_entry()?;
    let (authority, _authority_root) = test_authority_composition(&workspace_root)?;
    let paths = sigil_tools_builtin::BuiltinToolPaths::workspace_defaults(&workspace_root);
    let scratch = sigil_runtime::authority_scratch_control(paths.scratch_root.clone());
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
            lifecycle_route: None,
            executor: Arc::clone(&authority.command_execution)
                as Arc<dyn sigil_tools_builtin::ManagedTerminalExecutionPortV1>,
        },
    );
    assert!(handles.terminal.is_some());
    let provider = PlannedProvider::new(vec![
        tool_call_plan(
            "core-write",
            "write_file",
            serde_json::json!({
                "path": "core-note.txt",
                "content": "written through approved Core execution\n"
            }),
        ),
        tool_call_plan(
            "core-command",
            "exec_command",
            serde_json::json!({
            "command": "printf core-command-ok > core-command.txt && cat core-command.txt"
            }),
        ),
        StreamPlan::Chunks(vec![
            ProviderChunk::TextDelta("core tools finished".to_owned()),
            ProviderChunk::Done,
        ]),
    ]);
    let projection_store = JsonlSessionStore::new(&session_log_path)?;
    let worker = spawn_test_worker_with_existing_authority_composition(
        config.clone(),
        session_log_path.clone(),
        Agent::new(provider, registry),
        workspace_root.clone(),
        Arc::clone(&authority),
    )?;
    worker.recv_until(|message| matches!(message, WorkerMessage::WorkerReady))?;
    let durable_records = JsonlSessionStore::read_event_records(&session_log_path)?;
    let mut app = crate::AppState::from_root_config(&workspace_root.join("sigil.toml"), &config);
    app.session_log_path = session_log_path.clone();
    app.session_id = durable_records
        .first()
        .context("worker must initialize its durable session before becoming ready")?
        .stored_event()
        .session_id
        .as_str()
        .to_owned();
    let application = crate::application_bridge::tests::connect_real_worker(
        &workspace_root.join("sigil.toml"),
        &workspace_root,
        &session_log_path,
        &app.session_id,
        worker.command_sender(),
        &authority,
        sigil_runtime::RuntimeSessionProjectionOwner::from_store(&projection_store),
    )?;
    sigil_runtime::application_run::application_run_context_view(
        &workspace_root.join("sigil.toml"),
        &workspace_root,
        &session_log_path,
        &app.session_id,
    )
    .context("Core bridge fixture run-context projection")?;
    let initial = runtime
        .block_on(application.refresh())
        .context("Core bridge initial idle projection")?;
    assert!(!initial.approval.pending);
    application
        .try_execute_action(
            &crate::app::AppAction::SubmitPrompt(
                "write a note and run the one-shot command".to_owned(),
            ),
            None,
            None,
        )
        .context("Core bridge submit admission")?
        .context("submit must use the application bridge")?;

    let mut approved_calls = Vec::new();
    let finished_entries = loop {
        match worker.recv_with_timeout(Duration::from_secs(15))? {
            WorkerMessage::Event(event) => {
                if let RunEvent::ToolApprovalRequested {
                    call,
                    approval_identity,
                    ..
                } = event.as_ref()
                {
                    if call.id == "core-write" {
                        assert!(!workspace_root.join("core-note.txt").exists());
                    }
                    approved_calls.push(call.id.clone());
                    assert_eq!(approval_identity.session_id, app.session_id);
                    let outbox = sigil_kernel::PublicEventOutboxProjectionV1::from_records(
                        &JsonlSessionStore::read_event_records(&session_log_path)?,
                    )?;
                    assert!(outbox.events_in_order().iter().any(|entry| matches!(
                        &entry.event.event,
                        sigil_kernel::PublicRunEventKind::ApprovalRequested { .. }
                    )));
                    assert_eq!(
                        outbox.pending_for_adapter("tui").len(),
                        outbox.events_in_order().len(),
                        "enqueueing a native card must not ACK the public projection"
                    );
                    app.handle(event.as_ref().clone())?;
                    let action = app
                        .handle_key_event(KeyEvent::new(KeyCode::Char('y'), KeyModifiers::NONE))?
                        .context("y must produce an approval action")?;
                    assert!(matches!(
                        action,
                        crate::app::AppAction::ApprovalDecision { approved: true, .. }
                    ));
                    // No application refresh since idle: the shipping command edge must obtain
                    // the new durable approval binding even for the first immediate keypress.
                    application
                        .try_execute_action(&action, None, None)
                        .context("Core bridge approval admission")?
                        .context("approval must use the shipping application bridge")?;
                }
            }
            WorkerMessage::RunFinished { result, entries } => {
                assert_eq!(result.final_text, "core tools finished");
                assert_eq!(result.tool_calls, 2);
                break entries;
            }
            WorkerMessage::RunFailed(error) => bail!("core tool run failed: {error}"),
            _ => {}
        }
    };
    assert_eq!(
        fs::read_to_string(workspace_root.join("core-note.txt"))?,
        "written through approved Core execution\n"
    );
    let results = finished_entries
        .iter()
        .filter_map(|entry| match entry {
            SessionLogEntry::ToolResultV3(result) => Some(result),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(results.len(), 2);
    assert!(
        results.iter().all(|result| result.facts.status == "ok"),
        "tool results: {results:?}"
    );
    assert_eq!(approved_calls, ["core-write", "core-command"]);
    assert_eq!(
        fs::read_to_string(workspace_root.join("core-command.txt"))?,
        "core-command-ok"
    );
    assert!(
        results[1]
            .initial_model_view
            .preview
            .contains("core-command-ok")
    );
    let projected = runtime
        .block_on(application.refresh())
        .context("Core bridge final projection")?;
    assert!(!projected.approval.pending);
    assert!(projected.run.active_binding.is_none());
    app.apply_application_projection(&projected);
    assert!(!application.read_handle()?.read_event_records()?.is_empty());
    let mut acknowledged = 0;
    loop {
        let batch = runtime.block_on(application.refresh_delivery())?;
        for notice in batch.notices {
            app.handle_worker_message(WorkerMessage::Notice(notice.as_str().to_owned()))?;
        }
        let delivered = application.take_applied_delivery_event_ids()?;
        acknowledged +=
            runtime.block_on(application.acknowledge_public_events(delivered, &batch.frontier))?;
        if !batch.has_more {
            break;
        }
    }
    assert!(acknowledged > 0);
    let outbox = sigil_kernel::PublicEventOutboxProjectionV1::from_records(
        &JsonlSessionStore::read_event_records(&session_log_path)?,
    )?;
    assert!(outbox.pending_for_adapter("tui").is_empty());
    let public = outbox.events_in_order();
    assert_eq!(
        public
            .iter()
            .filter(|entry| matches!(
                entry.event.event,
                sigil_kernel::PublicRunEventKind::RunStarted { .. }
            ))
            .count(),
        1
    );
    assert_eq!(
        public
            .iter()
            .filter(|entry| matches!(
                entry.event.event,
                sigil_kernel::PublicRunEventKind::RunFinished { .. }
            ))
            .count(),
        1
    );
    worker.shutdown()?;
    Ok(())
}

fn tool_call_plan(id: &str, name: &str, arguments: serde_json::Value) -> StreamPlan {
    let args_json = arguments.to_string();
    StreamPlan::Chunks(vec![
        ProviderChunk::ToolCallStart {
            id: id.to_owned(),
            name: name.to_owned(),
        },
        ProviderChunk::ToolCallArgsDelta {
            id: id.to_owned(),
            delta: args_json.clone(),
        },
        ProviderChunk::ToolCallComplete(ToolCall {
            id: id.to_owned(),
            name: name.to_owned(),
            args_json,
        }),
        ProviderChunk::Done,
    ])
}

#[test]
fn core_worker_persists_public_start_before_immediate_cancel() -> Result<()> {
    let temp = tempdir()?;
    let workspace_root = temp.path().to_path_buf();
    let session_log_path = temp.path().join(".sigil/sessions/core-early-cancel.jsonl");
    let config = core_root_config(&workspace_root)?;
    let (provider_name, route) =
        sigil_runtime::provider_connections::resolve_default_model_route(&config)?;
    let mut session = sigil_kernel::Session::new_with_route(provider_name, route)
        .with_store(JsonlSessionStore::new(&session_log_path)?);
    session.ensure_identity_entry()?;
    let gate = Arc::new(tokio::sync::Notify::new());
    let provider = PlannedProvider::new(vec![StreamPlan::GatedChunks {
        gate,
        chunks: vec![
            ProviderChunk::TextDelta("must not finish".to_owned()),
            ProviderChunk::Done,
        ],
    }]);
    let worker = spawn_test_worker_with_role_provider_builder(
        config,
        session_log_path.clone(),
        Agent::new(provider, ToolRegistry::new()),
        workspace_root,
        failing_role_provider_builder(),
    )?;
    worker.send(WorkerCommand::SubmitPrompt {
        prompt: "cancel immediately after foreground admission".to_owned(),
        reasoning_effort: ReasoningEffort::Max,
    })?;
    let mut observed_started = false;
    loop {
        match worker.recv_with_timeout(Duration::from_secs(10))? {
            WorkerMessage::RunStarted { .. } => {
                // Cancel at admission without waiting for provider execution. Sending this
                // before RunStarted could let the urgent command overtake SubmitPrompt.
                worker.send(WorkerCommand::CancelRun)?;
                let records = JsonlSessionStore::read_event_records(&session_log_path)?;
                assert!(records.iter().any(|record| matches!(
                    sigil_kernel::conversation_run_lifecycle_record_from_stream(record),
                    Ok(Some(sigil_kernel::ConversationRunLifecycleRecordV1::ConversationRunStartedV1(_)))
                )), "native RunStarted must follow durable foreground admission");
                observed_started = true;
            }
            WorkerMessage::RunCancelled { .. } => break,
            WorkerMessage::RunFailed(error) => bail!("early Core cancellation failed: {error}"),
            WorkerMessage::RunInterrupted { .. } => {
                bail!("early Core cancellation was not quiescent")
            }
            WorkerMessage::RunFinished { .. } => {
                bail!("the gated provider finished before cancellation")
            }
            _ => {}
        }
    }
    assert!(observed_started);
    worker.shutdown()?;
    let records = JsonlSessionStore::read_event_records(&session_log_path)?;
    let lifecycle = records
        .iter()
        .map(sigil_kernel::conversation_run_lifecycle_record_from_stream)
        .collect::<Result<Vec<_>>>()?
        .into_iter()
        .flatten()
        .collect::<Vec<_>>();
    let [
        sigil_kernel::ConversationRunLifecycleRecordV1::ConversationRunStartedV1(started),
        sigil_kernel::ConversationRunLifecycleRecordV1::ConversationRunFinalizedV1(finalized),
    ] = lifecycle.as_slice()
    else {
        bail!("early cancellation must close exactly one admitted foreground run");
    };
    assert_eq!(started.run_id(), finalized.run_id());
    assert_eq!(
        finalized.status(),
        sigil_kernel::ConversationRunTerminalStatusV1::Cancelled
    );
    let outbox = sigil_kernel::PublicEventOutboxProjectionV1::from_records(&records)?;
    let events = outbox.events_in_order();
    assert!(matches!(
        events.first().map(|entry| &entry.event.event),
        Some(sigil_kernel::PublicRunEventKind::RunStarted { .. })
    ));
    assert!(matches!(
        events.last().map(|entry| &entry.event.event),
        Some(sigil_kernel::PublicRunEventKind::RunCancelled)
    ));
    assert_eq!(
        events
            .iter()
            .filter(|entry| matches!(
                entry.event.event,
                sigil_kernel::PublicRunEventKind::RunStarted { .. }
            ))
            .count(),
        1
    );
    assert_eq!(
        events
            .iter()
            .filter(|entry| matches!(
                entry.event.event,
                sigil_kernel::PublicRunEventKind::RunCancelled
            ))
            .count(),
        1
    );
    Ok(())
}

#[test]
fn core_worker_cancels_blocked_provider_and_continues_same_session_without_late_effects()
-> Result<()> {
    let temp = tempdir()?;
    let workspace_root = temp.path().to_path_buf();
    let session_log_path = temp.path().join(".sigil/sessions/core-cancel.jsonl");
    let mut config = core_root_config(&workspace_root)?;
    config.permission.mode = sigil_kernel::PermissionMode::AutoEdit;
    let (authority, _authority_root) = test_authority_composition(&workspace_root)?;
    let paths = sigil_tools_builtin::BuiltinToolPaths::workspace_defaults(&workspace_root);
    let mut registry = ToolRegistry::new();
    let scratch = sigil_runtime::authority_scratch_control(paths.scratch_root.clone());
    sigil_tools_builtin::register_builtin_tools_with_selection(
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
            lifecycle_route: None,
            executor: Arc::clone(&authority.command_execution)
                as Arc<dyn sigil_tools_builtin::ManagedTerminalExecutionPortV1>,
        },
    );
    let gate = Arc::new(tokio::sync::Notify::new());
    let StreamPlan::Chunks(late_tool_chunks) = tool_call_plan(
        "cancelled-write",
        "write_file",
        serde_json::json!({"path": "late-effect.txt", "content": "must never be written"}),
    ) else {
        unreachable!("tool fixture returns chunks")
    };
    let (provider, stream_started) = PlannedProvider::new_with_stream_start_signal(vec![
        StreamPlan::GatedChunks {
            gate: Arc::clone(&gate),
            chunks: late_tool_chunks,
        },
        StreamPlan::Chunks(vec![
            ProviderChunk::TextDelta("continued after cancellation".to_owned()),
            ProviderChunk::Done,
        ]),
    ]);
    let worker = spawn_test_worker_with_existing_authority_composition(
        config,
        session_log_path.clone(),
        Agent::new(provider, registry),
        workspace_root.clone(),
        authority,
    )?;
    worker.send(WorkerCommand::SubmitPrompt {
        prompt: "block before returning the write call".to_owned(),
        reasoning_effort: ReasoningEffort::Max,
    })?;
    stream_started.recv_timeout(Duration::from_secs(5))?;
    worker.send(WorkerCommand::CancelRun)?;
    let mut cancellation_terminals = 0;
    loop {
        match worker.recv_with_timeout(Duration::from_secs(10))? {
            WorkerMessage::RunCancelled {
                session_log_path: cancelled_path,
                ..
            } => {
                assert_eq!(cancelled_path, session_log_path);
                cancellation_terminals += 1;
                break;
            }
            WorkerMessage::RunInterrupted { .. } => {
                bail!("provider cancellation was not quiescent")
            }
            WorkerMessage::RunFailed(error) => bail!("Core cancellation failed: {error}"),
            WorkerMessage::RunFinished { .. } => bail!("blocked run completed before cancellation"),
            _ => {}
        }
    }
    // Release the old stream only after the cancellation terminal. A stale owner must be unable
    // to execute its auto-approved write or consume the next provider turn.
    gate.notify_one();
    worker.send(WorkerCommand::SubmitPrompt {
        prompt: "continue in the same Core session".to_owned(),
        reasoning_effort: ReasoningEffort::Max,
    })?;
    loop {
        match worker.recv_with_timeout(Duration::from_secs(10))? {
            WorkerMessage::RunFinished { result, .. } => {
                assert_eq!(result.final_text, "continued after cancellation");
                assert_eq!(result.tool_calls, 0);
                break;
            }
            WorkerMessage::RunCancelled { .. } | WorkerMessage::RunInterrupted { .. } => {
                cancellation_terminals += 1;
            }
            WorkerMessage::RunFailed(error) => bail!("Core continuation failed: {error}"),
            _ => {}
        }
    }
    worker.shutdown()?;
    assert_eq!(cancellation_terminals, 1);
    assert!(!workspace_root.join("late-effect.txt").exists());
    let records = JsonlSessionStore::read_event_records(&session_log_path)?;
    let terminals = records
        .iter()
        .map(|record| &record.stored_event().payload)
        .filter(|payload| {
            payload.get("record").and_then(serde_json::Value::as_str) == Some("finalized")
        })
        .collect::<Vec<_>>();
    assert_eq!(terminals.len(), 1);
    assert_eq!(terminals[0]["outcome"], "cancelled");
    assert_eq!(terminals[0]["cleanup_complete"], true);
    let entries = JsonlSessionStore::read_entries(&session_log_path)?;
    assert_eq!(
        entries
            .iter()
            .filter(|entry| matches!(
                entry,
                SessionLogEntry::Control(ControlEntry::SessionCompositionBound(_))
            ))
            .count(),
        1
    );
    assert!(entries.iter().all(|entry| !matches!(
        entry,
        SessionLogEntry::ToolResultV3(_) | SessionLogEntry::Control(ControlEntry::ToolExecution(_))
    )));
    Ok(())
}

#[test]
fn core_worker_runs_chat_queue_after_rejecting_plan_and_task_without_role_runtime() -> Result<()> {
    let temp = tempdir()?;
    let workspace_root = temp.path().to_path_buf();
    let session_log_path = temp.path().join(".sigil/sessions/core-chat.jsonl");
    let invalid_profile = temp.path().join(".sigil/agents/broken");
    fs::create_dir_all(&invalid_profile)?;
    fs::write(invalid_profile.join("agent.toml"), "this is not valid TOML")?;
    let config = core_root_config(&workspace_root)?;
    let gate = Arc::new(tokio::sync::Notify::new());
    let (provider, stream_started) = PlannedProvider::new_with_stream_start_signal(vec![
        StreamPlan::GatedChunks {
            gate: Arc::clone(&gate),
            chunks: vec![
                ProviderChunk::TextDelta("first core answer".to_owned()),
                ProviderChunk::Done,
            ],
        },
        StreamPlan::Chunks(vec![
            ProviderChunk::TextDelta("queued core answer".to_owned()),
            ProviderChunk::Done,
        ]),
    ]);
    let worker = spawn_test_worker_with_role_provider_builder(
        config,
        session_log_path.clone(),
        Agent::new(provider, ToolRegistry::new()),
        workspace_root,
        failing_role_provider_builder(),
    )?;

    for command in [
        WorkerCommand::SubmitPlanPrompt {
            prompt: "unavailable plan".to_owned(),
            reasoning_effort: ReasoningEffort::Max,
        },
        WorkerCommand::SubmitTask {
            prompt: "unavailable task".to_owned(),
        },
    ] {
        worker.send(command)?;
        let failure =
            worker.recv_until(|message| matches!(message, WorkerMessage::RunFailed(_)))?;
        assert!(matches!(failure, WorkerMessage::RunFailed(error)
            if error.contains("task orchestration is unavailable")));
    }

    worker.send(WorkerCommand::SubmitPrompt {
        prompt: "ordinary core chat".to_owned(),
        reasoning_effort: ReasoningEffort::Max,
    })?;
    stream_started.recv_timeout(Duration::from_secs(5))?;
    worker.send(WorkerCommand::QueueConversationInput {
        prompt: "queued core chat".to_owned(),
        kind: ConversationInputKind::Chat,
        target: ConversationInputTarget::MainThread,
        reasoning_effort: ReasoningEffort::Max,
    })?;
    worker.recv_until(|message| {
        matches!(message, WorkerMessage::ConversationQueueUpdated { items, .. } if items.len() == 1)
    })?;
    gate.notify_one();

    let mut answers = Vec::new();
    let mut final_message_ids = std::collections::HashSet::new();
    while answers.len() < 2 {
        match worker.recv_with_timeout(Duration::from_secs(10))? {
            WorkerMessage::Event(event) => {
                if let RunEvent::AssistantMessage(message) = event.as_ref()
                    && message.assistant_kind == Some(AssistantMessageKind::FinalAnswer)
                    && final_message_ids.insert(message.id.clone())
                {
                    answers.push(message.content.clone().unwrap_or_default());
                }
            }
            WorkerMessage::RunFinished { result, .. }
                if result
                    .final_message_id
                    .as_ref()
                    .is_none_or(|id| final_message_ids.insert(id.clone())) =>
            {
                answers.push(result.final_text)
            }
            WorkerMessage::RunFailed(error) => bail!("core conversation failed: {error}"),
            _ => {}
        }
    }
    assert_eq!(answers, ["first core answer", "queued core answer"]);
    worker.shutdown()?;

    let entries = JsonlSessionStore::read_entries(&session_log_path)?;
    let seals = entries
        .iter()
        .filter_map(|entry| match entry {
            SessionLogEntry::Control(ControlEntry::SessionCompositionBound(snapshot)) => {
                Some(snapshot)
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(seals.len(), 1);
    assert!(seals[0].capabilities.is_empty());
    assert!(entries.iter().any(|entry| matches!(entry,
        SessionLogEntry::Control(ControlEntry::ConversationInputStatusChanged(status))
            if status.status == ConversationInputStatus::Delivered
    )));
    assert!(!entries.iter().any(|entry| matches!(
        entry,
        SessionLogEntry::Control(
            ControlEntry::TaskRun(_)
                | ControlEntry::PlanReviewAttempt(_)
                | ControlEntry::AgentThreadStarted(_)
        )
    )));
    Ok(())
}

#[test]
fn core_worker_user_input_resolution_clears_durable_public_pending() -> Result<()> {
    use sigil_kernel::{
        ConversationRunLifecycleRecordV1, PublicRunEventKind, UserInputAnswerV1,
        UserInputAnswerValueV1, UserInputDecisionV1, UserInputResolutionV1, UserInputStatusV1,
    };
    use sigil_runtime::RuntimeApplicationProjectionSource;

    let runtime = tokio::runtime::Runtime::new()?;
    let cases = [
        (
            "submitted",
            UserInputDecisionV1::Submitted {
                answers: vec![UserInputAnswerV1 {
                    question_id: "scope".to_owned(),
                    value: UserInputAnswerValueV1::Text {
                        value: "runtime".to_owned(),
                    },
                }],
            },
            UserInputResolutionV1::Consumed,
        ),
        (
            "declined",
            UserInputDecisionV1::Declined,
            UserInputResolutionV1::Declined,
        ),
        (
            "run-cancelled",
            UserInputDecisionV1::RunCancelled,
            UserInputResolutionV1::RunCancelled,
        ),
    ];
    for (case, decision, expected_resolution) in cases {
        let continues = matches!(decision, UserInputDecisionV1::Submitted { .. });
        let temp = tempdir()?;
        let workspace_root = temp.path().to_path_buf();
        let session_log_path = temp
            .path()
            .join(format!(".sigil/sessions/core-user-input-{case}.jsonl"));
        let config = core_root_config(&workspace_root)?;
        let (provider_name, route) =
            sigil_runtime::provider_connections::resolve_default_model_route(&config)?;
        let mut session = sigil_kernel::Session::new_with_route(provider_name, route)
            .with_store(JsonlSessionStore::new(&session_log_path)?);
        session.ensure_identity_entry()?;
        let session_id = session.session_scope_id().to_owned();
        let mut plans = vec![tool_call_plan(
            "core-question",
            sigil_kernel::REQUEST_USER_INPUT_TOOL_NAME,
            serde_json::json!({
                "questions": [{
                    "id": "scope",
                    "question": "Which module should be inspected?"
                }]
            }),
        )];
        if continues {
            plans.push(StreamPlan::Chunks(vec![
                ProviderChunk::TextDelta("The runtime module is selected.".to_owned()),
                ProviderChunk::Done,
            ]));
        }
        let worker = spawn_test_worker_with_role_provider_builder(
            config,
            session_log_path.clone(),
            Agent::new(PlannedProvider::new(plans), ToolRegistry::new()),
            workspace_root.clone(),
            failing_role_provider_builder(),
        )?;
        worker.send(WorkerCommand::SubmitPrompt {
            prompt: "ask which module to inspect before continuing".to_owned(),
            reasoning_effort: ReasoningEffort::Max,
        })?;
        let request = loop {
            match worker.recv_with_timeout(Duration::from_secs(10))? {
                WorkerMessage::UserInputRequested { request, .. } => break request,
                WorkerMessage::RunFailed(error) => {
                    bail!("Core {case} question failed: {error}");
                }
                WorkerMessage::RunFinished { .. } => {
                    bail!("Core {case} finished without the typed question");
                }
                _ => {}
            }
        };
        assert_eq!(request.status, UserInputStatusV1::Requested);
        assert_eq!(request.identity.session_scope_id.as_str(), session_id);
        let binding = sigil_runtime::RuntimeSessionProjectionBinding::new(
            workspace_root.join("sigil.toml"),
            workspace_root,
            session_log_path.clone(),
            session_id,
            sigil_application::ApplicationInstanceId::new("core-user-input-test")?,
            sigil_application::AuthenticatedSubject::new("local-user")?,
            Some(sigil_application::WorkspaceScopeId::new(
                "fixture-workspace",
            )?),
            1,
            1,
            1,
            1,
        )?
        .with_owner(sigil_runtime::RuntimeSessionProjectionOwner::from_store(
            &JsonlSessionStore::new(&session_log_path)?,
        ));
        let pending = runtime.block_on(binding.open_projection(
            sigil_application::OpenProjectionRequest {
                scope: binding.scope().clone(),
                observer_generation: 1,
                resume_from: None,
            },
        ))?;
        assert!(pending.envelope.projection.user_input.pending);
        worker.send(WorkerCommand::SubmitUserInputDecision {
            command_id: Some(format!("core-user-input-{case}-decision")),
            request_id: request.identity.request_id.as_str().to_owned(),
            generation: request.identity.generation,
            expected_request_hash: request.request_hash.clone(),
            decision,
        })?;
        loop {
            match worker.recv_with_timeout(Duration::from_secs(10))? {
                WorkerMessage::UserInputDecisionApplied {
                    request: applied,
                    continuation_started,
                    ..
                } => {
                    assert_eq!(continuation_started, continues);
                    assert_eq!(applied.identity, request.identity);
                    if !continues {
                        assert_eq!(applied.status, UserInputStatusV1::Resolved);
                        assert_eq!(applied.resolution, Some(expected_resolution.clone()));
                        break;
                    }
                }
                WorkerMessage::RunFinished { result, .. } if continues => {
                    assert_eq!(result.final_text, "The runtime module is selected.");
                    assert!(result.final_message_id.is_some());
                    break;
                }
                WorkerMessage::RunFailed(error) => {
                    bail!("Core {case} input decision failed: {error}");
                }
                WorkerMessage::RunFinished { .. } => {
                    bail!("Core {case} unexpectedly continued the provider");
                }
                _ => {}
            }
        }
        let records = JsonlSessionStore::read_event_records(&session_log_path)?;
        let outbox = sigil_kernel::PublicEventOutboxProjectionV1::from_records(&records)?;
        let input_updates = outbox
            .events_in_order()
            .into_iter()
            .filter_map(|entry| match &entry.event.event {
                PublicRunEventKind::UserInputChanged {
                    request_id,
                    generation,
                    request_hash,
                    status,
                    request: public_request,
                } if request_id == request.identity.request_id.as_str()
                    && *generation == request.identity.generation
                    && request_hash == &request.request_hash =>
                {
                    Some((*status, public_request))
                }
                _ => None,
            })
            .collect::<Vec<_>>();
        let (status, resolved) = input_updates
            .last()
            .context("resolved input must retain a canonical public update")?;
        assert_eq!(*status, UserInputStatusV1::Resolved, "case {case}");
        assert_eq!(
            resolved.resolution,
            Some(expected_resolution),
            "case {case}"
        );
        let lifecycle = records
            .iter()
            .map(sigil_kernel::conversation_run_lifecycle_record_from_stream)
            .collect::<Result<Vec<_>>>()?
            .into_iter()
            .flatten()
            .collect::<Vec<_>>();
        let expected_runs = if continues { 2 } else { 1 };
        assert_eq!(
            lifecycle
                .iter()
                .filter(|entry| matches!(
                    entry,
                    ConversationRunLifecycleRecordV1::ConversationRunStartedV1(_)
                ))
                .count(),
            expected_runs,
            "case {case} must not restart the resolved input's original root"
        );
        assert_eq!(
            lifecycle
                .iter()
                .filter(|entry| matches!(
                    entry,
                    ConversationRunLifecycleRecordV1::ConversationRunFinalizedV1(_)
                ))
                .count(),
            expected_runs,
            "case {case} must not duplicate the original awaiting-input terminal"
        );
        assert_eq!(
            outbox.pending_for_adapter("tui").len(),
            outbox.events_in_order().len()
        );
        let resolved_snapshot = runtime.block_on(binding.open_projection(
            sigil_application::OpenProjectionRequest {
                scope: binding.scope().clone(),
                observer_generation: 1,
                resume_from: None,
            },
        ))?;
        let projection = resolved_snapshot.envelope.projection;
        assert!(!projection.user_input.pending, "case {case}");
        assert!(projection.run.active_binding.is_none(), "case {case}");
        worker.shutdown()?;
    }
    Ok(())
}

#[test]
fn application_queue_commit_waits_for_owner_and_replays_after_worker_reopen() -> Result<()> {
    let runtime = tokio::runtime::Runtime::new()?;
    let _runtime_guard = runtime.enter();
    let temp = tempdir()?;
    let workspace = temp.path().to_path_buf();
    let session_path = workspace.join(".sigil/sessions/application-queue.jsonl");
    let config = core_root_config(&workspace)?;
    let (provider, route) =
        sigil_runtime::provider_connections::resolve_default_model_route(&config)?;
    let store = JsonlSessionStore::new(&session_path)?;
    let mut session =
        sigil_kernel::Session::new_with_route(provider, route).with_store(store.clone());
    session.ensure_identity_entry()?;
    let session_id = session.session_scope_id().to_owned();
    let (authority, _authority_root) = test_authority_composition(&workspace)?;
    let mut worker = spawn_test_worker_with_existing_authority_composition(
        config.clone(),
        session_path.clone(),
        Agent::new(PlannedProvider::new(Vec::new()), ToolRegistry::new()),
        workspace.clone(),
        Arc::clone(&authority),
    )?;
    worker.recv_until(|message| matches!(message, WorkerMessage::WorkerReady))?;
    let application = crate::application_bridge::tests::connect_real_worker(
        &workspace.join("sigil.toml"),
        &workspace,
        &session_path,
        &session_id,
        worker.command_sender(),
        &authority,
        sigil_runtime::RuntimeSessionProjectionOwner::from_store(&store),
    )?;
    runtime
        .block_on(application.refresh())
        .context("initial queue application projection")?;
    let action = crate::app::AppAction::SetConversationQueuePaused { paused: true };
    let request = application
        .prepare_action(&action, None, None)?
        .context("queue request")?;
    let receipt = runtime
        .block_on(application.execute_prepared(request.clone()))
        .context("first queue application admission")?;
    assert!(
        matches!(
            receipt,
            sigil_application::ApplicationCommandReceipt::Settled(_)
        ),
        "actual owner commit must be known before dispatch releases its guard: {receipt:?}"
    );
    let binding =
        sigil_runtime::application_operation_owner::application_operation_binding(&request)?
            .context("causal queue binding")?;
    assert!(
        sigil_kernel::session::reconcile_application_operation(&store.read_handle(), &binding)?
            .is_some()
    );
    worker.stop()?;
    drop(application);
    worker = spawn_test_worker_with_existing_authority_composition(
        config,
        session_path.clone(),
        Agent::new(PlannedProvider::new(Vec::new()), ToolRegistry::new()),
        workspace.clone(),
        Arc::clone(&authority),
    )?;
    worker.recv_until(|message| matches!(message, WorkerMessage::WorkerReady))?;
    let reopened = crate::application_bridge::tests::connect_real_worker(
        &workspace.join("sigil.toml"),
        &workspace,
        &session_path,
        &session_id,
        worker.command_sender(),
        &authority,
        sigil_runtime::RuntimeSessionProjectionOwner::from_store(&store),
    )?;
    runtime.block_on(reopened.refresh())?;
    assert!(matches!(
        runtime.block_on(reopened.execute_prepared(request))?,
        sigil_application::ApplicationCommandReceipt::Replayed(_)
    ));
    let records = store.read_handle().read_event_records()?;
    assert_eq!(
        records
            .iter()
            .filter(|record| matches!(
                record.session_log_entry(),
                Ok(Some(SessionLogEntry::Control(
                    ControlEntry::ConversationInputQueueControl(_)
                )))
            ))
            .count(),
        1
    );
    worker.shutdown()?;
    Ok(())
}

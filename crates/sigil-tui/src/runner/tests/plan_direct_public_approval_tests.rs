use std::{fs, sync::Arc, time::Duration};

use anyhow::{Context, Result, bail};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use sigil_kernel::{
    Agent, ControlEntry, EventHandler, JsonlSessionStore, PlanArtifactProjection,
    PlanTaskStartMode, ProviderChunk, PublicRunEventKind, ReasoningEffort, RunEvent,
    SessionLogEntry, TaskRunStatus, ToolCall, ToolRegistry,
};
use tempfile::tempdir;

use super::{
    super::{WorkerCommand, WorkerMessage},
    common::{
        PlannedProvider, StreamPlan, planned_role_provider_builder,
        routed_unauthenticated_test_root_config,
        spawn_test_worker_with_role_provider_builder_and_authority,
        submit_plan_review_result_chunks, test_authority_composition,
    },
};

#[test]
fn approved_direct_task_publishes_approval_and_accepts_the_real_application_command() -> Result<()>
{
    let runtime = tokio::runtime::Runtime::new()?;
    let _runtime_guard = runtime.enter();
    let temp = tempdir()?;
    let workspace = temp.path().to_path_buf();
    let config_path = workspace.join("sigil.toml");
    let session_path = workspace.join(".sigil/sessions/direct-approval.jsonl");
    let mut config = routed_unauthenticated_test_root_config(&workspace, "planned-model");
    config.storage.state_root =
        sigil_kernel::StorageRoot::Path(workspace.join("state").display().to_string());
    config.storage.cache_root =
        sigil_kernel::StorageRoot::Path(workspace.join("cache").display().to_string());
    config.save(&config_path)?;
    let (provider_name, route) =
        sigil_runtime::provider_connections::resolve_default_model_route(&config)?;
    let mut routed_session = sigil_kernel::Session::new_with_route(provider_name, route)
        .with_store(JsonlSessionStore::new(&session_path)?);
    routed_session.ensure_identity_entry()?;
    let (authority, authority_root) = test_authority_composition(&workspace)?;
    let mut registry = ToolRegistry::new();
    let paths = sigil_tools_builtin::BuiltinToolPaths::workspace_defaults(&workspace);
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
    let draft = serde_json::json!({
        "schema_version": 1, "outcome": "draft",
        "content": "# Write the approved note\n\n1. Write approval-note.txt with the approved content.",
    });
    let args_json = serde_json::json!({
        "path":"approval-note.txt", "content":"approved direct write\n"
    })
    .to_string();
    let role_provider = planned_role_provider_builder(vec![
        StreamPlan::Chunks(vec![
            ProviderChunk::ToolCallStart {
                id: "direct-write".to_owned(),
                name: "write_file".to_owned(),
            },
            ProviderChunk::ToolCallArgsDelta {
                id: "direct-write".to_owned(),
                delta: args_json.clone(),
            },
            ProviderChunk::ToolCallComplete(ToolCall {
                id: "direct-write".to_owned(),
                name: "write_file".to_owned(),
                args_json,
            }),
            ProviderChunk::Done,
        ]),
        StreamPlan::Chunks(vec![
            ProviderChunk::TextDelta("approved note written".to_owned()),
            ProviderChunk::Done,
        ]),
    ]);
    let worker = spawn_test_worker_with_role_provider_builder_and_authority(
        config.clone(),
        session_path.clone(),
        Agent::new(
            PlannedProvider::new(vec![StreamPlan::Chunks(submit_plan_review_result_chunks(
                "note-plan",
                &draft.to_string(),
            ))]),
            registry,
        ),
        workspace.clone(),
        role_provider,
        Arc::clone(&authority),
        Some(authority_root),
    )?;
    worker.recv_until(|message| matches!(message, WorkerMessage::WorkerReady))?;
    let records = JsonlSessionStore::read_event_records(&session_path)?;
    let session_id = records
        .first()
        .context("worker should initialize the session")?
        .stored_event()
        .session_id
        .as_str()
        .to_owned();
    let store = JsonlSessionStore::new(&session_path)?;
    let application = crate::application_bridge::tests::connect_real_worker(
        &config_path,
        &workspace,
        &session_path,
        &session_id,
        worker.command_sender(),
        &authority,
        sigil_runtime::RuntimeSessionProjectionOwner::from_store(&store),
    )?;
    sigil_runtime::application_run::application_run_context_view(
        &config_path,
        &workspace,
        &session_path,
        &session_id,
    )
    .context("Direct bridge fixture run-context projection")?;
    runtime
        .block_on(application.refresh())
        .context("Direct bridge initial projection")?;
    worker.send(WorkerCommand::SubmitPlanPrompt {
        prompt: "plan the note write".to_owned(),
        reasoning_effort: ReasoningEffort::Max,
    })?;
    worker.recv_until(|message| matches!(message, WorkerMessage::PlanRunFinished { .. }))?;
    let entries = JsonlSessionStore::read_entries(&session_path)?;
    let draft = PlanArtifactProjection::from_entries(&entries)
        .latest_pending_plan()
        .context("plan should be ready")?
        .clone();
    worker.send(WorkerCommand::CreateTaskFromPlan {
        plan_id: draft.plan_id.as_str().to_owned(),
        expected_plan_hash: draft.plan_hash,
        start_mode: PlanTaskStartMode::CreateAndRun,
        permission_grant: None,
    })?;
    let requested = worker.recv_until_with_timeout(Duration::from_secs(20), |message| {
        matches!(message, WorkerMessage::Event(event) if matches!(event.as_ref(), RunEvent::ToolApprovalRequested { call, .. } if call.id == "direct-write"))
            || matches!(message, WorkerMessage::RunFailed(_))
    })?;
    let WorkerMessage::Event(event) = requested else {
        bail!("Direct Task failed before approval: {requested:?}")
    };
    assert!(!workspace.join("approval-note.txt").exists());
    let records = JsonlSessionStore::read_event_records(&session_path)?;
    let outbox = sigil_kernel::PublicEventOutboxProjectionV1::from_records(&records)?;
    let approval_run = outbox.events_in_order().into_iter().find_map(|entry| {
        matches!(&entry.event.event, PublicRunEventKind::ApprovalRequested { call, .. } if call.id == "direct-write")
            .then(|| entry.run_id.clone())
    }).context("the native Direct approval must have a durable public binding")?;
    let mut app = crate::AppState::from_root_config(&config_path, &config);
    app.session_id = session_id;
    app.session_log_path = session_path.clone();
    app.handle(*event)?;
    let action = app
        .handle_key_event(KeyEvent::new(KeyCode::Char('y'), KeyModifiers::NONE))?
        .context("the approval key should produce an action")?;
    // This is the real application bridge, including refresh and exact approval binding;
    // sending a raw WorkerCommand directly would miss the native regression.
    application
        .try_execute_action(&action, None, None)?
        .context("approval must be admitted through the application bridge")?;
    let terminal = worker.recv_until_with_timeout(Duration::from_secs(20), |message| {
        matches!(
            message,
            WorkerMessage::TaskRunFinished { .. } | WorkerMessage::RunFailed(_)
        )
    })?;
    let WorkerMessage::TaskRunFinished {
        status, entries, ..
    } = terminal
    else {
        bail!("Direct Task did not finish: {terminal:?}")
    };
    assert_eq!(status, TaskRunStatus::Completed);
    let admission_index = entries
        .iter()
        .position(|entry| {
            matches!(
                entry,
                SessionLogEntry::Control(ControlEntry::TaskDirectExecutionAdmittedV1(_))
            )
        })
        .context("the approved Plan must have durable direct execution authority")?;
    let write_index = entries
        .iter()
        .position(|entry| {
            matches!(entry,
                SessionLogEntry::Control(ControlEntry::ToolExecution(execution))
                    if execution.call_id == "direct-write"
            )
        })
        .context("the approved write must have an execution audit")?;
    assert!(admission_index < write_index);
    assert_eq!(
        fs::read_to_string(workspace.join("approval-note.txt"))?,
        "approved direct write\n"
    );
    assert_eq!(entries.iter().filter(|entry| matches!(entry,
        SessionLogEntry::Control(ControlEntry::ToolExecution(execution))
            if execution.call_id == "direct-write" && execution.status == sigil_kernel::ToolExecutionStatus::Completed
    )).count(), 1);
    let outbox = sigil_kernel::PublicEventOutboxProjectionV1::from_records(
        &JsonlSessionStore::read_event_records(&session_path)?,
    )?;
    assert_eq!(
        outbox
            .events_in_order()
            .into_iter()
            .filter(|entry| entry.run_id == approval_run
                && matches!(entry.event.event, PublicRunEventKind::RunFinished { .. }))
            .count(),
        1,
        "the same public Direct run must settle exactly once"
    );
    worker.shutdown()?;
    Ok(())
}

#[test]
fn worker_boot_and_session_switch_reconcile_unfinished_public_runs_once() -> Result<()> {
    let runtime = tokio::runtime::Runtime::new()?;
    let _runtime_guard = runtime.enter();
    let temp = tempdir()?;
    let workspace = temp.path().to_path_buf();
    let config_path = workspace.join("sigil.toml");
    let mut config = routed_unauthenticated_test_root_config(&workspace, "planned-model");
    config.storage.state_root =
        sigil_kernel::StorageRoot::Path(workspace.join("state").display().to_string());
    config.storage.cache_root =
        sigil_kernel::StorageRoot::Path(workspace.join("cache").display().to_string());
    config.save(&config_path)?;
    let paths = [
        workspace.join(".sigil/sessions/first.jsonl"),
        workspace.join(".sigil/sessions/second.jsonl"),
    ];
    let run_ids = ["interrupted-first", "interrupted-second"];
    for (path, run_id) in paths.iter().zip(run_ids) {
        let (provider, route) =
            sigil_runtime::provider_connections::resolve_default_model_route(&config)?;
        let mut session = sigil_kernel::Session::new_with_route(provider, route)
            .with_store(JsonlSessionStore::new(path)?);
        session.ensure_identity_entry()?;
        // Use the shipping recorder to create the same unfinished lifecycle/outbox pair that
        // remains when a foreground process stops before publishing its terminal result.
        drop(sigil_runtime::ApplicationRunEventRecorder::start(
            &session,
            run_id,
            "interrupted foreground work",
        )?);
    }
    let (authority, _authority_root) = test_authority_composition(&workspace)?;
    let mut app = crate::AppState::from_root_config(&config_path, &config);
    app.session_log_path = paths[0].clone();
    app.session_id = JsonlSessionStore::read_event_records(&paths[0])?[0]
        .session_id()
        .to_owned();
    let early_scope = sigil_application::ApplicationScope {
        application_instance: sigil_application::ApplicationInstanceId::new("startup")?,
        authenticated_subject: sigil_application::AuthenticatedSubject::new("local")?,
        workspace: None,
        session: Some(sigil_application::SessionScopeId::new(&app.session_id)?),
    };
    let mut early_projection = crate::application_bridge::tests::snapshot(early_scope)
        .envelope
        .projection;
    early_projection.run.status = sigil_application::SafeText::new("running")?;
    app.apply_application_projection(&early_projection);
    assert!(app.runtime.is_busy);
    let mut worker = spawn_test_worker_with_role_provider_builder_and_authority(
        config.clone(),
        paths[0].clone(),
        Agent::new(PlannedProvider::new(Vec::new()), ToolRegistry::new()),
        workspace.clone(),
        planned_role_provider_builder(Vec::new()),
        Arc::clone(&authority),
        None,
    )?;
    loop {
        let message = worker.recv_with_timeout(Duration::from_secs(10))?;
        let ready = matches!(message, WorkerMessage::WorkerReady);
        app.handle_worker_message(message)?;
        if ready {
            break;
        }
    }
    assert!(
        !app.runtime.is_busy,
        "recovered terminal must release startup busy state"
    );
    app.apply_application_projection(&early_projection);
    assert!(
        !app.runtime.is_busy,
        "an older running projection cannot revive the recovered run"
    );
    for (round, index) in [0, 1, 0].into_iter().enumerate() {
        let run_id = run_ids[index];
        if round == 2 {
            // A session switch retires its worker; the launcher starts a worker against the
            // restored attachment. Reopening the first session must not repeat its terminal.
            worker.stop()?;
            worker = spawn_test_worker_with_role_provider_builder_and_authority(
                config.clone(),
                paths[index].clone(),
                Agent::new(PlannedProvider::new(Vec::new()), ToolRegistry::new()),
                workspace.clone(),
                planned_role_provider_builder(Vec::new()),
                Arc::clone(&authority),
                None,
            )?;
            worker.recv_until(|message| matches!(message, WorkerMessage::WorkerReady))?;
        } else if round == 1 {
            worker
                .send(WorkerCommand::SwitchSession {
                    session_log_path: paths[index].clone(),
                    attachment_recovery_binding: None,
                })
                .map_err(|error| {
                    let mut messages = Vec::new();
                    while let Ok(message) = worker.recv_with_timeout(Duration::from_millis(10)) {
                        messages.push(format!("{message:?}"));
                    }
                    anyhow::anyhow!("switch round {round} failed: {error:#}; {messages:?}")
                })?;
            worker
                .recv_until(|message| matches!(message, WorkerMessage::SessionSwitched { .. }))?;
        }
        assert_eq!(interrupted_run_count(&paths[index], run_id)?, 1);
        let store = JsonlSessionStore::new(&paths[index])?;
        let records = JsonlSessionStore::read_event_records(&paths[index])?;
        let session_id = records
            .first()
            .context("recorded session identity")?
            .session_id();
        let application = crate::application_bridge::tests::connect_real_worker(
            &config_path,
            &workspace,
            &paths[index],
            session_id,
            worker.command_sender(),
            &authority,
            sigil_runtime::RuntimeSessionProjectionOwner::from_store(&store),
        )?;
        let projection = runtime.block_on(application.refresh())?;
        assert_eq!(projection.run.status.as_str(), "interrupted");
        assert!(projection.run.active_binding.is_none());
    }
    worker.shutdown()?;
    Ok(())
}

fn interrupted_run_count(path: &std::path::Path, run_id: &str) -> Result<usize> {
    let records = JsonlSessionStore::read_event_records(path)?;
    let outbox = sigil_kernel::PublicEventOutboxProjectionV1::from_records(&records)?;
    Ok(outbox
        .events_in_order()
        .iter()
        .filter(|entry| {
            entry.run_id == run_id
                && matches!(entry.event.event, PublicRunEventKind::RunInterrupted { .. })
        })
        .count())
}

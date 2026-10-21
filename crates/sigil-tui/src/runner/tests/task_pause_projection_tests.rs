use std::{fs, sync::Arc, time::Duration};

use anyhow::{Context, Result, bail};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use sigil_kernel::{
    Agent, ControlEntry, JsonlSessionStore, ProviderChunk, RunEvent, SessionLogEntry,
    TaskRunStatus, ToolCall, ToolRegistry,
};

use crate::app::{AppAction, AppState};

use super::{
    super::WorkerMessage,
    common::{
        PlannedProvider, StreamPlan, TestWorker, planned_role_provider_builder,
        routed_unauthenticated_test_root_config,
        spawn_test_worker_with_role_provider_builder_and_authority, test_authority_composition,
    },
};

fn tool_call_plan(call_id: &str, name: &str, args: serde_json::Value) -> StreamPlan {
    let args_json = args.to_string();
    StreamPlan::Chunks(vec![
        ProviderChunk::ToolCallStart {
            id: call_id.to_owned(),
            name: name.to_owned(),
        },
        ProviderChunk::ToolCallArgsDelta {
            id: call_id.to_owned(),
            delta: args_json.clone(),
        },
        ProviderChunk::ToolCallComplete(ToolCall {
            id: call_id.to_owned(),
            name: name.to_owned(),
            args_json,
        }),
        ProviderChunk::Done,
    ])
}

fn write_plan(call_id: &str, content: &str) -> StreamPlan {
    tool_call_plan(
        call_id,
        "write_file",
        serde_json::json!({"path":"pause-note.txt", "content":content}),
    )
}

fn pump_to_approval(worker: &TestWorker, app: &mut AppState) -> Result<Vec<&'static str>> {
    let mut order = Vec::new();
    loop {
        let message = worker.recv_with_timeout(Duration::from_secs(20))?;
        match &message {
            WorkerMessage::TaskRunStarted { .. } => order.push("started"),
            WorkerMessage::TaskStateSynchronized { .. } => order.push("snapshot"),
            WorkerMessage::RunFailed(error) => bail!("Task failed before approval: {error}"),
            WorkerMessage::TaskRunFinished {
                status, entries, ..
            } => {
                let tasks = sigil_kernel::TaskStateProjection::from_entries(entries);
                let reason = tasks.latest_task().and_then(|task| task.reason.as_deref());
                bail!(
                    "Task ended before approval: {status:?}, reason={reason:?}, observed={order:?}, notice={:?}",
                    app.last_notice()
                )
            }
            _ => {}
        }
        let requested = matches!(&message, WorkerMessage::Event(event)
            if matches!(event.as_ref(), RunEvent::ToolApprovalRequested { .. }));
        app.handle_worker_message(message)?;
        if requested {
            order.push("approval");
            return Ok(order);
        }
    }
}

#[test]
fn task_pause_projection_real_worker_pauses_at_approval_and_continues_the_same_task() -> Result<()>
{
    run_pause_continuation(None)
}

#[test]
fn task_pause_projection_typed_chat_continuation_resynchronizes_the_same_task_scope() -> Result<()>
{
    run_pause_continuation(Some(sigil_kernel::TaskContinuationControlKind::ResumeTask))
}

#[test]
fn task_pause_projection_typed_chat_guidance_continues_the_same_task_scope() -> Result<()> {
    run_pause_continuation(Some(
        sigil_kernel::TaskContinuationControlKind::ApplyCurrentRequestAsGuidance,
    ))
}

fn run_pause_continuation(
    typed_control: Option<sigil_kernel::TaskContinuationControlKind>,
) -> Result<()> {
    let runtime = tokio::runtime::Runtime::new()?;
    let _runtime_guard = runtime.enter();
    let root = tempfile::tempdir()?;
    let workspace = root.path().to_path_buf();
    let config_path = workspace.join("sigil.toml");
    let session_path = workspace.join(".sigil/sessions/task-pause.jsonl");
    let mut config = routed_unauthenticated_test_root_config(&workspace, "planned-model");
    config.composition = sigil_kernel::RuntimeCompositionConfig::new(
        sigil_kernel::RuntimeCompositionProfile::Core,
        [sigil_kernel::OptionalCapability::TaskOrchestration],
    );
    if typed_control.is_some() {
        // The shared worker fixture intentionally defaults to Manual. Model-selected typed
        // continuation needs the real Auto route; explicit /task continue remains Manual.
        config.task.routing_policy = sigil_kernel::TaskRoutingPolicy::Auto;
    }
    config.storage.state_root =
        sigil_kernel::StorageRoot::Path(workspace.join("state").display().to_string());
    config.storage.cache_root =
        sigil_kernel::StorageRoot::Path(workspace.join("cache").display().to_string());
    config.save(&config_path)?;
    let (provider_name, route) =
        sigil_runtime::provider_connections::resolve_default_model_route(&config)?;
    let store = JsonlSessionStore::new(&session_path)?;
    let mut session =
        sigil_kernel::Session::new_with_route(provider_name, route).with_store(store.clone());
    session.ensure_identity_entry()?;
    let session_id = session.session_scope_id().to_owned();
    let (authority, authority_root) = test_authority_composition(&workspace)?;
    let paths = sigil_tools_builtin::BuiltinToolPaths::workspace_defaults(&workspace);
    let scratch = sigil_runtime::authority_scratch_control(paths.scratch_root.clone());
    let mut registry = ToolRegistry::new();
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
    let worker = spawn_test_worker_with_role_provider_builder_and_authority(
        config.clone(),
        session_path.clone(),
        Agent::new(
            PlannedProvider::new(if let Some(control) = typed_control {
                vec![tool_call_plan(
                    "typed-continuation",
                    sigil_kernel::CONTINUE_EXISTING_TASK_TOOL_NAME,
                    serde_json::json!({"action": control}),
                )]
            } else {
                Vec::new()
            }),
            registry,
        ),
        workspace.clone(),
        planned_role_provider_builder(vec![
            write_plan("pause-before-write", "must not be written\n"),
            write_plan("continue-write", "resumed same Task\n"),
            StreamPlan::Chunks(vec![
                ProviderChunk::TextDelta("note written".to_owned()),
                ProviderChunk::Done,
            ]),
        ]),
        Arc::clone(&authority),
        Some(authority_root),
    )?;
    worker.recv_until(|message| matches!(message, WorkerMessage::WorkerReady))?;
    let application = crate::application_bridge::tests::connect_real_worker(
        &config_path,
        &workspace,
        &session_path,
        &session_id,
        worker.command_sender(),
        &authority,
        sigil_runtime::RuntimeSessionProjectionOwner::from_store(&store),
    )?;
    runtime.block_on(application.refresh())?;
    let mut app = AppState::from_root_config(&config_path, &config);
    app.session_id = session_id.clone();
    app.session_log_path = session_path.clone();
    app.handle_paste_text("/task write the note after approval");
    let start = app.submit_input()?.context("explicit Task action")?;
    application
        .try_execute_action(&start, None, None)?
        .context("real Task command admission")?;
    let order =
        pump_to_approval(&worker, &mut app).context("initial Task before any pause request")?;
    assert_eq!(order, ["started", "snapshot", "approval"]);
    assert!(app.approval_review_open());
    assert!(!workspace.join("pause-note.txt").exists());
    assert!(
        app.handle_key_event(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE))?
            .is_none()
    );
    assert!(!app.approval_review_open());
    assert!(
        crate::view_model::UiViewModel::from_app(&app)
            .footer
            .hints
            .starts_with("F5 pause")
    );
    let pause = app
        .handle_key_event(KeyEvent::new(KeyCode::F(5), KeyModifiers::NONE))?
        .context("F5 must use the real delivered admission")?;
    let AppAction::PauseTask { request } = &pause else {
        bail!("F5 did not create PauseTask")
    };
    let task_id = request.task_id.clone();
    assert!(request.has_exact_identity());
    application
        .try_execute_action(&pause, None, None)?
        .context("real typed pause admission")?;
    let paused_entries = loop {
        let message = worker.recv_with_timeout(Duration::from_secs(20))?;
        if let WorkerMessage::TaskRunPaused {
            task_id: paused,
            entries,
            ..
        } = &message
        {
            assert_eq!(paused, task_id.as_str());
            let entries = entries.clone();
            app.handle_worker_message(message)?;
            break entries;
        }
        if let WorkerMessage::RunFailed(error) = &message {
            bail!("pause failed: {error}");
        }
        app.handle_worker_message(message)?;
    };
    assert!(!app.runtime.is_busy);
    assert!(!workspace.join("pause-note.txt").exists());
    let tasks = sigil_kernel::TaskStateProjection::from_entries(&paused_entries);
    assert_eq!(
        tasks
            .tasks
            .get(&task_id)
            .context("same paused Task")?
            .status,
        TaskRunStatus::Paused
    );
    let records = JsonlSessionStore::read_event_records(&session_path)?;
    let cleanup = records
        .iter()
        .map(|record| &record.stored_event().payload)
        .find(|payload| {
            payload.get("record").and_then(serde_json::Value::as_str) == Some("finalized")
        })
        .context("pause must settle the actual root cancellation")?;
    assert_eq!(cleanup["cleanup_complete"], true);
    assert_eq!(cleanup["outcome"], "cancelled");
    assert_eq!(cleanup["active_effects"], 0);
    assert_eq!(cleanup["active_tasks"], 0);
    app.handle_paste_text(if typed_control.is_some() {
        "Use the existing Task without changing its objective."
    } else {
        "/task continue"
    });
    let continuation = app.submit_input()?.context("continue the paused Task")?;
    if typed_control.is_some() {
        assert!(
            matches!(&continuation, AppAction::SubmitPrompt(_)),
            "the model, not the host, selects continuation"
        );
    } else {
        assert!(matches!(&continuation, AppAction::ContinueTask { .. }));
    }
    application
        .try_execute_action(&continuation, None, None)?
        .context("real continuation admission")?;
    assert_eq!(
        pump_to_approval(&worker, &mut app).context("continued Task after settled pause")?,
        ["started", "snapshot", "approval"]
    );
    let approve = app
        .handle_key_event(KeyEvent::new(KeyCode::Char('y'), KeyModifiers::NONE))?
        .context("approve resumed write")?;
    application
        .try_execute_action(&approve, None, None)?
        .context("real approval admission")?;
    let final_entries = loop {
        let message = worker.recv_with_timeout(Duration::from_secs(20))?;
        if let WorkerMessage::TaskRunFinished {
            task_id: finished,
            status,
            entries,
        } = &message
        {
            assert_eq!(finished, task_id.as_str());
            assert_eq!(*status, TaskRunStatus::Completed);
            let entries = entries.clone();
            app.handle_worker_message(message)?;
            break entries;
        }
        if let WorkerMessage::RunFailed(error) = &message {
            bail!("continuation failed: {error}");
        }
        app.handle_worker_message(message)?;
    };
    assert_eq!(
        fs::read_to_string(workspace.join("pause-note.txt"))?,
        "resumed same Task\n"
    );
    let tasks = sigil_kernel::TaskStateProjection::from_entries(&final_entries);
    assert_eq!(tasks.tasks.len(), 1);
    assert_eq!(
        tasks
            .tasks
            .get(&task_id)
            .context("completed original Task")?
            .status,
        TaskRunStatus::Completed
    );
    if typed_control.is_some() {
        assert!(
            final_entries.iter().any(|entry| matches!(
                entry, SessionLogEntry::Control(ControlEntry::TaskContinuationSelected(selected))
                    if selected.task_id == task_id && Some(selected.control) == typed_control
            )),
            "the real typed model continuation must be durably selected"
        );
    }
    assert_eq!(
        final_entries
            .iter()
            .filter(|entry| matches!(
                entry,
                SessionLogEntry::Control(ControlEntry::TaskDirectExecutionAdmittedV1(_))
            ))
            .count(),
        1,
        "private synchronization must never append the admission again"
    );
    assert_eq!(
        final_entries
            .iter()
            .filter(|entry| matches!(
                entry,
                SessionLogEntry::Control(ControlEntry::TaskRunCancellationScopeBound(_))
            ))
            .count(),
        2,
        "continuation binds a fresh root without replaying either scope entry"
    );
    assert!(!app.runtime.is_busy);
    worker.shutdown()?;
    Ok(())
}

#[test]
fn task_pause_projection_keeps_exact_runtime_admission_and_scope_validation() -> Result<()> {
    let task_id = sigil_kernel::TaskId::new("exact_task")?;
    let admission = sigil_kernel::TaskDirectExecutionAdmittedV1::task_request(
        task_id.clone(),
        "exact objective",
        1,
    );
    let entries = vec![
        SessionLogEntry::Control(ControlEntry::TaskRun(sigil_kernel::TaskRunEntry {
            task_id: task_id.clone(),
            parent_session_ref: sigil_kernel::SessionRef::new_relative("parent.jsonl")?,
            objective: "exact objective".to_owned(),
            title: None,
            status: TaskRunStatus::Running,
            reason: None,
        })),
        SessionLogEntry::Control(ControlEntry::TaskDirectExecutionAdmittedV1(
            admission.clone(),
        )),
        SessionLogEntry::Control(ControlEntry::TaskRunCancellationScopeBound(
            sigil_kernel::TaskRunCancellationScopeBoundEntry {
                task_id: task_id.clone(),
                run_scope_id: "owned-scope".to_owned(),
            },
        )),
    ];
    let valid = sigil_kernel::TaskPauseRequest::direct(task_id.clone(), admission.admission_id);
    let target = sigil_kernel::RunCancellationTarget::Task {
        task_id: task_id.as_str().to_owned(),
    };
    let validate = sigil_runtime::agent_supervisor::task_execution::validate_task_pause_request;
    validate(&valid, &target, "owned-scope", &entries)?;
    let stale = sigil_kernel::TaskPauseRequest::direct(task_id, "stale-admission");
    assert!(matches!(validate(&stale, &target, "owned-scope", &entries),
        Err(sigil_runtime::agent_supervisor::task_execution::TaskPauseValidationError::ExecutionAuthorityChanged)));
    let other = sigil_kernel::TaskPauseRequest::direct(
        sigil_kernel::TaskId::new("other_task")?,
        "other-admission",
    );
    assert!(matches!(validate(&other, &target, "owned-scope", &entries),
        Err(sigil_runtime::agent_supervisor::task_execution::TaskPauseValidationError::TargetMismatch)));
    assert!(matches!(validate(&valid, &sigil_kernel::RunCancellationTarget::Run, "ordinary-run-scope", &entries),
        Err(sigil_runtime::agent_supervisor::task_execution::TaskPauseValidationError::TargetMismatch)));
    Ok(())
}

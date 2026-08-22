use anyhow::Result;
use sigil_kernel::{
    Agent, AgentProfileId, AgentProfileTrustEntry, AgentTrustState, ControlEntry,
    ConversationInputQueueId, JsonlSessionStore, ProviderCapabilities, ReasoningStreamSupport,
    RunCancellationTarget, SecretString, Session, SessionLogEntry, SessionRef, TaskId,
    TaskPauseRequest, TaskPlanEntry, TaskPlanStatus, TaskRunEntry, TaskRunStatus, TaskStepId,
    TaskStepSpec, ToolRegistry,
};
use std::sync::Arc;

use super::{
    super::{
        WorkerCommand,
        worker_loop::{
            IdleAutoCompactionPreparation, IdleAutoCompactionState, IdleV2CompactionPreparation,
            SessionTransitionKind, WorkerCommandDomain, WorkerLoopState,
            changed_task_completion_progress, changed_task_provider_route_diagnostics,
            classify_worker_command, task_completion_progress_for_active_task, transition_session,
            validate_task_pause_request,
        },
    },
    super::{
        terminal_lifecycle_bridge::ChannelTerminalLifecycleRouter,
        worker_event::{WorkerEventPayloadSender, WorkerWakeCoalescer},
    },
    common::{PlannedProvider, routed_session_identity, routed_test_root_config, test_root_config},
};

fn provider_capabilities() -> ProviderCapabilities {
    ProviderCapabilities {
        exact_prefix_cache: true,
        reports_cache_tokens: true,
        reasoning_stream: ReasoningStreamSupport::Native,
        supports_reasoning_effort: true,
        supports_tool_stream: true,
        supports_background_tasks: false,
        supports_response_handles: false,
        supports_reasoning_artifacts: false,
        supports_structured_output: true,
        supports_assistant_prefix_seed: false,
        supports_schema_constrained_tools: true,
        supports_agent_background_resume: false,
        supports_agent_thread_usage: false,
        supports_agent_result_replay: false,
        supports_infill_completion: false,
        supports_system_fingerprint: true,
        tool_name_max_chars: 64,
    }
}

#[test]
fn worker_loop_state_initializes_domain_owners_from_session() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let session_log_path = temp.path().join("session.jsonl");
    let root_config = test_root_config(temp.path(), "planned", "planned-model");
    let registry = sigil_runtime::AgentProfileRegistry::from_root_config_with_workspace(
        &root_config,
        temp.path(),
    )?;
    let supervisor = sigil_runtime::AgentSupervisor::new(
        registry,
        sigil_runtime::AgentBudgetPolicy::from_root_config(&root_config),
        provider_capabilities(),
    );
    let session = Session::new("planned", "planned-model");
    let (event_tx, _event_rx) = std::sync::mpsc::channel();
    let terminal_lifecycle_router = ChannelTerminalLifecycleRouter::new(event_tx.clone());

    let mut state = WorkerLoopState::new(
        session_log_path.clone(),
        Some(session),
        sigil_runtime::interactive_session_attachment::InteractiveSessionAttachmentLease::acquire(
            &session_log_path,
        )?
        .into(),
        supervisor,
        sigil_runtime::AgentToolBackgroundRuns::default(),
        event_tx.clone(),
        WorkerWakeCoalescer::new(event_tx, None),
        terminal_lifecycle_router,
        None,
        None,
    );

    assert_eq!(state.session.log_path, session_log_path);
    let current_session = state
        .session
        .current
        .as_ref()
        .expect("constructor should retain the supplied session");
    assert_eq!(current_session.provider_name(), "planned");
    assert_eq!(current_session.model_name(), "planned-model");
    assert!(state.run.active.is_none());
    assert_eq!(state.run.next_id, 1);
    assert!(state.run.discarded_ids.is_empty());
    assert!(state.compaction.pending.is_none());
    assert_eq!(state.compaction.next_request_id, 1);
    assert!(state.refresh.pending_mcp_servers.is_empty());
    assert!(
        state
            .agent
            .last_task_provider_route_diagnostics
            .routes
            .is_empty()
    );
    assert!(state.agent.last_task_completion_progress.batch.is_none());
    assert!(state.approval_command_receipts.is_empty());
    assert!(
        state.defer_startup_artifact_gc,
        "startup artifact GC must wait for the first command so a fresh worker never races a resume"
    );
    let notice = "artifact maintenance deferred: fixture".to_owned();
    assert_eq!(
        state.artifact_gc.changed_deferred_notice(notice.clone()),
        Some(notice.clone())
    );
    assert_eq!(
        state.artifact_gc.changed_deferred_notice(notice.clone()),
        None
    );
    let other_notice = "artifact maintenance deferred: another fixture".to_owned();
    assert_eq!(
        state
            .artifact_gc
            .changed_deferred_notice(other_notice.clone()),
        Some(other_notice)
    );
    assert_eq!(
        state.artifact_gc.changed_deferred_notice(notice.clone()),
        None,
        "a different intervening failure must not re-enable an already shown notice"
    );
    state.artifact_gc.clear_deferred_notice();
    assert_eq!(
        state.artifact_gc.changed_deferred_notice(notice.clone()),
        Some(notice)
    );
    Ok(())
}

#[test]
fn task_pause_validation_rejects_stale_plan_and_wrong_active_target() -> Result<()> {
    let task_id = TaskId::new("task_1")?;
    let entries = vec![
        SessionLogEntry::Control(ControlEntry::TaskRun(TaskRunEntry {
            task_id: task_id.clone(),
            parent_session_ref: SessionRef::new_relative("parent.jsonl")?,
            objective: "pause safely".to_owned(),
            title: None,

            status: TaskRunStatus::Running,
            reason: None,
        })),
        SessionLogEntry::Control(ControlEntry::TaskPlan(TaskPlanEntry {
            task_id: task_id.clone(),
            plan_version: 2,
            status: TaskPlanStatus::Accepted,
            steps: vec![TaskStepSpec {
                step_id: TaskStepId::new("step_1")?,
                title: "Inspect".to_owned(),
                display_name: None,
                detail: None,
                role: sigil_kernel::AgentRole::SubagentRead,
                depends_on: Vec::new(),
                intent_refs: Vec::new(),
                mode: Some(sigil_kernel::TaskStepMode::Read),
                isolation: Some(sigil_kernel::TaskIsolationMode::SharedReadOnly),
            }],
            reason: None,
        })),
        SessionLogEntry::Control(ControlEntry::TaskRunCancellationScopeBound(
            sigil_kernel::TaskRunCancellationScopeBoundEntry {
                task_id: task_id.clone(),
                run_scope_id: "scope_1".to_owned(),
            },
        )),
    ];
    let target = RunCancellationTarget::Task {
        task_id: task_id.as_str().to_owned(),
    };
    let request = TaskPauseRequest::new(task_id, 2);
    validate_task_pause_request(&request, &target, "scope_1", &entries)
        .expect("exact active task plan should pause");

    let stale = TaskPauseRequest::new(request.task_id.clone(), 1);
    assert!(
        validate_task_pause_request(&stale, &target, "scope_1", &entries)
            .expect_err("stale plan must fail")
            .contains("execution authority changed")
    );
    let wrong_target = RunCancellationTarget::Task {
        task_id: "task_2".to_owned(),
    };
    assert!(
        validate_task_pause_request(&request, &wrong_target, "scope_1", &entries)
            .expect_err("wrong active task must fail")
            .contains("another task")
    );
    validate_task_pause_request(&request, &RunCancellationTarget::Run, "scope_1", &entries)
        .expect("automatic handoff task should inherit the active root cancellation scope");
    assert!(
        validate_task_pause_request(&request, &RunCancellationTarget::Run, "scope_2", &entries,)
            .expect_err("another root run scope must fail")
            .contains("another task")
    );
    Ok(())
}

#[test]
fn task_route_diagnostics_emit_only_on_change_and_clear_when_task_stops() {
    let empty = sigil_runtime::TaskProviderRouteDiagnosticsSnapshot::default();
    let active = task_route_diagnostics_fixture("route", 1);

    assert_eq!(
        changed_task_provider_route_diagnostics(true, active.clone(), &empty),
        Some(active.clone())
    );
    assert_eq!(
        changed_task_provider_route_diagnostics(true, active.clone(), &active),
        None
    );
    assert_eq!(
        changed_task_provider_route_diagnostics(false, active, &empty),
        None
    );
    assert_eq!(
        changed_task_provider_route_diagnostics(
            false,
            empty.clone(),
            &task_route_diagnostics_fixture("old", 0),
        ),
        Some(empty)
    );
}

#[test]
fn task_completion_progress_emits_only_on_change_and_clears_when_task_stops() {
    let empty = sigil_runtime::TaskCompletionProgressSnapshot::default();
    let active = task_completion_progress_fixture();

    assert_eq!(
        changed_task_completion_progress(true, active.clone(), &empty),
        Some(active.clone())
    );
    assert_eq!(
        changed_task_completion_progress(true, active.clone(), &active),
        None
    );
    assert_eq!(
        changed_task_completion_progress(false, active, &empty),
        None
    );
    assert_eq!(
        changed_task_completion_progress(false, empty.clone(), &task_completion_progress_fixture(),),
        Some(empty)
    );
}

#[test]
fn task_completion_progress_does_not_cross_task_identity() {
    let current = task_completion_progress_fixture();

    assert_eq!(
        task_completion_progress_for_active_task(Some("task_1"), current.clone()),
        current
    );
    assert_eq!(
        task_completion_progress_for_active_task(
            Some("task_2"),
            task_completion_progress_fixture(),
        ),
        sigil_runtime::TaskCompletionProgressSnapshot::default()
    );
    assert_eq!(
        task_completion_progress_for_active_task(None, task_completion_progress_fixture()),
        sigil_runtime::TaskCompletionProgressSnapshot::default()
    );
}

fn task_completion_progress_fixture() -> sigil_runtime::TaskCompletionProgressSnapshot {
    sigil_runtime::TaskCompletionProgressSnapshot {
        batch: Some(sigil_runtime::TaskCompletionProgress {
            generation: 1,
            task_id: "task_1".to_owned(),
            plan_version: 1,
            arrived: 1,
            total: 1,
            members: vec![sigil_runtime::TaskCompletionProgressMember {
                step_id: "read".to_owned(),
                title: "Read".to_owned(),
                request_order: 1,
                arrival_order: Some(1),
                outcome: Some(sigil_runtime::TaskCompletionOutcome::Succeeded),
            }],
        }),
    }
}

fn task_route_diagnostics_fixture(
    route: &str,
    in_flight: usize,
) -> sigil_runtime::TaskProviderRouteDiagnosticsSnapshot {
    sigil_runtime::TaskProviderRouteDiagnosticsSnapshot {
        routes: vec![sigil_runtime::TaskProviderRouteDiagnostics {
            route_fingerprint: format!("sha256:{route}"),
            provider_name: "deepseek".to_owned(),
            model_name: "deepseek-v4-flash".to_owned(),
            consumers: Vec::new(),
            in_flight,
            waiting: 0,
            concurrency_window: 2,
            max_concurrency: 4,
            cooldown_remaining_ms: usize::from(in_flight == 0) as u64,
            consecutive_rate_limits: u32::from(in_flight == 0),
        }],
    }
}

#[test]
fn worker_commands_are_routed_to_explicit_domains() {
    let cases = [
        (WorkerCommand::CancelRun, WorkerCommandDomain::RunPlan),
        (
            WorkerCommand::PauseTask {
                request: TaskPauseRequest::new(TaskId::new("task_1").expect("task id"), 1),
            },
            WorkerCommandDomain::RunPlan,
        ),
        (
            WorkerCommand::StartNewSession {
                session_log_path: "new-session.jsonl".into(),
            },
            WorkerCommandDomain::Session,
        ),
        (
            WorkerCommand::ReadToolArtifactPage {
                request_id: 8,
                artifact_ref: sigil_kernel::ToolArtifactRefV1 {
                    artifact_id: format!("ta1_{}", "a".repeat(32)),
                },
                selector: sigil_kernel::ToolArtifactSelectorV1::ByteSlice {
                    offset: 0,
                    limit: 1024,
                },
            },
            WorkerCommandDomain::Session,
        ),
        (
            WorkerCommand::StartV2Compaction,
            WorkerCommandDomain::QueueCompaction,
        ),
        (
            WorkerCommand::PreviewV2Compaction,
            WorkerCommandDomain::QueueCompaction,
        ),
        (
            WorkerCommand::BackgroundActiveAgent,
            WorkerCommandDomain::AgentTask,
        ),
        (
            WorkerCommand::LoadIntentStack { request_id: 9 },
            WorkerCommandDomain::IntentStack,
        ),
        (
            WorkerCommand::CheckChangedFilesDiagnostics,
            WorkerCommandDomain::VerificationCheckpoint,
        ),
        (
            WorkerCommand::ReviewTaskIntegration {
                request: sigil_kernel::TaskIntegrationReviewRequest {
                    request_id: "integration-review-request".to_owned(),
                    task_id: sigil_kernel::TaskId::new("task-1").expect("task id"),
                    plan_id: sigil_kernel::IntegrationPlanId::new("plan-1").expect("plan id"),
                    plan_version: 1,
                    preview_digest: format!("sha256:{}", "a".repeat(64)),
                },
            },
            WorkerCommandDomain::VerificationCheckpoint,
        ),
        (
            WorkerCommand::AcceptTaskIntegration {
                request: sigil_kernel::TaskIntegrationReviewRequest {
                    request_id: "integration-accept-request".to_owned(),
                    task_id: sigil_kernel::TaskId::new("task-1").expect("task id"),
                    plan_id: sigil_kernel::IntegrationPlanId::new("plan-1").expect("plan id"),
                    plan_version: 1,
                    preview_digest: format!("sha256:{}", "b".repeat(64)),
                },
            },
            WorkerCommandDomain::VerificationCheckpoint,
        ),
        (
            WorkerCommand::CancelProviderModelsRefresh { request_id: 7 },
            WorkerCommandDomain::ProviderMcp,
        ),
        (WorkerCommand::Shutdown, WorkerCommandDomain::Maintenance),
    ];

    for (command, expected) in cases {
        assert_eq!(classify_worker_command(command).domain(), expected);
    }
}

#[test]
fn detached_background_runs_block_session_transitions() {
    assert_eq!(
        SessionTransitionKind::Switch.block_reason(false, true, false, false),
        Some("cannot switch sessions while a background agent is running")
    );
    assert_eq!(
        SessionTransitionKind::StartNew.block_reason(false, true, false, false),
        Some("cannot start a new session while a background agent is running")
    );
    assert_eq!(
        SessionTransitionKind::LocalFork.block_reason(false, true, false, false),
        Some("cannot fork a local session while a background agent is running")
    );
    assert_eq!(
        SessionTransitionKind::CheckpointFork.block_reason(false, true, false, false),
        Some("cannot fork conversation while a background agent is running")
    );
}

#[test]
fn maintenance_tasks_only_block_fork_transitions() {
    assert_eq!(
        SessionTransitionKind::Switch.block_reason(false, false, true, false),
        None,
        "switching away from a session must join its in-flight maintenance, not reject"
    );
    assert_eq!(
        SessionTransitionKind::StartNew.block_reason(false, false, true, false),
        None,
        "starting a new session must join in-flight maintenance, not reject"
    );
    assert_eq!(
        SessionTransitionKind::LocalFork.block_reason(false, false, true, false),
        Some("cannot fork a local session while session maintenance is running")
    );
    assert_eq!(
        SessionTransitionKind::CheckpointFork.block_reason(false, false, true, false),
        Some("cannot fork conversation while session maintenance is running")
    );
}

#[test]
fn session_transition_rebuilds_session_scoped_worker_state() -> Result<()> {
    let runtime = tokio::runtime::Runtime::new()?;
    let temp = tempfile::tempdir()?;
    let current_path = temp.path().join("current.jsonl");
    let target_path = temp.path().join("target.jsonl");
    let current_store = JsonlSessionStore::new(&current_path)?;
    let target_store = JsonlSessionStore::new(&target_path)?;
    let root_config = routed_test_root_config(temp.path(), "planned-model");
    target_store.append(&SessionLogEntry::Control(routed_session_identity(
        &root_config,
        "planned-model",
    )?))?;
    let current_session = Session::new_with_route(
        "deepseek",
        sigil_runtime::provider_connections::resolve_default_model_route(&root_config)
            .map_err(anyhow::Error::new)?
            .1,
    )
    .with_store(current_store);
    let registry = sigil_runtime::AgentProfileRegistry::from_root_config_with_workspace(
        &root_config,
        temp.path(),
    )?;
    let supervisor = sigil_runtime::AgentSupervisor::new(
        registry,
        sigil_runtime::AgentBudgetPolicy::from_root_config(&root_config),
        provider_capabilities(),
    );
    let provider_capabilities = provider_capabilities();
    let agent = Arc::new(Agent::new(
        PlannedProvider::new(Vec::new()),
        ToolRegistry::new(),
    ));
    let (event_tx, _event_rx) = std::sync::mpsc::channel();
    let terminal_lifecycle_router = ChannelTerminalLifecycleRouter::new(event_tx.clone());
    let mut state = WorkerLoopState::new(
        current_path.clone(),
        Some(current_session),
        sigil_runtime::interactive_session_attachment::InteractiveSessionAttachmentLease::acquire(
            &current_path,
        )?
        .into(),
        supervisor,
        sigil_runtime::AgentToolBackgroundRuns::default(),
        event_tx.clone(),
        WorkerWakeCoalescer::new(event_tx, None),
        terminal_lifecycle_router,
        None,
        None,
    );
    let queue_id = ConversationInputQueueId::new("queue_1")?;
    state
        .session
        .exact_prompts
        .insert(queue_id.clone(), SecretString::new("private prompt"));
    state.session.last_queued_pre_turn_block = Some((queue_id, "blocked".to_owned()));
    state
        .session
        .detached_durable_controls
        .push(ControlEntry::Note {
            kind: "detached-before-switch".to_owned(),
            data: serde_json::Value::Null,
        });
    state
        .session
        .pending_agent_result_continuations
        .push(sigil_kernel::AgentThreadId::new("agent_pending")?);
    state
        .compaction
        .idle_auto
        .request_after_successful_chat_run();
    let (message_tx, _message_rx) = std::sync::mpsc::channel();

    let message = transition_session(
        SessionTransitionKind::Switch,
        target_path.clone(),
        &runtime,
        &root_config,
        &provider_capabilities,
        temp.path(),
        &agent,
        &mut state,
        &message_tx,
    )?;

    assert_eq!(message.session_log_path, target_path);
    assert_eq!(state.session.log_path, target_path);
    assert!(state.session.exact_prompts.is_empty());
    assert!(state.session.detached_durable_controls.is_empty());
    assert!(state.session.last_queued_pre_turn_block.is_none());
    assert!(state.session.pending_agent_result_continuations.is_empty());
    assert!(!state.compaction.idle_auto.is_requested());
    assert!(
        state.session.active_projection_subscription.is_some(),
        "session transition must retain the new active-projection subscription"
    );
    let projection_binding = state
        .wake_coalescer
        .current_projection_binding()
        .expect("session transition should install a projection binding");
    assert_eq!(
        projection_binding.session_scope_id,
        state
            .session
            .current
            .as_ref()
            .expect("transition keeps the target session")
            .session_scope_id()
    );

    let same_scope_queue_id = ConversationInputQueueId::new("queue_same_scope")?;
    state.session.exact_prompts.insert(
        same_scope_queue_id.clone(),
        SecretString::new("same scope prompt"),
    );
    transition_session(
        SessionTransitionKind::Switch,
        target_path.clone(),
        &runtime,
        &root_config,
        &provider_capabilities,
        temp.path(),
        &agent,
        &mut state,
        &message_tx,
    )?;
    assert!(
        state
            .session
            .exact_prompts
            .contains_key(&same_scope_queue_id)
    );
    let rebound_projection = state
        .wake_coalescer
        .current_projection_binding()
        .expect("same-scope transition should rebind the projection observer");
    assert_eq!(
        rebound_projection.session_scope_id,
        projection_binding.session_scope_id
    );
    assert!(
        rebound_projection.observer_id > projection_binding.observer_id,
        "every session transition must fence the previous observer generation"
    );
    assert!(state.session.active_projection_subscription.is_some());

    let retained_block = Some((same_scope_queue_id, "retain on failure".to_owned()));
    state.session.last_queued_pre_turn_block = retained_block.clone();
    let invalid_path = temp.path().join("invalid-target");
    std::fs::create_dir(&invalid_path)?;
    assert!(
        transition_session(
            SessionTransitionKind::Switch,
            invalid_path,
            &runtime,
            &root_config,
            &provider_capabilities,
            temp.path(),
            &agent,
            &mut state,
            &message_tx,
        )
        .is_err()
    );
    assert_eq!(state.session.log_path, target_path);
    assert_eq!(state.session.last_queued_pre_turn_block, retained_block);
    Ok(())
}

#[test]
fn session_transition_joins_in_flight_maintenance_instead_of_rejecting() -> Result<()> {
    let runtime = tokio::runtime::Runtime::new()?;
    let temp = tempfile::tempdir()?;
    let current_path = temp.path().join("current.jsonl");
    let target_path = temp.path().join("target.jsonl");
    let current_store = JsonlSessionStore::new(&current_path)?;
    let target_store = JsonlSessionStore::new(&target_path)?;
    let root_config = routed_test_root_config(temp.path(), "planned-model");
    target_store.append(&SessionLogEntry::Control(routed_session_identity(
        &root_config,
        "planned-model",
    )?))?;
    let current_session = Session::new_with_route(
        "deepseek",
        sigil_runtime::provider_connections::resolve_default_model_route(&root_config)
            .map_err(anyhow::Error::new)?
            .1,
    )
    .with_store(current_store);
    let registry = sigil_runtime::AgentProfileRegistry::from_root_config_with_workspace(
        &root_config,
        temp.path(),
    )?;
    let supervisor = sigil_runtime::AgentSupervisor::new(
        registry,
        sigil_runtime::AgentBudgetPolicy::from_root_config(&root_config),
        provider_capabilities(),
    );
    let provider_capabilities = provider_capabilities();
    let agent = Arc::new(Agent::new(
        PlannedProvider::new(Vec::new()),
        ToolRegistry::new(),
    ));
    let (event_tx, _event_rx) = std::sync::mpsc::channel();
    let terminal_lifecycle_router = ChannelTerminalLifecycleRouter::new(event_tx.clone());
    let mut state = WorkerLoopState::new(
        current_path.clone(),
        Some(current_session),
        sigil_runtime::interactive_session_attachment::InteractiveSessionAttachmentLease::acquire(
            &current_path,
        )?
        .into(),
        supervisor,
        sigil_runtime::AgentToolBackgroundRuns::default(),
        event_tx.clone(),
        WorkerWakeCoalescer::new(event_tx.clone(), None),
        terminal_lifecycle_router,
        None,
        None,
    );
    let session_scope_id = state
        .session
        .current
        .as_ref()
        .expect("current session should be present")
        .session_scope_id()
        .to_owned();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let releaser = std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(200));
        let _ = release_tx.send(());
    });
    state
        .compaction
        .preparation_tasks
        .start_idle(
            &runtime,
            state.compaction.next_request_id,
            session_scope_id,
            Arc::clone(&state.session.attachment_lease),
            WorkerEventPayloadSender::compaction(event_tx),
            move || {
                let _ = release_rx.recv();
                Ok(IdleV2CompactionPreparation {
                    state: IdleAutoCompactionState::default(),
                    preparation: Ok(IdleAutoCompactionPreparation::NotRequested),
                    session: Session::new("idle", "model"),
                })
            },
        )
        .map_err(anyhow::Error::msg)?;
    assert!(
        state.compaction.preparation_tasks.has_active(),
        "maintenance task should be in flight before the transition"
    );
    let (message_tx, _message_rx) = std::sync::mpsc::channel();
    let message = transition_session(
        SessionTransitionKind::Switch,
        target_path.clone(),
        &runtime,
        &root_config,
        &provider_capabilities,
        temp.path(),
        &agent,
        &mut state,
        &message_tx,
    )?;
    assert_eq!(message.session_log_path, target_path);
    assert_eq!(state.session.log_path, target_path);
    assert!(
        !state.compaction.preparation_tasks.has_active(),
        "the transition must join in-flight maintenance"
    );
    let _ = releaser.join();
    Ok(())
}

fn assert_fork_transition_resets_session_state(kind: SessionTransitionKind) -> Result<()> {
    let runtime = tokio::runtime::Runtime::new()?;
    let temp = tempfile::tempdir()?;
    let current_path = temp.path().join("current.jsonl");
    let target_path = temp.path().join("fork.jsonl");
    let current_store = JsonlSessionStore::new(&current_path)?;
    let target_store = JsonlSessionStore::new(&target_path)?;
    let root_config = routed_test_root_config(temp.path(), "planned-model");
    target_store.append(&SessionLogEntry::Control(routed_session_identity(
        &root_config,
        "planned-model",
    )?))?;
    let current_session = Session::new_with_route(
        "deepseek",
        sigil_runtime::provider_connections::resolve_default_model_route(&root_config)
            .map_err(anyhow::Error::new)?
            .1,
    )
    .with_store(current_store);
    let capabilities = provider_capabilities();
    let registry = sigil_runtime::AgentProfileRegistry::from_root_config_with_workspace(
        &root_config,
        temp.path(),
    )?;
    let supervisor = sigil_runtime::AgentSupervisor::new(
        registry,
        sigil_runtime::AgentBudgetPolicy::from_root_config(&root_config),
        capabilities.clone(),
    );
    let agent = Arc::new(Agent::new(
        PlannedProvider::new(Vec::new()),
        ToolRegistry::new(),
    ));
    let (event_tx, _event_rx) = std::sync::mpsc::channel();
    let terminal_lifecycle_router = ChannelTerminalLifecycleRouter::new(event_tx.clone());
    let mut state = WorkerLoopState::new(
        current_path.clone(),
        Some(current_session),
        sigil_runtime::interactive_session_attachment::InteractiveSessionAttachmentLease::acquire(
            &current_path,
        )?
        .into(),
        supervisor,
        sigil_runtime::AgentToolBackgroundRuns::default(),
        event_tx.clone(),
        WorkerWakeCoalescer::new(event_tx, None),
        terminal_lifecycle_router,
        None,
        None,
    );
    let queue_id = ConversationInputQueueId::new("fork_queue")?;
    state
        .session
        .exact_prompts
        .insert(queue_id.clone(), SecretString::new("fork-local prompt"));
    state.session.last_queued_pre_turn_block = Some((queue_id, "blocked".to_owned()));
    state
        .session
        .detached_durable_controls
        .push(ControlEntry::Note {
            kind: "detached-before-fork".to_owned(),
            data: serde_json::Value::Null,
        });
    state
        .session
        .pending_agent_result_continuations
        .push(sigil_kernel::AgentThreadId::new("fork_pending")?);
    state
        .compaction
        .idle_auto
        .request_after_successful_chat_run();
    let (message_tx, _message_rx) = std::sync::mpsc::channel();

    transition_session(
        kind,
        target_path.clone(),
        &runtime,
        &root_config,
        &capabilities,
        temp.path(),
        &agent,
        &mut state,
        &message_tx,
    )?;

    assert_eq!(state.session.log_path, target_path);
    assert!(state.session.exact_prompts.is_empty());
    assert!(state.session.detached_durable_controls.is_empty());
    assert!(state.session.last_queued_pre_turn_block.is_none());
    assert!(state.session.pending_agent_result_continuations.is_empty());
    assert!(!state.compaction.idle_auto.is_requested());
    Ok(())
}

#[test]
fn local_fork_transition_resets_session_scoped_state() -> Result<()> {
    assert_fork_transition_resets_session_state(SessionTransitionKind::LocalFork)
}

#[test]
fn checkpoint_fork_transition_resets_session_scoped_state() -> Result<()> {
    assert_fork_transition_resets_session_state(SessionTransitionKind::CheckpointFork)
}

#[test]
fn session_transition_rebinds_agent_trust_and_tool_surface() -> Result<()> {
    let runtime = tokio::runtime::Runtime::new()?;
    let temp = tempfile::tempdir()?;
    let agent_dir = temp.path().join(".sigil/agents/scope-canary");
    std::fs::create_dir_all(&agent_dir)?;
    std::fs::write(
        agent_dir.join("agent.toml"),
        r#"
description = "Session scope canary."
instructions = "Inspect the workspace."
invocation_policy = "model_allowed"
allowed_tools = ["grep"]
"#,
    )?;

    let root_config = routed_test_root_config(temp.path(), "planned-model");
    let capabilities = provider_capabilities();
    let profile_id = AgentProfileId::new("scope-canary")?;
    let base_registry = sigil_runtime::AgentProfileRegistry::from_root_config_with_workspace(
        &root_config,
        temp.path(),
    )?;
    let snapshot = base_registry.capture_snapshot(&profile_id)?;
    let current_path = temp.path().join("current.jsonl");
    let trusted_path = temp.path().join("trusted.jsonl");
    let untrusted_path = temp.path().join("untrusted.jsonl");
    let current_store = JsonlSessionStore::new(&current_path)?;
    for path in [&trusted_path, &untrusted_path] {
        JsonlSessionStore::new(path)?.append(&SessionLogEntry::Control(
            routed_session_identity(&root_config, "planned-model")?,
        ))?;
    }
    JsonlSessionStore::new(&trusted_path)?.append(&SessionLogEntry::Control(
        ControlEntry::AgentProfileTrustDecision(AgentProfileTrustEntry {
            profile_id: profile_id.clone(),
            source: snapshot.source,
            source_hash: snapshot.source_hash,
            profile_hash: snapshot.profile_hash,
            decision: AgentTrustState::Trusted,
            reviewed_at_ms: 42,
        }),
    ))?;
    let current_session = Session::new_with_route(
        "deepseek",
        sigil_runtime::provider_connections::resolve_default_model_route(&root_config)
            .map_err(anyhow::Error::new)?
            .1,
    )
    .with_store(current_store);
    let supervisor = sigil_runtime::AgentSupervisor::new(
        base_registry,
        sigil_runtime::AgentBudgetPolicy::from_root_config(&root_config),
        capabilities.clone(),
    );
    let mut tool_registry = ToolRegistry::new();
    sigil_runtime::register_agent_tools_with_workspace(
        &mut tool_registry,
        &root_config,
        temp.path(),
    )?;
    let agent = Arc::new(Agent::new(PlannedProvider::new(Vec::new()), tool_registry));
    let (event_tx, _event_rx) = std::sync::mpsc::channel();
    let terminal_lifecycle_router = ChannelTerminalLifecycleRouter::new(event_tx.clone());
    let mut state = WorkerLoopState::new(
        current_path.clone(),
        Some(current_session),
        sigil_runtime::interactive_session_attachment::InteractiveSessionAttachmentLease::acquire(
            &current_path,
        )?
        .into(),
        supervisor,
        sigil_runtime::AgentToolBackgroundRuns::default(),
        event_tx.clone(),
        WorkerWakeCoalescer::new(event_tx, None),
        terminal_lifecycle_router,
        None,
        None,
    );
    let (message_tx, _message_rx) = std::sync::mpsc::channel();

    transition_session(
        SessionTransitionKind::Switch,
        trusted_path,
        &runtime,
        &root_config,
        &capabilities,
        temp.path(),
        &agent,
        &mut state,
        &message_tx,
    )?;
    assert_eq!(
        state
            .agent
            .supervisor
            .registry()
            .get(&profile_id)
            .expect("workspace profile should remain registered")
            .trust_state,
        AgentTrustState::Trusted
    );
    assert!(
        agent
            .tool_registry()
            .spec_for(sigil_runtime::SPAWN_AGENT_TOOL_NAME)
            .expect("spawn agent tool should be registered")
            .description
            .contains(profile_id.as_str())
    );

    transition_session(
        SessionTransitionKind::Switch,
        untrusted_path,
        &runtime,
        &root_config,
        &capabilities,
        temp.path(),
        &agent,
        &mut state,
        &message_tx,
    )?;
    assert_eq!(
        state
            .agent
            .supervisor
            .registry()
            .get(&profile_id)
            .expect("workspace profile should remain registered")
            .trust_state,
        AgentTrustState::NeedsReview
    );
    assert!(
        !agent
            .tool_registry()
            .spec_for(sigil_runtime::SPAWN_AGENT_TOOL_NAME)
            .expect("spawn agent tool should remain registered")
            .description
            .contains(profile_id.as_str())
    );
    Ok(())
}

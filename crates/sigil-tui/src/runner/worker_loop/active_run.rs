use super::*;

pub(in crate::runner) struct ActiveRun {
    pub(in crate::runner) run_id: u64,
    /// Exact logical run owned by the shared public-event recorder, when this path uses it.
    pub(in crate::runner) public_run_id: Option<String>,
    pub(in crate::runner) handle: tokio::task::JoinHandle<()>,
    pub(in crate::runner) approval_tx: mpsc::Sender<ApprovalSignal>,
    pub(in crate::runner) elicitation_audit_buffer: McpElicitationAuditBuffer,
    pub(in crate::runner) cancellation_owner: RunCancellationOwner,
    pub(in crate::runner) cancellation_recorder: RunCancellationRecorder,
    pub(in crate::runner) cancellation_target: RunCancellationTarget,
    /// Only revision workers bind a child logical run. It lets cancellation defer to an exact
    /// already-committed revision terminal instead of overwriting it with a generic foreground
    /// cancellation result.
    pub(in crate::runner) revision_terminal_run_id: Option<String>,
    pub(in crate::runner) url_capability_registrar: Option<Arc<dyn UserUrlCapabilityRegistrar>>,
    pub(in crate::runner) image_attachment_resolver: Option<Arc<dyn ImageAttachmentResolver>>,
}

// Keep the foreground cancellation deadline below the worker's public terminal wait window.
// Once this bounded interval elapses we persist `Interrupted` with cleanup uncertainty and
// retain the join handle for the worker's shared shutdown drain.
const RUN_QUIESCENCE_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::runner) enum ActiveRunStopDisposition {
    Cancel,
    PauseTask,
}

pub(in crate::runner) fn finalize_completed_run_cancellation(owner: &RunCancellationOwner) -> bool {
    // A returned failure or suspended routing microturn may leave the root phase open. Close it
    // before releasing its owner, without erasing outstanding work or cleanup-failure evidence.
    owner.handle().try_finalize_naturally();
    // A stop that already won still needs the live owner to perform its durable settlement.
    !owner.is_cancel_reserved()
}

pub(in crate::runner) fn prepare_run_cancellation(
    session: &Session,
) -> std::result::Result<
    (
        RunCancellationOwner,
        RunCancellationRecorder,
        RunCancellationHandle,
        RunTaskGuard,
    ),
    String,
> {
    let recorder = session
        .run_cancellation_recorder()
        .map_err(|error| format!("failed to create cancellation recorder: {error}"))?;
    let owner = RunCancellationOwner::new();
    let handle = owner.handle();
    let task_guard = handle
        .register_task()
        .map_err(|error| format!("failed to register root run task: {error}"))?;
    Ok((owner, recorder, handle, task_guard))
}

pub(in crate::runner) fn prepare_task_run_cancellation(
    session: &mut Session,
    task_id: &TaskId,
) -> std::result::Result<
    (
        RunCancellationOwner,
        RunCancellationRecorder,
        RunCancellationHandle,
        RunTaskGuard,
    ),
    String,
> {
    let prepared = sigil_runtime::agent_supervisor::task_execution::prepare_task_run_cancellation(
        session, task_id,
    )
    .map_err(|error| format!("failed to prepare task cancellation: {error:#}"))?;
    Ok((
        prepared.owner,
        prepared.recorder,
        prepared.handle,
        prepared.task_guard,
    ))
}

pub(in crate::runner) fn bind_task_run_cancellation_scope(
    session: &mut Session,
    task_id: &TaskId,
    handle: &RunCancellationHandle,
) -> std::result::Result<(), String> {
    sigil_runtime::agent_supervisor::task_execution::bind_task_run_cancellation_scope(
        session, task_id, handle,
    )
    .map_err(|error| format!("failed to bind task cancellation scope: {error:#}"))
}

#[allow(clippy::too_many_arguments)]
pub(in crate::runner) fn cancel_active_run(
    active_run: ActiveRun,
    runtime: &tokio::runtime::Runtime,
    root_config: &RootConfig,
    current_session_log_path: &Path,
    current_session: &mut Option<Session>,
    detached_durable_controls: &mut Vec<ControlEntry>,
    message_tx: &mpsc::Sender<WorkerMessage>,
    elicitation_handler: &Arc<ChannelMcpElicitationHandler>,
    agent_supervisor: Option<&sigil_runtime::AgentSupervisor>,
    discarded_run_ids: &mut BTreeSet<u64>,
    retired_runs: &mut Vec<tokio::task::JoinHandle<()>>,
    stop_control: &super::super::protocol::WorkerStopControl,
    disposition: ActiveRunStopDisposition,
    reason: &str,
) {
    let url_capability_registrar = active_run.url_capability_registrar.clone();
    let image_attachment_resolver = active_run.image_attachment_resolver.clone();
    let tool_artifact_store = current_session
        .as_ref()
        .and_then(Session::tool_artifact_store);
    elicitation_handler.set_audit_buffer(None);
    if !active_run.cancellation_owner.reserve_cancel()
        && !active_run.cancellation_owner.is_cancel_reserved()
    {
        let _ = message_tx.send(WorkerMessage::Notice(
            "run already reached its natural terminal state before cancellation".to_owned(),
        ));
        retired_runs.push(active_run.handle);
        return;
    }
    let started = Instant::now();
    let deadline = stop_control
        .shutdown_deadline()
        .map_or(started + RUN_QUIESCENCE_TIMEOUT, |deadline| {
            deadline.min(started + RUN_QUIESCENCE_TIMEOUT)
        });
    let requested_at_ms = current_unix_time_ms();
    let request_id = format!(
        "{}-{}",
        match disposition {
            ActiveRunStopDisposition::Cancel => "cancel",
            ActiveRunStopDisposition::PauseTask => "pause",
        },
        active_run.cancellation_owner.handle().scope_id()
    );
    let request = RunCancellationRequestedEntry {
        request_id: request_id.clone(),
        run_scope_id: active_run.cancellation_owner.handle().scope_id().to_owned(),
        target: active_run.cancellation_target.clone(),
        reason: reason.to_owned(),
        requested_at_ms,
        quiescence_deadline_ms: requested_at_ms
            .saturating_add(deadline.saturating_duration_since(started).as_millis() as u64),
    };
    stop_control.stage(WorkerShutdownStage::CancellationRequest);
    let request_persisted = match active_run.cancellation_recorder.append_requested(&request) {
        Ok(_) => true,
        Err(error) => {
            let _ = message_tx.send(WorkerMessage::Notice(format!(
                "cancellation audit request failed; cleanup will continue: {error:#}"
            )));
            false
        }
    };
    let _ = match (&disposition, &active_run.cancellation_target) {
        (ActiveRunStopDisposition::PauseTask, RunCancellationTarget::Task { task_id }) => {
            message_tx.send(WorkerMessage::TaskPauseRequested {
                task_id: task_id.clone(),
            })
        }
        _ => message_tx.send(WorkerMessage::RunCancellationRequested),
    };
    let activated = active_run.cancellation_owner.activate_reserved_cancel();
    debug_assert!(
        activated,
        "reserved cancellation must activate exactly once"
    );
    discarded_run_ids.insert(active_run.run_id);
    let _ = active_run.approval_tx.send(ApprovalSignal::Cancel);
    let agent_cancel_impact =
        agent_supervisor.map(sigil_runtime::AgentSupervisor::cancel_foreground_run);
    let mut handle = active_run.handle;
    stop_control.stage(WorkerShutdownStage::RunQuiescence);
    let joined = runtime.block_on(async {
        tokio::time::timeout_at(tokio::time::Instant::from_std(deadline), &mut handle).await
    });
    let join_confirmed = matches!(joined, Ok(Ok(())));
    let quiescence = if join_confirmed {
        runtime.block_on(
            active_run
                .cancellation_owner
                .wait_for_quiescence(Duration::ZERO),
        )
    } else {
        if joined.is_err() {
            handle.abort();
            // Aborting is only a request. Keep the actual task owned until its join completes.
            retired_runs.push(handle);
        }
        RunQuiescenceOutcome::TimedOut {
            active_effects: active_run.cancellation_owner.handle().active_effects(),
            active_tasks: active_run.cancellation_owner.handle().active_tasks(),
        }
    };
    let (outcome, cleanup_complete, active_effects, active_tasks, terminal_reason) =
        match quiescence {
            RunQuiescenceOutcome::Quiescent
                if join_confirmed && active_run.cancellation_owner.cleanup_complete() =>
            {
                (
                    RunCancellationTerminalOutcome::Cancelled,
                    true,
                    0,
                    0,
                    "cancellation quiescence confirmed".to_owned(),
                )
            }
            RunQuiescenceOutcome::Quiescent | RunQuiescenceOutcome::TimedOut { .. } => {
                let (active_effects, active_tasks) = match quiescence {
                    RunQuiescenceOutcome::Quiescent => (0, 0),
                    RunQuiescenceOutcome::TimedOut {
                        active_effects,
                        active_tasks,
                    } => (active_effects, active_tasks),
                };
                (
                    RunCancellationTerminalOutcome::Interrupted,
                    false,
                    active_effects,
                    active_tasks,
                    "cancellation deadline exceeded; cleanup could not be confirmed".to_owned(),
                )
            }
        };
    if !cleanup_complete || !request_persisted {
        active_run
            .cancellation_owner
            .handle()
            .mark_cleanup_incomplete();
    }
    if !request_persisted {
        stop_control.stage(WorkerShutdownStage::SessionReload);
        if let Ok(session) = load_active_run_session(
            &root_config.agent.runtime_provider,
            &root_config.agent.model,
            current_session_log_path,
            url_capability_registrar.clone(),
            image_attachment_resolver.clone(),
            tool_artifact_store.clone(),
        ) {
            *current_session = Some(session);
            detached_durable_controls.clear();
        }
        let _ = message_tx.send(WorkerMessage::RunFailed(
            "run was interrupted, but its cancellation request could not be persisted".to_owned(),
        ));
        return;
    }
    stop_control.stage(WorkerShutdownStage::CancellationFinalization);
    if let Err(error) =
        active_run
            .cancellation_recorder
            .append_finalized(&RunCancellationFinalizedEntry {
                request_id,
                run_scope_id: request.run_scope_id,
                outcome,
                cleanup_complete,
                active_effects,
                active_tasks,
                reason: terminal_reason.clone(),
                finalized_at_ms: current_unix_time_ms(),
            })
    {
        active_run
            .cancellation_owner
            .handle()
            .mark_cleanup_incomplete();
        let _ = message_tx.send(WorkerMessage::RunFailed(format!(
            "failed to persist cancellation terminal outcome: {error:#}"
        )));
        return;
    }
    stop_control.stage(WorkerShutdownStage::SessionReload);
    match load_active_run_session(
        &root_config.agent.runtime_provider,
        &root_config.agent.model,
        current_session_log_path,
        url_capability_registrar,
        image_attachment_resolver,
        tool_artifact_store,
    ) {
        Ok(session) => {
            let mut session = session;
            // A successful reload already contains every control persisted while the run owned
            // the detached session. Clear the delta before later cancellation-audit work so an
            // error in that work cannot carry already-projected controls into the next run.
            detached_durable_controls.clear();
            let durable_revision_terminal =
                if let Some(revision_run_id) = active_run.revision_terminal_run_id.as_deref() {
                    match durable_revision_terminal_event(&mut session, revision_run_id) {
                        Ok(Some(event)) => Some(event),
                        Ok(None) => None,
                        Err(error) => {
                            let _ = message_tx.send(WorkerMessage::Notice(format!(
                            "could not reconcile the revision terminal before cancellation: {error}"
                        )));
                            None
                        }
                    }
                } else {
                    None
                };
            let audit_result =
                append_mcp_elicitation_audits(&mut session, &active_run.elicitation_audit_buffer);
            if let Some(event) = durable_revision_terminal.as_ref() {
                deliver_durable_revision_terminal_after_audit(
                    event,
                    audit_result,
                    current_session_log_path,
                    &session,
                    message_tx,
                );
                *current_session = Some(session);
                return;
            }
            if let Err(error) = audit_result {
                let _ = message_tx.send(WorkerMessage::RunFailed(error));
                *current_session = Some(session);
                return;
            }
            let mut cancel_handler = ChannelEventHandler::new(message_tx.clone());
            if let Some(agent_cancel_impact) = agent_cancel_impact
                && let Err(error) = sigil_runtime::AgentSupervisor::append_foreground_cancel_audit(
                    &mut session,
                    &mut cancel_handler,
                    agent_cancel_impact,
                    reason,
                )
            {
                let _ = message_tx.send(WorkerMessage::RunFailed(format!(
                    "failed to append cancelled agent state: {error:#}"
                )));
                *current_session = Some(session);
                return;
            }
            let task_state = match (outcome, disposition, &active_run.cancellation_target) {
                (
                    RunCancellationTerminalOutcome::Cancelled,
                    ActiveRunStopDisposition::PauseTask,
                    RunCancellationTarget::Task { task_id },
                ) => append_paused_task_state(&mut session, &mut cancel_handler, task_id),
                (
                    RunCancellationTerminalOutcome::Cancelled,
                    ActiveRunStopDisposition::PauseTask,
                    RunCancellationTarget::Run | RunCancellationTarget::AgentThread { .. },
                ) => Err("pause disposition is missing its exact task target".to_owned()),
                (
                    RunCancellationTerminalOutcome::Cancelled,
                    ActiveRunStopDisposition::Cancel,
                    RunCancellationTarget::Task { task_id },
                ) => append_interrupted_task_state(
                    &mut session,
                    &mut cancel_handler,
                    Some(task_id),
                    "task run stopped from TUI; task remains available to continue",
                ),
                (
                    RunCancellationTerminalOutcome::Cancelled,
                    ActiveRunStopDisposition::Cancel,
                    RunCancellationTarget::Run,
                ) => {
                    // A submitted conversation can create a Task after ActiveRun is installed.
                    // Only its durable scope binding may select that Task after root quiescence.
                    sigil_runtime::agent_supervisor::task_execution::append_run_scoped_task_interruption(
                        &mut session,
                        &mut cancel_handler,
                        active_run.cancellation_owner.handle().scope_id(),
                        "task run stopped from TUI; task remains available to continue",
                    )
                    .map(|_| ())
                    .map_err(|error| error.to_string())
                }
                (
                    RunCancellationTerminalOutcome::Cancelled,
                    ActiveRunStopDisposition::Cancel,
                    RunCancellationTarget::AgentThread { .. },
                ) => Ok(()),
                (
                    RunCancellationTerminalOutcome::Interrupted,
                    _,
                    RunCancellationTarget::Task { task_id },
                ) => append_interrupted_task_state(
                    &mut session,
                    &mut cancel_handler,
                    Some(task_id),
                    &terminal_reason,
                ),
                (
                    RunCancellationTerminalOutcome::Interrupted,
                    _,
                    RunCancellationTarget::Run | RunCancellationTarget::AgentThread { .. },
                ) => Ok(()),
            };
            if let Err(error) = task_state {
                let _ = message_tx.send(WorkerMessage::RunFailed(error));
                *current_session = Some(session);
                return;
            }
            if let Some(public_run_id) = active_run.public_run_id.as_deref()
                && let Err(error) =
                    sigil_runtime::ApplicationRunEventRecorder::resume(&session, public_run_id)
                        .and_then(|recorder| {
                            recorder.finish_cancelled(
                                outcome == RunCancellationTerminalOutcome::Interrupted,
                                &terminal_reason,
                            )
                        })
            {
                let _ = message_tx.send(WorkerMessage::RunFailed(format!(
                    "failed to persist the cancelled public run terminal: {error:#}"
                )));
                *current_session = Some(session);
                return;
            }
            let session_id = session.session_scope_id().to_owned();
            let entries = session.entries().to_vec();
            *current_session = Some(session);
            let message = match (outcome, disposition, &active_run.cancellation_target) {
                (
                    RunCancellationTerminalOutcome::Cancelled,
                    ActiveRunStopDisposition::PauseTask,
                    RunCancellationTarget::Task { task_id },
                ) => WorkerMessage::TaskRunPaused {
                    session_id,
                    task_id: task_id.clone(),
                    session_log_path: current_session_log_path.to_path_buf(),
                    provider_name: current_session
                        .as_ref()
                        .map(|session| session.provider_name().to_owned())
                        .unwrap_or_else(|| root_config.agent.runtime_provider.clone()),
                    model_name: current_session
                        .as_ref()
                        .map(|session| session.model_name().to_owned())
                        .unwrap_or_else(|| root_config.agent.model.clone()),
                    entries,
                },
                (
                    RunCancellationTerminalOutcome::Cancelled,
                    ActiveRunStopDisposition::PauseTask,
                    RunCancellationTarget::Run | RunCancellationTarget::AgentThread { .. },
                ) => unreachable!("pause disposition requires a task cancellation target"),
                (
                    RunCancellationTerminalOutcome::Cancelled,
                    ActiveRunStopDisposition::Cancel,
                    RunCancellationTarget::Task { .. },
                ) => WorkerMessage::RunInterrupted {
                    session_id,
                    session_log_path: current_session_log_path.to_path_buf(),
                    provider_name: current_session
                        .as_ref()
                        .map(|session| session.provider_name().to_owned())
                        .unwrap_or_else(|| root_config.agent.runtime_provider.clone()),
                    model_name: current_session
                        .as_ref()
                        .map(|session| session.model_name().to_owned())
                        .unwrap_or_else(|| root_config.agent.model.clone()),
                    reason: "task run stopped; task remains available to continue".to_owned(),
                    entries,
                },
                (
                    RunCancellationTerminalOutcome::Cancelled,
                    ActiveRunStopDisposition::Cancel,
                    RunCancellationTarget::Run | RunCancellationTarget::AgentThread { .. },
                ) => WorkerMessage::RunCancelled {
                    session_id,
                    session_log_path: current_session_log_path.to_path_buf(),
                    provider_name: current_session
                        .as_ref()
                        .map(|session| session.provider_name().to_owned())
                        .unwrap_or_else(|| root_config.agent.runtime_provider.clone()),
                    model_name: current_session
                        .as_ref()
                        .map(|session| session.model_name().to_owned())
                        .unwrap_or_else(|| root_config.agent.model.clone()),
                    entries,
                },
                (RunCancellationTerminalOutcome::Interrupted, _, _) => {
                    WorkerMessage::RunInterrupted {
                        session_id,
                        session_log_path: current_session_log_path.to_path_buf(),
                        provider_name: current_session
                            .as_ref()
                            .map(|session| session.provider_name().to_owned())
                            .unwrap_or_else(|| root_config.agent.runtime_provider.clone()),
                        model_name: current_session
                            .as_ref()
                            .map(|session| session.model_name().to_owned())
                            .unwrap_or_else(|| root_config.agent.model.clone()),
                        reason: terminal_reason,
                        entries,
                    }
                }
            };
            let _ = message_tx.send(message);
        }
        Err(error) => {
            let _ = message_tx.send(WorkerMessage::RunFailed(format!("{error:#}")));
        }
    }
}

fn durable_revision_terminal_event(
    session: &mut Session,
    revision_run_id: &str,
) -> std::result::Result<Option<sigil_kernel::PublicRunEventKind>, String> {
    let Some(outbox) = session
        .reconcile_plan_review_revision_terminal(revision_run_id)
        .map_err(|error| format!("failed to read the durable revision terminal: {error:#}"))?
    else {
        return Ok(None);
    };
    Ok(Some(outbox.event.event))
}

pub(in crate::runner) fn revision_terminal_worker_message(
    event: &sigil_kernel::PublicRunEventKind,
    session_id: String,
    session_log_path: &Path,
    provider_name: String,
    model_name: String,
    entries: Vec<SessionLogEntry>,
) -> std::result::Result<WorkerMessage, String> {
    Ok(match event {
        sigil_kernel::PublicRunEventKind::RunFinished { final_text } => {
            WorkerMessage::PlanRunFinished {
                result: AgentRunResult {
                    final_text: final_text.clone(),
                    tool_calls: 0,
                    final_message_id: None,
                },
                entries,
            }
        }
        sigil_kernel::PublicRunEventKind::RunCancelled => WorkerMessage::RunCancelled {
            session_id,
            session_log_path: session_log_path.to_path_buf(),
            provider_name,
            model_name,
            entries,
        },
        sigil_kernel::PublicRunEventKind::RunInterrupted { reason } => {
            WorkerMessage::RunInterrupted {
                session_id,
                session_log_path: session_log_path.to_path_buf(),
                provider_name,
                model_name,
                reason: reason.clone(),
                entries,
            }
        }
        sigil_kernel::PublicRunEventKind::RunFailed { error } => {
            WorkerMessage::RunFailed(error.clone())
        }
        sigil_kernel::PublicRunEventKind::RunBlocked { reason } => {
            WorkerMessage::PlanReviewBlocked {
                reason: reason.clone(),
                paused: false,
                entries,
            }
        }
        sigil_kernel::PublicRunEventKind::RunPaused { reason } => {
            WorkerMessage::PlanReviewBlocked {
                reason: reason.clone(),
                paused: true,
                entries,
            }
        }
        _ => {
            return Err("durable revision terminal has an unexpected public event kind".to_owned());
        }
    })
}

/// Completes TUI cancellation delivery from the already-reconciled revision terminal. An audit
/// append failure remains visible as a Notice, but cannot rewrite that exact durable fact.
pub(in crate::runner) fn deliver_durable_revision_terminal_after_audit(
    event: &sigil_kernel::PublicRunEventKind,
    audit_result: std::result::Result<(), String>,
    session_log_path: &Path,
    session: &Session,
    message_tx: &mpsc::Sender<WorkerMessage>,
) {
    if let Err(error) = audit_result {
        let _ = message_tx.send(WorkerMessage::Notice(format!(
            "revision audit delivery failed; durable execution state is unchanged: {error}"
        )));
    }
    match revision_terminal_worker_message(
        event,
        session.session_scope_id().to_owned(),
        session_log_path,
        session.provider_name().to_owned(),
        session.model_name().to_owned(),
        session.entries().to_vec(),
    ) {
        Ok(message) => {
            let _ = message_tx.send(message);
        }
        Err(projection_error) => {
            let _ = message_tx.send(WorkerMessage::Notice(format!(
                "could not project the durable revision terminal; durable execution state is unchanged: {projection_error}"
            )));
        }
    }
}

fn load_active_run_session(
    provider_name: &str,
    model_name: &str,
    session_log_path: &Path,
    registrar: Option<Arc<dyn UserUrlCapabilityRegistrar>>,
    image_attachment_resolver: Option<Arc<dyn ImageAttachmentResolver>>,
    tool_artifact_store: Option<sigil_kernel::ToolArtifactStore>,
) -> std::result::Result<Session, String> {
    let mut session = load_session(provider_name, model_name, session_log_path)
        .map_err(|error| format!("failed to reload active-run session: {error:#}"))?;
    let registrar = registrar.ok_or_else(|| {
        "active run lost its session URL capability registrar attachment".to_owned()
    })?;
    session
        .try_attach_user_url_capability_registrar(registrar)
        .map_err(|error| format!("failed to restore active-run URL capabilities: {error:#}"))?;
    let image_attachment_resolver = image_attachment_resolver
        .ok_or_else(|| "active run lost its image attachment resolver".to_owned())?;
    session
        .try_attach_image_attachment_resolver(image_attachment_resolver)
        .map_err(|error| format!("failed to restore active-run image cache: {error:#}"))?;
    if tool_artifact_store
        .as_ref()
        .is_some_and(|store| store.session_scope_id() == session.session_scope_id())
    {
        session.attach_tool_artifact_store_override(
            tool_artifact_store.expect("artifact store was checked above"),
        );
    }
    Ok(session)
}

pub(in crate::runner) struct RunTaskResult {
    pub(in crate::runner) run_id: u64,
    pub(in crate::runner) session: Session,
    pub(in crate::runner) payload: RunTaskPayload,
    pub(in crate::runner) post_run_maintenance:
        Option<sigil_runtime::application_run::ApplicationPostRunMaintenance>,
}

pub(in crate::runner) enum RunTaskPayload {
    AwaitingUserInput {
        request: sigil_kernel::UserInputRequestRefV1,
    },
    Chat {
        result: std::result::Result<AgentRunResult, String>,
        plan_mode: bool,
        /// True when the chat run was an automatic plan review whose typed draft was already
        /// committed durably by the shared PlanReviewCoordinator.
        plan_review: bool,
        queue_id: Option<ConversationInputQueueId>,
        /// Present only for a first foreground conversation request that may qualify for the
        /// one-shot overflow-recovery controller. Recovery runs deliberately omit it.
        provider_logical_run_id: Option<String>,
        agent_result_continuation_thread_ids: Vec<AgentThreadId>,
    },
    PlanReviewBlocked {
        reason: String,
        paused: bool,
    },
    PlanReviewCancelled,
    PlanReviewInterrupted {
        reason: String,
    },
    Agent {
        profile_id: String,
        result: std::result::Result<AgentRunResult, String>,
    },
    Task {
        task_id: String,
        queue_id: Option<ConversationInputQueueId>,
        result: std::result::Result<TaskRunStatus, String>,
    },
}

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

// Cancellation acknowledgement is immediate. Crossing this target only emits a slow-cleanup
// notice: the same owner keeps every handle and settles once actual cleanup is known.
const RUN_QUIESCENCE_NOTICE_AFTER: Duration = Duration::from_secs(2);
const RUN_STOP_OBSERVATION_INTERVAL: Duration = Duration::from_millis(100);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::runner) enum ActiveRunStopDisposition {
    Cancel,
    PauseTask,
}

pub(in crate::runner) fn finalize_completed_run_cancellation(owner: &RunCancellationOwner) -> bool {
    // A returned failure or suspended model-selected operation may leave the root phase open. Close it
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
    state: &mut WorkerLoopState,
    message_tx: &mpsc::Sender<WorkerMessage>,
    elicitation_handler: &Arc<ChannelMcpElicitationHandler>,
    disposition: ActiveRunStopDisposition,
    reason: &str,
) {
    let url_capability_registrar = active_run.url_capability_registrar.clone();
    let image_attachment_resolver = active_run.image_attachment_resolver.clone();
    let stop_control = state.stop_control.clone();
    let tool_artifact_store = state
        .session
        .current
        .as_ref()
        .and_then(Session::tool_artifact_store);
    let background_runs = state.agent.background_runs.clone();
    elicitation_handler.set_audit_buffer(None);
    if !active_run.cancellation_owner.reserve_cancel()
        && !active_run.cancellation_owner.is_cancel_reserved()
    {
        let _ = message_tx.send(WorkerMessage::Notice(
            "run already reached its natural terminal state before cancellation".to_owned(),
        ));
        state.run.retired.push(active_run.handle);
        return;
    }
    let cancellation_scope_id = active_run.cancellation_owner.handle().scope_id().to_owned();
    let started = Instant::now();
    let notice_at = started + RUN_QUIESCENCE_NOTICE_AFTER;
    if stop_control.is_closing() {
        super::shutdown::request_independent_worker_stops(state);
    }
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
            .saturating_add(notice_at.saturating_duration_since(started).as_millis() as u64),
    };
    stop_control.stage(WorkerShutdownStage::CancellationRequest);
    let request_stage = active_run
        .cancellation_owner
        .handle()
        .begin_cleanup_stage(sigil_kernel::RunCleanupStage::AuditPersistence);
    let request_persisted = match active_run.cancellation_recorder.append_requested(&request) {
        Ok(_) => true,
        Err(error) => {
            stop_control.fail_stage(WorkerShutdownStage::CancellationRequest);
            active_run
                .cancellation_owner
                .handle()
                .mark_cleanup_incomplete();
            let _ = message_tx.send(WorkerMessage::Notice(format!(
                "cancellation audit request failed; cleanup will continue: {error:#}"
            )));
            false
        }
    };
    request_stage.finish(request_persisted);
    let _ = match (&disposition, &active_run.cancellation_target) {
        (ActiveRunStopDisposition::PauseTask, RunCancellationTarget::Task { task_id }) => {
            message_tx.send(WorkerMessage::TaskPauseRequested {
                task_id: task_id.clone(),
            })
        }
        _ => message_tx.send(WorkerMessage::RunCancellationRequested),
    };
    let activation_stage = active_run
        .cancellation_owner
        .handle()
        .begin_cleanup_stage(sigil_kernel::RunCleanupStage::CancellationActivation);
    let activated = active_run.cancellation_owner.activate_reserved_cancel();
    activation_stage.finish(activated);
    debug_assert!(
        activated,
        "reserved cancellation must activate exactly once"
    );
    state.run.discarded_ids.insert(active_run.run_id);
    let _ = active_run.approval_tx.send(ApprovalSignal::Cancel);
    let agent_cancel_impact = state
        .agent
        .supervisor
        .as_ref()
        .map(sigil_runtime::AgentSupervisor::cancel_foreground_run);
    let mut handle = active_run.handle;
    stop_control.stage(WorkerShutdownStage::RunQuiescence);
    let joined = wait_for_cancelled_run(
        &mut handle,
        &active_run.cancellation_owner,
        runtime,
        state,
        message_tx,
        notice_at,
    );
    let join_confirmed = joined.is_ok();
    if !join_confirmed || !active_run.cancellation_owner.cleanup_complete() {
        stop_control.fail_stage(WorkerShutdownStage::RunQuiescence);
        active_run
            .cancellation_owner
            .handle()
            .mark_cleanup_incomplete();
    }
    let mut cancellation_session = None;
    if !matches!(
        active_run.cancellation_target,
        RunCancellationTarget::AgentThread { .. }
    ) {
        match load_active_run_session(
            &root_config.agent.runtime_provider,
            &root_config.agent.model,
            state.session.log_path.as_path(),
            url_capability_registrar.clone(),
            image_attachment_resolver.clone(),
            tool_artifact_store.clone(),
            &background_runs,
            &cancellation_scope_id,
        ) {
            Ok(mut session) => {
                if let Err(error) =
                    runtime.block_on(background_runs.cancel_task_background_agents_for_scope(
                        &mut session,
                        &active_run.cancellation_target,
                        active_run.cancellation_owner.handle().scope_id(),
                        reason,
                        &mut ChannelEventHandler::new(message_tx.clone()),
                    ))
                {
                    stop_control.fail_stage(WorkerShutdownStage::CancellationFinalization);
                    active_run
                        .cancellation_owner
                        .handle()
                        .mark_cleanup_incomplete();
                    let _ = message_tx.send(WorkerMessage::Notice(format!(
                        "Direct Task background cleanup could not be confirmed after TUI cancellation: {error:#}"
                    )));
                }
                cancellation_session = Some(session);
            }
            Err(error) => {
                stop_control.fail_stage(WorkerShutdownStage::SessionReload);
                active_run
                    .cancellation_owner
                    .handle()
                    .mark_cleanup_incomplete();
                let _ = message_tx.send(WorkerMessage::Notice(format!(
                    "Direct Task background cleanup could not inspect the durable session after TUI cancellation: {error}"
                )));
            }
        }
    }
    let cleanup_complete = join_confirmed && active_run.cancellation_owner.cleanup_complete();
    let outcome = if cleanup_complete {
        RunCancellationTerminalOutcome::Cancelled
    } else {
        RunCancellationTerminalOutcome::Interrupted
    };
    let terminal_reason = match joined {
        Ok(()) if cleanup_complete => "cancellation quiescence confirmed",
        Ok(()) => "cancellation cleanup reported a failure",
        Err(error) if error.is_panic() => "run task panicked during cancellation cleanup",
        Err(_) => "run task was aborted before cancellation cleanup completed",
    }
    .to_owned();
    // The owned root has joined and every registered task/effect has released its guard.
    // A failed cleanup latch stays failed even though no operation is still active.
    let active_effects = active_run.cancellation_owner.handle().active_effects();
    let active_tasks = active_run.cancellation_owner.handle().active_tasks();
    let current_session_log_path = state.session.log_path.as_path();
    let current_session = &mut state.session.current;
    let detached_durable_controls = &mut state.session.detached_durable_controls;
    if !request_persisted {
        stop_control.fail_stage(WorkerShutdownStage::CancellationRequest);
        active_run
            .cancellation_owner
            .handle()
            .mark_cleanup_incomplete();
        stop_control.stage(WorkerShutdownStage::SessionReload);
        if let Some(session) = cancellation_session.take().or_else(|| {
            load_active_run_session(
                &root_config.agent.runtime_provider,
                &root_config.agent.model,
                current_session_log_path,
                url_capability_registrar.clone(),
                image_attachment_resolver.clone(),
                tool_artifact_store.clone(),
                &background_runs,
                &cancellation_scope_id,
            )
            .ok()
        }) {
            *current_session = Some(session);
            detached_durable_controls.clear();
        }
        let _ = message_tx.send(WorkerMessage::RunFailed(
            "run was interrupted, but its cancellation request could not be persisted".to_owned(),
        ));
        return;
    }
    stop_control.stage(WorkerShutdownStage::CancellationFinalization);
    let finalization_stage = active_run
        .cancellation_owner
        .handle()
        .begin_cleanup_stage(sigil_kernel::RunCleanupStage::AuditPersistence);
    let finalized =
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
            });
    finalization_stage.finish(finalized.is_ok());
    if let Err(error) = finalized {
        stop_control.fail_stage(WorkerShutdownStage::CancellationFinalization);
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
    let reloaded_session = cancellation_session.take().map_or_else(
        || {
            load_active_run_session(
                &root_config.agent.runtime_provider,
                &root_config.agent.model,
                current_session_log_path,
                url_capability_registrar,
                image_attachment_resolver,
                tool_artifact_store,
                &background_runs,
                &cancellation_scope_id,
            )
        },
        Ok,
    );
    match reloaded_session {
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
            let audit_stage = active_run
                .cancellation_owner
                .handle()
                .begin_cleanup_stage(sigil_kernel::RunCleanupStage::AuditPersistence);
            let audit_result =
                append_mcp_elicitation_audits(&mut session, &active_run.elicitation_audit_buffer);
            audit_stage.finish(audit_result.is_ok());
            if audit_result.is_err() {
                stop_control.fail_stage(WorkerShutdownStage::CancellationFinalization);
                active_run
                    .cancellation_owner
                    .handle()
                    .mark_cleanup_incomplete();
            }
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
                stop_control.fail_stage(WorkerShutdownStage::CancellationFinalization);
                active_run
                    .cancellation_owner
                    .handle()
                    .mark_cleanup_incomplete();
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
                    task_id,
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
                    task_id,
                    &terminal_reason,
                ),
                (
                    RunCancellationTerminalOutcome::Interrupted,
                    _,
                    RunCancellationTarget::Run | RunCancellationTarget::AgentThread { .. },
                ) => Ok(()),
            };
            if let Err(error) = task_state {
                stop_control.fail_stage(WorkerShutdownStage::CancellationFinalization);
                active_run
                    .cancellation_owner
                    .handle()
                    .mark_cleanup_incomplete();
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
                stop_control.fail_stage(WorkerShutdownStage::CancellationFinalization);
                active_run
                    .cancellation_owner
                    .handle()
                    .mark_cleanup_incomplete();
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
            stop_control.fail_stage(WorkerShutdownStage::SessionReload);
            active_run
                .cancellation_owner
                .handle()
                .mark_cleanup_incomplete();
            let _ = message_tx.send(WorkerMessage::RunFailed(format!("{error:#}")));
        }
    }
}

/// Keep the root owner on this worker while the UI remains independently responsive. A closing
/// flag can arrive during an ordinary Cancel/Pause, so stop independent owners before joining
/// this run; no shutdown command needs to pass through the blocked command queue first.
fn wait_for_cancelled_run(
    handle: &mut tokio::task::JoinHandle<()>,
    owner: &RunCancellationOwner,
    runtime: &tokio::runtime::Runtime,
    state: &mut WorkerLoopState,
    message_tx: &mpsc::Sender<WorkerMessage>,
    notice_at: Instant,
) -> Result<(), tokio::task::JoinError> {
    let mut joined = None;
    let mut join_stage = Some(
        owner
            .handle()
            .begin_cleanup_stage(sigil_kernel::RunCleanupStage::ThreadJoin),
    );
    let mut notice_sent = false;
    let mut independent_stops_requested = false;
    loop {
        if !independent_stops_requested && state.stop_control.is_closing() {
            super::shutdown::request_independent_worker_stops(state);
            independent_stops_requested = true;
        }
        if owner.is_quiescent()
            && let Some(result) = joined.take()
        {
            return result;
        }
        if !notice_sent && Instant::now() >= notice_at {
            let _ = message_tx.send(WorkerMessage::Notice(
                "cancellation is still cleaning up; waiting for owned tasks and effects to finish"
                    .to_owned(),
            ));
            notice_sent = true;
        }
        if joined.is_none() {
            if let Ok(result) = runtime.block_on(async {
                tokio::time::timeout(RUN_STOP_OBSERVATION_INTERVAL, &mut *handle).await
            }) {
                join_stage
                    .take()
                    .expect("the owned root is joined exactly once")
                    .finish(result.is_ok());
                // Preserve a real panic immediately while still waiting for independently
                // registered cleanup to finish. A timeout never changes this failure latch.
                if result.is_err() {
                    owner.handle().mark_cleanup_incomplete();
                    state
                        .stop_control
                        .fail_stage(WorkerShutdownStage::RunQuiescence);
                }
                joined = Some(result);
            }
        } else {
            runtime.block_on(owner.wait_for_quiescence(RUN_STOP_OBSERVATION_INTERVAL));
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
    background_runs: &sigil_runtime::AgentToolBackgroundRuns,
    live_cancellation_scope_id: &str,
) -> std::result::Result<Session, String> {
    let live_background_agent_threads = background_runs
        .thread_ids()
        .map_err(|error| format!("failed to inspect active background-agent owner: {error:#}"))?;
    let store = JsonlSessionStore::new(session_log_path)
        .map_err(|error| format!("failed to reopen active-run session: {error:#}"))?
        .with_live_background_agent_threads(live_background_agent_threads)
        .with_live_run_cancellation_scopes(std::collections::BTreeSet::from([
            live_cancellation_scope_id.to_owned(),
        ]));
    let mut session =
        Session::load_from_store(provider_name.to_owned(), model_name.to_owned(), store)
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

use super::*;

pub(in crate::runner) struct ActiveRun {
    pub(in crate::runner) run_id: u64,
    pub(in crate::runner) handle: tokio::task::JoinHandle<()>,
    pub(in crate::runner) approval_tx: mpsc::Sender<ApprovalSignal>,
    pub(in crate::runner) elicitation_audit_buffer: McpElicitationAuditBuffer,
    pub(in crate::runner) cancellation_owner: RunCancellationOwner,
    pub(in crate::runner) cancellation_recorder: RunCancellationRecorder,
    pub(in crate::runner) url_capability_registrar: Option<Arc<dyn UserUrlCapabilityRegistrar>>,
}

const RUN_QUIESCENCE_TIMEOUT: Duration = Duration::from_secs(5);

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

#[allow(clippy::too_many_arguments)]
pub(in crate::runner) fn cancel_active_run(
    active_run: ActiveRun,
    runtime: &tokio::runtime::Runtime,
    root_config: &RootConfig,
    current_session_log_path: &Path,
    current_session: &mut Option<Session>,
    message_tx: &mpsc::Sender<WorkerMessage>,
    elicitation_handler: &Arc<ChannelMcpElicitationHandler>,
    agent_supervisor: &sigil_runtime::AgentSupervisor,
    discarded_run_ids: &mut BTreeSet<u64>,
    reason: &str,
) {
    let url_capability_registrar = active_run.url_capability_registrar.clone();
    elicitation_handler.set_audit_buffer(None);
    if !active_run.cancellation_owner.reserve_cancel() {
        let _ = message_tx.send(WorkerMessage::Notice(
            "run already reached its natural terminal state before cancellation".to_owned(),
        ));
        return;
    }
    let requested_at_ms = current_unix_time_ms();
    let request_id = format!(
        "cancel-{}",
        active_run.cancellation_owner.handle().scope_id()
    );
    let request = RunCancellationRequestedEntry {
        request_id: request_id.clone(),
        run_scope_id: active_run.cancellation_owner.handle().scope_id().to_owned(),
        target: RunCancellationTarget::Run,
        reason: reason.to_owned(),
        requested_at_ms,
        quiescence_deadline_ms: requested_at_ms
            .saturating_add(RUN_QUIESCENCE_TIMEOUT.as_millis() as u64),
    };
    let request_persisted = match active_run.cancellation_recorder.append_requested(&request) {
        Ok(_) => true,
        Err(error) => {
            let _ = message_tx.send(WorkerMessage::Notice(format!(
                "cancellation audit request failed; cleanup will continue: {error:#}"
            )));
            false
        }
    };
    let _ = message_tx.send(WorkerMessage::RunCancellationRequested);
    let activated = active_run.cancellation_owner.activate_reserved_cancel();
    debug_assert!(
        activated,
        "reserved cancellation must activate exactly once"
    );
    discarded_run_ids.insert(active_run.run_id);
    let _ = active_run.approval_tx.send(ApprovalSignal::Cancel);
    let agent_cancel_impact = agent_supervisor.cancel_foreground_run();
    let mut handle = active_run.handle;
    let join_confirmed = runtime.block_on(async {
        matches!(
            tokio::time::timeout(RUN_QUIESCENCE_TIMEOUT, &mut handle).await,
            Ok(Ok(()))
        )
    });
    let quiescence = if join_confirmed {
        runtime.block_on(
            active_run
                .cancellation_owner
                .wait_for_quiescence(Duration::ZERO),
        )
    } else {
        handle.abort();
        let _ = runtime.block_on(handle);
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
    if !request_persisted {
        if let Ok(session) = load_active_run_session(
            &root_config.agent.provider,
            &root_config.agent.model,
            current_session_log_path,
            url_capability_registrar.clone(),
        ) {
            *current_session = Some(session);
        }
        let _ = message_tx.send(WorkerMessage::RunFailed(
            "run was interrupted, but its cancellation request could not be persisted".to_owned(),
        ));
        return;
    }
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
        let _ = message_tx.send(WorkerMessage::RunFailed(format!(
            "failed to persist cancellation terminal outcome: {error:#}"
        )));
        return;
    }
    match load_active_run_session(
        &root_config.agent.provider,
        &root_config.agent.model,
        current_session_log_path,
        url_capability_registrar,
    ) {
        Ok(session) => {
            let mut session = session;
            if let Err(error) =
                append_mcp_elicitation_audits(&mut session, &active_run.elicitation_audit_buffer)
            {
                let _ = message_tx.send(WorkerMessage::RunFailed(error));
                *current_session = Some(session);
                return;
            }
            let mut cancel_handler = ChannelEventHandler::new(message_tx.clone());
            if let Err(error) = sigil_runtime::AgentSupervisor::append_foreground_cancel_audit(
                &mut session,
                &mut cancel_handler,
                agent_cancel_impact,
                reason,
            ) {
                let _ = message_tx.send(WorkerMessage::RunFailed(format!(
                    "failed to append cancelled agent state: {error:#}"
                )));
                *current_session = Some(session);
                return;
            }
            let task_state = match outcome {
                RunCancellationTerminalOutcome::Cancelled => {
                    append_cancelled_task_state(&mut session)
                }
                RunCancellationTerminalOutcome::Interrupted => {
                    append_interrupted_task_state(&mut session, &terminal_reason)
                }
            };
            if let Err(error) = task_state {
                let _ = message_tx.send(WorkerMessage::RunFailed(error));
                *current_session = Some(session);
                return;
            }
            let entries = session.entries().to_vec();
            *current_session = Some(session);
            let message = match outcome {
                RunCancellationTerminalOutcome::Cancelled => WorkerMessage::RunCancelled {
                    session_log_path: current_session_log_path.to_path_buf(),
                    provider_name: current_session
                        .as_ref()
                        .map(|session| session.provider_name().to_owned())
                        .unwrap_or_else(|| root_config.agent.provider.clone()),
                    model_name: current_session
                        .as_ref()
                        .map(|session| session.model_name().to_owned())
                        .unwrap_or_else(|| root_config.agent.model.clone()),
                    entries,
                },
                RunCancellationTerminalOutcome::Interrupted => WorkerMessage::RunInterrupted {
                    session_log_path: current_session_log_path.to_path_buf(),
                    provider_name: current_session
                        .as_ref()
                        .map(|session| session.provider_name().to_owned())
                        .unwrap_or_else(|| root_config.agent.provider.clone()),
                    model_name: current_session
                        .as_ref()
                        .map(|session| session.model_name().to_owned())
                        .unwrap_or_else(|| root_config.agent.model.clone()),
                    reason: terminal_reason,
                    entries,
                },
            };
            let _ = message_tx.send(message);
        }
        Err(error) => {
            let _ = message_tx.send(WorkerMessage::RunFailed(format!("{error:#}")));
        }
    }
}

fn load_active_run_session(
    provider_name: &str,
    model_name: &str,
    session_log_path: &Path,
    registrar: Option<Arc<dyn UserUrlCapabilityRegistrar>>,
) -> std::result::Result<Session, String> {
    let mut session = load_session(provider_name, model_name, session_log_path)
        .map_err(|error| format!("failed to reload active-run session: {error:#}"))?;
    let registrar = registrar.ok_or_else(|| {
        "active run lost its session URL capability registrar attachment".to_owned()
    })?;
    session
        .try_attach_user_url_capability_registrar(registrar)
        .map_err(|error| format!("failed to restore active-run URL capabilities: {error:#}"))?;
    Ok(session)
}

pub(in crate::runner) struct RunTaskResult {
    pub(in crate::runner) run_id: u64,
    pub(in crate::runner) session: Session,
    pub(in crate::runner) payload: RunTaskPayload,
}

pub(in crate::runner) enum RunTaskPayload {
    Chat {
        result: std::result::Result<AgentRunResult, String>,
        plan_mode: bool,
        queue_id: Option<ConversationInputQueueId>,
        agent_result_continuation_thread_ids: Vec<AgentThreadId>,
    },
    Agent {
        profile_id: String,
        result: std::result::Result<AgentRunResult, String>,
    },
    Task {
        task_id: String,
        result: std::result::Result<TaskRunStatus, String>,
    },
}

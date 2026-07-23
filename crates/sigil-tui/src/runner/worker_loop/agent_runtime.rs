use super::*;

pub(in crate::runner) struct WorkerAgentEventSink {
    pub(in crate::runner) sender: mpsc::Sender<WorkerMessage>,
}

impl sigil_runtime::AgentToolBackgroundEventSink for WorkerAgentEventSink {
    fn handle_agent_event(&self, thread_id: &AgentThreadId, event: RunEvent) {
        let _ = self.sender.send(WorkerMessage::AgentThreadEvent {
            thread_id: thread_id.clone(),
            event: Box::new(event),
        });
    }

    fn handle_agent_status(
        &self,
        thread_id: &AgentThreadId,
        status: AgentThreadStatus,
        reason: Option<String>,
    ) {
        let _ = self.sender.send(WorkerMessage::AgentThreadStatusLive {
            entry: AgentThreadStatusChangedEntry {
                thread_id: thread_id.clone(),
                status,
                reason,
                updated_at_ms: Some(current_unix_time_ms()),
            },
        });
    }
}

pub(in crate::runner) fn manual_agent_invocation_result(
    invocation: &sigil_runtime::ManualAgentInvocationResult,
) -> AgentRunResult {
    let final_text = invocation
        .result
        .as_ref()
        .map(|result| result.summary.trim())
        .filter(|summary| !summary.is_empty())
        .map(str::to_owned)
        .unwrap_or_else(|| match invocation.status {
            Some(AgentThreadStatus::Running) | Some(AgentThreadStatus::Started) => format!(
                "agent {} is running in background",
                invocation.thread_id.as_str()
            ),
            Some(AgentThreadStatus::Failed) => {
                format!("agent {} failed", invocation.thread_id.as_str())
            }
            Some(AgentThreadStatus::Cancelled) | Some(AgentThreadStatus::Interrupted) => {
                format!("agent {} was interrupted", invocation.thread_id.as_str())
            }
            _ => format!("agent {} completed", invocation.thread_id.as_str()),
        });
    AgentRunResult {
        final_text,
        tool_calls: 0,
        final_message_id: None,
    }
}

pub(in crate::runner) fn manual_agent_parent_summary(
    profile_id: &str,
    invocation: &sigil_runtime::ManualAgentInvocationResult,
) -> String {
    let status = invocation
        .status
        .map(agent_thread_status_label)
        .unwrap_or("unknown");
    let mut lines = vec![
        format!(
            "Agent @{profile_id} finished. thread_id={} status={status}.",
            invocation.thread_id.as_str()
        ),
        "Use read_agent_result for the full child answer when more detail is needed.".to_owned(),
    ];
    if let Some(result) = invocation.result.as_ref() {
        let summary = result.summary.trim();
        if !summary.is_empty() {
            lines.push(String::new());
            lines.push("Summary:".to_owned());
            lines.push(summary.to_owned());
        }
        if result.final_answer_ref.is_some() {
            lines.push(String::new());
            lines.push("Full answer is available through the agent result reference.".to_owned());
        }
    }
    lines.join("\n")
}

pub(in crate::runner) fn agent_thread_status_label(status: AgentThreadStatus) -> &'static str {
    match status {
        AgentThreadStatus::Started => "started",
        AgentThreadStatus::Running => "running",
        AgentThreadStatus::Blocked => "blocked",
        AgentThreadStatus::Completed => "completed",
        AgentThreadStatus::Failed => "failed",
        AgentThreadStatus::Cancelled => "cancelled",
        AgentThreadStatus::Interrupted => "interrupted",
        AgentThreadStatus::Closed => "closed",
        AgentThreadStatus::Unavailable => "unavailable",
        AgentThreadStatus::Unknown => "unknown",
    }
}

pub(in crate::runner) fn collect_finished_background_agent_runs(
    runtime: &tokio::runtime::Runtime,
    background_runs: &sigil_runtime::AgentToolBackgroundRuns,
    agent_supervisor: &sigil_runtime::AgentSupervisor,
    root_config: &RootConfig,
    base_registry: &ToolRegistry,
    current_session: &mut Option<Session>,
    message_tx: &mpsc::Sender<WorkerMessage>,
) -> Vec<AgentThreadId> {
    if !background_runs.has_finished() {
        return Vec::new();
    }
    let Some(session) = current_session.as_mut() else {
        return Vec::new();
    };
    let mut handler = ChannelEventHandler::new(message_tx.clone());
    let mut agent_delegate = sigil_runtime::AgentToolRuntime::new(
        agent_supervisor.clone(),
        root_config.clone(),
        base_registry.clone(),
    )
    .with_background_runs(background_runs.clone());
    match runtime.block_on(agent_delegate.collect_finished_background_runs(session, &mut handler)) {
        Ok(thread_ids) => thread_ids,
        Err(error) => {
            let _ = message_tx.send(WorkerMessage::Notice(format!(
                "agent background collection failed: {error:#}"
            )));
            Vec::new()
        }
    }
}

pub(in crate::runner) fn partition_agent_result_continuations(
    session: Option<&Session>,
    thread_ids: Vec<AgentThreadId>,
) -> (Vec<AgentThreadId>, Vec<AgentThreadId>) {
    let projection = session.map(Session::agent_thread_state_projection);
    let mut blocking = Vec::new();
    let mut non_blocking = Vec::new();
    for thread_id in thread_ids {
        let non_blocking_background = projection
            .as_ref()
            .and_then(|projection| projection.threads.get(&thread_id))
            .and_then(|thread| thread.invocation_mode)
            .is_some_and(|mode| mode == AgentInvocationMode::Background);
        if non_blocking_background {
            non_blocking.push(thread_id);
        } else {
            blocking.push(thread_id);
        }
    }
    (blocking, non_blocking)
}

pub(in crate::runner) fn pending_agent_result_continuations_from_session(
    session: Option<&Session>,
) -> Vec<AgentThreadId> {
    session
        .map(Session::agent_result_continuation_projection)
        .map(|projection| projection.pending_thread_ids)
        .unwrap_or_default()
}

pub(in crate::runner) fn agent_result_continuation_new_thread_ids(
    session: Option<&Session>,
    thread_ids: &[AgentThreadId],
) -> Vec<AgentThreadId> {
    let projection = session.map(Session::agent_result_continuation_projection);
    thread_ids
        .iter()
        .filter(|thread_id| {
            projection
                .as_ref()
                .and_then(|projection| projection.statuses.get(*thread_id))
                .is_none()
        })
        .cloned()
        .collect()
}

pub(in crate::runner) fn extend_agent_thread_ids_unique(
    target: &mut Vec<AgentThreadId>,
    thread_ids: impl IntoIterator<Item = AgentThreadId>,
) {
    for thread_id in thread_ids {
        if !target.contains(&thread_id) {
            target.push(thread_id);
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub(in crate::runner) fn start_agent_result_continuation_run<P>(
    runtime: &tokio::runtime::Runtime,
    agent: Arc<Agent<P>>,
    agent_supervisor: &sigil_runtime::AgentSupervisor,
    root_config: &RootConfig,
    session_log_path: &Path,
    base_registry: &ToolRegistry,
    options: &AgentRunOptions,
    background_runs: &sigil_runtime::AgentToolBackgroundRuns,
    current_session: &mut Option<Session>,
    task_result_tx: &mpsc::Sender<RunTaskResult>,
    message_tx: &mpsc::Sender<WorkerMessage>,
    elicitation_handler: Arc<ChannelMcpElicitationHandler>,
    next_run_id: &mut u64,
    completed_thread_ids: Vec<AgentThreadId>,
) -> Option<ActiveRun>
where
    P: sigil_kernel::Provider + Send + Sync + 'static,
{
    if let Err(error) = append_agent_result_continuation_status_entries(
        session_log_path,
        current_session,
        &completed_thread_ids,
        AgentResultContinuationStatus::Started,
        Some("parent continuation started"),
    ) {
        let _ = message_tx.send(WorkerMessage::RunFailed(error));
        return None;
    }
    let Some(run_session) = current_session.take() else {
        let _ = message_tx.send(WorkerMessage::RunFailed(
            "session state is unavailable for agent result continuation".to_owned(),
        ));
        return None;
    };

    let _ = message_tx.send(WorkerMessage::AgentResultContinuationStarted {
        thread_ids: completed_thread_ids.clone(),
    });

    let mut handler = ChannelEventHandler::new(message_tx.clone());
    let (approval_tx, approval_rx) = mpsc::channel();
    let elicitation_audit_buffer: McpElicitationAuditBuffer =
        Arc::new(std::sync::Mutex::new(Vec::new()));
    elicitation_handler.set_audit_buffer(Some(Arc::clone(&elicitation_audit_buffer)));
    let run_elicitation_audit_buffer = Arc::clone(&elicitation_audit_buffer);
    let mut agent_delegate = sigil_runtime::AgentToolRuntime::new(
        agent_supervisor.clone(),
        root_config.clone(),
        base_registry.clone(),
    )
    .with_background_runs(background_runs.clone());
    let options = options.clone();
    let task_result_tx = task_result_tx.clone();
    let run_id = *next_run_id;
    *next_run_id = (*next_run_id).saturating_add(1);
    let continuation_prompt = agent_result_continuation_prompt(&completed_thread_ids);
    let (cancellation_owner, cancellation_recorder, cancellation_handle, cancellation_task_guard) =
        match prepare_run_cancellation(&run_session) {
            Ok(cancellation) => cancellation,
            Err(error) => {
                *current_session = Some(run_session);
                let _ = message_tx.send(WorkerMessage::RunFailed(error));
                return None;
            }
        };

    let url_capability_registrar = run_session.user_url_capability_registrar();
    let image_attachment_resolver = run_session.image_attachment_resolver();
    let handle = runtime.spawn(async move {
        let _cancellation_task_guard = cancellation_task_guard;
        let mut run_session = run_session;
        let result = {
            let mut approval_handler = ChannelApprovalHandler::new(approval_rx);
            agent
                .run_with_approval_input_and_agent_delegate(
                    &mut run_session,
                    AgentRunInput::without_persisted_user_message(vec![ModelMessage::user(
                        continuation_prompt,
                    )])
                    .with_cancellation(cancellation_handle),
                    options,
                    &mut handler,
                    &mut approval_handler,
                    &mut agent_delegate,
                )
                .await
                .map(|output| output.result)
                .map_err(|error| format!("{error:#}"))
        };
        let result =
            match append_mcp_elicitation_audits(&mut run_session, &run_elicitation_audit_buffer) {
                Ok(()) => result,
                Err(error) => Err(error),
            };
        let _ = task_result_tx.send(RunTaskResult {
            run_id,
            session: run_session,
            payload: RunTaskPayload::Chat {
                result,
                plan_mode: false,
                queue_id: None,
                provider_logical_run_id: None,
                agent_result_continuation_thread_ids: completed_thread_ids,
            },
        });
    });

    Some(ActiveRun {
        run_id,
        handle,
        approval_tx,
        elicitation_audit_buffer,
        cancellation_owner,
        cancellation_recorder,
        cancellation_target: RunCancellationTarget::Run,
        url_capability_registrar,
        image_attachment_resolver,
    })
}

pub(in crate::runner) fn agent_result_continuation_prompt(thread_ids: &[AgentThreadId]) -> String {
    let threads = thread_ids
        .iter()
        .map(AgentThreadId::as_str)
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "One or more background child agents completed: {threads}.\n\
         Continue the original user request now. First call wait_agent for each completed thread \
         to collect its terminal status and result reference. Use read_agent_result only when the \
         bounded summary is insufficient. Do not copy the child transcript directly into the \
         parent conversation; summarize only the child result needed for the final answer."
    )
}

#[allow(clippy::too_many_arguments)]
pub(in crate::runner) fn start_queued_conversation_run<P>(
    runtime: &tokio::runtime::Runtime,
    agent: Arc<Agent<P>>,
    agent_supervisor: &sigil_runtime::AgentSupervisor,
    root_config: &RootConfig,
    base_registry: &ToolRegistry,
    options: &AgentRunOptions,
    background_runs: &sigil_runtime::AgentToolBackgroundRuns,
    current_session: &mut Option<Session>,
    task_result_tx: &mpsc::Sender<RunTaskResult>,
    message_tx: &mpsc::Sender<WorkerMessage>,
    elicitation_handler: Arc<ChannelMcpElicitationHandler>,
    role_provider_builder: Arc<dyn TaskRoleProviderBuilder>,
    session_log_path: &Path,
    next_run_id: &mut u64,
    queued: PreparedQueuedConversationCandidate,
) -> Option<ActiveRun>
where
    P: sigil_kernel::Provider + Send + Sync + 'static,
{
    let queue_id = queued.promotion.queue_id.clone();
    let safe_prompt = queued
        .promotion
        .durable_user_message
        .content
        .clone()
        .unwrap_or_default();
    let reasoning_effort = queued.reasoning_effort.clone();
    let dispatch_run_id = queued.promotion.dispatch_run_id.clone();
    let frozen_request = queued.frozen_request;
    let Some(session) = current_session.as_ref() else {
        let _ = message_tx.send(WorkerMessage::RunFailed(
            "session state is unavailable for follow-up".to_owned(),
        ));
        return None;
    };
    let parent_session_ref = match session_ref_for_log_path(session_log_path) {
        Ok(session_ref) => session_ref,
        Err(error) => {
            append_queue_status_and_notify(
                current_session,
                message_tx,
                queue_id,
                ConversationInputStatus::Rejected,
                Some(error.clone()),
            );
            let _ = message_tx.send(WorkerMessage::RunFailed(error));
            return None;
        }
    };
    let (cancellation_owner, cancellation_recorder, cancellation_handle, cancellation_task_guard) =
        match prepare_run_cancellation(session) {
            Ok(cancellation) => cancellation,
            Err(error) => {
                append_queue_status_and_notify(
                    current_session,
                    message_tx,
                    queue_id,
                    ConversationInputStatus::Rejected,
                    Some(error.clone()),
                );
                let _ = message_tx.send(WorkerMessage::RunFailed(error));
                return None;
            }
        };
    let Some(run_session) = current_session.take() else {
        let _ = message_tx.send(WorkerMessage::RunFailed(
            "session state is unavailable for follow-up".to_owned(),
        ));
        return None;
    };

    let _ = message_tx.send(WorkerMessage::ConversationQueueDispatchStarted {
        queue_id: queue_id.clone(),
        prompt: safe_prompt.clone(),
    });

    let mut handler = ChannelEventHandler::new(message_tx.clone());
    let (approval_tx, approval_rx) = mpsc::channel();
    let elicitation_audit_buffer: McpElicitationAuditBuffer =
        Arc::new(std::sync::Mutex::new(Vec::new()));
    elicitation_handler.set_audit_buffer(Some(Arc::clone(&elicitation_audit_buffer)));
    let run_elicitation_audit_buffer = Arc::clone(&elicitation_audit_buffer);
    let run_message_tx = message_tx.clone();
    let mut options = options.clone();
    if let Some(reasoning_effort) = reasoning_effort {
        options.reasoning_effort = Some(reasoning_effort);
    }
    let mut agent_delegate = sigil_runtime::AgentToolRuntime::new(
        agent_supervisor.clone(),
        root_config.clone(),
        base_registry.clone(),
    )
    .with_background_runs(background_runs.clone());
    let task_result_tx = task_result_tx.clone();
    let conversation_coordinator =
        ConversationCoordinator::new(root_config.task.enabled, root_config.task.routing_policy);
    let task_root_config = root_config.clone();
    let task_base_registry = base_registry.clone();
    let task_agent_supervisor = agent_supervisor.clone();
    let run_id = *next_run_id;
    *next_run_id = (*next_run_id).saturating_add(1);
    let url_capability_registrar = run_session.user_url_capability_registrar();
    let image_attachment_resolver = run_session.image_attachment_resolver();
    let handle = runtime.spawn(async move {
        let _cancellation_task_guard = cancellation_task_guard;
        let mut run_session = run_session;
        let mut payload = {
            let mut approval_handler = ChannelApprovalHandler::new(approval_rx);
            let input = AgentRunInput::without_persisted_user_message(Vec::new())
                .with_initial_frozen_provider_request(frozen_request);
            let input = conversation_coordinator
                .bind_conversation_input(
                    &run_session,
                    input,
                    parent_session_ref.clone(),
                    dispatch_run_id.clone(),
                    Some(ConversationSourceTurn {
                        message_id: queued.promotion.durable_user_message.id.clone(),
                        objective: safe_prompt.clone(),
                    }),
                    current_unix_time_ms(),
                )
                .map(|input| input.with_cancellation(cancellation_handle.clone()))
                .map_err(|error| format!("{error:#}"));
            let output = match input {
                Ok(input) => agent
                    .run_with_approval_input_and_agent_delegate(
                        &mut run_session,
                        input,
                        options.clone(),
                        &mut handler,
                        &mut approval_handler,
                        &mut agent_delegate,
                    )
                    .await
                    .map_err(|error| format!("{error:#}")),
                Err(error) => Err(error),
            };
            match output {
                Ok(output) => match output.disposition {
                    AgentRunDisposition::FinalAnswer => RunTaskPayload::Chat {
                        result: Ok(output.result),
                        plan_mode: false,
                        queue_id: Some(queue_id.clone()),
                        provider_logical_run_id: None,
                        agent_result_continuation_thread_ids: Vec::new(),
                    },
                    AgentRunDisposition::StartDurableTask(action) => {
                        let projection = run_session.task_state_projection();
                        let task = projection.tasks.get(&action.task_id).cloned();
                        match task {
                            Some(task) => {
                                let task_id = action.task_id.as_str().to_owned();
                                let _ = run_message_tx.send(WorkerMessage::TaskRunStarted {
                                    task_id: task_id.clone(),
                                    objective: task.objective.clone(),
                                });
                                let result = run_admitted_task_to_root_terminal(
                                    &mut run_session,
                                    AdmittedTaskRunOrchestration {
                                        task_id: action.task_id,
                                        parent_session_ref: task.parent_session_ref,
                                        objective: task.objective,
                                        root_config: task_root_config,
                                        options,
                                        base_registry: task_base_registry,
                                        agent_supervisor: task_agent_supervisor,
                                        role_provider_builder: role_provider_builder.as_ref(),
                                        handler: &mut handler,
                                        cancellation_handle,
                                    },
                                    &mut approval_handler,
                                )
                                .await;
                                RunTaskPayload::Task {
                                    task_id,
                                    queue_id: Some(queue_id.clone()),
                                    result,
                                }
                            }
                            None => {
                                let error = if cancellation_handle.try_finalize_naturally() {
                                    "accepted task handoff is missing its durable task".to_owned()
                                } else {
                                    "run cancellation won the missing-task terminal-state race"
                                        .to_owned()
                                };
                                RunTaskPayload::Chat {
                                    result: Err(error),
                                    plan_mode: false,
                                    queue_id: Some(queue_id.clone()),
                                    provider_logical_run_id: None,
                                    agent_result_continuation_thread_ids: Vec::new(),
                                }
                            }
                        }
                    }
                    AgentRunDisposition::Interrupted => RunTaskPayload::Chat {
                        result: Err("run was interrupted before a final answer".to_owned()),
                        plan_mode: false,
                        queue_id: Some(queue_id.clone()),
                        provider_logical_run_id: None,
                        agent_result_continuation_thread_ids: Vec::new(),
                    },
                    AgentRunDisposition::Blocked => RunTaskPayload::Chat {
                        result: Err("run was blocked before a final answer".to_owned()),
                        plan_mode: false,
                        queue_id: Some(queue_id.clone()),
                        provider_logical_run_id: None,
                        agent_result_continuation_thread_ids: Vec::new(),
                    },
                    AgentRunDisposition::TaskPlanAccepted => RunTaskPayload::Chat {
                        result: Err("task planning completed outside a task run".to_owned()),
                        plan_mode: false,
                        queue_id: Some(queue_id.clone()),
                        provider_logical_run_id: None,
                        agent_result_continuation_thread_ids: Vec::new(),
                    },
                },
                Err(error) => RunTaskPayload::Chat {
                    result: Err(error),
                    plan_mode: false,
                    queue_id: Some(queue_id.clone()),
                    provider_logical_run_id: None,
                    agent_result_continuation_thread_ids: Vec::new(),
                },
            }
        };
        if let Err(error) =
            append_mcp_elicitation_audits(&mut run_session, &run_elicitation_audit_buffer)
        {
            payload = match payload {
                RunTaskPayload::Chat {
                    plan_mode,
                    queue_id,
                    provider_logical_run_id,
                    agent_result_continuation_thread_ids,
                    ..
                } => RunTaskPayload::Chat {
                    result: Err(error),
                    plan_mode,
                    queue_id,
                    provider_logical_run_id,
                    agent_result_continuation_thread_ids,
                },
                RunTaskPayload::Task {
                    task_id, queue_id, ..
                } => RunTaskPayload::Task {
                    task_id,
                    queue_id,
                    result: Err(error),
                },
                RunTaskPayload::Agent { profile_id, .. } => RunTaskPayload::Agent {
                    profile_id,
                    result: Err(error),
                },
            };
        }
        let _ = task_result_tx.send(RunTaskResult {
            run_id,
            session: run_session,
            payload,
        });
    });

    Some(ActiveRun {
        run_id,
        handle,
        approval_tx,
        elicitation_audit_buffer,
        cancellation_owner,
        cancellation_recorder,
        cancellation_target: RunCancellationTarget::Run,
        url_capability_registrar,
        image_attachment_resolver,
    })
}

/// Starts the single post-compaction retry for an already durable, exact context-window
/// rejection. The recovered first provider turn receives the frozen target directly, so it does
/// not append the user message again or rebuild a different request.
#[allow(clippy::too_many_arguments)]
pub(in crate::runner) fn start_portable_overflow_recovery_run<P>(
    runtime: &tokio::runtime::Runtime,
    agent: Arc<Agent<P>>,
    agent_supervisor: &sigil_runtime::AgentSupervisor,
    root_config: &RootConfig,
    base_registry: &ToolRegistry,
    options: &AgentRunOptions,
    background_runs: &sigil_runtime::AgentToolBackgroundRuns,
    current_session: &mut Option<Session>,
    task_result_tx: &mpsc::Sender<RunTaskResult>,
    message_tx: &mpsc::Sender<WorkerMessage>,
    elicitation_handler: Arc<ChannelMcpElicitationHandler>,
    next_run_id: &mut u64,
    frozen_request: sigil_kernel::FrozenProviderRequestMaterial,
    logical_run_id: String,
) -> anyhow::Result<ActiveRun>
where
    P: sigil_kernel::Provider + Send + Sync + 'static,
{
    let session = current_session
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("session state is unavailable for overflow recovery"))?;
    let (cancellation_owner, cancellation_recorder, cancellation_handle, cancellation_task_guard) =
        prepare_run_cancellation(session).map_err(anyhow::Error::msg)?;
    let run_session = current_session
        .take()
        .ok_or_else(|| anyhow::anyhow!("session state is unavailable for overflow recovery"))?;

    let _ = message_tx.send(WorkerMessage::Notice(
        "context window was rejected before generation; compacted history and retrying once"
            .to_owned(),
    ));
    let mut handler = ChannelEventHandler::new(message_tx.clone());
    let (approval_tx, approval_rx) = mpsc::channel();
    let elicitation_audit_buffer: McpElicitationAuditBuffer =
        Arc::new(std::sync::Mutex::new(Vec::new()));
    elicitation_handler.set_audit_buffer(Some(Arc::clone(&elicitation_audit_buffer)));
    let run_elicitation_audit_buffer = Arc::clone(&elicitation_audit_buffer);
    let mut agent_delegate = sigil_runtime::AgentToolRuntime::new(
        agent_supervisor.clone(),
        root_config.clone(),
        base_registry.clone(),
    )
    .with_background_runs(background_runs.clone());
    let options = options.clone();
    let task_result_tx = task_result_tx.clone();
    let run_id = *next_run_id;
    *next_run_id = (*next_run_id).saturating_add(1);
    let url_capability_registrar = run_session.user_url_capability_registrar();
    let image_attachment_resolver = run_session.image_attachment_resolver();
    let handle = runtime.spawn(async move {
        let _cancellation_task_guard = cancellation_task_guard;
        let mut run_session = run_session;
        let result = {
            let mut approval_handler = ChannelApprovalHandler::new(approval_rx);
            let input = AgentRunInput::without_persisted_user_message(Vec::new())
                .with_initial_frozen_provider_request(frozen_request)
                .with_logical_run_id(logical_run_id)
                .with_cancellation(cancellation_handle);
            agent
                .run_with_approval_input_and_agent_delegate(
                    &mut run_session,
                    input,
                    options,
                    &mut handler,
                    &mut approval_handler,
                    &mut agent_delegate,
                )
                .await
                .map(|output| output.result)
                .map_err(|error| format!("{error:#}"))
        };
        let result =
            match append_mcp_elicitation_audits(&mut run_session, &run_elicitation_audit_buffer) {
                Ok(()) => result,
                Err(error) => Err(error),
            };
        let _ = task_result_tx.send(RunTaskResult {
            run_id,
            session: run_session,
            payload: RunTaskPayload::Chat {
                result,
                plan_mode: false,
                queue_id: None,
                provider_logical_run_id: None,
                agent_result_continuation_thread_ids: Vec::new(),
            },
        });
    });

    Ok(ActiveRun {
        run_id,
        handle,
        approval_tx,
        elicitation_audit_buffer,
        cancellation_owner,
        cancellation_recorder,
        cancellation_target: RunCancellationTarget::Run,
        url_capability_registrar,
        image_attachment_resolver,
    })
}

pub(in crate::runner) async fn chat_agent_run_input_with_repo_context(
    context_resolver: &sigil_runtime::RequestContextResolver,
    prompt: String,
    plan_mode: bool,
    background_ready_context: Vec<ModelMessage>,
) -> AgentRunInput {
    let runtime_context = context_resolver.resolve(&prompt).await.unwrap_or_default();
    let input = if plan_mode {
        let mut transient_context = plan_mode_transient_context(prompt);
        transient_context.extend(background_ready_context);
        AgentRunInput::without_persisted_user_message(transient_context)
    } else if background_ready_context.is_empty() {
        AgentRunInput::user(prompt)
    } else {
        AgentRunInput::transient(prompt, background_ready_context)
    };
    input.with_runtime_context(runtime_context)
}

pub(in crate::runner) fn append_mcp_elicitation_audits(
    session: &mut Session,
    audit_buffer: &McpElicitationAuditBuffer,
) -> std::result::Result<(), String> {
    let controls = {
        let mut buffer = audit_buffer
            .lock()
            .map_err(|_| "failed to lock MCP elicitation audit buffer".to_owned())?;
        std::mem::take(&mut *buffer)
    };
    for control in controls {
        if let Some(route) = subagent_elicitation_route_for_control(session, &control) {
            session
                .append_control(route)
                .map_err(|error| format!("failed to append MCP elicitation route: {error:#}"))?;
        }
        session
            .append_control(control)
            .map_err(|error| format!("failed to append MCP elicitation audit: {error:#}"))?;
    }
    Ok(())
}

pub(in crate::runner) fn subagent_elicitation_route_for_control(
    session: &Session,
    control: &ControlEntry,
) -> Option<ControlEntry> {
    let ControlEntry::McpElicitation(elicitation) = control else {
        return None;
    };
    let projection = session.task_state_projection();
    let task = projection.latest_task()?;
    let child = current_subagent_child(task)
        .or_else(|| latest_subagent_child_from_entries(session, task))?;
    let status = match elicitation.action {
        sigil_kernel::McpElicitationDecision::Accepted => TaskRouteStatus::Resolved,
        sigil_kernel::McpElicitationDecision::Declined => TaskRouteStatus::Rejected,
        sigil_kernel::McpElicitationDecision::Cancelled => TaskRouteStatus::Cancelled,
    };
    let route_id = TaskRouteId::new(format!(
        "route_mcp_{}",
        stable_route_suffix(&elicitation.message_hash)
    ))
    .ok()?;
    Some(ControlEntry::TaskSubagentElicitationRoute(
        TaskSubagentElicitationRouteEntry {
            route_id,
            task_id: task.task_id.clone(),
            plan_version: child.plan_version,
            step_id: child.step_id.clone(),
            role: child.role,
            child_session_ref: child.child_session_ref.clone(),
            server_name: elicitation.server_name.clone(),
            status,
        },
    ))
}

pub(in crate::runner) fn current_subagent_child(
    task: &TaskRunProjection,
) -> Option<TaskChildSessionEntry> {
    let (plan_version, step_id) = task.current_step.as_ref()?;
    task.child_sessions
        .values()
        .find(|child| {
            child.plan_version == *plan_version
                && child.step_id == *step_id
                && is_routable_subagent_child(child)
        })
        .cloned()
}

pub(in crate::runner) fn latest_subagent_child_from_entries(
    session: &Session,
    task: &TaskRunProjection,
) -> Option<TaskChildSessionEntry> {
    session.entries().iter().rev().find_map(|entry| {
        let SessionLogEntry::Control(ControlEntry::TaskChildSession(child)) = entry else {
            return None;
        };
        if child.task_id == task.task_id && is_routable_subagent_child(child) {
            Some(child.clone())
        } else {
            None
        }
    })
}

pub(in crate::runner) fn is_routable_subagent_child(child: &TaskChildSessionEntry) -> bool {
    matches!(
        child.role,
        AgentRole::SubagentRead | AgentRole::SubagentWrite
    ) && child.status != TaskChildSessionStatus::Unavailable
}

pub(in crate::runner) fn stable_route_suffix(value: &str) -> String {
    let suffix = value
        .chars()
        .filter(|character| character.is_ascii_alphanumeric())
        .take(16)
        .collect::<String>();
    if suffix.is_empty() {
        "unknown".to_owned()
    } else {
        suffix
    }
}

pub(in crate::runner) fn close_agent_thread(
    root_config: &RootConfig,
    current_session_log_path: &Path,
    current_session: &mut Option<Session>,
    thread_id: AgentThreadId,
    reason: Option<String>,
) -> std::result::Result<(AgentThreadId, Vec<SessionLogEntry>), String> {
    let mut session = load_session_with_runtime_attachments(
        &root_config.agent.provider,
        &root_config.agent.model,
        current_session_log_path,
        current_session.as_ref(),
    )
    .map_err(|error| format!("failed to load session before agent close: {error:#}"))?;
    let mut result = sigil_runtime::close_agent_thread(&session, thread_id.clone(), reason);
    if result.is_error() {
        *current_session = Some(session);
        return Err(format!("agent close failed: {}", result.content));
    }

    let mut closed_thread_id = None;
    for control in std::mem::take(&mut result.control_entries) {
        if let ControlEntry::AgentThreadClosed(close) = &control {
            closed_thread_id = Some(close.thread_id.clone());
        }
        session
            .append_control(control)
            .map_err(|error| format!("failed to append agent close state: {error:#}"))?;
    }
    let entries = session.entries().to_vec();
    *current_session = Some(session);
    Ok((closed_thread_id.unwrap_or(thread_id), entries))
}

#[allow(clippy::too_many_arguments)]
pub(in crate::runner) fn cancel_agent_thread(
    runtime: &tokio::runtime::Runtime,
    background_runs: &sigil_runtime::AgentToolBackgroundRuns,
    agent_supervisor: &sigil_runtime::AgentSupervisor,
    root_config: &RootConfig,
    base_registry: &ToolRegistry,
    options: &AgentRunOptions,
    current_session: &mut Option<Session>,
    thread_id: AgentThreadId,
    reason: Option<String>,
) -> std::result::Result<(AgentThreadId, Vec<SessionLogEntry>), String> {
    let Some(session) = current_session.as_mut() else {
        return Err("session state is unavailable before agent cancel".to_owned());
    };
    let mut agent_delegate = sigil_runtime::AgentToolRuntime::new(
        agent_supervisor.clone(),
        root_config.clone(),
        base_registry.clone(),
    )
    .with_background_runs(background_runs.clone());
    let result = runtime
        .block_on(agent_delegate.cancel_agent_thread(session, thread_id.clone(), reason, options))
        .map_err(|error| format!("agent cancel failed: {error:#}"))?;
    if result.is_error() {
        return Err(format!("agent cancel failed: {}", result.content));
    }
    Ok((thread_id, session.entries().to_vec()))
}

#[allow(clippy::too_many_arguments)]
pub(in crate::runner) fn message_agent_thread(
    runtime: &tokio::runtime::Runtime,
    background_runs: &sigil_runtime::AgentToolBackgroundRuns,
    agent_supervisor: &sigil_runtime::AgentSupervisor,
    root_config: &RootConfig,
    base_registry: &ToolRegistry,
    options: &AgentRunOptions,
    current_session: &mut Option<Session>,
    thread_id: AgentThreadId,
    prompt: String,
) -> std::result::Result<(ToolResult, Vec<ControlEntry>), String> {
    let Some(session) = current_session.as_mut() else {
        return Err("session state is unavailable before agent message".to_owned());
    };
    let mut agent_delegate = sigil_runtime::AgentToolRuntime::new(
        agent_supervisor.clone(),
        root_config.clone(),
        base_registry.clone(),
    )
    .with_background_runs(background_runs.clone());
    runtime
        .block_on(agent_delegate.route_agent_message(session, thread_id, prompt, options))
        .map_err(|error| format!("agent message failed: {error:#}"))
}

pub(in crate::runner) fn queued_background_ready_transient_context(
    session: Option<&Session>,
) -> Vec<ModelMessage> {
    const MAX_READY_THREAD_IDS: usize = 5;
    let Some(session) = session else {
        return Vec::new();
    };
    let mut thread_ids = session
        .agent_result_continuation_projection()
        .pending_thread_ids
        .into_iter()
        .map(|thread_id| thread_id.as_str().to_owned())
        .collect::<Vec<_>>();
    if thread_ids.is_empty() {
        return Vec::new();
    }
    let hidden_count = thread_ids.len().saturating_sub(MAX_READY_THREAD_IDS);
    thread_ids.truncate(MAX_READY_THREAD_IDS);
    let hidden_suffix = if hidden_count == 0 {
        String::new()
    } else {
        format!(" and {hidden_count} more")
    };
    vec![ModelMessage::system(format!(
        "Background agent result ready notice: child agent results are available for {}{}. This notice is transient and does not preempt the queued user input. Continue the queued user request first; call wait_agent/read_agent_result only if those background results are relevant.",
        thread_ids.join(", "),
        hidden_suffix
    ))]
}

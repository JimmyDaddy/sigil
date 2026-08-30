use super::*;

pub(in crate::runner) fn effective_orchestration_root_config(
    root_config: &RootConfig,
    session: &Session,
) -> RootConfig {
    let mut effective = root_config.clone();
    sigil_runtime::OrchestrationRouteGuard::new(
        session.provider_name(),
        session.model_name(),
        sigil_runtime::ORCHESTRATION_RUNTIME_BUILD_ID,
    )
    .apply_effective_task_config(session, &mut effective.task);
    effective
}

pub(in crate::runner) struct WorkerAgentEventSink {
    pub(in crate::runner) sender: mpsc::Sender<WorkerMessage>,
    pub(in crate::runner) wake_coalescer: WorkerWakeCoalescer,
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

    fn handle_agent_completion_ready(&self, thread_id: &AgentThreadId) {
        self.wake_coalescer.notify_background_agent(thread_id);
    }
}

pub(in crate::runner) struct WorkerSupervisorEventSink {
    pub(in crate::runner) wake_coalescer: WorkerWakeCoalescer,
}

impl sigil_runtime::AgentSupervisorEventSink for WorkerSupervisorEventSink {
    fn handle_supervisor_change(&self, change: sigil_runtime::AgentSupervisorChange) {
        self.wake_coalescer.notify_supervisor(change);
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
        effective_orchestration_root_config(root_config, session),
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
            session
                .map(Session::agent_thread_state_projection)
                .and_then(|threads| threads.threads.get(*thread_id).cloned())
                .is_some_and(|thread| thread.result.is_some())
                && projection
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
    task_result_tx: &WorkerEventPayloadSender<RunTaskResult>,
    message_tx: &mpsc::Sender<WorkerMessage>,
    elicitation_handler: Arc<ChannelMcpElicitationHandler>,
    next_run_id: &mut u64,
    _terminal_lifecycle_router: &ChannelTerminalLifecycleRouter,
    tool_artifact_read_budget: ToolArtifactReadBudgetV1,
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
        effective_orchestration_root_config(root_config, &run_session),
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
                    .with_tool_artifact_read_budget(tool_artifact_read_budget)
                    .with_cancellation(cancellation_handle),
                    options,
                    &mut handler,
                    &mut approval_handler,
                    &mut agent_delegate,
                )
                .await
                .map_err(|error| format!("{error:#}"))
                .and_then(agent_result_continuation_run_result)
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
                plan_review: false,
                queue_id: None,
                provider_logical_run_id: None,
                agent_result_continuation_thread_ids: completed_thread_ids,
            },
            post_run_maintenance: None,
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

#[allow(clippy::too_many_arguments)]
pub(in crate::runner) fn apply_user_input_decision<P>(
    runtime: &tokio::runtime::Runtime,
    agent: Arc<Agent<P>>,
    agent_supervisor: &sigil_runtime::AgentSupervisor,
    root_config: &RootConfig,
    base_registry: &ToolRegistry,
    options: &AgentRunOptions,
    background_runs: &sigil_runtime::AgentToolBackgroundRuns,
    current_session: &mut Option<Session>,
    task_result_tx: &WorkerEventPayloadSender<RunTaskResult>,
    message_tx: &mpsc::Sender<WorkerMessage>,
    elicitation_handler: Arc<ChannelMcpElicitationHandler>,
    next_run_id: &mut u64,
    tool_artifact_read_budget: ToolArtifactReadBudgetV1,
    identity: sigil_kernel::UserInputIdentityV1,
    request_hash: String,
    command_id: sigil_kernel::UserInputCommandId,
    decision: sigil_kernel::UserInputDecisionV1,
) -> std::result::Result<Option<ActiveRun>, String>
where
    P: sigil_kernel::Provider + Send + Sync + 'static,
{
    let session = current_session
        .as_mut()
        .ok_or_else(|| "session state is unavailable for user input".to_owned())?;
    let continuation_requested = matches!(
        decision,
        sigil_kernel::UserInputDecisionV1::Submitted { .. }
    );
    let prepared_cancellation = if continuation_requested {
        Some(prepare_run_cancellation(session)?)
    } else {
        None
    };
    let _receipt = sigil_kernel::accept_user_input_decision(
        session,
        sigil_kernel::UserInputDecisionCommandV1 {
            identity: identity.clone(),
            request_hash: request_hash.clone(),
            command_id,
            decision,
        },
        current_unix_time_ms(),
    )
    .map_err(|error| format!("user input decision failed: {error:#}"))?;
    if !continuation_requested {
        let projection = session
            .user_input_projection()
            .map_err(|error| format!("user input projection failed: {error:#}"))?;
        let request = projection
            .request(&identity)
            .map(sigil_kernel::UserInputRequestStateV1::public_view)
            .ok_or_else(|| "resolved user input request is missing".to_owned())?;
        let _ = message_tx.send(WorkerMessage::UserInputDecisionApplied {
            request,
            continuation_started: false,
            entries: session.entries().to_vec(),
        });
        return Ok(None);
    }
    let run_id = *next_run_id;
    *next_run_id = (*next_run_id).saturating_add(1);
    let physical_attempt_id = sigil_kernel::new_provider_physical_attempt_id();
    let preparation = sigil_kernel::prepare_user_input_continuation(
        session,
        &identity,
        &request_hash,
        "sigil-tui",
        &physical_attempt_id,
        current_unix_time_ms(),
    )
    .map_err(|error| format!("user input continuation failed to start: {error:#}"))?;
    if preparation.already_started {
        return Err("user input continuation is already owned by another physical run".to_owned());
    }
    let continuation_logical_run_id = preparation.continuation.continuation_logical_run_id.clone();
    let source_thread_id = identity.source_thread_id.clone();
    let root_logical_run_id = identity.root_logical_run_id.as_str().to_owned();
    let request = preparation.request;
    let entries = session.entries().to_vec();
    let _ = message_tx.send(WorkerMessage::UserInputDecisionApplied {
        request,
        continuation_started: true,
        entries,
    });
    let run_session = current_session
        .take()
        .ok_or_else(|| "session state disappeared before user input continuation".to_owned())?;
    let (cancellation_owner, cancellation_recorder, cancellation_handle, cancellation_task_guard) =
        prepared_cancellation.expect("submitted input prepares cancellation");
    let mut handler = ChannelEventHandler::new(message_tx.clone());
    let (approval_tx, approval_rx) = mpsc::channel();
    let elicitation_audit_buffer: McpElicitationAuditBuffer =
        Arc::new(std::sync::Mutex::new(Vec::new()));
    elicitation_handler.set_audit_buffer(Some(Arc::clone(&elicitation_audit_buffer)));
    let run_elicitation_audit_buffer = Arc::clone(&elicitation_audit_buffer);
    let mut agent_delegate = sigil_runtime::AgentToolRuntime::new(
        agent_supervisor.clone(),
        effective_orchestration_root_config(root_config, &run_session),
        base_registry.clone(),
    )
    .with_background_runs(background_runs.clone());
    let options = options.clone();
    let task_result_tx = task_result_tx.clone();
    let url_capability_registrar = run_session.user_url_capability_registrar();
    let image_attachment_resolver = run_session.image_attachment_resolver();
    let handle = runtime.spawn(async move {
        let _cancellation_task_guard = cancellation_task_guard;
        let mut run_session = run_session;
        let mut approval_handler = ChannelApprovalHandler::new(approval_rx);
        let output = agent
            .run_with_approval_input_and_agent_delegate(
                &mut run_session,
                AgentRunInput::without_persisted_user_message(Vec::new())
                    .with_logical_run_id(continuation_logical_run_id.as_str())
                    .with_user_input_continuation_context(root_logical_run_id, source_thread_id)
                    .with_initial_provider_physical_attempt_id(physical_attempt_id)
                    .with_tool_artifact_read_budget(tool_artifact_read_budget)
                    .with_cancellation(cancellation_handle),
                options,
                &mut handler,
                &mut approval_handler,
                &mut agent_delegate,
            )
            .await;
        let (payload, resolution) = match output {
            Ok(output) => match output.disposition {
                AgentRunDisposition::FinalAnswer => (
                    RunTaskPayload::Chat {
                        result: Ok(output.result),
                        plan_mode: false,
                        plan_review: false,
                        queue_id: None,
                        provider_logical_run_id: None,
                        agent_result_continuation_thread_ids: Vec::new(),
                    },
                    sigil_kernel::UserInputResolutionV1::Consumed,
                ),
                AgentRunDisposition::AwaitingUserInput(request) => (
                    RunTaskPayload::AwaitingUserInput { request },
                    sigil_kernel::UserInputResolutionV1::Consumed,
                ),
                _ => (
                    RunTaskPayload::Chat {
                        result: Err(
                            "user input continuation ended with an unsupported disposition"
                                .to_owned(),
                        ),
                        plan_mode: false,
                        plan_review: false,
                        queue_id: None,
                        provider_logical_run_id: None,
                        agent_result_continuation_thread_ids: Vec::new(),
                    },
                    sigil_kernel::UserInputResolutionV1::Failed {
                        failure_class: "unsupported_disposition".to_owned(),
                        retryable: true,
                    },
                ),
            },
            Err(error) => (
                RunTaskPayload::Chat {
                    result: Err(format!("{error:#}")),
                    plan_mode: false,
                    plan_review: false,
                    queue_id: None,
                    provider_logical_run_id: None,
                    agent_result_continuation_thread_ids: Vec::new(),
                },
                sigil_kernel::UserInputResolutionV1::Failed {
                    failure_class: "continuation_failed".to_owned(),
                    retryable: true,
                },
            ),
        };
        let settlement = if matches!(
            resolution,
            sigil_kernel::UserInputResolutionV1::Failed { .. }
        ) {
            sigil_kernel::reconcile_user_input_continuation_after_failed_run(
                &mut run_session,
                &identity,
                &request_hash,
                current_unix_time_ms(),
            )
            .map(|_| ())
        } else {
            run_session.append_user_input_lifecycle(vec![
                sigil_kernel::UserInputLifecycleEntryV1::Resolved(
                    sigil_kernel::UserInputResolvedV1 {
                        schema_version: sigil_kernel::USER_INPUT_SCHEMA_VERSION,
                        identity,
                        request_hash,
                        resolution,
                        resolved_at_unix_ms: current_unix_time_ms(),
                    },
                ),
            ])
        };
        let payload = match settlement {
            Ok(()) => payload,
            Err(error) => RunTaskPayload::Chat {
                result: Err(format!(
                    "user input continuation settlement failed: {error:#}"
                )),
                plan_mode: false,
                plan_review: false,
                queue_id: None,
                provider_logical_run_id: None,
                agent_result_continuation_thread_ids: Vec::new(),
            },
        };
        let payload =
            match append_mcp_elicitation_audits(&mut run_session, &run_elicitation_audit_buffer) {
                Ok(()) => payload,
                Err(error) => RunTaskPayload::Chat {
                    result: Err(error),
                    plan_mode: false,
                    plan_review: false,
                    queue_id: None,
                    provider_logical_run_id: None,
                    agent_result_continuation_thread_ids: Vec::new(),
                },
            };
        let _ = task_result_tx.send(RunTaskResult {
            run_id,
            session: run_session,
            payload,
            post_run_maintenance: None,
        });
    });
    Ok(Some(ActiveRun {
        run_id,
        handle,
        approval_tx,
        elicitation_audit_buffer,
        cancellation_owner,
        cancellation_recorder,
        cancellation_target: RunCancellationTarget::Run,
        url_capability_registrar,
        image_attachment_resolver,
    }))
}

pub(in crate::runner) enum PlanReviewExecutionResult {
    Finished(sigil_kernel::AgentRunResult),
    AwaitingUserInput(sigil_kernel::UserInputRequestRefV1),
    Blocked { reason: String, paused: bool },
}

#[allow(clippy::too_many_arguments)]
pub(in crate::runner) async fn run_automatic_plan_review<H, A>(
    run_session: &mut Session,
    action: sigil_kernel::StartPlanReviewAction,
    agent: &sigil_kernel::Agent<impl sigil_kernel::Provider>,
    root_config: &RootConfig,
    options: AgentRunOptions,
    tool_registry: sigil_kernel::ToolRegistry,
    workspace_snapshot_id: Option<String>,
    handler: &mut H,
    approval_handler: &mut A,
    cancellation_handle: sigil_kernel::RunCancellationHandle,
    managed_plan_review_child_resources: Option<
        Arc<dyn sigil_runtime::plan_review_coordinator::PlanReviewChildResourceProvisionerV1>,
    >,
) -> std::result::Result<PlanReviewExecutionResult, String>
where
    H: sigil_kernel::EventHandler + Send,
    A: sigil_kernel::ApprovalHandler + Send,
{
    let request = sigil_runtime::PlanReviewCoordinator::prepare_automatic_plan_review(
        run_session,
        &action,
        workspace_snapshot_id,
        current_unix_time_ms(),
    )
    .map_err(|error| format!("failed to prepare plan review: {error:#}"))?;
    run_prepared_plan_review(
        run_session,
        &request,
        agent,
        root_config,
        options,
        tool_registry,
        handler,
        approval_handler,
        cancellation_handle,
        managed_plan_review_child_resources,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
pub(in crate::runner) async fn run_explicit_plan_review<H, A>(
    run_session: &mut Session,
    prompt: &str,
    root_logical_run_id: &str,
    workspace_snapshot_id: Option<String>,
    agent: &sigil_kernel::Agent<impl sigil_kernel::Provider>,
    root_config: &RootConfig,
    options: AgentRunOptions,
    tool_registry: sigil_kernel::ToolRegistry,
    handler: &mut H,
    approval_handler: &mut A,
    cancellation_handle: sigil_kernel::RunCancellationHandle,
    managed_plan_review_child_resources: Option<
        Arc<dyn sigil_runtime::plan_review_coordinator::PlanReviewChildResourceProvisionerV1>,
    >,
) -> std::result::Result<PlanReviewExecutionResult, String>
where
    H: sigil_kernel::EventHandler + Send,
    A: sigil_kernel::ApprovalHandler + Send,
{
    let request = sigil_runtime::PlanReviewCoordinator::prepare_explicit_plan_review(
        run_session,
        prompt,
        root_logical_run_id,
        workspace_snapshot_id,
        current_unix_time_ms(),
    )
    .map_err(|error| format!("failed to prepare explicit plan review: {error:#}"))?;
    run_prepared_plan_review(
        run_session,
        &request,
        agent,
        root_config,
        options,
        tool_registry,
        handler,
        approval_handler,
        cancellation_handle,
        managed_plan_review_child_resources,
    )
    .await
}

/// Runs an already-prepared plan review (automatic decision or revision) and commits the typed
/// draft to the parent session.
pub(in crate::runner) async fn run_prepared_plan_review<H, A>(
    run_session: &mut Session,
    request: &sigil_runtime::PlanReviewRunRequest,
    agent: &sigil_kernel::Agent<impl sigil_kernel::Provider>,
    root_config: &RootConfig,
    options: AgentRunOptions,
    tool_registry: sigil_kernel::ToolRegistry,
    handler: &mut H,
    approval_handler: &mut A,
    cancellation_handle: sigil_kernel::RunCancellationHandle,
    managed_plan_review_child_resources: Option<
        Arc<dyn sigil_runtime::plan_review_coordinator::PlanReviewChildResourceProvisionerV1>,
    >,
) -> std::result::Result<PlanReviewExecutionResult, String>
where
    H: sigil_kernel::EventHandler + Send,
    A: sigil_kernel::ApprovalHandler + Send,
{
    sigil_runtime::PlanReviewCoordinator::ensure_attempt_started(
        run_session,
        request,
        current_unix_time_ms(),
    )
    .map_err(|error| format!("failed to start plan review attempt: {error:#}"))?;
    let plan_review_workspace_root = options.workspace_root.clone();
    let child_resource_provisioner = managed_plan_review_child_resources;
    let outcome_result = match child_resource_provisioner {
        Some(provisioner) => {
            sigil_runtime::PlanReviewCoordinator::run_plan_review_with_resource_provisioner(
                run_session,
                request,
                agent,
                options,
                tool_registry,
                handler,
                approval_handler,
                cancellation_handle,
                provisioner,
            )
            .await
        }
        None => {
            #[cfg(test)]
            {
                sigil_runtime::PlanReviewCoordinator::run_plan_review(
                    run_session,
                    request,
                    agent,
                    options,
                    tool_registry,
                    handler,
                    approval_handler,
                    cancellation_handle,
                )
                .await
            }
            #[cfg(not(test))]
            {
                Err(anyhow::anyhow!(
                    "TUI plan review requires the composed child resource bundle"
                ))
            }
        }
    };
    let outcome = match outcome_result {
        Ok(outcome) => outcome,
        Err(error) => {
            let close = sigil_runtime::PlanReviewCoordinator::close_plan_review_run_if_open(
                run_session,
                request,
                &sigil_runtime::PlanReviewRunOutcome::Failed(
                    "plan review run failed before an outcome".to_owned(),
                ),
                current_unix_time_ms(),
            );
            return Err(match close {
                Ok(()) => format!("plan review run failed: {error:#}"),
                Err(close_error) => format!(
                    "plan review run failed ({error:#}) and its terminal closure also failed ({close_error:#})"
                ),
            });
        }
    };
    match outcome {
        sigil_runtime::PlanReviewRunOutcome::AwaitingUserInput { request: pending } => {
            sigil_runtime::PlanReviewCoordinator::close_plan_review_run(
                run_session,
                request,
                &sigil_runtime::PlanReviewRunOutcome::AwaitingUserInput {
                    request: pending.clone(),
                },
                current_unix_time_ms(),
            )
            .map_err(|error| format!("failed to suspend plan review: {error:#}"))?;
            Ok(PlanReviewExecutionResult::AwaitingUserInput(
                sigil_kernel::UserInputRequestRefV1 {
                    identity: pending.identity.clone(),
                    request_hash: pending.request_hash.clone(),
                },
            ))
        }
        sigil_runtime::PlanReviewRunOutcome::DraftReady { draft } => {
            let compile_input = sigil_runtime::PlanReviewCoordinator::plan_compile_input(
                run_session,
                root_config,
                &plan_review_workspace_root,
                request,
            )
            .map_err(|error| format!("failed to prepare plan compile input: {error:#}"))?;
            sigil_runtime::PlanReviewCoordinator::commit_draft_from_child(
                run_session,
                &draft,
                request,
                &compile_input,
                current_unix_time_ms(),
            )
            .map_err(|error| format!("failed to commit plan review draft: {error:#}"))?;
            Ok(PlanReviewExecutionResult::Finished(
                sigil_kernel::AgentRunResult {
                    final_text: format!("Plan ready: {}", draft.summary),
                    tool_calls: 0,
                    final_message_id: None,
                },
            ))
        }
        sigil_runtime::PlanReviewRunOutcome::CompletedWithoutDraft => {
            sigil_runtime::PlanReviewCoordinator::complete_without_draft(
                run_session,
                request,
                current_unix_time_ms(),
            )
            .map_err(|error| format!("failed to close plan review: {error:#}"))?;
            Ok(PlanReviewExecutionResult::Finished(
                sigil_kernel::AgentRunResult {
                    final_text: "Plan review closed without a draft; no task was created."
                        .to_owned(),
                    tool_calls: 0,
                    final_message_id: None,
                },
            ))
        }
        sigil_runtime::PlanReviewRunOutcome::Cancelled => {
            sigil_runtime::PlanReviewCoordinator::close_plan_review_run(
                run_session,
                request,
                &sigil_runtime::PlanReviewRunOutcome::Cancelled,
                current_unix_time_ms(),
            )
            .map_err(|close_error| {
                format!(
                    "plan review cancelled and its terminal closure also failed: {close_error:#}"
                )
            })?;
            Err("plan review was cancelled before a draft".to_owned())
        }
        sigil_runtime::PlanReviewRunOutcome::Interrupted(error) => {
            sigil_runtime::PlanReviewCoordinator::close_plan_review_run(
                run_session,
                request,
                &sigil_runtime::PlanReviewRunOutcome::Interrupted(error.clone()),
                current_unix_time_ms(),
            )
            .map_err(|close_error| {
                format!("plan review interrupted ({error}) and its terminal closure also failed: {close_error:#}")
            })?;
            Err(error)
        }
        sigil_runtime::PlanReviewRunOutcome::Blocked(reason) => {
            sigil_runtime::PlanReviewCoordinator::close_plan_review_run(
                run_session,
                request,
                &sigil_runtime::PlanReviewRunOutcome::Blocked(reason.clone()),
                current_unix_time_ms(),
            )
            .map_err(|close_error| format!(
                "plan review blocked ({reason}) and its terminal closure also failed: {close_error:#}"
            ))?;
            Ok(PlanReviewExecutionResult::Blocked {
                reason,
                paused: false,
            })
        }
        sigil_runtime::PlanReviewRunOutcome::Paused(reason) => {
            sigil_runtime::PlanReviewCoordinator::close_plan_review_run(
                run_session,
                request,
                &sigil_runtime::PlanReviewRunOutcome::Paused(reason.clone()),
                current_unix_time_ms(),
            )
            .map_err(|close_error| format!(
                "plan review paused ({reason}) and its terminal closure also failed: {close_error:#}"
            ))?;
            Ok(PlanReviewExecutionResult::Blocked {
                reason,
                paused: true,
            })
        }
        sigil_runtime::PlanReviewRunOutcome::Failed(error) => {
            sigil_runtime::PlanReviewCoordinator::close_plan_review_run(
                run_session,
                request,
                &sigil_runtime::PlanReviewRunOutcome::Failed(error.clone()),
                current_unix_time_ms(),
            )
            .map_err(|close_error| {
                format!("plan review failed ({error}) and its terminal closure also failed: {close_error:#}")
            })?;
            Err(error)
        }
        sigil_runtime::PlanReviewRunOutcome::SubmitOnlyProtocolViolation(error) => {
            sigil_runtime::PlanReviewCoordinator::close_plan_review_run(
                run_session,
                request,
                &sigil_runtime::PlanReviewRunOutcome::SubmitOnlyProtocolViolation(error.clone()),
                current_unix_time_ms(),
            )
            .map_err(|close_error| {
                format!("plan review violated submit-only finalization ({error}) and its terminal closure also failed: {close_error:#}")
            })?;
            Err(error)
        }
    }
}

pub(in crate::runner) fn agent_result_continuation_run_result(
    output: sigil_kernel::AgentRunOutput,
) -> std::result::Result<sigil_kernel::AgentRunResult, String> {
    match output.disposition {
        AgentRunDisposition::FinalAnswer => Ok(output.result),
        AgentRunDisposition::AwaitingUserInput(_) => Err(
            "agent result continuation requested user input outside the foreground conversation"
                .to_owned(),
        ),
        AgentRunDisposition::Interrupted => {
            Err("agent result continuation was interrupted before a final answer".to_owned())
        }
        AgentRunDisposition::Blocked => {
            Err("agent result continuation was blocked before a final answer".to_owned())
        }
        AgentRunDisposition::StartDurableTask(_) => {
            Err("agent result continuation cannot hand off to a durable task".to_owned())
        }
        AgentRunDisposition::ContinueDurableTask(_) => {
            Err("agent result continuation cannot continue a durable task".to_owned())
        }
        AgentRunDisposition::RunPendingPlan(_) => {
            Err("agent result continuation cannot execute a pending plan".to_owned())
        }
        AgentRunDisposition::PendingPlanDecisionRequired(_) => {
            Err("agent result continuation cannot decide a pending plan".to_owned())
        }
        AgentRunDisposition::StartPlanReview(_) => {
            Err("agent result continuation cannot start a plan review".to_owned())
        }
        AgentRunDisposition::PlanReviewDraftSubmitted(_) => {
            Err("agent result continuation cannot submit a plan review draft".to_owned())
        }
        AgentRunDisposition::TaskPlanAccepted => {
            Err("agent result continuation cannot accept a task plan".to_owned())
        }
    }
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
    context_resolver: &sigil_runtime::RequestContextResolver,
    background_runs: &sigil_runtime::AgentToolBackgroundRuns,
    current_session: &mut Option<Session>,
    task_result_tx: &WorkerEventPayloadSender<RunTaskResult>,
    message_tx: &mpsc::Sender<WorkerMessage>,
    elicitation_handler: Arc<ChannelMcpElicitationHandler>,
    role_provider_builder: Arc<dyn TaskRoleProviderBuilder>,
    managed_verification_execution: Option<
        Arc<dyn sigil_kernel::verification::VerificationExecutionPortV1>,
    >,
    managed_plan_review_child_resources: Option<
        Arc<dyn sigil_runtime::plan_review_coordinator::PlanReviewChildResourceProvisionerV1>,
    >,
    session_log_path: &Path,
    next_run_id: &mut u64,
    _terminal_lifecycle_router: &ChannelTerminalLifecycleRouter,
    tool_artifact_read_budget: ToolArtifactReadBudgetV1,
    queued: PreparedQueuedConversationCandidate,
) -> Option<ActiveRun>
where
    P: sigil_kernel::Provider + Send + Sync + 'static,
{
    let PreparedQueuedConversationCandidate {
        promotion,
        frozen_request,
        reasoning_effort,
        background_ready_context,
        runtime_context,
        ..
    } = queued;
    let queue_id = promotion.queue_id.clone();
    let safe_prompt = promotion
        .durable_user_message
        .content
        .clone()
        .unwrap_or_default();
    let dispatch_run_id = promotion.dispatch_run_id.clone();
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
        effective_orchestration_root_config(root_config, &run_session),
        base_registry.clone(),
    )
    .with_background_runs(background_runs.clone());
    let task_result_tx = task_result_tx.clone();
    let conversation_coordinator =
        ConversationCoordinator::new(root_config.task.enabled, root_config.task.routing_policy)
            .with_writable_memory_routing(root_config.memory.writable)
            .with_orchestration_route_guard(sigil_runtime::OrchestrationRouteGuard::new(
                &root_config.agent.runtime_provider,
                &root_config.agent.model,
                sigil_runtime::ORCHESTRATION_RUNTIME_BUILD_ID,
            ))
            .with_route_capability_evidence(sigil_runtime::RouteCapabilityEvidence {
                provider_supports_routing_tools: agent.provider_capabilities().supports_tool_stream,
                route_qualified: sigil_runtime::route_qualification_evidence(root_config),
            });
    let task_root_config = root_config.clone();
    let plan_review_root_config = root_config.clone();
    let task_base_registry = base_registry.clone();
    let task_agent_supervisor = agent_supervisor.clone();
    let run_id = *next_run_id;
    *next_run_id = (*next_run_id).saturating_add(1);
    let url_capability_registrar = run_session.user_url_capability_registrar();
    let image_attachment_resolver = run_session.image_attachment_resolver();
    let pending_input_provider = DurableQueuePendingInputProvider::new(context_resolver.clone());
    let session_log_path = session_log_path.to_path_buf();
    let handle = runtime.spawn(async move {
        let _cancellation_task_guard = cancellation_task_guard;
        let mut run_session = run_session;
        let mut payload = {
            let mut approval_handler = ChannelApprovalHandler::new(approval_rx);
            let input = AgentRunInput::without_persisted_user_message(background_ready_context)
                .with_runtime_context(runtime_context)
                .with_initial_frozen_provider_request(frozen_request)
                .with_pending_input_provider(Arc::new(pending_input_provider))
                .with_tool_artifact_read_budget(tool_artifact_read_budget.clone());
            let input = conversation_coordinator
                .enforce_orchestration_route_kill_switch(&mut run_session, current_unix_time_ms())
                .and_then(|_| {
                    conversation_coordinator.bind_conversation_input(
                        &run_session,
                        input,
                        parent_session_ref.clone(),
                        dispatch_run_id.clone(),
                        Some(ConversationSourceTurn {
                            message_id: promotion.durable_user_message.id.clone(),
                            objective: safe_prompt.clone(),
                        }),
                        current_unix_time_ms(),
                    )
                })
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
                        plan_review: false,
                        queue_id: Some(queue_id.clone()),
                        provider_logical_run_id: None,
                        agent_result_continuation_thread_ids: Vec::new(),
                    },
                    AgentRunDisposition::AwaitingUserInput(request) => {
                        RunTaskPayload::AwaitingUserInput { request }
                    }
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
                                        managed_verification_execution:
                                            managed_verification_execution.clone(),
                                        handler: &mut handler,
                                        cancellation_handle,
                                        tool_artifact_read_budget,
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
                                    plan_review: false,
                                    queue_id: Some(queue_id.clone()),
                                    provider_logical_run_id: None,
                                    agent_result_continuation_thread_ids: Vec::new(),
                                }
                            }
                        }
                    }
                    AgentRunDisposition::ContinueDurableTask(action) => {
                        let task_id = action.task_id.as_str().to_owned();
                        let task =
                            sigil_runtime::validate_task_continuation_action(&run_session, &action)
                                .map_err(|error| {
                                    format!("typed task continuation is stale: {error}")
                                });
                        match task {
                            Ok(task) => {
                                let _ = run_message_tx.send(WorkerMessage::TaskRunStarted {
                                    task_id: task_id.clone(),
                                    objective: task.objective.clone(),
                                });
                                let result = continue_routed_task_to_root_terminal(
                                    &mut run_session,
                                    RoutedTaskContinuationOrchestration {
                                        task_id: action.task_id,
                                        parent_session_ref: task.parent_session_ref,
                                        objective: task.objective,
                                        guidance: action.guidance,
                                        guidance_receipt: action.guidance_receipt,
                                        root_config: task_root_config,
                                        options,
                                        base_registry: task_base_registry,
                                        agent_supervisor: task_agent_supervisor,
                                        role_provider_builder: role_provider_builder.as_ref(),
                                        managed_verification_execution:
                                            managed_verification_execution.clone(),
                                        handler: &mut handler,
                                        cancellation_handle,
                                        tool_artifact_read_budget,
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
                            Err(error) => {
                                let error = if cancellation_handle.try_finalize_naturally() {
                                    error
                                } else {
                                    "run cancellation won the stale-task terminal-state race"
                                        .to_owned()
                                };
                                RunTaskPayload::Chat {
                                    result: Err(error),
                                    plan_mode: false,
                                    plan_review: false,
                                    queue_id: Some(queue_id.clone()),
                                    provider_logical_run_id: None,
                                    agent_result_continuation_thread_ids: Vec::new(),
                                }
                            }
                        }
                    }
                    AgentRunDisposition::RunPendingPlan(action) => {
                        let adopted = adopt_plan_run(
                            &task_root_config,
                            &options.workspace_root,
                            &session_log_path,
                            &mut run_session,
                            action.plan_id.as_str().to_owned(),
                            action.plan_hash,
                            sigil_kernel::PlanTaskStartMode::CreateAndRun,
                            None,
                            sigil_kernel::PlanRunCommandSource::ModelTypedRoute,
                            Some(task_base_registry.contracts()),
                        )
                        .map_err(|error| format!("failed to execute the selected pending plan: {error:#}"));
                        match adopted {
                            Ok(adopted) => {
                                let _ = run_message_tx.send(WorkerMessage::TaskCreatedFromPlan {
                                    entry: adopted.entry.clone(),
                                    start_mode: sigil_kernel::PlanTaskStartMode::CreateAndRun,
                                    entries: adopted.entries.clone(),
                                });
                                let task_id = adopted.receipt.task_id.as_str().to_owned();
                                let task = run_session
                                    .task_state_projection()
                                    .tasks
                                    .get(&adopted.receipt.task_id)
                                    .cloned();
                                match (task, adopted.admission) {
                                    (Some(_task), sigil_kernel::TaskAdmissionOutcomeV1::Blocked(blocker)) => {
                                        let _ = run_message_tx.send(
                                            WorkerMessage::TaskAdmissionBlocked {
                                                task_id: task_id.clone(),
                                                blocker,
                                                entries: adopted.entries.clone(),
                                            },
                                        );
                                        RunTaskPayload::Chat {
                                            result: Ok(sigil_kernel::AgentRunResult {
                                                final_text: format!(
                                                    "Task {task_id} is blocked until the environment is resolved."
                                                ),
                                                tool_calls: 0,
                                                final_message_id: None,
                                            }),
                                            plan_mode: false,
                                            plan_review: false,
                                            queue_id: Some(queue_id.clone()),
                                            provider_logical_run_id: None,
                                            agent_result_continuation_thread_ids: Vec::new(),
                                        }
                                    }
                                    (Some(task), _) => {
                                        let _ = run_message_tx.send(WorkerMessage::TaskRunStarted {
                                            task_id: task_id.clone(),
                                            objective: task.objective.clone(),
                                        });
                                        let result = run_admitted_task_to_root_terminal(
                                            &mut run_session,
                                            AdmittedTaskRunOrchestration {
                                                task_id: adopted.receipt.task_id,
                                                parent_session_ref: task.parent_session_ref,
                                                objective: task.objective,
                                                root_config: task_root_config,
                                                options,
                                                base_registry: task_base_registry,
                                                agent_supervisor: task_agent_supervisor,
                                                role_provider_builder:
                                                    role_provider_builder.as_ref(),
                                                managed_verification_execution:
                                                    managed_verification_execution.clone(),
                                                handler: &mut handler,
                                                cancellation_handle,
                                                tool_artifact_read_budget,
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
                                    (None, _) => RunTaskPayload::Chat {
                                        result: Err(format!(
                                            "plan adoption created task {task_id} without durable task state"
                                        )),
                                        plan_mode: false,
                                        plan_review: false,
                                        queue_id: Some(queue_id.clone()),
                                        provider_logical_run_id: None,
                                        agent_result_continuation_thread_ids: Vec::new(),
                                    },
                                }
                            }
                            Err(error) => RunTaskPayload::Chat {
                                result: Err(error),
                                plan_mode: false,
                                plan_review: false,
                                queue_id: Some(queue_id.clone()),
                                provider_logical_run_id: None,
                                agent_result_continuation_thread_ids: Vec::new(),
                            },
                        }
                    }
                    AgentRunDisposition::PendingPlanDecisionRequired(_action) => {
                        RunTaskPayload::Chat {
                            result: Ok(sigil_kernel::AgentRunResult {
                                final_text: "The current plan is still awaiting a decision. Choose Run, Revise, Save, or Reject before continuing.".to_owned(),
                                tool_calls: output.result.tool_calls,
                                final_message_id: output.result.final_message_id,
                            }),
                            plan_mode: false,
                            plan_review: false,
                            queue_id: Some(queue_id.clone()),
                            provider_logical_run_id: None,
                            agent_result_continuation_thread_ids: Vec::new(),
                        }
                    }
                    AgentRunDisposition::Interrupted => RunTaskPayload::Chat {
                        result: Err("run was interrupted before a final answer".to_owned()),
                        plan_mode: false,
                        plan_review: false,
                        queue_id: Some(queue_id.clone()),
                        provider_logical_run_id: None,
                        agent_result_continuation_thread_ids: Vec::new(),
                    },
                    AgentRunDisposition::Blocked => RunTaskPayload::Chat {
                        result: Err("run was blocked before a final answer".to_owned()),
                        plan_mode: false,
                        plan_review: false,
                        queue_id: Some(queue_id.clone()),
                        provider_logical_run_id: None,
                        agent_result_continuation_thread_ids: Vec::new(),
                    },
                    AgentRunDisposition::TaskPlanAccepted => RunTaskPayload::Chat {
                        result: Err("task planning completed outside a task run".to_owned()),
                        plan_mode: false,
                        plan_review: false,
                        queue_id: Some(queue_id.clone()),
                        provider_logical_run_id: None,
                        agent_result_continuation_thread_ids: Vec::new(),
                    },
                    AgentRunDisposition::StartPlanReview(action) => {
                        let _ = run_message_tx.send(WorkerMessage::PlanRunStarted {
                            prompt: format!("plan review {}", action.plan_review_id.as_str()),
                        });
                        let plan_registry = sigil_runtime::build_plan_review_tool_registry(
                            agent.tool_registry(),
                            &plan_review_root_config,
                        )
                        .into_registry();
                        let result = run_automatic_plan_review(
                            &mut run_session,
                            action,
                            agent.as_ref(),
                            &plan_review_root_config,
                            options.clone(),
                                    plan_registry,
                            sigil_runtime::plan_handoff_workspace_snapshot_id(
                                &plan_review_root_config,
                                &options.workspace_root,
                            )
                            .ok()
                            .flatten(),
                                        &mut handler,
                                        &mut approval_handler,
                                        cancellation_handle.clone(),
                        managed_plan_review_child_resources.clone(),
                                    )
                        .await;
                        match result {
                            Ok(PlanReviewExecutionResult::Finished(result)) => {
                                RunTaskPayload::Chat {
                                    result: Ok(result),
                                    plan_mode: false,
                                    plan_review: true,
                                    queue_id: Some(queue_id.clone()),
                                    provider_logical_run_id: None,
                                    agent_result_continuation_thread_ids: Vec::new(),
                                }
                            }
                            Ok(PlanReviewExecutionResult::AwaitingUserInput(request)) => {
                                RunTaskPayload::AwaitingUserInput { request }
                            }
                            Ok(PlanReviewExecutionResult::Blocked { reason, paused }) => {
                                RunTaskPayload::PlanReviewBlocked { reason, paused }
                            }
                            Err(error) => RunTaskPayload::Chat {
                                result: Err(error),
                                plan_mode: false,
                                plan_review: true,
                                queue_id: Some(queue_id.clone()),
                                provider_logical_run_id: None,
                                agent_result_continuation_thread_ids: Vec::new(),
                            },
                        }
                    }
                    AgentRunDisposition::PlanReviewDraftSubmitted(_) => RunTaskPayload::Chat {
                        result: Err(
                            "plan review draft submitted outside a plan review run".to_owned()
                        ),
                        plan_mode: false,
                        plan_review: false,
                        queue_id: Some(queue_id.clone()),
                        provider_logical_run_id: None,
                        agent_result_continuation_thread_ids: Vec::new(),
                    },
                },
                Err(error) => RunTaskPayload::Chat {
                    result: Err(error),
                    plan_mode: false,
                    plan_review: false,
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
                RunTaskPayload::AwaitingUserInput { .. } => RunTaskPayload::Chat {
                    result: Err(error),
                    plan_mode: false,
                    plan_review: false,
                    queue_id: Some(queue_id.clone()),
                    provider_logical_run_id: None,
                    agent_result_continuation_thread_ids: Vec::new(),
                },
                RunTaskPayload::Chat {
                    plan_mode,
                    plan_review,
                    queue_id,
                    provider_logical_run_id,
                    agent_result_continuation_thread_ids,
                    ..
                } => RunTaskPayload::Chat {
                    result: Err(error),
                    plan_mode,
                    plan_review,
                    queue_id,
                    provider_logical_run_id,
                    agent_result_continuation_thread_ids,
                },
                RunTaskPayload::PlanReviewBlocked { .. } => RunTaskPayload::Chat {
                    result: Err(error),
                    plan_mode: false,
                    plan_review: true,
                    queue_id: Some(queue_id.clone()),
                    provider_logical_run_id: None,
                    agent_result_continuation_thread_ids: Vec::new(),
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
            post_run_maintenance: None,
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
    task_result_tx: &WorkerEventPayloadSender<RunTaskResult>,
    message_tx: &mpsc::Sender<WorkerMessage>,
    elicitation_handler: Arc<ChannelMcpElicitationHandler>,
    next_run_id: &mut u64,
    _terminal_lifecycle_router: &ChannelTerminalLifecycleRouter,
    frozen_request: sigil_kernel::FrozenProviderRequestMaterial,
    logical_run_id: String,
    tool_artifact_read_budget: ToolArtifactReadBudgetV1,
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
        effective_orchestration_root_config(root_config, &run_session),
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
                .with_tool_artifact_read_budget(tool_artifact_read_budget)
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
                plan_review: false,
                queue_id: None,
                provider_logical_run_id: None,
                agent_result_continuation_thread_ids: Vec::new(),
            },
            post_run_maintenance: None,
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
    input
        .with_runtime_context(runtime_context)
        .with_pending_input_provider(Arc::new(DurableQueuePendingInputProvider::new(
            context_resolver.clone(),
        )))
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
    let child = current_subagent_child(task).or_else(|| {
        (task.active_steps.len() <= 1)
            .then(|| latest_subagent_child_from_entries(session, task))
            .flatten()
    })?;
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
        &root_config.agent.runtime_provider,
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
        effective_orchestration_root_config(root_config, session),
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
        effective_orchestration_root_config(root_config, session),
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

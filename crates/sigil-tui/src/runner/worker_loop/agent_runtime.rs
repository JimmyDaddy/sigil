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
    agent_supervisor.reset_turn_budget();
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

    let handle = runtime.spawn(async move {
        let mut run_session = run_session;
        let result = {
            let mut approval_handler = ChannelApprovalHandler::new(approval_rx);
            agent
                .run_with_approval_input_and_agent_delegate(
                    &mut run_session,
                    AgentRunInput::without_persisted_user_message(vec![ModelMessage::user(
                        continuation_prompt,
                    )]),
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
                agent_result_continuation_thread_ids: completed_thread_ids,
            },
        });
    });

    Some(ActiveRun {
        run_id,
        handle,
        approval_tx,
        elicitation_audit_buffer,
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
    next_run_id: &mut u64,
    queued: ConversationInputQueuedEntry,
) -> Option<ActiveRun>
where
    P: sigil_kernel::Provider + Send + Sync + 'static,
{
    if queued.target != ConversationInputTarget::MainThread {
        append_queue_status_and_notify(
            current_session,
            message_tx,
            queued.queue_id,
            ConversationInputStatus::Rejected,
            Some("queued target is not dispatchable by the main conversation worker".to_owned()),
        );
        return None;
    }
    let plan_mode = match queued.kind {
        ConversationInputKind::Chat => false,
        ConversationInputKind::PlanPrompt => true,
        _ => {
            append_queue_status_and_notify(
                current_session,
                message_tx,
                queued.queue_id,
                ConversationInputStatus::Rejected,
                Some(
                    "queued input kind is not supported by the main conversation worker".to_owned(),
                ),
            );
            return None;
        }
    };

    let Some(run_session) = current_session.take() else {
        let _ = message_tx.send(WorkerMessage::RunFailed(
            "session state is unavailable for queued input".to_owned(),
        ));
        return None;
    };

    let ConversationInputQueuedEntry {
        queue_id,
        prompt,
        reasoning_effort,
        ..
    } = queued;
    let _ = message_tx.send(WorkerMessage::ConversationQueueDispatchStarted {
        queue_id: queue_id.clone(),
        prompt: prompt.clone(),
    });
    let background_ready_context = queued_background_ready_transient_context(Some(&run_session));

    let mut handler = ChannelEventHandler::new(message_tx.clone());
    let (approval_tx, approval_rx) = mpsc::channel();
    let elicitation_audit_buffer: McpElicitationAuditBuffer =
        Arc::new(std::sync::Mutex::new(Vec::new()));
    elicitation_handler.set_audit_buffer(Some(Arc::clone(&elicitation_audit_buffer)));
    let run_elicitation_audit_buffer = Arc::clone(&elicitation_audit_buffer);
    let mut options = options.clone();
    if let Some(reasoning_effort) = reasoning_effort {
        options.reasoning_effort = Some(reasoning_effort);
    }
    let delegation_requirement = agent_delegation_requirement_for_prompt(&prompt);
    agent_supervisor.reset_turn_budget();
    let mut agent_delegate = sigil_runtime::AgentToolRuntime::new(
        agent_supervisor.clone(),
        root_config.clone(),
        base_registry.clone(),
    )
    .with_background_runs(background_runs.clone());
    let plan_tools = plan_mode.then(|| {
        sigil_runtime::build_plan_prompt_tool_registry(base_registry, root_config).into_registry()
    });
    let task_result_tx = task_result_tx.clone();
    let run_id = *next_run_id;
    *next_run_id = (*next_run_id).saturating_add(1);

    let handle = runtime.spawn(async move {
        let mut run_session = run_session;
        let result = {
            let mut approval_handler = ChannelApprovalHandler::new(approval_rx);
            let mut input = if plan_mode {
                let mut transient_context = plan_mode_transient_context(prompt);
                transient_context.extend(background_ready_context);
                AgentRunInput::without_persisted_user_message(transient_context)
            } else if background_ready_context.is_empty() {
                AgentRunInput::user(prompt)
            } else {
                AgentRunInput::transient(prompt, background_ready_context)
            };
            if let Some(requirement) = delegation_requirement {
                input = input.with_agent_delegation_requirement(requirement);
            }
            if let Some(tools) = plan_tools {
                agent
                    .run_with_approval_input_tool_registry_and_agent_delegate(
                        &mut run_session,
                        input,
                        options,
                        tools,
                        &mut handler,
                        &mut approval_handler,
                        &mut agent_delegate,
                    )
                    .await
            } else {
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
            }
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
                plan_mode,
                queue_id: Some(queue_id),
                agent_result_continuation_thread_ids: Vec::new(),
            },
        });
    });

    Some(ActiveRun {
        run_id,
        handle,
        approval_tx,
        elicitation_audit_buffer,
    })
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
    let mut session = load_session(
        &root_config.agent.provider,
        &root_config.agent.model,
        current_session_log_path,
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

pub(in crate::runner) fn agent_delegation_requirement_for_prompt(
    prompt: &str,
) -> Option<AgentDelegationRequirement> {
    let normalized = prompt.to_lowercase().replace(
        ['\u{2010}', '\u{2011}', '\u{2012}', '\u{2013}', '\u{2014}'],
        "-",
    );
    let compact = normalized
        .chars()
        .filter(|character| !character.is_whitespace())
        .collect::<String>();
    let mentions_subagent = normalized.contains("subagent")
        || normalized.contains("sub-agent")
        || normalized.contains("sub agent")
        || compact.contains("子agent")
        || compact.contains("子代理");
    if !mentions_subagent {
        return None;
    }
    let compact_negations = [
        "不要用子agent",
        "不用子agent",
        "别用子agent",
        "不要使用子agent",
        "不使用子agent",
        "不需要子agent",
        "无需子agent",
        "别开子agent",
        "不要开子agent",
        "不要启动子agent",
    ];
    let normalized_negations = [
        "do not use subagent",
        "do not use sub-agent",
        "don't use subagent",
        "don't use sub-agent",
        "don't use a subagent",
        "don't use a sub-agent",
        "without subagent",
        "without sub-agent",
        "without a subagent",
        "without a sub-agent",
        "do not spawn subagent",
        "do not spawn a sub-agent",
    ];
    let negated = compact_negations
        .iter()
        .any(|phrase| compact.contains(phrase))
        || normalized_negations
            .iter()
            .any(|phrase| normalized.contains(phrase));
    if negated {
        return None;
    }
    let explicit = compact.contains("必须用子agent")
        || compact.contains("必须使用子agent")
        || compact.contains("同时用子agent")
        || compact.contains("让子agent")
        || compact.contains("用子agent")
        || compact.contains("使用子agent")
        || normalized.contains("must use subagent")
        || normalized.contains("must use sub-agent")
        || normalized.contains("use a subagent")
        || normalized.contains("use a sub-agent")
        || normalized.contains("use subagent")
        || normalized.contains("use sub-agent")
        || normalized.contains("delegate to a subagent")
        || normalized.contains("delegate to a sub-agent");
    explicit.then(|| {
        AgentDelegationRequirement::new(
            "the user explicitly requested sub-agent delegation in the current prompt",
        )
    })
}

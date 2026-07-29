use super::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::runner) enum SessionTransitionKind {
    Switch,
    StartNew,
    LocalFork,
    CheckpointFork,
}

impl SessionTransitionKind {
    pub(in crate::runner) fn block_reason(
        self,
        foreground_run_active: bool,
        background_run_active: bool,
    ) -> Option<&'static str> {
        if foreground_run_active {
            return Some(match self {
                Self::Switch => "cannot switch sessions while the agent is running",
                Self::StartNew => "cannot start a new session while the agent is running",
                Self::LocalFork => "cannot fork a local session while the agent is running",
                Self::CheckpointFork => "cannot fork conversation while the agent is running",
            });
        }
        background_run_active.then_some(match self {
            Self::Switch => "cannot switch sessions while a background agent is running",
            Self::StartNew => "cannot start a new session while a background agent is running",
            Self::LocalFork => "cannot fork a local session while a background agent is running",
            Self::CheckpointFork => "cannot fork conversation while a background agent is running",
        })
    }

    fn trust_reason(self) -> &'static str {
        match self {
            Self::Switch => "trusted workspace carried into session",
            Self::StartNew => "trusted workspace carried into new session",
            Self::LocalFork => "trusted workspace carried into local conversation fork",
            Self::CheckpointFork => "trusted workspace carried into conversation fork",
        }
    }
}

pub(in crate::runner) struct SessionTransitionOutcome {
    pub(in crate::runner) session_log_path: PathBuf,
    pub(in crate::runner) provider_name: String,
    pub(in crate::runner) model_name: String,
    pub(in crate::runner) entries: Vec<SessionLogEntry>,
}

pub(in crate::runner) fn ensure_session_transition_allowed(
    kind: SessionTransitionKind,
    state: &WorkerLoopState,
) -> std::result::Result<(), String> {
    kind.block_reason(
        state.run.active.is_some(),
        state.agent.background_runs.has_any(),
    )
    .map_or(Ok(()), |reason| Err(reason.to_owned()))
}

pub(in crate::runner) fn transition_session<P>(
    kind: SessionTransitionKind,
    session_log_path: PathBuf,
    runtime: &tokio::runtime::Runtime,
    root_config: &RootConfig,
    provider_capabilities: &ProviderCapabilities,
    workspace_root: &Path,
    agent: &Arc<Agent<P>>,
    state: &mut WorkerLoopState,
    message_tx: &mpsc::Sender<WorkerMessage>,
) -> std::result::Result<SessionTransitionOutcome, String>
where
    P: sigil_kernel::Provider,
{
    ensure_session_transition_allowed(kind, state)?;

    let mut session = load_routed_session_with_runtime_attachments(
        root_config,
        &session_log_path,
        state.session.current.as_ref(),
    )
    .map_err(|error| format!("{error:#}"))?;
    let cleanup_report = runtime
        .block_on(
            sigil_runtime::isolated_workspace::reconcile_isolated_workspace_cleanup(
                &mut session,
                workspace_root,
            ),
        )
        .map_err(|error| format!("failed to reconcile isolated task workspaces: {error:#}"))?;
    if cleanup_report.inspected > 0 {
        let _ = message_tx.send(WorkerMessage::Notice(format!(
            "reconciled {} isolated task workspace(s): {} removed, {} already missing, {} require review",
            cleanup_report.inspected,
            cleanup_report.removed,
            cleanup_report.already_missing,
            cleanup_report.failed
        )));
    }
    let promotion_report = runtime
        .block_on(
            sigil_runtime::integration_lanes::reconcile_integration_promotions(
                &mut session,
                workspace_root,
            ),
        )
        .map_err(|error| format!("failed to reconcile integration promotions: {error:#}"))?;
    if promotion_report.inspected > 0 {
        let _ = message_tx.send(WorkerMessage::Notice(format!(
            "reconciled {} interrupted integration promotion(s): {} promoted, {} cancelled, {} failed, {} require review",
            promotion_report.inspected,
            promotion_report.promoted,
            promotion_report.cancelled,
            promotion_report.failed,
            promotion_report.needs_review
        )));
    }
    let same_logical_session = state
        .session
        .current
        .as_ref()
        .is_some_and(|current| current.session_scope_id() == session.session_scope_id());
    let empty_exact_prompts = ExactConversationPromptStore::new();
    let target_exact_prompts = if same_logical_session {
        &state.session.exact_prompts
    } else {
        &empty_exact_prompts
    };
    mark_stale_dispatching_conversation_queue_items(&mut session, target_exact_prompts, message_tx);

    if state
        .session
        .current
        .as_ref()
        .is_some_and(|session| session_workspace_is_trusted(session, workspace_root))
    {
        ensure_session_workspace_trust(&mut session, workspace_root, kind.trust_reason())?;
    }

    let parent_session_ref = session_ref_for_log_path(&session_log_path)?;
    let pending_task_handoffs =
        ConversationCoordinator::new(root_config.task.enabled, root_config.task.routing_policy)
            .reconcile(&mut session, &parent_session_ref, current_unix_time_ms())
            .map_err(|error| format!("failed to reconcile durable task handoffs: {error:#}"))?;
    let effective_root_config =
        super::agent_runtime::effective_orchestration_root_config(root_config, &session);

    let target_agent_registry =
        sigil_runtime::AgentProfileRegistry::from_root_config_with_workspace_and_entries(
            &effective_root_config,
            workspace_root,
            session.entries(),
        )
        .map_err(|error| {
            format!("failed to rebuild agent profiles for target session: {error:#}")
        })?;
    let target_agent_budget =
        sigil_runtime::AgentBudgetPolicy::from_root_config(&effective_root_config);
    let target_agent_supervisor = sigil_runtime::AgentSupervisor::new(
        target_agent_registry.clone(),
        target_agent_budget.clone(),
        provider_capabilities.clone(),
    )
    .with_event_sink(Arc::new(WorkerSupervisorEventSink {
        wake_coalescer: state.wake_coalescer.clone(),
    }));
    let mut target_tool_registry = agent.tool_registry().clone();
    sigil_runtime::agent_tools::register_agent_tools_with_registry_and_mode(
        &mut target_tool_registry,
        target_agent_registry,
        target_agent_budget,
        effective_root_config.task.multi_agent_mode,
    )
    .map_err(|error| format!("failed to rebuild agent tools for target session: {error:#}"))?;

    let pending_agent_result_continuations =
        pending_agent_result_continuations_from_session(Some(&session));
    let provider_name = session.provider_name().to_owned();
    let model_name = session.model_name().to_owned();
    let entries = session.entries().to_vec();

    state.compaction.preparation_tasks.abort_all();
    state.compaction.local_preview = None;
    state.compaction.pending = None;
    state.compaction.idle_auto = IdleAutoCompactionState::default();
    state.session.pending_queued_pre_turn_preparation = None;
    state.session.last_queued_pre_turn_block = None;
    state.session.last_task_guidance_block = None;
    state.session.pending_agent_result_continuations = pending_agent_result_continuations;
    state.session.active_terminal_task_ids = session
        .terminal_task_projection()
        .active_task_ids
        .into_iter()
        .collect();
    state.session.detached_durable_controls.clear();
    if !same_logical_session {
        state.session.exact_prompts.clear();
    }
    state.session.active_projection_subscription = None;
    state.session.projection_reconciling = false;
    state.session.projection_retry_at = None;
    state.session.projection_reconciliation_error = None;
    state.session.projection_reconciliation_attempts = 0;
    state.session.projection_reconciliation_latched = false;
    state.session.current = Some(session);
    state.session.log_path = session_log_path.clone();
    let _binding = state.wake_coalescer.switch_session_scope(
        state
            .session
            .current
            .as_ref()
            .expect("transition installed the target session")
            .session_scope_id()
            .to_owned(),
    );
    state.session.task_guidance_dirty = true;
    state.session.conversation_queue_dirty = true;
    state.session.task_guidance_retry_at = None;
    state.session.conversation_queue_retry_at = None;
    state.session.task_guidance_retry_attempts = 0;
    state.session.conversation_queue_retry_attempts = 0;
    state.session.task_guidance_retry_latched = false;
    state.session.conversation_queue_retry_latched = false;
    state.agent.supervisor = target_agent_supervisor;
    state.run.pending_task_handoffs = pending_task_handoffs;
    register_worker_active_projection_observer(state)?;

    Ok(SessionTransitionOutcome {
        session_log_path,
        provider_name,
        model_name,
        entries,
    })
}

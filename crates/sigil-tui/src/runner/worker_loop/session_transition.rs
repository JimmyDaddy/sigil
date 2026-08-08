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
        maintenance_task_active: bool,
        terminal_task_active: bool,
    ) -> Option<&'static str> {
        if foreground_run_active {
            return Some(match self {
                Self::Switch => "cannot switch sessions while the agent is running",
                Self::StartNew => "cannot start a new session while the agent is running",
                Self::LocalFork => "cannot fork a local session while the agent is running",
                Self::CheckpointFork => "cannot fork conversation while the agent is running",
            });
        }
        if background_run_active {
            return Some(match self {
                Self::Switch => "cannot switch sessions while a background agent is running",
                Self::StartNew => "cannot start a new session while a background agent is running",
                Self::LocalFork => {
                    "cannot fork a local session while a background agent is running"
                }
                Self::CheckpointFork => {
                    "cannot fork conversation while a background agent is running"
                }
            });
        }
        if maintenance_task_active {
            // Switch/StartNew abandon the current session, so the transition itself joins its
            // in-flight maintenance before installing the target session instead of being
            // rejected. Forks copy the current session and keep it current, so they still wait.
            match self {
                Self::Switch | Self::StartNew => {}
                Self::LocalFork => {
                    return Some(
                        "cannot fork a local session while session maintenance is running",
                    );
                }
                Self::CheckpointFork => {
                    return Some("cannot fork conversation while session maintenance is running");
                }
            }
        }
        terminal_task_active.then_some(match self {
            Self::Switch => "cannot switch sessions while a terminal task is active",
            Self::StartNew => "cannot start a new session while a terminal task is active",
            Self::LocalFork => "cannot fork a local session while a terminal task is active",
            Self::CheckpointFork => "cannot fork conversation while a terminal task is active",
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
    pub(in crate::runner) session_attachment:
        Arc<sigil_runtime::interactive_session_attachment::InteractiveSessionAttachmentLease>,
}

enum SessionTransitionAttachment<'a> {
    Acquire,
    Supplied(Arc<sigil_runtime::interactive_session_attachment::InteractiveSessionAttachmentLease>),
    Recovery(Option<&'a str>),
}

#[derive(Debug)]
struct SessionTransitionRecovery {
    code: sigil_kernel::PublicRouteRecoveryCode,
    actions: Vec<sigil_kernel::PublicRouteRecoveryAction>,
    recovery_binding: String,
    retryable: bool,
    target_session: crate::runner::WorkerRouteRecoverySessionTarget,
    attachment:
        Arc<sigil_runtime::interactive_session_attachment::InteractiveSessionAttachmentLease>,
    source: anyhow::Error,
}

impl std::fmt::Display for SessionTransitionRecovery {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "session route recovery is required: {:#}",
            self.source
        )
    }
}

impl std::error::Error for SessionTransitionRecovery {}

pub(in crate::runner) fn session_transition_recovery_message(
    error: &anyhow::Error,
) -> Option<(WorkerMessage, WorkerMessage)> {
    let recovery = error.downcast_ref::<SessionTransitionRecovery>()?;
    Some((
        WorkerMessage::SessionAttachmentTransferred {
            session_log_path: recovery.target_session.session_log_path.clone(),
            attachment: Arc::clone(&recovery.attachment),
        },
        WorkerMessage::SessionRouteRecoveryRequired {
            code: recovery.code,
            actions: recovery.actions.clone(),
            recovery_binding: recovery.recovery_binding.clone(),
            retryable: recovery.retryable,
            target_session: Some(recovery.target_session.clone()),
        },
    ))
}

pub(in crate::runner) fn ensure_session_transition_allowed(
    kind: SessionTransitionKind,
    state: &WorkerLoopState,
) -> std::result::Result<(), String> {
    kind.block_reason(
        state.run.active.is_some(),
        state.agent.background_runs.has_any(),
        state.compaction.preparation_tasks.has_active() || state.artifact_gc.tasks.has_active(),
        !state.session.active_terminal_task_ids.is_empty(),
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
) -> anyhow::Result<SessionTransitionOutcome>
where
    P: sigil_kernel::Provider,
{
    transition_session_with_attachment_and_recovery(
        kind,
        session_log_path,
        SessionTransitionAttachment::Acquire,
        runtime,
        root_config,
        provider_capabilities,
        workspace_root,
        agent,
        state,
        message_tx,
    )
}

pub(in crate::runner) fn transition_session_with_attachment<P>(
    kind: SessionTransitionKind,
    session_log_path: PathBuf,
    supplied_target_attachment: Arc<
        sigil_runtime::interactive_session_attachment::InteractiveSessionAttachmentLease,
    >,
    runtime: &tokio::runtime::Runtime,
    root_config: &RootConfig,
    provider_capabilities: &ProviderCapabilities,
    workspace_root: &Path,
    agent: &Arc<Agent<P>>,
    state: &mut WorkerLoopState,
    message_tx: &mpsc::Sender<WorkerMessage>,
) -> anyhow::Result<SessionTransitionOutcome>
where
    P: sigil_kernel::Provider,
{
    transition_session_with_attachment_and_recovery(
        kind,
        session_log_path,
        SessionTransitionAttachment::Supplied(supplied_target_attachment),
        runtime,
        root_config,
        provider_capabilities,
        workspace_root,
        agent,
        state,
        message_tx,
    )
}

pub(in crate::runner) fn transition_session_with_attachment_recovery<P>(
    kind: SessionTransitionKind,
    session_log_path: PathBuf,
    attachment_recovery_binding: Option<&str>,
    runtime: &tokio::runtime::Runtime,
    root_config: &RootConfig,
    provider_capabilities: &ProviderCapabilities,
    workspace_root: &Path,
    agent: &Arc<Agent<P>>,
    state: &mut WorkerLoopState,
    message_tx: &mpsc::Sender<WorkerMessage>,
) -> anyhow::Result<SessionTransitionOutcome>
where
    P: sigil_kernel::Provider,
{
    transition_session_with_attachment_and_recovery(
        kind,
        session_log_path,
        SessionTransitionAttachment::Recovery(attachment_recovery_binding),
        runtime,
        root_config,
        provider_capabilities,
        workspace_root,
        agent,
        state,
        message_tx,
    )
}

fn transition_session_with_attachment_and_recovery<P>(
    kind: SessionTransitionKind,
    session_log_path: PathBuf,
    attachment: SessionTransitionAttachment<'_>,
    runtime: &tokio::runtime::Runtime,
    root_config: &RootConfig,
    provider_capabilities: &ProviderCapabilities,
    workspace_root: &Path,
    agent: &Arc<Agent<P>>,
    state: &mut WorkerLoopState,
    message_tx: &mpsc::Sender<WorkerMessage>,
) -> anyhow::Result<SessionTransitionOutcome>
where
    P: sigil_kernel::Provider,
{
    ensure_session_transition_allowed(kind, state).map_err(anyhow::Error::msg)?;

    let mut target_attachment = if state.session.log_path == session_log_path {
        None
    } else {
        Some(match attachment {
            SessionTransitionAttachment::Supplied(attachment) => {
                anyhow::ensure!(
                    attachment.session_path() == JsonlSessionStore::new(&session_log_path)?.path(),
                    "supplied transition attachment belongs to another durable session"
                );
                attachment
            }
            SessionTransitionAttachment::Recovery(Some(recovery_binding)) => Arc::new(
                sigil_runtime::interactive_session_attachment::InteractiveSessionAttachmentLease::acquire_for_path_retry(
                    &session_log_path,
                    recovery_binding,
                )
                .map_err(anyhow::Error::new)?,
            ),
            SessionTransitionAttachment::Acquire
            | SessionTransitionAttachment::Recovery(None) => Arc::new(
                sigil_runtime::interactive_session_attachment::InteractiveSessionAttachmentLease::acquire(
                    &session_log_path,
                )
                .map_err(anyhow::Error::new)?,
            ),
        })
    };

    let route_attachment = target_attachment
        .as_ref()
        .map(Arc::as_ref)
        .unwrap_or_else(|| state.session.attachment_lease.as_ref());
    let mut session = match load_routed_session_with_runtime_attachments(
        root_config,
        &session_log_path,
        state.session.current.as_ref(),
        route_attachment,
    ) {
        Ok(session) => session,
        Err(source) => {
            let recovery =
                crate::runner::worker_session_route_recovery_message(&source, &session_log_path);
            if kind == SessionTransitionKind::Switch
                && let Some(attachment) = target_attachment.take()
                && let Some(WorkerMessage::SessionRouteRecoveryRequired {
                    code,
                    actions,
                    recovery_binding,
                    retryable,
                    ..
                }) = recovery
                && let Ok(target) = load_session(
                    &root_config.agent.runtime_provider,
                    &root_config.agent.model,
                    &session_log_path,
                )
            {
                return Err(anyhow::Error::new(SessionTransitionRecovery {
                    code,
                    actions,
                    recovery_binding,
                    retryable,
                    target_session: crate::runner::WorkerRouteRecoverySessionTarget {
                        session_log_path,
                        provider_name: target.provider_name().to_owned(),
                        model_name: target.model_name().to_owned(),
                        entries: target.entries().to_vec(),
                    },
                    attachment,
                    source,
                }));
            }
            return Err(source);
        }
    };
    let cleanup_report = runtime
        .block_on(
            sigil_runtime::isolated_workspace::reconcile_isolated_workspace_cleanup(
                &mut session,
                workspace_root,
            ),
        )
        .map_err(|error| {
            anyhow::anyhow!("failed to reconcile isolated task workspaces: {error:#}")
        })?;
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
        .map_err(|error| {
            anyhow::anyhow!("failed to reconcile integration promotions: {error:#}")
        })?;
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
        ensure_session_workspace_trust(&mut session, workspace_root, kind.trust_reason())
            .map_err(anyhow::Error::msg)?;
    }

    let parent_session_ref =
        session_ref_for_log_path(&session_log_path).map_err(anyhow::Error::msg)?;
    let pending_task_handoffs =
        ConversationCoordinator::new(root_config.task.enabled, root_config.task.routing_policy)
            .reconcile(&mut session, &parent_session_ref, current_unix_time_ms())
            .map_err(|error| {
                anyhow::anyhow!("failed to reconcile durable task handoffs: {error:#}")
            })?;
    let effective_root_config =
        super::agent_runtime::effective_orchestration_root_config(root_config, &session);

    let target_agent_registry =
        sigil_runtime::AgentProfileRegistry::from_root_config_with_workspace_and_entries(
            &effective_root_config,
            workspace_root,
            session.entries(),
        )
        .map_err(|error| {
            anyhow::anyhow!("failed to rebuild agent profiles for target session: {error:#}")
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
    .map_err(|error| {
        anyhow::anyhow!("failed to rebuild agent tools for target session: {error:#}")
    })?;

    let pending_agent_result_continuations =
        pending_agent_result_continuations_from_session(Some(&session));
    let provider_name = session.provider_name().to_owned();
    let model_name = session.model_name().to_owned();
    let entries = session.entries().to_vec();

    state.compaction.preparation_tasks.cancel_and_join(runtime);
    state.artifact_gc.tasks.cancel_and_join(runtime);
    state.compaction.local_preview = None;
    state.compaction.pending = None;
    state.compaction.idle_auto = IdleAutoCompactionState::default();
    state.session.pending_queued_pre_turn_preparation = None;
    state.session.pending_cost_only_tool_output_aging = None;
    state.session.tool_artifact_read_budget = ToolArtifactReadBudgetV1::default();
    state.session.last_queued_pre_turn_block = None;
    state.session.last_task_guidance_block = None;
    state.session.pending_agent_result_continuations = pending_agent_result_continuations;
    state.session.active_terminal_task_ids = session
        .terminal_task_projection()
        .active_task_ids
        .into_iter()
        .collect();
    state.session.terminal_lifecycle_generations = session
        .terminal_task_projection()
        .tasks
        .into_iter()
        .map(|(task_id, summary)| (task_id, summary.generation))
        .collect();
    state.session.terminal_task_control_identities.clear();
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
    if let Some(target_attachment) = target_attachment {
        state.session.attachment_lease = target_attachment;
    }
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
    state.session.tool_output_pressure_dirty = true;
    state.session.artifact_gc_dirty = true;
    state.session.task_guidance_retry_at = None;
    state.session.conversation_queue_retry_at = None;
    state.session.task_guidance_retry_attempts = 0;
    state.session.conversation_queue_retry_attempts = 0;
    state.session.task_guidance_retry_latched = false;
    state.session.conversation_queue_retry_latched = false;
    state.agent.supervisor = target_agent_supervisor;
    state.run.pending_task_handoffs = pending_task_handoffs;
    register_worker_active_projection_observer(state).map_err(anyhow::Error::msg)?;

    Ok(SessionTransitionOutcome {
        session_log_path,
        provider_name,
        model_name,
        entries,
        session_attachment: Arc::clone(&state.session.attachment_lease),
    })
}

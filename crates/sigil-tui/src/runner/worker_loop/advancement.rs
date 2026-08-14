use super::*;
use crate::runner::{V2CompactionAdmission, V2CompactionPreviewState};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::runner) enum WorkerAdvancementControl {
    PollCommand,
    SkipCommandPoll,
}

const PROJECTION_RECONCILIATION_BASE_BACKOFF: Duration = Duration::from_millis(250);
const PROJECTION_RECONCILIATION_MAX_BACKOFF: Duration = Duration::from_secs(8);
const PROJECTION_RECONCILIATION_MAX_ATTEMPTS: u8 = 6;
const AUTHORITY_CAS_RETRY_MAX_ATTEMPTS: u8 = 6;

fn arm_authority_cas_retry(
    retry_at: &mut Option<Instant>,
    attempts: &mut u8,
    latched: &mut bool,
    now: Instant,
) -> bool {
    *attempts = attempts.saturating_add(1);
    if *attempts >= AUTHORITY_CAS_RETRY_MAX_ATTEMPTS {
        *retry_at = None;
        *latched = true;
        return false;
    }
    *retry_at = Some(now + projection_reconciliation_backoff(*attempts));
    true
}

fn reset_authority_cas_retry(
    retry_at: &mut Option<Instant>,
    attempts: &mut u8,
    latched: &mut bool,
) {
    *retry_at = None;
    *attempts = 0;
    *latched = false;
}

fn release_due_authority_cas_retry(
    retry_at: &mut Option<Instant>,
    dirty: &mut bool,
    now: Instant,
) -> bool {
    if !retry_at.is_some_and(|deadline| deadline <= now) {
        return false;
    }
    *retry_at = None;
    *dirty = true;
    true
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ProjectionReconciliationControl {
    Ready,
    Reconciled,
    Blocked,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ProjectionReconciliationFailureDisposition {
    RetryAfter(Duration),
    Latched,
}

pub(in crate::runner) struct WorkerAdvancementContext<'a, P> {
    pub(in crate::runner) runtime: &'a tokio::runtime::Runtime,
    pub(in crate::runner) agent: &'a mut Arc<Agent<P>>,
    pub(in crate::runner) root_config: &'a RootConfig,
    pub(in crate::runner) provider_capabilities: &'a ProviderCapabilities,
    pub(in crate::runner) workspace_root: &'a PathBuf,
    pub(in crate::runner) options: &'a AgentRunOptions,
    pub(in crate::runner) message_tx: &'a mpsc::Sender<WorkerMessage>,
    pub(in crate::runner) elicitation_handler: &'a Arc<ChannelMcpElicitationHandler>,
    pub(in crate::runner) mcp_event_handler: &'a Arc<ChannelMcpRuntimeEventHandler>,
    pub(in crate::runner) role_provider_builder: &'a Arc<dyn TaskRoleProviderBuilder>,
    pub(in crate::runner) context_resolver: &'a sigil_runtime::RequestContextResolver,
    pub(in crate::runner) state: &'a mut WorkerLoopState,
}

impl<'a, P> WorkerAdvancementContext<'a, P> {
    fn reborrow(&mut self) -> WorkerAdvancementContext<'_, P> {
        WorkerAdvancementContext {
            runtime: self.runtime,
            agent: &mut *self.agent,
            root_config: self.root_config,
            provider_capabilities: self.provider_capabilities,
            workspace_root: self.workspace_root,
            options: self.options,
            message_tx: self.message_tx,
            elicitation_handler: self.elicitation_handler,
            mcp_event_handler: self.mcp_event_handler,
            role_provider_builder: self.role_provider_builder,
            context_resolver: self.context_resolver,
            state: &mut *self.state,
        }
    }
}

pub(in crate::runner) fn advance_worker_loop<P>(
    mut context: WorkerAdvancementContext<'_, P>,
) -> WorkerAdvancementControl
where
    P: sigil_kernel::Provider + Send + Sync + 'static,
{
    let allow_observational_refresh = !context.state.readiness.has_priority_ready_work();
    let run_active = context.state.run.active.is_some();
    if run_active != context.state.last_observed_run_active {
        context.state.last_observed_run_active = run_active;
        context.state.session.task_guidance_dirty = true;
        context.state.session.conversation_queue_dirty = true;
    }
    // Terminal run/OAuth completions may still settle while projection authority is reconciling.
    let run_advanced = matches!(
        advance_run_results(context.reborrow()),
        WorkerAdvancementControl::SkipCommandPoll
    );
    let terminal_lifecycle_advanced =
        advance_terminal_lifecycle_updates(context.message_tx, context.options, context.state);
    // Consume coalesced projection readiness after the P1 active-run terminal. A projection
    // invalidation becomes a persistent fail-closed state before any new authority work starts.
    let refresh_advanced = matches!(
        advance_refreshes(context.reborrow(), allow_observational_refresh),
        WorkerAdvancementControl::SkipCommandPoll
    );
    let oauth_advanced = advance_mcp_oauth_results(context.message_tx, context.state);
    let task_route_diagnostics_advanced =
        advance_task_provider_route_diagnostics(context.message_tx, context.state);
    let task_completion_progress_advanced =
        advance_task_completion_progress(context.message_tx, context.state);

    match advance_projection_reconciliation(context.message_tx, context.state) {
        ProjectionReconciliationControl::Blocked => {
            return if run_advanced
                || terminal_lifecycle_advanced
                || refresh_advanced
                || oauth_advanced
                || task_route_diagnostics_advanced
                || task_completion_progress_advanced
            {
                WorkerAdvancementControl::SkipCommandPoll
            } else {
                WorkerAdvancementControl::PollCommand
            };
        }
        ProjectionReconciliationControl::Reconciled => {
            // Reconciliation rehydrates every authority family below in the same safe-point pass;
            // returning here would let an already queued ordinary command overtake that work.
        }
        ProjectionReconciliationControl::Ready => {}
    }

    // Safe-point priority after terminal settlement:
    // compaction result -> blocking child continuation -> task guidance -> queued user work
    // -> recovered handoff -> deterministic tool aging -> opportunistic idle compaction
    // -> artifact maintenance.
    if matches!(
        advance_compaction_results(context.reborrow()),
        WorkerAdvancementControl::SkipCommandPoll
    ) || matches!(
        advance_background_agents(context.reborrow()),
        WorkerAdvancementControl::SkipCommandPoll
    ) || matches!(
        advance_pending_agent_continuations(context.reborrow()),
        WorkerAdvancementControl::SkipCommandPoll
    ) || matches!(
        advance_task_guidance(context.reborrow()),
        WorkerAdvancementControl::SkipCommandPoll
    ) || matches!(
        advance_pending_task_handoffs(context.reborrow()),
        WorkerAdvancementControl::SkipCommandPoll
    ) || matches!(
        advance_conversation_queue(context.reborrow()),
        WorkerAdvancementControl::SkipCommandPoll
    ) || matches!(
        advance_tool_output_pressure(context.reborrow()),
        WorkerAdvancementControl::SkipCommandPoll
    ) || matches!(
        advance_idle_compaction(context.reborrow()),
        WorkerAdvancementControl::SkipCommandPoll
    ) || matches!(
        advance_artifact_gc_results(context.reborrow()),
        WorkerAdvancementControl::SkipCommandPoll
    ) || matches!(
        advance_artifact_gc_start(context.reborrow()),
        WorkerAdvancementControl::SkipCommandPoll
    ) || run_advanced
        || terminal_lifecycle_advanced
        || refresh_advanced
        || oauth_advanced
        || task_route_diagnostics_advanced
        || task_completion_progress_advanced
    {
        WorkerAdvancementControl::SkipCommandPoll
    } else {
        WorkerAdvancementControl::PollCommand
    }
}

fn advance_terminal_lifecycle_updates(
    message_tx: &mpsc::Sender<WorkerMessage>,
    options: &AgentRunOptions,
    state: &mut WorkerLoopState,
) -> bool {
    let current_session_scope_id = state
        .wake_coalescer
        .current_projection_binding()
        .map(|binding| binding.session_scope_id);
    let mut advanced = false;
    while let Some(routed) = state.readiness.terminal_lifecycle_updates.pop_front() {
        advanced = true;
        if current_session_scope_id.as_deref() != Some(routed.session_scope_id.as_str()) {
            continue;
        }
        let entry = routed.update.task;
        if state
            .session
            .terminal_lifecycle_generations
            .get(&entry.handle.task_id)
            .is_some_and(|generation| *generation >= entry.generation)
        {
            continue;
        }
        let identity = TerminalTaskControlIdentity {
            session_scope_id: routed.session_scope_id.clone(),
            run_id: routed.run_id.clone(),
            task_id: entry.handle.task_id.as_str().to_owned(),
            expected_generation: entry.generation,
        };
        state
            .session
            .terminal_lifecycle_generations
            .insert(entry.handle.task_id.clone(), entry.generation);
        if entry.status.is_active() {
            state
                .session
                .active_terminal_task_ids
                .insert(entry.handle.task_id.clone());
            state
                .session
                .terminal_task_control_identities
                .insert(entry.handle.task_id.clone(), identity.clone());
        } else {
            state
                .session
                .active_terminal_task_ids
                .remove(&entry.handle.task_id);
            state
                .session
                .terminal_task_control_identities
                .remove(&entry.handle.task_id);
        }

        let control = ControlEntry::TerminalTask(entry.clone());
        if let Some(session) = state.session.current.as_mut() {
            session.record_durably_appended_controls([control]);
            if entry.status.is_terminal()
                && let Some(profile) = terminal_start_execution_profile_for_task(
                    session.entries(),
                    &entry.handle.task_id,
                )
                && let Err(error) = MutationEventRecorder::new(
                    match JsonlSessionStore::new(&state.session.log_path) {
                        Ok(store) => store,
                        Err(error) => {
                            let _ = message_tx.send(WorkerMessage::Notice(format!(
                                "failed to open terminal lifecycle mutation recorder for {}: {error:#}",
                                entry.handle.task_id.as_str()
                            )));
                            continue;
                        }
                    },
                )
                .reconcile_execution_mutation_profile(&options.workspace_root, &profile)
            {
                let _ = message_tx.send(WorkerMessage::Notice(format!(
                    "failed to reconcile terminal task {} workspace mutation after {}: {error:#}",
                    entry.handle.task_id.as_str(),
                    routed.run_id
                )));
            }
            let entries = session.entries().to_vec();
            let _ = message_tx.send(WorkerMessage::TerminalTaskUpdated {
                identity,
                entry,
                entries,
            });
        } else {
            state.session.detached_durable_controls.push(control);
            if entry.status.is_terminal() {
                let durable_entries = JsonlSessionStore::read_entries(&state.session.log_path);
                match durable_entries {
                    Ok(entries) => {
                        if let Some(profile) = terminal_start_execution_profile_for_task(
                            &entries,
                            &entry.handle.task_id,
                        ) && let Ok(store) = JsonlSessionStore::new(&state.session.log_path)
                            && let Err(error) = MutationEventRecorder::new(store)
                                .reconcile_execution_mutation_profile(
                                    &options.workspace_root,
                                    &profile,
                                )
                        {
                            let _ = message_tx.send(WorkerMessage::Notice(format!(
                                "failed to reconcile detached terminal task {} workspace mutation after {}: {error:#}",
                                entry.handle.task_id.as_str(),
                                routed.run_id
                            )));
                        }
                    }
                    Err(error) => {
                        let _ = message_tx.send(WorkerMessage::Notice(format!(
                            "failed to read terminal task {} mutation profile after {}: {error:#}",
                            entry.handle.task_id.as_str(),
                            routed.run_id
                        )));
                    }
                }
            }
        }
    }
    advanced
}

fn advance_projection_reconciliation(
    message_tx: &mpsc::Sender<WorkerMessage>,
    state: &mut WorkerLoopState,
) -> ProjectionReconciliationControl {
    if !state.session.projection_reconciling {
        return ProjectionReconciliationControl::Ready;
    }
    if state.session.projection_reconciliation_latched {
        return ProjectionReconciliationControl::Blocked;
    }

    let now = Instant::now();
    if state
        .session
        .projection_retry_at
        .is_some_and(|retry_at| retry_at > now)
    {
        return ProjectionReconciliationControl::Blocked;
    }

    let reconciliation = state
        .session
        .current
        .as_ref()
        .ok_or_else(|| "active session is unavailable".to_owned())
        .and_then(|session| {
            let snapshot = session
                .active_projection_snapshot()
                .map_err(|error| format!("{error:#}"))?
                .ok_or_else(|| "active session has no durable projection".to_owned())?;
            let pending_agent_continuations =
                pending_agent_continuations_from_snapshot(session, &snapshot)?;
            let active_terminal_task_ids =
                active_terminal_task_ids_from_snapshot(session, &snapshot)?;
            Ok((pending_agent_continuations, active_terminal_task_ids))
        });
    match reconciliation {
        Ok((pending_agent_continuations, active_terminal_task_ids)) => {
            state.session.projection_reconciling = false;
            state.session.projection_retry_at = None;
            state.session.projection_reconciliation_error = None;
            state.session.projection_reconciliation_attempts = 0;
            state.session.projection_reconciliation_latched = false;
            state.session.task_guidance_dirty = true;
            state.session.conversation_queue_dirty = true;
            state.session.tool_output_pressure_dirty = true;
            state.session.artifact_gc_dirty = true;
            state.session.pending_agent_result_continuations = pending_agent_continuations;
            state.session.active_terminal_task_ids = active_terminal_task_ids;
            let _ = message_tx.send(WorkerMessage::Notice(
                "active session projection reconciled; scheduler authority restored".to_owned(),
            ));
            ProjectionReconciliationControl::Reconciled
        }
        Err(error) => {
            match record_projection_reconciliation_failure(
                &mut state.session.projection_reconciliation_attempts,
            ) {
                ProjectionReconciliationFailureDisposition::RetryAfter(delay) => {
                    state.session.projection_retry_at = Some(now + delay);
                }
                ProjectionReconciliationFailureDisposition::Latched => {
                    state.session.projection_reconciliation_latched = true;
                    state.session.projection_retry_at = None;
                    let _ = message_tx.send(WorkerMessage::Notice(format!(
                        "active session projection reconciliation latched after {} failed attempts; authority-bearing work remains disabled until the session is reloaded: {error}",
                        state.session.projection_reconciliation_attempts
                    )));
                }
            }
            if state.session.projection_reconciliation_error.as_deref() != Some(&error) {
                let _ = message_tx.send(WorkerMessage::Notice(format!(
                    "active session projection is reconciling; authority-bearing work remains disabled: {error}"
                )));
                state.session.projection_reconciliation_error = Some(error);
            }
            ProjectionReconciliationControl::Blocked
        }
    }
}

fn active_task_id(state: &WorkerLoopState) -> Option<&str> {
    match state
        .run
        .active
        .as_ref()
        .map(|active| &active.cancellation_target)
    {
        Some(RunCancellationTarget::Task { task_id }) => Some(task_id),
        Some(RunCancellationTarget::Run | RunCancellationTarget::AgentThread { .. }) | None => None,
    }
}

fn active_conversation_queue_is_idle(session: &Session) -> anyhow::Result<bool> {
    match session.active_projection_snapshot()? {
        Some(snapshot) => Ok(snapshot
            .conversation_queue()
            .queue
            .items
            .iter()
            .all(|item| item.status.is_terminal())),
        None => Ok(session
            .conversation_queue_projection()
            .items
            .iter()
            .all(|item| item.status.is_terminal())),
    }
}

fn active_next_dispatchable_queue_id(
    session: &Session,
) -> anyhow::Result<Option<ConversationInputQueueId>> {
    match session.active_projection_snapshot()? {
        Some(snapshot) => Ok(snapshot
            .conversation_queue()
            .queue
            .next_dispatchable
            .clone()),
        None => Ok(session.conversation_queue_projection().next_dispatchable),
    }
}

fn enter_projection_reconciliation(
    message_tx: &mpsc::Sender<WorkerMessage>,
    state: &mut WorkerLoopState,
    error: impl std::fmt::Display,
) {
    let error = error.to_string();
    schedule_projection_reconciliation_after_invalidation(
        &mut state.session.projection_reconciling,
        &mut state.session.projection_retry_at,
        &mut state.session.projection_reconciliation_attempts,
        &mut state.session.projection_reconciliation_latched,
        Instant::now(),
    );
    if state.session.projection_reconciliation_error.as_deref() != Some(&error) {
        let _ = message_tx.send(WorkerMessage::Notice(format!(
            "active session projection requires reconciliation; authority-bearing work was disabled: {error}"
        )));
        state.session.projection_reconciliation_error = Some(error);
    }
}

fn schedule_projection_reconciliation_after_invalidation(
    reconciling: &mut bool,
    retry_at: &mut Option<Instant>,
    attempts: &mut u8,
    latched: &mut bool,
    now: Instant,
) {
    if !*reconciling {
        *attempts = 0;
        *latched = false;
        *retry_at = Some(now);
    } else if retry_at.is_none() && !*latched {
        *retry_at = Some(now);
    }
    *reconciling = true;
}

fn projection_reconciliation_backoff(attempt: u8) -> Duration {
    let exponent = u32::from(attempt.saturating_sub(1).min(5));
    let base = PROJECTION_RECONCILIATION_BASE_BACKOFF
        .saturating_mul(1_u32 << exponent)
        .min(PROJECTION_RECONCILIATION_MAX_BACKOFF);
    let jitter_ceiling_ms = u64::try_from((base.as_millis() / 5).max(1)).unwrap_or(u64::MAX);
    let jitter_ms = u64::from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .subsec_nanos(),
    ) % (jitter_ceiling_ms + 1);
    base.saturating_add(Duration::from_millis(jitter_ms))
}

fn record_projection_reconciliation_failure(
    attempts: &mut u8,
) -> ProjectionReconciliationFailureDisposition {
    *attempts = attempts.saturating_add(1);
    if *attempts >= PROJECTION_RECONCILIATION_MAX_ATTEMPTS {
        ProjectionReconciliationFailureDisposition::Latched
    } else {
        ProjectionReconciliationFailureDisposition::RetryAfter(projection_reconciliation_backoff(
            *attempts,
        ))
    }
}

fn advance_task_provider_route_diagnostics(
    message_tx: &mpsc::Sender<WorkerMessage>,
    state: &mut WorkerLoopState,
) -> bool {
    let task_run_active = active_task_id(state).is_some();
    let active_snapshot = if task_run_active {
        state.agent.supervisor.task_provider_route_diagnostics()
    } else {
        sigil_runtime::TaskProviderRouteDiagnosticsSnapshot::default()
    };
    let Some(snapshot) = changed_task_provider_route_diagnostics(
        task_run_active,
        active_snapshot,
        &state.agent.last_task_provider_route_diagnostics,
    ) else {
        return false;
    };
    state.agent.last_task_provider_route_diagnostics = snapshot.clone();
    let _ = message_tx.send(WorkerMessage::TaskProviderRouteDiagnosticsUpdated { snapshot });
    true
}

fn advance_task_completion_progress(
    message_tx: &mpsc::Sender<WorkerMessage>,
    state: &mut WorkerLoopState,
) -> bool {
    let active_task_id = active_task_id(state);
    let active_snapshot = task_completion_progress_for_active_task(
        active_task_id,
        state.agent.supervisor.task_completion_progress(),
    );
    let Some(snapshot) = changed_task_completion_progress(
        active_task_id.is_some(),
        active_snapshot,
        &state.agent.last_task_completion_progress,
    ) else {
        return false;
    };
    state.agent.last_task_completion_progress = snapshot.clone();
    let _ = message_tx.send(WorkerMessage::TaskCompletionProgressUpdated { snapshot });
    true
}

pub(in crate::runner) fn task_completion_progress_for_active_task(
    active_task_id: Option<&str>,
    snapshot: sigil_runtime::TaskCompletionProgressSnapshot,
) -> sigil_runtime::TaskCompletionProgressSnapshot {
    if active_task_id.is_some_and(|active_task_id| {
        snapshot
            .batch
            .as_ref()
            .is_some_and(|batch| batch.task_id == active_task_id)
    }) {
        snapshot
    } else {
        sigil_runtime::TaskCompletionProgressSnapshot::default()
    }
}

pub(in crate::runner) fn changed_task_provider_route_diagnostics(
    task_run_active: bool,
    active_snapshot: sigil_runtime::TaskProviderRouteDiagnosticsSnapshot,
    previous: &sigil_runtime::TaskProviderRouteDiagnosticsSnapshot,
) -> Option<sigil_runtime::TaskProviderRouteDiagnosticsSnapshot> {
    let snapshot = if task_run_active {
        active_snapshot
    } else {
        sigil_runtime::TaskProviderRouteDiagnosticsSnapshot::default()
    };
    (snapshot != *previous).then_some(snapshot)
}

pub(in crate::runner) fn changed_task_completion_progress(
    task_run_active: bool,
    active_snapshot: sigil_runtime::TaskCompletionProgressSnapshot,
    previous: &sigil_runtime::TaskCompletionProgressSnapshot,
) -> Option<sigil_runtime::TaskCompletionProgressSnapshot> {
    let snapshot = if task_run_active {
        active_snapshot
    } else {
        sigil_runtime::TaskCompletionProgressSnapshot::default()
    };
    (snapshot != *previous).then_some(snapshot)
}

fn advance_pending_task_handoffs<P>(
    context: WorkerAdvancementContext<'_, P>,
) -> WorkerAdvancementControl
where
    P: sigil_kernel::Provider + Send + Sync + 'static,
{
    let WorkerAdvancementContext {
        runtime,
        agent,
        root_config,
        options,
        message_tx,
        elicitation_handler,
        role_provider_builder,
        state,
        ..
    } = context;
    if state.run.active.is_some() || state.run.pending_task_handoffs.is_empty() {
        return WorkerAdvancementControl::PollCommand;
    }
    let queued_main_input_ready = match state
        .session
        .current
        .as_ref()
        .map(active_next_dispatchable_queue_id)
        .transpose()
    {
        Ok(queue_id) => queue_id.flatten().is_some(),
        Err(error) => {
            enter_projection_reconciliation(message_tx, state, error);
            return WorkerAdvancementControl::SkipCommandPoll;
        }
    };
    if queued_main_input_ready {
        return WorkerAdvancementControl::PollCommand;
    }
    let action = state.run.pending_task_handoffs.remove(0);
    let Some(mut run_session) = state.session.current.take() else {
        let _ = message_tx.send(WorkerMessage::RunFailed(
            "session state is unavailable for recovered task handoff".to_owned(),
        ));
        return WorkerAdvancementControl::SkipCommandPoll;
    };
    let task = run_session
        .task_state_projection()
        .tasks
        .get(&action.task_id)
        .cloned();
    let Some(task) = task else {
        state.session.current = Some(run_session);
        let _ = message_tx.send(WorkerMessage::RunFailed(
            "recovered task handoff is missing its durable task".to_owned(),
        ));
        return WorkerAdvancementControl::SkipCommandPoll;
    };
    let (cancellation_owner, cancellation_recorder, cancellation_handle, cancellation_task_guard) =
        match prepare_task_run_cancellation(&mut run_session, &action.task_id) {
            Ok(cancellation) => cancellation,
            Err(error) => {
                state.session.current = Some(run_session);
                let _ = message_tx.send(WorkerMessage::RunFailed(error));
                return WorkerAdvancementControl::SkipCommandPoll;
            }
        };
    let task_id_value = action.task_id.as_str().to_owned();
    let _ = message_tx.send(WorkerMessage::TaskRunStarted {
        task_id: task_id_value.clone(),
        objective: task.objective.clone(),
    });
    let handler = ChannelEventHandler::new(message_tx.clone());
    let (approval_tx, approval_rx) = mpsc::channel();
    let elicitation_audit_buffer: McpElicitationAuditBuffer =
        Arc::new(std::sync::Mutex::new(Vec::new()));
    elicitation_handler.set_audit_buffer(Some(Arc::clone(&elicitation_audit_buffer)));
    let run_id = state.allocate_run_id();
    let url_capability_registrar = run_session.user_url_capability_registrar();
    let image_attachment_resolver = run_session.image_attachment_resolver();
    let cancellation_target = RunCancellationTarget::Task {
        task_id: task_id_value.clone(),
    };
    if let Err(error) =
        state.acquire_route_execution_owner_for_scope(run_session.session_scope_id())
    {
        state.session.current = Some(run_session);
        let _ = message_tx.send(WorkerMessage::RunFailed(error));
        return WorkerAdvancementControl::SkipCommandPoll;
    }
    let handle = spawn_task_run(
        runtime,
        TaskRunSpawn {
            run_id,
            session: run_session,
            task_id: action.task_id,
            task_id_value,
            parent_session_ref: task.parent_session_ref,
            objective: task.objective,
            root_config: root_config.clone(),
            options: options.clone(),
            base_registry: agent.tool_registry().clone(),
            agent_supervisor: state.agent.supervisor.clone(),
            role_provider_builder: Arc::clone(role_provider_builder),
            task_result_tx: state.run.result_tx.clone(),
            approval_rx,
            handler,
            elicitation_audit_buffer: Arc::clone(&elicitation_audit_buffer),
            cancellation_handle,
            cancellation_task_guard,
            tool_artifact_read_budget: state.session.tool_artifact_read_budget.clone(),
        },
    );
    state.run.active = Some(ActiveRun {
        run_id,
        handle,
        approval_tx,
        elicitation_audit_buffer,
        cancellation_owner,
        cancellation_recorder,
        cancellation_target,
        url_capability_registrar,
        image_attachment_resolver,
    });
    WorkerAdvancementControl::SkipCommandPoll
}

fn advance_task_guidance<P>(context: WorkerAdvancementContext<'_, P>) -> WorkerAdvancementControl
where
    P: sigil_kernel::Provider + Send + Sync + 'static,
{
    let WorkerAdvancementContext {
        runtime,
        agent,
        root_config,
        options,
        message_tx,
        elicitation_handler,
        role_provider_builder,
        state,
        ..
    } = context;
    if state.run.active.is_some() {
        return WorkerAdvancementControl::PollCommand;
    }
    if !state.session.task_guidance_dirty {
        return WorkerAdvancementControl::PollCommand;
    }
    state.session.task_guidance_dirty = false;
    state.session.task_guidance_retry_at = None;
    let preparation = match state.session.current.as_ref() {
        Some(session) => {
            prepare_next_task_guidance_candidate(session, &state.session.exact_prompts)
        }
        None => Ok(TaskGuidancePreparation::NoQueuedGuidance),
    };
    let preparation = match preparation {
        Ok(preparation) => preparation,
        Err(error) => {
            let _ = message_tx.send(WorkerMessage::Notice(error));
            if !arm_authority_cas_retry(
                &mut state.session.task_guidance_retry_at,
                &mut state.session.task_guidance_retry_attempts,
                &mut state.session.task_guidance_retry_latched,
                Instant::now(),
            ) {
                let _ = message_tx.send(WorkerMessage::Notice(
                    "task guidance authority retry latched after 6 failures; a new relevant durable event or session reload is required".to_owned(),
                ));
            }
            return WorkerAdvancementControl::PollCommand;
        }
    };
    match preparation {
        TaskGuidancePreparation::NoQueuedGuidance => {
            state.session.last_task_guidance_block = None;
            WorkerAdvancementControl::PollCommand
        }
        TaskGuidancePreparation::Waiting { queue_id, reason } => {
            notify_task_guidance_block_once(message_tx, state, queue_id, reason);
            WorkerAdvancementControl::PollCommand
        }
        TaskGuidancePreparation::Terminal {
            queue_id,
            status,
            reason,
        } => {
            state.session.last_task_guidance_block = None;
            state.session.exact_prompts.remove(&queue_id);
            state.session.task_guidance_dirty = true;
            state.session.conversation_queue_dirty = true;
            append_queue_status_and_notify(
                &mut state.session.current,
                message_tx,
                queue_id,
                status,
                Some(reason),
            );
            WorkerAdvancementControl::SkipCommandPoll
        }
        TaskGuidancePreparation::Prepared(candidate) => {
            if !root_config.task.enabled {
                let queue_id = candidate.promotion.queue_id;
                state.session.exact_prompts.remove(&queue_id);
                state.session.task_guidance_dirty = true;
                state.session.conversation_queue_dirty = true;
                append_queue_status_and_notify(
                    &mut state.session.current,
                    message_tx,
                    queue_id,
                    ConversationInputStatus::Rejected,
                    Some(
                        "task guidance cannot dispatch while task execution is disabled".to_owned(),
                    ),
                );
                return WorkerAdvancementControl::SkipCommandPoll;
            }
            let candidate = *candidate;
            let Some(mut run_session) = state.session.current.take() else {
                return WorkerAdvancementControl::PollCommand;
            };
            let task = run_session
                .task_state_projection()
                .tasks
                .get(&candidate.promotion.task_id)
                .cloned();
            let Some(task) = task else {
                state.session.current = Some(run_session);
                return WorkerAdvancementControl::PollCommand;
            };
            // Creating the cancellation scope must remain no-write until the frontier-bound
            // guidance promotion succeeds. Persisting the Task binding first would advance the
            // same durable frontier and make this candidate reject itself as stale.
            let cancellation = match prepare_run_cancellation(&run_session) {
                Ok(cancellation) => cancellation,
                Err(error) => {
                    state.session.current = Some(run_session);
                    notify_task_guidance_block_once(
                        message_tx,
                        state,
                        candidate.promotion.queue_id,
                        error,
                    );
                    return WorkerAdvancementControl::PollCommand;
                }
            };
            let store = match JsonlSessionStore::new(&state.session.log_path) {
                Ok(store) => store,
                Err(error) => {
                    state.session.current = Some(run_session);
                    notify_task_guidance_block_once(
                        message_tx,
                        state,
                        candidate.promotion.queue_id,
                        format!("failed to open task guidance promotion store: {error:#}"),
                    );
                    return WorkerAdvancementControl::PollCommand;
                }
            };
            if let Err(error) = store.append_task_guidance_promoted_at(
                candidate.promotion.clone(),
                &candidate.source_frontier,
            ) {
                state.session.current = Some(run_session);
                if !arm_authority_cas_retry(
                    &mut state.session.task_guidance_retry_at,
                    &mut state.session.task_guidance_retry_attempts,
                    &mut state.session.task_guidance_retry_latched,
                    Instant::now(),
                ) {
                    let _ = message_tx.send(WorkerMessage::Notice(
                        "task guidance authority retry latched after 6 failures; a new relevant durable event or session reload is required".to_owned(),
                    ));
                }
                notify_task_guidance_block_once(
                    message_tx,
                    state,
                    candidate.promotion.queue_id,
                    format!("task guidance promotion compare-and-swap refused: {error:#}"),
                );
                return WorkerAdvancementControl::PollCommand;
            }
            if let Err(error) = run_session
                .record_durably_appended_task_guidance_promotion(candidate.promotion.clone())
            {
                state.session.current = Some(run_session);
                let _ = message_tx.send(WorkerMessage::Notice(format!(
                    "task guidance promotion was durable but live adoption failed: {error:#}"
                )));
                return WorkerAdvancementControl::SkipCommandPoll;
            }
            send_conversation_queue_update(message_tx, run_session.entries());
            if let Err(error) = bind_task_run_cancellation_scope(
                &mut run_session,
                &candidate.promotion.task_id,
                &cancellation.2,
            ) {
                state.session.current = Some(run_session);
                let _ = message_tx.send(WorkerMessage::Notice(format!(
                    "task guidance was promoted but cancellation binding could not be committed: {error}"
                )));
                return WorkerAdvancementControl::SkipCommandPoll;
            }
            let delivered = ConversationInputStatusEntry {
                queue_id: candidate.promotion.queue_id.clone(),
                status: ConversationInputStatus::Delivered,
                reason: Some("task guidance accepted at scheduler safe point".to_owned()),
                updated_at_ms: Some(current_unix_time_ms()),
            };
            if let Err(error) =
                run_session.append_control(ControlEntry::ConversationInputStatusChanged(delivered))
            {
                state.session.current = Some(run_session);
                let _ = message_tx.send(WorkerMessage::Notice(format!(
                    "task guidance was promoted but dispatch could not be committed: {error:#}"
                )));
                return WorkerAdvancementControl::SkipCommandPoll;
            }
            send_conversation_queue_update(message_tx, run_session.entries());
            state.session.last_task_guidance_block = None;
            state
                .session
                .exact_prompts
                .remove(&candidate.promotion.queue_id);
            state.session.task_guidance_dirty = true;
            state.session.conversation_queue_dirty = true;

            let task_id = candidate.promotion.task_id.clone();
            let task_id_value = task_id.as_str().to_owned();
            let _ = message_tx.send(WorkerMessage::TaskRunStarted {
                task_id: task_id_value.clone(),
                objective: sigil_kernel::safe_persistence_text(&task.objective),
            });
            let handler = ChannelEventHandler::new(message_tx.clone());
            let (approval_tx, approval_rx) = mpsc::channel();
            let elicitation_audit_buffer: McpElicitationAuditBuffer =
                Arc::new(std::sync::Mutex::new(Vec::new()));
            elicitation_handler.set_audit_buffer(Some(Arc::clone(&elicitation_audit_buffer)));
            let run_id = state.allocate_run_id();
            let url_capability_registrar = run_session.user_url_capability_registrar();
            let image_attachment_resolver = run_session.image_attachment_resolver();
            let cancellation_target = RunCancellationTarget::Task {
                task_id: task_id_value.clone(),
            };
            let (
                cancellation_owner,
                cancellation_recorder,
                cancellation_handle,
                cancellation_task_guard,
            ) = cancellation;
            let tool_artifact_read_budget = state.session.begin_root_tool_artifact_read_budget();
            if let Err(error) =
                state.acquire_route_execution_owner_for_scope(run_session.session_scope_id())
            {
                state.session.current = Some(run_session);
                let _ = message_tx.send(WorkerMessage::RunFailed(error));
                return WorkerAdvancementControl::SkipCommandPoll;
            }
            let handle = spawn_task_continue(
                runtime,
                TaskContinueSpawn {
                    run_id,
                    session: run_session,
                    task_id,
                    task_id_value,
                    parent_session_ref: task.parent_session_ref,
                    objective: task.objective,
                    guidance: Some(candidate.exact_guidance),
                    guidance_promotion: Some(candidate.promotion),
                    root_config: root_config.clone(),
                    options: options.clone(),
                    base_registry: agent.tool_registry().clone(),
                    agent_supervisor: state.agent.supervisor.clone(),
                    role_provider_builder: Arc::clone(role_provider_builder),
                    task_result_tx: state.run.result_tx.clone(),
                    approval_rx,
                    handler,
                    elicitation_audit_buffer: Arc::clone(&elicitation_audit_buffer),
                    cancellation_handle,
                    cancellation_task_guard,
                    tool_artifact_read_budget,
                },
            );
            state.run.active = Some(ActiveRun {
                run_id,
                handle,
                approval_tx,
                elicitation_audit_buffer,
                cancellation_owner,
                cancellation_recorder,
                cancellation_target,
                url_capability_registrar,
                image_attachment_resolver,
            });
            WorkerAdvancementControl::SkipCommandPoll
        }
    }
}

fn notify_task_guidance_block_once(
    message_tx: &mpsc::Sender<WorkerMessage>,
    state: &mut WorkerLoopState,
    queue_id: ConversationInputQueueId,
    reason: String,
) {
    let block = (queue_id, reason);
    if state.session.last_task_guidance_block.as_ref() != Some(&block) {
        let _ = message_tx.send(WorkerMessage::Notice(format!(
            "task guidance is waiting: {}",
            block.1
        )));
    }
    state.session.last_task_guidance_block = Some(block);
}

fn advance_refreshes<P>(
    context: WorkerAdvancementContext<'_, P>,
    allow_observational_refresh: bool,
) -> WorkerAdvancementControl
where
    P: sigil_kernel::Provider + Send + Sync + 'static,
{
    let WorkerAdvancementContext {
        runtime,
        agent,
        root_config,
        provider_capabilities,
        options,
        message_tx,
        elicitation_handler,
        mcp_event_handler,
        state,
        ..
    } = context;
    let wake_readiness = state.readiness.take_wake_readiness(&state.wake_coalescer);
    if wake_readiness.projection_invalid {
        schedule_projection_reconciliation_after_invalidation(
            &mut state.session.projection_reconciling,
            &mut state.session.projection_retry_at,
            &mut state.session.projection_reconciliation_attempts,
            &mut state.session.projection_reconciliation_latched,
            Instant::now(),
        );
    }
    if wake_readiness.task_guidance_dirty() {
        state.session.task_guidance_dirty = true;
        reset_authority_cas_retry(
            &mut state.session.task_guidance_retry_at,
            &mut state.session.task_guidance_retry_attempts,
            &mut state.session.task_guidance_retry_latched,
        );
    }
    if wake_readiness.conversation_queue_dirty() {
        state.session.conversation_queue_dirty = true;
        reset_authority_cas_retry(
            &mut state.session.conversation_queue_retry_at,
            &mut state.session.conversation_queue_retry_attempts,
            &mut state.session.conversation_queue_retry_latched,
        );
    }
    if wake_readiness.tool_output_pressure_dirty() {
        state.session.tool_output_pressure_dirty = true;
        state.session.artifact_gc_dirty = true;
        state.session.pending_cost_only_tool_output_aging = None;
    }
    if wake_readiness
        .projection_families
        .contains(&ActiveProjectionFamily::AgentContinuation)
    {
        let pending = state
            .session
            .current
            .as_ref()
            .map(pending_agent_continuations_from_active_projection)
            .transpose();
        match pending {
            Ok(Some(pending)) => state.session.pending_agent_result_continuations = pending,
            Ok(None) => state.session.pending_agent_result_continuations.clear(),
            Err(error) => {
                enter_projection_reconciliation(message_tx, state, error);
                return WorkerAdvancementControl::SkipCommandPoll;
            }
        }
    }
    if wake_readiness
        .projection_families
        .contains(&ActiveProjectionFamily::TerminalTask)
    {
        let active = state
            .session
            .current
            .as_ref()
            .map(active_terminal_task_ids_from_active_projection)
            .transpose();
        match active {
            Ok(Some(active)) => state.session.active_terminal_task_ids = active,
            Ok(None) => state.session.active_terminal_task_ids.clear(),
            Err(error) => {
                enter_projection_reconciliation(message_tx, state, error);
                return WorkerAdvancementControl::SkipCommandPoll;
            }
        }
    }
    let now = Instant::now();
    let timer_due = state.readiness.take_timer_due();
    let mut advanced = timer_due || wake_readiness.any;
    if release_due_authority_cas_retry(
        &mut state.session.task_guidance_retry_at,
        &mut state.session.task_guidance_dirty,
        now,
    ) {
        advanced = true;
    }
    if release_due_authority_cas_retry(
        &mut state.session.conversation_queue_retry_at,
        &mut state.session.conversation_queue_dirty,
        now,
    ) {
        advanced = true;
    }
    if allow_observational_refresh {
        let resync_servers = std::mem::take(&mut state.readiness.mcp_resync_servers);
        if !resync_servers.is_empty() {
            state.refresh.pending_mcp_servers.extend(resync_servers);
            state.refresh.next_mcp_retry_at = Instant::now();
            advanced = true;
        }
        while let Some(event) = state.readiness.mcp_runtime_events.pop_front() {
            advanced = true;
            match event {
                McpRuntimeEvent::Progress(notification) => {
                    let _ = message_tx.send(WorkerMessage::McpProgress { notification });
                }
                McpRuntimeEvent::ListChanged(notification) => {
                    state
                        .refresh
                        .pending_mcp_servers
                        .insert(notification.server_name.clone());
                    let _ = message_tx.send(WorkerMessage::McpListChanged { notification });
                }
            }
        }
    }

    if allow_observational_refresh
        && state.run.active.is_none()
        && !state.refresh.pending_mcp_servers.is_empty()
        && Instant::now() >= state.refresh.next_mcp_retry_at
    {
        let shared_registry_blocked = refresh_pending_mcp_servers(
            runtime,
            agent,
            root_config,
            provider_capabilities,
            options,
            message_tx,
            Arc::clone(elicitation_handler),
            Arc::clone(mcp_event_handler),
            state
                .session
                .current
                .as_ref()
                .and_then(Session::mutation_event_recorder),
            state
                .session
                .current
                .as_ref()
                .and_then(|session| session.egress_audit_recorder().ok()),
            &mut state.refresh.pending_mcp_servers,
        );
        state.refresh.next_mcp_retry_at = if shared_registry_blocked {
            Instant::now() + MCP_REFRESH_RETRY_INTERVAL
        } else {
            Instant::now()
        };
        advanced = true;
    }

    advanced |= drain_provider_status_results(
        &mut state.readiness.provider_status_results,
        &mut state.refresh.provider_status_tasks,
        message_tx,
    );
    if advanced {
        WorkerAdvancementControl::SkipCommandPoll
    } else {
        WorkerAdvancementControl::PollCommand
    }
}

fn advance_tool_output_pressure<P>(
    context: WorkerAdvancementContext<'_, P>,
) -> WorkerAdvancementControl
where
    P: sigil_kernel::Provider + Send + Sync + 'static,
{
    let WorkerAdvancementContext {
        message_tx, state, ..
    } = context;
    if state.run.active.is_some() || !state.session.tool_output_pressure_dirty {
        return WorkerAdvancementControl::PollCommand;
    }
    let Some(session) = state.session.current.as_ref() else {
        state.session.tool_output_pressure_dirty = false;
        state.session.artifact_gc_dirty = false;
        state.session.pending_cost_only_tool_output_aging = None;
        return WorkerAdvancementControl::PollCommand;
    };
    let snapshot = match session.active_projection_snapshot() {
        Ok(Some(snapshot)) => snapshot,
        Ok(None) => {
            state.session.tool_output_pressure_dirty = false;
            state.session.pending_cost_only_tool_output_aging = None;
            return WorkerAdvancementControl::PollCommand;
        }
        Err(error) => {
            enter_projection_reconciliation(message_tx, state, error);
            return WorkerAdvancementControl::SkipCommandPoll;
        }
    };
    let pressure = snapshot.tool_output_pressure();
    let candidate = sigil_kernel::ToolOutputAgingBatchV1::select(
        &pressure,
        sigil_kernel::ToolOutputAgingReasonV1::CostOnly,
    )
    .and_then(|batch| {
        batch
            .as_ref()
            .map(|batch| sigil_kernel::ToolOutputAgingActivatedV1::prepare(&pressure, batch))
            .transpose()
    });
    match candidate {
        Ok(candidate) => {
            state.session.tool_output_pressure_dirty = false;
            state.session.artifact_gc_dirty = true;
            if let Some(activation) = candidate {
                if cost_only_tool_output_aging_admitted(session) {
                    match session.append_tool_output_aging_activation(
                        snapshot.frontier(),
                        activation.clone(),
                    ) {
                        Ok(Some(_)) => {
                            state.session.pending_cost_only_tool_output_aging = None;
                            return WorkerAdvancementControl::SkipCommandPoll;
                        }
                        Ok(None) => {
                            state.session.tool_output_pressure_dirty = true;
                            state.session.pending_cost_only_tool_output_aging = None;
                            return WorkerAdvancementControl::SkipCommandPoll;
                        }
                        Err(error) => {
                            enter_projection_reconciliation(message_tx, state, error);
                            return WorkerAdvancementControl::SkipCommandPoll;
                        }
                    }
                }
                state.session.pending_cost_only_tool_output_aging = Some(activation);
            } else {
                state.session.pending_cost_only_tool_output_aging = None;
            }
            WorkerAdvancementControl::SkipCommandPoll
        }
        Err(error) => {
            enter_projection_reconciliation(message_tx, state, error);
            WorkerAdvancementControl::SkipCommandPoll
        }
    }
}

fn cost_only_tool_output_aging_admitted(session: &sigil_kernel::Session) -> bool {
    session.entries().iter().rev().find_map(|entry| {
        let sigil_kernel::SessionLogEntry::Control(ControlEntry::UsageSnapshot(usage)) = entry
        else {
            return None;
        };
        Some(observed_cache_read_tokens(usage))
    }) == Some(Some(0))
}

fn observed_cache_read_tokens(usage: &sigil_kernel::UsageStats) -> Option<u64> {
    if let Some(cache_usage) = usage.cache_usage.as_ref() {
        return cache_usage.read.as_ref().map(|count| count.tokens);
    }
    (usage.prompt_tokens > 0
        && usage
            .cache_hit_tokens
            .saturating_add(usage.cache_miss_tokens)
            == usage.prompt_tokens)
        .then_some(usage.cache_hit_tokens)
}

fn advance_artifact_gc_start<P>(
    context: WorkerAdvancementContext<'_, P>,
) -> WorkerAdvancementControl
where
    P: sigil_kernel::Provider + Send + Sync + 'static,
{
    let WorkerAdvancementContext {
        runtime,
        root_config,
        workspace_root,
        message_tx,
        state,
        ..
    } = context;
    if state.run.active.is_some()
        || state.artifact_gc.tasks.has_active()
        || !state.session.artifact_gc_dirty
        || state.defer_startup_artifact_gc
    {
        return WorkerAdvancementControl::PollCommand;
    }
    let Some(session) = state.session.current.as_ref() else {
        state.session.artifact_gc_dirty = false;
        return WorkerAdvancementControl::PollCommand;
    };
    let snapshot = match session.active_projection_snapshot() {
        Ok(Some(snapshot)) => snapshot,
        Ok(None) => {
            state.session.artifact_gc_dirty = false;
            return WorkerAdvancementControl::PollCommand;
        }
        Err(error) => {
            enter_projection_reconciliation(message_tx, state, error);
            return WorkerAdvancementControl::SkipCommandPoll;
        }
    };
    let pressure = snapshot.tool_output_pressure();
    let roots = pressure.artifact_gc_roots();
    if let Err(error) = roots.validate() {
        enter_projection_reconciliation(message_tx, state, error);
        return WorkerAdvancementControl::SkipCommandPoll;
    }
    let session_scope_id = session.session_scope_id().to_owned();
    let session_ref = match session_ref_for_log_path(&state.session.log_path) {
        Ok(session_ref) => session_ref,
        Err(error) => {
            state.session.artifact_gc_dirty = false;
            let notice = format!("artifact maintenance deferred: {error}");
            if let Some(notice) = state.artifact_gc.changed_deferred_notice(notice) {
                let _ = message_tx.send(WorkerMessage::Notice(notice));
            }
            return WorkerAdvancementControl::SkipCommandPoll;
        }
    };
    let Some(lifecycle) = local_session_lifecycle_service_for_source(
        root_config,
        workspace_root,
        &state.session.log_path,
    ) else {
        state.session.artifact_gc_dirty = false;
        return WorkerAdvancementControl::PollCommand;
    };
    let request_id = state.artifact_gc.next_request_id;
    state.artifact_gc.next_request_id = state.artifact_gc.next_request_id.saturating_add(1);
    state.session.artifact_gc_dirty = false;
    state.artifact_gc.tasks.start(
        runtime,
        request_id,
        session_scope_id,
        Arc::clone(&state.session.attachment_lease),
        pressure.cursor,
        state.artifact_gc.result_tx.clone(),
        lifecycle,
        session_ref,
        roots,
    );
    WorkerAdvancementControl::SkipCommandPoll
}

fn advance_artifact_gc_results<P>(
    context: WorkerAdvancementContext<'_, P>,
) -> WorkerAdvancementControl
where
    P: sigil_kernel::Provider + Send + Sync + 'static,
{
    let WorkerAdvancementContext {
        message_tx, state, ..
    } = context;
    let mut advanced = false;
    while let Some(result) = state.readiness.artifact_gc_results.pop_front() {
        if !state
            .artifact_gc
            .tasks
            .accept_result(result.request_id, &result.session_scope_id)
        {
            continue;
        }
        advanced = true;
        if state
            .session
            .current
            .as_ref()
            .is_none_or(|session| session.session_scope_id() != result.session_scope_id)
        {
            continue;
        }
        match result.result {
            Ok(_) => {
                state.artifact_gc.clear_deferred_notice();
                let current_cursor = match state
                    .session
                    .current
                    .as_ref()
                    .expect("current artifact GC result scope was checked")
                    .active_projection_snapshot()
                {
                    Ok(Some(snapshot)) => snapshot.tool_output_pressure().cursor,
                    Ok(None) => None,
                    Err(error) => {
                        enter_projection_reconciliation(message_tx, state, error);
                        continue;
                    }
                };
                if current_cursor != result.projection_cursor {
                    state.session.artifact_gc_dirty = true;
                }
            }
            Err(error) => {
                let notice =
                    format!("artifact maintenance deferred until the next session change: {error}");
                if let Some(notice) = state.artifact_gc.changed_deferred_notice(notice) {
                    let _ = message_tx.send(WorkerMessage::Notice(notice));
                }
            }
        }
    }
    if advanced {
        WorkerAdvancementControl::SkipCommandPoll
    } else {
        WorkerAdvancementControl::PollCommand
    }
}

pub(in crate::runner) fn pending_agent_continuations_from_active_projection(
    session: &Session,
) -> std::result::Result<Vec<AgentThreadId>, String> {
    let snapshot = session
        .active_projection_snapshot()
        .map_err(|error| format!("{error:#}"))?
        .ok_or_else(|| "agent continuation refresh requires a durable projection".to_owned())?;
    pending_agent_continuations_from_snapshot(session, &snapshot)
}

fn pending_agent_continuations_from_snapshot(
    session: &Session,
    snapshot: &sigil_kernel::session::ActiveSessionProjectionSnapshot,
) -> std::result::Result<Vec<AgentThreadId>, String> {
    if snapshot.pending_agent_continuations_may_be_incomplete() {
        return session
            .try_agent_result_continuation_projection_from_durable()
            .map_err(|error| format!("{error:#}"))?
            .map(|projection| projection.pending_thread_ids)
            .ok_or_else(|| "agent continuation fallback requires a durable session".to_owned());
    }
    Ok(snapshot
        .pending_agent_continuations()
        .iter()
        .cloned()
        .collect::<Vec<_>>())
}

fn active_terminal_task_ids_from_active_projection(
    session: &Session,
) -> std::result::Result<BTreeSet<TerminalTaskId>, String> {
    let snapshot = session
        .active_projection_snapshot()
        .map_err(|error| format!("{error:#}"))?
        .ok_or_else(|| "terminal task refresh requires a durable projection".to_owned())?;
    active_terminal_task_ids_from_snapshot(session, &snapshot)
}

fn active_terminal_task_ids_from_snapshot(
    session: &Session,
    snapshot: &sigil_kernel::session::ActiveSessionProjectionSnapshot,
) -> std::result::Result<BTreeSet<TerminalTaskId>, String> {
    if snapshot.active_terminal_tasks_may_be_incomplete() {
        return session
            .try_terminal_task_projection_from_durable()
            .map_err(|error| format!("{error:#}"))?
            .map(|projection| projection.active_task_ids.into_iter().collect())
            .ok_or_else(|| "terminal task fallback requires a durable session".to_owned());
    }
    Ok(snapshot.active_terminal_tasks().clone())
}

fn advance_compaction_results<P>(
    context: WorkerAdvancementContext<'_, P>,
) -> WorkerAdvancementControl
where
    P: sigil_kernel::Provider + Send + Sync + 'static,
{
    let WorkerAdvancementContext {
        runtime,
        agent,
        root_config,
        workspace_root: _,
        options,
        message_tx,
        elicitation_handler,
        context_resolver: _,
        state,
        ..
    } = context;
    let mut advanced = false;
    while let Some(preparation_result) = state.readiness.compaction_results.pop_front() {
        advanced = true;
        match preparation_result {
            CompactionPreparationTaskResult::Manual {
                request_id,
                session_scope_id,
                result,
            } => {
                if !state
                    .compaction
                    .preparation_tasks
                    .accept_result(request_id, &session_scope_id)
                {
                    continue;
                }
                if let Err(error) = state.acquire_route_execution_owner() {
                    let _ = message_tx
                        .send(WorkerMessage::V2CompactionApplyFailed { request_id, error });
                    continue;
                }
                let Some(session) = state.session.current.as_ref() else {
                    continue;
                };
                if state.run.active.is_some() || session.session_scope_id() != session_scope_id {
                    let _ = message_tx.send(WorkerMessage::Notice(
                        "discarded stale V2 compaction preparation".to_owned(),
                    ));
                    continue;
                }
                match result {
                    Ok(prepared) => {
                        let ManualV2CompactionPreparation {
                            review,
                            local_preview,
                            pending,
                            apply_source,
                        } = *prepared;
                        if local_preview.is_none() && pending.is_none() {
                            let reason = match review.admission {
                                V2CompactionAdmission::Unavailable { reason } => reason,
                                _ => "semantic compaction did not produce an admitted checkpoint"
                                    .to_owned(),
                            };
                            let _ = message_tx.send(WorkerMessage::V2CompactionApplyFailed {
                                request_id,
                                error: reason,
                            });
                            continue;
                        }
                        let effective_config = options.compaction_config.clone();
                        let current_preview =
                            sigil_runtime::context_window::compaction_preview_for_strategy(
                                session,
                                &effective_config,
                            )
                            .ok()
                            .flatten();
                        if current_preview.as_ref() != Some(&review.preview) {
                            let mismatch = current_preview.as_ref().map_or_else(
                                || "current preview is unavailable".to_owned(),
                                |current| {
                                    format!(
                                        "prepared/current cursor={}/{}, folded_match={}, retained_match={}, protected={}/{}, adaptive_match={}, active_compaction_match={}",
                                        review
                                            .preview
                                            .plan
                                            .base_stream_cursor
                                            .last_applied_stream_sequence,
                                        current
                                            .plan
                                            .base_stream_cursor
                                            .last_applied_stream_sequence,
                                        review.preview.plan.folded_event_ids
                                            == current.plan.folded_event_ids,
                                        review.preview.plan.retained_event_ids
                                            == current.plan.retained_event_ids,
                                        review.preview.plan.protected_events.len(),
                                        current.plan.protected_events.len(),
                                        review.preview.plan.adaptive_tail
                                            == current.plan.adaptive_tail,
                                        review.preview.active_compaction_id
                                            == current.active_compaction_id,
                                    )
                                },
                            );
                            let _ = message_tx.send(WorkerMessage::Notice(
                                format!(
                                    "discarded stale V2 compaction preparation after session history changed ({mismatch})"
                                ),
                            ));
                            continue;
                        }
                        if let Some(local_preview) = local_preview {
                            state.compaction.local_preview = Some(local_preview);
                            let _ = message_tx.send(WorkerMessage::V2CompactionPreviewed {
                                state: V2CompactionPreviewState::Review(Box::new(review)),
                            });
                            continue;
                        }
                        let Some(pending) = pending else {
                            let _ = message_tx.send(WorkerMessage::V2CompactionApplyFailed {
                                request_id,
                                error: "semantic compaction did not retain admitted apply material"
                                    .to_owned(),
                            });
                            continue;
                        };
                        let folded_event_count = pending.folded_event_count();
                        match pending.apply_with_optional_native(
                            session,
                            &state.session.log_path,
                            agent.provider(),
                            runtime,
                            root_config.compaction.native_carrier_enabled,
                        ) {
                            Ok((outcome, native_notice)) => {
                                if let Some(notice) = native_notice {
                                    let _ = message_tx.send(WorkerMessage::Notice(notice));
                                }
                                let entries = state
                                    .session
                                    .current
                                    .as_ref()
                                    .map(|current| current.entries().to_vec())
                                    .unwrap_or_default();
                                let _ = message_tx.send(WorkerMessage::V2CompactionApplied {
                                    request_id,
                                    source: apply_source,
                                    compaction_id: outcome.compaction_id,
                                    folded_event_count,
                                    entries,
                                });
                            }
                            Err(error) => {
                                let _ = message_tx.send(WorkerMessage::V2CompactionApplyFailed {
                                    request_id,
                                    error: format!("{error:#}"),
                                });
                            }
                        }
                    }
                    Err(error) => {
                        let _ = message_tx.send(WorkerMessage::RunFailed(format!(
                            "V2 compaction review failed: {error}"
                        )));
                    }
                }
            }
            CompactionPreparationTaskResult::Idle {
                request_id,
                session_scope_id,
                result,
            } => {
                if !state
                    .compaction
                    .preparation_tasks
                    .accept_result(request_id, &session_scope_id)
                {
                    continue;
                }
                let queue_idle = state
                    .session
                    .current
                    .as_ref()
                    .filter(|session| session.session_scope_id() == session_scope_id)
                    .map(active_conversation_queue_is_idle)
                    .transpose();
                let queue_idle = match queue_idle {
                    Ok(Some(queue_idle)) => queue_idle,
                    Ok(None) => false,
                    Err(error) => {
                        enter_projection_reconciliation(message_tx, state, error);
                        continue;
                    }
                };
                let idle_frontier_is_current = queue_idle
                    && state.run.active.is_none()
                    && state.session.pending_agent_result_continuations.is_empty()
                    && state.compaction.pending.is_none();
                if !idle_frontier_is_current {
                    let _ = message_tx.send(WorkerMessage::Notice(
                        "discarded stale automatic compaction preparation".to_owned(),
                    ));
                    continue;
                }
                match result {
                    Ok(prepared) => {
                        state.compaction.idle_auto = prepared.state;
                        if let Err(error) = state.acquire_route_execution_owner() {
                            let _ = message_tx.send(WorkerMessage::Notice(format!(
                                "automatic compaction could not acquire route ownership: {error}"
                            )));
                            continue;
                        }
                        finish_idle_auto_compaction(
                            prepared.preparation,
                            prepared.session,
                            &mut state.session.current,
                            &state.session.log_path,
                            message_tx,
                            agent.provider(),
                            runtime,
                            root_config.compaction.native_carrier_enabled,
                        );
                    }
                    Err(error) => {
                        let _ = message_tx.send(WorkerMessage::Notice(format!(
                            "automatic compaction preflight was not applied: {error}"
                        )));
                    }
                }
            }
            CompactionPreparationTaskResult::PreTurn {
                request_id,
                session_scope_id,
                result,
            } => {
                if !state
                    .compaction
                    .preparation_tasks
                    .accept_result(request_id, &session_scope_id)
                {
                    continue;
                }
                let Some(session) = state.session.current.as_ref() else {
                    continue;
                };
                if state.run.active.is_some() || session.session_scope_id() != session_scope_id {
                    let _ = message_tx.send(WorkerMessage::Notice(
                        "discarded stale queued pre-turn preparation".to_owned(),
                    ));
                    continue;
                }
                let active_snapshot = match session.active_projection_snapshot() {
                    Ok(Some(snapshot)) => snapshot,
                    Ok(None) => {
                        let _ = message_tx.send(WorkerMessage::Notice(
                            "discarded queued pre-turn preparation without a durable active projection"
                                .to_owned(),
                        ));
                        continue;
                    }
                    Err(error) => {
                        enter_projection_reconciliation(message_tx, state, error);
                        continue;
                    }
                };
                match result {
                    Ok(prepared)
                        if active_snapshot.frontier() == &prepared.prepared_frontier
                            && active_snapshot
                                .conversation_queue()
                                .queue
                                .next_dispatchable
                                .as_ref()
                                == Some(&prepared.queue_id) =>
                    {
                        state.session.pending_queued_pre_turn_preparation = Some(*prepared);
                    }
                    Ok(_) => {
                        let _ = message_tx.send(WorkerMessage::Notice(
                            "discarded queued pre-turn preparation after queue frontier changed"
                                .to_owned(),
                        ));
                    }
                    Err(error) => {
                        let _ = message_tx.send(WorkerMessage::Notice(format!(
                                "queued pre-turn admission was not evaluated; queued input was not sent: {error}"
                            )));
                    }
                }
            }
            CompactionPreparationTaskResult::Overflow {
                request_id,
                session_scope_id,
                result,
            } => {
                if !state
                    .compaction
                    .preparation_tasks
                    .accept_result(request_id, &session_scope_id)
                {
                    continue;
                }
                let prepared = match result {
                    Ok(prepared) => prepared,
                    Err(error) => {
                        let _ = message_tx.send(WorkerMessage::RunFailed(format!(
                            "overflow recovery preparation task failed: {error}"
                        )));
                        continue;
                    }
                };
                let source_is_current = state
                    .session
                    .current
                    .as_ref()
                    .filter(|session| {
                        state.run.active.is_none() && session.session_scope_id() == session_scope_id
                    })
                    .and_then(|session| {
                        exact_context_window_rejection_source(
                            session,
                            &prepared.source_logical_run_id,
                        )
                        .ok()
                        .flatten()
                    })
                    .is_some_and(|source| source == prepared.source_physical_attempt_id);
                if !source_is_current {
                    let _ = message_tx.send(WorkerMessage::Notice(
                        "discarded stale overflow recovery preparation".to_owned(),
                    ));
                    let _ = message_tx.send(WorkerMessage::RunFailed(prepared.original_run_error));
                    continue;
                }
                let pending = match prepared.preparation {
                    Ok(pending) => pending,
                    Err(preparation_error) => {
                        let _ = message_tx.send(WorkerMessage::Notice(format!(
                            "overflow recovery is unavailable: {preparation_error}"
                        )));
                        let _ =
                            message_tx.send(WorkerMessage::RunFailed(prepared.original_run_error));
                        continue;
                    }
                };
                let compaction_request_id = pending.request_id();
                let folded_event_count = pending.folded_event_count();
                let frozen_request = pending.frozen_target_request();
                if let Err(error) = state.acquire_route_execution_owner() {
                    let _ = message_tx.send(WorkerMessage::RunFailed(error));
                    continue;
                }
                let applied = state.session.current.as_ref().map(|session| {
                    pending.apply_with_optional_native(
                        session,
                        &state.session.log_path,
                        agent.provider(),
                        runtime,
                        root_config.compaction.native_carrier_enabled,
                    )
                });
                let outcome = match applied {
                    Some(Ok((outcome, native_notice))) => {
                        if let Some(notice) = native_notice {
                            let _ = message_tx.send(WorkerMessage::Notice(notice));
                        }
                        outcome
                    }
                    Some(Err(apply_error)) => {
                        let _ = message_tx.send(WorkerMessage::Notice(format!(
                            "overflow recovery compaction was not applied: {apply_error:#}"
                        )));
                        let _ =
                            message_tx.send(WorkerMessage::RunFailed(prepared.original_run_error));
                        continue;
                    }
                    None => {
                        let _ =
                            message_tx.send(WorkerMessage::RunFailed(prepared.original_run_error));
                        continue;
                    }
                };
                let Some(session) = state.session.current.as_ref() else {
                    let _ = message_tx.send(WorkerMessage::RunFailed(
                        "overflow recovery applied without a loaded session".to_owned(),
                    ));
                    continue;
                };
                let entries = session.entries().to_vec();
                let _ = message_tx.send(WorkerMessage::V2CompactionApplied {
                    request_id: compaction_request_id,
                    source: V2CompactionApplySource::OverflowRecovery,
                    compaction_id: outcome.compaction_id,
                    folded_event_count,
                    entries,
                });
                match start_portable_overflow_recovery_run(
                    runtime,
                    Arc::clone(agent),
                    &state.agent.supervisor,
                    root_config,
                    agent.tool_registry(),
                    options,
                    &state.agent.background_runs,
                    &mut state.session.current,
                    &state.run.result_tx,
                    message_tx,
                    Arc::clone(elicitation_handler),
                    &mut state.run.next_id,
                    &state.terminal_lifecycle_router,
                    frozen_request,
                    format!("overflow-recovery-{}", prepared.source_physical_attempt_id),
                    state.session.tool_artifact_read_budget.clone(),
                ) {
                    Ok(recovery_run) => state.run.active = Some(recovery_run),
                    Err(start_error) => {
                        let _ = message_tx.send(WorkerMessage::Notice(format!(
                            "overflow recovery was applied but its one-shot retry could not start: {start_error:#}"
                        )));
                        let _ =
                            message_tx.send(WorkerMessage::RunFailed(prepared.original_run_error));
                    }
                }
            }
        }
    }
    if advanced {
        WorkerAdvancementControl::SkipCommandPoll
    } else {
        WorkerAdvancementControl::PollCommand
    }
}

fn advance_run_results<P>(context: WorkerAdvancementContext<'_, P>) -> WorkerAdvancementControl
where
    P: sigil_kernel::Provider + Send + Sync + 'static,
{
    let WorkerAdvancementContext {
        runtime,
        agent,
        root_config,
        workspace_root,
        options,
        message_tx,
        elicitation_handler,
        context_resolver,
        state,
        ..
    } = context;
    let mut advanced = false;
    while let Some(mut task_result) = state.readiness.run_results.pop_front() {
        advanced = true;
        if state.run.discarded_ids.remove(&task_result.run_id) {
            continue;
        }
        if state
            .run
            .active
            .as_ref()
            .is_none_or(|active| active.run_id != task_result.run_id)
        {
            let _ = message_tx.send(WorkerMessage::Notice(format!(
                "discarded stale run completion {}",
                task_result.run_id
            )));
            continue;
        }
        elicitation_handler.set_audit_buffer(None);
        state.run.active = None;
        // The completed run returns the authoritative in-memory session for its own appends. Queue
        // controls accepted while that session was detached were already persisted through the
        // same linear writer, so merge only that tracked delta instead of rereading the JSONL.
        task_result
            .session
            .record_durably_appended_controls(state.session.detached_durable_controls.drain(..));
        state.session.current = Some(task_result.session);
        match task_result.payload {
            RunTaskPayload::AwaitingUserInput { request } => {
                let projected = state
                    .session
                    .current
                    .as_ref()
                    .and_then(|session| session.user_input_projection().ok())
                    .and_then(|projection| projection.request(&request.identity).cloned());
                match projected {
                    Some(projected) if projected.requested.request_hash == request.request_hash => {
                        let entries = state
                            .session
                            .current
                            .as_ref()
                            .map(|session| session.entries().to_vec())
                            .unwrap_or_default();
                        let _ = message_tx.send(WorkerMessage::UserInputRequested {
                            request: projected.public_view(),
                            entries,
                        });
                    }
                    _ => {
                        let _ = message_tx.send(WorkerMessage::RunFailed(
                            "run suspended for user input, but its durable request is unavailable"
                                .to_owned(),
                        ));
                    }
                }
            }
            RunTaskPayload::Chat {
                result: Ok(run_result),
                plan_mode,
                plan_review,
                queue_id,
                agent_result_continuation_thread_ids,
                ..
            } => {
                if let Some(queue_id) = queue_id.as_ref() {
                    append_queue_status_and_notify(
                        &mut state.session.current,
                        message_tx,
                        queue_id.clone(),
                        ConversationInputStatus::Delivered,
                        None,
                    );
                }
                if !agent_result_continuation_thread_ids.is_empty() {
                    append_agent_result_continuation_status_and_notify(
                        &mut state.session.current,
                        message_tx,
                        &agent_result_continuation_thread_ids,
                        AgentResultContinuationStatus::Completed,
                        Some("parent continuation completed"),
                    );
                }
                if plan_mode
                    && !plan_review
                    && let Err(error) = append_plan_draft(
                        root_config,
                        workspace_root,
                        &state.session.log_path,
                        &mut state.session.current,
                        &run_result.final_text,
                        run_result.final_message_id.clone(),
                        task_result.run_id,
                    )
                {
                    let _ = message_tx.send(WorkerMessage::Notice(error));
                }
                let entries = state
                    .session
                    .current
                    .as_ref()
                    .map(|session| session.entries().to_vec())
                    .unwrap_or_default();
                let message = if plan_mode || plan_review {
                    WorkerMessage::PlanRunFinished {
                        result: run_result,
                        entries,
                    }
                } else {
                    WorkerMessage::RunFinished {
                        result: run_result,
                        entries,
                    }
                };
                let _ = message_tx.send(message);
                state
                    .compaction
                    .idle_auto
                    .request_after_successful_chat_run();
            }
            RunTaskPayload::Agent {
                profile_id,
                result: Ok(run_result),
            } => {
                let entries = state
                    .session
                    .current
                    .as_ref()
                    .map(|session| session.entries().to_vec())
                    .unwrap_or_default();
                let _ = message_tx.send(WorkerMessage::AgentRunFinished {
                    profile_id,
                    result: run_result,
                    entries,
                });
            }
            RunTaskPayload::Chat {
                result: Err(error),
                plan_mode,
                plan_review: _,
                queue_id,
                provider_logical_run_id,
                agent_result_continuation_thread_ids,
            } => {
                if let Some(queue_id) = queue_id.as_ref() {
                    let classification = state.session.current
                            .as_ref()
                            .ok_or_else(|| {
                                "conversation queue recovery requires a loaded session".to_owned()
                            })
                            .and_then(|session| {
                                let attempts = session
                                    .provider_physical_attempt_projection()
                                    .map_err(|attempt_error| {
                                        format!(
                                            "provider attempt evidence is unavailable: {attempt_error:#}"
                                        )
                                    })?;
                                classify_promoted_queued_conversation(session, &attempts, queue_id)
                            });
                    match classification {
                        Ok(QueuedConversationTerminalClassification::Delivered { reason }) => {
                            append_queue_status_and_notify(
                                    &mut state.session.current,
                                    message_tx,
                                    queue_id.clone(),
                                    ConversationInputStatus::Delivered,
                                    reason.or_else(|| {
                                        Some(
                                            "queued provider attempt reached a terminal after output or side effects"
                                                .to_owned(),
                                        )
                                    }),
                                );
                        }
                        Ok(QueuedConversationTerminalClassification::Rejected { reason }) => {
                            append_queue_failure_and_pause_and_notify(
                                &state.session.log_path,
                                &mut state.session.current,
                                &mut state.session.detached_durable_controls,
                                message_tx,
                                queue_id.clone(),
                                format!("{reason}: {error}"),
                            );
                        }
                        Ok(QueuedConversationTerminalClassification::Stale { reason })
                        | Err(reason) => {
                            append_queue_status_and_notify(
                                &mut state.session.current,
                                message_tx,
                                queue_id.clone(),
                                ConversationInputStatus::Stale,
                                Some(format!("{reason}: {error}")),
                            );
                        }
                    }
                }
                if !agent_result_continuation_thread_ids.is_empty() {
                    append_agent_result_continuation_status_and_notify(
                        &mut state.session.current,
                        message_tx,
                        &agent_result_continuation_thread_ids,
                        AgentResultContinuationStatus::Failed,
                        Some(error.as_str()),
                    );
                }

                let mut overflow_preparation_started = false;
                if queue_id.is_none()
                    && !plan_mode
                    && agent_result_continuation_thread_ids.is_empty()
                    && let Some(logical_run_id) = provider_logical_run_id.as_deref()
                {
                    let source_physical_attempt_id = match state.session.current.as_ref() {
                        Some(session) => {
                            match exact_context_window_rejection_source(session, logical_run_id) {
                                Ok(source_physical_attempt_id) => source_physical_attempt_id,
                                Err(source_error) => {
                                    let _ = message_tx.send(WorkerMessage::Notice(format!(
                                        "overflow recovery evidence is unavailable: {source_error:#}"
                                    )));
                                    None
                                }
                            }
                        }
                        None => None,
                    };
                    if let Some(source_physical_attempt_id) = source_physical_attempt_id {
                        let Some(session) = state.session.current.as_ref() else {
                            let _ = message_tx.send(WorkerMessage::Notice(
                                "overflow recovery requires a loaded session".to_owned(),
                            ));
                            let _ = message_tx.send(WorkerMessage::RunFailed(error));
                            continue;
                        };
                        let request_id = state.compaction.next_request_id;
                        state.compaction.next_request_id =
                            state.compaction.next_request_id.saturating_add(1);
                        let expected_session_scope_id = session.session_scope_id().to_owned();
                        let stable_snapshot = match capture_stable_idle_compaction_snapshot(session)
                        {
                            Ok(Some(snapshot)) => snapshot,
                            Ok(None) => {
                                let _ = message_tx.send(WorkerMessage::Notice(
                                    "overflow recovery requires a stable active-session frontier"
                                        .to_owned(),
                                ));
                                let _ = message_tx.send(WorkerMessage::RunFailed(error));
                                continue;
                            }
                            Err(snapshot_error) => {
                                let _ = message_tx.send(WorkerMessage::Notice(format!(
                                    "overflow recovery source could not be captured: {snapshot_error:#}"
                                )));
                                let _ = message_tx.send(WorkerMessage::RunFailed(error));
                                continue;
                            }
                        };
                        let root_config = root_config.clone();
                        let workspace_root = workspace_root.clone();
                        let session_log_path = state.session.log_path.clone();
                        let options = options.clone();
                        let tools = agent.tool_registry().specs();
                        let runtime_handle = runtime.handle().clone();
                        let overflow_context_resolver = context_resolver.clone();
                        let preparation_agent = Arc::clone(agent);
                        let source_logical_run_id = logical_run_id.to_owned();
                        let original_run_error = error.clone();
                        let start_result = state.compaction.preparation_tasks.start_overflow(
                                runtime,
                                request_id,
                                expected_session_scope_id.clone(),
                                Arc::clone(&state.session.attachment_lease),
                                state.compaction.preparation_tx.clone(),
                                move || {
                                    let preparation = (|| {
                                        let Some(mut session) = stable_snapshot
                                            .materialize_compaction_session()
                                            .map_err(|error| format!("{error:#}"))?
                                        else {
                                            return Err(
                                                "overflow recovery source changed before preparation"
                                                    .to_owned(),
                                            );
                                        };
                                        if session.session_scope_id() != expected_session_scope_id {
                                            return Err(
                                                "overflow recovery preparation loaded a different session scope"
                                                    .to_owned(),
                                            );
                                        }
                                        runtime_handle
                                            .block_on(prepare_overflow_recovery_compaction(
                                                request_id,
                                                &root_config,
                                                &workspace_root,
                                                &session_log_path,
                                                &mut session,
                                                &options,
                                                tools,
                                                source_physical_attempt_id.clone(),
                                                preparation_agent.provider(),
                                                &overflow_context_resolver,
                                            ))
                                            .map_err(|error| format!("{error:#}"))
                                    })();
                                    Ok(OverflowV2CompactionPreparation {
                                        source_physical_attempt_id,
                                        source_logical_run_id,
                                        original_run_error,
                                        preparation,
                                    })
                                },
                            );
                        if let Err(start_error) = start_result {
                            let _ = message_tx.send(WorkerMessage::RunFailed(start_error));
                            continue;
                        }
                        let _ = message_tx.send(WorkerMessage::Notice(
                                "context window was rejected before generation; preparing one owned overflow recovery"
                                    .to_owned(),
                            ));
                        overflow_preparation_started = true;
                    }
                }
                if !overflow_preparation_started {
                    let _ = message_tx.send(WorkerMessage::RunFailed(error));
                }
            }
            RunTaskPayload::Agent {
                result: Err(error), ..
            } => {
                let _ = message_tx.send(WorkerMessage::RunFailed(error));
            }
            RunTaskPayload::Task {
                task_id,
                queue_id,
                result: Ok(status),
            } => {
                if let Some(queue_id) = queue_id {
                    append_queue_status_and_notify(
                        &mut state.session.current,
                        message_tx,
                        queue_id,
                        ConversationInputStatus::Delivered,
                        Some("queued prompt handed off to a durable task".to_owned()),
                    );
                }
                let entries = state
                    .session
                    .current
                    .as_ref()
                    .map(|session| session.entries().to_vec())
                    .unwrap_or_default();
                let _ = message_tx.send(WorkerMessage::TaskRunFinished {
                    task_id,
                    status,
                    entries,
                });
            }
            RunTaskPayload::Task {
                task_id,
                queue_id,
                result: Err(error),
            } => {
                if let Some(queue_id) = queue_id {
                    append_queue_status_and_notify(
                        &mut state.session.current,
                        message_tx,
                        queue_id,
                        ConversationInputStatus::Delivered,
                        Some("queued prompt handed off to a durable task".to_owned()),
                    );
                }
                let entries = state
                    .session
                    .current
                    .as_ref()
                    .map(|session| session.entries().to_vec())
                    .unwrap_or_default();
                let status = TaskId::new(task_id.clone())
                    .ok()
                    .and_then(|task_id| {
                        state.session.current.as_ref().and_then(|session| {
                            session
                                .task_state_projection()
                                .tasks
                                .get(&task_id)
                                .map(|task| task.status)
                        })
                    })
                    .unwrap_or(TaskRunStatus::Failed);
                let _ = message_tx.send(WorkerMessage::TaskRunFinished {
                    task_id,
                    status,
                    entries,
                });
                let _ = message_tx.send(WorkerMessage::RunFailed(error));
            }
        }
    }
    if advanced {
        WorkerAdvancementControl::SkipCommandPoll
    } else {
        WorkerAdvancementControl::PollCommand
    }
}

fn advance_idle_compaction<P>(context: WorkerAdvancementContext<'_, P>) -> WorkerAdvancementControl
where
    P: sigil_kernel::Provider + Send + Sync + 'static,
{
    let WorkerAdvancementContext {
        runtime,
        agent,
        root_config,
        workspace_root,
        options,
        message_tx,
        context_resolver,
        state,
        ..
    } = context;
    if !state.compaction.idle_auto.is_requested() {
        return WorkerAdvancementControl::PollCommand;
    }
    let conversation_queue_is_idle = match state
        .session
        .current
        .as_ref()
        .map(active_conversation_queue_is_idle)
        .transpose()
    {
        Ok(Some(queue_idle)) => queue_idle,
        Ok(None) => true,
        Err(error) => {
            enter_projection_reconciliation(message_tx, state, error);
            return WorkerAdvancementControl::SkipCommandPoll;
        }
    };
    let scheduler_eligibility = IdleAutoCompactionSchedulerEligibility {
        run_active: state.run.active.is_some(),
        conversation_queue_idle: conversation_queue_is_idle,
        pending_agent_result_continuation: !state
            .session
            .pending_agent_result_continuations
            .is_empty(),
        pending_compaction: state.compaction.pending.is_some(),
        preparation_active: state.compaction.preparation_tasks.has_active(),
    };
    let context_capabilities = state
        .session
        .current
        .as_ref()
        .map(|session| agent.provider().context_capabilities(session.model_name()))
        .unwrap_or_else(sigil_kernel::ProviderContextCapabilities::unknown);
    let effective_strategy = match idle_auto_compaction_preflight(
        &state.compaction.idle_auto,
        state.session.current.as_ref(),
        &options.compaction_config,
        &context_capabilities,
        scheduler_eligibility,
    )
    .decision
    {
        IdleAutoCompactionPreflightDecision::NotRequested
        | IdleAutoCompactionPreflightDecision::SchedulerBlocked(_) => {
            return WorkerAdvancementControl::PollCommand;
        }
        IdleAutoCompactionPreflightDecision::NotEligible(_) => {
            state.compaction.idle_auto.cancel_requested_run();
            return WorkerAdvancementControl::PollCommand;
        }
        IdleAutoCompactionPreflightDecision::ProceedToDetailedPreparation {
            effective_strategy,
        } => effective_strategy,
    };
    if let Some(session) = state.session.current.as_ref() {
        let stable_snapshot = match capture_stable_idle_compaction_snapshot(session) {
            Ok(Some(snapshot)) => snapshot,
            Ok(None) => {
                state.compaction.idle_auto.cancel_requested_run();
                let _ = message_tx.send(WorkerMessage::Notice(
                    "automatic compaction preparation was skipped because the live session-entry projection was not stable"
                        .to_owned(),
                ));
                return WorkerAdvancementControl::PollCommand;
            }
            Err(error) => {
                state.compaction.idle_auto.cancel_requested_run();
                let _ = message_tx.send(WorkerMessage::Notice(format!(
                    "automatic compaction preparation could not capture a stable session snapshot: {error:#}"
                )));
                return WorkerAdvancementControl::PollCommand;
            }
        };
        let request_id = state.compaction.next_request_id;
        state.compaction.next_request_id = state.compaction.next_request_id.saturating_add(1);
        let expected_session_scope_id = session.session_scope_id().to_owned();
        let mut root_config = root_config.clone();
        let workspace_root = workspace_root.clone();
        let session_log_path = state.session.log_path.clone();
        let mut options = options.clone();
        root_config.compaction.strategy = effective_strategy;
        options.compaction_config.strategy = effective_strategy;
        let tools = agent.tool_registry().specs();
        let runtime_handle = runtime.handle().clone();
        let idle_context_resolver = context_resolver.clone();
        let preparation_agent = Arc::clone(agent);
        let mut idle_auto_state = state.compaction.idle_auto.clone();
        state.compaction.idle_auto.cancel_requested_run();
        let start_result = state.compaction.preparation_tasks.start_idle(
            runtime,
            request_id,
            expected_session_scope_id.clone(),
            Arc::clone(&state.session.attachment_lease),
            state.compaction.preparation_tx.clone(),
            move || {
                let Some(mut session) = stable_snapshot
                    .materialize_compaction_session()
                    .map_err(|error| format!("{error:#}"))?
                else {
                    return Err(
                        "automatic compaction source snapshot changed before preparation"
                            .to_owned(),
                    );
                };
                if session.session_scope_id() != expected_session_scope_id {
                    return Err(
                        "automatic compaction preparation materialized a different session scope"
                            .to_owned(),
                    );
                }
                let preparation = prepare_idle_auto_compaction(
                    &mut idle_auto_state,
                    &root_config,
                    &workspace_root,
                    &session_log_path,
                    preparation_agent.provider(),
                    &mut session,
                    &options,
                    tools,
                    &idle_context_resolver,
                    &runtime_handle,
                )
                .map_err(|error| format!("{error:#}"));
                if preparation.is_err() {
                    idle_auto_state.cancel_requested_run();
                }
                if capture_stable_idle_compaction_snapshot(&session)
                    .map_err(|error| format!("{error:#}"))?
                    .is_none()
                {
                    return Err(
                        "automatic compaction prepared session no longer matches the durable session-entry projection"
                            .to_owned(),
                    );
                }
                Ok(IdleV2CompactionPreparation {
                    state: idle_auto_state,
                    preparation,
                    session,
                })
            },
        );
        if let Err(error) = start_result {
            let _ = message_tx.send(WorkerMessage::Notice(format!(
                "automatic compaction could not acquire session execution ownership: {error}"
            )));
        }
    }
    WorkerAdvancementControl::PollCommand
}

fn advance_background_agents<P>(
    context: WorkerAdvancementContext<'_, P>,
) -> WorkerAdvancementControl
where
    P: sigil_kernel::Provider + Send + Sync + 'static,
{
    let WorkerAdvancementContext {
        runtime,
        agent,
        root_config,
        options,
        message_tx,
        elicitation_handler,
        state,
        ..
    } = context;
    if state.run.active.is_none() {
        let completed_agent_threads = collect_finished_background_agent_runs(
            runtime,
            &state.agent.background_runs,
            &state.agent.supervisor,
            root_config,
            agent.tool_registry(),
            &mut state.session.current,
            message_tx,
        );
        if !completed_agent_threads.is_empty() {
            let new_continuation_threads = agent_result_continuation_new_thread_ids(
                state.session.current.as_ref(),
                &completed_agent_threads,
            );
            if !new_continuation_threads.is_empty()
                && let Err(error) = append_agent_result_continuation_status_entries(
                    &state.session.log_path,
                    &mut state.session.current,
                    &new_continuation_threads,
                    AgentResultContinuationStatus::Pending,
                    Some("child agent result ready"),
                )
            {
                let _ = message_tx.send(WorkerMessage::RunFailed(error));
                return WorkerAdvancementControl::SkipCommandPoll;
            }
            let (blocking, non_blocking) = partition_agent_result_continuations(
                state.session.current.as_ref(),
                completed_agent_threads,
            );
            extend_agent_thread_ids_unique(
                &mut state.session.pending_agent_result_continuations,
                non_blocking,
            );
            let queued_input_ready = match state
                .session
                .current
                .as_ref()
                .map(active_next_dispatchable_queue_id)
                .transpose()
            {
                Ok(queue_id) => queue_id.flatten().is_some(),
                Err(error) => {
                    enter_projection_reconciliation(message_tx, state, error);
                    return WorkerAdvancementControl::SkipCommandPoll;
                }
            };
            let mut continuation_threads = blocking;
            if !queued_input_ready {
                continuation_threads.append(&mut state.session.pending_agent_result_continuations);
            }
            if continuation_threads.is_empty() {
                return WorkerAdvancementControl::SkipCommandPoll;
            }
            state.run.active = start_agent_result_continuation_run(
                runtime,
                Arc::clone(agent),
                &state.agent.supervisor,
                root_config,
                &state.session.log_path,
                agent.tool_registry(),
                options,
                &state.agent.background_runs,
                &mut state.session.current,
                &state.run.result_tx,
                message_tx,
                Arc::clone(elicitation_handler),
                &mut state.run.next_id,
                &state.terminal_lifecycle_router,
                state.session.tool_artifact_read_budget.clone(),
                continuation_threads,
            );
            if state.run.active.is_some() {
                return WorkerAdvancementControl::SkipCommandPoll;
            }
        }
    }

    WorkerAdvancementControl::PollCommand
}

fn advance_pending_agent_continuations<P>(
    context: WorkerAdvancementContext<'_, P>,
) -> WorkerAdvancementControl
where
    P: sigil_kernel::Provider + Send + Sync + 'static,
{
    let WorkerAdvancementContext {
        runtime,
        agent,
        root_config,
        options,
        message_tx,
        elicitation_handler,
        state,
        ..
    } = context;
    if state.run.active.is_none() {
        let queued_input_ready = match state
            .session
            .current
            .as_ref()
            .map(active_next_dispatchable_queue_id)
            .transpose()
        {
            Ok(queue_id) => queue_id.flatten().is_some(),
            Err(error) => {
                enter_projection_reconciliation(message_tx, state, error);
                return WorkerAdvancementControl::SkipCommandPoll;
            }
        };
        if !queued_input_ready && !state.session.pending_agent_result_continuations.is_empty() {
            let continuation_threads =
                std::mem::take(&mut state.session.pending_agent_result_continuations);
            state.run.active = start_agent_result_continuation_run(
                runtime,
                Arc::clone(agent),
                &state.agent.supervisor,
                root_config,
                &state.session.log_path,
                agent.tool_registry(),
                options,
                &state.agent.background_runs,
                &mut state.session.current,
                &state.run.result_tx,
                message_tx,
                Arc::clone(elicitation_handler),
                &mut state.run.next_id,
                &state.terminal_lifecycle_router,
                state.session.tool_artifact_read_budget.clone(),
                continuation_threads,
            );
            if state.run.active.is_some() {
                return WorkerAdvancementControl::SkipCommandPoll;
            }
        }
    }

    WorkerAdvancementControl::PollCommand
}

fn advance_conversation_queue<P>(
    context: WorkerAdvancementContext<'_, P>,
) -> WorkerAdvancementControl
where
    P: sigil_kernel::Provider + Send + Sync + 'static,
{
    let WorkerAdvancementContext {
        runtime,
        agent,
        root_config,
        workspace_root,
        options,
        message_tx,
        elicitation_handler,
        role_provider_builder,
        context_resolver,
        state,
        ..
    } = context;
    if state.run.active.is_none() {
        if !state.session.conversation_queue_dirty
            && state.session.pending_queued_pre_turn_preparation.is_none()
        {
            return WorkerAdvancementControl::PollCommand;
        }
        let next_queue_id = if state.session.conversation_queue_dirty {
            state.session.conversation_queue_dirty = false;
            state.session.conversation_queue_retry_at = None;
            match state.session.current.as_ref() {
                Some(session) => match session.active_projection_snapshot() {
                    Ok(Some(snapshot)) => snapshot
                        .conversation_queue()
                        .queue
                        .next_dispatchable
                        .clone(),
                    Ok(None) => None,
                    Err(error) => {
                        enter_projection_reconciliation(message_tx, state, error);
                        None
                    }
                },
                None => None,
            }
        } else {
            None
        };
        if state.session.pending_queued_pre_turn_preparation.is_none()
            && !state.compaction.preparation_tasks.has_active()
            && let Some(queue_id) = next_queue_id.clone()
            && let Some(session) = state.session.current.as_ref()
        {
            let stable_snapshot = match capture_stable_idle_compaction_snapshot(session) {
                Ok(Some(snapshot)) => snapshot,
                Ok(None) => {
                    let _ = message_tx.send(WorkerMessage::Notice(
                        "queued pre-turn preparation requires a stable durable session projection"
                            .to_owned(),
                    ));
                    return WorkerAdvancementControl::PollCommand;
                }
                Err(error) => {
                    enter_projection_reconciliation(message_tx, state, error);
                    return WorkerAdvancementControl::SkipCommandPoll;
                }
            };
            let request_id = state.compaction.next_request_id;
            state.compaction.next_request_id = state.compaction.next_request_id.saturating_add(1);
            let expected_session_scope_id = session.session_scope_id().to_owned();
            let root_config = root_config.clone();
            let workspace_root = workspace_root.clone();
            let session_log_path = state.session.log_path.clone();
            let exact_prompts = state.session.exact_prompts.clone();
            let options = options.clone();
            let conversation_coordinator = ConversationCoordinator::new(
                root_config.task.enabled,
                root_config.task.routing_policy,
            )
            .with_writable_memory_routing(root_config.memory.writable)
            .with_orchestration_route_guard(sigil_runtime::OrchestrationRouteGuard::new(
                &root_config.agent.runtime_provider,
                &root_config.agent.model,
                sigil_runtime::ORCHESTRATION_RUNTIME_BUILD_ID,
            ))
            .with_route_capability_evidence(sigil_runtime::RouteCapabilityEvidence {
                provider_supports_routing_tools: agent.provider_capabilities().supports_tool_stream,
                route_qualified: sigil_runtime::route_qualification_evidence(&root_config),
            });
            let route_capability = conversation_coordinator.resolve_route_capability(session);
            let tools = if route_capability.routes_automatically() {
                conversation_coordinator.route_tool_specs_for_session(session, route_capability)
            } else {
                agent.tool_registry().specs()
            };
            let runtime_handle = runtime.handle().clone();
            let queue_context_resolver = context_resolver.clone();
            let preparation_agent = Arc::clone(agent);
            let start_result = state.compaction.preparation_tasks.start_pre_turn(
                runtime,
                request_id,
                expected_session_scope_id.clone(),
                Arc::clone(&state.session.attachment_lease),
                state.compaction.preparation_tx.clone(),
                move || {
                    let Some(mut session) = stable_snapshot
                        .materialize_compaction_session()
                        .map_err(|error| format!("{error:#}"))?
                    else {
                        return Err(
                            "queued pre-turn source snapshot changed before preparation".to_owned()
                        );
                    };
                    if session.session_scope_id() != expected_session_scope_id {
                        return Err(
                            "queued pre-turn preparation materialized a different session scope"
                                .to_owned(),
                        );
                    }
                    let snapshot = session
                        .active_projection_snapshot()
                        .map_err(|error| {
                            format!("failed to read queued pre-turn active projection: {error:#}")
                        })?
                        .ok_or_else(|| {
                            "queued pre-turn preparation requires a durable active projection"
                                .to_owned()
                        })?;
                    if snapshot
                        .conversation_queue()
                        .queue
                        .next_dispatchable
                        .as_ref()
                        != Some(&queue_id)
                    {
                        return Err(
                            "queued pre-turn preparation loaded a different queue frontier"
                                .to_owned(),
                        );
                    }
                    let admission = prepare_next_queued_conversation_pre_turn_admission(
                        &root_config,
                        &workspace_root,
                        &session_log_path,
                        preparation_agent.provider(),
                        &mut session,
                        &exact_prompts,
                        &options.memory_config,
                        tools,
                        options.reasoning_effort.clone(),
                        options.traffic_partition_key.clone(),
                        &queue_context_resolver,
                        &runtime_handle,
                    )
                    .map_err(|error| format!("{error:#}"))?;
                    let prepared_frontier = session
                        .active_projection_snapshot()
                        .map_err(|error| {
                            format!("failed to read prepared queued pre-turn projection: {error:#}")
                        })?
                        .ok_or_else(|| {
                            "queued pre-turn preparation lost its durable active projection"
                                .to_owned()
                        })?
                        .frontier()
                        .clone();
                    Ok(PreTurnV2CompactionPreparation {
                        queue_id,
                        admission,
                        session: Some(session),
                        prepared_frontier,
                    })
                },
            );
            if let Err(error) = start_result {
                let _ = message_tx.send(WorkerMessage::Notice(format!(
                    "queued pre-turn compaction could not acquire session execution ownership: {error}"
                )));
            }
        }

        let mut pending_preparation = state.session.pending_queued_pre_turn_preparation.take();
        if let Some(prepared) = pending_preparation.as_mut() {
            let Some(prepared_session) = prepared.session.take() else {
                let _ = message_tx.send(WorkerMessage::Notice(
                    "queued pre-turn preparation lost its stable session projection".to_owned(),
                ));
                return WorkerAdvancementControl::SkipCommandPoll;
            };
            state.session.current = Some(prepared_session);
        }
        let candidate = match pending_preparation {
            None => {
                if next_queue_id.is_none() {
                    state.session.last_queued_pre_turn_block = None;
                }
                None
            }
            Some(PreTurnV2CompactionPreparation {
                admission: QueuedConversationPreTurnAdmission::NoQueuedInput,
                ..
            }) => {
                state.session.last_queued_pre_turn_block = None;
                None
            }
            Some(PreTurnV2CompactionPreparation {
                admission:
                    QueuedConversationPreTurnAdmission::Blocked {
                        queue_id,
                        reason,
                        candidate,
                    },
                ..
            }) => match candidate {
                Some(candidate) => {
                    let notice = format!(
                        "queued pre-turn compaction is unavailable ({reason}); dispatching the unchanged frozen request"
                    );
                    let block = (queue_id, notice.clone());
                    if state.session.last_queued_pre_turn_block.as_ref() != Some(&block) {
                        let _ = message_tx.send(WorkerMessage::Notice(notice));
                    }
                    state.session.last_queued_pre_turn_block = Some(block);
                    Some(*candidate)
                }
                None => {
                    let block = (queue_id, reason);
                    if state.session.last_queued_pre_turn_block.as_ref() != Some(&block) {
                        let _ = message_tx.send(WorkerMessage::Notice(format!(
                            "queued follow-up is waiting for a local pre-turn admission: {}",
                            block.1
                        )));
                    }
                    state.session.last_queued_pre_turn_block = Some(block);
                    None
                }
            },
            Some(PreTurnV2CompactionPreparation {
                admission: QueuedConversationPreTurnAdmission::ExactFit(admitted),
                ..
            }) => {
                state.session.last_queued_pre_turn_block = None;
                Some(admitted.candidate)
            }
            Some(PreTurnV2CompactionPreparation {
                admission: QueuedConversationPreTurnAdmission::PortablePreflightReady(pending),
                ..
            }) => {
                if let Err(error) = state.acquire_route_execution_owner() {
                    let _ = message_tx.send(WorkerMessage::RunFailed(error));
                    return WorkerAdvancementControl::SkipCommandPoll;
                }
                let Some(session) = state.session.current.as_ref() else {
                    return WorkerAdvancementControl::SkipCommandPoll;
                };
                let folded_event_count = pending.folded_event_count();
                match pending.apply_compaction(
                    session,
                    &state.session.log_path,
                    agent.provider(),
                    runtime,
                    root_config.compaction.native_carrier_enabled,
                ) {
                    Ok((candidate, outcome, native_notice)) => {
                        if let Some(notice) = native_notice {
                            let _ = message_tx.send(WorkerMessage::Notice(notice));
                        }
                        let entries = session.entries().to_vec();
                        state.session.last_queued_pre_turn_block = None;
                        let _ = message_tx.send(WorkerMessage::V2CompactionApplied {
                            request_id: 0,
                            source: V2CompactionApplySource::PreTurnPressure,
                            compaction_id: outcome.compaction_id,
                            folded_event_count,
                            entries,
                        });
                        Some(candidate)
                    }
                    Err(error) => {
                        let _ = message_tx.send(WorkerMessage::Notice(format!(
                                "queued pre-turn compaction was not applied; queued input was not sent: {error:#}"
                            )));
                        None
                    }
                }
            }
        };
        if let Some(candidate) = candidate {
            let queue_id = candidate.promotion.queue_id.clone();
            let committed = match state.session.current.as_mut() {
                Some(session) => commit_prepared_queued_conversation_candidate(
                    &state.session.log_path,
                    session,
                    candidate,
                ),
                None => Err("session state is unavailable for queued promotion".to_owned()),
            };
            match committed {
                Ok(candidate) => {
                    state.session.exact_prompts.remove(&queue_id);
                    state.session.task_guidance_dirty = true;
                    state.session.conversation_queue_dirty = true;
                    let tool_artifact_read_budget =
                        state.session.begin_root_tool_artifact_read_budget();
                    if let Some(session) = state.session.current.as_ref() {
                        send_conversation_queue_update(message_tx, session.entries());
                    }
                    state.run.active = start_queued_conversation_run(
                        runtime,
                        Arc::clone(agent),
                        &state.agent.supervisor,
                        root_config,
                        agent.tool_registry(),
                        options,
                        &state.agent.background_runs,
                        &mut state.session.current,
                        &state.run.result_tx,
                        message_tx,
                        Arc::clone(elicitation_handler),
                        Arc::clone(role_provider_builder),
                        &state.session.log_path,
                        &mut state.run.next_id,
                        &state.terminal_lifecycle_router,
                        tool_artifact_read_budget,
                        candidate,
                    );
                }
                Err(error) => {
                    if !arm_authority_cas_retry(
                        &mut state.session.conversation_queue_retry_at,
                        &mut state.session.conversation_queue_retry_attempts,
                        &mut state.session.conversation_queue_retry_latched,
                        Instant::now(),
                    ) {
                        let _ = message_tx.send(WorkerMessage::Notice(
                            "conversation queue authority retry latched after 6 failures; a new relevant durable event or session reload is required".to_owned(),
                        ));
                    }
                    let _ = message_tx.send(WorkerMessage::Notice(format!(
                        "queued promotion was not dispatched: {error}"
                    )));
                }
            }
        }
        if state.run.active.is_some() {
            return WorkerAdvancementControl::SkipCommandPoll;
        }
    }
    WorkerAdvancementControl::PollCommand
}

#[cfg(test)]
mod authority_retry_tests {
    use super::*;

    #[test]
    fn transient_authority_failure_rearms_only_after_the_deadline() {
        let now = Instant::now();
        let mut retry_at = None;
        let mut attempts = 0;
        let mut latched = false;
        let mut dirty = false;
        assert!(arm_authority_cas_retry(
            &mut retry_at,
            &mut attempts,
            &mut latched,
            now
        ));
        let first_deadline = retry_at.expect("first retry is armed");

        assert!(!release_due_authority_cas_retry(
            &mut retry_at,
            &mut dirty,
            now
        ));
        assert!(!dirty, "retry must not form an immediate tight loop");
        assert!(release_due_authority_cas_retry(
            &mut retry_at,
            &mut dirty,
            first_deadline
        ));
        assert!(dirty, "fresh preparation is rearmed at the event deadline");
        assert!(retry_at.is_none());
        assert_eq!(attempts, 1);
        assert!(!latched);
    }

    #[test]
    fn repeated_authority_failures_back_off_and_latch() {
        let now = Instant::now();
        let mut retry_at = None;
        let mut attempts = 0;
        let mut latched = false;
        let mut delays = Vec::new();
        for _ in 1..AUTHORITY_CAS_RETRY_MAX_ATTEMPTS {
            assert!(arm_authority_cas_retry(
                &mut retry_at,
                &mut attempts,
                &mut latched,
                now
            ));
            delays.push(
                retry_at
                    .expect("bounded retry is armed")
                    .saturating_duration_since(now),
            );
        }
        assert!(delays.windows(2).all(|pair| pair[1] >= pair[0]));
        assert!(!arm_authority_cas_retry(
            &mut retry_at,
            &mut attempts,
            &mut latched,
            now
        ));
        assert!(latched);
        assert!(retry_at.is_none());

        reset_authority_cas_retry(&mut retry_at, &mut attempts, &mut latched);
        assert_eq!(attempts, 0);
        assert!(!latched);
    }
}

#[cfg(test)]
mod projection_reconciliation_tests {
    use super::*;

    #[test]
    fn repeated_invalid_notice_preserves_future_reconciliation_deadline() {
        let now = Instant::now();
        let future = now + PROJECTION_RECONCILIATION_BASE_BACKOFF;
        let mut reconciling = true;
        let mut retry_at = Some(future);
        let mut attempts = 2;
        let mut latched = false;

        schedule_projection_reconciliation_after_invalidation(
            &mut reconciling,
            &mut retry_at,
            &mut attempts,
            &mut latched,
            now,
        );

        assert!(reconciling);
        assert_eq!(retry_at, Some(future));
        assert_eq!(attempts, 2);
        assert!(!latched);
    }

    #[test]
    fn first_invalid_notice_schedules_immediate_reconciliation() {
        let now = Instant::now();
        let mut reconciling = false;
        let mut retry_at = None;
        let mut attempts = 5;
        let mut latched = true;

        schedule_projection_reconciliation_after_invalidation(
            &mut reconciling,
            &mut retry_at,
            &mut attempts,
            &mut latched,
            now,
        );

        assert!(reconciling);
        assert_eq!(retry_at, Some(now));
        assert_eq!(attempts, 0);
        assert!(!latched);
    }

    #[test]
    fn invalid_notice_does_not_unlatch_exhausted_reconciliation() {
        let now = Instant::now();
        let mut reconciling = true;
        let mut retry_at = None;
        let mut attempts = PROJECTION_RECONCILIATION_MAX_ATTEMPTS;
        let mut latched = true;

        schedule_projection_reconciliation_after_invalidation(
            &mut reconciling,
            &mut retry_at,
            &mut attempts,
            &mut latched,
            now,
        );

        assert!(reconciling);
        assert_eq!(retry_at, None);
        assert_eq!(attempts, PROJECTION_RECONCILIATION_MAX_ATTEMPTS);
        assert!(latched);
    }

    #[test]
    fn reconciliation_backoff_is_exponential_and_bounded_with_jitter() {
        for attempt in 1..PROJECTION_RECONCILIATION_MAX_ATTEMPTS {
            let delay = projection_reconciliation_backoff(attempt);
            let exponent = u32::from(attempt.saturating_sub(1).min(5));
            let base = PROJECTION_RECONCILIATION_BASE_BACKOFF
                .saturating_mul(1_u32 << exponent)
                .min(PROJECTION_RECONCILIATION_MAX_BACKOFF);
            assert!(delay >= base);
            assert!(delay <= base + base / 5 + Duration::from_millis(1));
        }
    }

    #[test]
    fn reconciliation_failure_budget_latches_without_another_deadline() {
        let mut attempts = 0;
        for _ in 1..PROJECTION_RECONCILIATION_MAX_ATTEMPTS {
            assert!(matches!(
                record_projection_reconciliation_failure(&mut attempts),
                ProjectionReconciliationFailureDisposition::RetryAfter(_)
            ));
        }
        assert_eq!(
            record_projection_reconciliation_failure(&mut attempts),
            ProjectionReconciliationFailureDisposition::Latched
        );
        assert_eq!(attempts, PROJECTION_RECONCILIATION_MAX_ATTEMPTS);
    }
}

#[cfg(test)]
mod tool_output_aging_admission_tests {
    use super::*;

    #[test]
    fn cost_only_aging_requires_observed_cache_miss_evidence() {
        let unknown = sigil_kernel::UsageStats {
            prompt_tokens: 240_000,
            ..sigil_kernel::UsageStats::default()
        };
        assert_eq!(observed_cache_read_tokens(&unknown), None);

        let cache_hit = sigil_kernel::UsageStats {
            prompt_tokens: 240_000,
            cache_hit_tokens: 226_000,
            cache_miss_tokens: 14_000,
            ..sigil_kernel::UsageStats::default()
        };
        assert_eq!(observed_cache_read_tokens(&cache_hit), Some(226_000));

        let cache_miss = sigil_kernel::UsageStats {
            prompt_tokens: 240_000,
            cache_miss_tokens: 240_000,
            ..sigil_kernel::UsageStats::default()
        };
        assert_eq!(observed_cache_read_tokens(&cache_miss), Some(0));
    }
}

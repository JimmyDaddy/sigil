use super::*;
use crate::runner::ManagedTuiArtifactStoreLease;

const MAX_EVENT_DRAIN: usize = 64;
static WORKER_REACTOR_EVENT_WAKE_TOTAL: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);
static WORKER_REACTOR_DEADLINE_TOTAL: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);
static WORKER_REACTOR_ADVANCEMENT_TOTAL: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct WorkerReactorMetricsSnapshot {
    pub event_wake_total: u64,
    pub deadline_total: u64,
    pub advancement_total: u64,
}

impl WorkerReactorMetricsSnapshot {
    #[must_use]
    pub fn saturating_delta(self, earlier: Self) -> Self {
        Self {
            event_wake_total: self
                .event_wake_total
                .saturating_sub(earlier.event_wake_total),
            deadline_total: self.deadline_total.saturating_sub(earlier.deadline_total),
            advancement_total: self
                .advancement_total
                .saturating_sub(earlier.advancement_total),
        }
    }
}

#[must_use]
pub fn worker_reactor_metrics() -> WorkerReactorMetricsSnapshot {
    use std::sync::atomic::Ordering;

    WorkerReactorMetricsSnapshot {
        event_wake_total: WORKER_REACTOR_EVENT_WAKE_TOTAL.load(Ordering::Relaxed),
        deadline_total: WORKER_REACTOR_DEADLINE_TOTAL.load(Ordering::Relaxed),
        advancement_total: WORKER_REACTOR_ADVANCEMENT_TOTAL.load(Ordering::Relaxed),
    }
}

fn record_worker_advancement() {
    WORKER_REACTOR_ADVANCEMENT_TOTAL.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
}

pub(in crate::runner) struct WorkerLoopTerminalRuntime {
    lifecycle_router: ChannelTerminalLifecycleRouter,
    control: Option<sigil_tools_builtin::TerminalTaskControlHandle>,
    /// Session-scoped scratch lease registry shared with bash/terminal tools; used by startup
    /// TTL GC and session-delete cleanup so live namespaces are never reclaimed.
    scratch_control: Option<sigil_tools_builtin::ScratchNamespaceControl>,
}

impl WorkerLoopTerminalRuntime {
    pub(in crate::runner) fn new(
        lifecycle_router: ChannelTerminalLifecycleRouter,
        control: Option<sigil_tools_builtin::TerminalTaskControlHandle>,
    ) -> Self {
        Self {
            lifecycle_router,
            control,
            scratch_control: None,
        }
    }

    pub(in crate::runner) fn with_scratch_control(
        mut self,
        scratch_control: sigil_tools_builtin::ScratchNamespaceControl,
    ) -> Self {
        self.scratch_control = Some(scratch_control);
        self
    }
}

pub(in crate::runner) struct WorkerLoopSessionAttachment {
    pub(in crate::runner) log_path: PathBuf,
    pub(in crate::runner) lease:
        Arc<sigil_runtime::interactive_session_attachment::InteractiveSessionAttachmentLease>,
}

impl WorkerLoopSessionAttachment {
    #[cfg(test)]
    pub(in crate::runner) fn new(
        log_path: PathBuf,
        lease: sigil_runtime::interactive_session_attachment::InteractiveSessionAttachmentLease,
    ) -> Self {
        Self {
            log_path,
            lease: Arc::new(lease),
        }
    }

    pub(in crate::runner) fn from_shared(
        log_path: PathBuf,
        lease: Arc<
            sigil_runtime::interactive_session_attachment::InteractiveSessionAttachmentLease,
        >,
    ) -> Self {
        Self { log_path, lease }
    }
}

#[allow(clippy::too_many_arguments)] // Worker-loop entry: one context bundle per surface.
pub(in crate::runner) fn run_worker_loop<P>(
    runtime: tokio::runtime::Runtime,
    mut agent: Arc<Agent<P>>,
    root_config: RootConfig,
    config_path: PathBuf,
    workspace_root: PathBuf,
    session_attachment: WorkerLoopSessionAttachment,
    options: AgentRunOptions,
    permission_mode_override: std::sync::Arc<sigil_kernel::PermissionModeOverride>,
    event_inbox: WorkerEventInbox,
    message_tx: mpsc::Sender<WorkerMessage>,
    mcp_handlers: WorkerLoopMcpHandlers,
    terminal_runtime: WorkerLoopTerminalRuntime,
    managed_storage_writer: Option<
        std::sync::Arc<sigil_runtime::managed_storage_writer::ManagedStorageWriterAdapterV1>,
    >,
    managed_artifact_store: Option<ManagedTuiArtifactStoreLease>,
) where
    P: sigil_kernel::Provider + Send + Sync + 'static,
{
    let WorkerLoopSessionAttachment {
        log_path: session_log_path,
        lease: attachment_lease,
    } = session_attachment;
    let provider_capabilities = agent.provider_capabilities();
    let (event_tx, event_rx, urgent_command_rx) = event_inbox;
    let WorkerLoopMcpHandlers {
        elicitation_handler,
        event_handler: mcp_event_handler,
        role_provider_builder,
        context_resolver,
    } = mcp_handlers;
    let WorkerLoopTerminalRuntime {
        lifecycle_router: terminal_lifecycle_router,
        control: terminal_control,
        scratch_control,
    } = terminal_runtime;
    let initial_exact_conversation_prompts = ExactConversationPromptStore::new();
    let attachment_paths = sigil_runtime::resolve_sigil_paths(
        &root_config.storage,
        &root_config.session,
        &workspace_root,
    );
    let default_image_attachment_resolver: Arc<dyn ImageAttachmentResolver> = Arc::new(
        sigil_runtime::ControlledImageAttachmentCache::new(attachment_paths.attachments_root),
    );
    let mut initial_session = match load_session_with_runtime_attachments(
        &root_config.agent.runtime_provider,
        &root_config.agent.model,
        &session_log_path,
        None,
    ) {
        Ok(mut session) => {
            if let Some(artifact_store) = managed_artifact_store.as_ref() {
                session.attach_tool_artifact_store_override(artifact_store.store());
            }
            if let Err(error) = session.try_attach_image_attachment_resolver(Arc::clone(
                &default_image_attachment_resolver,
            )) {
                let _ = message_tx.send(WorkerMessage::RunFailed(format!(
                    "failed to attach image cache resolver: {error:#}"
                )));
                return;
            }
            match runtime.block_on(
                sigil_runtime::isolated_workspace::reconcile_isolated_workspace_cleanup(
                    &mut session,
                    &workspace_root,
                ),
            ) {
                Ok(report) if report.inspected > 0 => {
                    let _ = message_tx.send(WorkerMessage::Notice(format!(
                        "reconciled {} isolated task workspace(s): {} removed, {} already missing, {} require review",
                        report.inspected, report.removed, report.already_missing, report.failed
                    )));
                }
                Ok(_) => {}
                Err(error) => {
                    let _ = message_tx.send(WorkerMessage::RunFailed(format!(
                        "failed to reconcile isolated task workspaces: {error:#}"
                    )));
                    return;
                }
            }
            match runtime.block_on(
                sigil_runtime::integration_lanes::reconcile_integration_promotions(
                    &mut session,
                    &workspace_root,
                ),
            ) {
                Ok(report) if report.inspected > 0 => {
                    let _ = message_tx.send(WorkerMessage::Notice(format!(
                        "reconciled {} interrupted integration promotion(s): {} promoted, {} cancelled, {} failed, {} require review",
                        report.inspected,
                        report.promoted,
                        report.cancelled,
                        report.failed,
                        report.needs_review
                    )));
                }
                Ok(_) => {}
                Err(error) => {
                    let _ = message_tx.send(WorkerMessage::RunFailed(format!(
                        "failed to reconcile integration promotions: {error:#}"
                    )));
                    return;
                }
            }
            mark_stale_dispatching_conversation_queue_items(
                &mut session,
                &initial_exact_conversation_prompts,
                &message_tx,
            );
            Some(session)
        }
        Err(error) => {
            let _ = message_tx.send(WorkerMessage::RunFailed(format!("{error:#}")));
            return;
        }
    };
    let pending_task_handoffs = match initial_session.as_mut() {
        Some(session) => {
            session_ref_for_log_path(&session_log_path).and_then(|parent_session_ref| {
                ConversationCoordinator::new(
                    root_config.task.enabled,
                    root_config.task.routing_policy,
                )
                .reconcile(session, &parent_session_ref, current_unix_time_ms())
                .map_err(|error| format!("failed to reconcile durable task handoffs: {error:#}"))
            })
        }
        None => Ok(Vec::new()),
    };
    let pending_task_handoffs = match pending_task_handoffs {
        Ok(actions) => actions,
        Err(error) => {
            let _ = message_tx.send(WorkerMessage::RunFailed(error));
            return;
        }
    };

    let session_entries = initial_session
        .as_ref()
        .map(Session::entries)
        .unwrap_or(&[]);
    let wake_coalescer = WorkerWakeCoalescer::new(
        event_tx.clone(),
        initial_session
            .as_ref()
            .map(|session| session.session_scope_id().to_owned()),
    );
    let agent_supervisor =
        match sigil_runtime::AgentProfileRegistry::from_root_config_with_workspace_and_entries(
            &root_config,
            &workspace_root,
            session_entries,
        ) {
            Ok(registry) => sigil_runtime::AgentSupervisor::new(
                registry,
                sigil_runtime::AgentBudgetPolicy::from_root_config(&root_config),
                provider_capabilities.clone(),
            )
            .with_event_sink(Arc::new(WorkerSupervisorEventSink {
                wake_coalescer: wake_coalescer.clone(),
            })),
            Err(error) => {
                let _ = message_tx.send(WorkerMessage::RunFailed(format!("{error:#}")));
                return;
            }
        };
    let background_agent_runs =
        sigil_runtime::AgentToolBackgroundRuns::with_event_sink(Arc::new(WorkerAgentEventSink {
            sender: message_tx.clone(),
            wake_coalescer: wake_coalescer.clone(),
        }));
    let mut state = WorkerLoopState::new(
        session_log_path,
        initial_session,
        attachment_lease,
        agent_supervisor,
        background_agent_runs,
        event_tx,
        wake_coalescer,
        terminal_lifecycle_router,
        terminal_control,
        scratch_control.clone(),
        managed_storage_writer,
        managed_artifact_store,
    );
    // RFC-0062 14.1: one startup TTL sweep over the workspace scratch namespaces. Leases are
    // in-memory only, so a fresh worker cannot hold one; expired namespaces from crashed or
    // deleted sessions are reclaimed here, and the sweep never races a live tool or terminal
    // because none exist yet in this process.
    if let Some(scratch_control) = scratch_control {
        runtime.spawn_blocking(move || {
            match scratch_control.gc_scratch_namespaces(
                &sigil_tools_builtin::ScratchGcConfig::default(),
                current_unix_time_ms(),
            ) {
                Ok(report) if report.deleted > 0 => {
                    tracing::debug!(
                        deleted = report.deleted,
                        reclaimed_bytes = report.deleted_bytes,
                        "startup scratch TTL sweep reclaimed expired namespaces"
                    );
                }
                Ok(_report) => {}
                Err(error) => {
                    tracing::debug!(%error, "startup scratch TTL sweep failed");
                }
            }
        });
    }
    if let Err(error) = register_worker_active_projection_observer(&mut state) {
        let _ = message_tx.send(WorkerMessage::RunFailed(error));
        return;
    }
    state.run.pending_task_handoffs = pending_task_handoffs;
    let _ = message_tx.send(WorkerMessage::WorkerReady);

    loop {
        state.compaction.preparation_tasks.reap_finished();
        state.artifact_gc.tasks.reap_finished();
        if let Err(error) = state.synchronize_route_execution_owner() {
            let _ = message_tx.send(WorkerMessage::RunFailed(error));
            break;
        }
        if let Some(command) = pop_next_urgent_command(&urgent_command_rx, &mut state.readiness) {
            if matches!(
                dispatch_worker_command(
                    WorkerCommandContext {
                        runtime: &runtime,
                        agent: &mut agent,
                        root_config: &root_config,
                        config_path: &config_path,
                        provider_capabilities: &provider_capabilities,
                        workspace_root: &workspace_root,
                        options: &options,
                        permission_mode_override: &permission_mode_override,
                        message_tx: &message_tx,
                        elicitation_handler: &elicitation_handler,
                        mcp_event_handler: &mcp_event_handler,
                        role_provider_builder: &role_provider_builder,
                        context_resolver: &context_resolver,
                        state: &mut state,
                    },
                    command,
                ),
                WorkerCommandDispatchControl::Break
            ) {
                break;
            }
            continue;
        }

        if state.readiness.has_priority_ready_work() {
            record_worker_advancement();
            let _ = advance_worker_loop(WorkerAdvancementContext {
                runtime: &runtime,
                agent: &mut agent,
                root_config: &root_config,
                provider_capabilities: &provider_capabilities,
                workspace_root: &workspace_root,
                options: &options,
                message_tx: &message_tx,
                elicitation_handler: &elicitation_handler,
                mcp_event_handler: &mcp_event_handler,
                role_provider_builder: &role_provider_builder,
                context_resolver: &context_resolver,
                state: &mut state,
            });
            continue;
        }

        let artifact_gc_active = state.artifact_gc.tasks.has_active();
        if let Some(command) = pop_next_ordinary_command(
            &mut state.readiness,
            state.session.projection_reconciling,
            artifact_gc_active,
        ) {
            if matches!(
                dispatch_worker_command(
                    WorkerCommandContext {
                        runtime: &runtime,
                        agent: &mut agent,
                        root_config: &root_config,
                        config_path: &config_path,
                        provider_capabilities: &provider_capabilities,
                        workspace_root: &workspace_root,
                        options: &options,
                        permission_mode_override: &permission_mode_override,
                        message_tx: &message_tx,
                        elicitation_handler: &elicitation_handler,
                        mcp_event_handler: &mcp_event_handler,
                        role_provider_builder: &role_provider_builder,
                        context_resolver: &context_resolver,
                        state: &mut state,
                    },
                    command,
                ),
                WorkerCommandDispatchControl::Break
            ) {
                break;
            }
            continue;
        }

        if state.readiness.has_ready_work() {
            record_worker_advancement();
            let _ = advance_worker_loop(WorkerAdvancementContext {
                runtime: &runtime,
                agent: &mut agent,
                root_config: &root_config,
                provider_capabilities: &provider_capabilities,
                workspace_root: &workspace_root,
                options: &options,
                message_tx: &message_tx,
                elicitation_handler: &elicitation_handler,
                mcp_event_handler: &mcp_event_handler,
                role_provider_builder: &role_provider_builder,
                context_resolver: &context_resolver,
                state: &mut state,
            });
            continue;
        }

        record_worker_advancement();
        if matches!(
            advance_worker_loop(WorkerAdvancementContext {
                runtime: &runtime,
                agent: &mut agent,
                root_config: &root_config,
                provider_capabilities: &provider_capabilities,
                workspace_root: &workspace_root,
                options: &options,
                message_tx: &message_tx,
                elicitation_handler: &elicitation_handler,
                mcp_event_handler: &mcp_event_handler,
                role_provider_builder: &role_provider_builder,
                context_resolver: &context_resolver,
                state: &mut state,
            }),
            WorkerAdvancementControl::SkipCommandPoll
        ) {
            continue;
        }

        let event = match recv_next_worker_event(&event_rx, state.nearest_deadline()) {
            Ok(event) => {
                WORKER_REACTOR_EVENT_WAKE_TOTAL.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                event
            }
            Err(WorkerEventReceiveError::DeadlineElapsed) => {
                WORKER_REACTOR_DEADLINE_TOTAL.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                WorkerEvent::TimerDue
            }
            Err(WorkerEventReceiveError::Disconnected) => break,
        };
        ingest_worker_event_batch(&event_rx, &mut state.readiness, event);
    }

    state.refresh.provider_status_tasks.abort_all();
    state.compaction.preparation_tasks.cancel_and_join(&runtime);
    state.artifact_gc.tasks.cancel_and_join(&runtime);
}

fn pop_next_urgent_command(
    urgent_command_rx: &mpsc::Receiver<WorkerCommand>,
    readiness: &mut WorkerReadiness,
) -> Option<WorkerCommand> {
    match urgent_command_rx.try_recv() {
        Ok(command) => Some(command),
        Err(mpsc::TryRecvError::Empty | mpsc::TryRecvError::Disconnected) => {
            readiness.pop_urgent_command()
        }
    }
}

fn pop_next_ordinary_command(
    readiness: &mut WorkerReadiness,
    projection_reconciling: bool,
    artifact_gc_active: bool,
) -> Option<WorkerCommand> {
    if projection_reconciling {
        readiness.pop_projection_recovery_command()
    } else if artifact_gc_active {
        readiness.pop_ordinary_command_unless(command_conflicts_with_artifact_gc)
    } else {
        readiness.pop_ordinary_command()
    }
}

fn command_conflicts_with_artifact_gc(command: &WorkerCommand) -> bool {
    matches!(
        command,
        WorkerCommand::ReadToolArtifactPage { .. }
            | WorkerCommand::InspectLocalSession { .. }
            | WorkerCommand::ForkLocalSession { .. }
            | WorkerCommand::ForkConversationAtCheckpoint { .. }
            | WorkerCommand::ExportLocalSession { .. }
            | WorkerCommand::SetLocalSessionPin { .. }
            | WorkerCommand::PreviewLocalSessionDelete { .. }
            | WorkerCommand::ApplyLocalSessionDelete { .. }
            | WorkerCommand::PreviewSessionRetention { .. }
            | WorkerCommand::ApplySessionRetention { .. }
    )
}

fn ingest_worker_event_batch(
    event_rx: &mpsc::Receiver<WorkerEvent>,
    readiness: &mut WorkerReadiness,
    first_event: WorkerEvent,
) -> usize {
    readiness.ingest(first_event);
    let mut ingested = 1;
    for _ in 1..MAX_EVENT_DRAIN {
        match event_rx.try_recv() {
            Ok(event) => {
                readiness.ingest(event);
                ingested += 1;
            }
            Err(mpsc::TryRecvError::Empty | mpsc::TryRecvError::Disconnected) => break,
        }
    }
    ingested
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WorkerEventReceiveError {
    DeadlineElapsed,
    Disconnected,
}

fn recv_next_worker_event(
    event_rx: &mpsc::Receiver<WorkerEvent>,
    nearest_deadline: Option<Instant>,
) -> Result<WorkerEvent, WorkerEventReceiveError> {
    let Some(deadline) = nearest_deadline else {
        return event_rx
            .recv()
            .map_err(|_| WorkerEventReceiveError::Disconnected);
    };
    let now = Instant::now();
    if deadline <= now {
        return Err(WorkerEventReceiveError::DeadlineElapsed);
    }
    event_rx
        .recv_timeout(deadline.saturating_duration_since(now))
        .map_err(|error| match error {
            mpsc::RecvTimeoutError::Timeout => WorkerEventReceiveError::DeadlineElapsed,
            mpsc::RecvTimeoutError::Disconnected => WorkerEventReceiveError::Disconnected,
        })
}

pub(in crate::runner) fn finish_idle_auto_compaction<P>(
    preparation: Result<IdleAutoCompactionPreparation, String>,
    prepared_session: Session,
    current_session: &mut Option<Session>,
    current_session_log_path: &Path,
    message_tx: &mpsc::Sender<WorkerMessage>,
    provider: &P,
    runtime: &tokio::runtime::Runtime,
    native_carrier_enabled: bool,
) where
    P: sigil_kernel::Provider,
{
    match capture_stable_idle_compaction_snapshot(&prepared_session) {
        Ok(Some(_)) => {}
        Ok(None) => {
            let _ = message_tx.send(WorkerMessage::Notice(
                "discarded stale automatic compaction session before apply".to_owned(),
            ));
            return;
        }
        Err(error) => {
            let _ = message_tx.send(WorkerMessage::Notice(format!(
                "discarded automatic compaction session after its durable frontier could not be verified: {error:#}"
            )));
            return;
        }
    }

    let previous_session = current_session.take();
    *current_session = Some(prepared_session);

    let preparation = match preparation {
        Ok(preparation) => preparation,
        Err(error) => {
            if let Some(adoption_error) =
                retain_prepared_idle_session_or_restore(current_session, previous_session)
            {
                let _ = message_tx.send(WorkerMessage::Notice(adoption_error));
            }
            let _ = message_tx.send(WorkerMessage::Notice(format!(
                "automatic compaction preflight was not applied: {error}"
            )));
            return;
        }
    };

    match preparation {
        IdleAutoCompactionPreparation::Ready(pending) => {
            let Some(session) = current_session.as_ref() else {
                return;
            };
            let folded_event_count = pending.folded_event_count();
            let idle_auto_scope_fingerprint =
                pending.idle_auto_scope_fingerprint().map(str::to_owned);
            match (*pending).apply_with_optional_native(
                session,
                current_session_log_path,
                provider,
                runtime,
                native_carrier_enabled,
            ) {
                Ok((outcome, native_notice)) => {
                    if let Some(notice) = native_notice {
                        let _ = message_tx.send(WorkerMessage::Notice(notice));
                    }
                    if let Some(adoption_error) =
                        retain_prepared_idle_session_or_restore(current_session, previous_session)
                    {
                        let _ = message_tx.send(WorkerMessage::Notice(adoption_error));
                    }
                    let entries = current_session
                        .as_ref()
                        .map(|session| session.entries().to_vec())
                        .unwrap_or_default();
                    let _ = message_tx.send(WorkerMessage::V2CompactionApplied {
                        request_id: 0,
                        source: V2CompactionApplySource::IdleAutomatic,
                        compaction_id: outcome.compaction_id,
                        folded_event_count,
                        entries,
                    });
                }
                Err(error) => {
                    if let Some(adoption_error) =
                        retain_prepared_idle_session_or_restore(current_session, previous_session)
                    {
                        let _ = message_tx.send(WorkerMessage::Notice(adoption_error));
                    }
                    let latch_status = idle_auto_scope_fingerprint.as_deref().map_or_else(
                        || Ok(false),
                        |scope_fingerprint| {
                            has_failed_idle_automatic_scope(
                                current_session_log_path,
                                scope_fingerprint,
                            )
                        },
                    );
                    let notice = match latch_status {
                        Ok(true) => format!(
                            "automatic compaction was not applied; unchanged history is now held by its durable failure latch: {error:#}"
                        ),
                        Ok(false) => format!(
                            "automatic compaction was not applied before a durable failure latch could be confirmed: {error:#}"
                        ),
                        Err(latch_error) => format!(
                            "automatic compaction was not applied; durable failure latch status could not be confirmed ({latch_error:#}): {error:#}"
                        ),
                    };
                    let _ = message_tx.send(WorkerMessage::Notice(notice));
                }
            }
        }
        IdleAutoCompactionPreparation::NoFoldableHistory => {
            if let Some(adoption_error) =
                retain_prepared_idle_session_or_restore(current_session, previous_session)
            {
                let _ = message_tx.send(WorkerMessage::Notice(adoption_error));
            }
            let _ = message_tx.send(WorkerMessage::Notice(
                "automatic compaction skipped: no newly foldable history".to_owned(),
            ));
        }
        IdleAutoCompactionPreparation::FailureLatched => {
            if let Some(adoption_error) =
                retain_prepared_idle_session_or_restore(current_session, previous_session)
            {
                let _ = message_tx.send(WorkerMessage::Notice(adoption_error));
            }
            let _ = message_tx.send(WorkerMessage::Notice(
                "automatic compaction is held after a previous failed attempt; new fold material or target policy is required"
                    .to_owned(),
            ));
        }
        IdleAutoCompactionPreparation::CircuitOpen { decision } => {
            if let Some(adoption_error) =
                retain_prepared_idle_session_or_restore(current_session, previous_session)
            {
                let _ = message_tx.send(WorkerMessage::Notice(adoption_error));
            }
            let reason = match decision {
                sigil_kernel::CompactionCircuitBreakerDecisionV1::Allowed => {
                    "circuit unexpectedly reported allowed".to_owned()
                }
                sigil_kernel::CompactionCircuitBreakerDecisionV1::SameCursorAndLayoutFailed => {
                    "the same source cursor and cache layout already failed".to_owned()
                }
                sigil_kernel::CompactionCircuitBreakerDecisionV1::SemanticSummarizerRouteDisabled {
                    consecutive_failures,
                } => format!(
                    "the semantic summarizer route is disabled after {consecutive_failures} consecutive timeout/inflation failures"
                ),
                sigil_kernel::CompactionCircuitBreakerDecisionV1::RealTurnRequired {
                    latest_compaction_sequence,
                } => format!(
                    "a completed real turn is required after compaction sequence {latest_compaction_sequence}"
                ),
                sigil_kernel::CompactionCircuitBreakerDecisionV1::PostActivationEmergency {
                    layer,
                } => format!(
                    "the first post-compaction turn remains at emergency pressure; blocking layer: {layer:?}"
                ),
            };
            let _ = message_tx.send(WorkerMessage::Notice(format!(
                "automatic compaction circuit is open: {reason}"
            )));
        }
        IdleAutoCompactionPreparation::CoolingDown {
            retry_after_unix_ms,
        } => {
            if let Some(adoption_error) =
                retain_prepared_idle_session_or_restore(current_session, previous_session)
            {
                let _ = message_tx.send(WorkerMessage::Notice(adoption_error));
            }
            let _ = message_tx.send(WorkerMessage::Notice(format!(
                "automatic compaction admission is cooling down until {retry_after_unix_ms}"
            )));
        }
        IdleAutoCompactionPreparation::AdmissionUnavailable { reason } => {
            if let Some(adoption_error) =
                retain_prepared_idle_session_or_restore(current_session, previous_session)
            {
                let _ = message_tx.send(WorkerMessage::Notice(adoption_error));
            }
            let _ = message_tx.send(WorkerMessage::Notice(format!(
                "automatic compaction was not applied: local target admission is unavailable ({reason})"
            )));
        }
        IdleAutoCompactionPreparation::NotRequested
        | IdleAutoCompactionPreparation::NotHardThreshold => {
            if let Some(adoption_error) =
                retain_prepared_idle_session_or_restore(current_session, previous_session)
            {
                let _ = message_tx.send(WorkerMessage::Notice(adoption_error));
            }
        }
    }
}

fn retain_prepared_idle_session_or_restore(
    current_session: &mut Option<Session>,
    previous_session: Option<Session>,
) -> Option<String> {
    let stability = current_session
        .as_ref()
        .map(capture_stable_idle_compaction_snapshot)
        .transpose();
    match stability {
        Ok(Some(Some(_))) => None,
        Ok(Some(None) | None) => {
            *current_session = previous_session;
            Some(
                "discarded automatic compaction session after the durable session-entry frontier changed"
                    .to_owned(),
            )
        }
        Err(error) => {
            *current_session = previous_session;
            Some(format!(
                "discarded automatic compaction session after its durable frontier could not be verified: {error:#}"
            ))
        }
    }
}

#[cfg(test)]
mod reactor_tests {
    use super::*;
    use crate::runner::{
        WorkerCommandSender,
        worker_event::{
            MAX_PENDING_MCP_RUNTIME_EVENTS, WorkerEventPayloadSender, WorkerMcpRuntimeEventSender,
        },
    };

    struct NoNetworkOAuthExecutor;

    #[async_trait::async_trait]
    impl sigil_mcp::McpOAuthHttpExecutor for NoNetworkOAuthExecutor {
        async fn execute(
            &self,
            _request: sigil_mcp::McpOAuthHttpRequest,
        ) -> std::result::Result<sigil_mcp::McpOAuthHttpResponse, sigil_mcp::McpOAuthTransportError>
        {
            panic!("non-OAuth test server must not perform network I/O")
        }
    }

    #[test]
    fn idle_inbox_without_deadline_blocks_until_an_event_arrives() {
        let (event_tx, event_rx) = mpsc::channel();
        let artifact_gc_tasks = ArtifactGcTaskManager::new();
        let entered = Arc::new(std::sync::Barrier::new(2));
        let receiver_entered = Arc::clone(&entered);
        let receiver = std::thread::spawn(move || {
            receiver_entered.wait();
            recv_next_worker_event(&event_rx, None)
        });

        entered.wait();
        std::thread::yield_now();
        assert!(
            !receiver.is_finished(),
            "idle receiver returned without an event or deadline"
        );
        assert!(
            !artifact_gc_tasks.has_active(),
            "idle receive must not poll or launch artifact maintenance"
        );

        event_tx
            .send(WorkerEvent::TimerDue)
            .expect("test wake should send");
        assert!(matches!(receiver.join(), Ok(Ok(WorkerEvent::TimerDue))));
    }

    #[test]
    fn public_command_sender_wakes_the_unified_inbox() {
        let (event_tx, event_rx) = mpsc::channel();
        let (urgent_tx, _urgent_rx) = mpsc::channel();
        let command_tx = WorkerCommandSender::new(event_tx, urgent_tx);
        command_tx
            .send(WorkerCommand::SubmitTask {
                prompt: "wake worker".to_owned(),
            })
            .expect("command should send");

        assert!(matches!(
            recv_next_worker_event(&event_rx, None),
            Ok(WorkerEvent::Command(WorkerCommand::SubmitTask { prompt }))
                if prompt == "wake worker"
        ));
    }

    #[test]
    fn run_and_compaction_completion_senders_wake_the_unified_inbox() {
        let (event_tx, event_rx) = mpsc::channel();
        WorkerEventPayloadSender::run(event_tx.clone())
            .send(RunTaskResult {
                run_id: 7,
                session: Session::new("test", "model"),
                payload: RunTaskPayload::Agent {
                    profile_id: "reviewer".to_owned(),
                    result: Err("done".to_owned()),
                },
                post_run_maintenance: None,
            })
            .expect("run completion should send");
        let event =
            recv_next_worker_event(&event_rx, None).expect("run completion should be received");
        let WorkerEvent::RunCompleted(result) = event else {
            panic!("run completion should retain its typed event");
        };
        assert_eq!(result.run_id, 7);

        WorkerEventPayloadSender::compaction(event_tx)
            .send(CompactionPreparationTaskResult::Manual {
                request_id: 9,
                session_scope_id: "session-a".to_owned(),
                result: Err("done".to_owned()),
            })
            .expect("compaction completion should send");
        assert!(matches!(
            recv_next_worker_event(&event_rx, None),
            Ok(WorkerEvent::CompactionPrepared(
                CompactionPreparationTaskResult::Manual { request_id: 9, .. }
            ))
        ));
    }

    #[test]
    fn provider_status_adapter_delivers_one_typed_terminal_result() {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .expect("provider adapter runtime should build");
        let (event_tx, event_rx) = mpsc::channel();
        let result_tx = provider_status_result_sender(&runtime, &event_tx);
        result_tx
            .send(ProviderStatusTaskResult::Balance {
                request_id: 41,
                snapshot: sigil_runtime::BalanceSnapshot {
                    status: "ready".to_owned(),
                    ..sigil_runtime::BalanceSnapshot::default()
                },
            })
            .expect("first terminal result should send");
        let _ = result_tx.send(ProviderStatusTaskResult::Balance {
            request_id: 42,
            snapshot: sigil_runtime::BalanceSnapshot::default(),
        });

        assert!(matches!(
            recv_next_worker_event(&event_rx, None),
            Ok(WorkerEvent::ProviderStatusResolved(
                ProviderStatusTaskResult::Balance { request_id: 41, .. }
            ))
        ));
        assert!(
            event_rx.try_recv().is_err(),
            "one-shot adapter must not forward a second terminal result"
        );
    }

    #[test]
    fn oauth_and_mcp_runtime_deliver_typed_inbox_payloads() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("OAuth fixture runtime should build");
        let server = sigil_kernel::McpServerConfig {
            name: "stdio-test".to_owned(),
            ..sigil_kernel::McpServerConfig::default()
        };
        let service = sigil_runtime::McpOAuthRuntimeService::new(
            Arc::new(sigil_runtime::McpOAuthCredentialManager::system()),
            Arc::new(NoNetworkOAuthExecutor),
        );
        let status = runtime
            .block_on(service.inspect(&server))
            .expect("stdio MCP should produce a no-network OAuth status");
        let (event_tx, event_rx) = mpsc::channel();
        WorkerEventPayloadSender::mcp_oauth(event_tx.clone())
            .send(McpOAuthTaskResult {
                server_name: "stdio-test".to_owned(),
                status,
                activate_server: false,
            })
            .expect("OAuth terminal result should send");
        assert!(matches!(
            recv_next_worker_event(&event_rx, None),
            Ok(WorkerEvent::McpOAuthCompleted(McpOAuthTaskResult {
                server_name,
                activate_server: false,
                ..
            })) if server_name == "stdio-test"
        ));

        let mcp_sender = WorkerMcpRuntimeEventSender::new(event_tx);
        mcp_sender
            .send(McpRuntimeEvent::ListChanged(
                sigil_runtime::McpListChangedNotification {
                    server_name: "stdio-test".to_owned(),
                    kind: sigil_runtime::McpListChangedKind::Tools,
                },
            ))
            .expect("MCP runtime notice should send");
        let mut readiness = WorkerReadiness::new();
        readiness.ingest(
            recv_next_worker_event(&event_rx, None)
                .expect("coalesced MCP readiness should be received"),
        );
        assert!(matches!(
            readiness.mcp_runtime_events.pop_front(),
            Some(McpRuntimeEvent::ListChanged(notification))
                if notification.server_name == "stdio-test"
                    && notification.kind == sigil_runtime::McpListChangedKind::Tools
        ));
    }

    #[test]
    fn elapsed_deadline_is_one_shot_and_does_not_consume_an_event() {
        let (event_tx, event_rx) = mpsc::channel();
        event_tx
            .send(WorkerEvent::TimerDue)
            .expect("queued event should send");
        let elapsed_deadline = Instant::now()
            .checked_sub(Duration::from_millis(1))
            .expect("one millisecond subtraction should be representable");

        assert!(matches!(
            recv_next_worker_event(&event_rx, Some(elapsed_deadline)),
            Err(WorkerEventReceiveError::DeadlineElapsed)
        ));
        assert!(matches!(
            recv_next_worker_event(&event_rx, None),
            Ok(WorkerEvent::TimerDue)
        ));

        let mut readiness = WorkerReadiness::new();
        readiness.ingest(WorkerEvent::TimerDue);
        assert!(readiness.take_timer_due());
        assert!(!readiness.take_timer_due());
    }

    #[test]
    fn armed_deadline_returns_an_already_queued_event_without_timer_synthesis() {
        let (event_tx, event_rx) = mpsc::channel();
        event_tx
            .send(WorkerEvent::ControlWake)
            .expect("control wake should send");
        let armed_deadline = Instant::now()
            .checked_add(Duration::from_secs(60))
            .expect("test deadline should be representable");
        assert!(matches!(
            recv_next_worker_event(&event_rx, Some(armed_deadline)),
            Ok(WorkerEvent::ControlWake)
        ));
    }

    #[test]
    fn event_batch_drain_is_bounded() {
        let (event_tx, event_rx) = mpsc::channel();
        for ordinal in 0..(MAX_EVENT_DRAIN + 3) {
            event_tx
                .send(WorkerEvent::Command(WorkerCommand::SubmitTask {
                    prompt: format!("ordinary-{ordinal}"),
                }))
                .expect("ordinary event should send");
        }
        let first = event_rx.recv().expect("first event should be available");
        let mut readiness = WorkerReadiness::new();
        assert_eq!(
            ingest_worker_event_batch(&event_rx, &mut readiness, first),
            MAX_EVENT_DRAIN
        );
        for ordinal in 0..MAX_EVENT_DRAIN {
            assert!(matches!(
                readiness.pop_ordinary_command(),
                Some(WorkerCommand::SubmitTask { prompt })
                    if prompt == format!("ordinary-{ordinal}")
            ));
        }
        assert!(matches!(
            event_rx.try_recv(),
            Ok(WorkerEvent::Command(WorkerCommand::SubmitTask { prompt }))
                if prompt == format!("ordinary-{MAX_EVENT_DRAIN}")
        ));
    }

    #[test]
    fn urgent_cancel_and_shutdown_precede_mcp_and_ordinary_floods() {
        let (event_tx, event_rx) = mpsc::channel();
        let (urgent_tx, urgent_rx) = mpsc::channel();
        let command_tx = WorkerCommandSender::new(event_tx.clone(), urgent_tx);
        let mcp_sender = WorkerMcpRuntimeEventSender::new(event_tx.clone());
        for ordinal in 0..(MAX_PENDING_MCP_RUNTIME_EVENTS * 4) {
            mcp_sender
                .send(McpRuntimeEvent::Progress(
                    sigil_runtime::McpProgressNotification {
                        server_name: "filesystem".to_owned(),
                        progress_token: format!("flood-{ordinal}"),
                        progress: Some(ordinal as f64),
                        total: None,
                        message: None,
                    },
                ))
                .expect("MCP flood should remain bounded");
            event_tx
                .send(WorkerEvent::Command(WorkerCommand::SubmitTask {
                    prompt: format!("ordinary-{ordinal}"),
                }))
                .expect("ordinary flood should send");
        }
        command_tx
            .send(WorkerCommand::CancelRun)
            .expect("urgent cancel should enter control lane");
        command_tx
            .send(WorkerCommand::Shutdown)
            .expect("urgent shutdown should enter control lane");

        let mut readiness = WorkerReadiness::new();
        assert!(matches!(
            pop_next_urgent_command(&urgent_rx, &mut readiness),
            Some(WorkerCommand::CancelRun)
        ));
        assert!(matches!(
            pop_next_urgent_command(&urgent_rx, &mut readiness),
            Some(WorkerCommand::Shutdown)
        ));
        assert!(
            event_rx.try_recv().is_ok(),
            "ordinary inbox remains flooded while urgent control is serviced"
        );
        assert_eq!(mcp_sender.pending_len(), MAX_PENDING_MCP_RUNTIME_EVENTS);
    }

    #[test]
    fn authority_projection_precedes_but_timer_yields_to_ordinary_commands() {
        let (event_tx, event_rx) = mpsc::channel();
        let coalescer = WorkerWakeCoalescer::new(event_tx, Some("session-a".to_owned()));
        let binding = coalescer
            .current_projection_binding()
            .expect("projection binding should exist");
        coalescer.notify_session_projection(
            &binding,
            false,
            &[sigil_kernel::session::ActiveProjectionFamily::Queue]
                .into_iter()
                .collect(),
        );
        let mut readiness = WorkerReadiness::new();
        readiness.ingest(WorkerEvent::Command(WorkerCommand::SubmitTask {
            prompt: "ordinary".to_owned(),
        }));
        readiness.ingest(WorkerEvent::TimerDue);
        assert!(readiness.has_ready_work());
        assert!(!readiness.has_priority_ready_work());
        assert!(matches!(
            readiness.pop_ordinary_command(),
            Some(WorkerCommand::SubmitTask { prompt }) if prompt == "ordinary"
        ));
        assert!(readiness.take_timer_due());
        readiness.ingest(event_rx.recv().expect("projection wake should arrive"));
        assert!(readiness.has_priority_ready_work());
        assert!(readiness.take_wake_readiness(&coalescer).any);
    }

    #[test]
    fn artifact_source_wakes_coalesce_and_remain_below_ordinary_commands() {
        let (event_tx, event_rx) = mpsc::channel();
        let coalescer = WorkerWakeCoalescer::new(event_tx, Some("session-a".to_owned()));
        let binding = coalescer
            .current_projection_binding()
            .expect("projection binding should exist");
        let changed = [sigil_kernel::session::ActiveProjectionFamily::ToolOutputPressure]
            .into_iter()
            .collect();
        coalescer.notify_session_projection(&binding, false, &changed);
        coalescer.notify_session_projection(&binding, false, &changed);

        let mut readiness = WorkerReadiness::new();
        readiness.ingest(WorkerEvent::Command(WorkerCommand::SubmitTask {
            prompt: "ordinary".to_owned(),
        }));
        readiness.ingest(event_rx.recv().expect("coalesced artifact wake"));
        assert!(
            event_rx.try_recv().is_err(),
            "coalesced source changes must publish one wake token"
        );
        assert!(!readiness.has_priority_ready_work());
        assert!(matches!(
            readiness.pop_ordinary_command(),
            Some(WorkerCommand::SubmitTask { prompt }) if prompt == "ordinary"
        ));
        let wake = readiness.take_wake_readiness(&coalescer);
        assert!(wake.any);
        assert!(wake.tool_output_pressure_dirty());
    }

    #[test]
    fn projection_reconciliation_blocks_ordinary_but_not_urgent_control() {
        let (_urgent_tx, urgent_rx) = mpsc::channel();
        let mut readiness = WorkerReadiness::new();
        readiness.ingest(WorkerEvent::Command(WorkerCommand::SubmitTask {
            prompt: "ordinary".to_owned(),
        }));
        readiness.ingest(WorkerEvent::Command(WorkerCommand::SwitchSession {
            session_log_path: PathBuf::from("recover-session.jsonl"),
            attachment_recovery_binding: None,
        }));
        readiness.ingest(WorkerEvent::Command(WorkerCommand::CancelRun));

        assert!(matches!(
            pop_next_urgent_command(&urgent_rx, &mut readiness),
            Some(WorkerCommand::CancelRun)
        ));
        assert!(matches!(
            pop_next_ordinary_command(&mut readiness, true, false),
            Some(WorkerCommand::SwitchSession { session_log_path, .. })
                if session_log_path == std::path::Path::new("recover-session.jsonl")
        ));
        assert!(pop_next_ordinary_command(&mut readiness, true, false).is_none());
        assert!(matches!(
            pop_next_ordinary_command(&mut readiness, false, false),
            Some(WorkerCommand::SubmitTask { prompt }) if prompt == "ordinary"
        ));
    }

    #[test]
    fn active_artifact_gc_defers_conflicting_session_commands_without_reordering() {
        let mut readiness = WorkerReadiness::new();
        readiness.ingest(WorkerEvent::Command(
            WorkerCommand::ForkConversationAtCheckpoint {
                request_id: 7,
                request: ControlledCheckpointRestoreRequest {
                    checkpoint_id: "checkpoint-1".to_owned(),
                    checkpoint_digest: "digest-1".to_owned(),
                },
            },
        ));
        readiness.ingest(WorkerEvent::Command(WorkerCommand::SetLocalSessionPin {
            request_id: 7,
            source_path: PathBuf::from("session.jsonl"),
            pinned: true,
        }));
        readiness.ingest(WorkerEvent::Command(WorkerCommand::SubmitTask {
            prompt: "later".to_owned(),
        }));

        assert!(pop_next_ordinary_command(&mut readiness, false, true).is_none());
        assert!(matches!(
            pop_next_ordinary_command(&mut readiness, false, false),
            Some(WorkerCommand::ForkConversationAtCheckpoint { request_id: 7, .. })
        ));
        assert!(matches!(
            pop_next_ordinary_command(&mut readiness, false, false),
            Some(WorkerCommand::SetLocalSessionPin { request_id: 7, .. })
        ));
        assert!(matches!(
            pop_next_ordinary_command(&mut readiness, false, false),
            Some(WorkerCommand::SubmitTask { prompt }) if prompt == "later"
        ));
    }

    #[test]
    fn worker_scheduler_has_no_general_fifty_millisecond_poll() {
        let forbidden = ["Duration::from_millis(", "50", ")"].concat();
        assert!(!include_str!("scheduler.rs").contains(&forbidden));
    }

    #[test]
    fn terminal_lifecycle_has_no_steady_state_timer_or_status_probe() {
        assert!(!include_str!("state.rs").contains("next_terminal_task_refresh_at"));
        assert!(!include_str!("advancement.rs").contains("refresh_terminal_task_statuses"));
        assert!(!include_str!("../worker_loop.rs").contains("TERMINAL_TASK_REFRESH_INTERVAL"));
    }

    #[test]
    fn prepared_idle_session_is_kept_only_while_its_entry_frontier_is_stable() -> anyhow::Result<()>
    {
        let temp = tempfile::tempdir()?;
        let store = JsonlSessionStore::new(temp.path().join("session.jsonl"))?;
        let prepared = Session::new("prepared", "model").with_store(store.clone());
        let previous = Session::new("previous", "model");
        let mut current = Some(prepared);

        assert!(retain_prepared_idle_session_or_restore(&mut current, Some(previous)).is_none());
        assert_eq!(
            current.as_ref().map(Session::provider_name),
            Some("prepared")
        );

        store.append(&SessionLogEntry::Control(ControlEntry::Note {
            kind: "external-session-entry".to_owned(),
            data: serde_json::Value::Null,
        }))?;
        let previous = Session::new("restored", "model");
        let notice = retain_prepared_idle_session_or_restore(&mut current, Some(previous))
            .expect("new durable session entry invalidates the prepared session");
        assert!(notice.contains("durable session-entry frontier changed"));
        assert_eq!(
            current.as_ref().map(Session::provider_name),
            Some("restored")
        );
        Ok(())
    }

    #[test]
    fn idle_compaction_finish_has_no_session_reload_path() {
        let source = include_str!("scheduler.rs");
        let finish = source
            .split_once("pub(in crate::runner) fn finish_idle_auto_compaction")
            .expect("finish function remains present")
            .1
            .split_once("#[cfg(test)]")
            .expect("reactor tests remain after finish")
            .0;
        assert!(!finish.contains("load_session_with_runtime_attachments"));
        assert!(!finish.contains("load_session_with_captured_runtime_attachments"));
    }
}

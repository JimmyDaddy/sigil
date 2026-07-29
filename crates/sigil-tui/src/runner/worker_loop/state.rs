use super::*;

pub(in crate::runner) struct WorkerLoopState {
    pub(in crate::runner) event_tx: mpsc::Sender<WorkerEvent>,
    pub(in crate::runner) wake_coalescer: WorkerWakeCoalescer,
    pub(in crate::runner) readiness: WorkerReadiness,
    pub(in crate::runner) session: SessionWorkerState,
    pub(in crate::runner) run: RunWorkerState,
    pub(in crate::runner) compaction: CompactionWorkerState,
    pub(in crate::runner) refresh: RefreshWorkerState,
    pub(in crate::runner) agent: AgentWorkerState,
    pub(in crate::runner) mcp_oauth: McpOAuthWorkerState,
    pub(in crate::runner) processed_worker_command_ids: BTreeSet<String>,
    pub(in crate::runner) last_observed_run_active: bool,
}

impl WorkerLoopState {
    pub(in crate::runner) fn new(
        session_log_path: PathBuf,
        session: Option<Session>,
        agent_supervisor: sigil_runtime::AgentSupervisor,
        background_agent_runs: sigil_runtime::AgentToolBackgroundRuns,
        event_tx: mpsc::Sender<WorkerEvent>,
        wake_coalescer: WorkerWakeCoalescer,
    ) -> Self {
        let pending_agent_result_continuations =
            pending_agent_result_continuations_from_session(session.as_ref());
        let active_terminal_task_ids = session
            .as_ref()
            .map(|session| {
                session
                    .terminal_task_projection()
                    .active_task_ids
                    .into_iter()
                    .collect()
            })
            .unwrap_or_default();
        Self {
            event_tx: event_tx.clone(),
            wake_coalescer,
            readiness: WorkerReadiness::new(),
            session: SessionWorkerState {
                log_path: session_log_path,
                current: session,
                detached_durable_controls: Vec::new(),
                exact_prompts: ExactConversationPromptStore::new(),
                active_projection_subscription: None,
                projection_reconciling: false,
                projection_retry_at: None,
                projection_reconciliation_error: None,
                projection_reconciliation_attempts: 0,
                projection_reconciliation_latched: false,
                task_guidance_dirty: true,
                conversation_queue_dirty: true,
                task_guidance_retry_at: None,
                conversation_queue_retry_at: None,
                task_guidance_retry_attempts: 0,
                conversation_queue_retry_attempts: 0,
                task_guidance_retry_latched: false,
                conversation_queue_retry_latched: false,
                active_terminal_task_ids,
                pending_agent_result_continuations,
                last_queued_pre_turn_block: None,
                last_task_guidance_block: None,
                pending_queued_pre_turn_preparation: None,
            },
            run: RunWorkerState {
                result_tx: WorkerEventPayloadSender::run(event_tx.clone()),
                active: None,
                discarded_ids: BTreeSet::new(),
                next_id: 1,
                pending_task_handoffs: Vec::new(),
            },
            compaction: CompactionWorkerState {
                preparation_tx: WorkerEventPayloadSender::compaction(event_tx.clone()),
                preparation_tasks: CompactionPreparationTaskManager::new(),
                next_request_id: 1,
                local_preview: None,
                pending: None,
                idle_auto: IdleAutoCompactionState::default(),
            },
            refresh: RefreshWorkerState {
                provider_status_tasks: ProviderStatusTaskManager::new(),
                pending_mcp_servers: BTreeSet::new(),
                next_mcp_retry_at: Instant::now(),
                next_terminal_task_refresh_at: Instant::now(),
            },
            agent: AgentWorkerState {
                supervisor: agent_supervisor,
                background_runs: background_agent_runs,
                last_task_provider_route_diagnostics:
                    sigil_runtime::TaskProviderRouteDiagnosticsSnapshot::default(),
                last_task_completion_progress:
                    sigil_runtime::TaskCompletionProgressSnapshot::default(),
            },
            mcp_oauth: McpOAuthWorkerState {
                result_tx: WorkerEventPayloadSender::mcp_oauth(event_tx),
                active: BTreeMap::new(),
            },
            processed_worker_command_ids: BTreeSet::new(),
            last_observed_run_active: false,
        }
    }

    pub(in crate::runner) fn nearest_deadline(&self) -> Option<Instant> {
        let mcp_deadline = (self.run.active.is_none()
            && !self.refresh.pending_mcp_servers.is_empty())
        .then_some(self.refresh.next_mcp_retry_at);
        let terminal_deadline = (!self.session.projection_reconciling
            && self.run.active.is_none()
            && !self.session.active_terminal_task_ids.is_empty())
        .then_some(self.refresh.next_terminal_task_refresh_at);
        [
            mcp_deadline,
            terminal_deadline,
            self.session.projection_retry_at,
            self.session.task_guidance_retry_at,
            self.session.conversation_queue_retry_at,
        ]
        .into_iter()
        .flatten()
        .min()
    }
}

pub(in crate::runner) fn register_worker_active_projection_observer(
    state: &mut WorkerLoopState,
) -> std::result::Result<(), String> {
    state.session.active_projection_subscription = None;
    let Some(session) = state.session.current.as_ref() else {
        return Ok(());
    };
    let binding = state
        .wake_coalescer
        .current_projection_binding()
        .ok_or_else(|| "active projection observer requires a session binding".to_owned())?;
    if binding.session_scope_id != session.session_scope_id() {
        return Err("active projection observer binding belongs to another session".to_owned());
    }
    let observer: Arc<dyn ActiveProjectionObserver> = Arc::new(
        WorkerActiveProjectionObserver::new(state.wake_coalescer.clone(), binding),
    );
    state.session.active_projection_subscription = session
        .register_active_projection_observer(observer)
        .map_err(|error| format!("failed to register active projection observer: {error:#}"))?;
    Ok(())
}

pub(in crate::runner) struct McpOAuthWorkerState {
    pub(in crate::runner) result_tx: WorkerEventPayloadSender<McpOAuthTaskResult>,
    pub(in crate::runner) active: BTreeMap<String, ActiveMcpOAuthFlow>,
}

pub(in crate::runner) struct SessionWorkerState {
    pub(in crate::runner) log_path: PathBuf,
    pub(in crate::runner) current: Option<Session>,
    pub(in crate::runner) detached_durable_controls: Vec<ControlEntry>,
    pub(in crate::runner) exact_prompts: ExactConversationPromptStore,
    pub(in crate::runner) active_projection_subscription: Option<ActiveProjectionSubscription>,
    pub(in crate::runner) projection_reconciling: bool,
    pub(in crate::runner) projection_retry_at: Option<Instant>,
    pub(in crate::runner) projection_reconciliation_error: Option<String>,
    pub(in crate::runner) projection_reconciliation_attempts: u8,
    pub(in crate::runner) projection_reconciliation_latched: bool,
    pub(in crate::runner) task_guidance_dirty: bool,
    pub(in crate::runner) conversation_queue_dirty: bool,
    pub(in crate::runner) task_guidance_retry_at: Option<Instant>,
    pub(in crate::runner) conversation_queue_retry_at: Option<Instant>,
    pub(in crate::runner) task_guidance_retry_attempts: u8,
    pub(in crate::runner) conversation_queue_retry_attempts: u8,
    pub(in crate::runner) task_guidance_retry_latched: bool,
    pub(in crate::runner) conversation_queue_retry_latched: bool,
    pub(in crate::runner) active_terminal_task_ids: BTreeSet<TerminalTaskId>,
    pub(in crate::runner) pending_agent_result_continuations: Vec<AgentThreadId>,
    pub(in crate::runner) last_queued_pre_turn_block: Option<(ConversationInputQueueId, String)>,
    pub(in crate::runner) last_task_guidance_block: Option<(ConversationInputQueueId, String)>,
    pub(in crate::runner) pending_queued_pre_turn_preparation:
        Option<PreTurnV2CompactionPreparation>,
}

pub(in crate::runner) struct RunWorkerState {
    pub(in crate::runner) result_tx: WorkerEventPayloadSender<RunTaskResult>,
    pub(in crate::runner) active: Option<ActiveRun>,
    pub(in crate::runner) discarded_ids: BTreeSet<u64>,
    pub(in crate::runner) next_id: u64,
    pub(in crate::runner) pending_task_handoffs: Vec<StartDurableTaskAction>,
}

pub(in crate::runner) struct CompactionWorkerState {
    pub(in crate::runner) preparation_tx: WorkerEventPayloadSender<CompactionPreparationTaskResult>,
    pub(in crate::runner) preparation_tasks: CompactionPreparationTaskManager,
    pub(in crate::runner) next_request_id: u64,
    pub(in crate::runner) local_preview: Option<PendingLocalV2Compaction>,
    pub(in crate::runner) pending: Option<PendingV2Compaction>,
    pub(in crate::runner) idle_auto: IdleAutoCompactionState,
}

pub(in crate::runner) struct RefreshWorkerState {
    pub(in crate::runner) provider_status_tasks: ProviderStatusTaskManager,
    pub(in crate::runner) pending_mcp_servers: BTreeSet<String>,
    pub(in crate::runner) next_mcp_retry_at: Instant,
    pub(in crate::runner) next_terminal_task_refresh_at: Instant,
}

pub(in crate::runner) struct AgentWorkerState {
    pub(in crate::runner) supervisor: sigil_runtime::AgentSupervisor,
    pub(in crate::runner) background_runs: sigil_runtime::AgentToolBackgroundRuns,
    pub(in crate::runner) last_task_provider_route_diagnostics:
        sigil_runtime::TaskProviderRouteDiagnosticsSnapshot,
    pub(in crate::runner) last_task_completion_progress:
        sigil_runtime::TaskCompletionProgressSnapshot,
}

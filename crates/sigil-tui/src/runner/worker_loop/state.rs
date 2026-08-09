use super::*;

const MAX_APPROVAL_COMMAND_RECEIPTS: usize = 256;
const MAX_ARTIFACT_GC_DEFERRED_NOTICES: usize = 32;

pub(in crate::runner) struct WorkerLoopState {
    pub(in crate::runner) event_tx: mpsc::Sender<WorkerEvent>,
    pub(in crate::runner) wake_coalescer: WorkerWakeCoalescer,
    pub(in crate::runner) terminal_lifecycle_router: ChannelTerminalLifecycleRouter,
    pub(in crate::runner) terminal_control: Option<sigil_tools_builtin::TerminalTaskControlHandle>,
    /// Session-scoped scratch lease registry shared with bash/terminal tools; session-delete
    /// cleanup uses it so live namespaces are never reclaimed.
    pub(in crate::runner) scratch_control: Option<sigil_tools_builtin::ScratchNamespaceControl>,
    pub(in crate::runner) readiness: WorkerReadiness,
    pub(in crate::runner) session: SessionWorkerState,
    pub(in crate::runner) run: RunWorkerState,
    pub(in crate::runner) compaction: CompactionWorkerState,
    pub(in crate::runner) artifact_gc: ArtifactGcWorkerState,
    pub(in crate::runner) refresh: RefreshWorkerState,
    pub(in crate::runner) agent: AgentWorkerState,
    pub(in crate::runner) mcp_oauth: McpOAuthWorkerState,
    pub(in crate::runner) approval_command_receipts: BTreeMap<String, WorkerApprovalCommandReceipt>,
    approval_command_receipt_order: VecDeque<String>,
    pub(in crate::runner) last_observed_run_active: bool,
    /// Startup artifact GC is deferred until the first dispatched command so a user who resumes
    /// another session immediately after launch is never blocked on maintenance for the session
    /// they are about to abandon.
    pub(in crate::runner) defer_startup_artifact_gc: bool,
}

impl WorkerLoopState {
    pub(in crate::runner) fn new(
        session_log_path: PathBuf,
        session: Option<Session>,
        attachment_lease: Arc<
            sigil_runtime::interactive_session_attachment::InteractiveSessionAttachmentLease,
        >,
        agent_supervisor: sigil_runtime::AgentSupervisor,
        background_agent_runs: sigil_runtime::AgentToolBackgroundRuns,
        event_tx: mpsc::Sender<WorkerEvent>,
        wake_coalescer: WorkerWakeCoalescer,
        terminal_lifecycle_router: ChannelTerminalLifecycleRouter,
        terminal_control: Option<sigil_tools_builtin::TerminalTaskControlHandle>,
        scratch_control: Option<sigil_tools_builtin::ScratchNamespaceControl>,
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
        let terminal_lifecycle_generations = session
            .as_ref()
            .map(|session| {
                session
                    .terminal_task_projection()
                    .tasks
                    .into_iter()
                    .map(|(task_id, summary)| (task_id, summary.generation))
                    .collect()
            })
            .unwrap_or_default();
        Self {
            event_tx: event_tx.clone(),
            wake_coalescer,
            terminal_lifecycle_router,
            terminal_control,
            scratch_control,
            readiness: WorkerReadiness::new(),
            session: SessionWorkerState {
                log_path: session_log_path,
                current: session,
                attachment_lease,
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
                tool_output_pressure_dirty: true,
                artifact_gc_dirty: true,
                task_guidance_retry_at: None,
                conversation_queue_retry_at: None,
                task_guidance_retry_attempts: 0,
                conversation_queue_retry_attempts: 0,
                task_guidance_retry_latched: false,
                conversation_queue_retry_latched: false,
                active_terminal_task_ids,
                terminal_lifecycle_generations,
                terminal_task_control_identities: BTreeMap::new(),
                pending_agent_result_continuations,
                last_queued_pre_turn_block: None,
                last_task_guidance_block: None,
                pending_queued_pre_turn_preparation: None,
                pending_cost_only_tool_output_aging: None,
                tool_artifact_read_budget: ToolArtifactReadBudgetV1::default(),
            },
            run: RunWorkerState {
                result_tx: WorkerEventPayloadSender::run(event_tx.clone()),
                active: None,
                route_execution_owner: None,
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
            artifact_gc: ArtifactGcWorkerState {
                result_tx: WorkerEventPayloadSender::artifact_gc(event_tx.clone()),
                tasks: ArtifactGcTaskManager::new(),
                next_request_id: 1,
                seen_deferred_notices: BTreeSet::new(),
            },
            refresh: RefreshWorkerState {
                provider_status_tasks: ProviderStatusTaskManager::new(),
                pending_mcp_servers: BTreeSet::new(),
                next_mcp_retry_at: Instant::now(),
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
            approval_command_receipts: BTreeMap::new(),
            approval_command_receipt_order: VecDeque::new(),
            last_observed_run_active: false,
            defer_startup_artifact_gc: true,
        }
    }

    pub(in crate::runner) fn remember_approval_command_receipt(
        &mut self,
        receipt: WorkerApprovalCommandReceipt,
    ) {
        let command_id = receipt.command_id.clone();
        if self
            .approval_command_receipts
            .insert(command_id.clone(), receipt)
            .is_none()
        {
            self.approval_command_receipt_order.push_back(command_id);
        }
        while self.approval_command_receipt_order.len() > MAX_APPROVAL_COMMAND_RECEIPTS {
            if let Some(expired_command_id) = self.approval_command_receipt_order.pop_front() {
                self.approval_command_receipts.remove(&expired_command_id);
            }
        }
    }

    pub(in crate::runner) fn clear_approval_command_receipts(&mut self) {
        self.approval_command_receipts.clear();
        self.approval_command_receipt_order.clear();
    }

    pub(in crate::runner) fn allocate_run_id(&mut self) -> u64 {
        let run_id = self.run.next_id;
        self.run.next_id = self.run.next_id.saturating_add(1);
        run_id
    }

    pub(in crate::runner) fn synchronize_route_execution_owner(
        &mut self,
    ) -> std::result::Result<(), String> {
        let provider_execution_active = self.run.active.is_some()
            || self.agent.background_runs.has_any()
            || !self.session.active_terminal_task_ids.is_empty();
        if !provider_execution_active {
            self.run.route_execution_owner = None;
            return Ok(());
        }
        let session_scope_id = self
            .session
            .current
            .as_ref()
            .map(|session| session.session_scope_id().to_owned());
        let Some(session_scope_id) = session_scope_id else {
            return Ok(());
        };
        self.acquire_route_execution_owner_for_scope(&session_scope_id)
    }

    pub(in crate::runner) fn acquire_route_execution_owner(
        &mut self,
    ) -> std::result::Result<(), String> {
        if self.run.route_execution_owner.is_some() {
            return Ok(());
        }
        let Some(session) = self.session.current.as_ref() else {
            // Some isolated test/runtime harnesses intentionally run without a durable session.
            // Production workers always install the routed session before reporting readiness.
            return Ok(());
        };
        let session_scope_id = session.session_scope_id().to_owned();
        self.acquire_route_execution_owner_for_scope(&session_scope_id)
    }

    pub(in crate::runner) fn acquire_route_execution_owner_for_scope(
        &mut self,
        session_scope_id: &str,
    ) -> std::result::Result<(), String> {
        if self.run.route_execution_owner.is_some() {
            return Ok(());
        }
        let authority = self
            .session
            .attachment_lease
            .route_mutation_authority(session_scope_id)
            .map_err(|error| format!("session route authority is unavailable: {error:#}"))?;
        self.run.route_execution_owner =
            Some(authority.acquire_execution_owner().map_err(|error| {
                format!("session route execution owner is unavailable: {error}")
            })?);
        Ok(())
    }

    pub(in crate::runner) fn nearest_deadline(&self) -> Option<Instant> {
        let mcp_deadline = (self.run.active.is_none()
            && !self.refresh.pending_mcp_servers.is_empty())
        .then_some(self.refresh.next_mcp_retry_at);
        [
            mcp_deadline,
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
    pub(in crate::runner) attachment_lease:
        Arc<sigil_runtime::interactive_session_attachment::InteractiveSessionAttachmentLease>,
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
    pub(in crate::runner) tool_output_pressure_dirty: bool,
    pub(in crate::runner) artifact_gc_dirty: bool,
    pub(in crate::runner) task_guidance_retry_at: Option<Instant>,
    pub(in crate::runner) conversation_queue_retry_at: Option<Instant>,
    pub(in crate::runner) task_guidance_retry_attempts: u8,
    pub(in crate::runner) conversation_queue_retry_attempts: u8,
    pub(in crate::runner) task_guidance_retry_latched: bool,
    pub(in crate::runner) conversation_queue_retry_latched: bool,
    pub(in crate::runner) active_terminal_task_ids: BTreeSet<TerminalTaskId>,
    pub(in crate::runner) terminal_lifecycle_generations: BTreeMap<TerminalTaskId, u64>,
    pub(in crate::runner) terminal_task_control_identities:
        BTreeMap<TerminalTaskId, TerminalTaskControlIdentity>,
    pub(in crate::runner) pending_agent_result_continuations: Vec<AgentThreadId>,
    pub(in crate::runner) last_queued_pre_turn_block: Option<(ConversationInputQueueId, String)>,
    pub(in crate::runner) last_task_guidance_block: Option<(ConversationInputQueueId, String)>,
    pub(in crate::runner) pending_queued_pre_turn_preparation:
        Option<PreTurnV2CompactionPreparation>,
    pub(in crate::runner) pending_cost_only_tool_output_aging:
        Option<sigil_kernel::ToolOutputAgingActivatedV1>,
    pub(in crate::runner) tool_artifact_read_budget: ToolArtifactReadBudgetV1,
}

impl SessionWorkerState {
    /// Starts one foreground root turn and keeps the same owner available to post-run TUI reads.
    pub(in crate::runner) fn begin_root_tool_artifact_read_budget(
        &mut self,
    ) -> ToolArtifactReadBudgetV1 {
        let budget = ToolArtifactReadBudgetV1::default();
        self.tool_artifact_read_budget = budget.clone();
        budget
    }
}

pub(in crate::runner) struct RunWorkerState {
    pub(in crate::runner) result_tx: WorkerEventPayloadSender<RunTaskResult>,
    pub(in crate::runner) active: Option<ActiveRun>,
    pub(in crate::runner) route_execution_owner:
        Option<sigil_runtime::provider_connections::SessionRouteExecutionOwner>,
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

pub(in crate::runner) struct ArtifactGcWorkerState {
    pub(in crate::runner) result_tx: WorkerEventPayloadSender<ArtifactGcTaskResult>,
    pub(in crate::runner) tasks: ArtifactGcTaskManager,
    pub(in crate::runner) next_request_id: u64,
    seen_deferred_notices: BTreeSet<String>,
}

impl ArtifactGcWorkerState {
    pub(in crate::runner) fn changed_deferred_notice(&mut self, notice: String) -> Option<String> {
        if self.seen_deferred_notices.contains(&notice)
            || self.seen_deferred_notices.len() >= MAX_ARTIFACT_GC_DEFERRED_NOTICES
        {
            return None;
        }
        self.seen_deferred_notices.insert(notice.clone());
        Some(notice)
    }

    pub(in crate::runner) fn clear_deferred_notice(&mut self) {
        self.seen_deferred_notices.clear();
    }
}

pub(in crate::runner) struct RefreshWorkerState {
    pub(in crate::runner) provider_status_tasks: ProviderStatusTaskManager,
    pub(in crate::runner) pending_mcp_servers: BTreeSet<String>,
    pub(in crate::runner) next_mcp_retry_at: Instant,
}

pub(in crate::runner) struct AgentWorkerState {
    pub(in crate::runner) supervisor: sigil_runtime::AgentSupervisor,
    pub(in crate::runner) background_runs: sigil_runtime::AgentToolBackgroundRuns,
    pub(in crate::runner) last_task_provider_route_diagnostics:
        sigil_runtime::TaskProviderRouteDiagnosticsSnapshot,
    pub(in crate::runner) last_task_completion_progress:
        sigil_runtime::TaskCompletionProgressSnapshot,
}

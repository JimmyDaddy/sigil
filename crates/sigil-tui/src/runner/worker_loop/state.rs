use super::*;
use crate::runner::ManagedTuiArtifactStoreLease;

const MAX_APPROVAL_COMMAND_RECEIPTS: usize = 256;
const MAX_ARTIFACT_GC_DEFERRED_NOTICES: usize = 32;

pub(in crate::runner) struct WorkerLoopState {
    pub(in crate::runner) plugin_hook_execution:
        Option<Arc<dyn sigil_runtime::ManagedPluginHookExecutionPortV1>>,
    pub(in crate::runner) stop_control: super::super::protocol::WorkerStopControl,
    pub(in crate::runner) event_tx: mpsc::Sender<WorkerEvent>,
    pub(in crate::runner) wake_coalescer: WorkerWakeCoalescer,
    pub(in crate::runner) terminal_lifecycle_router: ChannelTerminalLifecycleRouter,
    pub(in crate::runner) terminal_control: Option<sigil_tools_builtin::TerminalTaskControlHandle>,
    /// Session-scoped scratch lease registry shared with bash/terminal tools; session-delete
    /// cleanup uses it so live namespaces are never reclaimed.
    pub(in crate::runner) scratch_control: Option<sigil_tools_builtin::ScratchNamespaceControl>,
    pub(in crate::runner) readiness: WorkerReadiness,
    pub(in crate::runner) session: SessionWorkerState,
    pub(in crate::runner) managed_storage_writer: Option<
        std::sync::Arc<sigil_runtime::managed_storage_writer::ManagedStorageWriterAdapterV1>,
    >,
    pub(in crate::runner) managed_plan_review_child_resources: Option<
        std::sync::Arc<
            dyn sigil_runtime::plan_review_coordinator::PlanReviewChildResourceProvisionerV1,
        >,
    >,
    pub(in crate::runner) managed_artifact_store: Option<ManagedTuiArtifactStoreLease>,
    pub(in crate::runner) run: RunWorkerState,
    pub(in crate::runner) compaction: CompactionWorkerState,
    pub(in crate::runner) artifact_gc: ArtifactGcWorkerState,
    pub(in crate::runner) refresh: RefreshWorkerState,
    pub(in crate::runner) agent: AgentWorkerState,
    pub(in crate::runner) mcp_oauth: McpOAuthWorkerState,
    pub(in crate::runner) session_maintenance: SessionMaintenanceTaskManager,
    pub(in crate::runner) approval_command_receipts: BTreeMap<String, WorkerApprovalCommandReceipt>,
    approval_command_receipt_order: VecDeque<String>,
    pub(in crate::runner) last_observed_run_active: bool,
    /// Startup artifact GC is deferred until the first dispatched command so a user who resumes
    /// another session immediately after launch is never blocked on maintenance for the session
    /// they are about to abandon.
    pub(in crate::runner) defer_startup_artifact_gc: bool,
}

impl WorkerLoopState {
    #[cfg_attr(not(test), allow(dead_code))]
    #[allow(clippy::too_many_arguments)]
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
        managed_storage_writer: Option<
            std::sync::Arc<sigil_runtime::managed_storage_writer::ManagedStorageWriterAdapterV1>,
        >,
        managed_artifact_store: Option<ManagedTuiArtifactStoreLease>,
    ) -> Self {
        Self::new_with_optional_attachment(
            session_log_path,
            session,
            Some(attachment_lease),
            Some(agent_supervisor),
            background_agent_runs,
            event_tx,
            wake_coalescer,
            terminal_lifecycle_router,
            terminal_control,
            scratch_control,
            managed_storage_writer,
            managed_artifact_store,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub(in crate::runner) fn new_with_optional_attachment(
        session_log_path: PathBuf,
        session: Option<Session>,
        attachment_lease: Option<
            Arc<sigil_runtime::interactive_session_attachment::InteractiveSessionAttachmentLease>,
        >,
        agent_supervisor: Option<sigil_runtime::AgentSupervisor>,
        background_agent_runs: sigil_runtime::AgentToolBackgroundRuns,
        event_tx: mpsc::Sender<WorkerEvent>,
        wake_coalescer: WorkerWakeCoalescer,
        terminal_lifecycle_router: ChannelTerminalLifecycleRouter,
        terminal_control: Option<sigil_tools_builtin::TerminalTaskControlHandle>,
        scratch_control: Option<sigil_tools_builtin::ScratchNamespaceControl>,
        managed_storage_writer: Option<
            std::sync::Arc<sigil_runtime::managed_storage_writer::ManagedStorageWriterAdapterV1>,
        >,
        managed_artifact_store: Option<ManagedTuiArtifactStoreLease>,
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
            plugin_hook_execution: None,
            stop_control: Default::default(),
            event_tx: event_tx.clone(),
            wake_coalescer,
            terminal_lifecycle_router,
            terminal_control,
            scratch_control,
            managed_storage_writer,
            managed_plan_review_child_resources: None,
            managed_artifact_store,
            readiness: WorkerReadiness::new(),
            session: SessionWorkerState {
                log_path: session_log_path,
                application_operation_owner: session
                    .as_ref()
                    .and_then(|session| session.application_operation_owner().ok()),
                durable_read_handle: session
                    .as_ref()
                    .and_then(Session::durable_event_read_handle),
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
                returned_application_operations: BTreeMap::new(),
                result_tx: WorkerEventPayloadSender::run(event_tx.clone()),
                active: None,
                retired: Vec::new(),
                task_panicked: false,
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
                pending_plugin_surface: false,
                provider_status_tasks: ProviderStatusTaskManager::new(),
                pending_mcp_servers: BTreeSet::new(),
                next_mcp_retry_at: Instant::now(),
            },
            agent: AgentWorkerState {
                supervisor: agent_supervisor,
                background_runs: background_agent_runs,
                last_task_provider_route_diagnostics:
                    sigil_runtime::TaskProviderRouteDiagnosticsSnapshot::default(),
            },
            mcp_oauth: McpOAuthWorkerState {
                result_tx: WorkerEventPayloadSender::mcp_oauth(event_tx),
                active: BTreeMap::new(),
                retired: Vec::new(),
                task_panicked: false,
            },
            session_maintenance: SessionMaintenanceTaskManager::default(),
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
            || self.run.retired.iter().any(|handle| !handle.is_finished())
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
        let Some(attachment) = self.session.attachment_lease.as_ref() else {
            return Err("session route authority is unavailable".to_owned());
        };
        let authority = attachment
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
        // A root can publish its result before its closure finishes dropping owned work.
        // Keep the route owner until join completion and schedule reap only while such handles
        // exist; otherwise an idle inbox would never observe the final owner release.
        let retired_run_deadline =
            (!self.run.retired.is_empty()).then(|| Instant::now() + Duration::from_millis(10));
        [
            mcp_deadline,
            retired_run_deadline,
            self.session.projection_retry_at,
            self.session.task_guidance_retry_at,
            self.session.conversation_queue_retry_at,
        ]
        .into_iter()
        .flatten()
        .min()
    }
}

#[derive(Default)]
pub(in crate::runner) struct SessionMaintenanceTaskManager {
    tasks: Vec<tokio::task::JoinHandle<()>>,
    task_panicked: bool,
}

impl SessionMaintenanceTaskManager {
    pub(in crate::runner) fn request_stop(&self) {
        for task in &self.tasks {
            task.abort();
        }
    }

    pub(in crate::runner) fn shutdown_until(
        &mut self,
        runtime: &tokio::runtime::Runtime,
        deadline: Instant,
    ) -> Result<(), super::shutdown::OwnedTaskDrainFailure> {
        super::shutdown::drain_owned_tasks_until(
            &mut self.tasks,
            &mut self.task_panicked,
            runtime,
            deadline,
        )
    }

    pub(in crate::runner) fn spawn(
        &mut self,
        runtime: &tokio::runtime::Runtime,
        maintenance: sigil_runtime::application_run::ApplicationPostRunMaintenance,
    ) {
        let _ =
            super::shutdown::reap_finished_owned_tasks(&mut self.tasks, &mut self.task_panicked);
        self.tasks.push(runtime.spawn(async move {
            if let Err(error) = maintenance.execute().await {
                tracing::debug!(
                    %error,
                    "post-run semantic session maintenance was not applied"
                );
            }
        }));
    }
}

impl Drop for SessionMaintenanceTaskManager {
    fn drop(&mut self) {
        for task in &self.tasks {
            task.abort();
        }
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
    pub(in crate::runner) retired: Vec<tokio::task::JoinHandle<()>>,
    pub(in crate::runner) task_panicked: bool,
}

pub(in crate::runner) struct SessionWorkerState {
    pub(in crate::runner) application_operation_owner:
        Option<sigil_kernel::session::SessionApplicationOperationOwner>,
    pub(in crate::runner) log_path: PathBuf,
    /// Owner-derived reader retained while the durable session is temporarily moved into a
    /// foreground task. Detached lifecycle observations must use this coordinator instead of
    /// opening the JSONL path a second time while the task is appending.
    pub(in crate::runner) durable_read_handle:
        Option<sigil_kernel::session::SessionRecordReadHandle>,
    pub(in crate::runner) current: Option<Session>,
    pub(in crate::runner) attachment_lease: Option<
        Arc<sigil_runtime::interactive_session_attachment::InteractiveSessionAttachmentLease>,
    >,
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
    /// Identity metadata lives only until the original run handle has been joined.
    pub(in crate::runner) returned_application_operations:
        BTreeMap<tokio::task::Id, sigil_kernel::ApplicationOperationBindingV1>,
    pub(in crate::runner) retired: Vec<tokio::task::JoinHandle<()>>,
    pub(in crate::runner) task_panicked: bool,
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
    pub(in crate::runner) pending_plugin_surface: bool,
    pub(in crate::runner) provider_status_tasks: ProviderStatusTaskManager,
    pub(in crate::runner) pending_mcp_servers: BTreeSet<String>,
    pub(in crate::runner) next_mcp_retry_at: Instant,
}

pub(in crate::runner) struct AgentWorkerState {
    pub(in crate::runner) supervisor: Option<sigil_runtime::AgentSupervisor>,
    pub(in crate::runner) background_runs: sigil_runtime::AgentToolBackgroundRuns,
    pub(in crate::runner) last_task_provider_route_diagnostics:
        sigil_runtime::TaskProviderRouteDiagnosticsSnapshot,
}

#[cfg(test)]
mod shutdown_tests {
    use super::*;

    #[test]
    fn finished_session_maintenance_panic_is_latched_after_reaping() {
        let runtime =
            tokio::runtime::Runtime::new().expect("build maintenance shutdown test runtime");
        let handle = runtime.spawn(async { panic!("session maintenance fixture panic") });
        let deadline = Instant::now() + Duration::from_secs(1);
        while !handle.is_finished() && Instant::now() < deadline {
            std::thread::yield_now();
        }
        assert!(handle.is_finished());
        let mut manager = SessionMaintenanceTaskManager {
            tasks: vec![handle],
            task_panicked: false,
        };
        let _ = super::super::shutdown::reap_finished_owned_tasks(
            &mut manager.tasks,
            &mut manager.task_panicked,
        );
        assert!(manager.tasks.is_empty());
        for _ in 0..2 {
            manager.request_stop();
            assert!(matches!(
                manager.shutdown_until(&runtime, deadline),
                Err(super::super::shutdown::OwnedTaskDrainFailure::TaskPanicked)
            ));
        }
    }
}

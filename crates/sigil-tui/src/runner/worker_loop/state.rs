use super::*;

pub(in crate::runner) struct WorkerLoopState {
    pub(in crate::runner) session: SessionWorkerState,
    pub(in crate::runner) run: RunWorkerState,
    pub(in crate::runner) compaction: CompactionWorkerState,
    pub(in crate::runner) refresh: RefreshWorkerState,
    pub(in crate::runner) agent: AgentWorkerState,
    pub(in crate::runner) mcp_oauth: McpOAuthWorkerState,
    pub(in crate::runner) processed_worker_command_ids: BTreeSet<String>,
}

impl WorkerLoopState {
    pub(in crate::runner) fn new(
        session_log_path: PathBuf,
        session: Option<Session>,
        agent_supervisor: sigil_runtime::AgentSupervisor,
        background_agent_runs: sigil_runtime::AgentToolBackgroundRuns,
    ) -> Self {
        let pending_agent_result_continuations =
            pending_agent_result_continuations_from_session(session.as_ref());
        let (task_result_tx, task_result_rx) = mpsc::channel();
        let (provider_status_tx, provider_status_rx) = mpsc::channel();
        let (compaction_preparation_tx, compaction_preparation_rx) = mpsc::channel();
        let (mcp_oauth_result_tx, mcp_oauth_result_rx) = mpsc::channel();
        Self {
            session: SessionWorkerState {
                log_path: session_log_path,
                current: session,
                detached_durable_controls: Vec::new(),
                exact_prompts: ExactConversationPromptStore::new(),
                pending_agent_result_continuations,
                last_queued_pre_turn_block: None,
                pending_queued_pre_turn_preparation: None,
            },
            run: RunWorkerState {
                result_tx: task_result_tx,
                result_rx: task_result_rx,
                active: None,
                discarded_ids: BTreeSet::new(),
                next_id: 1,
                pending_task_handoffs: Vec::new(),
            },
            compaction: CompactionWorkerState {
                preparation_tx: compaction_preparation_tx,
                preparation_rx: compaction_preparation_rx,
                preparation_tasks: CompactionPreparationTaskManager::new(),
                next_request_id: 1,
                pending: None,
                idle_auto: IdleAutoCompactionState::default(),
            },
            refresh: RefreshWorkerState {
                provider_status_tx,
                provider_status_rx,
                provider_status_tasks: ProviderStatusTaskManager::new(),
                pending_mcp_servers: BTreeSet::new(),
                next_mcp_retry_at: Instant::now(),
                next_terminal_task_refresh_at: Instant::now(),
            },
            agent: AgentWorkerState {
                supervisor: agent_supervisor,
                background_runs: background_agent_runs,
            },
            mcp_oauth: McpOAuthWorkerState {
                result_tx: mcp_oauth_result_tx,
                result_rx: mcp_oauth_result_rx,
                active: BTreeMap::new(),
            },
            processed_worker_command_ids: BTreeSet::new(),
        }
    }
}

pub(in crate::runner) struct McpOAuthWorkerState {
    pub(in crate::runner) result_tx: mpsc::Sender<McpOAuthTaskResult>,
    pub(in crate::runner) result_rx: mpsc::Receiver<McpOAuthTaskResult>,
    pub(in crate::runner) active: BTreeMap<String, ActiveMcpOAuthFlow>,
}

pub(in crate::runner) struct SessionWorkerState {
    pub(in crate::runner) log_path: PathBuf,
    pub(in crate::runner) current: Option<Session>,
    pub(in crate::runner) detached_durable_controls: Vec<ControlEntry>,
    pub(in crate::runner) exact_prompts: ExactConversationPromptStore,
    pub(in crate::runner) pending_agent_result_continuations: Vec<AgentThreadId>,
    pub(in crate::runner) last_queued_pre_turn_block: Option<(ConversationInputQueueId, String)>,
    pub(in crate::runner) pending_queued_pre_turn_preparation:
        Option<PreTurnV2CompactionPreparation>,
}

pub(in crate::runner) struct RunWorkerState {
    pub(in crate::runner) result_tx: mpsc::Sender<RunTaskResult>,
    pub(in crate::runner) result_rx: mpsc::Receiver<RunTaskResult>,
    pub(in crate::runner) active: Option<ActiveRun>,
    pub(in crate::runner) discarded_ids: BTreeSet<u64>,
    pub(in crate::runner) next_id: u64,
    pub(in crate::runner) pending_task_handoffs: Vec<StartDurableTaskAction>,
}

pub(in crate::runner) struct CompactionWorkerState {
    pub(in crate::runner) preparation_tx: mpsc::Sender<CompactionPreparationTaskResult>,
    pub(in crate::runner) preparation_rx: mpsc::Receiver<CompactionPreparationTaskResult>,
    pub(in crate::runner) preparation_tasks: CompactionPreparationTaskManager,
    pub(in crate::runner) next_request_id: u64,
    pub(in crate::runner) pending: Option<PendingV2Compaction>,
    pub(in crate::runner) idle_auto: IdleAutoCompactionState,
}

pub(in crate::runner) struct RefreshWorkerState {
    pub(in crate::runner) provider_status_tx: mpsc::Sender<ProviderStatusTaskResult>,
    pub(in crate::runner) provider_status_rx: mpsc::Receiver<ProviderStatusTaskResult>,
    pub(in crate::runner) provider_status_tasks: ProviderStatusTaskManager,
    pub(in crate::runner) pending_mcp_servers: BTreeSet<String>,
    pub(in crate::runner) next_mcp_retry_at: Instant,
    pub(in crate::runner) next_terminal_task_refresh_at: Instant,
}

pub(in crate::runner) struct AgentWorkerState {
    pub(in crate::runner) supervisor: sigil_runtime::AgentSupervisor,
    pub(in crate::runner) background_runs: sigil_runtime::AgentToolBackgroundRuns,
}

use std::{
    cell::{Cell, RefCell},
    collections::{BTreeMap, BTreeSet, VecDeque},
    path::PathBuf,
    time::Instant,
};

use sigil_kernel::{
    ControlledCheckpointRestorePreview, ControlledCheckpointRestoreRequest,
    ConversationInputQueueId, ConversationInputQueuedEntry, EvidenceScope, ImageAttachment,
    ReasoningEffort, SessionLogEntry, SessionStats, TaskIntegrationReviewRequest, ToolCall,
};
use sigil_runtime::BalanceSnapshot;

use crate::{
    approval::{ApprovalAction, ApprovalDiffMode, PendingApproval},
    runner::WorkerCommand,
    sessions::{SessionHistoryEntry, SessionViewMode},
    timeline::RunPhase,
};

use super::{
    ActiveAgentChildTranscript, AgentView, ComposerMode, ComposerPasteSpan, ComposerQueueAction,
    MutationArtifactRetentionPreview, PendingPlanApproval, PendingUserInputForm, SessionViewCache,
    TimelineRenderStore, TimelineTextSelection, ToolActivityCacheEntry,
    egress_disclosure_flow::{EgressDisclosureCard, PendingEgressDisclosure},
    modal_flow::PendingModelPickerRefresh,
    runtime_status::{McpProgressState, McpServerRuntimeStatus},
    session_lifecycle_flow::SessionRetentionMaintenancePreview,
};

#[derive(Debug, Default)]
pub(crate) struct TimelineState {
    pub(in crate::app) expanded_thinking_entry_indices: BTreeSet<usize>,
    pub(in crate::app) collapsed_thinking_entry_indices: BTreeSet<usize>,
    pub(in crate::app) expanded_diagram_entry_indices: BTreeSet<usize>,
    pub(in crate::app) selected_tool_activity_key: Option<String>,
    pub(in crate::app) expanded_tool_activity_keys: BTreeSet<String>,
    pub(in crate::app) collapsed_tool_activity_keys: BTreeSet<String>,
    pub(in crate::app) tool_activity_visible_rows: BTreeMap<String, usize>,
    pub(in crate::app) streaming_assistant_index: Option<usize>,
    pub(in crate::app) streaming_reasoning_index: Option<usize>,
    pub(in crate::app) render_store: TimelineRenderStore,
    pub(in crate::app) text_selection: Option<TimelineTextSelection>,
    pub(in crate::app) text_selection_anchor: Option<usize>,
    pub(in crate::app) text_selection_anchor_column: Option<usize>,
    pub(in crate::app) revision: u64,
    pub(in crate::app) defer_renders: bool,
    pub(in crate::app) deferred_render_indexes: BTreeSet<usize>,
    pub(in crate::app) tool_activity_cache: Vec<ToolActivityCacheEntry>,
}

#[derive(Debug, Default)]
pub(crate) struct ReviewState {
    pub(in crate::app) checkpoint_restore_preview: Option<ControlledCheckpointRestorePreview>,
    pub(in crate::app) checkpoint_expected_request: Option<ControlledCheckpointRestoreRequest>,
    pub(in crate::app) checkpoint_request_id: Option<u64>,
    pub(in crate::app) checkpoint_action_pending: bool,
    pub(in crate::app) latest_checkpoint_restore_sequence: Option<u64>,
    pub(in crate::app) readiness_sequences_by_scope: BTreeMap<EvidenceScope, u64>,
    pub(in crate::app) verification_card_focused: bool,
    pub(in crate::app) verification_inspect_open: bool,
    pub(in crate::app) integration_review_request: Option<TaskIntegrationReviewRequest>,
    pub(in crate::app) integration_review_diff_lines: Vec<String>,
}

#[derive(Debug)]
pub(crate) struct AgentPanelState {
    pub(in crate::app) selected: usize,
    pub(in crate::app) active_view: AgentView,
    pub(in crate::app) active_child_transcript: Option<ActiveAgentChildTranscript>,
    // Values come only from SafePersist run-event projections, never provider-exact arguments.
    pub(in crate::app) safe_child_tool_calls: BTreeMap<(String, String), ToolCall>,
    // The first key component scopes reused provider call ids to one child session/thread.
    pub(in crate::app) child_tool_progress_execution_ids: BTreeMap<(String, String), String>,
    // Tracks the current invocation occurrence, including a failed audit placeholder that still
    // awaits its ToolResult. The occurrence carries its latest-80 render position separately from
    // its lifecycle kind so a reused provider call id can start a distinct logical card.
    pub(in crate::app) child_tool_card_entry_indices:
        BTreeMap<(String, String), ChildToolCardOccurrence>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::app) enum ChildToolCardOccurrence {
    Running(Option<usize>),
    TerminalPlaceholder(Option<usize>),
}

impl ChildToolCardOccurrence {
    pub(in crate::app) fn entry_index(self) -> Option<usize> {
        match self {
            Self::Running(index) | Self::TerminalPlaceholder(index) => index,
        }
    }

    pub(in crate::app) fn set_entry_index(&mut self, index: Option<usize>) {
        match self {
            Self::Running(current) | Self::TerminalPlaceholder(current) => *current = index,
        }
    }

    pub(in crate::app) fn is_running(self) -> bool {
        matches!(self, Self::Running(_))
    }
}

impl Default for AgentPanelState {
    fn default() -> Self {
        Self {
            selected: 0,
            active_view: AgentView::Main,
            active_child_transcript: None,
            safe_child_tool_calls: BTreeMap::new(),
            child_tool_progress_execution_ids: BTreeMap::new(),
            child_tool_card_entry_indices: BTreeMap::new(),
        }
    }
}

#[derive(Debug, Default)]
pub(crate) struct EgressDisclosureState {
    pub(in crate::app) pending: VecDeque<PendingEgressDisclosure>,
    pub(in crate::app) recent: Option<EgressDisclosureCard>,
    pub(in crate::app) rendered: Cell<bool>,
}

#[derive(Debug)]
pub(crate) struct RuntimeStatusState {
    pub(crate) provider_name: String,
    pub(crate) model_name: String,
    pub(crate) model_route: Option<sigil_kernel::ResolvedModelRoute>,
    pub(crate) permission_mode: String,
    pub(crate) memory_enabled: bool,
    pub(crate) memory_document_count: usize,
    pub(crate) memory_last_status: String,
    pub(crate) mutation_artifact_retention_preview: MutationArtifactRetentionPreview,
    pub(crate) session_retention_preview: SessionRetentionMaintenancePreview,
    pub(crate) compaction_status: String,
    pub(crate) code_intelligence_status: String,
    pub(crate) code_intelligence_server_lines: BTreeMap<String, String>,
    pub(crate) code_intelligence_diagnostics_line: Option<String>,
    pub(crate) code_intelligence_diagnostics_by_path:
        BTreeMap<String, crate::approval::ApprovalDiagnosticSummary>,
    pub(crate) mcp_server_statuses: BTreeMap<String, McpServerRuntimeStatus>,
    pub(crate) stats: SessionStats,
    pub(crate) session_delta_stats: SessionStats,
    pub(crate) is_busy: bool,
    pub(in crate::app) mcp_progress: Option<McpProgressState>,
    pub(crate) reasoning_effort: ReasoningEffort,
    pub(crate) run_phase: RunPhase,
    pub(crate) last_phase_marker: Option<String>,
    pub(crate) balance_snapshot: BalanceSnapshot,
    pub(crate) next_background_request_id: u64,
    pub(crate) pending_worker_commands: Vec<WorkerCommand>,
    pub(crate) pending_session_route_recovery_binding: Option<String>,
    pub(crate) pending_session_route_recovery_code: Option<sigil_kernel::PublicRouteRecoveryCode>,
    pub(crate) pending_session_route_recovery_target: Option<PathBuf>,
    pub(crate) pending_session_route_selection: Option<(String, sigil_kernel::ResolvedModelRoute)>,
    pub(crate) worker_rebind_required: bool,
    pub(crate) active_balance_refresh_id: Option<u64>,
    pub(in crate::app) active_model_picker_refresh: Option<PendingModelPickerRefresh>,
    pub(in crate::app) connection_model_catalog_views:
        BTreeMap<String, CachedConnectionModelCatalogView>,
    pub(in crate::app) setup_model_catalog_rx: Option<
        std::sync::mpsc::Receiver<
            Result<sigil_runtime::provider_connections::ModelCatalogResult, String>,
        >,
    >,
    pub(in crate::app) connection_inventory:
        Option<sigil_runtime::provider_connections::ConnectionInventory>,
    pub(in crate::app) connection_inventory_revision: u64,
    pub(in crate::app) connection_inventory_rx: Option<
        std::sync::mpsc::Receiver<(
            u64,
            sigil_runtime::provider_connections::ConnectionInventory,
        )>,
    >,
    #[cfg(not(test))]
    pub(in crate::app) connection_inventory_worker: Option<ConnectionInventoryWorker>,
    #[cfg(not(test))]
    pub(in crate::app) pending_connection_inventory_config: Option<sigil_kernel::RootConfig>,
    pub(crate) active_task: Option<ActiveTaskRuntimeStatus>,
    pub(crate) task_provider_route_diagnostics: sigil_runtime::TaskProviderRouteDiagnosticsSnapshot,
    pub(crate) task_completion_progress: sigil_runtime::TaskCompletionProgressSnapshot,
}

#[derive(Debug, Clone)]
pub(in crate::app) struct CachedConnectionModelCatalogView {
    pub(in crate::app) cached_at: Instant,
    pub(in crate::app) result: sigil_runtime::provider_connections::ModelCatalogResult,
}

#[cfg(not(test))]
#[derive(Debug)]
pub(in crate::app) struct ConnectionInventoryWorker {
    pub(in crate::app) cancelled: std::sync::Arc<std::sync::atomic::AtomicBool>,
    pub(in crate::app) handle: Option<std::thread::JoinHandle<()>>,
}

#[cfg(not(test))]
impl ConnectionInventoryWorker {
    pub(in crate::app) fn finish(mut self) {
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

#[cfg(not(test))]
impl Drop for ConnectionInventoryWorker {
    fn drop(&mut self) {
        self.cancelled
            .store(true, std::sync::atomic::Ordering::Release);
    }
}

#[derive(Debug, Clone)]
pub(crate) struct ActiveTaskRuntimeStatus {
    pub(crate) task_id: String,
    pub(crate) objective: String,
}

#[derive(Debug)]
pub(crate) struct ComposerState {
    pub(crate) input: String,
    pub(crate) mode: ComposerMode,
    pub(crate) pending_plan_approval: Option<PendingPlanApproval>,
    pub(crate) pending_user_input: Option<PendingUserInputForm>,
    pub(crate) input_history: Vec<String>,
    pub(crate) agent_panel_focused: bool,
    pub(crate) queue_panel_focused: bool,
    pub(crate) queue_selected: usize,
    pub(crate) queue_action_selected: ComposerQueueAction,
    pub(crate) queue_edit_target: Option<ConversationInputQueueId>,
    pub(crate) optimistic_queue_items: Vec<ConversationInputQueuedEntry>,
    pub(crate) deferred_queue_promotions: Vec<ConversationInputQueuedEntry>,
    pub(crate) next_optimistic_queue_id: u64,
    pub(crate) input_cursor: usize,
    pub(in crate::app) input_paste_spans: Vec<ComposerPasteSpan>,
    pub(crate) input_history_index: Option<usize>,
    pub(crate) input_history_draft: Option<String>,
    pub(crate) cleared_input_draft: Option<String>,
    pub(crate) input_kill_buffer: Option<String>,
    pub(crate) image_attachments: Vec<ImageAttachment>,
    pub(crate) selected_image_attachment: Option<usize>,
}

impl Default for ComposerState {
    fn default() -> Self {
        Self {
            input: String::new(),
            mode: ComposerMode::Build,
            pending_plan_approval: None,
            pending_user_input: None,
            input_history: Vec::new(),
            agent_panel_focused: false,
            queue_panel_focused: false,
            queue_selected: 0,
            queue_action_selected: ComposerQueueAction::KeepNext,
            queue_edit_target: None,
            optimistic_queue_items: Vec::new(),
            deferred_queue_promotions: Vec::new(),
            next_optimistic_queue_id: 1,
            input_cursor: 0,
            input_paste_spans: Vec::new(),
            input_history_index: None,
            input_history_draft: None,
            cleared_input_draft: None,
            input_kill_buffer: None,
            image_attachments: Vec::new(),
            selected_image_attachment: None,
        }
    }
}

#[derive(Debug)]
pub(crate) struct ApprovalState {
    pub(crate) pending: Option<PendingApproval>,
    pub(crate) scroll_back: usize,
    pub(crate) metadata_collapsed: bool,
    pub(crate) selected_file_index: usize,
    pub(crate) selected_hunk_index: usize,
    pub(crate) diff_mode: ApprovalDiffMode,
    pub(crate) selected_action: ApprovalAction,
}

impl Default for ApprovalState {
    fn default() -> Self {
        Self {
            pending: None,
            scroll_back: 0,
            metadata_collapsed: true,
            selected_file_index: 0,
            selected_hunk_index: 0,
            diff_mode: ApprovalDiffMode::Full,
            selected_action: ApprovalAction::Deny,
        }
    }
}

impl ApprovalState {
    pub(crate) fn has_actionable_pending(&self) -> bool {
        self.pending
            .as_ref()
            .is_some_and(PendingApproval::actions_available)
    }
}

#[derive(Debug)]
pub(crate) struct SessionBrowserState {
    pub(crate) history: Vec<SessionHistoryEntry>,
    pub(crate) history_visible_limit: usize,
    pub(crate) history_selected: usize,
    pub(crate) history_filter: String,
    pub(crate) view_mode: SessionViewMode,
    pub(crate) current_entries: Vec<SessionLogEntry>,
    pub(crate) current_entries_revision: u64,
    pub(in crate::app) view_cache: RefCell<SessionViewCache>,
}

impl Default for SessionBrowserState {
    fn default() -> Self {
        Self {
            history: Vec::new(),
            history_visible_limit: 9,
            history_selected: 0,
            history_filter: String::new(),
            view_mode: SessionViewMode::Provider,
            current_entries: Vec::new(),
            current_entries_revision: 0,
            view_cache: RefCell::new(SessionViewCache::default()),
        }
    }
}

use std::{cell::RefCell, collections::BTreeMap};

use sigil_kernel::{ConversationInputQueueId, ReasoningEffort, SessionLogEntry, SessionStats};
use sigil_runtime::BalanceSnapshot;

use crate::{
    approval::{ApprovalAction, ApprovalDiffMode, PendingApproval},
    runner::WorkerCommand,
    sessions::{SessionHistoryEntry, SessionViewMode},
    timeline::RunPhase,
};

use super::{
    ComposerMode, ComposerPasteSpan, ComposerQueueAction, MutationArtifactRetentionPreview,
    PendingPlanApproval, SessionViewCache,
    modal_flow::PendingModelPickerRefresh,
    runtime_status::{McpProgressState, McpServerRuntimeStatus},
};

#[derive(Debug)]
pub(crate) struct RuntimeStatusState {
    pub(crate) provider_name: String,
    pub(crate) model_name: String,
    pub(crate) permission_default_mode: String,
    pub(crate) memory_enabled: bool,
    pub(crate) memory_document_count: usize,
    pub(crate) memory_last_status: String,
    pub(crate) mutation_artifact_retention_preview: MutationArtifactRetentionPreview,
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
    pub(crate) active_balance_refresh_id: Option<u64>,
    pub(in crate::app) active_model_picker_refresh: Option<PendingModelPickerRefresh>,
}

#[derive(Debug)]
pub(crate) struct ComposerState {
    pub(crate) input: String,
    pub(crate) mode: ComposerMode,
    pub(crate) pending_plan_approval: Option<PendingPlanApproval>,
    pub(crate) input_history: Vec<String>,
    pub(crate) agent_panel_focused: bool,
    pub(crate) queue_panel_focused: bool,
    pub(crate) queue_selected: usize,
    pub(crate) queue_action_selected: ComposerQueueAction,
    pub(crate) queue_edit_target: Option<ConversationInputQueueId>,
    pub(crate) input_cursor: usize,
    pub(in crate::app) input_paste_spans: Vec<ComposerPasteSpan>,
    pub(crate) input_history_index: Option<usize>,
    pub(crate) input_history_draft: Option<String>,
    pub(crate) cleared_input_draft: Option<String>,
    pub(crate) input_kill_buffer: Option<String>,
}

impl Default for ComposerState {
    fn default() -> Self {
        Self {
            input: String::new(),
            mode: ComposerMode::Build,
            pending_plan_approval: None,
            input_history: Vec::new(),
            agent_panel_focused: false,
            queue_panel_focused: false,
            queue_selected: 0,
            queue_action_selected: ComposerQueueAction::KeepNext,
            queue_edit_target: None,
            input_cursor: 0,
            input_paste_spans: Vec::new(),
            input_history_index: None,
            input_history_draft: None,
            cleared_input_draft: None,
            input_kill_buffer: None,
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
            metadata_collapsed: false,
            selected_file_index: 0,
            selected_hunk_index: 0,
            diff_mode: ApprovalDiffMode::Full,
            selected_action: ApprovalAction::Deny,
        }
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

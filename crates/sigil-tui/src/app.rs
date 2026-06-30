use std::{
    cell::{Ref, RefCell},
    collections::{BTreeMap, BTreeSet, HashMap},
    ops::Range,
    path::{Path, PathBuf},
    time::SystemTime,
};

mod agent_flow;
mod approval_flow;
mod command_dispatch;
mod config_flow;
mod conversation_queue_flow;
mod diagnostics_flow;
mod formatting;
mod input_flow;
mod input_history;
mod modal_flow;
mod mouse_flow;
mod runtime_status;
mod session_flow;
mod setup_flow;
mod slash_flow;
pub(crate) mod task_sidebar;
mod timeline_flow;
mod tool_card_interaction;
mod worker_bridge;
mod workspace_trust_flow;

use anyhow::Result;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::text::Line;
use sigil_kernel::{
    AgentResultContinuationProjection, AgentThreadId, AgentThreadStateProjection, ApprovalMode,
    CompactionConfig, CompactionRecord, CompactionThresholdStatus, ConversationInputKind,
    ConversationInputQueueId, ConversationInputTarget, MemoryConfig, MutationArtifactCleanupTarget,
    MutationArtifactInventoryItem, MutationArtifactRetentionReport, PlanApprovalPermission,
    ReasoningEffort, RootConfig, SecretRedactor, Session, SessionConfig, SessionLogEntry,
    SessionStats, StorageConfig, TaskStateProjection, ToolPreviewSnapshot, plan_text_hash,
    resolve_workspace_root,
};
use sigil_runtime::{
    BalanceSnapshot, ContextWindowSource, SigilPaths, effective_compaction_config,
    resolve_context_window_tokens, resolve_sigil_paths, set_active_provider_model,
};
use uuid::Uuid;

pub(crate) use crate::approval::{
    ApprovalAction, ApprovalChangeSetSummary, ApprovalDiagnosticSummary, ApprovalDiffLine,
    ApprovalDiffLineKind, ApprovalFileRow, ApprovalModalView,
};
pub use crate::approval::{ApprovalDiffMode, PendingApproval};
use crate::commands::{UiCommand, command_for_key_event};
pub(crate) use crate::config_panel::ConfigState;
pub use crate::input::PaneFocus;
use crate::runner::{QueueMoveDirection, WorkerCommand};
pub use crate::sessions::{SessionHistoryEntry, SessionViewMode};
pub(crate) use crate::setup::{SetupField, SetupState};
use crate::slash::ResolvedSlashCommand;
pub use crate::timeline::{EventEntry, TimelineEntry, TimelineRole};
pub(crate) use crate::timeline::{
    LiveActivitySummary, RunPhase, SessionHistoryRow, SidebarCard, ThinkingBlockMode,
    ToolActivityCacheEntry,
};
pub(crate) use crate::workspace_trust::WorkspaceTrustGateState;

use self::config_flow::cycle_approval_mode;
use self::formatting::*;
use self::modal_flow::{ModalState, ModelPickerRefresh, PendingModelPickerRefresh};
use self::runtime_status::{McpProgressState, ResolvedUsageCostCurrency};
pub(crate) use self::runtime_status::{
    McpServerRuntimeStatus, TimelineTextSelection, code_intelligence_config_status,
    diagnostic_summary_label, initial_mcp_server_status, initial_mcp_server_statuses,
};
use self::session_flow::{current_focus_label, short_session_token};

const SESSION_HISTORY_TITLE_SCAN_LIMIT: usize = 256;
pub(crate) const SCRATCH_DIR_LABEL: &str = "cache/tmp";

#[derive(Debug, Clone, PartialEq, Eq)]
enum AgentView {
    Main,
    Child {
        child_task_id: String,
        child_session_ref: sigil_kernel::SessionRef,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ComposerQueueAction {
    SendNow,
    KeepNext,
    Edit,
    Delete,
}

impl ComposerQueueAction {
    pub(crate) const ORDER: [Self; 4] = [Self::SendNow, Self::KeepNext, Self::Edit, Self::Delete];

    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::SendNow => "Send now",
            Self::KeepNext => "Keep next",
            Self::Edit => "Edit",
            Self::Delete => "Delete",
        }
    }

    pub(crate) fn detail(self) -> &'static str {
        match self {
            Self::SendNow => "interrupt current turn",
            Self::KeepNext => "run after current turn",
            Self::Edit => "edit queued input",
            Self::Delete => "remove queued input",
        }
    }

    pub(crate) fn is_destructive(self) -> bool {
        matches!(self, Self::Delete)
    }

    fn next(self, forward: bool) -> Self {
        let current = Self::ORDER
            .iter()
            .position(|action| *action == self)
            .unwrap_or(0);
        let len = Self::ORDER.len();
        let next = if forward {
            (current + 1) % len
        } else {
            current.checked_sub(1).unwrap_or(len - 1)
        };
        Self::ORDER[next]
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ComposerMode {
    Build,
    Plan,
}

impl ComposerMode {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Build => "Build",
            Self::Plan => "Plan",
        }
    }

    fn notice(self) -> &'static str {
        match self {
            Self::Build => "thinking",
            Self::Plan => "planning",
        }
    }

    fn phase_marker(self) -> &'static str {
        match self {
            Self::Build => "thinking",
            Self::Plan => "plan",
        }
    }
}

#[derive(Debug, Clone)]
struct ActiveAgentChildTranscript {
    path: PathBuf,
    file_signature: ChildTranscriptFileSignature,
    timeline_entries: Vec<TimelineEntry>,
    rendered_body_lines: Vec<Line<'static>>,
    total_timeline_entries: usize,
    transcript_truncated: bool,
    load_error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ChildTranscriptFileSignature {
    len: u64,
    modified: Option<SystemTime>,
}

impl ChildTranscriptFileSignature {
    fn empty() -> Self {
        Self {
            len: 0,
            modified: None,
        }
    }
}

#[derive(Debug, Clone)]
struct AgentSidebarItem {
    label: String,
    detail: String,
    target: Option<AgentView>,
    thread_id: Option<sigil_kernel::AgentThreadId>,
    muted: bool,
}

#[derive(Debug, Clone, Default)]
struct SessionViewCache {
    entries_len: usize,
    entries_revision: u64,
    task_projection: TaskStateProjection,
    agent_projection: AgentThreadStateProjection,
    task_sidebar_lines: Vec<String>,
    task_strip_view: Option<task_sidebar::TaskStripView>,
    agent_child_items: Vec<AgentSidebarItem>,
    agent_graph_summary_line: Option<String>,
    compaction_preview_line: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PendingPlanApproval {
    pub(crate) plan_text: String,
    pub(crate) plan_hash: String,
    pub(crate) scope_summary: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ComposerPasteSpan {
    start: usize,
    end: usize,
    char_count: usize,
    line_count: usize,
}

#[derive(Debug, Clone)]
pub(crate) enum MutationArtifactRetentionPreview {
    Pending,
    Ready {
        report: MutationArtifactRetentionReport,
        artifacts: Vec<MutationArtifactInventoryItem>,
    },
    Unavailable(String),
}

#[derive(Debug)]
pub struct AppState {
    pub config_path: PathBuf,
    pub workspace_root: PathBuf,
    pub sigil_paths: SigilPaths,
    pub session_log_dir: PathBuf,
    pub session_log_path: PathBuf,
    pub provider_name: String,
    pub model_name: String,
    pub permission_default_mode: String,
    pub memory_enabled: bool,
    pub memory_document_count: usize,
    pub memory_last_status: String,
    mutation_artifact_retention_preview: MutationArtifactRetentionPreview,
    pub compaction_status: String,
    pub code_intelligence_status: String,
    pub code_intelligence_server_lines: BTreeMap<String, String>,
    pub code_intelligence_diagnostics_line: Option<String>,
    pub(crate) code_intelligence_diagnostics_by_path: BTreeMap<String, ApprovalDiagnosticSummary>,
    pub(crate) mcp_server_statuses: BTreeMap<String, McpServerRuntimeStatus>,
    pub session_id: String,
    pub input: String,
    composer_mode: ComposerMode,
    pending_plan_approval: Option<PendingPlanApproval>,
    pub input_history: Vec<String>,
    pub timeline: Vec<TimelineEntry>,
    pub events: Vec<EventEntry>,
    pub stats: SessionStats,
    pub session_delta_stats: SessionStats,
    pub should_quit: bool,
    pub is_busy: bool,
    pub pending_approval: Option<PendingApproval>,
    pub active_pane: PaneFocus,
    pub timeline_scroll_back: usize,
    pub approval_scroll_back: usize,
    pub activity_scroll_back: usize,
    info_rail_detail: bool,
    pub session_history: Vec<SessionHistoryEntry>,
    pub session_history_visible_limit: usize,
    pub session_history_selected: usize,
    pub session_history_filter: String,
    config_snapshot: Option<RootConfig>,
    secret_redactor: SecretRedactor,
    setup_state: Option<SetupState>,
    workspace_trust_gate_state: Option<WorkspaceTrustGateState>,
    config_state: Option<ConfigState>,
    modal_state: Option<ModalState>,
    session_view_mode: SessionViewMode,
    current_session_entries: Vec<SessionLogEntry>,
    current_session_entries_revision: u64,
    session_view_cache: RefCell<SessionViewCache>,
    tool_preview_snapshots: HashMap<String, ToolPreviewSnapshot>,
    latest_compaction_record: Option<CompactionRecord>,
    compaction_config: CompactionConfig,
    memory_config: MemoryConfig,
    thinking_block_mode: ThinkingBlockMode,
    expanded_thinking_entry_indices: BTreeSet<usize>,
    collapsed_thinking_entry_indices: BTreeSet<usize>,
    selected_tool_activity_key: Option<String>,
    expanded_tool_activity_keys: BTreeSet<String>,
    collapsed_tool_activity_keys: BTreeSet<String>,
    pending_terminal_cancel_confirmation: Option<String>,
    pending_mouse_slash_confirmation: Option<ResolvedSlashCommand>,
    mouse_hover_target: Option<crate::mouse::HitTarget>,
    pending_mouse_left_down: bool,
    pending_tool_card_body_click_entry: Option<usize>,
    last_notice: Option<String>,
    mcp_progress: Option<McpProgressState>,
    reasoning_effort: ReasoningEffort,
    run_phase: RunPhase,
    last_phase_marker: Option<String>,
    streaming_assistant_index: Option<usize>,
    streaming_reasoning_index: Option<usize>,
    timeline_render_cache: Vec<Line<'static>>,
    timeline_plain_cache: Vec<String>,
    timeline_prefix_hashes: Vec<u64>,
    timeline_render_ranges: Vec<Range<usize>>,
    timeline_text_selection: Option<TimelineTextSelection>,
    timeline_text_selection_anchor: Option<usize>,
    timeline_text_selection_anchor_column: Option<usize>,
    timeline_revision: u64,
    defer_timeline_renders: bool,
    deferred_timeline_render_indexes: BTreeSet<usize>,
    tool_activity_cache: Vec<ToolActivityCacheEntry>,
    usage_sidebar_cache: Vec<String>,
    sidebar_selected_card: SidebarCard,
    sidebar_agent_selected: usize,
    composer_agent_panel_focused: bool,
    composer_queue_panel_focused: bool,
    composer_queue_selected: usize,
    composer_queue_action_selected: ComposerQueueAction,
    queue_edit_target: Option<ConversationInputQueueId>,
    active_agent_view: AgentView,
    active_agent_child_transcript: Option<ActiveAgentChildTranscript>,
    balance_snapshot: BalanceSnapshot,
    next_background_request_id: u64,
    pending_worker_commands: Vec<WorkerCommand>,
    active_balance_refresh_id: Option<u64>,
    active_model_picker_refresh: Option<PendingModelPickerRefresh>,
    terminal_width: u16,
    terminal_height: u16,
    input_cursor: usize,
    input_paste_spans: Vec<ComposerPasteSpan>,
    input_history_index: Option<usize>,
    input_history_draft: Option<String>,
    cleared_input_draft: Option<String>,
    input_kill_buffer: Option<String>,
    approval_metadata_collapsed: bool,
    approval_selected_file_index: usize,
    approval_selected_hunk_index: usize,
    approval_diff_mode: ApprovalDiffMode,
    approval_selected_action: ApprovalAction,
    slash_selector_index: usize,
}

#[derive(Debug, Clone)]
pub enum AppAction {
    SubmitPrompt(String),
    QueueConversationInput {
        prompt: String,
        kind: ConversationInputKind,
        target: ConversationInputTarget,
    },
    CancelQueuedConversationInput {
        queue_id: ConversationInputQueueId,
    },
    EditQueuedConversationInput {
        queue_id: ConversationInputQueueId,
        prompt: String,
    },
    MoveQueuedConversationInput {
        queue_id: ConversationInputQueueId,
        direction: QueueMoveDirection,
    },
    PromoteQueuedConversationInput {
        queue_id: ConversationInputQueueId,
    },
    SendQueuedConversationInputNow {
        queue_id: ConversationInputQueueId,
    },
    SetConversationQueuePaused {
        paused: bool,
    },
    SubmitPlanPrompt(String),
    ApprovePlan {
        plan_text: String,
        permission: PlanApprovalPermission,
        scope_summary: String,
        clear_planning_context: bool,
    },
    SubmitTask(String),
    InvokeInlineSkill {
        skill_id: String,
        arguments: String,
    },
    InvokeChildSessionSkill {
        skill_id: String,
        arguments: String,
    },
    InvokeAgentProfile {
        profile_id: String,
        prompt: String,
        parent_prompt: String,
    },
    ContinueTask {
        task_id: Option<String>,
        guidance: Option<String>,
    },
    ApprovalDecision {
        call_id: String,
        approved: bool,
    },
    ApprovalDecisionWithArgs {
        call_id: String,
        args_json: String,
    },
    BackgroundActiveAgent,
    CancelRun,
    CancelTerminalTask {
        task_id: String,
    },
    CloseAgent {
        thread_id: AgentThreadId,
        reason: Option<String>,
    },
    MessageAgent {
        thread_id: AgentThreadId,
        prompt: String,
    },
    CopyToClipboard {
        text: String,
    },
    CompactNow,
    CheckChangedFilesDiagnostics,
    CleanMutationArtifacts {
        target: MutationArtifactCleanupTarget,
    },
    DeleteMutationArtifact {
        artifact_id: String,
    },
    TrustWorkspace,
    ApproveVerificationCheck {
        check_spec_id: String,
    },
    SandboxVerificationCheck {
        check_spec_id: String,
    },
    ActivateLazyMcp {
        server_name: Option<String>,
    },
    RefreshMcpServer {
        server_name: String,
    },
    StartNewSession {
        session_log_path: PathBuf,
    },
    SwitchSession {
        session_log_path: PathBuf,
    },
    RuntimeConfigUpdated {
        root_config: Box<RootConfig>,
    },
    SetupCompleted {
        config_path: PathBuf,
        root_config: Box<RootConfig>,
    },
    ConfigSaved {
        root_config: Box<RootConfig>,
    },
}

impl AppState {
    pub fn from_root_config(config_path: &Path, root_config: &RootConfig) -> Self {
        let launch_cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        let workspace_root =
            resolve_workspace_root(config_path, &launch_cwd, &root_config.workspace.root);
        let sigil_paths =
            resolve_sigil_paths(&root_config.storage, &root_config.session, &workspace_root);
        let session_log_dir = sigil_paths.session_log_dir.clone();
        let session_id = Uuid::new_v4().to_string();
        let permission_default_mode = root_config.permission.default_mode.as_str().to_owned();
        let initial_compaction_status = effective_compaction_config(
            &root_config.agent.provider,
            &root_config.agent.model,
            &root_config.compaction,
        )
        .threshold_status(0)
        .as_str()
        .to_owned();
        let initial_code_intelligence_status =
            code_intelligence_config_status(&root_config.code_intelligence);

        let mut app = Self {
            config_path: config_path.to_path_buf(),
            workspace_root,
            sigil_paths,
            session_log_dir,
            session_log_path: PathBuf::new(),
            provider_name: root_config.agent.provider.clone(),
            model_name: root_config.agent.model.clone(),
            permission_default_mode,
            memory_enabled: root_config.memory.enabled,
            memory_document_count: 0,
            memory_last_status: "pending".to_owned(),
            mutation_artifact_retention_preview: MutationArtifactRetentionPreview::Pending,
            compaction_status: initial_compaction_status,
            code_intelligence_status: initial_code_intelligence_status,
            code_intelligence_server_lines: BTreeMap::new(),
            code_intelligence_diagnostics_line: None,
            code_intelligence_diagnostics_by_path: BTreeMap::new(),
            mcp_server_statuses: initial_mcp_server_statuses(root_config),
            session_id,
            input: String::new(),
            composer_mode: ComposerMode::Build,
            pending_plan_approval: None,
            input_history: Vec::new(),
            timeline: Vec::new(),
            events: Vec::new(),
            stats: SessionStats::default(),
            session_delta_stats: SessionStats::default(),
            should_quit: false,
            is_busy: false,
            pending_approval: None,
            active_pane: PaneFocus::Composer,
            timeline_scroll_back: 0,
            approval_scroll_back: 0,
            activity_scroll_back: 0,
            info_rail_detail: false,
            session_history: Vec::new(),
            session_history_visible_limit: 9,
            session_history_selected: 0,
            session_history_filter: String::new(),
            config_snapshot: Some(root_config.clone()),
            secret_redactor: sigil_runtime::secret_redactor_for_root_config(root_config),
            setup_state: None,
            workspace_trust_gate_state: None,
            config_state: None,
            modal_state: None,
            session_view_mode: SessionViewMode::Provider,
            current_session_entries: Vec::new(),
            current_session_entries_revision: 0,
            session_view_cache: RefCell::new(SessionViewCache::default()),
            tool_preview_snapshots: HashMap::new(),
            latest_compaction_record: None,
            compaction_config: root_config.compaction.clone(),
            memory_config: root_config.memory.clone(),
            thinking_block_mode: ThinkingBlockMode::Collapsed,
            expanded_thinking_entry_indices: BTreeSet::new(),
            collapsed_thinking_entry_indices: BTreeSet::new(),
            selected_tool_activity_key: None,
            expanded_tool_activity_keys: BTreeSet::new(),
            collapsed_tool_activity_keys: BTreeSet::new(),
            pending_terminal_cancel_confirmation: None,
            pending_mouse_slash_confirmation: None,
            mouse_hover_target: None,
            pending_mouse_left_down: false,
            pending_tool_card_body_click_entry: None,
            last_notice: None,
            mcp_progress: None,
            reasoning_effort: ReasoningEffort::Max,
            run_phase: RunPhase::Idle,
            last_phase_marker: None,
            streaming_assistant_index: None,
            streaming_reasoning_index: None,
            timeline_render_cache: Vec::new(),
            timeline_plain_cache: Vec::new(),
            timeline_prefix_hashes: Vec::new(),
            timeline_render_ranges: Vec::new(),
            timeline_text_selection: None,
            timeline_text_selection_anchor: None,
            timeline_text_selection_anchor_column: None,
            timeline_revision: 0,
            defer_timeline_renders: false,
            deferred_timeline_render_indexes: BTreeSet::new(),
            tool_activity_cache: Vec::new(),
            usage_sidebar_cache: Vec::new(),
            sidebar_selected_card: SidebarCard::Permission,
            sidebar_agent_selected: 0,
            composer_agent_panel_focused: false,
            composer_queue_panel_focused: false,
            composer_queue_selected: 0,
            composer_queue_action_selected: ComposerQueueAction::SendNow,
            queue_edit_target: None,
            active_agent_view: AgentView::Main,
            active_agent_child_transcript: None,
            balance_snapshot: BalanceSnapshot {
                status: "pending".to_owned(),
                ..BalanceSnapshot::default()
            },
            next_background_request_id: 1,
            pending_worker_commands: Vec::new(),
            active_balance_refresh_id: None,
            active_model_picker_refresh: None,
            terminal_width: 120,
            terminal_height: 32,
            input_cursor: 0,
            input_paste_spans: Vec::new(),
            input_history_index: None,
            input_history_draft: None,
            cleared_input_draft: None,
            input_kill_buffer: None,
            approval_metadata_collapsed: false,
            approval_selected_file_index: 0,
            approval_selected_hunk_index: 0,
            approval_diff_mode: ApprovalDiffMode::Full,
            approval_selected_action: ApprovalAction::Deny,
            slash_selector_index: 0,
        };
        app.session_log_path = app
            .session_log_dir
            .join(format!("session-{}.jsonl", app.session_id));
        app.load_input_history();
        app.refresh_memory_summary();
        app.recompute_compaction_status(false);
        app.refresh_session_history();
        app.bootstrap();
        app.schedule_balance_refresh();
        app.refresh_usage_sidebar_cache();
        app
    }

    pub fn from_setup(
        config_path: PathBuf,
        workspace_root: PathBuf,
        startup_error: Option<String>,
    ) -> Self {
        let sigil_paths = resolve_sigil_paths(
            &StorageConfig::default(),
            &SessionConfig::default(),
            &workspace_root,
        );
        let session_log_dir = sigil_paths.session_log_dir.clone();
        let session_id = Uuid::new_v4().to_string();
        let mut app = Self {
            config_path: config_path.clone(),
            workspace_root: workspace_root.clone(),
            sigil_paths,
            session_log_dir,
            session_log_path: PathBuf::new(),
            provider_name: "deepseek".to_owned(),
            model_name: "deepseek-v4-flash".to_owned(),
            permission_default_mode: ApprovalMode::Ask.as_str().to_owned(),
            memory_enabled: true,
            memory_document_count: 0,
            memory_last_status: "pending".to_owned(),
            mutation_artifact_retention_preview: MutationArtifactRetentionPreview::Pending,
            compaction_status: CompactionThresholdStatus::NotAvailable.as_str().to_owned(),
            code_intelligence_status: "off".to_owned(),
            code_intelligence_server_lines: BTreeMap::new(),
            code_intelligence_diagnostics_line: None,
            code_intelligence_diagnostics_by_path: BTreeMap::new(),
            mcp_server_statuses: BTreeMap::new(),
            session_id,
            input: String::new(),
            composer_mode: ComposerMode::Build,
            pending_plan_approval: None,
            input_history: Vec::new(),
            timeline: Vec::new(),
            events: Vec::new(),
            stats: SessionStats::default(),
            session_delta_stats: SessionStats::default(),
            should_quit: false,
            is_busy: false,
            pending_approval: None,
            active_pane: PaneFocus::Composer,
            timeline_scroll_back: 0,
            approval_scroll_back: 0,
            activity_scroll_back: 0,
            info_rail_detail: false,
            session_history: Vec::new(),
            session_history_visible_limit: 9,
            session_history_selected: 0,
            session_history_filter: String::new(),
            config_snapshot: None,
            secret_redactor: SecretRedactor::default(),
            setup_state: Some(SetupState::new(config_path, startup_error.clone())),
            workspace_trust_gate_state: None,
            config_state: None,
            modal_state: None,
            session_view_mode: SessionViewMode::Provider,
            current_session_entries: Vec::new(),
            current_session_entries_revision: 0,
            session_view_cache: RefCell::new(SessionViewCache::default()),
            tool_preview_snapshots: HashMap::new(),
            latest_compaction_record: None,
            compaction_config: CompactionConfig::default(),
            memory_config: MemoryConfig::default(),
            thinking_block_mode: ThinkingBlockMode::Collapsed,
            expanded_thinking_entry_indices: BTreeSet::new(),
            collapsed_thinking_entry_indices: BTreeSet::new(),
            selected_tool_activity_key: None,
            expanded_tool_activity_keys: BTreeSet::new(),
            collapsed_tool_activity_keys: BTreeSet::new(),
            pending_terminal_cancel_confirmation: None,
            pending_mouse_slash_confirmation: None,
            mouse_hover_target: None,
            pending_mouse_left_down: false,
            pending_tool_card_body_click_entry: None,
            last_notice: startup_error,
            mcp_progress: None,
            reasoning_effort: ReasoningEffort::Max,
            run_phase: RunPhase::Idle,
            last_phase_marker: None,
            streaming_assistant_index: None,
            streaming_reasoning_index: None,
            timeline_render_cache: Vec::new(),
            timeline_plain_cache: Vec::new(),
            timeline_prefix_hashes: Vec::new(),
            timeline_render_ranges: Vec::new(),
            timeline_text_selection: None,
            timeline_text_selection_anchor: None,
            timeline_text_selection_anchor_column: None,
            timeline_revision: 0,
            defer_timeline_renders: false,
            deferred_timeline_render_indexes: BTreeSet::new(),
            tool_activity_cache: Vec::new(),
            usage_sidebar_cache: Vec::new(),
            sidebar_selected_card: SidebarCard::Permission,
            sidebar_agent_selected: 0,
            composer_agent_panel_focused: false,
            composer_queue_panel_focused: false,
            composer_queue_selected: 0,
            composer_queue_action_selected: ComposerQueueAction::SendNow,
            queue_edit_target: None,
            active_agent_view: AgentView::Main,
            active_agent_child_transcript: None,
            balance_snapshot: BalanceSnapshot {
                status: "missing auth".to_owned(),
                ..BalanceSnapshot::default()
            },
            next_background_request_id: 1,
            pending_worker_commands: Vec::new(),
            active_balance_refresh_id: None,
            active_model_picker_refresh: None,
            terminal_width: 120,
            terminal_height: 32,
            input_cursor: 0,
            input_paste_spans: Vec::new(),
            input_history_index: None,
            input_history_draft: None,
            cleared_input_draft: None,
            input_kill_buffer: None,
            approval_metadata_collapsed: false,
            approval_selected_file_index: 0,
            approval_selected_hunk_index: 0,
            approval_diff_mode: ApprovalDiffMode::Full,
            approval_selected_action: ApprovalAction::Deny,
            slash_selector_index: 0,
        };
        app.session_log_path = app
            .session_log_dir
            .join(format!("session-{}.jsonl", app.session_id));
        app.load_input_history();
        app.bootstrap_setup();
        app.refresh_usage_sidebar_cache();
        app
    }

    pub(crate) fn code_intelligence_sidebar_lines(&self) -> Vec<String> {
        if self.code_intelligence_server_lines.is_empty()
            && self.code_intelligence_diagnostics_line.is_none()
            && self.code_intelligence_diagnostics_by_path.is_empty()
        {
            return vec![format!("status: {}", self.code_intelligence_status)];
        }
        let mut lines = self
            .code_intelligence_server_lines
            .values()
            .cloned()
            .collect::<Vec<_>>();
        if let Some(line) = &self.code_intelligence_diagnostics_line {
            lines.push(line.clone());
        }
        lines.extend(self.code_intelligence_diagnostic_file_lines());
        lines
    }

    fn code_intelligence_diagnostic_file_lines(&self) -> Vec<String> {
        if self.code_intelligence_diagnostics_by_path.is_empty() {
            return Vec::new();
        }
        const MAX_DIAGNOSTIC_FILES: usize = 4;
        let mut summaries = self
            .code_intelligence_diagnostics_by_path
            .iter()
            .map(|(path, summary)| (path.as_str(), *summary))
            .collect::<Vec<_>>();
        summaries.sort_by(|(left_path, left), (right_path, right)| {
            right
                .errors
                .cmp(&left.errors)
                .then_with(|| right.warnings.cmp(&left.warnings))
                .then_with(|| left_path.cmp(right_path))
        });

        let mut lines = vec![format!("latest diagnostics: {} files", summaries.len())];
        lines.extend(
            summaries
                .iter()
                .take(MAX_DIAGNOSTIC_FILES)
                .map(|(path, summary)| format!("{path}: {}", diagnostic_summary_label(*summary))),
        );
        let hidden = summaries.len().saturating_sub(MAX_DIAGNOSTIC_FILES);
        if hidden > 0 {
            lines.push(format!("+{hidden} more files"));
        }
        lines
    }

    fn bootstrap(&mut self) {
        self.timeline.clear();
        self.tool_activity_cache.clear();
        self.events.clear();
        self.ensure_scratch_dir();
        self.push_timeline(TimelineRole::System, "sigil ready.");
        self.push_event("session", format!("active {}", self.session_id));
        self.push_event("workspace", self.workspace_root.display().to_string());
        self.push_event(
            "model",
            format!("{}/{}", self.provider_name, self.model_name),
        );
        self.push_event("effort", self.reasoning_effort.as_str());
        self.push_event("approval_default", self.permission_default_mode.clone());
        self.push_event(
            "memory",
            format!(
                "enabled={} docs={} status={}",
                self.memory_enabled, self.memory_document_count, self.memory_last_status
            ),
        );
        self.push_event("compaction", self.compaction_status.clone());
        self.push_event("code_intelligence", self.code_intelligence_status.clone());
        self.push_event("session_log", self.session_log_path.display().to_string());
        self.push_event("focus", self.active_pane.label());
        self.reset_scroll();
    }

    fn bootstrap_setup(&mut self) {
        self.timeline.clear();
        self.tool_activity_cache.clear();
        self.events.clear();
        self.ensure_scratch_dir();
        self.push_timeline(TimelineRole::System, "quick setup");
        self.push_timeline(TimelineRole::Notice, "launch dir = workspace");
        if let Some(error) = self
            .setup_state
            .as_ref()
            .and_then(|state| state.startup_error.clone())
        {
            self.push_timeline(TimelineRole::Notice, format!("load failed: {error}"));
        }
        self.push_event("mode", "setup");
        if let Some(state) = &self.setup_state {
            self.push_event("config_path", state.config_path.display().to_string());
        }
        self.push_event("workspace", self.workspace_root.display().to_string());
        self.reset_scroll();
    }

    fn ensure_scratch_dir(&mut self) {
        let temp_dir = self.sigil_paths.scratch_root.clone();
        match std::fs::create_dir_all(&temp_dir) {
            Ok(()) => self.push_event("scratch", SCRATCH_DIR_LABEL),
            Err(error) => self.push_event(
                "scratch",
                format!("failed to create {}: {error}", temp_dir.display()),
            ),
        }
    }

    fn reset_for_new_session(&mut self, provider_name: String, model_name: String, notice: String) {
        self.provider_name = provider_name;
        self.model_name = model_name;
        self.session_id = Uuid::new_v4().to_string();
        self.session_log_path = self
            .session_log_dir
            .join(format!("session-{}.jsonl", self.session_id));
        self.stats = SessionStats::default();
        self.session_delta_stats = SessionStats::default();
        self.is_busy = false;
        self.pending_approval = None;
        self.active_pane = PaneFocus::Composer;
        self.timeline_scroll_back = 0;
        self.approval_scroll_back = 0;
        self.activity_scroll_back = 0;
        self.current_session_entries.clear();
        self.mark_current_session_entries_changed();
        self.tool_preview_snapshots.clear();
        self.latest_compaction_record = None;
        self.run_phase = RunPhase::Idle;
        self.last_phase_marker = None;
        self.streaming_assistant_index = None;
        self.streaming_reasoning_index = None;
        self.approval_metadata_collapsed = false;
        self.approval_selected_file_index = 0;
        self.approval_selected_hunk_index = 0;
        self.approval_diff_mode = ApprovalDiffMode::Full;
        self.approval_selected_action = ApprovalAction::Deny;
        self.selected_tool_activity_key = None;
        self.expanded_thinking_entry_indices.clear();
        self.collapsed_thinking_entry_indices.clear();
        self.expanded_tool_activity_keys.clear();
        self.collapsed_tool_activity_keys.clear();
        self.pending_terminal_cancel_confirmation = None;
        self.pending_mouse_slash_confirmation = None;
        self.mouse_hover_target = None;
        self.pending_mouse_left_down = false;
        self.pending_tool_card_body_click_entry = None;
        self.active_agent_view = AgentView::Main;
        self.active_agent_child_transcript = None;
        self.composer_agent_panel_focused = false;
        self.cleared_input_draft = None;
        self.input_kill_buffer = None;
        self.input_paste_spans.clear();
        self.bootstrap();
        self.last_notice = Some(notice.clone());
        self.push_timeline(TimelineRole::Notice, notice);
        self.refresh_session_history();
        self.refresh_usage_sidebar_cache();
    }

    fn new_session_log_path(&self) -> PathBuf {
        self.session_log_dir
            .join(format!("session-{}.jsonl", Uuid::new_v4()))
    }

    pub fn handle_key_event(&mut self, key: KeyEvent) -> Result<Option<AppAction>> {
        if self.is_workspace_trust_gate_mode() {
            return self.handle_workspace_trust_gate_key_event(key);
        }
        if self.is_setup_mode() {
            return self.handle_setup_key_event(key);
        }
        if self.is_config_mode() {
            return self.handle_config_key_event(key);
        }

        if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
            if let Some(text) = self.selected_timeline_text() {
                self.last_notice = Some(format!(
                    "copy pending {}",
                    timeline_flow::clipboard_copy_status(&text)
                ));
                self.push_event(
                    "selection:copy",
                    format!("pending {}", timeline_flow::clipboard_copy_status(&text)),
                );
                return Ok(Some(AppAction::CopyToClipboard { text }));
            }
            self.modal_state = None;
            if self.is_busy {
                self.last_notice = Some("cancellation requested".to_owned());
                self.push_timeline(TimelineRole::Notice, "cancel requested");
                return Ok(Some(AppAction::CancelRun));
            }
            self.should_quit = true;
            return Ok(None);
        }
        if self.has_modal() {
            let outcome = if key.code == KeyCode::Enter {
                self.submit_modal()
            } else {
                self.handle_modal_key_event(key)
            };
            self.apply_modal_outcome(outcome);
            return Ok(None);
        }

        if key.code == KeyCode::Esc && key.modifiers.is_empty() && self.is_busy {
            self.modal_state = None;
            self.last_notice = Some("cancellation requested".to_owned());
            self.push_timeline(TimelineRole::Notice, "cancel requested");
            return Ok(Some(AppAction::CancelRun));
        }

        if let Some(outcome) = self.handle_pending_approval_key_event(key) {
            return Ok(outcome);
        }

        if let Some(outcome) = self.handle_pending_plan_approval_key_event(key) {
            return Ok(Some(outcome));
        }

        if self.is_busy
            && self.pending_approval.is_none()
            && matches!(self.run_phase, RunPhase::Agent(_))
            && matches!(key.code, KeyCode::Char('b') | KeyCode::Char('B'))
            && has_control_without_alt(key)
        {
            self.last_notice = Some("agent background requested".to_owned());
            self.push_timeline(TimelineRole::Notice, "agent background requested");
            self.push_event("agent", "background requested");
            return Ok(Some(AppAction::BackgroundActiveAgent));
        }

        if self.active_pane == PaneFocus::Activity
            && self.pending_approval.is_none()
            && !key.modifiers.contains(KeyModifiers::CONTROL)
        {
            match key.code {
                KeyCode::Char(character)
                    if key.modifiers.is_empty()
                        && normalize_command_prefix_character(character).is_some() =>
                {
                    self.active_pane = PaneFocus::Composer;
                    self.insert_input_character('/');
                    self.reset_input_history_navigation();
                    self.reset_slash_selector();
                    return Ok(None);
                }
                KeyCode::BackTab if self.sidebar_selected_card == SidebarCard::Permission => {
                    return self.toggle_runtime_permission_mode();
                }
                KeyCode::Up => {
                    self.move_sidebar_selection(false);
                    return Ok(None);
                }
                KeyCode::Down => {
                    self.move_sidebar_selection(true);
                    return Ok(None);
                }
                KeyCode::Enter if self.sidebar_selected_card == SidebarCard::Agents => {
                    self.activate_selected_agent_view();
                    return Ok(None);
                }
                KeyCode::Esc => {
                    if self.handle_ui_command(UiCommand::ClearToolCardFocus) {
                        return Ok(None);
                    }
                    self.active_pane = PaneFocus::Composer;
                    return Ok(None);
                }
                KeyCode::Char(_) | KeyCode::Backspace | KeyCode::Left | KeyCode::Right
                    if key.modifiers.is_empty() =>
                {
                    return Ok(None);
                }
                _ => {}
            }
        }

        if let Some(command) = command_for_key_event(key) {
            if command == UiCommand::CheckChangedFilesDiagnostics {
                return Ok(self.request_changed_files_diagnostics());
            }
            if command == UiCommand::CancelFocusedTerminalTask {
                return Ok(self.request_focused_terminal_task_cancel());
            }
            self.handle_ui_command(command);
            return Ok(None);
        }

        match key.code {
            KeyCode::Char('u')
                if key.modifiers.contains(KeyModifiers::CONTROL)
                    && self.pending_approval.is_none() =>
            {
                self.scroll_timeline(self.transcript_page_step());
            }
            KeyCode::Char('d')
                if key.modifiers.contains(KeyModifiers::CONTROL)
                    && self.pending_approval.is_none() =>
            {
                self.unscroll_timeline(self.transcript_page_step());
            }
            KeyCode::Char('p')
                if key.modifiers.contains(KeyModifiers::CONTROL)
                    && self.pending_approval.is_none() =>
            {
                self.navigate_input_history(true);
            }
            KeyCode::Char('n')
                if key.modifiers.contains(KeyModifiers::CONTROL)
                    && self.pending_approval.is_none() =>
            {
                self.navigate_input_history(false);
            }
            KeyCode::Home
                if key.modifiers.contains(KeyModifiers::CONTROL)
                    && self.pending_approval.is_none() =>
            {
                self.scroll_timeline_to_top();
            }
            KeyCode::End
                if key.modifiers.contains(KeyModifiers::CONTROL)
                    && self.pending_approval.is_none() =>
            {
                self.unscroll_timeline(usize::MAX / 2);
            }
            KeyCode::Char('t')
                if key.modifiers.contains(KeyModifiers::CONTROL)
                    && self.pending_approval.is_none() =>
            {
                if self.active_pane != PaneFocus::Activity && self.has_collapsible_thinking_blocks()
                {
                    self.toggle_thinking_block_mode();
                    return Ok(None);
                }
                if self.selected_tool_activity_key.is_some() && self.toggle_selected_tool_card() {
                    return Ok(None);
                }
                if self.has_collapsible_thinking_blocks() {
                    self.toggle_thinking_block_mode();
                }
            }
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {}
            KeyCode::Char('z') | KeyCode::Char('Z')
                if self.active_pane == PaneFocus::Composer && has_control_without_alt(key) =>
            {
                if self.restore_cleared_input_draft() {
                    self.reset_input_history_navigation();
                    self.reset_slash_selector();
                    self.last_notice = Some("draft restored".to_owned());
                }
            }
            KeyCode::Char('a') | KeyCode::Char('A')
                if self.active_pane == PaneFocus::Composer && has_control_without_alt(key) =>
            {
                self.move_input_cursor_line_start();
            }
            KeyCode::Char('e') | KeyCode::Char('E')
                if self.active_pane == PaneFocus::Composer && has_control_without_alt(key) =>
            {
                self.move_input_cursor_line_end();
            }
            KeyCode::Char('b') | KeyCode::Char('B')
                if self.active_pane == PaneFocus::Composer && has_control_without_alt(key) =>
            {
                self.move_input_cursor_left();
            }
            KeyCode::Char('f') | KeyCode::Char('F')
                if self.active_pane == PaneFocus::Composer && has_control_without_alt(key) =>
            {
                self.move_input_cursor_right();
            }
            KeyCode::Char('h') | KeyCode::Char('H')
                if self.active_pane == PaneFocus::Composer && has_control_without_alt(key) =>
            {
                self.remove_input_character_before_cursor();
                self.reset_input_history_navigation();
                self.reset_slash_selector();
            }
            KeyCode::Char('w') | KeyCode::Char('W')
                if self.active_pane == PaneFocus::Composer && has_control_without_alt(key) =>
            {
                self.remove_input_word_before_cursor();
                self.reset_input_history_navigation();
                self.reset_slash_selector();
            }
            KeyCode::Char('k') | KeyCode::Char('K')
                if self.active_pane == PaneFocus::Composer && has_control_without_alt(key) =>
            {
                self.kill_input_to_line_end();
                self.reset_input_history_navigation();
                self.reset_slash_selector();
            }
            KeyCode::Char('y') | KeyCode::Char('Y')
                if self.active_pane == PaneFocus::Composer && has_control_without_alt(key) =>
            {
                self.yank_input_kill_buffer();
                self.reset_input_history_navigation();
                self.reset_slash_selector();
            }
            KeyCode::Char('j') | KeyCode::Char('J')
                if self.active_pane == PaneFocus::Composer && has_control_without_alt(key) =>
            {
                self.insert_input_character('\n');
                self.reset_input_history_navigation();
                self.reset_slash_selector();
            }
            KeyCode::Char('b') | KeyCode::Char('B')
                if self.active_pane == PaneFocus::Composer && has_alt_without_control(key) =>
            {
                self.move_input_cursor_word_left();
            }
            KeyCode::Char('f') | KeyCode::Char('F')
                if self.active_pane == PaneFocus::Composer && has_alt_without_control(key) =>
            {
                self.move_input_cursor_word_right();
            }
            KeyCode::Tab
                if self.active_pane == PaneFocus::Composer && self.has_slash_selector() =>
            {
                self.accept_slash_selector();
            }
            KeyCode::BackTab
                if self.active_pane == PaneFocus::Composer && self.has_slash_selector() =>
            {
                self.move_slash_selector(false);
            }
            KeyCode::Tab if self.composer_queue_panel_focused => {
                self.cycle_composer_queue_action(true);
            }
            KeyCode::BackTab if self.composer_queue_panel_focused => {
                self.cycle_composer_queue_action(false);
            }
            KeyCode::Right if self.composer_queue_panel_focused && key.modifiers.is_empty() => {
                self.cycle_composer_queue_action(true);
            }
            KeyCode::Left if self.composer_queue_panel_focused && key.modifiers.is_empty() => {
                self.cycle_composer_queue_action(false);
            }
            KeyCode::Tab => {}
            KeyCode::BackTab if self.pending_approval.is_none() => {
                return self.toggle_runtime_permission_mode();
            }
            KeyCode::Up
                if self.composer_queue_panel_focused
                    && has_alt_without_control(key)
                    && self.selected_composer_queue_is_first() =>
            {
                return Ok(self.move_selected_queue_item(QueueMoveDirection::Up));
            }
            KeyCode::Down
                if self.composer_queue_panel_focused
                    && has_alt_without_control(key)
                    && self.selected_composer_queue_is_last() =>
            {
                return Ok(self.move_selected_queue_item(QueueMoveDirection::Down));
            }
            KeyCode::Up if self.composer_queue_panel_focused && has_alt_without_control(key) => {
                return Ok(self.move_selected_queue_item(QueueMoveDirection::Up));
            }
            KeyCode::Down if self.composer_queue_panel_focused && has_alt_without_control(key) => {
                return Ok(self.move_selected_queue_item(QueueMoveDirection::Down));
            }
            KeyCode::Up if self.composer_queue_panel_focused && key.modifiers.is_empty() => {
                if self.selected_composer_queue_is_first() {
                    self.blur_composer_queue_panel();
                } else {
                    self.move_composer_queue_selection(false);
                }
            }
            KeyCode::Down if self.composer_queue_panel_focused && key.modifiers.is_empty() => {
                if self.selected_composer_queue_is_last() && self.focus_composer_agent_panel() {
                    self.blur_composer_queue_panel();
                } else {
                    self.move_composer_queue_selection(true);
                }
            }
            KeyCode::Esc if self.composer_queue_panel_focused && key.modifiers.is_empty() => {
                self.blur_composer_queue_panel();
            }
            KeyCode::Enter if self.composer_queue_panel_focused && key.modifiers.is_empty() => {
                return Ok(self.execute_selected_queue_action());
            }
            KeyCode::Backspace | KeyCode::Delete
                if self.composer_queue_panel_focused && key.modifiers.is_empty() =>
            {
                return Ok(self.cancel_selected_queue_item());
            }
            KeyCode::Char(_) if self.composer_queue_panel_focused && key.modifiers.is_empty() => {}
            KeyCode::Up if self.composer_agent_panel_focused && key.modifiers.is_empty() => {
                if self.selected_composer_agent_is_first() {
                    if !self.focus_composer_queue_panel() {
                        self.blur_composer_agent_panel();
                    }
                } else {
                    self.move_composer_agent_selection(false);
                }
            }
            KeyCode::Down if self.composer_agent_panel_focused && key.modifiers.is_empty() => {
                self.move_composer_agent_selection(true);
            }
            KeyCode::Esc if self.composer_agent_panel_focused && key.modifiers.is_empty() => {
                self.blur_composer_agent_panel();
            }
            KeyCode::Char('c') | KeyCode::Char('C')
                if self.composer_agent_panel_focused && key.modifiers.is_empty() =>
            {
                return self.close_selected_agent_from_panel();
            }
            KeyCode::Char('m') | KeyCode::Char('M')
                if self.composer_agent_panel_focused && key.modifiers.is_empty() =>
            {
                self.begin_message_selected_agent_from_panel();
            }
            KeyCode::Enter if self.composer_agent_panel_focused && key.modifiers.is_empty() => {
                self.activate_selected_agent_view();
            }
            KeyCode::Up
                if self.active_pane == PaneFocus::Composer
                    && self.input_history_index.is_some()
                    && key.modifiers.is_empty() =>
            {
                self.navigate_input_history(true);
            }
            KeyCode::Down
                if self.active_pane == PaneFocus::Composer
                    && self.input_history_index.is_some()
                    && key.modifiers.is_empty() =>
            {
                self.navigate_input_history(false);
            }
            KeyCode::Up if self.active_pane == PaneFocus::Composer && self.has_slash_selector() => {
                self.move_slash_selector(false)
            }
            KeyCode::Down
                if self.active_pane == PaneFocus::Composer && self.has_slash_selector() =>
            {
                self.move_slash_selector(true)
            }
            KeyCode::Up if self.active_pane == PaneFocus::Composer => {
                if self.input_cursor_visual_row() == 0 {
                    self.navigate_input_history(true);
                } else {
                    self.move_input_cursor_vertical(true);
                }
            }
            KeyCode::Down if self.active_pane == PaneFocus::Composer => {
                if self.input_cursor_visual_row() == self.input_last_visual_row() {
                    if self.input_history_index.is_some()
                        || (!self.focus_composer_queue_panel()
                            && !self.focus_composer_agent_panel())
                    {
                        self.navigate_input_history(false);
                    }
                } else {
                    self.move_input_cursor_vertical(false);
                }
            }
            KeyCode::Home if self.active_pane == PaneFocus::Composer => {
                self.move_input_cursor_home()
            }
            KeyCode::End if self.active_pane == PaneFocus::Composer => self.move_input_cursor_end(),
            KeyCode::Left
                if self.active_pane == PaneFocus::Composer
                    && (has_control_without_alt(key) || has_alt_without_control(key)) =>
            {
                self.move_input_cursor_word_left();
            }
            KeyCode::Right
                if self.active_pane == PaneFocus::Composer
                    && (has_control_without_alt(key) || has_alt_without_control(key)) =>
            {
                self.move_input_cursor_word_right();
            }
            KeyCode::PageUp => self.scroll_timeline(self.transcript_page_step()),
            KeyCode::PageDown => self.unscroll_timeline(self.transcript_page_step()),
            KeyCode::Home => self.scroll_timeline_to_top(),
            KeyCode::End => self.unscroll_timeline(usize::MAX / 2),
            KeyCode::Esc => {
                if self.input.is_empty() && self.handle_ui_command(UiCommand::ClearToolCardFocus) {
                    return Ok(None);
                }
                if self.cancel_queue_edit() {
                    self.clear_input_preserving_draft();
                    self.reset_input_history_navigation();
                    self.reset_slash_selector();
                    self.active_pane = PaneFocus::Composer;
                    return Ok(None);
                }
                if self.input.is_empty() && self.composer_mode == ComposerMode::Plan {
                    self.composer_mode = ComposerMode::Build;
                    self.last_notice = Some("build mode".to_owned());
                    self.push_event("mode", "build");
                    return Ok(None);
                }
                self.blur_composer_aux_panels();
                self.clear_input_preserving_draft();
                self.reset_input_history_navigation();
                self.reset_slash_selector();
                self.active_pane = PaneFocus::Composer;
            }
            KeyCode::Left if self.active_pane == PaneFocus::Composer => {
                self.move_input_cursor_left()
            }
            KeyCode::Right if self.active_pane == PaneFocus::Composer => {
                self.move_input_cursor_right()
            }
            KeyCode::Backspace
                if self.active_pane == PaneFocus::Composer
                    && (has_control_without_alt(key) || has_alt_without_control(key)) =>
            {
                self.remove_input_word_before_cursor();
                self.reset_input_history_navigation();
                self.reset_slash_selector();
            }
            KeyCode::Backspace => {
                self.active_pane = PaneFocus::Composer;
                self.blur_composer_aux_panels();
                self.remove_input_character_before_cursor();
                self.reset_input_history_navigation();
                self.reset_slash_selector();
            }
            KeyCode::Delete
                if self.active_pane == PaneFocus::Composer
                    && (has_control_without_alt(key) || has_alt_without_control(key)) =>
            {
                self.remove_input_word_after_cursor();
                self.reset_input_history_navigation();
                self.reset_slash_selector();
            }
            KeyCode::Delete if self.active_pane == PaneFocus::Composer => {
                self.remove_input_character_at_cursor();
                self.reset_input_history_navigation();
                self.reset_slash_selector();
            }
            _ if self.active_pane == PaneFocus::Composer && is_composer_newline_key(key) => {
                self.active_pane = PaneFocus::Composer;
                self.blur_composer_aux_panels();
                self.insert_input_character('\n');
                self.reset_input_history_navigation();
                self.reset_slash_selector();
            }
            _ if is_composer_submit_key(key) => {
                self.active_pane = PaneFocus::Composer;
                self.blur_composer_aux_panels();
                if self.should_accept_slash_selector_on_enter() {
                    self.accept_slash_selector();
                    return Ok(None);
                }
                return self.submit_input();
            }
            KeyCode::Char(character) if is_composer_text_key(key) => {
                self.active_pane = PaneFocus::Composer;
                self.blur_composer_aux_panels();
                let normalized = if normalize_command_prefix_character(character).is_some()
                    && self.input.trim().is_empty()
                {
                    '/'
                } else {
                    character
                };
                self.insert_input_character(normalized);
                self.reset_input_history_navigation();
                self.reset_slash_selector();
            }
            _ => {}
        }
        Ok(None)
    }

    fn handle_pending_plan_approval_key_event(&mut self, key: KeyEvent) -> Option<AppAction> {
        self.pending_plan_approval.as_ref()?;
        if !key.modifiers.is_empty() {
            return None;
        }
        match key.code {
            KeyCode::Char('a') | KeyCode::Char('A') => {
                self.approve_pending_plan(PlanApprovalPermission::Ask)
            }
            KeyCode::Char('w') | KeyCode::Char('W') => {
                self.approve_pending_plan(PlanApprovalPermission::WorkspaceEdits)
            }
            KeyCode::Char('c') | KeyCode::Char('C') => {
                self.clear_pending_plan_approval();
                self.composer_mode = ComposerMode::Plan;
                self.last_notice = Some("continue planning".to_owned());
                self.push_event("plan", "continue");
                None
            }
            KeyCode::Esc | KeyCode::Char('d') | KeyCode::Char('D') => {
                self.clear_pending_plan_approval();
                self.last_notice = Some("plan approval dismissed".to_owned());
                self.push_event("plan", "dismissed");
                None
            }
            _ => None,
        }
    }

    fn approve_pending_plan(&mut self, permission: PlanApprovalPermission) -> Option<AppAction> {
        let pending = self.pending_plan_approval.take()?;
        self.last_notice = Some(match permission {
            PlanApprovalPermission::Ask => "approving plan: ask".to_owned(),
            PlanApprovalPermission::WorkspaceEdits => "approving plan: workspace edits".to_owned(),
        });
        self.push_event("plan", "approve");
        Some(AppAction::ApprovePlan {
            plan_text: pending.plan_text,
            permission,
            scope_summary: pending.scope_summary,
            clear_planning_context: true,
        })
    }

    pub fn cache_hit_ratio(&self) -> f64 {
        let total = self.stats.cache_hit_tokens + self.stats.cache_miss_tokens;
        if total == 0 {
            0.0
        } else {
            self.stats.cache_hit_tokens as f64 / total as f64
        }
    }

    pub fn last_notice(&self) -> Option<&str> {
        self.last_notice.as_deref()
    }

    pub fn terminal_mouse_capture_enabled(&self) -> bool {
        self.config_snapshot
            .as_ref()
            .is_some_and(|config| config.terminal.mouse_capture)
    }

    pub fn terminal_keyboard_enhancement_enabled(&self) -> bool {
        self.config_snapshot
            .as_ref()
            .is_some_and(|config| config.terminal.keyboard_enhancement)
    }

    pub fn terminal_osc52_clipboard_enabled(&self) -> bool {
        self.config_snapshot
            .as_ref()
            .is_none_or(|config| config.terminal.osc52_clipboard)
    }

    pub fn terminal_scroll_sensitivity(&self) -> usize {
        self.config_snapshot
            .as_ref()
            .map(|config| config.terminal.scroll_sensitivity as usize)
            .unwrap_or(sigil_kernel::config::DEFAULT_TERMINAL_SCROLL_SENSITIVITY as usize)
            .max(1)
    }

    pub(crate) fn root_config_snapshot(&self) -> Option<&RootConfig> {
        self.config_snapshot.as_ref()
    }

    pub fn set_terminal_size(&mut self, width: u16, height: u16) -> bool {
        let next_width = width.max(3);
        let next_height = height.max(8);
        let height_changed = self.terminal_height != next_height;
        let width_changed = self.terminal_width != next_width;
        self.terminal_width = next_width;
        self.terminal_height = next_height;
        self.clamp_input_cursor();
        if width_changed {
            self.rebuild_timeline_render_cache();
            self.rerender_active_agent_child_transcript();
        }
        self.timeline_scroll_back = self
            .timeline_scroll_back
            .min(self.max_timeline_scroll_back());
        width_changed || height_changed
    }

    pub(crate) fn footer_strip_height(&self) -> u16 {
        let desired = self
            .composer_height()
            .saturating_add(self.composer_agent_panel_rows());
        desired.min(self.terminal_height.saturating_sub(2).max(4))
    }

    pub fn submit_input(&mut self) -> Result<Option<AppAction>> {
        let prompt = self.input.trim().to_owned();
        if prompt.is_empty() {
            return Ok(None);
        }
        self.discard_cleared_input_draft();
        self.record_input_history(prompt.clone());
        self.reset_input_history_navigation();

        if self.queue_edit_target.is_some() {
            return Ok(self.finish_queue_edit_submission(prompt));
        }

        if prompt.starts_with('/') {
            let Some(command) = self.resolve_slash_command(&prompt) else {
                self.push_timeline(TimelineRole::Notice, "unknown slash command");
                self.push_event("slash:unknown", prompt.clone());
                self.last_notice = Some("unknown slash command".to_owned());
                return Ok(None);
            };

            return self.execute_slash_command(command, prompt);
        }

        if prompt.trim_start().starts_with('@') {
            if self.is_busy {
                self.input.clear();
                self.input_cursor = 0;
                self.input_paste_spans.clear();
                self.reset_slash_selector();
                self.push_timeline(TimelineRole::Notice, "queued for next turn");
                self.push_event("queue", format!("queued busy input {prompt}"));
                self.last_notice = Some("queued for next turn".to_owned());
                return Ok(Some(AppAction::QueueConversationInput {
                    prompt,
                    kind: ConversationInputKind::Chat,
                    target: ConversationInputTarget::MainThread,
                }));
            }
            let (profile_id, agent_prompt) = match self.resolve_agent_mention_invocation(&prompt) {
                Ok(invocation) => invocation,
                Err(error) => {
                    let notice = error.to_string();
                    self.push_timeline(TimelineRole::Notice, notice.clone());
                    self.push_event("agent:unknown", prompt.clone());
                    self.last_notice = Some(notice);
                    return Ok(None);
                }
            };
            return Ok(Some(self.start_agent_profile_invocation(
                profile_id,
                agent_prompt,
                prompt,
            )));
        }

        if self.is_busy {
            self.input.clear();
            self.input_cursor = 0;
            self.input_paste_spans.clear();
            self.reset_slash_selector();
            self.push_timeline(TimelineRole::Notice, "queued for next turn");
            self.push_event("queue", format!("queued busy input {prompt}"));
            self.last_notice = Some("queued for next turn".to_owned());
            return Ok(Some(AppAction::QueueConversationInput {
                prompt,
                kind: ConversationInputKind::Chat,
                target: ConversationInputTarget::MainThread,
            }));
        }

        self.clear_pending_plan_approval();

        if self.composer_mode == ComposerMode::Plan {
            self.input.clear();
            self.input_cursor = 0;
            self.input_paste_spans.clear();
            self.reset_slash_selector();
            self.timeline_scroll_back = 0;
            self.push_timeline(TimelineRole::User, prompt.clone());
            self.push_event("input", format!("submitted plan prompt {prompt}"));
            self.active_pane = PaneFocus::Composer;
            self.push_event("focus", current_focus_label(self));
            self.is_busy = true;
            self.run_phase = RunPhase::Thinking;
            self.last_notice = Some(ComposerMode::Plan.notice().to_owned());
            self.last_phase_marker = None;
            self.push_phase_marker(format!(
                "{}|{}",
                ComposerMode::Plan.phase_marker(),
                self.model_name
            ));
            self.streaming_assistant_index = None;
            self.streaming_reasoning_index = None;
            self.composer_mode = ComposerMode::Build;
            self.refresh_usage_sidebar_cache();
            return Ok(Some(AppAction::SubmitPlanPrompt(prompt)));
        }

        self.input.clear();
        self.input_cursor = 0;
        self.input_paste_spans.clear();
        self.reset_slash_selector();
        self.timeline_scroll_back = 0;
        self.push_timeline(TimelineRole::User, prompt.clone());
        self.push_event("input", format!("submitted {prompt}"));
        self.active_pane = PaneFocus::Composer;
        self.push_event("focus", current_focus_label(self));
        self.is_busy = true;
        self.run_phase = RunPhase::Thinking;
        self.last_notice = Some("thinking".to_owned());
        self.last_phase_marker = None;
        self.push_phase_marker(format!("thinking|{}", self.model_name));
        self.streaming_assistant_index = None;
        self.streaming_reasoning_index = None;
        self.refresh_usage_sidebar_cache();
        Ok(Some(AppAction::SubmitPrompt(prompt)))
    }

    pub(super) fn execute_slash_command(
        &mut self,
        command: ResolvedSlashCommand,
        prompt: String,
    ) -> Result<Option<AppAction>> {
        self.input.clear();
        self.input_cursor = 0;
        self.input_paste_spans.clear();
        self.pending_mouse_slash_confirmation = None;
        self.reset_slash_selector();
        self.push_event("slash", prompt.clone());
        match command.canonical.as_str() {
            "/compact" => {
                if self.is_busy {
                    self.push_timeline(TimelineRole::Notice, "busy; compact later");
                    Ok(None)
                } else {
                    self.last_notice = Some("compact requested".to_owned());
                    Ok(Some(AppAction::CompactNow))
                }
            }
            "/config" => {
                self.open_config_panel();
                Ok(None)
            }
            "/doctor" => {
                self.show_doctor_report();
                Ok(None)
            }
            "@agent" => self.execute_agent_slash_command(&command, &prompt),
            "/agent" => self.activate_agent_from_command(&command.arg),
            "/effort" => self.set_runtime_reasoning_effort_from_command(&command.arg),
            "/model" => self.set_runtime_model_from_command(&command.arg),
            "/queue" => self.execute_queue_slash_command(&command.arg),
            "/new" => {
                if self.is_busy {
                    self.push_timeline(TimelineRole::Notice, "busy; start new session later");
                    return Ok(None);
                }
                let session_log_path = self.new_session_log_path();
                Ok(Some(AppAction::StartNewSession { session_log_path }))
            }
            "/plan" => {
                if self.is_busy {
                    self.push_timeline(TimelineRole::Notice, "busy; plan later");
                    return Ok(None);
                }
                let arg = command.arg.trim();
                if arg.is_empty() {
                    self.input.clear();
                    self.input_cursor = 0;
                    self.input_paste_spans.clear();
                    self.reset_slash_selector();
                    self.composer_mode = ComposerMode::Plan;
                    self.last_notice = Some("plan mode".to_owned());
                    self.push_event("mode", "plan");
                    return Ok(None);
                }
                if arg == "continue" || arg.starts_with("continue ") {
                    self.push_timeline(
                        TimelineRole::Notice,
                        "plan mode cannot continue durable tasks; use /task continue",
                    );
                    self.last_notice = Some("use /task continue".to_owned());
                    return Ok(None);
                }

                let plan_prompt = arg.to_owned();
                self.clear_pending_plan_approval();
                self.input.clear();
                self.input_cursor = 0;
                self.input_paste_spans.clear();
                self.reset_slash_selector();
                self.timeline_scroll_back = 0;
                self.push_timeline(TimelineRole::User, format!("/plan {plan_prompt}"));
                self.push_event("input", format!("submitted plan prompt {plan_prompt}"));
                self.active_pane = PaneFocus::Composer;
                self.push_event("focus", current_focus_label(self));
                self.is_busy = true;
                self.run_phase = RunPhase::Thinking;
                self.last_notice = Some(ComposerMode::Plan.notice().to_owned());
                self.last_phase_marker = None;
                self.push_phase_marker(format!(
                    "{}|{}",
                    ComposerMode::Plan.phase_marker(),
                    self.model_name
                ));
                self.streaming_assistant_index = None;
                self.streaming_reasoning_index = None;
                self.refresh_usage_sidebar_cache();
                Ok(Some(AppAction::SubmitPlanPrompt(plan_prompt)))
            }
            "/task" => {
                if self.is_busy {
                    self.push_timeline(TimelineRole::Notice, "busy; task later");
                    return Ok(None);
                }
                let arg = command.arg.trim();
                if arg.is_empty() {
                    self.push_timeline(TimelineRole::Notice, "usage: /task <task|continue>");
                    self.last_notice = Some("usage: /task <task|continue>".to_owned());
                    return Ok(None);
                }
                if arg == "continue" || arg.starts_with("continue ") {
                    let guidance = arg
                        .strip_prefix("continue")
                        .map(str::trim)
                        .filter(|value| !value.is_empty())
                        .map(ToOwned::to_owned);
                    self.is_busy = true;
                    self.run_phase = RunPhase::Thinking;
                    self.last_notice = Some("continuing task".to_owned());
                    self.last_phase_marker = None;
                    self.push_phase_marker(format!("task|{}", self.model_name));
                    self.streaming_assistant_index = None;
                    self.streaming_reasoning_index = None;
                    self.refresh_usage_sidebar_cache();
                    return Ok(Some(AppAction::ContinueTask {
                        task_id: None,
                        guidance,
                    }));
                }

                let objective = arg.to_owned();
                self.clear_pending_plan_approval();
                self.timeline_scroll_back = 0;
                self.push_timeline(TimelineRole::User, format!("/task {objective}"));
                self.push_event("input", format!("submitted task {objective}"));
                self.active_pane = PaneFocus::Composer;
                self.push_event("focus", current_focus_label(self));
                self.is_busy = true;
                self.run_phase = RunPhase::Thinking;
                self.last_notice = Some("planning task".to_owned());
                self.last_phase_marker = None;
                self.push_phase_marker(format!("task|{}", self.model_name));
                self.streaming_assistant_index = None;
                self.streaming_reasoning_index = None;
                self.refresh_usage_sidebar_cache();
                Ok(Some(AppAction::SubmitTask(objective)))
            }
            "/quit" => {
                self.should_quit = true;
                self.push_timeline(TimelineRole::Notice, "quitting");
                Ok(None)
            }
            "/resume" => {
                if self.is_busy {
                    self.push_timeline(TimelineRole::Notice, "busy; resume later");
                    return Ok(None);
                }

                self.refresh_session_history();
                match self.resolve_resume_target(&command.arg) {
                    Some(path) => Ok(Some(AppAction::SwitchSession {
                        session_log_path: path,
                    })),
                    None => {
                        self.push_timeline(TimelineRole::Notice, "no matching session");
                        Ok(None)
                    }
                }
            }
            _ => {
                if let Some(action) = self.execute_skill_slash_command(&command, &prompt)? {
                    return Ok(Some(action));
                }
                self.push_timeline(TimelineRole::Notice, "unknown slash command");
                Ok(None)
            }
        }
    }

    fn execute_agent_slash_command(
        &mut self,
        command: &ResolvedSlashCommand,
        prompt: &str,
    ) -> Result<Option<AppAction>> {
        if self.is_busy {
            self.push_timeline(TimelineRole::Notice, "busy; invoke agent later");
            self.last_notice = Some("busy; invoke agent later".to_owned());
            return Ok(None);
        }
        let Some((profile_id, agent_prompt)) =
            command.arg.trim_start().split_once(char::is_whitespace)
        else {
            self.push_timeline(TimelineRole::Notice, "usage: /agent-name <prompt>");
            self.last_notice = Some("usage: /agent-name <prompt>".to_owned());
            return Ok(None);
        };
        let agent_prompt = agent_prompt.trim();
        if agent_prompt.is_empty() {
            self.push_timeline(TimelineRole::Notice, "usage: /agent-name <prompt>");
            self.last_notice = Some("usage: /agent-name <prompt>".to_owned());
            return Ok(None);
        }
        Ok(Some(self.start_agent_profile_invocation(
            profile_id.to_owned(),
            agent_prompt.to_owned(),
            prompt.to_owned(),
        )))
    }

    fn start_agent_profile_invocation(
        &mut self,
        profile_id: String,
        agent_prompt: String,
        prompt: String,
    ) -> AppAction {
        self.clear_pending_plan_approval();
        self.input.clear();
        self.input_cursor = 0;
        self.input_paste_spans.clear();
        self.reset_slash_selector();
        self.timeline_scroll_back = 0;
        self.push_timeline(TimelineRole::User, prompt.clone());
        self.push_event("input", format!("invoked agent {profile_id}"));
        self.active_pane = PaneFocus::Composer;
        self.push_event("focus", current_focus_label(self));
        self.is_busy = true;
        self.run_phase = RunPhase::Agent(profile_id.clone());
        self.last_notice = Some(format!("waiting for agent @{profile_id}"));
        self.last_phase_marker = None;
        self.push_phase_marker(format!("agent|{profile_id}"));
        self.streaming_assistant_index = None;
        self.streaming_reasoning_index = None;
        self.composer_mode = ComposerMode::Build;
        self.refresh_usage_sidebar_cache();
        AppAction::InvokeAgentProfile {
            profile_id,
            prompt: agent_prompt,
            parent_prompt: prompt,
        }
    }

    fn execute_skill_slash_command(
        &mut self,
        command: &ResolvedSlashCommand,
        prompt: &str,
    ) -> Result<Option<AppAction>> {
        let Some(skill_id) = command.canonical.strip_prefix('/') else {
            return Ok(None);
        };
        let Some(skill) = self.exact_skill_descriptor(skill_id) else {
            return Ok(None);
        };
        let item_kind = slash_skill_display_kind(&skill);
        if self.is_busy {
            self.push_timeline(TimelineRole::Notice, format!("busy; use {item_kind} later"));
            self.last_notice = Some(format!("busy; use {item_kind} later"));
            return Ok(None);
        }
        if !skill.enabled {
            self.push_timeline(
                TimelineRole::Notice,
                format!("{item_kind} {skill_id} is disabled"),
            );
            self.last_notice = Some(format!("{item_kind} {skill_id} is disabled"));
            return Ok(None);
        }
        if skill.trust != sigil_kernel::SkillTrustState::Trusted {
            self.push_timeline(
                TimelineRole::Notice,
                format!("{item_kind} {skill_id} is not trusted"),
            );
            self.last_notice = Some(format!("{item_kind} {skill_id} is not trusted"));
            return Ok(None);
        }
        if !skill.user_invocable {
            self.push_timeline(
                TimelineRole::Notice,
                format!("{item_kind} {skill_id} is not user-invocable"),
            );
            self.last_notice = Some(format!("{item_kind} {skill_id} is not user-invocable"));
            return Ok(None);
        }

        self.timeline_scroll_back = 0;
        self.push_timeline(TimelineRole::User, prompt.to_owned());
        self.push_event("input", format!("invoked skill {skill_id}"));
        self.active_pane = PaneFocus::Composer;
        self.push_event("focus", current_focus_label(self));
        self.is_busy = true;
        self.run_phase = RunPhase::Thinking;
        self.last_phase_marker = None;
        self.streaming_assistant_index = None;
        self.streaming_reasoning_index = None;
        self.refresh_usage_sidebar_cache();

        let arguments = command.arg.trim().to_owned();
        match skill.run_as {
            sigil_kernel::SkillRunMode::Inline => {
                self.last_notice = Some(format!("using skill {skill_id}"));
                self.push_phase_marker(format!("thinking|{}", self.model_name));
                Ok(Some(AppAction::InvokeInlineSkill {
                    skill_id: skill_id.to_owned(),
                    arguments,
                }))
            }
            sigil_kernel::SkillRunMode::ChildSession => {
                self.last_notice = Some(format!("invoking agent {skill_id}"));
                self.push_phase_marker(format!("task|{}", self.model_name));
                Ok(Some(AppAction::InvokeChildSessionSkill {
                    skill_id: skill_id.to_owned(),
                    arguments,
                }))
            }
        }
    }

    pub(crate) fn run_phase(&self) -> RunPhase {
        self.run_phase.clone()
    }

    pub(crate) fn run_phase_label(&self) -> String {
        match &self.run_phase {
            RunPhase::Idle => "ready".to_owned(),
            RunPhase::Thinking => "thinking".to_owned(),
            RunPhase::Agent(profile_id) => format!("agent @{profile_id}"),
            RunPhase::Tool(name) => format!("tool {name}"),
            RunPhase::Streaming => "streaming".to_owned(),
        }
    }

    pub(crate) fn session_display_title(&self) -> String {
        self.timeline
            .iter()
            .find(|entry| entry.role == TimelineRole::User)
            .and_then(|entry| {
                entry
                    .text
                    .lines()
                    .map(str::trim)
                    .find(|line| !line.is_empty())
                    .map(|line| truncate_session_view_text(line, 56))
            })
            .unwrap_or_else(|| {
                format!(
                    "{} · {}",
                    self.workspace_root
                        .file_name()
                        .and_then(|value| value.to_str())
                        .unwrap_or("session"),
                    self.model_name
                )
            })
    }

    #[cfg(test)]
    pub(crate) fn latest_user_prompt_preview(&self) -> Option<String> {
        let entry = self
            .timeline
            .iter()
            .rev()
            .find(|entry| entry.role == TimelineRole::User)?;
        let mut visible_lines = entry
            .text
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty());
        let first_line = visible_lines.next()?;
        let extra_lines = visible_lines.count();
        Some(if extra_lines == 0 {
            first_line.to_owned()
        } else {
            format!("{first_line}  +{extra_lines} more")
        })
    }

    pub(crate) fn session_sidebar_lines(&self) -> Vec<String> {
        vec![
            format!("provider: {}", self.provider_name),
            format!("model: {}", self.model_name),
            format!("effort: {}", self.reasoning_effort.as_str()),
            format!("phase: {}", self.run_phase_label()),
            format!("session: {}", short_session_token(&self.session_id)),
        ]
    }

    pub(crate) fn task_memory_sidebar_lines(&self) -> Vec<String> {
        let Some(task_memory) = self
            .latest_compaction_record
            .as_ref()
            .and_then(|record| record.task_memory.as_ref())
        else {
            return vec!["task memory: none yet".to_owned()];
        };

        let mut parts = vec!["task memory: ready".to_owned()];
        if !task_memory.decisions.is_empty() {
            parts.push(format!("{} decisions", task_memory.decisions.len()));
        }
        if !task_memory.files_changed.is_empty() {
            parts.push(format!("{} files", task_memory.files_changed.len()));
        }
        if !task_memory.unresolved_issues.is_empty() {
            parts.push(format!(
                "{} unresolved",
                task_memory.unresolved_issues.len()
            ));
        }
        vec![parts.join(" · ")]
    }

    pub(crate) fn task_sidebar_lines(&self) -> Vec<String> {
        self.session_view_cache().task_sidebar_lines.clone()
    }

    pub(crate) fn task_strip_view(&self) -> Option<task_sidebar::TaskStripView> {
        self.session_view_cache().task_strip_view.clone()
    }

    fn session_view_cache(&self) -> Ref<'_, SessionViewCache> {
        self.ensure_session_view_cache();
        self.session_view_cache.borrow()
    }

    fn ensure_session_view_cache(&self) {
        let needs_refresh = {
            let cache = self.session_view_cache.borrow();
            cache.entries_len != self.current_session_entries.len()
                || cache.entries_revision != self.current_session_entries_revision
        };
        if needs_refresh {
            let cache = self.build_session_view_cache();
            *self.session_view_cache.borrow_mut() = cache;
        }
    }

    fn mark_current_session_entries_changed(&mut self) {
        self.current_session_entries_revision =
            self.current_session_entries_revision.saturating_add(1);
        self.refresh_session_view_cache();
    }

    fn refresh_session_view_cache(&mut self) {
        let cache = self.build_session_view_cache();
        *self.session_view_cache.borrow_mut() = cache;
    }

    fn build_session_view_cache(&self) -> SessionViewCache {
        let entries = &self.current_session_entries;
        let task_projection = TaskStateProjection::from_entries(entries);
        let agent_projection = AgentThreadStateProjection::from_entries(entries);
        let continuation_projection = AgentResultContinuationProjection::from_entries(entries);
        let agent_child_items = agent_flow::agent_sidebar_child_items_from_projections(
            &task_projection,
            &agent_projection,
            &continuation_projection,
        );
        let agent_graph_summary_line =
            sigil_runtime::agent_graph_product_summary_from_entries(entries)
                .map(|summary| summary.display_line());
        SessionViewCache {
            entries_len: entries.len(),
            entries_revision: self.current_session_entries_revision,
            task_projection,
            agent_projection,
            task_sidebar_lines: task_sidebar::task_sidebar_lines(entries),
            task_strip_view: task_sidebar::task_strip_view(entries),
            agent_child_items,
            agent_graph_summary_line,
            compaction_preview_line: self.compaction_preview_sidebar_line(entries),
        }
    }

    fn compaction_preview_sidebar_line(&self, entries: &[SessionLogEntry]) -> Option<String> {
        if entries.is_empty() {
            return None;
        }
        let session = Session::from_entries(
            self.provider_name.clone(),
            self.model_name.clone(),
            entries.to_vec(),
        );
        match session.compaction_preview(&self.compaction_config) {
            Ok(Some(preview)) => Some(format!(
                "compact: fold {} keep {}",
                preview.record.compacted_message_count, preview.record.retained_tail_message_count
            )),
            Ok(None) => Some("compact: nothing to fold".to_owned()),
            Err(error) => Some(format!(
                "compact: {}",
                truncate_session_view_text(&error.to_string(), 28)
            )),
        }
    }

    pub(crate) fn pending_plan_approval(&self) -> Option<&PendingPlanApproval> {
        self.pending_plan_approval.as_ref()
    }

    pub(crate) fn set_pending_plan_approval_from_text(&mut self, plan_text: &str) {
        let plan_text = plan_text.trim();
        if plan_text.is_empty() {
            self.pending_plan_approval = None;
            return;
        }
        self.pending_plan_approval = Some(PendingPlanApproval {
            plan_text: plan_text.to_owned(),
            plan_hash: plan_text_hash(plan_text),
            scope_summary: first_nonempty_plan_line(plan_text)
                .unwrap_or_else(|| "approved plan scope".to_owned()),
        });
    }

    fn clear_pending_plan_approval(&mut self) {
        self.pending_plan_approval = None;
    }

    pub(crate) fn composer_mode_label(&self) -> &'static str {
        self.composer_mode.label()
    }

    pub(crate) fn reasoning_effort_label(&self) -> &'static str {
        self.reasoning_effort.as_str()
    }

    pub(crate) fn info_rail_detail_enabled(&self) -> bool {
        self.info_rail_detail
    }

    pub(crate) fn toggle_info_rail_detail(&mut self) {
        self.info_rail_detail = !self.info_rail_detail;
        let mode = if self.info_rail_detail {
            "detail"
        } else {
            "compact"
        };
        self.last_notice = Some(format!("info rail: {mode}"));
        self.push_event("info_rail", mode);
    }

    pub(crate) fn context_usage_line(&self) -> String {
        let resolved = self.resolved_context_window();
        match resolved.tokens {
            Some(cap) if cap > 0 => format!(
                "ctx: {}% · prompt {} / {} {} · {}",
                self.context_usage_percent(cap),
                format_token_compact(self.stats.last_prompt_tokens),
                format_token_compact(cap as u64),
                context_window_source_label(resolved.source),
                self.context_usage_hint(cap)
            ),
            _ => format!(
                "ctx: n/a · prompt {} · set fallback_context_window_tokens",
                format_token_compact(self.stats.last_prompt_tokens)
            ),
        }
    }

    pub(crate) fn compaction_policy_line(&self) -> String {
        let resolved = self.resolved_context_window();
        match resolved.tokens {
            Some(cap) if cap > 0 => format!(
                "policy: {} {} · soft {}% ({}) · hard {}% ({})",
                context_window_source_label(resolved.source),
                format_token_count(cap as u64),
                ratio_to_percent(self.compaction_config.soft_threshold_ratio),
                format_token_compact(threshold_token_count(
                    cap,
                    self.compaction_config.soft_threshold_ratio
                )),
                ratio_to_percent(self.compaction_config.hard_threshold_ratio),
                format_token_compact(threshold_token_count(
                    cap,
                    self.compaction_config.hard_threshold_ratio
                ))
            ),
            _ => format!(
                "policy: soft {}% · hard {}%",
                ratio_to_percent(self.compaction_config.soft_threshold_ratio),
                ratio_to_percent(self.compaction_config.hard_threshold_ratio)
            ),
        }
    }

    pub(crate) fn permission_card_lines(&self) -> Vec<String> {
        vec![
            format!("mode: {}", self.permission_default_mode),
            "Shift-Tab cycle + save".to_owned(),
            if self.is_busy {
                "busy: locked during run".to_owned()
            } else {
                "scope: saved default".to_owned()
            },
        ]
    }

    fn refresh_usage_sidebar_cache(&mut self) {
        let currency = self.usage_cost_currency();
        let session_spent = self.stats.input_cost + self.stats.output_cost;
        let delta_spent =
            self.session_delta_stats.input_cost + self.session_delta_stats.output_cost;
        let saved = self.stats.cache_savings;
        let session_spent = currency.format_cost(session_spent);
        let delta_spent = currency.format_cost(delta_spent);
        let saved = currency.format_cost(saved);
        let balance_line = self.balance_sidebar_line();
        let mut lines = vec![
            self.context_usage_line(),
            self.session_token_line(),
            format!("compact: {}", self.compaction_status),
            self.compaction_policy_line(),
            self.tool_card_status_line(),
            format!(
                "cache: {:.0}% · save {saved}",
                self.cache_hit_ratio() * 100.0
            ),
            format!("total spent: {session_spent}"),
            format!("spent since opening: {delta_spent}"),
            balance_line,
        ];
        let compaction_preview_line = if self.is_busy {
            None
        } else {
            self.session_view_cache().compaction_preview_line.clone()
        };
        if let Some(line) = compaction_preview_line {
            lines.push(line);
        }
        self.usage_sidebar_cache = lines;
    }

    pub(crate) fn usage_sidebar_lines(&self) -> &[String] {
        &self.usage_sidebar_cache
    }

    pub(crate) fn balance_sidebar_line(&self) -> String {
        if self.balance_snapshot.available {
            match (
                self.balance_snapshot.total,
                self.balance_snapshot.currency.as_deref(),
            ) {
                (Some(total), Some(currency)) => format!("balance: {currency} {total:.2}"),
                _ => format!("balance: {}", self.balance_snapshot.status),
            }
        } else {
            format!("balance: {}", self.balance_snapshot.status)
        }
    }

    fn session_token_line(&self) -> String {
        format!(
            "session tok: input {} · output {}",
            format_token_compact(self.stats.prompt_tokens),
            format_token_compact(self.stats.completion_tokens)
        )
    }

    fn usage_cost_currency(&self) -> ResolvedUsageCostCurrency {
        let configured = self
            .config_snapshot
            .as_ref()
            .map(|config| config.appearance.usage_cost_currency)
            .unwrap_or_default();
        ResolvedUsageCostCurrency::from_config(
            configured,
            self.balance_snapshot.currency.as_deref(),
        )
    }

    #[cfg(test)]
    pub(crate) fn footer_status_line(&self) -> String {
        let currency = self.usage_cost_currency();
        let session_spent = self.stats.input_cost + self.stats.output_cost;
        let delta_spent =
            self.session_delta_stats.input_cost + self.session_delta_stats.output_cost;
        let session_spent = currency.format_cost(session_spent);
        let delta_spent = currency.format_cost(delta_spent);
        let token_line = format!(
            "tok {}",
            format_token_compact(self.stats.last_prompt_tokens)
        );
        let context = match self.resolved_context_window().tokens {
            Some(cap) if cap > 0 => format!("ctx {}%", self.context_usage_percent(cap)),
            _ => "ctx n/a".to_owned(),
        };
        format!(
            "{}  ·  {}  ·  cache {:.0}%  ·  spent {delta_spent} since opening / {session_spent} total  ·  write {}  ·  Ctrl-C {}",
            token_line,
            context,
            self.cache_hit_ratio() * 100.0,
            self.permission_default_mode,
            if self.is_busy { "cancel" } else { "quit" }
        )
    }

    fn resolved_context_window(&self) -> sigil_runtime::ResolvedContextWindow {
        resolve_context_window_tokens(
            &self.provider_name,
            &self.model_name,
            self.compaction_config.context_window_tokens,
        )
    }

    fn resolved_compaction_config(&self) -> CompactionConfig {
        effective_compaction_config(
            &self.provider_name,
            &self.model_name,
            &self.compaction_config,
        )
    }

    fn context_usage_percent(&self, cap: u32) -> u64 {
        ((self.stats.last_prompt_tokens as f64 / cap as f64) * 100.0)
            .round()
            .clamp(0.0, 999.0) as u64
    }

    fn context_usage_hint(&self, cap: u32) -> String {
        match self
            .resolved_compaction_config()
            .threshold_status(self.stats.last_prompt_tokens)
        {
            CompactionThresholdStatus::Off => "compact off".to_owned(),
            CompactionThresholdStatus::NotAvailable => "threshold n/a".to_owned(),
            CompactionThresholdStatus::Ready => format!(
                "soft at {}",
                format_token_compact(threshold_token_count(
                    cap,
                    self.compaction_config.soft_threshold_ratio
                ))
            ),
            CompactionThresholdStatus::Soft => "soft; /compact".to_owned(),
            CompactionThresholdStatus::Hard => "hard; auto-compact".to_owned(),
        }
    }

    pub fn is_setup_mode(&self) -> bool {
        self.setup_state.is_some()
    }

    pub fn is_config_mode(&self) -> bool {
        self.config_state.is_some()
    }

    pub fn has_modal(&self) -> bool {
        self.modal_state.is_some()
    }

    fn recompute_compaction_status(&mut self, emit_feedback: bool) {
        let next = self
            .resolved_compaction_config()
            .threshold_status(self.stats.last_prompt_tokens);
        let next_label = next.as_str().to_owned();
        if self.compaction_status == next_label {
            return;
        }

        self.compaction_status = next_label.clone();
        self.push_event("compaction", next_label);
        if !emit_feedback {
            return;
        }

        match next {
            CompactionThresholdStatus::Soft => {
                self.push_timeline(TimelineRole::Notice, "soft threshold; /compact when ready");
            }
            CompactionThresholdStatus::Hard => {
                self.push_timeline(TimelineRole::Notice, "hard threshold; auto-compact on idle");
            }
            CompactionThresholdStatus::Off
            | CompactionThresholdStatus::NotAvailable
            | CompactionThresholdStatus::Ready => {}
        }
    }

    fn move_sidebar_selection(&mut self, next: bool) {
        match self.sidebar_selected_card {
            SidebarCard::Permission => {
                self.sidebar_selected_card = if next {
                    self.sidebar_selected_card.next()
                } else {
                    self.sidebar_selected_card.previous()
                };
            }
            SidebarCard::Agents => {
                let last_index = self.agent_sidebar_rows().len().saturating_sub(1);
                if next {
                    if self.sidebar_agent_selected < last_index {
                        self.sidebar_agent_selected += 1;
                    } else {
                        self.sidebar_selected_card = SidebarCard::Usage;
                    }
                } else if self.sidebar_agent_selected > 0 {
                    self.sidebar_agent_selected -= 1;
                } else {
                    self.sidebar_selected_card = SidebarCard::Permission;
                }
            }
            SidebarCard::Usage => {
                self.sidebar_selected_card = if next {
                    self.sidebar_selected_card.next()
                } else {
                    self.sidebar_selected_card.previous()
                };
            }
        }
    }

    fn toggle_runtime_permission_mode(&mut self) -> Result<Option<AppAction>> {
        if self.is_busy {
            self.last_notice = Some("busy; permission locked".to_owned());
            self.push_timeline(
                TimelineRole::Notice,
                "busy; permission mode stays unchanged",
            );
            return Ok(None);
        }
        let Some(root_config) = self.config_snapshot.as_ref() else {
            return Ok(None);
        };
        let mut next_config = root_config.clone();
        next_config.permission.default_mode =
            cycle_approval_mode(next_config.permission.default_mode);
        persisted_root_config(&next_config).save(&self.config_path)?;
        self.apply_runtime_config_snapshot(&next_config);
        self.last_notice = Some(format!(
            "default mode = {}",
            next_config.permission.default_mode.as_str()
        ));
        self.push_event("approval_default", self.permission_default_mode.clone());
        self.push_timeline(
            TimelineRole::Notice,
            format!("default permission -> {}", self.permission_default_mode),
        );
        self.schedule_balance_refresh();
        Ok(Some(AppAction::RuntimeConfigUpdated {
            root_config: Box::new(next_config),
        }))
    }

    fn set_runtime_reasoning_effort_from_command(
        &mut self,
        argument: &str,
    ) -> Result<Option<AppAction>> {
        let Some(effort) = parse_reasoning_effort(argument) else {
            self.last_notice = Some("usage: /effort <low|medium|high|max>".to_owned());
            self.push_timeline(TimelineRole::Notice, "usage: /effort <low|medium|high|max>");
            return Ok(None);
        };

        self.reasoning_effort = effort.clone();
        self.last_notice = Some(format!("reasoning effort = {}", effort.as_str()));
        self.push_event("effort", effort.as_str());
        self.push_timeline(
            TimelineRole::Notice,
            format!("reasoning effort -> {}", effort.as_str()),
        );
        Ok(None)
    }

    fn set_runtime_model_from_command(&mut self, argument: &str) -> Result<Option<AppAction>> {
        if self.is_busy {
            self.last_notice = Some("busy; model locked".to_owned());
            self.push_timeline(TimelineRole::Notice, "busy; switch model after the run");
            return Ok(None);
        }

        let Some(model) = normalize_runtime_model(argument) else {
            self.last_notice = Some("usage: /model <flash|pro|id>".to_owned());
            self.push_timeline(TimelineRole::Notice, "usage: /model <flash|pro|id>");
            return Ok(None);
        };

        if model == self.model_name {
            self.last_notice = Some(format!("model already active = {model}"));
            self.push_timeline(
                TimelineRole::Notice,
                format!("model already active -> {model}"),
            );
            return Ok(None);
        }

        let Some(root_config) = self.config_snapshot.as_ref() else {
            return Ok(None);
        };

        let mut next_config = root_config.clone();
        set_active_provider_model(&mut next_config, &model)?;

        self.apply_runtime_config_snapshot(&next_config);
        self.reset_for_new_session(
            next_config.agent.provider.clone(),
            model.clone(),
            format!("model -> {model}; started a fresh session"),
        );
        self.schedule_balance_refresh();

        Ok(Some(AppAction::RuntimeConfigUpdated {
            root_config: Box::new(next_config),
        }))
    }
}

fn slash_skill_display_kind(skill: &sigil_kernel::SkillDescriptor) -> &'static str {
    if matches!(skill.run_as, sigil_kernel::SkillRunMode::ChildSession) {
        "agent"
    } else {
        "skill"
    }
}

fn is_composer_newline_key(key: KeyEvent) -> bool {
    (key.modifiers.contains(KeyModifiers::SHIFT) || key.modifiers.contains(KeyModifiers::ALT))
        && matches!(
            key.code,
            KeyCode::Enter | KeyCode::Char('\n') | KeyCode::Char('\r')
        )
}

fn is_composer_submit_key(key: KeyEvent) -> bool {
    !key.modifiers.contains(KeyModifiers::SHIFT)
        && !key.modifiers.contains(KeyModifiers::ALT)
        && matches!(
            key.code,
            KeyCode::Enter | KeyCode::Char('\n') | KeyCode::Char('\r')
        )
}

fn is_composer_text_key(key: KeyEvent) -> bool {
    matches!(key.code, KeyCode::Char(character) if !key.modifiers.contains(KeyModifiers::CONTROL) && !character.is_control())
}

fn has_control_without_alt(key: KeyEvent) -> bool {
    key.modifiers.contains(KeyModifiers::CONTROL) && !key.modifiers.contains(KeyModifiers::ALT)
}

fn has_alt_without_control(key: KeyEvent) -> bool {
    key.modifiers.contains(KeyModifiers::ALT) && !key.modifiers.contains(KeyModifiers::CONTROL)
}

fn context_window_source_label(source: ContextWindowSource) -> &'static str {
    match source {
        ContextWindowSource::Provider => "provider",
        ContextWindowSource::Config => "fallback",
        ContextWindowSource::None => "n/a",
    }
}

fn first_nonempty_plan_line(plan_text: &str) -> Option<String> {
    plan_text
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .map(|line| truncate_session_view_text(line, 80))
}

fn threshold_token_count(cap: u32, ratio: f32) -> u64 {
    (f64::from(cap) * f64::from(ratio.max(0.0))).round() as u64
}

#[cfg(test)]
mod tests;

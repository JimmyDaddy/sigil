use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    path::{Path, PathBuf},
    time::SystemTime,
};

mod agent_flow;
mod approval_flow;
mod command_dispatch;
mod config_flow;
mod conversation_queue_flow;
mod diagnostics_flow;
mod file_type;
mod formatting;
mod input_flow;
mod input_history;
mod key_router;
mod modal_flow;
mod mouse_flow;
mod pending_plan_flow;
mod runtime_command_flow;
mod runtime_status;
mod runtime_view_flow;
mod session_flow;
mod session_review;
mod session_view_cache_flow;
mod setup_flow;
mod sidebar_flow;
mod slash_flow;
mod state;
mod submit_flow;
pub(crate) mod task_sidebar;
mod timeline_flow;
mod timeline_render_store;
mod tool_card_interaction;
mod usage_sidebar_flow;
mod worker_bridge;
mod workspace_trust_flow;

use anyhow::Result;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::text::Line;
use sigil_kernel::{
    AgentThreadId, AgentThreadStateProjection, CompactionConfig, CompactionRecord,
    CompactionThresholdStatus, ConversationInputKind, ConversationInputQueueId,
    ConversationInputTarget, MemoryConfig, MutationArtifactCleanupTarget,
    MutationArtifactInventoryItem, MutationArtifactRetentionReport, PermissionMode,
    PlanApprovalPermission, PlanTaskStartMode, ReasoningEffort, RootConfig, SecretRedactor,
    SessionConfig, SessionStats, StorageConfig, TaskStateProjection, ToolPreviewSnapshot,
    resolve_workspace_root,
};
use sigil_runtime::{
    BalanceSnapshot, SigilPaths, effective_compaction_config, resolve_sigil_paths,
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
use crate::runner::QueueMoveDirection;
pub use crate::sessions::{SessionHistoryEntry, SessionViewMode};
pub(crate) use crate::setup::{SetupField, SetupState};
use crate::slash::ResolvedSlashCommand;
pub use crate::timeline::{EventEntry, TimelineEntry, TimelineRole};
pub(crate) use crate::timeline::{
    LiveActivitySummary, RunPhase, SessionHistoryRow, SidebarCard, ThinkingBlockMode,
    ToolActivityCacheEntry,
};
pub(crate) use crate::workspace_trust::WorkspaceTrustGateState;

use self::formatting::*;
use self::modal_flow::{ModalState, ModelPickerRefresh};
use self::runtime_status::McpProgressState;
pub(crate) use self::runtime_status::{
    McpServerRuntimeStatus, TimelineTextSelection, code_intelligence_config_status,
    diagnostic_summary_label, initial_mcp_server_status, initial_mcp_server_statuses,
};
use self::state::{ApprovalState, ComposerState, RuntimeStatusState, SessionBrowserState};
use self::timeline_render_store::TimelineRenderStore;
#[cfg(test)]
pub(crate) use self::usage_sidebar_flow::context_window_source_label;

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
    KeepNext,
    SendNow,
    Edit,
    Delete,
}

impl ComposerQueueAction {
    pub(crate) const ORDER: [Self; 4] = [Self::KeepNext, Self::SendNow, Self::Edit, Self::Delete];

    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::KeepNext => "Run next",
            Self::SendNow => "Interrupt",
            Self::Edit => "Edit",
            Self::Delete => "Delete",
        }
    }

    pub(crate) fn detail(self) -> &'static str {
        match self {
            Self::KeepNext => "run after the current turn",
            Self::SendNow => "stop current turn and run this follow-up",
            Self::Edit => "edit follow-up",
            Self::Delete => "remove follow-up",
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
    session_review_lines: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PendingPlanApproval {
    pub(crate) plan_id: Option<String>,
    pub(crate) plan_text: String,
    pub(crate) plan_hash: String,
    pub(crate) summary: String,
    pub(crate) steps: Vec<String>,
    pub(crate) target_paths: Vec<String>,
    pub(crate) suggested_checks: Vec<String>,
    pub(crate) target_path_count: usize,
    pub(crate) suggested_check_count: usize,
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
    pub session_id: String,
    pub(crate) runtime: RuntimeStatusState,
    pub(crate) composer: ComposerState,
    pub(crate) approval: ApprovalState,
    pub(crate) session_browser: SessionBrowserState,
    pub timeline: Vec<TimelineEntry>,
    pub events: Vec<EventEntry>,
    pub should_quit: bool,
    pub active_pane: PaneFocus,
    pub timeline_scroll_back: usize,
    pub activity_scroll_back: usize,
    info_rail_detail: bool,
    config_snapshot: Option<RootConfig>,
    terminal_keyboard_enhancement_enabled: bool,
    secret_redactor: SecretRedactor,
    setup_state: Option<SetupState>,
    workspace_trust_gate_state: Option<WorkspaceTrustGateState>,
    config_state: Option<ConfigState>,
    modal_state: Option<ModalState>,
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
    tool_activity_visible_rows: BTreeMap<String, usize>,
    pending_terminal_cancel_confirmation: Option<String>,
    pending_mouse_slash_confirmation: Option<ResolvedSlashCommand>,
    mouse_hover_target: Option<crate::mouse::HitTarget>,
    pending_mouse_left_down: bool,
    pending_tool_card_body_click_entry: Option<usize>,
    last_notice: Option<String>,
    streaming_assistant_index: Option<usize>,
    streaming_reasoning_index: Option<usize>,
    timeline_render_store: TimelineRenderStore,
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
    active_agent_view: AgentView,
    active_agent_child_transcript: Option<ActiveAgentChildTranscript>,
    terminal_width: u16,
    terminal_height: u16,
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
    CreateTaskFromPlan {
        plan_id: String,
        expected_plan_hash: String,
        start_mode: PlanTaskStartMode,
        permission_grant: Option<PlanApprovalPermission>,
    },
    RejectPlan {
        plan_id: String,
        expected_plan_hash: String,
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
    ApprovalSessionDecision {
        call_id: String,
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
    CancelAgent {
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
        let permission_mode = root_config.permission.mode.as_str().to_owned();
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
            session_id,
            runtime: RuntimeStatusState {
                provider_name: root_config.agent.provider.clone(),
                model_name: root_config.agent.model.clone(),
                permission_mode,
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
                stats: SessionStats::default(),
                session_delta_stats: SessionStats::default(),
                is_busy: false,
                mcp_progress: None,
                reasoning_effort: ReasoningEffort::Max,
                run_phase: RunPhase::Idle,
                last_phase_marker: None,
                balance_snapshot: BalanceSnapshot {
                    status: "pending".to_owned(),
                    ..BalanceSnapshot::default()
                },
                next_background_request_id: 1,
                pending_worker_commands: Vec::new(),
                active_balance_refresh_id: None,
                active_model_picker_refresh: None,
            },
            composer: ComposerState::default(),
            approval: ApprovalState::default(),
            session_browser: SessionBrowserState::default(),
            timeline: Vec::new(),
            events: Vec::new(),
            should_quit: false,
            active_pane: PaneFocus::Composer,
            timeline_scroll_back: 0,
            activity_scroll_back: 0,
            info_rail_detail: false,
            config_snapshot: Some(root_config.clone()),
            terminal_keyboard_enhancement_enabled: false,
            secret_redactor: sigil_runtime::secret_redactor_for_root_config(root_config),
            setup_state: None,
            workspace_trust_gate_state: None,
            config_state: None,
            modal_state: None,
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
            tool_activity_visible_rows: BTreeMap::new(),
            pending_terminal_cancel_confirmation: None,
            pending_mouse_slash_confirmation: None,
            mouse_hover_target: None,
            pending_mouse_left_down: false,
            pending_tool_card_body_click_entry: None,
            last_notice: None,
            streaming_assistant_index: None,
            streaming_reasoning_index: None,
            timeline_render_store: TimelineRenderStore::default(),
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
            active_agent_view: AgentView::Main,
            active_agent_child_transcript: None,
            terminal_width: 120,
            terminal_height: 32,
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
            session_id,
            runtime: RuntimeStatusState {
                provider_name: "deepseek".to_owned(),
                model_name: "deepseek-v4-flash".to_owned(),
                permission_mode: PermissionMode::Manual.as_str().to_owned(),
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
                stats: SessionStats::default(),
                session_delta_stats: SessionStats::default(),
                is_busy: false,
                mcp_progress: None,
                reasoning_effort: ReasoningEffort::Max,
                run_phase: RunPhase::Idle,
                last_phase_marker: None,
                balance_snapshot: BalanceSnapshot {
                    status: "missing auth".to_owned(),
                    ..BalanceSnapshot::default()
                },
                next_background_request_id: 1,
                pending_worker_commands: Vec::new(),
                active_balance_refresh_id: None,
                active_model_picker_refresh: None,
            },
            composer: ComposerState::default(),
            approval: ApprovalState::default(),
            session_browser: SessionBrowserState::default(),
            timeline: Vec::new(),
            events: Vec::new(),
            should_quit: false,
            active_pane: PaneFocus::Composer,
            timeline_scroll_back: 0,
            activity_scroll_back: 0,
            info_rail_detail: false,
            config_snapshot: None,
            terminal_keyboard_enhancement_enabled: false,
            secret_redactor: SecretRedactor::default(),
            setup_state: Some(SetupState::new(config_path, startup_error.clone())),
            workspace_trust_gate_state: None,
            config_state: None,
            modal_state: None,
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
            tool_activity_visible_rows: BTreeMap::new(),
            pending_terminal_cancel_confirmation: None,
            pending_mouse_slash_confirmation: None,
            mouse_hover_target: None,
            pending_mouse_left_down: false,
            pending_tool_card_body_click_entry: None,
            last_notice: startup_error,
            streaming_assistant_index: None,
            streaming_reasoning_index: None,
            timeline_render_store: TimelineRenderStore::default(),
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
            active_agent_view: AgentView::Main,
            active_agent_child_transcript: None,
            terminal_width: 120,
            terminal_height: 32,
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
        if self.runtime.code_intelligence_server_lines.is_empty()
            && self.runtime.code_intelligence_diagnostics_line.is_none()
            && self
                .runtime
                .code_intelligence_diagnostics_by_path
                .is_empty()
        {
            return vec![format!("status: {}", self.runtime.code_intelligence_status)];
        }
        let mut lines = self
            .runtime
            .code_intelligence_server_lines
            .values()
            .cloned()
            .collect::<Vec<_>>();
        if let Some(line) = &self.runtime.code_intelligence_diagnostics_line {
            lines.push(line.clone());
        }
        lines.extend(self.code_intelligence_diagnostic_file_lines());
        lines
    }

    fn code_intelligence_diagnostic_file_lines(&self) -> Vec<String> {
        if self
            .runtime
            .code_intelligence_diagnostics_by_path
            .is_empty()
        {
            return Vec::new();
        }
        const MAX_DIAGNOSTIC_FILES: usize = 4;
        let mut summaries = self
            .runtime
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
        self.tool_activity_visible_rows.clear();
        self.events.clear();
        self.ensure_scratch_dir();
        self.push_timeline(TimelineRole::System, "sigil ready.");
        self.push_event("session", format!("active {}", self.session_id));
        self.push_event("workspace", self.workspace_root.display().to_string());
        self.push_event(
            "model",
            format!("{}/{}", self.runtime.provider_name, self.runtime.model_name),
        );
        self.push_event("effort", self.runtime.reasoning_effort.as_str());
        self.push_event("permission_mode", self.runtime.permission_mode.clone());
        self.push_event(
            "memory",
            format!(
                "enabled={} docs={} status={}",
                self.runtime.memory_enabled,
                self.runtime.memory_document_count,
                self.runtime.memory_last_status
            ),
        );
        self.push_event("compaction", self.runtime.compaction_status.clone());
        self.push_event(
            "code_intelligence",
            self.runtime.code_intelligence_status.clone(),
        );
        self.push_event("session_log", self.session_log_path.display().to_string());
        self.push_event("focus", self.active_pane.label());
        self.reset_scroll();
    }

    fn bootstrap_setup(&mut self) {
        self.timeline.clear();
        self.tool_activity_cache.clear();
        self.tool_activity_visible_rows.clear();
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
        self.runtime.provider_name = provider_name;
        self.runtime.model_name = model_name;
        self.session_id = Uuid::new_v4().to_string();
        self.session_log_path = self
            .session_log_dir
            .join(format!("session-{}.jsonl", self.session_id));
        self.runtime.stats = SessionStats::default();
        self.runtime.session_delta_stats = SessionStats::default();
        self.runtime.is_busy = false;
        self.approval.pending = None;
        self.active_pane = PaneFocus::Composer;
        self.timeline_scroll_back = 0;
        self.approval.scroll_back = 0;
        self.activity_scroll_back = 0;
        self.session_browser.current_entries.clear();
        self.mark_current_session_entries_changed();
        self.tool_preview_snapshots.clear();
        self.latest_compaction_record = None;
        self.runtime.run_phase = RunPhase::Idle;
        self.runtime.last_phase_marker = None;
        self.streaming_assistant_index = None;
        self.streaming_reasoning_index = None;
        self.approval.metadata_collapsed = false;
        self.approval.selected_file_index = 0;
        self.approval.selected_hunk_index = 0;
        self.approval.diff_mode = ApprovalDiffMode::Full;
        self.approval.selected_action = ApprovalAction::Deny;
        self.selected_tool_activity_key = None;
        self.expanded_thinking_entry_indices.clear();
        self.collapsed_thinking_entry_indices.clear();
        self.expanded_tool_activity_keys.clear();
        self.collapsed_tool_activity_keys.clear();
        self.tool_activity_visible_rows.clear();
        self.pending_terminal_cancel_confirmation = None;
        self.pending_mouse_slash_confirmation = None;
        self.mouse_hover_target = None;
        self.pending_mouse_left_down = false;
        self.pending_tool_card_body_click_entry = None;
        self.active_agent_view = AgentView::Main;
        self.active_agent_child_transcript = None;
        self.composer.agent_panel_focused = false;
        self.composer.cleared_input_draft = None;
        self.composer.input_kill_buffer = None;
        self.composer.input_paste_spans.clear();
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
            if self.runtime.is_busy {
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

        if key.code == KeyCode::Esc && key.modifiers.is_empty() && self.runtime.is_busy {
            self.modal_state = None;
            self.last_notice = Some("cancellation requested".to_owned());
            self.push_timeline(TimelineRole::Notice, "cancel requested");
            return Ok(Some(AppAction::CancelRun));
        }

        if let Some(outcome) = self.handle_pending_plan_approval_key_event(key) {
            return Ok(outcome);
        }

        if let Some(outcome) = self.handle_key_router_event(key)? {
            return Ok(outcome);
        }

        if let Some(outcome) = self.handle_pending_approval_key_event(key) {
            return Ok(outcome);
        }

        if self.runtime.is_busy
            && self.approval.pending.is_none()
            && matches!(self.runtime.run_phase, RunPhase::Agent(_))
            && matches!(key.code, KeyCode::Char('b') | KeyCode::Char('B'))
            && has_control_without_alt(key)
        {
            self.last_notice = Some("agent background requested".to_owned());
            self.push_timeline(TimelineRole::Notice, "agent background requested");
            self.push_event("agent", "background requested");
            return Ok(Some(AppAction::BackgroundActiveAgent));
        }

        if self.active_pane == PaneFocus::Activity
            && self.approval.pending.is_none()
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
                    && self.approval.pending.is_none() =>
            {
                self.scroll_timeline(self.transcript_page_step());
                return Ok(None);
            }
            KeyCode::Char('d')
                if key.modifiers.contains(KeyModifiers::CONTROL)
                    && self.approval.pending.is_none() =>
            {
                self.unscroll_timeline(self.transcript_page_step());
                return Ok(None);
            }
            KeyCode::Char('p')
                if key.modifiers.contains(KeyModifiers::CONTROL)
                    && self.approval.pending.is_none() =>
            {
                self.navigate_input_history(true);
                return Ok(None);
            }
            KeyCode::Char('n')
                if key.modifiers.contains(KeyModifiers::CONTROL)
                    && self.approval.pending.is_none() =>
            {
                self.navigate_input_history(false);
                return Ok(None);
            }
            KeyCode::Home
                if key.modifiers.contains(KeyModifiers::CONTROL)
                    && self.approval.pending.is_none() =>
            {
                self.scroll_timeline_to_top();
                return Ok(None);
            }
            KeyCode::End
                if key.modifiers.contains(KeyModifiers::CONTROL)
                    && self.approval.pending.is_none() =>
            {
                self.unscroll_timeline(usize::MAX / 2);
                return Ok(None);
            }
            KeyCode::Char('t')
                if key.modifiers.contains(KeyModifiers::CONTROL)
                    && self.approval.pending.is_none() =>
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
                return Ok(None);
            }
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                return Ok(None);
            }
            _ => {}
        }

        if let Some(outcome) = self.handle_composer_key_event(key)? {
            return Ok(outcome);
        }

        match key.code {
            KeyCode::BackTab if self.approval.pending.is_none() => {
                return self.toggle_runtime_permission_mode();
            }
            KeyCode::PageUp => self.scroll_timeline(self.transcript_page_step()),
            KeyCode::PageDown => self.unscroll_timeline(self.transcript_page_step()),
            KeyCode::Home => self.scroll_timeline_to_top(),
            KeyCode::End => self.unscroll_timeline(usize::MAX / 2),
            KeyCode::Esc => {
                if self.composer.input.is_empty()
                    && self.handle_ui_command(UiCommand::ClearToolCardFocus)
                {
                    return Ok(None);
                }
                if self.cancel_queue_edit() {
                    self.clear_input_preserving_draft();
                    self.reset_input_history_navigation();
                    self.reset_slash_selector();
                    self.active_pane = PaneFocus::Composer;
                    return Ok(None);
                }
                if self.composer.input.is_empty() && self.composer.mode == ComposerMode::Plan {
                    self.composer.mode = ComposerMode::Build;
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
            _ => {}
        }
        Ok(None)
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

#[cfg(test)]
mod tests;

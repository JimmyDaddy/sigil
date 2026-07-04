use std::{
    cell::Ref,
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
mod formatting;
mod input_flow;
mod input_history;
mod key_router;
mod modal_flow;
mod mouse_flow;
mod runtime_status;
mod session_flow;
mod session_review;
mod setup_flow;
mod slash_flow;
mod state;
pub(crate) mod task_sidebar;
mod timeline_flow;
mod timeline_render_store;
mod tool_card_interaction;
mod worker_bridge;
mod workspace_trust_flow;

use anyhow::Result;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::text::Line;
use sigil_kernel::{
    AgentResultContinuationProjection, AgentThreadId, AgentThreadStateProjection, CompactionConfig,
    CompactionRecord, CompactionThresholdStatus, ConversationInputKind, ConversationInputQueueId,
    ConversationInputTarget, MemoryConfig, MutationArtifactCleanupTarget,
    MutationArtifactInventoryItem, MutationArtifactRetentionReport, PermissionMode,
    PlanApprovalPermission, PlanDraftCreatedEntry, PlanTaskStartMode, ReasoningEffort, RootConfig,
    SecretRedactor, Session, SessionConfig, SessionLogEntry, SessionStats, StorageConfig,
    TaskStateProjection, TerminalKeyboardEnhancement, ToolPreviewSnapshot, resolve_workspace_root,
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

use self::config_flow::cycle_permission_mode;
use self::formatting::*;
use self::modal_flow::{ModalState, ModelPickerRefresh};
use self::runtime_status::{McpProgressState, ResolvedUsageCostCurrency};
pub(crate) use self::runtime_status::{
    McpServerRuntimeStatus, TimelineTextSelection, code_intelligence_config_status,
    diagnostic_summary_label, initial_mcp_server_status, initial_mcp_server_statuses,
};
use self::session_flow::{current_focus_label, short_session_token};
use self::state::{ApprovalState, ComposerState, RuntimeStatusState, SessionBrowserState};
use self::timeline_render_store::TimelineRenderStore;

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
            }
            KeyCode::Char('d')
                if key.modifiers.contains(KeyModifiers::CONTROL)
                    && self.approval.pending.is_none() =>
            {
                self.unscroll_timeline(self.transcript_page_step());
            }
            KeyCode::Char('p')
                if key.modifiers.contains(KeyModifiers::CONTROL)
                    && self.approval.pending.is_none() =>
            {
                self.navigate_input_history(true);
            }
            KeyCode::Char('n')
                if key.modifiers.contains(KeyModifiers::CONTROL)
                    && self.approval.pending.is_none() =>
            {
                self.navigate_input_history(false);
            }
            KeyCode::Home
                if key.modifiers.contains(KeyModifiers::CONTROL)
                    && self.approval.pending.is_none() =>
            {
                self.scroll_timeline_to_top();
            }
            KeyCode::End
                if key.modifiers.contains(KeyModifiers::CONTROL)
                    && self.approval.pending.is_none() =>
            {
                self.unscroll_timeline(usize::MAX / 2);
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
            KeyCode::Tab if self.active_pane == PaneFocus::Composer && key.modifiers.is_empty() => {
                self.focus_composer_queue_panel();
            }
            KeyCode::Tab => {}
            KeyCode::BackTab if self.approval.pending.is_none() => {
                return self.toggle_runtime_permission_mode();
            }
            KeyCode::Up
                if self.active_pane == PaneFocus::Composer
                    && self.composer.input_history_index.is_some()
                    && key.modifiers.is_empty() =>
            {
                self.navigate_input_history(true);
            }
            KeyCode::Down
                if self.active_pane == PaneFocus::Composer
                    && self.composer.input_history_index.is_some()
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
                    self.navigate_input_history(false);
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
                    && self.composer.input.trim().is_empty()
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

    fn handle_pending_plan_approval_key_event(
        &mut self,
        key: KeyEvent,
    ) -> Option<Option<AppAction>> {
        self.composer.pending_plan_approval.as_ref()?;
        match key.code {
            KeyCode::Enter if self.composer.input.trim().is_empty() && key.modifiers.is_empty() => {
                Some(self.create_task_from_pending_plan(PlanTaskStartMode::CreateAndRun, None))
            }
            KeyCode::Esc if key.modifiers.is_empty() => Some(self.reject_pending_plan()),
            _ => None,
        }
    }

    fn reject_pending_plan(&mut self) -> Option<AppAction> {
        let pending = self.composer.pending_plan_approval.as_ref()?;
        let Some(plan_id) = pending.plan_id.clone() else {
            self.clear_pending_plan_approval();
            self.last_notice = Some("plan dismissed".to_owned());
            self.push_event("plan", "dismissed");
            return None;
        };
        let expected_plan_hash = pending.plan_hash.clone();
        self.last_notice = Some("rejecting plan".to_owned());
        self.push_event("plan", "reject");
        Some(AppAction::RejectPlan {
            plan_id,
            expected_plan_hash,
        })
    }

    fn create_task_from_pending_plan(
        &mut self,
        start_mode: PlanTaskStartMode,
        permission_grant: Option<PlanApprovalPermission>,
    ) -> Option<AppAction> {
        let pending = self.composer.pending_plan_approval.take()?;
        let Some(plan_id) = pending.plan_id else {
            self.last_notice = Some("plan is not durable yet".to_owned());
            self.composer.pending_plan_approval = Some(pending);
            return None;
        };
        self.last_notice = Some(match start_mode {
            PlanTaskStartMode::CreatePaused if permission_grant.is_some() => {
                "creating task with scoped edits".to_owned()
            }
            PlanTaskStartMode::CreatePaused => "creating task from plan".to_owned(),
            PlanTaskStartMode::CreateAndRun => "creating and running task from plan".to_owned(),
        });
        self.push_event("plan", "create_task");
        Some(AppAction::CreateTaskFromPlan {
            plan_id,
            expected_plan_hash: pending.plan_hash,
            start_mode,
            permission_grant,
        })
    }

    pub fn cache_hit_ratio(&self) -> f64 {
        let total = self.runtime.stats.cache_hit_tokens + self.runtime.stats.cache_miss_tokens;
        if total == 0 {
            0.0
        } else {
            self.runtime.stats.cache_hit_tokens as f64 / total as f64
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
        self.terminal_keyboard_enhancement_enabled
    }

    pub fn set_terminal_keyboard_enhancement_enabled(&mut self, enabled: bool) {
        self.terminal_keyboard_enhancement_enabled = enabled;
    }

    pub fn terminal_keyboard_enhancement_policy(&self) -> TerminalKeyboardEnhancement {
        self.config_snapshot
            .as_ref()
            .map(|config| config.terminal.keyboard_enhancement)
            .unwrap_or(TerminalKeyboardEnhancement::Off)
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
            self.rebuild_timeline_render_store();
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
        let prompt = self.composer.input.trim().to_owned();
        if prompt.is_empty() {
            return Ok(None);
        }
        self.discard_cleared_input_draft();
        self.record_input_history(prompt.clone());
        self.reset_input_history_navigation();

        if self.composer.queue_edit_target.is_some() {
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
            if self.runtime.is_busy {
                self.push_timeline(TimelineRole::Notice, "busy; @agent input kept for later");
                self.push_event("agent:busy", prompt);
                self.last_notice = Some("busy; @agent input kept for later".to_owned());
                return Ok(None);
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

        if self.runtime.is_busy {
            let (kind, target) = self.active_conversation_queue_submission();
            self.push_optimistic_conversation_queue_item(prompt.clone(), kind, target.clone());
            self.composer.input.clear();
            self.composer.input_cursor = 0;
            self.composer.input_paste_spans.clear();
            self.reset_slash_selector();
            self.last_notice = Some("follow-up will run next".to_owned());
            return Ok(Some(AppAction::QueueConversationInput {
                prompt,
                kind,
                target,
            }));
        }

        self.clear_pending_plan_approval();

        if self.composer.mode == ComposerMode::Plan {
            self.composer.input.clear();
            self.composer.input_cursor = 0;
            self.composer.input_paste_spans.clear();
            self.reset_slash_selector();
            self.timeline_scroll_back = 0;
            self.push_timeline(TimelineRole::User, prompt.clone());
            self.push_event("input", format!("submitted plan prompt {prompt}"));
            self.active_pane = PaneFocus::Composer;
            self.push_event("focus", current_focus_label(self));
            self.runtime.is_busy = true;
            self.runtime.run_phase = RunPhase::Thinking;
            self.last_notice = Some(ComposerMode::Plan.notice().to_owned());
            self.runtime.last_phase_marker = None;
            self.push_phase_marker(format!(
                "{}|{}",
                ComposerMode::Plan.phase_marker(),
                self.runtime.model_name
            ));
            self.streaming_assistant_index = None;
            self.streaming_reasoning_index = None;
            self.composer.mode = ComposerMode::Build;
            self.refresh_usage_sidebar_cache();
            return Ok(Some(AppAction::SubmitPlanPrompt(prompt)));
        }

        self.composer.input.clear();
        self.composer.input_cursor = 0;
        self.composer.input_paste_spans.clear();
        self.reset_slash_selector();
        self.timeline_scroll_back = 0;
        self.push_timeline(TimelineRole::User, prompt.clone());
        self.push_event("input", format!("submitted {prompt}"));
        self.active_pane = PaneFocus::Composer;
        self.push_event("focus", current_focus_label(self));
        self.runtime.is_busy = true;
        self.runtime.run_phase = RunPhase::Thinking;
        self.last_notice = Some("thinking".to_owned());
        self.runtime.last_phase_marker = None;
        self.push_phase_marker(format!("thinking|{}", self.runtime.model_name));
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
        self.composer.input.clear();
        self.composer.input_cursor = 0;
        self.composer.input_paste_spans.clear();
        self.pending_mouse_slash_confirmation = None;
        self.reset_slash_selector();
        self.push_event("slash", prompt.clone());
        match command.canonical.as_str() {
            "/compact" => {
                if self.runtime.is_busy {
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
                if self.runtime.is_busy {
                    self.push_timeline(TimelineRole::Notice, "busy; start new session later");
                    return Ok(None);
                }
                let session_log_path = self.new_session_log_path();
                Ok(Some(AppAction::StartNewSession { session_log_path }))
            }
            "/plan" => {
                if self.runtime.is_busy {
                    self.push_timeline(TimelineRole::Notice, "busy; plan later");
                    return Ok(None);
                }
                let arg = command.arg.trim();
                if arg.is_empty() {
                    self.composer.input.clear();
                    self.composer.input_cursor = 0;
                    self.composer.input_paste_spans.clear();
                    self.reset_slash_selector();
                    self.composer.mode = ComposerMode::Plan;
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
                self.composer.input.clear();
                self.composer.input_cursor = 0;
                self.composer.input_paste_spans.clear();
                self.reset_slash_selector();
                self.timeline_scroll_back = 0;
                self.push_timeline(TimelineRole::User, format!("/plan {plan_prompt}"));
                self.push_event("input", format!("submitted plan prompt {plan_prompt}"));
                self.active_pane = PaneFocus::Composer;
                self.push_event("focus", current_focus_label(self));
                self.runtime.is_busy = true;
                self.runtime.run_phase = RunPhase::Thinking;
                self.last_notice = Some(ComposerMode::Plan.notice().to_owned());
                self.runtime.last_phase_marker = None;
                self.push_phase_marker(format!(
                    "{}|{}",
                    ComposerMode::Plan.phase_marker(),
                    self.runtime.model_name
                ));
                self.streaming_assistant_index = None;
                self.streaming_reasoning_index = None;
                self.refresh_usage_sidebar_cache();
                Ok(Some(AppAction::SubmitPlanPrompt(plan_prompt)))
            }
            "/task" => {
                if self.runtime.is_busy {
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
                    self.runtime.is_busy = true;
                    self.runtime.run_phase = RunPhase::Thinking;
                    self.last_notice = Some("continuing task".to_owned());
                    self.runtime.last_phase_marker = None;
                    self.push_phase_marker(format!("task|{}", self.runtime.model_name));
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
                self.runtime.is_busy = true;
                self.runtime.run_phase = RunPhase::Thinking;
                self.last_notice = Some("planning task".to_owned());
                self.runtime.last_phase_marker = None;
                self.push_phase_marker(format!("task|{}", self.runtime.model_name));
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
                if self.runtime.is_busy {
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
        if self.runtime.is_busy {
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
        self.composer.input.clear();
        self.composer.input_cursor = 0;
        self.composer.input_paste_spans.clear();
        self.reset_slash_selector();
        self.timeline_scroll_back = 0;
        self.push_timeline(TimelineRole::User, prompt.clone());
        self.push_event("input", format!("invoked agent {profile_id}"));
        self.active_pane = PaneFocus::Composer;
        self.push_event("focus", current_focus_label(self));
        self.runtime.is_busy = true;
        self.runtime.run_phase = RunPhase::Agent(profile_id.clone());
        self.last_notice = Some(format!("waiting for agent @{profile_id}"));
        self.runtime.last_phase_marker = None;
        self.push_phase_marker(format!("agent|{profile_id}"));
        self.streaming_assistant_index = None;
        self.streaming_reasoning_index = None;
        self.composer.mode = ComposerMode::Build;
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
        if self.runtime.is_busy {
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
        self.runtime.is_busy = true;
        self.runtime.run_phase = RunPhase::Thinking;
        self.runtime.last_phase_marker = None;
        self.streaming_assistant_index = None;
        self.streaming_reasoning_index = None;
        self.refresh_usage_sidebar_cache();

        let arguments = command.arg.trim().to_owned();
        match skill.run_as {
            sigil_kernel::SkillRunMode::Inline => {
                self.last_notice = Some(format!("using skill {skill_id}"));
                self.push_phase_marker(format!("thinking|{}", self.runtime.model_name));
                Ok(Some(AppAction::InvokeInlineSkill {
                    skill_id: skill_id.to_owned(),
                    arguments,
                }))
            }
            sigil_kernel::SkillRunMode::ChildSession => {
                self.last_notice = Some(format!("invoking agent {skill_id}"));
                self.push_phase_marker(format!("task|{}", self.runtime.model_name));
                Ok(Some(AppAction::InvokeChildSessionSkill {
                    skill_id: skill_id.to_owned(),
                    arguments,
                }))
            }
        }
    }

    pub(crate) fn run_phase(&self) -> RunPhase {
        self.runtime.run_phase.clone()
    }

    pub(crate) fn run_phase_label(&self) -> String {
        match &self.runtime.run_phase {
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
                    self.runtime.model_name
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
            format!("provider: {}", self.runtime.provider_name),
            format!("model: {}", self.runtime.model_name),
            format!("effort: {}", self.runtime.reasoning_effort.as_str()),
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

    pub(crate) fn session_review_sidebar_lines(&self) -> Vec<String> {
        self.session_view_cache().session_review_lines.clone()
    }

    pub(crate) fn task_sidebar_lines(&self) -> Vec<String> {
        self.session_view_cache().task_sidebar_lines.clone()
    }

    pub(crate) fn task_strip_view(&self) -> Option<task_sidebar::TaskStripView> {
        self.session_view_cache().task_strip_view.clone()
    }

    fn session_view_cache(&self) -> Ref<'_, SessionViewCache> {
        self.ensure_session_view_cache();
        self.session_browser.view_cache.borrow()
    }

    fn ensure_session_view_cache(&self) {
        let needs_refresh = {
            let cache = self.session_browser.view_cache.borrow();
            cache.entries_len != self.session_browser.current_entries.len()
                || cache.entries_revision != self.session_browser.current_entries_revision
        };
        if needs_refresh {
            let cache = self.build_session_view_cache();
            *self.session_browser.view_cache.borrow_mut() = cache;
        }
    }

    fn mark_current_session_entries_changed(&mut self) {
        self.session_browser.current_entries_revision = self
            .session_browser
            .current_entries_revision
            .saturating_add(1);
        self.refresh_session_view_cache();
    }

    fn refresh_session_view_cache(&mut self) {
        let cache = self.build_session_view_cache();
        *self.session_browser.view_cache.borrow_mut() = cache;
    }

    fn build_session_view_cache(&self) -> SessionViewCache {
        let entries = &self.session_browser.current_entries;
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
            entries_revision: self.session_browser.current_entries_revision,
            task_projection,
            agent_projection,
            task_sidebar_lines: task_sidebar::task_sidebar_lines(entries),
            task_strip_view: task_sidebar::task_strip_view(entries),
            agent_child_items,
            agent_graph_summary_line,
            compaction_preview_line: self.compaction_preview_sidebar_line(entries),
            session_review_lines: session_review::session_review_sidebar_lines(
                &self.session_log_path,
                entries,
            ),
        }
    }

    fn compaction_preview_sidebar_line(&self, entries: &[SessionLogEntry]) -> Option<String> {
        if entries.is_empty() {
            return None;
        }
        let session = Session::from_entries(
            self.runtime.provider_name.clone(),
            self.runtime.model_name.clone(),
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
        self.composer.pending_plan_approval.as_ref()
    }

    pub(crate) fn set_pending_plan_approval_from_draft(&mut self, draft: &PlanDraftCreatedEntry) {
        if draft.steps.is_empty() {
            self.composer.pending_plan_approval = None;
            return;
        }
        let plan_text = draft
            .inline_text
            .clone()
            .unwrap_or_else(|| draft.summary.clone());
        let plan_text = plan_text.trim();
        if plan_text.is_empty() {
            self.composer.pending_plan_approval = None;
            return;
        }
        let steps = draft
            .steps
            .iter()
            .map(|step| step.title.clone())
            .collect::<Vec<_>>();
        let suggested_checks = draft
            .suggested_checks
            .iter()
            .map(|check| {
                std::iter::once(check.command.command.as_str())
                    .chain(check.command.args.iter().map(String::as_str))
                    .collect::<Vec<_>>()
                    .join(" ")
            })
            .collect::<Vec<_>>();
        self.composer.pending_plan_approval = Some(PendingPlanApproval {
            plan_id: Some(draft.plan_id.as_str().to_owned()),
            plan_text: plan_text.to_owned(),
            plan_hash: draft.plan_hash.clone(),
            summary: draft.summary.clone(),
            steps,
            target_paths: draft.target_paths.clone(),
            suggested_checks,
            target_path_count: draft.target_paths.len(),
            suggested_check_count: draft.suggested_checks.len(),
        });
    }

    fn clear_pending_plan_approval(&mut self) {
        self.composer.pending_plan_approval = None;
    }

    pub(crate) fn composer_mode_label(&self) -> &'static str {
        if self.composer.pending_plan_approval.is_some() {
            return ComposerMode::Plan.label();
        }
        self.composer.mode.label()
    }

    pub(crate) fn reasoning_effort_label(&self) -> &'static str {
        self.runtime.reasoning_effort.as_str()
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
                format_token_compact(self.runtime.stats.last_prompt_tokens),
                format_token_compact(cap as u64),
                context_window_source_label(resolved.source),
                self.context_usage_hint(cap)
            ),
            _ => format!(
                "ctx: n/a · prompt {} · set fallback_context_window_tokens",
                format_token_compact(self.runtime.stats.last_prompt_tokens)
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
            format!("mode: {}", self.runtime.permission_mode),
            "Shift-Tab cycle + save".to_owned(),
            if self.runtime.is_busy {
                "busy: locked during run".to_owned()
            } else {
                "scope: saved default".to_owned()
            },
        ]
    }

    fn refresh_usage_sidebar_cache(&mut self) {
        let currency = self.usage_cost_currency();
        let session_spent = self.runtime.stats.input_cost + self.runtime.stats.output_cost;
        let delta_spent = self.runtime.session_delta_stats.input_cost
            + self.runtime.session_delta_stats.output_cost;
        let saved = self.runtime.stats.cache_savings;
        let session_spent = currency.format_cost(session_spent);
        let delta_spent = currency.format_cost(delta_spent);
        let saved = currency.format_cost(saved);
        let balance_line = self.balance_sidebar_line();
        let mut lines = vec![
            self.context_usage_line(),
            self.session_token_line(),
            format!("compact: {}", self.runtime.compaction_status),
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
        let compaction_preview_line = if self.runtime.is_busy {
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
        if self.runtime.balance_snapshot.available {
            match (
                self.runtime.balance_snapshot.total,
                self.runtime.balance_snapshot.currency.as_deref(),
            ) {
                (Some(total), Some(currency)) => format!("balance: {currency} {total:.2}"),
                _ => format!("balance: {}", self.runtime.balance_snapshot.status),
            }
        } else {
            format!("balance: {}", self.runtime.balance_snapshot.status)
        }
    }

    fn session_token_line(&self) -> String {
        format!(
            "session tok: input {} · output {}",
            format_token_compact(self.runtime.stats.prompt_tokens),
            format_token_compact(self.runtime.stats.completion_tokens)
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
            self.runtime.balance_snapshot.currency.as_deref(),
        )
    }

    #[cfg(test)]
    pub(crate) fn footer_status_line(&self) -> String {
        let currency = self.usage_cost_currency();
        let session_spent = self.runtime.stats.input_cost + self.runtime.stats.output_cost;
        let delta_spent = self.runtime.session_delta_stats.input_cost
            + self.runtime.session_delta_stats.output_cost;
        let session_spent = currency.format_cost(session_spent);
        let delta_spent = currency.format_cost(delta_spent);
        let token_line = format!(
            "tok {}",
            format_token_compact(self.runtime.stats.last_prompt_tokens)
        );
        let context = match self.resolved_context_window().tokens {
            Some(cap) if cap > 0 => format!("ctx {}%", self.context_usage_percent(cap)),
            _ => "ctx n/a".to_owned(),
        };
        format!(
            "{}  ·  {}  ·  cache {:.0}%  ·  spent {delta_spent} since opening / {session_spent} total  ·  mode {}  ·  Ctrl-C {}",
            token_line,
            context,
            self.cache_hit_ratio() * 100.0,
            self.runtime.permission_mode,
            if self.runtime.is_busy {
                "cancel"
            } else {
                "quit"
            }
        )
    }

    fn resolved_context_window(&self) -> sigil_runtime::ResolvedContextWindow {
        resolve_context_window_tokens(
            &self.runtime.provider_name,
            &self.runtime.model_name,
            self.compaction_config.context_window_tokens,
        )
    }

    fn resolved_compaction_config(&self) -> CompactionConfig {
        effective_compaction_config(
            &self.runtime.provider_name,
            &self.runtime.model_name,
            &self.compaction_config,
        )
    }

    fn context_usage_percent(&self, cap: u32) -> u64 {
        ((self.runtime.stats.last_prompt_tokens as f64 / cap as f64) * 100.0)
            .round()
            .clamp(0.0, 999.0) as u64
    }

    fn context_usage_hint(&self, cap: u32) -> String {
        match self
            .resolved_compaction_config()
            .threshold_status(self.runtime.stats.last_prompt_tokens)
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
            .threshold_status(self.runtime.stats.last_prompt_tokens);
        let next_label = next.as_str().to_owned();
        if self.runtime.compaction_status == next_label {
            return;
        }

        self.runtime.compaction_status = next_label.clone();
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
        if self.runtime.is_busy {
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
        next_config.permission.mode = cycle_permission_mode(next_config.permission.mode);
        persisted_root_config(&next_config).save(&self.config_path)?;
        self.apply_runtime_config_snapshot(&next_config);
        self.last_notice = Some(format!(
            "permission mode = {}",
            next_config.permission.mode.as_str()
        ));
        self.push_event("permission_mode", self.runtime.permission_mode.clone());
        self.push_timeline(
            TimelineRole::Notice,
            format!("permission mode -> {}", self.runtime.permission_mode),
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

        self.runtime.reasoning_effort = effort.clone();
        self.last_notice = Some(format!("reasoning effort = {}", effort.as_str()));
        self.push_event("effort", effort.as_str());
        self.push_timeline(
            TimelineRole::Notice,
            format!("reasoning effort -> {}", effort.as_str()),
        );
        Ok(None)
    }

    fn set_runtime_model_from_command(&mut self, argument: &str) -> Result<Option<AppAction>> {
        if self.runtime.is_busy {
            self.last_notice = Some("busy; model locked".to_owned());
            self.push_timeline(TimelineRole::Notice, "busy; switch model after the run");
            return Ok(None);
        }

        let Some(model) = normalize_runtime_model(argument) else {
            self.last_notice = Some("usage: /model <flash|pro|id>".to_owned());
            self.push_timeline(TimelineRole::Notice, "usage: /model <flash|pro|id>");
            return Ok(None);
        };

        if model == self.runtime.model_name {
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

fn threshold_token_count(cap: u32, ratio: f32) -> u64 {
    (f64::from(cap) * f64::from(ratio.max(0.0))).round() as u64
}

#[cfg(test)]
mod tests;

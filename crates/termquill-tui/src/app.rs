use std::{
    collections::{BTreeSet, HashMap},
    env,
    ops::Range,
    path::{Path, PathBuf},
    sync::mpsc::Receiver,
};

mod approval_flow;
mod command_dispatch;
mod config_flow;
mod formatting;
mod input_flow;
mod modal_flow;
mod mouse_flow;
mod session_flow;
mod setup_flow;
mod slash_flow;
mod timeline_flow;
mod tool_focus;
mod worker_bridge;

use anyhow::{Result, bail};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::text::Line;
use termquill_kernel::{
    AgentConfig, ApprovalMode, CodeIntelStartup, CodeIntelligenceConfig, CompactionConfig,
    CompactionRecord, CompactionThresholdStatus, MemoryConfig, PermissionConfig, ReasoningEffort,
    RootConfig, Session, SessionConfig, SessionLogEntry, SessionStats, ToolPreviewSnapshot,
    WorkspaceConfig, resolve_workspace_root,
};
use termquill_provider_deepseek::{DeepSeekProviderConfig, StrictToolsMode, TERMQUILL_API_KEY_ENV};
use uuid::Uuid;

pub(crate) use crate::approval::{
    ApprovalAction, ApprovalDiffLine, ApprovalDiffLineKind, ApprovalFileRow, ApprovalModalView,
};
pub use crate::approval::{ApprovalDiffMode, PendingApproval};
use crate::commands::{
    UiCommand, command_for_key_event, keyboard_help_lines, metadata_slash_commands,
    metadata_slash_help_lines,
};
pub(crate) use crate::config_panel::{
    ConfigDraft, ConfigField, ConfigFieldMove, ConfigFooterAction, ConfigSection, ConfigState,
    config_field_accepts_char, default_deepseek_provider_config, load_deepseek_provider_config,
    render_config_value_row, serialize_deepseek_provider_value,
};
use crate::context_window::{
    ContextWindowSource, effective_compaction_config, resolve_context_window_tokens,
};
pub use crate::input::PaneFocus;
use crate::provider_status::BalanceSnapshot;
pub use crate::sessions::{SessionHistoryEntry, SessionViewMode};
pub(crate) use crate::setup::{SetupField, SetupState};
pub use crate::timeline::{EventEntry, TimelineEntry, TimelineRole};
pub(crate) use crate::timeline::{
    LiveActivitySummary, RunPhase, SessionHistoryRow, SidebarAgentRow, SidebarCard,
    ThinkingBlockMode,
};

use self::config_flow::cycle_approval_mode;
use self::formatting::*;
use self::modal_flow::{
    ModalState, ModelPickerRefresh, ModelPickerTarget, SecretInputTarget, TextInputState,
    TextInputTarget,
};
use self::session_flow::{current_focus_label, short_session_token};

const SESSION_HISTORY_TITLE_SCAN_LIMIT: usize = 256;

fn code_intelligence_config_status(config: &CodeIntelligenceConfig) -> String {
    if !config.enabled || config.startup == CodeIntelStartup::Off {
        "off".to_owned()
    } else {
        config.startup.as_str().to_owned()
    }
}

#[derive(Debug)]
pub struct AppState {
    pub config_path: PathBuf,
    pub workspace_root: PathBuf,
    pub session_log_dir: PathBuf,
    pub session_log_path: PathBuf,
    pub provider_name: String,
    pub model_name: String,
    pub permission_default_mode: String,
    pub memory_enabled: bool,
    pub memory_document_count: usize,
    pub memory_last_status: String,
    pub compaction_status: String,
    pub code_intelligence_status: String,
    pub session_id: String,
    pub input: String,
    pub input_history: Vec<String>,
    pub timeline: Vec<TimelineEntry>,
    pub events: Vec<EventEntry>,
    pub stats: SessionStats,
    pub should_quit: bool,
    pub is_busy: bool,
    pub pending_approval: Option<PendingApproval>,
    pub active_pane: PaneFocus,
    pub timeline_scroll_back: usize,
    pub approval_scroll_back: usize,
    pub activity_scroll_back: usize,
    pub session_history: Vec<SessionHistoryEntry>,
    pub session_history_visible_limit: usize,
    pub session_history_selected: usize,
    pub session_history_filter: String,
    config_snapshot: Option<RootConfig>,
    setup_state: Option<SetupState>,
    config_state: Option<ConfigState>,
    modal_state: Option<ModalState>,
    session_view_mode: SessionViewMode,
    current_session_entries: Vec<SessionLogEntry>,
    tool_preview_snapshots: HashMap<String, ToolPreviewSnapshot>,
    latest_compaction_record: Option<CompactionRecord>,
    compaction_config: CompactionConfig,
    memory_config: MemoryConfig,
    thinking_block_mode: ThinkingBlockMode,
    selected_tool_activity_key: Option<String>,
    expanded_tool_activity_keys: BTreeSet<String>,
    collapsed_tool_activity_keys: BTreeSet<String>,
    last_notice: Option<String>,
    reasoning_effort: ReasoningEffort,
    run_phase: RunPhase,
    last_phase_marker: Option<String>,
    streaming_assistant_index: Option<usize>,
    streaming_reasoning_index: Option<usize>,
    timeline_render_cache: Vec<Line<'static>>,
    timeline_plain_cache: Vec<String>,
    timeline_prefix_hashes: Vec<u64>,
    timeline_render_ranges: Vec<Range<usize>>,
    timeline_revision: u64,
    usage_sidebar_cache: Vec<String>,
    sidebar_selected_card: SidebarCard,
    sidebar_agent_selected: usize,
    balance_snapshot: BalanceSnapshot,
    balance_refresh_rx: Option<Receiver<BalanceSnapshot>>,
    model_picker_refresh_rx: Option<Receiver<ModelPickerRefresh>>,
    terminal_width: u16,
    terminal_height: u16,
    input_cursor: usize,
    input_history_index: Option<usize>,
    input_history_draft: Option<String>,
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
    ApprovalDecision {
        call_id: String,
        approved: bool,
    },
    CancelRun,
    CompactNow,
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
        let session_log_dir = workspace_root.join(&root_config.session.log_dir);
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
            session_log_dir,
            session_log_path: PathBuf::new(),
            provider_name: root_config.agent.provider.clone(),
            model_name: root_config.agent.model.clone(),
            permission_default_mode,
            memory_enabled: root_config.memory.enabled,
            memory_document_count: 0,
            memory_last_status: "pending".to_owned(),
            compaction_status: initial_compaction_status,
            code_intelligence_status: initial_code_intelligence_status,
            session_id,
            input: String::new(),
            input_history: Vec::new(),
            timeline: Vec::new(),
            events: Vec::new(),
            stats: SessionStats::default(),
            should_quit: false,
            is_busy: false,
            pending_approval: None,
            active_pane: PaneFocus::Composer,
            timeline_scroll_back: 0,
            approval_scroll_back: 0,
            activity_scroll_back: 0,
            session_history: Vec::new(),
            session_history_visible_limit: 9,
            session_history_selected: 0,
            session_history_filter: String::new(),
            config_snapshot: Some(root_config.clone()),
            setup_state: None,
            config_state: None,
            modal_state: None,
            session_view_mode: SessionViewMode::Provider,
            current_session_entries: Vec::new(),
            tool_preview_snapshots: HashMap::new(),
            latest_compaction_record: None,
            compaction_config: root_config.compaction.clone(),
            memory_config: root_config.memory.clone(),
            thinking_block_mode: ThinkingBlockMode::Collapsed,
            selected_tool_activity_key: None,
            expanded_tool_activity_keys: BTreeSet::new(),
            collapsed_tool_activity_keys: BTreeSet::new(),
            last_notice: None,
            reasoning_effort: ReasoningEffort::Max,
            run_phase: RunPhase::Idle,
            last_phase_marker: None,
            streaming_assistant_index: None,
            streaming_reasoning_index: None,
            timeline_render_cache: Vec::new(),
            timeline_plain_cache: Vec::new(),
            timeline_prefix_hashes: Vec::new(),
            timeline_render_ranges: Vec::new(),
            timeline_revision: 0,
            usage_sidebar_cache: Vec::new(),
            sidebar_selected_card: SidebarCard::Permission,
            sidebar_agent_selected: 0,
            balance_snapshot: BalanceSnapshot {
                status: "pending".to_owned(),
                ..BalanceSnapshot::default()
            },
            balance_refresh_rx: None,
            model_picker_refresh_rx: None,
            terminal_width: 120,
            terminal_height: 32,
            input_cursor: 0,
            input_history_index: None,
            input_history_draft: None,
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
        let session_log_dir = workspace_root.join(".termquill/sessions");
        let session_id = Uuid::new_v4().to_string();
        let mut app = Self {
            config_path: config_path.clone(),
            workspace_root: workspace_root.clone(),
            session_log_dir,
            session_log_path: PathBuf::new(),
            provider_name: "deepseek".to_owned(),
            model_name: "deepseek-v4-flash".to_owned(),
            permission_default_mode: ApprovalMode::Ask.as_str().to_owned(),
            memory_enabled: true,
            memory_document_count: 0,
            memory_last_status: "pending".to_owned(),
            compaction_status: CompactionThresholdStatus::NotAvailable.as_str().to_owned(),
            code_intelligence_status: "off".to_owned(),
            session_id,
            input: String::new(),
            input_history: Vec::new(),
            timeline: Vec::new(),
            events: Vec::new(),
            stats: SessionStats::default(),
            should_quit: false,
            is_busy: false,
            pending_approval: None,
            active_pane: PaneFocus::Composer,
            timeline_scroll_back: 0,
            approval_scroll_back: 0,
            activity_scroll_back: 0,
            session_history: Vec::new(),
            session_history_visible_limit: 9,
            session_history_selected: 0,
            session_history_filter: String::new(),
            config_snapshot: None,
            setup_state: Some(SetupState::new(config_path, startup_error.clone())),
            config_state: None,
            modal_state: None,
            session_view_mode: SessionViewMode::Provider,
            current_session_entries: Vec::new(),
            tool_preview_snapshots: HashMap::new(),
            latest_compaction_record: None,
            compaction_config: CompactionConfig::default(),
            memory_config: MemoryConfig::default(),
            thinking_block_mode: ThinkingBlockMode::Collapsed,
            selected_tool_activity_key: None,
            expanded_tool_activity_keys: BTreeSet::new(),
            collapsed_tool_activity_keys: BTreeSet::new(),
            last_notice: startup_error,
            reasoning_effort: ReasoningEffort::Max,
            run_phase: RunPhase::Idle,
            last_phase_marker: None,
            streaming_assistant_index: None,
            streaming_reasoning_index: None,
            timeline_render_cache: Vec::new(),
            timeline_plain_cache: Vec::new(),
            timeline_prefix_hashes: Vec::new(),
            timeline_render_ranges: Vec::new(),
            timeline_revision: 0,
            usage_sidebar_cache: Vec::new(),
            sidebar_selected_card: SidebarCard::Permission,
            sidebar_agent_selected: 0,
            balance_snapshot: BalanceSnapshot {
                status: "missing auth".to_owned(),
                ..BalanceSnapshot::default()
            },
            balance_refresh_rx: None,
            model_picker_refresh_rx: None,
            terminal_width: 120,
            terminal_height: 32,
            input_cursor: 0,
            input_history_index: None,
            input_history_draft: None,
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
        app.bootstrap_setup();
        app.refresh_usage_sidebar_cache();
        app
    }

    fn bootstrap(&mut self) {
        self.timeline.clear();
        self.events.clear();
        self.push_timeline(TimelineRole::System, "termquill ready.");
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
        self.events.clear();
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

    fn reset_for_new_session(&mut self, provider_name: String, model_name: String, notice: String) {
        self.provider_name = provider_name;
        self.model_name = model_name;
        self.session_id = Uuid::new_v4().to_string();
        self.session_log_path = self
            .session_log_dir
            .join(format!("session-{}.jsonl", self.session_id));
        self.stats = SessionStats::default();
        self.is_busy = false;
        self.pending_approval = None;
        self.active_pane = PaneFocus::Composer;
        self.timeline_scroll_back = 0;
        self.approval_scroll_back = 0;
        self.activity_scroll_back = 0;
        self.current_session_entries.clear();
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
        self.expanded_tool_activity_keys.clear();
        self.collapsed_tool_activity_keys.clear();
        self.bootstrap();
        self.last_notice = Some(notice.clone());
        self.push_timeline(TimelineRole::Notice, notice);
        self.refresh_session_history();
        self.refresh_usage_sidebar_cache();
    }

    pub fn handle_key_event(&mut self, key: KeyEvent) -> Result<Option<AppAction>> {
        if self.is_setup_mode() {
            return self.handle_setup_key_event(key);
        }
        if self.is_config_mode() {
            return self.handle_config_key_event(key);
        }

        if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
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

        if self.active_pane == PaneFocus::Activity
            && self.pending_approval.is_none()
            && !key.modifiers.contains(KeyModifiers::CONTROL)
        {
            match key.code {
                KeyCode::Char(character)
                    if normalize_command_prefix_character(character).is_some() =>
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
                KeyCode::PageUp | KeyCode::Home => {
                    self.sidebar_selected_card = SidebarCard::Permission;
                    self.sidebar_agent_selected = 0;
                    return Ok(None);
                }
                KeyCode::PageDown | KeyCode::End => {
                    self.sidebar_selected_card = SidebarCard::Usage;
                    return Ok(None);
                }
                KeyCode::Enter if self.sidebar_selected_card == SidebarCard::Agents => {
                    let detail = self
                        .agent_sidebar_rows()
                        .get(self.sidebar_agent_selected)
                        .map(|row| row.detail.clone())
                        .unwrap_or_else(|| "no agent selected".to_owned());
                    self.last_notice = Some(detail.clone());
                    self.push_timeline(TimelineRole::Notice, detail);
                    return Ok(None);
                }
                KeyCode::Esc => {
                    self.active_pane = PaneFocus::Composer;
                    return Ok(None);
                }
                KeyCode::Char(_) | KeyCode::Backspace | KeyCode::Left | KeyCode::Right => {
                    return Ok(None);
                }
                _ => {}
            }
        }

        if let Some(command) = command_for_key_event(key) {
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
                if self.selected_tool_activity_key.is_some() && self.toggle_selected_tool_card() {
                    return Ok(None);
                }
                self.toggle_thinking_block_mode();
            }
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {}
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
            KeyCode::Tab => {}
            KeyCode::BackTab if self.pending_approval.is_none() => {
                return self.toggle_runtime_permission_mode();
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
            KeyCode::PageUp => self.scroll_timeline(self.transcript_page_step()),
            KeyCode::PageDown => self.unscroll_timeline(self.transcript_page_step()),
            KeyCode::Home => self.scroll_timeline_to_top(),
            KeyCode::End => self.unscroll_timeline(usize::MAX / 2),
            KeyCode::Esc => {
                if self.input.is_empty() && self.handle_ui_command(UiCommand::ClearToolCardFocus) {
                    return Ok(None);
                }
                self.input.clear();
                self.input_cursor = 0;
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
            KeyCode::Backspace => {
                self.active_pane = PaneFocus::Composer;
                self.remove_input_character_before_cursor();
                self.reset_input_history_navigation();
                self.reset_slash_selector();
            }
            _ if self.active_pane == PaneFocus::Composer && is_composer_newline_key(key) => {
                self.active_pane = PaneFocus::Composer;
                self.insert_input_character('\n');
                self.reset_input_history_navigation();
                self.reset_slash_selector();
            }
            _ if is_composer_submit_key(key) => {
                self.active_pane = PaneFocus::Composer;
                if self.should_accept_slash_selector_on_enter() {
                    self.accept_slash_selector();
                    return Ok(None);
                }
                return self.submit_input();
            }
            KeyCode::Char(character) if is_composer_text_key(key) => {
                self.active_pane = PaneFocus::Composer;
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
        }
        self.timeline_scroll_back = self
            .timeline_scroll_back
            .min(self.max_timeline_scroll_back());
        width_changed || height_changed
    }

    pub(crate) fn footer_strip_height(&self) -> u16 {
        let desired = self.composer_height();
        desired.min(self.terminal_height.saturating_sub(2).max(4))
    }

    pub fn submit_input(&mut self) -> Result<Option<AppAction>> {
        let prompt = self.input.trim().to_owned();
        if prompt.is_empty() {
            return Ok(None);
        }
        self.record_input_history(prompt.clone());
        self.reset_input_history_navigation();

        if prompt.starts_with('/') {
            let Some(command) = self.resolve_slash_command(&prompt) else {
                self.push_timeline(TimelineRole::Notice, "unknown slash command");
                self.push_event("slash:unknown", prompt.clone());
                self.last_notice = Some("unknown slash command".to_owned());
                return Ok(None);
            };

            self.input.clear();
            self.input_cursor = 0;
            self.reset_slash_selector();
            self.push_event("slash", prompt.clone());
            return match command.canonical {
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
                "/effort" => self.set_runtime_reasoning_effort_from_command(&command.arg),
                "/model" => self.set_runtime_model_from_command(&command.arg),
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
                    self.push_timeline(TimelineRole::Notice, "unknown slash command");
                    Ok(None)
                }
            };
        }

        if self.is_busy {
            self.push_timeline(TimelineRole::Notice, "busy; submit later");
            self.push_event("notice", "submit ignored while busy");
            return Ok(None);
        }

        self.input.clear();
        self.input_cursor = 0;
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

    pub(crate) fn run_phase(&self) -> RunPhase {
        self.run_phase.clone()
    }

    pub(crate) fn run_phase_label(&self) -> String {
        match &self.run_phase {
            RunPhase::Idle => "ready".to_owned(),
            RunPhase::Thinking => "thinking".to_owned(),
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

    pub(crate) fn reasoning_effort_label(&self) -> &'static str {
        self.reasoning_effort.as_str()
    }

    pub(crate) fn context_usage_line(&self) -> String {
        match self.displayed_context_window_tokens() {
            Some(cap) if cap > 0 => format!(
                "ctx: {}% · {} / {} tok",
                self.context_usage_percent(cap),
                format_token_compact(self.stats.last_prompt_tokens),
                format_token_compact(cap as u64)
            ),
            _ => format!(
                "ctx: n/a · {} tok",
                format_token_compact(self.stats.last_prompt_tokens)
            ),
        }
    }

    pub(crate) fn compaction_policy_line(&self) -> String {
        let resolved = self.resolved_context_window();
        match resolved.tokens {
            Some(cap) if cap > 0 => format!(
                "policy: {} {} · soft {}% · hard {}%",
                format_token_count(cap as u64),
                match resolved.source {
                    ContextWindowSource::Provider => "model",
                    ContextWindowSource::Config => "cfg",
                    ContextWindowSource::None => "n/a",
                },
                ratio_to_percent(self.compaction_config.soft_threshold_ratio),
                ratio_to_percent(self.compaction_config.hard_threshold_ratio)
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

    pub(crate) fn agent_sidebar_rows(&self) -> Vec<SidebarAgentRow> {
        let rows = [
            (
                "main".to_owned(),
                if self.is_busy {
                    "running in current session".to_owned()
                } else {
                    "idle in current session".to_owned()
                },
                false,
            ),
            ("subagents".to_owned(), "not connected yet".to_owned(), true),
        ];
        rows.into_iter()
            .enumerate()
            .map(|(index, (label, detail, muted))| SidebarAgentRow {
                label,
                detail,
                selected: index == self.sidebar_agent_selected,
                muted,
            })
            .collect()
    }

    fn refresh_usage_sidebar_cache(&mut self) {
        let spent = self.stats.input_cost + self.stats.output_cost;
        let saved = self.stats.cache_savings;
        let balance_line = self.balance_sidebar_line();
        let mut lines = vec![
            self.context_usage_line(),
            format!("compact: {}", self.compaction_status),
            self.compaction_policy_line(),
            self.tool_card_status_line(),
            format!(
                "cache: {:.0}% · save ${saved:.4}",
                self.cache_hit_ratio() * 100.0
            ),
            format!("spent: ${spent:.4}"),
            balance_line,
        ];
        if !self.is_busy && !self.current_session_entries.is_empty() {
            let session = Session::from_entries(
                self.provider_name.clone(),
                self.model_name.clone(),
                self.current_session_entries.clone(),
            );
            match session.compaction_preview(&self.compaction_config) {
                Ok(Some(preview)) => lines.push(format!(
                    "compact: fold {} keep {}",
                    preview.record.compacted_message_count,
                    preview.record.retained_tail_message_count
                )),
                Ok(None) => lines.push("compact: nothing to fold".to_owned()),
                Err(error) => lines.push(format!(
                    "compact: {}",
                    truncate_session_view_text(&error.to_string(), 28)
                )),
            }
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

    #[cfg(test)]
    pub(crate) fn footer_status_line(&self) -> String {
        let spent = self.stats.input_cost + self.stats.output_cost;
        let token_line = format!(
            "tok {}",
            format_token_compact(self.stats.last_prompt_tokens)
        );
        let context = match self.displayed_context_window_tokens() {
            Some(cap) if cap > 0 => format!("ctx {}%", self.context_usage_percent(cap)),
            _ => "ctx n/a".to_owned(),
        };
        format!(
            "{}  ·  {}  ·  cache {:.0}%  ·  spent ${spent:.4}  ·  write {}  ·  Ctrl-C {}",
            token_line,
            context,
            self.cache_hit_ratio() * 100.0,
            self.permission_default_mode,
            if self.is_busy { "cancel" } else { "quit" }
        )
    }

    fn displayed_context_window_tokens(&self) -> Option<u32> {
        self.resolved_context_window().tokens
    }

    fn resolved_context_window(&self) -> crate::context_window::ResolvedContextWindow {
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
        next_config.agent.model = model.clone();
        let mut provider_config = load_deepseek_provider_config(&next_config)
            .unwrap_or_else(|| default_deepseek_provider_config(&model));
        provider_config.model = model.clone();
        next_config.providers.insert(
            "deepseek".to_owned(),
            serialize_deepseek_provider_value(&provider_config)?,
        );

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

fn is_composer_newline_key(key: KeyEvent) -> bool {
    key.modifiers.contains(KeyModifiers::SHIFT)
        && matches!(
            key.code,
            KeyCode::Enter | KeyCode::Char('\n') | KeyCode::Char('\r')
        )
}

fn is_composer_submit_key(key: KeyEvent) -> bool {
    !key.modifiers.contains(KeyModifiers::SHIFT)
        && matches!(
            key.code,
            KeyCode::Enter | KeyCode::Char('\n') | KeyCode::Char('\r')
        )
}

fn is_composer_text_key(key: KeyEvent) -> bool {
    matches!(key.code, KeyCode::Char(character) if !key.modifiers.contains(KeyModifiers::CONTROL) && !character.is_control())
}

#[cfg(test)]
mod tests;

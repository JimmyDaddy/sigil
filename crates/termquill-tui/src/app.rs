use std::{
    collections::BTreeSet,
    env,
    ops::Range,
    path::{Path, PathBuf},
    sync::mpsc::{self, Receiver},
    thread,
};

mod approval_flow;
mod command_dispatch;
mod config_flow;
mod formatting;
mod input_flow;
mod modal_flow;
mod session_flow;
mod slash_flow;
mod timeline_flow;
mod tool_focus;

use anyhow::{Result, bail};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::text::Line;
use termquill_kernel::{
    AgentConfig, ApprovalMode, CompactionConfig, CompactionRecord, CompactionThresholdStatus,
    EventHandler, MemoryConfig, PermissionConfig, ReasoningEffort, RootConfig, RunEvent, Session,
    SessionConfig, SessionLogEntry, SessionStats, WorkspaceConfig, resolve_workspace_root,
};
use termquill_provider_deepseek::{DeepSeekProviderConfig, StrictToolsMode, TERMQUILL_API_KEY_ENV};
use uuid::Uuid;

pub(crate) use crate::approval::{
    ApprovalDiffLine, ApprovalDiffLineKind, ApprovalFileRow, ApprovalModalView,
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
use crate::provider_status::{
    BalanceSnapshot, fetch_provider_balance_snapshot, fetch_remote_model_ids,
    resolve_provider_api_key,
};
use crate::runner::{CompactionTrigger, WorkerCommand, WorkerMessage};
pub use crate::sessions::{SessionHistoryEntry, SessionViewMode};
pub(crate) use crate::setup::{SetupField, SetupState};
#[cfg(test)]
use crate::slash::SLASH_COMMANDS;
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

#[derive(Debug)]
pub struct AppState {
    pub config_path: PathBuf,
    pub workspace_root: PathBuf,
    pub session_log_dir: PathBuf,
    pub session_log_path: PathBuf,
    pub provider_name: String,
    pub model_name: String,
    pub permission_write_mode: String,
    pub memory_enabled: bool,
    pub memory_document_count: usize,
    pub memory_last_status: String,
    pub compaction_status: String,
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
    latest_compaction_record: Option<CompactionRecord>,
    compaction_config: CompactionConfig,
    memory_config: MemoryConfig,
    thinking_block_mode: ThinkingBlockMode,
    selected_tool_timeline_entry: Option<usize>,
    expanded_tool_timeline_entries: BTreeSet<usize>,
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
        let permission_write_mode = root_config.permission.write_mode.as_str().to_owned();
        let initial_compaction_status = effective_compaction_config(
            &root_config.agent.provider,
            &root_config.agent.model,
            &root_config.compaction,
        )
        .threshold_status(0)
        .as_str()
        .to_owned();

        let mut app = Self {
            config_path: config_path.to_path_buf(),
            workspace_root,
            session_log_dir,
            session_log_path: PathBuf::new(),
            provider_name: root_config.agent.provider.clone(),
            model_name: root_config.agent.model.clone(),
            permission_write_mode,
            memory_enabled: root_config.memory.enabled,
            memory_document_count: 0,
            memory_last_status: "pending".to_owned(),
            compaction_status: initial_compaction_status,
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
            latest_compaction_record: None,
            compaction_config: root_config.compaction.clone(),
            memory_config: root_config.memory.clone(),
            thinking_block_mode: ThinkingBlockMode::Collapsed,
            selected_tool_timeline_entry: None,
            expanded_tool_timeline_entries: BTreeSet::new(),
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
            permission_write_mode: ApprovalMode::Ask.as_str().to_owned(),
            memory_enabled: true,
            memory_document_count: 0,
            memory_last_status: "pending".to_owned(),
            compaction_status: CompactionThresholdStatus::NotAvailable.as_str().to_owned(),
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
            latest_compaction_record: None,
            compaction_config: CompactionConfig::default(),
            memory_config: MemoryConfig::default(),
            thinking_block_mode: ThinkingBlockMode::Collapsed,
            selected_tool_timeline_entry: None,
            expanded_tool_timeline_entries: BTreeSet::new(),
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
        self.push_event("approval_default", self.permission_write_mode.clone());
        self.push_event(
            "memory",
            format!(
                "enabled={} docs={} status={}",
                self.memory_enabled, self.memory_document_count, self.memory_last_status
            ),
        );
        self.push_event("compaction", self.compaction_status.clone());
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
        self.latest_compaction_record = None;
        self.run_phase = RunPhase::Idle;
        self.last_phase_marker = None;
        self.streaming_assistant_index = None;
        self.streaming_reasoning_index = None;
        self.approval_metadata_collapsed = false;
        self.approval_selected_file_index = 0;
        self.approval_selected_hunk_index = 0;
        self.approval_diff_mode = ApprovalDiffMode::Full;
        self.selected_tool_timeline_entry = None;
        self.expanded_tool_timeline_entries.clear();
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

        if let Some(pending) = &self.pending_approval {
            match key.code {
                KeyCode::Char('y') | KeyCode::Char('Y') => {
                    return Ok(Some(AppAction::ApprovalDecision {
                        call_id: pending.call.id.clone(),
                        approved: true,
                    }));
                }
                KeyCode::Char('n') | KeyCode::Char('N') => {
                    return Ok(Some(AppAction::ApprovalDecision {
                        call_id: pending.call.id.clone(),
                        approved: false,
                    }));
                }
                _ => {}
            }
        }

        if self.pending_approval.is_some() && !key.modifiers.contains(KeyModifiers::CONTROL) {
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
                KeyCode::Char('m') | KeyCode::Char('M') => {
                    self.approval_metadata_collapsed = !self.approval_metadata_collapsed;
                    self.approval_scroll_back = 0;
                    self.push_event(
                        "approval:view",
                        if self.approval_metadata_collapsed {
                            "metadata collapsed"
                        } else {
                            "metadata expanded"
                        },
                    );
                    return Ok(None);
                }
                KeyCode::Char('[') => {
                    self.jump_approval_hunk(false);
                    return Ok(None);
                }
                KeyCode::Char(']') => {
                    self.jump_approval_hunk(true);
                    return Ok(None);
                }
                KeyCode::Char(',') => {
                    self.switch_approval_file(false);
                    return Ok(None);
                }
                KeyCode::Char('.') => {
                    self.switch_approval_file(true);
                    return Ok(None);
                }
                KeyCode::Char('v') | KeyCode::Char('V') => {
                    self.approval_diff_mode = self.approval_diff_mode.next();
                    self.approval_scroll_back = 0;
                    self.push_event("approval:view", self.approval_diff_mode.label());
                    return Ok(None);
                }
                KeyCode::Up => {
                    self.scroll_active_pane(1);
                    return Ok(None);
                }
                KeyCode::Down => {
                    self.unscroll_active_pane(1);
                    return Ok(None);
                }
                KeyCode::PageUp => {
                    self.scroll_active_pane(8);
                    return Ok(None);
                }
                KeyCode::PageDown => {
                    self.unscroll_active_pane(8);
                    return Ok(None);
                }
                KeyCode::Home => {
                    self.scroll_active_pane(usize::MAX / 2);
                    return Ok(None);
                }
                KeyCode::End => {
                    self.unscroll_active_pane(usize::MAX / 2);
                    return Ok(None);
                }
                KeyCode::Esc => {
                    self.active_pane = PaneFocus::Activity;
                    return Ok(None);
                }
                KeyCode::Char(_)
                | KeyCode::Backspace
                | KeyCode::Left
                | KeyCode::Right
                | KeyCode::Enter
                | KeyCode::Tab
                | KeyCode::BackTab => return Ok(None),
                _ => {}
            }
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
                if self.input.is_empty() {
                    self.scroll_timeline(1);
                } else if self.input_cursor_visual_row() == 0 {
                    self.navigate_input_history(true);
                } else {
                    self.move_input_cursor_vertical(true);
                }
            }
            KeyCode::Down if self.active_pane == PaneFocus::Composer => {
                if self.input.is_empty() {
                    self.unscroll_timeline(1);
                } else if self.input_cursor_visual_row() == self.input_last_visual_row() {
                    self.navigate_input_history(false);
                } else {
                    self.move_input_cursor_vertical(false);
                }
            }
            KeyCode::Home if self.active_pane == PaneFocus::Composer => {
                self.move_input_cursor_home()
            }
            KeyCode::End if self.active_pane == PaneFocus::Composer => self.move_input_cursor_end(),
            KeyCode::Up => self.scroll_timeline(1),
            KeyCode::Down => self.unscroll_timeline(1),
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
            KeyCode::Enter
                if self.active_pane == PaneFocus::Composer
                    && key.modifiers.contains(KeyModifiers::SHIFT) =>
            {
                self.active_pane = PaneFocus::Composer;
                self.insert_input_character('\n');
                self.reset_input_history_navigation();
                self.reset_slash_selector();
            }
            KeyCode::Enter => {
                self.active_pane = PaneFocus::Composer;
                if self.should_accept_slash_selector_on_enter() {
                    self.accept_slash_selector();
                    return Ok(None);
                }
                return self.submit_input();
            }
            KeyCode::Char(character) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
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

    pub fn poll_background_tasks(&mut self) -> bool {
        let mut dirty = false;
        let mut latest_balance = None;
        if let Some(receiver) = &self.balance_refresh_rx {
            while let Ok(snapshot) = receiver.try_recv() {
                latest_balance = Some(snapshot);
            }
        }
        if let Some(snapshot) = latest_balance {
            self.balance_snapshot = snapshot.clone();
            self.balance_refresh_rx = None;
            self.push_event("balance", snapshot.status);
            self.refresh_usage_sidebar_cache();
            dirty = true;
        }
        let mut latest_model_picker = None;
        if let Some(receiver) = &self.model_picker_refresh_rx {
            while let Ok(refresh) = receiver.try_recv() {
                latest_model_picker = Some(refresh);
            }
        }
        if let Some(refresh) = latest_model_picker {
            self.model_picker_refresh_rx = None;
            dirty |= self.apply_model_picker_refresh(refresh);
        }
        dirty
    }

    fn schedule_balance_refresh(&mut self) {
        if self.balance_refresh_rx.is_some() || self.is_setup_mode() {
            return;
        }
        let Some(root_config) = self.config_snapshot.as_ref() else {
            self.balance_snapshot.status = "n/a".to_owned();
            self.refresh_usage_sidebar_cache();
            return;
        };
        let provider_config = load_deepseek_provider_config(root_config)
            .unwrap_or_else(|| default_deepseek_provider_config(&root_config.agent.model));
        let Ok(provider_config) = provider_config.resolved() else {
            self.balance_snapshot.status = "balance unavailable".to_owned();
            self.refresh_usage_sidebar_cache();
            return;
        };
        if resolve_provider_api_key(&provider_config).is_none() {
            self.balance_snapshot = BalanceSnapshot {
                status: "missing auth".to_owned(),
                ..BalanceSnapshot::default()
            };
            self.refresh_usage_sidebar_cache();
            return;
        }

        self.balance_snapshot.status = "loading".to_owned();
        self.refresh_usage_sidebar_cache();
        let (tx, rx) = mpsc::channel();
        self.balance_refresh_rx = Some(rx);
        thread::spawn(move || {
            let snapshot =
                fetch_provider_balance_snapshot(&provider_config).unwrap_or(BalanceSnapshot {
                    status: "balance unavailable".to_owned(),
                    ..BalanceSnapshot::default()
                });
            let _ = tx.send(snapshot);
        });
    }

    pub fn handle_worker_message(&mut self, message: WorkerMessage) -> Result<()> {
        match message {
            WorkerMessage::Event(event) => self.handle(*event)?,
            WorkerMessage::RunStarted { prompt } => {
                self.run_phase = RunPhase::Thinking;
                self.last_notice = Some("thinking".to_owned());
                self.push_phase_marker(format!("thinking|{}", self.model_name));
                self.push_event("run:start", prompt);
            }
            WorkerMessage::RunFinished { result, entries } => {
                self.is_busy = false;
                self.run_phase = RunPhase::Idle;
                self.pending_approval = None;
                self.last_phase_marker = None;
                self.streaming_assistant_index = None;
                self.streaming_reasoning_index = None;
                self.last_notice = Some("agent idle".to_owned());
                self.sync_current_session_state(entries);
                self.refresh_session_history();
                self.recompute_compaction_status(false);
                self.schedule_balance_refresh();
                self.push_event(
                    "run:finish",
                    format!(
                        "tool_calls={} final_text_bytes={}",
                        result.tool_calls,
                        result.final_text.len()
                    ),
                );
            }
            WorkerMessage::RunCancelled {
                session_log_path,
                provider_name,
                model_name,
                entries,
            } => {
                self.is_busy = false;
                self.run_phase = RunPhase::Idle;
                self.pending_approval = None;
                self.last_phase_marker = None;
                self.streaming_assistant_index = None;
                self.streaming_reasoning_index = None;
                self.restore_session_view(
                    session_log_path,
                    provider_name,
                    model_name,
                    entries,
                    "run cancelled; restored",
                );
                self.schedule_balance_refresh();
            }
            WorkerMessage::SessionSwitched {
                session_log_path,
                provider_name,
                model_name,
                entries,
            } => {
                self.is_busy = false;
                self.run_phase = RunPhase::Idle;
                self.pending_approval = None;
                self.last_phase_marker = None;
                self.streaming_assistant_index = None;
                self.streaming_reasoning_index = None;
                self.restore_session_view(
                    session_log_path,
                    provider_name,
                    model_name,
                    entries,
                    "restored from disk",
                );
                self.schedule_balance_refresh();
            }
            WorkerMessage::SessionCompacted {
                session_log_path,
                provider_name,
                model_name,
                record,
                trigger,
                entries,
            } => {
                self.is_busy = false;
                self.run_phase = RunPhase::Idle;
                self.pending_approval = None;
                self.last_phase_marker = None;
                self.streaming_assistant_index = None;
                self.streaming_reasoning_index = None;
                match trigger {
                    CompactionTrigger::Manual => {
                        self.restore_session_view(
                            session_log_path,
                            provider_name,
                            model_name,
                            entries,
                            "Session compacted.",
                        );
                    }
                    CompactionTrigger::AutomaticHardThreshold => {
                        self.session_log_path = session_log_path;
                        self.provider_name = provider_name;
                        self.model_name = model_name;
                        self.sync_current_session_state(entries);
                        self.latest_compaction_record = Some(record.clone());
                        self.recompute_compaction_status(false);
                        self.last_notice = Some("auto compacted".to_owned());
                        self.refresh_session_history();
                        self.push_timeline(
                            TimelineRole::Notice,
                            format!(
                                "Auto-compacted: summary={} tail={}.",
                                record.compacted_message_count, record.retained_tail_message_count
                            ),
                        );
                        self.push_event(
                            "compaction",
                            format!(
                                "auto hard compacted={} tail={}",
                                record.compacted_message_count, record.retained_tail_message_count
                            ),
                        );
                        self.schedule_balance_refresh();
                    }
                }
            }
            WorkerMessage::Notice(message) => {
                self.last_notice = Some(message.clone());
                self.push_timeline(TimelineRole::Notice, message.clone());
                self.push_event("worker", message);
            }
            WorkerMessage::RunFailed(error) => {
                self.is_busy = false;
                self.run_phase = RunPhase::Idle;
                self.pending_approval = None;
                self.last_phase_marker = None;
                self.streaming_assistant_index = None;
                self.streaming_reasoning_index = None;
                self.refresh_usage_sidebar_cache();
                let summary = summarize_error(&error);
                self.last_notice = Some(summary.clone());
                self.push_timeline(TimelineRole::Notice, format!("Run failed: {summary}"));
                self.push_event("run:error", error);
            }
        }
        Ok(())
    }

    pub fn shutdown_command() -> WorkerCommand {
        WorkerCommand::Shutdown
    }

    pub fn into_worker_command(&self, action: AppAction) -> WorkerCommand {
        match action {
            AppAction::SubmitPrompt(prompt) => WorkerCommand::SubmitPrompt {
                prompt,
                reasoning_effort: self.reasoning_effort.clone(),
            },
            AppAction::ApprovalDecision { call_id, approved } => {
                WorkerCommand::ApprovalDecision { call_id, approved }
            }
            AppAction::CancelRun => WorkerCommand::CancelRun,
            AppAction::CompactNow => WorkerCommand::CompactNow,
            AppAction::SwitchSession { session_log_path } => {
                WorkerCommand::SwitchSession { session_log_path }
            }
            AppAction::SetupCompleted { .. }
            | AppAction::ConfigSaved { .. }
            | AppAction::RuntimeConfigUpdated { .. } => unreachable!(
                "setup/config/runtime updates are handled before worker command conversion"
            ),
        }
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

    #[cfg_attr(not(test), allow(dead_code))]
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

    #[cfg_attr(not(test), allow(dead_code))]
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

    #[allow(dead_code)]
    pub(crate) fn permission_card_lines(&self) -> Vec<String> {
        vec![
            format!("mode: {}", self.permission_write_mode),
            "Shift-Tab toggle".to_owned(),
            if self.is_busy {
                "busy: locked during run".to_owned()
            } else {
                "scope: current session".to_owned()
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

    #[cfg_attr(not(test), allow(dead_code))]
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

    #[cfg_attr(not(test), allow(dead_code))]
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
            self.permission_write_mode,
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
        next_config.permission.write_mode = cycle_approval_mode(next_config.permission.write_mode);
        self.apply_runtime_config_snapshot(&next_config);
        self.last_notice = Some(format!(
            "write mode = {}",
            next_config.permission.write_mode.as_str()
        ));
        self.push_event("approval_default", self.permission_write_mode.clone());
        self.push_timeline(
            TimelineRole::Notice,
            format!("write permission -> {}", self.permission_write_mode),
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

impl EventHandler for AppState {
    fn handle(&mut self, event: RunEvent) -> Result<()> {
        match event {
            RunEvent::TextDelta(delta) => {
                self.run_phase = RunPhase::Streaming;
                self.push_phase_marker("streaming".to_owned());
                self.append_assistant_delta(&delta);
                self.push_event("text", delta);
            }
            RunEvent::ReasoningDelta(delta) => {
                self.run_phase = RunPhase::Thinking;
                self.push_phase_marker(format!("thinking|{}", self.model_name));
                self.append_reasoning_delta(&delta);
                self.push_event("reasoning", delta);
            }
            RunEvent::ToolCallStarted(call) => {
                self.run_phase = RunPhase::Tool(call.name.clone());
                self.streaming_reasoning_index = None;
                self.push_phase_marker(format!("tool|{}", call.name));
                self.push_event("tool:start", format!("{} {}", call.name, call.id));
            }
            RunEvent::ToolCallArgsDelta { id, delta } => {
                if !matches!(self.run_phase, RunPhase::Tool(_)) {
                    self.run_phase = RunPhase::Tool("tool".to_owned());
                }
                self.push_event("tool:args", format!("{id} {delta}"));
            }
            RunEvent::ToolCallCompleted(call) => {
                self.run_phase = RunPhase::Tool(call.name.clone());
                self.streaming_reasoning_index = None;
                self.push_phase_marker(format!("tool|{}", call.name));
                self.push_event("tool:complete", format!("{} {}", call.name, call.id));
            }
            RunEvent::ToolApprovalRequested {
                call,
                spec,
                preview,
            } => {
                self.run_phase = RunPhase::Tool(call.name.clone());
                self.pending_approval = Some(PendingApproval {
                    call: call.clone(),
                    spec,
                    preview,
                });
                self.active_pane = PaneFocus::Activity;
                self.approval_scroll_back = 0;
                self.approval_metadata_collapsed = false;
                self.approval_selected_file_index = 0;
                self.approval_selected_hunk_index = 0;
                self.last_notice = Some(format!("approve {}", call.name));
                self.push_event("approval:request", format!("{} {}", call.name, call.id));
                self.push_timeline(
                    TimelineRole::Notice,
                    format!("Approve {}? Y allow, N deny.", call.name),
                );
            }
            RunEvent::ToolApprovalResolved {
                call_id,
                approved,
                reason,
            } => {
                self.run_phase = RunPhase::Thinking;
                self.pending_approval = None;
                self.active_pane = PaneFocus::Composer;
                self.push_event(
                    "approval:resolved",
                    format!(
                        "{} {}",
                        call_id,
                        if approved { "approved" } else { "denied" }
                    ),
                );
                if approved {
                    self.push_timeline(TimelineRole::Notice, format!("Approved {call_id}."));
                } else {
                    self.push_timeline(
                        TimelineRole::Notice,
                        format!(
                            "Denied {call_id}: {}",
                            reason.unwrap_or_else(|| "denied".to_owned())
                        ),
                    );
                }
            }
            RunEvent::ToolResult(result) => {
                self.run_phase = RunPhase::Tool(result.tool_name.clone());
                self.streaming_reasoning_index = None;
                self.push_phase_marker(format!("tool|{}", result.tool_name));
                let status = if result.is_error { "error" } else { "ok" };
                self.push_timeline(TimelineRole::Tool, format_tool_result_block(&result));
                self.push_event("tool:result", format!("{} {}", result.tool_name, status));
            }
            RunEvent::Usage(usage) => {
                self.stats.apply_usage(&usage);
                self.recompute_compaction_status(true);
                self.refresh_usage_sidebar_cache();
                self.push_event(
                    "usage",
                    format!(
                        "prompt={} completion={} cache_hit={} cache_miss={}",
                        usage.prompt_tokens,
                        usage.completion_tokens,
                        usage.cache_hit_tokens,
                        usage.cache_miss_tokens
                    ),
                );
            }
            RunEvent::Control(control) => {
                self.push_event("control", format!("{control:?}"));
            }
            RunEvent::ContinuationState(state) => {
                self.push_event("continuation", state.state_kind);
            }
            RunEvent::AssistantMessage(message) => {
                self.run_phase = RunPhase::Streaming;
                self.push_phase_marker("streaming".to_owned());
                self.streaming_assistant_index = None;
                self.streaming_reasoning_index = None;
                if let Some(content) = message.content
                    && !content.is_empty()
                {
                    let last_is_same = self
                        .timeline
                        .last()
                        .map(|entry| entry.role == TimelineRole::Assistant && entry.text == content)
                        .unwrap_or(false);
                    if !last_is_same {
                        self.push_timeline(TimelineRole::Assistant, content);
                    }
                }
            }
            RunEvent::Notice(note) => {
                self.last_notice = Some(note.clone());
                self.push_timeline(TimelineRole::Notice, note.clone());
                self.push_event("notice", note);
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use anyhow::Result;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use serde_json::json;
    use tempfile::tempdir;
    use termquill_kernel::{
        AgentConfig, ApprovalMode, CompactionConfig, CompactionRecord, ControlEntry, EventHandler,
        JsonlSessionStore, MemoryConfig, ModelMessage, PermissionConfig, ReasoningEffort,
        RootConfig, RunEvent, SessionConfig, SessionLogEntry, ToolCall, ToolPreview, ToolSpec,
        UsageStats, WorkspaceConfig,
    };

    use crate::runner::{CompactionTrigger, WorkerCommand};

    use super::{
        AppAction, AppState, ConfigField, ConfigSection, ModalState, ModelPickerRefresh,
        ModelPickerTarget, PaneFocus, RunPhase, SetupField, TimelineRole, WorkerMessage,
    };

    fn test_config() -> RootConfig {
        RootConfig {
            workspace: WorkspaceConfig {
                root: ".".to_owned(),
            },
            session: SessionConfig {
                log_dir: ".termquill/sessions".to_owned(),
            },
            agent: AgentConfig {
                provider: "deepseek".to_owned(),
                model: "deepseek-v4-flash".to_owned(),
                max_turns: 8,
                tool_timeout_secs: 30,
            },
            permission: PermissionConfig::default(),
            memory: MemoryConfig { enabled: true },
            compaction: CompactionConfig::default(),
            providers: std::collections::BTreeMap::new(),
            mcp_servers: Vec::new(),
        }
    }

    fn restored_entries(provider_name: &str, model_name: &str) -> Vec<SessionLogEntry> {
        vec![
            SessionLogEntry::Control(ControlEntry::SessionIdentity {
                provider_name: provider_name.to_owned(),
                model_name: model_name.to_owned(),
            }),
            SessionLogEntry::User(ModelMessage::user("restored user prompt")),
            SessionLogEntry::ToolResult(ModelMessage::tool("call-1", "restored tool output")),
            SessionLogEntry::Assistant(ModelMessage::assistant(
                Some("restored assistant answer".to_owned()),
                Vec::new(),
            )),
        ]
    }

    fn select_root_slash_command(app: &mut AppState, command: &str) -> Result<()> {
        let index = app
            .slash_selector_rows()
            .iter()
            .position(|(label, _)| label == command)
            .ok_or_else(|| anyhow::anyhow!("slash command {command} not found"))?;
        for _ in 0..index {
            let _ = app.handle_key_event(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE))?;
        }
        Ok(())
    }

    fn write_session_log(path: &Path, entries: &[SessionLogEntry]) -> Result<()> {
        let store = JsonlSessionStore::new(path)?;
        for entry in entries {
            store.append(entry)?;
        }
        Ok(())
    }

    fn sample_approval_preview() -> ToolPreview {
        ToolPreview {
            title: "Update note.txt".to_owned(),
            summary: "Preview summary".to_owned(),
            body: "--- current/note.txt\n+++ proposed/note.txt\n@@ -1,2 +1,2 @@\n alpha\n-beta\n+gamma".to_owned(),
            changed_files: vec!["note.txt".to_owned()],
            file_diffs: vec![termquill_kernel::ToolPreviewFile {
                path: "note.txt".to_owned(),
                diff: "--- current/note.txt\n+++ proposed/note.txt\n@@ -1,2 +1,2 @@\n alpha\n-beta\n+gamma".to_owned(),
            }],
        }
    }

    fn multi_file_approval_preview() -> ToolPreview {
        ToolPreview {
            title: "Update multiple files".to_owned(),
            summary: "Multi-file preview".to_owned(),
            body: String::new(),
            changed_files: vec!["note-a.txt".to_owned(), "note-b.txt".to_owned()],
            file_diffs: vec![
                termquill_kernel::ToolPreviewFile {
                    path: "note-a.txt".to_owned(),
                    diff: "--- current/note-a.txt\n+++ proposed/note-a.txt\n@@ -1,2 +1,2 @@\n alpha\n-beta\n+gamma\n@@ -5,2 +5,2 @@\n delta\n-epsilon\n+zeta".to_owned(),
                },
                termquill_kernel::ToolPreviewFile {
                    path: "note-b.txt".to_owned(),
                    diff: "--- current/note-b.txt\n+++ proposed/note-b.txt\n@@ -1,1 +1,1 @@\n-old\n+new".to_owned(),
                },
            ],
        }
    }

    fn inject_write_file_approval(app: &mut AppState, preview: ToolPreview) -> Result<()> {
        app.handle(RunEvent::ToolApprovalRequested {
            call: ToolCall {
                id: "call-1".to_owned(),
                name: "write_file".to_owned(),
                args_json: r#"{"path":"note.txt","content":"hello"}"#.to_owned(),
            },
            spec: ToolSpec {
                name: "write_file".to_owned(),
                description: "Write a file".to_owned(),
                input_schema: json!({"type":"object"}),
                read_only: false,
            },
            preview: Some(preview),
        })
    }

    #[test]
    fn normal_input_creates_user_and_running_state() -> Result<()> {
        let mut app = AppState::from_root_config(Path::new("termquill.toml"), &test_config());
        app.input = "hello".to_owned();
        let action = app.submit_input()?;
        assert!(
            app.timeline
                .iter()
                .any(|entry| { entry.role == TimelineRole::User && entry.text == "hello" })
        );
        assert!(matches!(action, Some(AppAction::SubmitPrompt(prompt)) if prompt == "hello"));
        assert!(app.is_busy);
        assert_eq!(app.active_pane, PaneFocus::Composer);
        assert_eq!(app.composer_height(), 5);
        assert!(
            !app.timeline
                .iter()
                .any(|entry| entry.role == TimelineRole::Phase)
        );
        assert!(app.events.iter().any(|event| {
            event.label == "phase" && event.detail == "thinking|deepseek-v4-flash"
        }));
        assert_eq!(app.run_phase(), RunPhase::Thinking);
        assert_eq!(app.last_notice(), Some("thinking"));
        Ok(())
    }

    #[test]
    fn short_transcript_stays_in_live_panel_instead_of_terminal_scrollback() {
        let mut app = AppState::from_root_config(Path::new("termquill.toml"), &test_config());
        app.set_terminal_size(120, 32);
        app.push_timeline(TimelineRole::User, "hello");
        app.push_timeline(TimelineRole::Assistant, "latest answer");

        assert_eq!(app.scrollback_line_count(), 0);
        let live = app
            .transcript_lines(app.timeline_viewport_rows())
            .into_iter()
            .flat_map(|line| line.spans.into_iter().map(|span| span.content.into_owned()))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(live.contains("hello"));
        assert!(live.contains("latest answer"));
    }

    #[test]
    fn cjk_input_cursor_visual_position_uses_display_width() {
        let mut app = AppState::from_root_config(Path::new("termquill.toml"), &test_config());
        app.set_terminal_size(40, 12);
        app.set_input_and_cursor("你好".to_owned());

        assert_eq!(app.input_cursor_visual_position(), (4, 0));
    }

    #[test]
    fn reasoning_delta_creates_collapsed_thinking_block() -> Result<()> {
        let mut app = AppState::from_root_config(Path::new("termquill.toml"), &test_config());

        app.handle(RunEvent::ReasoningDelta("planning step 1".to_owned()))?;
        app.handle(RunEvent::ReasoningDelta("\nplanning step 2".to_owned()))?;

        assert!(
            !app.timeline
                .iter()
                .any(|entry| entry.role == TimelineRole::Phase)
        );
        assert!(app.events.iter().any(|event| {
            event.label == "phase" && event.detail == "thinking|deepseek-v4-flash"
        }));
        assert!(app.timeline.iter().any(|entry| {
            entry.role == TimelineRole::Thinking && entry.text == "planning step 1\nplanning step 2"
        }));
        let collapsed = app.transcript_lines(20);
        assert!(collapsed.iter().any(|line| {
            line.spans
                .iter()
                .any(|span| span.content.as_ref().contains("Ctrl-T expand"))
        }));
        assert!(!collapsed.iter().any(|line| {
            line.spans
                .iter()
                .any(|span| span.content.as_ref().contains("planning step 2"))
        }));
        Ok(())
    }

    #[test]
    fn ctrl_t_toggles_thinking_block_expansion() -> Result<()> {
        let mut app = AppState::from_root_config(Path::new("termquill.toml"), &test_config());

        app.handle(RunEvent::ReasoningDelta("planning step 1".to_owned()))?;
        app.handle(RunEvent::ReasoningDelta("\nplanning step 2".to_owned()))?;

        let collapsed = app.transcript_lines(20);
        assert!(collapsed.iter().any(|line| {
            line.spans
                .iter()
                .any(|span| span.content.as_ref().contains("Ctrl-T expand"))
        }));
        assert!(!collapsed.iter().any(|line| {
            line.spans
                .iter()
                .any(|span| span.content.as_ref().contains("planning step 2"))
        }));

        app.handle_key_event(KeyEvent::new(KeyCode::Char('t'), KeyModifiers::CONTROL))?;

        let expanded = app.transcript_lines(20);
        assert!(expanded.iter().any(|line| {
            line.spans
                .iter()
                .any(|span| span.content.as_ref().contains("Ctrl-T collapse"))
        }));
        assert!(expanded.iter().any(|line| {
            line.spans
                .iter()
                .any(|span| span.content.as_ref().contains("planning step 2"))
        }));
        assert_eq!(app.last_notice(), Some("thinking expanded"));

        app.handle_key_event(KeyEvent::new(KeyCode::Char('t'), KeyModifiers::CONTROL))?;

        let recollapsed = app.transcript_lines(20);
        assert!(recollapsed.iter().any(|line| {
            line.spans
                .iter()
                .any(|span| span.content.as_ref().contains("Ctrl-T expand"))
        }));
        assert!(!recollapsed.iter().any(|line| {
            line.spans
                .iter()
                .any(|span| span.content.as_ref().contains("planning step 2"))
        }));
        assert_eq!(app.last_notice(), Some("thinking collapsed"));
        Ok(())
    }

    #[test]
    fn latest_session_can_be_restored_on_launch() -> Result<()> {
        let temp = tempdir()?;
        let config = RootConfig {
            workspace: WorkspaceConfig {
                root: temp.path().display().to_string(),
            },
            ..test_config()
        };
        let session_dir = temp.path().join(".termquill/sessions");
        let restored_path = session_dir.join("session-restored.jsonl");
        write_session_log(
            &restored_path,
            &restored_entries("restored-provider", "restored-model"),
        )?;

        let mut app =
            AppState::from_root_config(temp.path().join("termquill.toml").as_path(), &config);

        assert!(app.restore_latest_session_from_disk(&config));
        assert_eq!(app.session_log_path, restored_path);
        assert_eq!(app.provider_name, "restored-provider");
        assert_eq!(app.model_name, "restored-model");
        assert!(
            app.timeline
                .iter()
                .any(|entry| entry.text == "restored assistant answer")
        );
        assert_eq!(app.last_notice(), Some("restored latest session"));
        Ok(())
    }

    #[test]
    fn approval_request_stores_preview() -> Result<()> {
        let mut app = AppState::from_root_config(Path::new("termquill.toml"), &test_config());
        inject_write_file_approval(&mut app, sample_approval_preview())?;

        let pending = app.pending_approval.expect("expected pending approval");
        let preview = pending.preview.expect("expected preview");
        assert_eq!(preview.changed_files, vec!["note.txt".to_owned()]);
        assert!(preview.body.contains("+++ proposed/note.txt"));
        Ok(())
    }

    #[test]
    fn tool_result_is_rendered_as_multiline_json_block() -> Result<()> {
        let mut app = AppState::from_root_config(Path::new("termquill.toml"), &test_config());

        app.handle(RunEvent::ToolResult(termquill_kernel::ToolResult {
            call_id: "call-1".to_owned(),
            tool_name: "ls".to_owned(),
            content: "[\".git\",\"Cargo.toml\"]".to_owned(),
            is_error: false,
            metadata: termquill_kernel::ToolResultMeta::default(),
        }))?;

        let entry = app.timeline.last().expect("expected tool timeline entry");
        let rendered: serde_json::Value = serde_json::from_str(&entry.text)?;
        assert_eq!(entry.role, TimelineRole::Tool);
        assert_eq!(rendered["tool_name"], "ls");
        assert_eq!(rendered["preview_kind"], "json");
        assert_eq!(rendered["status"], "ok");
        assert!(rendered["preview_lines"].as_array().is_some_and(|lines| {
            lines
                .iter()
                .any(|line| line.as_str().is_some_and(|text| text.contains(".git")))
        }));
        Ok(())
    }

    #[test]
    fn compact_command_dispatches_worker_action_when_idle() -> Result<()> {
        let mut app = AppState::from_root_config(Path::new("termquill.toml"), &test_config());
        app.input = "/compact".to_owned();

        let action = app.submit_input()?;

        assert!(matches!(action, Some(AppAction::CompactNow)));
        Ok(())
    }

    #[test]
    fn compact_command_prefix_is_resolved_to_exact_command() -> Result<()> {
        let mut app = AppState::from_root_config(Path::new("termquill.toml"), &test_config());
        app.input = "/comp".to_owned();

        let action = app.submit_input()?;

        assert!(matches!(action, Some(AppAction::CompactNow)));
        Ok(())
    }

    #[test]
    fn effort_command_updates_runtime_effort_and_worker_submit_uses_it() -> Result<()> {
        let mut app = AppState::from_root_config(Path::new("termquill.toml"), &test_config());
        app.input = "/effort high".to_owned();

        assert!(app.submit_input()?.is_none());
        assert_eq!(app.reasoning_effort.as_str(), "high");

        let command = app.into_worker_command(AppAction::SubmitPrompt("hello".to_owned()));
        assert!(matches!(
            command,
            WorkerCommand::SubmitPrompt {
                prompt,
                reasoning_effort: ReasoningEffort::High,
            } if prompt == "hello"
        ));
        Ok(())
    }

    #[test]
    fn model_command_switches_runtime_model_and_starts_fresh_session() -> Result<()> {
        let mut app = AppState::from_root_config(Path::new("termquill.toml"), &test_config());
        let previous_session_id = app.session_id.clone();
        app.push_timeline(TimelineRole::Assistant, "old context");
        app.input = "/model pro".to_owned();

        let action = app.submit_input()?;

        assert!(matches!(
            action,
            Some(AppAction::RuntimeConfigUpdated { .. })
        ));
        assert_eq!(app.model_name, "deepseek-v4-pro");
        assert_ne!(app.session_id, previous_session_id);
        assert!(
            app.timeline
                .iter()
                .any(|entry| entry.text.contains("model -> deepseek-v4-pro"))
        );
        Ok(())
    }

    #[test]
    fn slash_command_hints_include_prefix_matches() {
        let mut app = AppState::from_root_config(Path::new("termquill.toml"), &test_config());
        app.input = "/res".to_owned();
        let hints = app.slash_command_hints();
        assert!(hints.iter().any(|hint| hint.contains("/resume")));
    }

    #[test]
    fn slash_command_hints_handles_leading_space() {
        let mut app = AppState::from_root_config(Path::new("termquill.toml"), &test_config());
        app.input = " /compact".to_owned();
        let hints = app.slash_command_hints();
        assert!(hints.iter().any(|hint| hint.contains("/compact")));
    }

    #[test]
    fn slash_command_input_starts_in_activity_mode() -> Result<()> {
        let mut app = AppState::from_root_config(Path::new("termquill.toml"), &test_config());
        app.active_pane = PaneFocus::Activity;
        app.input.clear();

        let _ = app.handle_key_event(KeyEvent::new(KeyCode::Char('/'), KeyModifiers::NONE))?;
        let _ = app.handle_key_event(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::NONE))?;

        assert_eq!(app.active_pane, PaneFocus::Composer);
        assert_eq!(app.input, "/c".to_owned());
        assert!(
            app.slash_command_hints()
                .iter()
                .any(|hint| hint.contains("/compact"))
        );
        Ok(())
    }

    #[test]
    fn ideographic_comma_starts_command_palette() -> Result<()> {
        let mut app = AppState::from_root_config(Path::new("termquill.toml"), &test_config());
        app.input.clear();

        let _ = app.handle_key_event(KeyEvent::new(KeyCode::Char('、'), KeyModifiers::NONE))?;
        let _ = app.handle_key_event(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::NONE))?;

        assert_eq!(app.input, "/c");
        assert!(
            app.slash_command_hints()
                .iter()
                .any(|hint| hint.contains("/compact"))
        );
        Ok(())
    }

    #[test]
    fn shift_enter_inserts_newline_without_submitting() -> Result<()> {
        let mut app = AppState::from_root_config(Path::new("termquill.toml"), &test_config());
        app.input = "hello".to_owned();
        app.input_cursor = app.input.chars().count();
        let timeline_len = app.timeline.len();

        let action = app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::SHIFT))?;

        assert!(action.is_none());
        assert_eq!(app.input, "hello\n");
        assert_eq!(app.timeline.len(), timeline_len);
        assert_eq!(app.composer_input_rows(), 2);
        Ok(())
    }

    #[test]
    fn composer_up_down_scroll_transcript_when_input_is_empty() -> Result<()> {
        let mut app = AppState::from_root_config(Path::new("termquill.toml"), &test_config());
        app.set_terminal_size(80, 12);
        for index in 0..8 {
            app.push_timeline(TimelineRole::Assistant, format!("message {index}"));
        }
        app.input.clear();
        app.input_cursor = 0;

        app.handle_key_event(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE))?;
        assert!(app.timeline_scroll_back > 0);

        app.handle_key_event(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE))?;
        assert_eq!(app.timeline_scroll_back, 0);
        Ok(())
    }

    #[test]
    fn ctrl_p_and_ctrl_n_navigate_prompt_history() -> Result<()> {
        let mut app = AppState::from_root_config(Path::new("termquill.toml"), &test_config());
        app.input_history = vec!["first".to_owned(), "second".to_owned()];
        app.input.clear();
        app.input_cursor = 0;

        app.handle_key_event(KeyEvent::new(KeyCode::Char('p'), KeyModifiers::CONTROL))?;
        assert_eq!(app.input, "second");

        app.handle_key_event(KeyEvent::new(KeyCode::Char('n'), KeyModifiers::CONTROL))?;
        assert!(app.input.is_empty());
        Ok(())
    }

    #[test]
    fn ctrl_u_and_ctrl_d_scroll_transcript_history() -> Result<()> {
        let mut app = AppState::from_root_config(Path::new("termquill.toml"), &test_config());
        app.set_terminal_size(80, 12);
        for index in 0..8 {
            app.push_timeline(TimelineRole::Assistant, format!("message {index}"));
        }

        let bottom = app.transcript_lines(4);
        app.handle_key_event(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL))?;
        let scrolled = app.transcript_lines(4);

        assert!(app.timeline_scroll_back > 0);
        assert_ne!(bottom, scrolled);

        app.handle_key_event(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL))?;
        assert_eq!(app.timeline_scroll_back, 0);
        Ok(())
    }

    #[test]
    fn ctrl_home_and_ctrl_end_jump_transcript_between_oldest_and_newest() -> Result<()> {
        let mut app = AppState::from_root_config(Path::new("termquill.toml"), &test_config());
        app.set_terminal_size(80, 12);
        for index in 0..8 {
            app.push_timeline(TimelineRole::Assistant, format!("message {index}"));
        }

        app.handle_key_event(KeyEvent::new(KeyCode::Home, KeyModifiers::CONTROL))?;
        assert_eq!(app.timeline_scroll_back, app.max_timeline_scroll_back());

        app.handle_key_event(KeyEvent::new(KeyCode::End, KeyModifiers::CONTROL))?;
        assert_eq!(app.timeline_scroll_back, 0);
        Ok(())
    }

    #[test]
    fn scrolling_transcript_to_top_reaches_earliest_message() -> Result<()> {
        let mut app = AppState::from_root_config(Path::new("termquill.toml"), &test_config());
        app.set_terminal_size(80, 12);
        for index in 0..20 {
            app.push_timeline(TimelineRole::Assistant, format!("message {index}"));
        }

        app.handle_key_event(KeyEvent::new(KeyCode::Home, KeyModifiers::CONTROL))?;
        let top = app.transcript_lines(app.timeline_viewport_rows());

        assert!(top.iter().any(|line| {
            line.spans
                .iter()
                .any(|span| span.content.as_ref().contains("message 0"))
        }));
        assert_eq!(app.timeline_scroll_back, app.max_timeline_scroll_back());
        Ok(())
    }

    #[test]
    fn transcript_live_tail_ignores_trailing_gap_rows() {
        let mut app = AppState::from_root_config(Path::new("termquill.toml"), &test_config());
        app.set_terminal_size(80, 12);
        app.push_timeline(TimelineRole::User, "hello");

        let tail = app.transcript_lines(1);
        let rendered = tail
            .iter()
            .flat_map(|line| line.spans.iter())
            .map(|span| span.content.as_ref())
            .collect::<String>();

        assert!(rendered.contains("hello"));
    }

    #[test]
    fn mouse_scroll_moves_transcript() {
        let mut app = AppState::from_root_config(Path::new("termquill.toml"), &test_config());
        app.set_terminal_size(80, 12);
        for index in 0..8 {
            app.push_timeline(TimelineRole::Assistant, format!("message {index}"));
        }

        app.handle_mouse_scroll(true);
        assert!(app.timeline_scroll_back > 0);

        app.handle_mouse_scroll(false);
        assert_eq!(app.timeline_scroll_back, 0);
    }

    #[test]
    fn slash_selector_shows_all_commands_for_root_slash() {
        let mut app = AppState::from_root_config(Path::new("termquill.toml"), &test_config());
        app.input = "/".to_owned();

        let rows = app.slash_selector_rows();

        assert_eq!(rows.len(), super::SLASH_COMMANDS.len());
        assert_eq!(app.slash_selector_selected_index(), Some(0));
    }

    #[test]
    fn slash_selector_does_not_register_tool_commands() {
        let mut app = AppState::from_root_config(Path::new("termquill.toml"), &test_config());
        app.input = "/".to_owned();

        let rows = app.slash_selector_rows();
        assert!(!rows.iter().any(|(label, _)| label == "/tool"));
        assert!(!rows.iter().any(|(label, _)| label == "/tools"));

        app.input = "/tools".to_owned();
        assert!(app.slash_selector_rows().is_empty());
        assert_eq!(app.slash_selector_empty_message(), Some("no slash match"));

        app.input = "/tool".to_owned();
        assert!(app.slash_selector_rows().is_empty());
        assert_eq!(app.slash_selector_empty_message(), Some("no slash match"));

        assert!(app.resolve_slash_command("/tool latest").is_none());
        assert!(app.resolve_slash_command("/tools full").is_none());
    }

    #[test]
    fn slash_selector_navigation_and_tab_completion_work() -> Result<()> {
        let mut app = AppState::from_root_config(Path::new("termquill.toml"), &test_config());
        app.input = "/".to_owned();

        let _ = app.handle_key_event(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE))?;
        let _ = app.handle_key_event(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE))?;

        assert_eq!(app.input, "/config".to_owned());
        assert_eq!(app.slash_selector_selected_index(), Some(0));
        Ok(())
    }

    #[test]
    fn slash_selector_offers_model_candidates_and_completes_argument() -> Result<()> {
        let mut app = AppState::from_root_config(Path::new("termquill.toml"), &test_config());
        app.input = "/model p".to_owned();

        let rows = app.slash_selector_rows();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].0, "pro");

        let _ = app.handle_key_event(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE))?;
        assert_eq!(app.input, "/model deepseek-v4-pro");
        Ok(())
    }

    #[test]
    fn slash_selector_executes_selected_model_candidate() -> Result<()> {
        let mut app = AppState::from_root_config(Path::new("termquill.toml"), &test_config());
        let previous_session_id = app.session_id.clone();
        app.input = "/model p".to_owned();

        let action = app.submit_input()?;

        assert!(matches!(
            action,
            Some(AppAction::RuntimeConfigUpdated { .. })
        ));
        assert_eq!(app.model_name, "deepseek-v4-pro");
        assert_ne!(app.session_id, previous_session_id);
        Ok(())
    }

    #[test]
    fn enter_on_root_slash_model_completes_into_second_stage_selector() -> Result<()> {
        let mut app = AppState::from_root_config(Path::new("termquill.toml"), &test_config());
        app.input = "/".to_owned();

        select_root_slash_command(&mut app, "/model")?;
        let _ = app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))?;

        assert_eq!(app.input, "/model ");
        let rows = app.slash_selector_rows();
        assert!(rows.iter().any(|(label, _)| label == "flash"));
        assert!(rows.iter().any(|(label, _)| label == "pro"));
        Ok(())
    }

    #[test]
    fn enter_on_root_slash_effort_completes_into_second_stage_selector() -> Result<()> {
        let mut app = AppState::from_root_config(Path::new("termquill.toml"), &test_config());
        app.input = "/".to_owned();

        select_root_slash_command(&mut app, "/effort")?;
        let _ = app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))?;

        assert_eq!(app.input, "/effort ");
        let rows = app.slash_selector_rows();
        assert!(rows.iter().any(|(label, _)| label == "low"));
        assert!(rows.iter().any(|(label, _)| label == "max"));
        Ok(())
    }

    #[test]
    fn model_command_is_noop_when_selected_model_is_already_active() -> Result<()> {
        let mut app = AppState::from_root_config(Path::new("termquill.toml"), &test_config());
        let previous_session_id = app.session_id.clone();
        app.input = "/model".to_owned();

        let action = app.submit_input()?;

        assert!(action.is_none());
        assert_eq!(app.model_name, "deepseek-v4-flash");
        assert_eq!(app.session_id, previous_session_id);
        assert_eq!(
            app.last_notice(),
            Some("model already active = deepseek-v4-flash")
        );
        Ok(())
    }

    #[test]
    fn slash_selector_orders_effort_candidates_by_current_value() {
        let mut app = AppState::from_root_config(Path::new("termquill.toml"), &test_config());
        app.reasoning_effort = ReasoningEffort::High;
        app.input = "/effort".to_owned();

        let rows = app.slash_selector_rows();

        assert_eq!(rows.first().map(|row| row.0.as_str()), Some("high"));
    }

    #[test]
    fn slash_selector_executes_selected_effort_candidate() -> Result<()> {
        let mut app = AppState::from_root_config(Path::new("termquill.toml"), &test_config());
        app.input = "/effort h".to_owned();

        assert!(app.submit_input()?.is_none());
        assert_eq!(app.reasoning_effort.as_str(), "high");
        Ok(())
    }

    #[test]
    fn tool_card_shortcuts_focus_and_toggle_one_card() -> Result<()> {
        let mut app = AppState::from_root_config(Path::new("termquill.toml"), &test_config());
        app.push_timeline(
            TimelineRole::Tool,
            r##"{
  "tool_name": "ls",
  "status": "ok",
  "preview_kind": "json",
  "summary": "first 1/1 lines · 8 B",
  "preview_lines": ["[\".git\"]"],
  "preview_value": [".git"],
  "hidden_lines": 0
}"##,
        );
        app.push_timeline(
            TimelineRole::Tool,
            r##"{
  "tool_name": "ls",
  "status": "ok",
  "preview_kind": "json",
  "summary": "first 1/1 lines · 11 B",
  "preview_lines": ["[\"src/lib.rs\"]"],
  "preview_value": ["src/lib.rs"],
  "hidden_lines": 0
}"##,
        );
        let tool_indices = app
            .tool_timeline_entry_indices()
            .expect("test app should contain two tool timeline entries");
        let first_tool = tool_indices[0];
        let second_tool = tool_indices[1];

        assert_eq!(app.selected_tool_timeline_entry, Some(second_tool));

        app.selected_tool_timeline_entry = None;
        let _ = app.handle_key_event(KeyEvent::new(KeyCode::Char('g'), KeyModifiers::CONTROL))?;
        assert_eq!(app.selected_tool_timeline_entry, Some(second_tool));

        let _ = app.handle_key_event(KeyEvent::new(KeyCode::Char('k'), KeyModifiers::ALT))?;
        assert_eq!(app.selected_tool_timeline_entry, Some(first_tool));

        let _ = app.handle_key_event(KeyEvent::new(KeyCode::Char('j'), KeyModifiers::ALT))?;
        assert_eq!(app.selected_tool_timeline_entry, Some(second_tool));

        let _ = app.handle_key_event(KeyEvent::new(KeyCode::Char('k'), KeyModifiers::ALT))?;
        assert_eq!(app.selected_tool_timeline_entry, Some(first_tool));

        let _ = app.handle_key_event(KeyEvent::new(KeyCode::Char('o'), KeyModifiers::CONTROL))?;
        assert!(app.expanded_tool_timeline_entries.contains(&first_tool));
        assert!(!app.expanded_tool_timeline_entries.contains(&second_tool));

        let lines = app.transcript_lines(40);
        assert!(lines.iter().any(|line| {
            line.spans
                .iter()
                .any(|span| span.content.as_ref().contains(".git"))
        }));
        assert!(lines.iter().any(|line| {
            line.spans
                .iter()
                .any(|span| span.content.as_ref().contains("hidden"))
        }));

        app.input = "draft".to_owned();
        let _ = app.handle_key_event(KeyEvent::new(KeyCode::Char('j'), KeyModifiers::ALT))?;
        assert_eq!(app.input, "draft");
        assert_eq!(app.selected_tool_timeline_entry, Some(first_tool));

        app.input.clear();
        let _ = app.handle_key_event(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE))?;
        assert_eq!(app.selected_tool_timeline_entry, None);
        assert_eq!(app.last_notice(), Some("tool focus cleared"));
        Ok(())
    }

    #[test]
    fn f1_opens_keyboard_help_modal_from_composer() -> Result<()> {
        let mut app = AppState::from_root_config(Path::new("termquill.toml"), &test_config());
        app.input = "draft".to_owned();
        app.push_timeline(
            TimelineRole::Tool,
            r##"{
  "tool_name": "ls",
  "status": "ok",
  "preview_kind": "json",
  "summary": "first 1/1 lines · 8 B",
  "preview_lines": ["[\".git\"]"],
  "preview_value": [".git"],
  "hidden_lines": 0
}"##,
        );

        let _ = app.handle_key_event(KeyEvent::new(KeyCode::F(1), KeyModifiers::NONE))?;

        assert_eq!(app.input, "draft");
        assert_eq!(app.modal_title(), Some("Keyboard Help"));
        let lines = app.modal_lines();
        assert!(lines.iter().any(|line| line.contains("F1:")));
        assert!(lines.iter().any(|line| line.contains("Ctrl-G:")));
        assert!(lines.iter().any(|line| line.starts_with("/model:")));
        assert!(!lines.iter().any(|line| line.starts_with("/tool:")));

        let _ = app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))?;

        assert!(!app.has_modal());
        assert_eq!(app.last_notice(), Some("closed keyboard help"));
        Ok(())
    }

    #[test]
    fn ctrl_c_quits_while_keyboard_help_modal_is_open() -> Result<()> {
        let mut app = AppState::from_root_config(Path::new("termquill.toml"), &test_config());

        let _ = app.handle_key_event(KeyEvent::new(KeyCode::F(1), KeyModifiers::NONE))?;
        let action =
            app.handle_key_event(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL))?;

        assert!(action.is_none());
        assert!(!app.has_modal());
        assert!(app.should_quit);
        Ok(())
    }

    #[test]
    fn slash_selector_preserves_custom_model_ids() -> Result<()> {
        let mut app = AppState::from_root_config(Path::new("termquill.toml"), &test_config());
        app.input = "/model ds-custom".to_owned();

        let rows = app.slash_selector_rows();
        assert_eq!(rows.first().map(|row| row.0.as_str()), Some("custom"));

        let _ = app.handle_key_event(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE))?;
        assert_eq!(app.input, "/model ds-custom");
        Ok(())
    }

    #[test]
    fn config_command_opens_first_editable_step() -> Result<()> {
        let mut app = AppState::from_root_config(Path::new("termquill.toml"), &test_config());
        app.input = "/config".to_owned();

        let action = app.submit_input()?;

        assert!(action.is_none());
        assert!(app.is_config_mode());
        assert_eq!(app.config_section_title(), Some("Provider"));
        assert_eq!(app.config_selected_field_label(), Some("model"));
        Ok(())
    }

    #[test]
    fn model_picker_opens_with_local_options_before_remote_refresh() -> Result<()> {
        let mut app = AppState::from_root_config(Path::new("termquill.toml"), &test_config());

        app.open_model_picker(ModelPickerTarget::Provider, "custom-model");

        assert!(matches!(app.modal_state, Some(ModalState::ModelPicker(_))));
        assert!(app.model_picker_refresh_rx.is_none());
        assert_eq!(app.last_notice(), Some("using local model list"));
        let lines = app.modal_lines().join("\n");
        assert!(lines.contains("deepseek-v4-flash"));
        assert!(lines.contains("custom-model"));
        Ok(())
    }

    #[test]
    fn model_picker_remote_refresh_updates_open_modal_options() -> Result<()> {
        let mut app = AppState::from_root_config(Path::new("termquill.toml"), &test_config());
        app.open_model_picker(ModelPickerTarget::Provider, "custom-model");

        let changed = app.apply_model_picker_refresh(ModelPickerRefresh {
            target: ModelPickerTarget::Provider,
            current: "custom-model".to_owned(),
            base_url: "https://example.com".to_owned(),
            result: Ok(vec![
                "remote-model-a".to_owned(),
                "remote-model-b".to_owned(),
            ]),
        });

        assert!(changed);
        assert_eq!(
            app.last_notice(),
            Some("loaded provider model list (https://example.com)")
        );
        let lines = app.modal_lines().join("\n");
        assert!(lines.contains("remote-model-a"));
        assert!(lines.contains("remote-model-b"));
        assert!(lines.contains("custom-model"));
        Ok(())
    }

    #[test]
    fn slash_command_does_not_pollute_timeline_as_user_message() -> Result<()> {
        let mut app = AppState::from_root_config(Path::new("termquill.toml"), &test_config());
        app.input = "/config".to_owned();

        let action = app.submit_input()?;

        assert!(action.is_none());
        assert!(
            !app.timeline
                .iter()
                .any(|entry| entry.role == TimelineRole::User && entry.text == "/config")
        );
        assert!(
            app.events
                .iter()
                .any(|event| event.label == "slash" && event.detail == "/config")
        );
        Ok(())
    }

    #[test]
    fn config_up_down_moves_between_fields_in_current_step() -> Result<()> {
        let mut app = AppState::from_root_config(Path::new("termquill.toml"), &test_config());
        app.open_config_panel();

        assert_eq!(app.config_section_title(), Some("Provider"));
        assert_eq!(app.config_selected_field_label(), Some("model"));

        let _ = app.handle_key_event(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE))?;
        assert_eq!(app.config_selected_field_label(), Some("api_key"));

        let _ = app.handle_key_event(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE))?;
        assert_eq!(app.config_selected_field_label(), Some("model"));
        Ok(())
    }

    #[test]
    fn config_down_to_footer_focuses_actions() -> Result<()> {
        let mut app = AppState::from_root_config(Path::new("termquill.toml"), &test_config());
        app.open_config_panel();
        app.config_state
            .as_mut()
            .expect("config state should still exist")
            .selected_field = Some(ConfigField::ProviderFimModel);

        let _ = app.handle_key_event(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE))?;
        assert_eq!(app.config_selected_field_label(), Some("save"));

        let _ = app.handle_key_event(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE))?;
        assert_eq!(app.config_selected_field_label(), Some("save_and_close"));

        let _ = app.handle_key_event(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE))?;
        assert_eq!(app.config_selected_field_label(), Some("close"));

        let _ = app.handle_key_event(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE))?;
        assert_eq!(app.config_selected_field_label(), Some("fim_model"));
        Ok(())
    }

    #[test]
    fn config_left_right_switches_steps() -> Result<()> {
        let mut app = AppState::from_root_config(Path::new("termquill.toml"), &test_config());
        app.open_config_panel();

        let _ = app.handle_key_event(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE))?;
        assert_eq!(app.config_section_title(), Some("Permissions"));
        assert_eq!(app.config_selected_field_label(), Some("write_mode"));

        let _ = app.handle_key_event(KeyEvent::new(KeyCode::Left, KeyModifiers::NONE))?;
        assert_eq!(app.config_section_title(), Some("Provider"));
        assert_eq!(app.config_selected_field_label(), Some("model"));
        Ok(())
    }

    #[test]
    fn config_enter_starts_and_commits_text_edit() -> Result<()> {
        let mut app = AppState::from_root_config(Path::new("termquill.toml"), &test_config());
        app.open_config_panel();

        let _ = app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))?;
        assert!(app.has_modal());
        assert_eq!(app.modal_title(), Some("Model"));

        let _ = app.handle_key_event(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE))?;
        let _ = app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))?;

        let state = app
            .config_state
            .as_ref()
            .expect("config state should still exist");
        assert_eq!(state.draft.provider_model, "deepseek-v4-pro");
        assert!(state.dirty);
        Ok(())
    }

    #[test]
    fn config_text_field_uses_modal_and_applies_value() -> Result<()> {
        let mut app = AppState::from_root_config(Path::new("termquill.toml"), &test_config());
        app.open_config_panel();
        app.config_state
            .as_mut()
            .expect("config state should still exist")
            .selected_field = Some(ConfigField::ProviderBaseUrl);

        assert!(!app.config_is_editing());

        let _ = app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))?;
        assert!(app.config_is_editing());
        assert_eq!(app.modal_title(), Some("Base URL"));

        let _ = app.handle_key_event(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE))?;
        let detail = app.modal_lines().join("\n");
        assert!(detail.contains("base_url: https://api.deepseek.comx|"));

        let _ = app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))?;
        assert!(!app.config_is_editing());
        let state = app
            .config_state
            .as_ref()
            .expect("config state should still exist");
        assert_eq!(state.draft.provider_base_url, "https://api.deepseek.comx");
        assert!(state.dirty);
        Ok(())
    }

    #[test]
    fn config_direct_typing_replaces_selected_text_value() -> Result<()> {
        let mut app = AppState::from_root_config(Path::new("termquill.toml"), &test_config());
        app.open_config_panel();

        let _ = app.handle_key_event(KeyEvent::new(KeyCode::Char('g'), KeyModifiers::NONE))?;
        assert!(app.has_modal());
        let _ = app.handle_key_event(KeyEvent::new(KeyCode::Char('p'), KeyModifiers::NONE))?;
        let detail = app.modal_lines().join("\n");
        assert!(detail.contains("model: gp|"));
        assert!(!detail.contains("deepseek-v4-flashg"));

        let _ = app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))?;
        let state = app
            .config_state
            .as_ref()
            .expect("config state should still exist");
        assert_eq!(state.draft.provider_model, "gp");
        Ok(())
    }

    #[test]
    fn config_escape_cancels_text_modal_before_closing_panel() -> Result<()> {
        let mut app = AppState::from_root_config(Path::new("termquill.toml"), &test_config());
        app.open_config_panel();
        app.config_state
            .as_mut()
            .expect("config state should still exist")
            .selected_field = Some(ConfigField::ProviderBaseUrl);

        let _ = app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))?;
        let _ = app.handle_key_event(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE))?;
        assert!(app.config_is_editing());

        let _ = app.handle_key_event(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE))?;
        assert!(app.is_config_mode());
        assert!(!app.config_is_editing());
        let state = app
            .config_state
            .as_ref()
            .expect("config state should still exist");
        assert_eq!(state.draft.provider_base_url, "https://api.deepseek.com");

        let _ = app.handle_key_event(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE))?;
        assert!(!app.is_config_mode());
        Ok(())
    }

    #[test]
    fn config_provider_flow_hides_advanced_provider_fields() {
        let mut app = AppState::from_root_config(Path::new("termquill.toml"), &test_config());
        app.open_config_panel();

        let detail = app.config_detail_lines().join("\n");

        assert!(!detail.contains("beta_base_url"));
        assert!(!detail.contains("user_id_strategy"));
        assert!(!detail.contains("anthropic_base_url"));
        assert!(!detail.contains("strict_tools_mode"));
        assert!(!detail.contains("request_timeout_secs"));
    }

    #[test]
    fn config_mode_closes_on_escape() -> Result<()> {
        let mut app = AppState::from_root_config(Path::new("termquill.toml"), &test_config());
        app.open_config_panel();

        let action = app.handle_key_event(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE))?;

        assert!(action.is_none());
        assert!(!app.is_config_mode());
        Ok(())
    }

    #[test]
    fn config_save_persists_draft_and_returns_reload_action() -> Result<()> {
        let temp = tempdir()?;
        let config_path = temp.path().join("termquill.toml");
        test_config().save(&config_path)?;

        let mut app = AppState::from_root_config(Path::new("termquill.toml"), &test_config());
        app.config_path = config_path.clone();
        app.open_config_panel();
        let state = app
            .config_state
            .as_mut()
            .expect("config state should exist after opening /config");
        state.set_section(ConfigSection::Provider);
        state.selected_field = Some(ConfigField::ProviderModel);
        state.draft.provider_model = "deepseek-v4-pro".to_owned();
        state.draft.provider_base_url = "https://example.invalid/api".to_owned();
        state.draft.provider_user_id_strategy = "stable_per_workspace".to_owned();
        state.draft.provider_fim_model = "deepseek-v4-flash".to_owned();
        state.draft.permission_write_mode = ApprovalMode::Allow;
        state.draft.memory_enabled = false;
        state.draft.compaction_soft_threshold_ratio = "0.40".to_owned();
        state.draft.compaction_hard_threshold_ratio = "0.75".to_owned();
        state.dirty = true;

        let action =
            app.handle_key_event(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL))?;

        let Some(AppAction::ConfigSaved { root_config }) = action else {
            panic!("expected config save action");
        };
        assert_eq!(root_config.agent.model, "deepseek-v4-pro");
        assert_eq!(root_config.permission.write_mode, ApprovalMode::Allow);
        assert!(!root_config.memory.enabled);
        assert_eq!(root_config.compaction.soft_threshold_ratio, 0.40);
        assert_eq!(root_config.compaction.hard_threshold_ratio, 0.75);
        assert!(!app.config_is_dirty());
        assert_eq!(app.permission_write_mode, "allow");
        assert!(!app.memory_enabled);

        let saved = RootConfig::load(&config_path)?;
        assert_eq!(saved.agent.model, "deepseek-v4-pro");
        assert_eq!(saved.permission.write_mode, ApprovalMode::Allow);
        assert!(!saved.memory.enabled);
        assert_eq!(saved.compaction.soft_threshold_ratio, 0.40);
        assert_eq!(saved.compaction.hard_threshold_ratio, 0.75);
        assert_eq!(
            saved
                .providers
                .get("deepseek")
                .and_then(|value| value.get("model"))
                .and_then(|value| value.as_str()),
            Some("deepseek-v4-pro")
        );
        assert_eq!(
            saved
                .providers
                .get("deepseek")
                .and_then(|value| value.get("base_url"))
                .and_then(|value| value.as_str()),
            Some("https://example.invalid/api")
        );
        assert_eq!(
            saved
                .providers
                .get("deepseek")
                .and_then(|value| value.get("user_id_strategy"))
                .and_then(|value| value.as_str()),
            Some("stable_per_workspace")
        );
        assert_eq!(
            saved
                .providers
                .get("deepseek")
                .and_then(|value| value.get("fim_model"))
                .and_then(|value| value.as_str()),
            Some("deepseek-v4-flash")
        );
        Ok(())
    }

    #[test]
    fn config_inline_api_key_uses_secret_modal_and_persists_to_disk() -> Result<()> {
        let temp = tempdir()?;
        let config_path = temp.path().join("termquill.toml");
        test_config().save(&config_path)?;

        let mut app = AppState::from_root_config(Path::new("termquill.toml"), &test_config());
        app.config_path = config_path.clone();
        app.open_config_panel();
        app.config_state
            .as_mut()
            .expect("config state should exist after opening /config")
            .selected_field = Some(ConfigField::ProviderApiKey);

        let _ = app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))?;
        assert!(app.has_modal());
        assert_eq!(app.modal_title(), Some("API Key"));

        for character in "runtime-secret".chars() {
            let _ =
                app.handle_key_event(KeyEvent::new(KeyCode::Char(character), KeyModifiers::NONE))?;
        }
        let _ = app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))?;

        let action =
            app.handle_key_event(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL))?;

        let Some(AppAction::ConfigSaved { root_config }) = action else {
            panic!("expected config save action");
        };
        assert_eq!(
            root_config
                .providers
                .get("deepseek")
                .and_then(|value| value.get("api_key"))
                .and_then(|value| value.as_str()),
            Some("runtime-secret")
        );

        let saved = RootConfig::load(&config_path)?;
        assert_eq!(
            saved
                .providers
                .get("deepseek")
                .and_then(|value| value.get("api_key"))
                .and_then(|value| value.as_str()),
            Some("runtime-secret")
        );
        Ok(())
    }

    #[test]
    fn config_modal_ctrl_s_applies_field_and_saves() -> Result<()> {
        let temp = tempdir()?;
        let config_path = temp.path().join("termquill.toml");
        test_config().save(&config_path)?;

        let mut app = AppState::from_root_config(Path::new("termquill.toml"), &test_config());
        app.config_path = config_path.clone();
        app.open_config_panel();
        app.config_state
            .as_mut()
            .expect("config state should exist after opening /config")
            .selected_field = Some(ConfigField::ProviderApiKey);

        let _ = app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))?;
        for character in "saved-from-modal".chars() {
            let _ =
                app.handle_key_event(KeyEvent::new(KeyCode::Char(character), KeyModifiers::NONE))?;
        }

        let action =
            app.handle_key_event(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL))?;

        let Some(AppAction::ConfigSaved { root_config }) = action else {
            panic!("expected config save action");
        };
        assert!(!app.has_modal());
        assert_eq!(
            root_config
                .providers
                .get("deepseek")
                .and_then(|value| value.get("api_key"))
                .and_then(|value| value.as_str()),
            Some("saved-from-modal")
        );
        let saved = RootConfig::load(&config_path)?;
        assert_eq!(
            saved
                .providers
                .get("deepseek")
                .and_then(|value| value.get("api_key"))
                .and_then(|value| value.as_str()),
            Some("saved-from-modal")
        );
        Ok(())
    }

    #[test]
    fn config_can_add_and_persist_mcp_server() -> Result<()> {
        let temp = tempdir()?;
        let config_path = temp.path().join("termquill.toml");
        test_config().save(&config_path)?;

        let mut app = AppState::from_root_config(Path::new("termquill.toml"), &test_config());
        app.config_path = config_path.clone();
        app.open_config_panel();
        {
            let state = app
                .config_state
                .as_mut()
                .expect("config state should exist after opening /config");
            state.set_section(ConfigSection::Mcp);
        }

        let action =
            app.handle_key_event(KeyEvent::new(KeyCode::Char('n'), KeyModifiers::CONTROL))?;
        assert!(action.is_none());
        let state = app
            .config_state
            .as_mut()
            .expect("config state should still exist");
        assert_eq!(state.draft.mcp_servers.len(), 1);
        state.draft.mcp_servers[0].name = "filesystem".to_owned();
        state.draft.mcp_servers[0].command = "npx".to_owned();
        state.draft.mcp_servers[0].args_csv =
            "-y, @modelcontextprotocol/server-filesystem, .".to_owned();
        state.draft.mcp_servers[0].startup_timeout_secs = "15".to_owned();

        let action =
            app.handle_key_event(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL))?;

        let Some(AppAction::ConfigSaved { root_config }) = action else {
            panic!("expected config save action");
        };
        assert_eq!(root_config.mcp_servers.len(), 1);
        assert_eq!(root_config.mcp_servers[0].name, "filesystem");
        assert_eq!(root_config.mcp_servers[0].command, "npx");
        assert_eq!(
            root_config.mcp_servers[0].args,
            vec![
                "-y".to_owned(),
                "@modelcontextprotocol/server-filesystem".to_owned(),
                ".".to_owned()
            ]
        );
        assert_eq!(root_config.mcp_servers[0].startup_timeout_secs, 15);

        let saved = RootConfig::load(&config_path)?;
        assert_eq!(saved.mcp_servers.len(), 1);
        assert_eq!(saved.mcp_servers[0].name, "filesystem");
        assert_eq!(saved.mcp_servers[0].command, "npx");
        assert_eq!(
            saved.mcp_servers[0].args,
            vec![
                "-y".to_owned(),
                "@modelcontextprotocol/server-filesystem".to_owned(),
                ".".to_owned()
            ]
        );
        assert_eq!(saved.mcp_servers[0].startup_timeout_secs, 15);
        Ok(())
    }

    #[test]
    fn config_save_is_blocked_while_busy() -> Result<()> {
        let temp = tempdir()?;
        let config_path = temp.path().join("termquill.toml");
        test_config().save(&config_path)?;

        let mut app = AppState::from_root_config(Path::new("termquill.toml"), &test_config());
        app.config_path = config_path.clone();
        app.open_config_panel();
        let state = app
            .config_state
            .as_mut()
            .expect("config state should exist after opening /config");
        state.set_section(ConfigSection::Provider);
        state.selected_field = Some(ConfigField::ProviderModel);
        state.draft.provider_model = "deepseek-v4-pro".to_owned();
        state.dirty = true;
        app.is_busy = true;

        let action =
            app.handle_key_event(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL))?;

        assert!(action.is_none());
        assert_eq!(app.last_notice(), Some("busy; save later"));
        let saved = RootConfig::load(&config_path)?;
        assert_eq!(saved.agent.model, "deepseek-v4-flash");
        Ok(())
    }

    #[test]
    fn config_close_requires_second_escape_when_dirty() -> Result<()> {
        let mut app = AppState::from_root_config(Path::new("termquill.toml"), &test_config());
        app.open_config_panel();
        let state = app
            .config_state
            .as_mut()
            .expect("config state should exist after opening /config");
        state.draft.provider_model = "deepseek-v4-pro".to_owned();
        state.dirty = true;

        let first = app.handle_key_event(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE))?;
        assert!(first.is_none());
        assert!(app.is_config_mode());
        assert_eq!(app.config_selected_field_label(), Some("save"));
        assert_eq!(
            app.last_notice(),
            Some("unsaved changes; Down footer to save, Esc discard")
        );

        let second = app.handle_key_event(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE))?;
        assert!(second.is_none());
        assert!(!app.is_config_mode());
        assert_eq!(app.last_notice(), Some("closed config; discarded changes"));
        Ok(())
    }

    #[test]
    fn config_f2_saves_and_keeps_config_open() -> Result<()> {
        let temp = tempdir()?;
        let config_path = temp.path().join("termquill.toml");
        test_config().save(&config_path)?;

        let mut app = AppState::from_root_config(Path::new("termquill.toml"), &test_config());
        app.config_path = config_path.clone();
        app.open_config_panel();
        let state = app
            .config_state
            .as_mut()
            .expect("config state should exist after opening /config");
        state.draft.provider_api_key = "saved-from-f2".to_owned();
        state.dirty = true;

        let action = app.handle_key_event(KeyEvent::new(KeyCode::F(2), KeyModifiers::NONE))?;

        let Some(AppAction::ConfigSaved { root_config }) = action else {
            panic!("expected config save action");
        };
        assert!(app.is_config_mode());
        assert_eq!(app.last_notice(), Some("saved config"));
        assert_eq!(
            root_config
                .providers
                .get("deepseek")
                .and_then(|value| value.get("api_key"))
                .and_then(|value| value.as_str()),
            Some("saved-from-f2")
        );

        let saved = RootConfig::load(&config_path)?;
        assert_eq!(
            saved
                .providers
                .get("deepseek")
                .and_then(|value| value.get("api_key"))
                .and_then(|value| value.as_str()),
            Some("saved-from-f2")
        );
        Ok(())
    }

    #[test]
    fn config_footer_enter_saves_without_function_keys() -> Result<()> {
        let temp = tempdir()?;
        let config_path = temp.path().join("termquill.toml");
        test_config().save(&config_path)?;

        let mut app = AppState::from_root_config(Path::new("termquill.toml"), &test_config());
        app.config_path = config_path.clone();
        app.open_config_panel();
        let state = app
            .config_state
            .as_mut()
            .expect("config state should exist after opening /config");
        state.selected_field = Some(ConfigField::ProviderFimModel);
        state.draft.provider_api_key = "saved-from-footer".to_owned();
        state.dirty = true;

        let _ = app.handle_key_event(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE))?;
        let action = app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))?;

        let Some(AppAction::ConfigSaved { root_config }) = action else {
            panic!("expected config save action");
        };
        assert!(app.is_config_mode());
        assert_eq!(app.last_notice(), Some("saved config"));
        assert_eq!(
            root_config
                .providers
                .get("deepseek")
                .and_then(|value| value.get("api_key"))
                .and_then(|value| value.as_str()),
            Some("saved-from-footer")
        );

        let saved = RootConfig::load(&config_path)?;
        assert_eq!(
            saved
                .providers
                .get("deepseek")
                .and_then(|value| value.get("api_key"))
                .and_then(|value| value.as_str()),
            Some("saved-from-footer")
        );
        Ok(())
    }

    #[test]
    fn config_f3_saves_and_closes_without_switching_step() -> Result<()> {
        let temp = tempdir()?;
        let config_path = temp.path().join("termquill.toml");
        test_config().save(&config_path)?;

        let mut app = AppState::from_root_config(Path::new("termquill.toml"), &test_config());
        app.config_path = config_path.clone();
        app.open_config_panel();
        let state = app
            .config_state
            .as_mut()
            .expect("config state should exist after opening /config");
        state.draft.provider_api_key = "saved-from-f3".to_owned();
        state.dirty = true;
        state.set_section(ConfigSection::Provider);
        state.selected_field = Some(ConfigField::ProviderApiKey);

        let action = app.handle_key_event(KeyEvent::new(KeyCode::F(3), KeyModifiers::NONE))?;

        let Some(AppAction::ConfigSaved { root_config }) = action else {
            panic!("expected config save action");
        };
        assert!(!app.is_config_mode());
        assert_eq!(app.last_notice(), Some("saved config and closed"));
        assert_eq!(
            root_config
                .providers
                .get("deepseek")
                .and_then(|value| value.get("api_key"))
                .and_then(|value| value.as_str()),
            Some("saved-from-f3")
        );
        Ok(())
    }

    #[test]
    fn config_footer_save_and_close_works_without_function_keys() -> Result<()> {
        let temp = tempdir()?;
        let config_path = temp.path().join("termquill.toml");
        test_config().save(&config_path)?;

        let mut app = AppState::from_root_config(Path::new("termquill.toml"), &test_config());
        app.config_path = config_path.clone();
        app.open_config_panel();
        let state = app
            .config_state
            .as_mut()
            .expect("config state should exist after opening /config");
        state.selected_field = Some(ConfigField::ProviderFimModel);
        state.draft.provider_api_key = "saved-from-footer-close".to_owned();
        state.dirty = true;

        let _ = app.handle_key_event(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE))?;
        let _ = app.handle_key_event(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE))?;
        let action = app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))?;

        let Some(AppAction::ConfigSaved { root_config }) = action else {
            panic!("expected config save action");
        };
        assert!(!app.is_config_mode());
        assert_eq!(app.last_notice(), Some("saved config and closed"));
        assert_eq!(
            root_config
                .providers
                .get("deepseek")
                .and_then(|value| value.get("api_key"))
                .and_then(|value| value.as_str()),
            Some("saved-from-footer-close")
        );

        let saved = RootConfig::load(&config_path)?;
        assert_eq!(
            saved
                .providers
                .get("deepseek")
                .and_then(|value| value.get("api_key"))
                .and_then(|value| value.as_str()),
            Some("saved-from-footer-close")
        );
        Ok(())
    }

    #[test]
    fn submit_root_slash_executes_selected_command() -> Result<()> {
        let mut app = AppState::from_root_config(Path::new("termquill.toml"), &test_config());
        app.input = "/".to_owned();

        let action = app.submit_input()?;

        assert!(matches!(action, Some(AppAction::CompactNow)));
        Ok(())
    }

    #[test]
    fn unknown_slash_command_does_not_become_normal_prompt() -> Result<()> {
        let mut app = AppState::from_root_config(Path::new("termquill.toml"), &test_config());
        app.input = "/unknown".to_owned();

        let action = app.submit_input()?;

        assert!(action.is_none());
        assert!(!app.is_busy);
        assert!(
            app.timeline
                .iter()
                .any(|entry| entry.text.contains("unknown slash command"))
        );
        Ok(())
    }

    #[test]
    fn setup_mode_saves_config_and_returns_runtime_boot_action() -> Result<()> {
        let temp = tempdir()?;
        let config_path = temp.path().join("config").join("termquill.toml");
        let workspace_root = temp.path().join("workspace");
        let mut app = AppState::from_setup(config_path.clone(), workspace_root.clone(), None);

        assert!(app.is_setup_mode());

        let action = app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))?;
        assert!(action.is_none());
        app.setup_state
            .as_mut()
            .expect("setup state should exist in setup mode")
            .selected_field = SetupField::Model;
        let _ = app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))?;
        assert!(app.has_modal());
        let _ = app.handle_key_event(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE))?;
        let _ = app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))?;
        app.setup_state
            .as_mut()
            .expect("setup state should exist in setup mode")
            .selected_field = SetupField::ApiKey;
        for character in "test-inline-key".chars() {
            let _ =
                app.handle_key_event(KeyEvent::new(KeyCode::Char(character), KeyModifiers::NONE))?;
        }
        let _ = app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))?;
        app.setup_state
            .as_mut()
            .expect("setup state should exist in setup mode")
            .selected_field = SetupField::Save;
        let action = app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))?;

        let Some(AppAction::SetupCompleted {
            config_path: saved_path,
            root_config,
        }) = action
        else {
            panic!("expected setup completion action");
        };
        assert_eq!(saved_path, config_path);
        assert_eq!(root_config.workspace.root, ".");
        assert_eq!(root_config.agent.model, "deepseek-v4-pro");
        let saved = RootConfig::load(&saved_path)?;
        assert_eq!(saved.agent.provider, "deepseek");
        assert_eq!(saved.agent.model, "deepseek-v4-pro");
        assert_eq!(
            saved
                .providers
                .get("deepseek")
                .and_then(|value| value.get("api_key"))
                .and_then(|value| value.as_str()),
            Some("test-inline-key")
        );
        assert_eq!(
            root_config
                .providers
                .get("deepseek")
                .and_then(|value| value.get("api_key"))
                .and_then(|value| value.as_str()),
            Some("test-inline-key")
        );
        assert!(saved.memory.enabled);
        assert!(saved.compaction.enabled);
        Ok(())
    }

    #[test]
    fn setup_modal_ctrl_s_applies_field_and_saves_config() -> Result<()> {
        let temp = tempdir()?;
        let config_path = temp.path().join("config").join("termquill.toml");
        let workspace_root = temp.path().join("workspace");
        let mut app = AppState::from_setup(config_path.clone(), workspace_root, None);
        {
            let state = app
                .setup_state
                .as_mut()
                .expect("setup state should exist in setup mode");
            state.trusted_current_folder = true;
            state.selected_field = SetupField::ApiKey;
        }

        let _ = app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))?;
        for character in "setup-saved-key".chars() {
            let _ =
                app.handle_key_event(KeyEvent::new(KeyCode::Char(character), KeyModifiers::NONE))?;
        }

        let action =
            app.handle_key_event(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL))?;

        let Some(AppAction::SetupCompleted {
            config_path: saved_path,
            root_config,
        }) = action
        else {
            panic!("expected setup completion action");
        };
        assert_eq!(saved_path, config_path);
        assert!(!app.has_modal());
        assert_eq!(
            root_config
                .providers
                .get("deepseek")
                .and_then(|value| value.get("api_key"))
                .and_then(|value| value.as_str()),
            Some("setup-saved-key")
        );

        let saved = RootConfig::load(&saved_path)?;
        assert_eq!(
            saved
                .providers
                .get("deepseek")
                .and_then(|value| value.get("api_key"))
                .and_then(|value| value.as_str()),
            Some("setup-saved-key")
        );
        Ok(())
    }

    #[test]
    fn setup_save_requires_credentials() -> Result<()> {
        let temp = tempdir()?;
        let config_path = temp.path().join("config").join("termquill.toml");
        let workspace_root = temp.path().join("workspace");
        let mut app = AppState::from_setup(config_path, workspace_root, None);
        let state = app
            .setup_state
            .as_mut()
            .expect("setup state should exist in setup mode");
        state.selected_field = SetupField::Save;
        state.trusted_current_folder = true;
        state.api_key.clear();

        let action = app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))?;

        assert!(action.is_none());
        let state = app
            .setup_state
            .as_ref()
            .expect("setup state should exist in setup mode");
        assert_eq!(state.selected_field, SetupField::Save);
        assert_eq!(
            app.last_notice(),
            Some("provide api_key or export TERMQUILL_API_KEY")
        );
        Ok(())
    }

    #[test]
    fn run_failed_surfaces_root_cause_summary_in_notice() -> Result<()> {
        let mut app = AppState::from_root_config(Path::new("termquill.toml"), &test_config());

        app.handle_worker_message(WorkerMessage::RunFailed(
            "deepseek request failed\n\nCaused by:\n    0: failed to send DeepSeek request\n    1: error sending request for url (https://api.example.com)"
                .to_owned(),
        ))?;

        assert_eq!(
            app.last_notice(),
            Some("error sending request for url (https://api.example.com)")
        );
        assert!(app.timeline.iter().any(|entry| {
            entry
                .text
                .contains("error sending request for url (https://api.example.com)")
        }));
        assert!(
            app.events.iter().any(|event| event.label == "run:error"
                && event.detail.contains("deepseek request failed"))
        );
        Ok(())
    }

    #[test]
    fn compaction_status_tracks_latest_prompt_tokens_instead_of_cumulative_totals() -> Result<()> {
        let mut config = test_config();
        config.agent.provider = "planned".to_owned();
        config.agent.model = "planned-model".to_owned();
        config.compaction.context_window_tokens = Some(100);
        config.compaction.soft_threshold_ratio = 0.5;
        config.compaction.hard_threshold_ratio = 0.8;
        let mut app = AppState::from_root_config(Path::new("termquill.toml"), &config);

        app.handle(RunEvent::Usage(UsageStats {
            prompt_tokens: 70,
            completion_tokens: 0,
            cache_hit_tokens: 0,
            cache_miss_tokens: 70,
            input_cost: 0.0,
            output_cost: 0.0,
            cache_savings: 0.0,
            system_fingerprint: None,
        }))?;
        assert_eq!(app.compaction_status, "soft");

        app.handle(RunEvent::Usage(UsageStats {
            prompt_tokens: 20,
            completion_tokens: 0,
            cache_hit_tokens: 0,
            cache_miss_tokens: 20,
            input_cost: 0.0,
            output_cost: 0.0,
            cache_savings: 0.0,
            system_fingerprint: None,
        }))?;

        assert_eq!(app.compaction_status, "ready");
        Ok(())
    }

    #[test]
    fn context_usage_and_compaction_policy_share_effective_window() -> Result<()> {
        let mut config = test_config();
        config.agent.model = "deepseek-v4-pro".to_owned();
        config.compaction.context_window_tokens = Some(128_000);
        config.compaction.soft_threshold_ratio = 0.5;
        config.compaction.hard_threshold_ratio = 0.8;
        let mut app = AppState::from_root_config(Path::new("termquill.toml"), &config);

        app.handle(RunEvent::Usage(UsageStats {
            prompt_tokens: 90_354,
            completion_tokens: 0,
            cache_hit_tokens: 0,
            cache_miss_tokens: 90_354,
            input_cost: 0.0,
            output_cost: 0.0,
            cache_savings: 0.0,
            system_fingerprint: None,
        }))?;

        assert_eq!(app.context_usage_line(), "ctx: 9% · 90.4K / 1.0M tok");
        assert_eq!(app.compaction_status, "ready");
        assert!(app.footer_status_line().contains("tok 90.4K"));
        assert!(app.footer_status_line().contains("ctx 9%"));
        assert!(
            app.usage_sidebar_lines()
                .iter()
                .any(|line| line == "policy: 1,000,000 model · soft 50% · hard 80%")
        );
        Ok(())
    }

    #[test]
    fn session_sidebar_lines_include_model_and_phase() {
        let mut app = AppState::from_root_config(Path::new("termquill.toml"), &test_config());
        app.run_phase = RunPhase::Thinking;

        let lines = app.session_sidebar_lines();

        assert!(lines.iter().any(|line| line == "provider: deepseek"));
        assert!(lines.iter().any(|line| line == "model: deepseek-v4-flash"));
        assert!(lines.iter().any(|line| line == "effort: max"));
        assert!(lines.iter().any(|line| line == "phase: thinking"));
    }

    #[test]
    fn exit_alias_quits_tui() -> Result<()> {
        let mut app = AppState::from_root_config(Path::new("termquill.toml"), &test_config());
        app.input = "/exit".to_owned();

        let action = app.submit_input()?;

        assert!(action.is_none());
        assert!(app.should_quit);
        assert!(
            app.timeline
                .iter()
                .any(|entry| entry.role == TimelineRole::Notice && entry.text == "quitting")
        );
        Ok(())
    }

    #[test]
    fn live_activity_summary_tracks_busy_phase() {
        let mut app = AppState::from_root_config(Path::new("termquill.toml"), &test_config());
        assert!(app.live_activity_summary().is_none());

        app.is_busy = true;
        app.run_phase = RunPhase::Tool("read_file".to_owned());

        let summary = app.live_activity_summary().expect("expected live summary");
        assert_eq!(summary.label, "tool");
        assert_eq!(summary.detail, "running read_file");
    }

    #[test]
    fn session_display_title_uses_first_user_prompt() -> Result<()> {
        let mut app = AppState::from_root_config(Path::new("termquill.toml"), &test_config());
        app.input = "Summarize the codebase architecture".to_owned();

        let action = app.submit_input()?;

        assert!(matches!(action, Some(AppAction::SubmitPrompt(_))));
        assert_eq!(
            app.session_display_title(),
            "Summarize the codebase architecture".to_owned()
        );
        Ok(())
    }

    #[test]
    fn latest_user_prompt_preview_reflects_recent_submission() -> Result<()> {
        let mut app = AppState::from_root_config(Path::new("termquill.toml"), &test_config());
        app.input = "hello from user".to_owned();

        let action = app.submit_input()?;

        assert!(matches!(action, Some(AppAction::SubmitPrompt(_))));
        assert_eq!(
            app.latest_user_prompt_preview(),
            Some("hello from user".to_owned())
        );
        Ok(())
    }

    #[test]
    fn automatic_compaction_message_resets_status_and_emits_notice() -> Result<()> {
        let mut config = test_config();
        config.agent.provider = "planned".to_owned();
        config.agent.model = "planned-model".to_owned();
        config.compaction.context_window_tokens = Some(100);
        config.compaction.soft_threshold_ratio = 0.5;
        config.compaction.hard_threshold_ratio = 0.8;
        let mut app = AppState::from_root_config(Path::new("termquill.toml"), &config);
        let session_log_path = app.session_log_path.clone();

        app.handle(RunEvent::Usage(UsageStats {
            prompt_tokens: 90,
            completion_tokens: 0,
            cache_hit_tokens: 0,
            cache_miss_tokens: 90,
            input_cost: 0.0,
            output_cost: 0.0,
            cache_savings: 0.0,
            system_fingerprint: None,
        }))?;
        assert_eq!(app.compaction_status, "hard");

        app.handle_worker_message(WorkerMessage::SessionCompacted {
            session_log_path,
            provider_name: app.provider_name.clone(),
            model_name: app.model_name.clone(),
            record: CompactionRecord {
                summary: "summary".to_owned(),
                compacted_message_count: 3,
                retained_tail_message_count: 2,
            },
            trigger: CompactionTrigger::AutomaticHardThreshold,
            entries: Vec::new(),
        })?;

        assert_eq!(app.compaction_status, "ready");
        assert_eq!(app.stats.last_prompt_tokens, 0);
        assert!(app.timeline.iter().any(|entry| {
            entry.role == TimelineRole::Notice && entry.text.contains("Auto-compacted")
        }));
        Ok(())
    }

    #[test]
    fn restored_session_view_shows_compaction_block_and_restored_prompt_pressure() -> Result<()> {
        let mut config = test_config();
        config.compaction.context_window_tokens = Some(100);
        config.compaction.soft_threshold_ratio = 0.5;
        config.compaction.hard_threshold_ratio = 0.8;
        let mut app = AppState::from_root_config(Path::new("termquill.toml"), &config);
        let session_log_path = app.session_log_path.clone();
        let entries = vec![
            SessionLogEntry::Control(ControlEntry::SessionIdentity {
                provider_name: "deepseek".to_owned(),
                model_name: "deepseek-v4-flash".to_owned(),
            }),
            SessionLogEntry::Control(ControlEntry::UsageSnapshot(UsageStats {
                prompt_tokens: 65,
                completion_tokens: 8,
                cache_hit_tokens: 45,
                cache_miss_tokens: 20,
                input_cost: 0.0,
                output_cost: 0.0,
                cache_savings: 0.0,
                system_fingerprint: None,
            })),
            SessionLogEntry::Control(ControlEntry::CompactionApplied(CompactionRecord {
                summary: "Compacted 2 earlier messages into a stable local summary.\n01. user hello\n02. assistant world".to_owned(),
                compacted_message_count: 2,
                retained_tail_message_count: 3,
            })),
            SessionLogEntry::User(ModelMessage::user("latest prompt")),
        ];

        app.handle_worker_message(WorkerMessage::SessionSwitched {
            session_log_path,
            provider_name: "deepseek".to_owned(),
            model_name: "deepseek-v4-flash".to_owned(),
            entries,
        })?;

        let lines = app.approval_preview_lines();
        assert_eq!(app.compaction_status, "ready");
        assert!(lines.iter().any(|line| line.contains("prompt=0")));
        assert!(
            lines
                .iter()
                .any(|line| line.contains("summary: compacted=2 tail=3"))
        );
        assert!(
            lines
                .iter()
                .any(|line| line.contains("[assistant] Compacted 2 earlier messages"))
        );
        assert!(lines.iter().any(|line| line.contains("/compact preview")));
        Ok(())
    }

    #[test]
    fn session_view_mode_toggle_switches_between_provider_and_audit() -> Result<()> {
        let mut app = AppState::from_root_config(Path::new("termquill.toml"), &test_config());
        app.handle_worker_message(WorkerMessage::SessionSwitched {
            session_log_path: app.session_log_path.clone(),
            provider_name: app.provider_name.clone(),
            model_name: app.model_name.clone(),
            entries: vec![
                SessionLogEntry::Control(ControlEntry::SessionIdentity {
                    provider_name: "deepseek".to_owned(),
                    model_name: "deepseek-v4-flash".to_owned(),
                }),
                SessionLogEntry::Control(ControlEntry::CompactionApplied(CompactionRecord {
                    summary: "Compacted 1 earlier messages into a stable local summary.".to_owned(),
                    compacted_message_count: 1,
                    retained_tail_message_count: 1,
                })),
                SessionLogEntry::User(ModelMessage::user("latest prompt")),
            ],
        })?;

        let provider_lines = app.approval_preview_lines().join("\n");
        assert!(provider_lines.contains("provider view"));
        assert!(provider_lines.contains("Provider:"));

        app.session_view_mode = super::SessionViewMode::Audit;
        let audit_lines = app.approval_preview_lines().join("\n");
        assert!(audit_lines.contains("audit view"));
        assert!(audit_lines.contains("Audit:"));
        assert!(audit_lines.contains("[ctl] compacted=1 tail=1"));
        Ok(())
    }

    #[test]
    fn composer_up_down_navigates_input_history() -> Result<()> {
        let mut app = AppState::from_root_config(Path::new("termquill.toml"), &test_config());
        app.input = "first".to_owned();
        assert!(matches!(
            app.submit_input()?,
            Some(AppAction::SubmitPrompt(prompt)) if prompt == "first"
        ));
        app.is_busy = false;

        app.input = "second".to_owned();
        assert!(matches!(
            app.submit_input()?,
            Some(AppAction::SubmitPrompt(prompt)) if prompt == "second"
        ));
        app.is_busy = false;

        app.input = "draft".to_owned();
        app.active_pane = PaneFocus::Composer;
        app.handle_key_event(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE))?;
        assert_eq!(app.input, "second");
        app.handle_key_event(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE))?;
        assert_eq!(app.input, "first");
        app.handle_key_event(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE))?;
        assert_eq!(app.input, "second");
        app.handle_key_event(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE))?;
        assert_eq!(app.input, "draft");
        Ok(())
    }

    #[test]
    fn composer_up_inside_wrapped_input_moves_cursor_before_history() -> Result<()> {
        let mut app = AppState::from_root_config(Path::new("termquill.toml"), &test_config());
        app.input = "first".to_owned();
        assert!(matches!(
            app.submit_input()?,
            Some(AppAction::SubmitPrompt(prompt)) if prompt == "first"
        ));
        app.is_busy = false;

        app.active_pane = PaneFocus::Composer;
        app.set_terminal_size(6, 20);
        app.input = "draft123456".to_owned();
        app.input_cursor = 7;

        app.handle_key_event(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE))?;

        assert_eq!(app.input, "draft123456");
        assert_eq!(app.input_cursor, 1);
        assert_eq!(app.input_history_index, None);
        Ok(())
    }

    #[test]
    fn composer_down_at_bottom_row_navigates_history() -> Result<()> {
        let mut app = AppState::from_root_config(Path::new("termquill.toml"), &test_config());
        app.input = "first".to_owned();
        assert!(matches!(
            app.submit_input()?,
            Some(AppAction::SubmitPrompt(prompt)) if prompt == "first"
        ));
        app.is_busy = false;

        app.input = "second".to_owned();
        assert!(matches!(
            app.submit_input()?,
            Some(AppAction::SubmitPrompt(prompt)) if prompt == "second"
        ));
        app.is_busy = false;

        app.active_pane = PaneFocus::Composer;
        app.set_terminal_size(6, 20);
        app.input = "draft123".to_owned();
        app.input_cursor = 1;

        app.handle_key_event(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE))?;
        assert_eq!(app.input, "second");
        app.input_cursor = app.input.chars().count();

        app.handle_key_event(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE))?;
        assert_eq!(app.input, "draft123");
        Ok(())
    }

    #[test]
    fn sessions_filter_narrows_sidebar_results() -> Result<()> {
        let temp = tempdir()?;
        let config = RootConfig {
            workspace: WorkspaceConfig {
                root: temp.path().display().to_string(),
            },
            ..test_config()
        };
        let session_dir = temp.path().join(".termquill/sessions");
        std::fs::create_dir_all(&session_dir)?;
        std::fs::write(session_dir.join("session-alpha.jsonl"), "")?;
        std::fs::write(session_dir.join("session-beta.jsonl"), "")?;

        let mut app = AppState::from_root_config(Path::new("termquill.toml"), &config);
        app.refresh_session_history();
        app.session_history_filter = "b".to_owned();
        let lines = app.recent_session_lines().join("\n");
        assert!(lines.contains("beta"));
        assert!(!lines.contains("alpha"));
        Ok(())
    }

    #[test]
    fn session_rows_mark_selected_and_current_entry() -> Result<()> {
        let temp = tempdir()?;
        let config = RootConfig {
            workspace: WorkspaceConfig {
                root: temp.path().display().to_string(),
            },
            ..test_config()
        };
        let session_dir = temp.path().join(".termquill/sessions");
        std::fs::create_dir_all(&session_dir)?;
        let alpha = session_dir.join("session-alpha.jsonl");
        let beta = session_dir.join("session-beta.jsonl");
        std::fs::write(&alpha, "")?;
        std::fs::write(&beta, "")?;

        let mut app = AppState::from_root_config(Path::new("termquill.toml"), &config);
        app.session_log_path = beta.clone();
        app.refresh_session_history();

        let rows = app.recent_session_rows();
        assert!(rows.iter().any(|row| {
            matches!(
                row,
                super::SessionHistoryRow::SessionItem {
                    label,
                    current: true,
                    selected: true,
                    ..
                } if label.contains("beta")
            )
        }));
        Ok(())
    }

    #[test]
    fn session_history_uses_first_user_prompt_as_display_title() -> Result<()> {
        let temp = tempdir()?;
        let config = RootConfig {
            workspace: WorkspaceConfig {
                root: temp.path().display().to_string(),
            },
            ..test_config()
        };
        let session_dir = temp.path().join(".termquill/sessions");
        std::fs::create_dir_all(&session_dir)?;
        let session_path = session_dir.join("session-title.jsonl");
        write_session_log(
            &session_path,
            &[
                SessionLogEntry::Control(ControlEntry::SessionIdentity {
                    provider_name: "deepseek".to_owned(),
                    model_name: "deepseek-v4-pro".to_owned(),
                }),
                SessionLogEntry::User(ModelMessage::user("Investigate selector title display")),
            ],
        )?;

        let mut app = AppState::from_root_config(Path::new("termquill.toml"), &config);
        app.refresh_session_history();

        assert_eq!(
            app.session_history
                .iter()
                .find(|entry| entry.path == session_path)
                .and_then(|entry| entry.title.as_deref()),
            Some("Investigate selector title display")
        );

        app.input = "/resume".to_owned();
        assert!(app.slash_selector_rows().iter().any(|(_, description)| {
            description.contains("Investigate selector title display")
        }));
        Ok(())
    }

    #[test]
    fn resume_command_shows_session_selector_and_enter_switches_selected_session() -> Result<()> {
        let temp = tempdir()?;
        let config = RootConfig {
            workspace: WorkspaceConfig {
                root: temp.path().display().to_string(),
            },
            ..test_config()
        };
        let session_dir = temp.path().join(".termquill/sessions");
        std::fs::create_dir_all(&session_dir)?;
        let restored_path = session_dir.join("session-restored.jsonl");
        let restored = restored_entries("restored-provider", "restored-model");
        write_session_log(&restored_path, &restored)?;

        let mut app = AppState::from_root_config(Path::new("termquill.toml"), &config);
        app.input = "/resume".to_owned();

        let selector_rows = app.slash_selector_rows();
        assert_eq!(app.slash_selector_title(), Some("Resume session"));
        assert_eq!(app.slash_selector_visible_rows(), 2);
        assert!(
            selector_rows
                .iter()
                .any(|(_, description)| description.contains("restored"))
        );

        let action = app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))?;
        assert!(matches!(
            action,
            Some(AppAction::SwitchSession { session_log_path }) if session_log_path == restored_path
        ));
        Ok(())
    }

    #[test]
    fn approval_diff_mode_cycles_to_changed_only() -> Result<()> {
        let mut app = AppState::from_root_config(Path::new("termquill.toml"), &test_config());
        inject_write_file_approval(&mut app, sample_approval_preview())?;
        app.handle_key_event(KeyEvent::new(KeyCode::Char('v'), KeyModifiers::NONE))?;
        app.handle_key_event(KeyEvent::new(KeyCode::Char('v'), KeyModifiers::NONE))?;
        let lines = app.approval_preview_lines().join("\n");
        assert!(lines.contains("mode=changed-only"));
        assert!(!lines.contains("   alpha"));
        assert!(lines.contains("-beta"));
        assert!(lines.contains("+gamma"));
        Ok(())
    }

    #[test]
    fn resume_command_then_session_switch_restores_durable_view() -> Result<()> {
        let temp = tempdir()?;
        let config = RootConfig {
            workspace: WorkspaceConfig {
                root: temp.path().display().to_string(),
            },
            ..test_config()
        };
        let session_dir = temp.path().join(".termquill/sessions");
        std::fs::create_dir_all(&session_dir)?;
        let restored_path = session_dir.join("session-restored.jsonl");
        let restored = restored_entries("restored-provider", "restored-model");
        write_session_log(&restored_path, &restored)?;

        let mut app = AppState::from_root_config(Path::new("termquill.toml"), &config);
        app.input = "/resume 1".to_owned();
        let action = app.submit_input()?;
        assert!(matches!(
            action,
            Some(AppAction::SwitchSession { session_log_path }) if session_log_path == restored_path
        ));

        let entries = JsonlSessionStore::read_entries(&restored_path)?;
        app.handle_worker_message(WorkerMessage::SessionSwitched {
            session_log_path: restored_path.clone(),
            provider_name: "restored-provider".to_owned(),
            model_name: "restored-model".to_owned(),
            entries,
        })?;

        assert_eq!(app.provider_name, "restored-provider");
        assert_eq!(app.model_name, "restored-model");
        assert_eq!(app.session_id, "restored");
        assert_eq!(app.session_log_path, restored_path);
        assert!(
            app.timeline
                .iter()
                .any(|entry| entry.text.contains("restored from disk"))
        );
        assert!(
            app.timeline
                .iter()
                .any(|entry| entry.text == "restored user prompt")
        );
        assert!(
            app.timeline
                .iter()
                .any(|entry| entry.text.contains("restored tool output"))
        );
        assert!(
            app.timeline
                .iter()
                .any(|entry| entry.text == "restored assistant answer")
        );
        assert!(
            app.events.iter().any(|event| event.label == "model"
                && event.detail == "restored-provider/restored-model")
        );
        assert!(
            app.events
                .iter()
                .any(|event| event.label == "restore" && event.detail == "entries=4")
        );
        Ok(())
    }

    #[test]
    fn ctrl_c_then_run_cancelled_restores_durable_session_view() -> Result<()> {
        let temp = tempdir()?;
        let config = RootConfig {
            workspace: WorkspaceConfig {
                root: temp.path().display().to_string(),
            },
            ..test_config()
        };
        let session_dir = temp.path().join(".termquill/sessions");
        std::fs::create_dir_all(&session_dir)?;
        let restored_path = session_dir.join("session-cancelled.jsonl");
        let restored = restored_entries("cancel-provider", "cancel-model");
        write_session_log(&restored_path, &restored)?;

        let mut app = AppState::from_root_config(Path::new("termquill.toml"), &config);
        app.input = "volatile prompt".to_owned();
        assert!(matches!(
            app.submit_input()?,
            Some(AppAction::SubmitPrompt(prompt)) if prompt == "volatile prompt"
        ));
        assert!(app.is_busy);

        let cancel_action =
            app.handle_key_event(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL))?;
        assert!(matches!(cancel_action, Some(AppAction::CancelRun)));
        assert!(
            app.timeline
                .iter()
                .any(|entry| entry.text.contains("cancel requested"))
        );

        let entries = JsonlSessionStore::read_entries(&restored_path)?;
        app.handle_worker_message(WorkerMessage::RunCancelled {
            session_log_path: restored_path.clone(),
            provider_name: "cancel-provider".to_owned(),
            model_name: "cancel-model".to_owned(),
            entries,
        })?;

        assert!(!app.is_busy);
        assert!(app.pending_approval.is_none());
        assert_eq!(app.provider_name, "cancel-provider");
        assert_eq!(app.model_name, "cancel-model");
        assert_eq!(app.session_id, "cancelled");
        assert_eq!(app.session_log_path, restored_path);
        assert!(
            app.timeline
                .iter()
                .any(|entry| { entry.text.contains("run cancelled; restored") })
        );
        assert!(
            !app.timeline
                .iter()
                .any(|entry| entry.text == "volatile prompt")
        );
        assert!(
            app.timeline
                .iter()
                .any(|entry| entry.text == "restored assistant answer")
        );
        assert!(
            app.events
                .iter()
                .any(|event| event.label == "restore" && event.detail == "entries=4")
        );
        assert!(
            app.events
                .iter()
                .any(|event| event.label == "model"
                    && event.detail == "cancel-provider/cancel-model")
        );
        Ok(())
    }

    #[test]
    fn approval_keys_emit_allow_and_deny_actions() -> Result<()> {
        let mut app = AppState::from_root_config(Path::new("termquill.toml"), &test_config());
        inject_write_file_approval(&mut app, sample_approval_preview())?;

        let allow = app.handle_key_event(KeyEvent::new(KeyCode::Char('y'), KeyModifiers::NONE))?;
        assert!(matches!(
            allow,
            Some(AppAction::ApprovalDecision { call_id, approved })
                if call_id == "call-1" && approved
        ));

        let deny = app.handle_key_event(KeyEvent::new(KeyCode::Char('n'), KeyModifiers::NONE))?;
        assert!(matches!(
            deny,
            Some(AppAction::ApprovalDecision { call_id, approved })
                if call_id == "call-1" && !approved
        ));
        Ok(())
    }

    #[test]
    fn approval_metadata_toggle_collapses_and_expands_preview_header() -> Result<()> {
        let mut app = AppState::from_root_config(Path::new("termquill.toml"), &test_config());
        inject_write_file_approval(&mut app, sample_approval_preview())?;

        let expanded = app.approval_preview_lines().join("\n");
        assert!(expanded.contains("tool=write_file"));
        assert!(expanded.contains("preview=Update note.txt"));

        assert!(
            app.handle_key_event(KeyEvent::new(KeyCode::Char('m'), KeyModifiers::NONE))?
                .is_none()
        );
        let collapsed = app.approval_preview_lines().join("\n");
        assert!(collapsed.contains("meta hidden"));
        assert!(!collapsed.contains("tool=write_file"));
        assert!(app.events.iter().any(|event| {
            event.label == "approval:view" && event.detail == "metadata collapsed"
        }));

        assert!(
            app.handle_key_event(KeyEvent::new(KeyCode::Char('m'), KeyModifiers::NONE))?
                .is_none()
        );
        let reexpanded = app.approval_preview_lines().join("\n");
        assert!(reexpanded.contains("tool=write_file"));
        assert!(app.events.iter().any(|event| {
            event.label == "approval:view" && event.detail == "metadata expanded"
        }));
        Ok(())
    }

    #[test]
    fn approval_hunk_and_file_navigation_updates_selection() -> Result<()> {
        let mut app = AppState::from_root_config(Path::new("termquill.toml"), &test_config());
        inject_write_file_approval(&mut app, multi_file_approval_preview())?;

        assert_eq!(app.approval_selected_file_index, 0);
        assert_eq!(app.approval_selected_hunk_index, 0);

        assert!(
            app.handle_key_event(KeyEvent::new(KeyCode::Char(']'), KeyModifiers::NONE))?
                .is_none()
        );
        assert_eq!(app.approval_selected_hunk_index, 1);
        assert!(app.approval_scroll_back > 0);
        let jumped = app.approval_preview_lines().join("\n");
        assert!(jumped.contains("hunk 2/2"));

        assert!(
            app.handle_key_event(KeyEvent::new(KeyCode::Char('.'), KeyModifiers::NONE))?
                .is_none()
        );
        assert_eq!(app.approval_selected_file_index, 1);
        assert_eq!(app.approval_selected_hunk_index, 0);
        assert_eq!(app.approval_scroll_back, 0);
        let second_file = app.approval_preview_lines().join("\n");
        assert!(second_file.contains("file 2/2"));
        assert!(second_file.contains("> note-b.txt"));

        assert!(
            app.handle_key_event(KeyEvent::new(KeyCode::Char(','), KeyModifiers::NONE))?
                .is_none()
        );
        assert_eq!(app.approval_selected_file_index, 0);
        let first_file = app.approval_preview_lines().join("\n");
        assert!(first_file.contains("file 1/2"));
        assert!(first_file.contains("> note-a.txt"));
        Ok(())
    }

    #[test]
    fn approval_modal_view_tracks_selected_hunk() -> Result<()> {
        let mut app = AppState::from_root_config(Path::new("termquill.toml"), &test_config());
        inject_write_file_approval(&mut app, multi_file_approval_preview())?;

        assert!(
            app.handle_key_event(KeyEvent::new(KeyCode::Char(']'), KeyModifiers::NONE))?
                .is_none()
        );

        let view = app
            .approval_modal_view()
            .expect("approval modal view should exist");
        assert_eq!(view.diff_label, "note-a.txt");
        assert_eq!(view.active_hunk_index, 2);
        assert_eq!(view.hunk_total, 2);
        assert!(
            view.file_rows
                .iter()
                .any(|row| row.path == "note-a.txt" && row.selected)
        );
        assert!(view.diff_lines.iter().any(|line| {
            line.active_hunk
                && line.kind == super::ApprovalDiffLineKind::Hunk
                && line.text.contains("@@ -5,2 +5,2 @@")
        }));
        Ok(())
    }
}

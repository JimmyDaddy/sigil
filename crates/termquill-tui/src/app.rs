use std::{
    collections::BTreeSet,
    env, fs,
    ops::Range,
    path::{Path, PathBuf},
    sync::mpsc::{self, Receiver},
    thread,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use anyhow::{Result, anyhow, bail};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::text::Line;
use reqwest::blocking::Client as BlockingClient;
use termquill_kernel::{
    AgentConfig, ApprovalMode, CompactionConfig, CompactionPreview, CompactionRecord,
    CompactionThresholdStatus, ControlEntry, EventHandler, JsonlSessionStore, McpServerConfig,
    MemoryConfig, ModelMessage, PermissionConfig, ReasoningEffort, RootConfig, RunEvent, Session,
    SessionConfig, SessionLogEntry, SessionStats, ToolCall, ToolPreview, ToolResult,
    ToolResultMeta, ToolSpec, UsageStats, WorkspaceConfig, inspect_memory_documents,
    latest_compaction_record, resolve_workspace_root, session_stats_from_entries,
};
use termquill_provider_deepseek::{DeepSeekProviderConfig, StrictToolsMode, TERMQUILL_API_KEY_ENV};
use unicode_width::UnicodeWidthChar;
use uuid::Uuid;

use crate::context_window::{
    ContextWindowSource, effective_compaction_config, resolve_context_window_tokens,
};
use crate::runner::{CompactionTrigger, WorkerCommand, WorkerMessage};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimelineRole {
    System,
    User,
    Assistant,
    Phase,
    Thinking,
    Tool,
    Notice,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaneFocus {
    Composer,
    Activity,
}

impl PaneFocus {
    pub fn label(self) -> &'static str {
        match self {
            Self::Composer => "composer",
            Self::Activity => "activity",
        }
    }
}

#[derive(Debug, Clone)]
pub struct TimelineEntry {
    pub role: TimelineRole,
    pub text: String,
}

#[derive(Debug, Clone)]
pub struct EventEntry {
    pub label: String,
    pub detail: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LiveActivitySummary {
    pub label: String,
    pub detail: String,
}

#[cfg_attr(not(test), allow(dead_code))]
#[derive(Debug, Clone)]
pub(crate) enum ActivityPanelRow {
    Event {
        label: String,
        detail: String,
    },
    SessionHeader {
        filter: String,
        total: usize,
    },
    SessionItem {
        index: usize,
        label: String,
        current: bool,
        selected: bool,
        meta: String,
    },
    Empty {
        text: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SidebarCard {
    Permission,
    Agents,
    Usage,
}

impl SidebarCard {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Permission => "permission",
            Self::Agents => "agents",
            Self::Usage => "usage",
        }
    }

    fn next(self) -> Self {
        match self {
            Self::Permission => Self::Agents,
            Self::Agents => Self::Usage,
            Self::Usage => Self::Permission,
        }
    }

    fn previous(self) -> Self {
        match self {
            Self::Permission => Self::Usage,
            Self::Agents => Self::Permission,
            Self::Usage => Self::Agents,
        }
    }
}

#[allow(dead_code)]
#[derive(Debug, Clone)]
pub(crate) struct SidebarAgentRow {
    pub label: String,
    pub detail: String,
    pub selected: bool,
    pub muted: bool,
}

#[derive(Debug, Clone, Default)]
struct BalanceSnapshot {
    total: Option<f64>,
    currency: Option<String>,
    available: bool,
    status: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RunPhase {
    Idle,
    Thinking,
    Tool(String),
    Streaming,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ApprovalDiffLineKind {
    Header,
    Hunk,
    Added,
    Removed,
    Context,
}

#[derive(Debug, Clone)]
pub(crate) struct ApprovalDiffLine {
    pub text: String,
    pub kind: ApprovalDiffLineKind,
    pub active_hunk: bool,
}

#[derive(Debug, Clone)]
pub(crate) struct ApprovalFileRow {
    pub path: String,
    pub selected: bool,
}

#[derive(Debug, Clone)]
pub(crate) struct ApprovalModalView {
    pub tool_name: String,
    pub call_id: String,
    pub access_label: &'static str,
    pub preview_title: String,
    pub preview_summary: String,
    pub metadata_collapsed: bool,
    pub file_rows: Vec<ApprovalFileRow>,
    pub changed_files: Vec<String>,
    pub diff_mode_label: &'static str,
    pub active_hunk_index: usize,
    pub hunk_total: usize,
    pub diff_label: String,
    pub diff_lines: Vec<ApprovalDiffLine>,
}

#[derive(Debug, Clone)]
pub struct PendingApproval {
    pub call: ToolCall,
    pub spec: ToolSpec,
    pub preview: Option<ToolPreview>,
}

#[derive(Debug, Clone)]
pub struct SessionHistoryEntry {
    pub path: PathBuf,
    pub label: String,
    pub modified_epoch_secs: u64,
    pub bytes: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApprovalDiffMode {
    Full,
    CurrentHunk,
    ChangedOnly,
}

impl ApprovalDiffMode {
    fn next(self) -> Self {
        match self {
            Self::Full => Self::CurrentHunk,
            Self::CurrentHunk => Self::ChangedOnly,
            Self::ChangedOnly => Self::Full,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Full => "full",
            Self::CurrentHunk => "current-hunk",
            Self::ChangedOnly => "changed-only",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionViewMode {
    Provider,
    Audit,
}

impl SessionViewMode {
    fn label(self) -> &'static str {
        match self {
            Self::Provider => "provider",
            Self::Audit => "audit",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ToolPreviewMode {
    Brief,
    Full,
}

impl ToolPreviewMode {
    fn as_str(self) -> &'static str {
        match self {
            Self::Brief => "brief",
            Self::Full => "full",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ThinkingBlockMode {
    Collapsed,
    Expanded,
}

impl ThinkingBlockMode {
    fn as_str(self) -> &'static str {
        match self {
            Self::Collapsed => "collapsed",
            Self::Expanded => "expanded",
        }
    }
}

#[cfg_attr(not(test), allow(dead_code))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ActivityPanelMode {
    Events,
    Sessions,
}

#[derive(Debug, Clone, Copy)]
struct SlashCommandSpec {
    canonical: &'static str,
    aliases: &'static [&'static str],
    description: &'static str,
    completes_with_space: bool,
}

#[derive(Debug, Clone)]
struct ResolvedSlashCommand {
    canonical: &'static str,
    arg: String,
}

#[derive(Debug, Clone)]
struct SlashSelectorEntry {
    fill: String,
    label: String,
    description: String,
    resolved: ResolvedSlashCommand,
}

#[derive(Debug, Clone, Copy)]
struct SlashArgumentOption {
    label: &'static str,
    value: &'static str,
    description: &'static str,
    keywords: &'static [&'static str],
}

const SLASH_COMMANDS: &[SlashCommandSpec] = &[
    SlashCommandSpec {
        canonical: "/compact",
        aliases: &[],
        description: "compact context",
        completes_with_space: false,
    },
    SlashCommandSpec {
        canonical: "/config",
        aliases: &[],
        description: "edit config",
        completes_with_space: false,
    },
    SlashCommandSpec {
        canonical: "/demo",
        aliases: &[],
        description: "demo run",
        completes_with_space: false,
    },
    SlashCommandSpec {
        canonical: "/effort",
        aliases: &["/e"],
        description: "set effort <low|medium|high|max>",
        completes_with_space: true,
    },
    SlashCommandSpec {
        canonical: "/model",
        aliases: &["/m"],
        description: "switch model <flash|pro|id>",
        completes_with_space: true,
    },
    SlashCommandSpec {
        canonical: "/quit",
        aliases: &["/q", "/exit"],
        description: "quit TUI",
        completes_with_space: false,
    },
    SlashCommandSpec {
        canonical: "/resume",
        aliases: &[],
        description: "resume latest or <n>",
        completes_with_space: true,
    },
    SlashCommandSpec {
        canonical: "/sessions",
        aliases: &[],
        description: "list sessions",
        completes_with_space: false,
    },
    SlashCommandSpec {
        canonical: "/tool",
        aliases: &[],
        description: "tool card <latest|next|prev|open|close|toggle>",
        completes_with_space: true,
    },
    SlashCommandSpec {
        canonical: "/tools",
        aliases: &[],
        description: "tool preview <brief|full>",
        completes_with_space: true,
    },
];

const KNOWN_MODEL_IDS: &[&str] = &["deepseek-v4-flash", "deepseek-v4-pro"];

const EFFORT_SELECTOR_OPTIONS: &[SlashArgumentOption] = &[
    SlashArgumentOption {
        label: "low",
        value: "low",
        description: "lighter reasoning",
        keywords: &["low"],
    },
    SlashArgumentOption {
        label: "medium",
        value: "medium",
        description: "balanced default",
        keywords: &["medium", "med"],
    },
    SlashArgumentOption {
        label: "high",
        value: "high",
        description: "deeper reasoning",
        keywords: &["high"],
    },
    SlashArgumentOption {
        label: "max",
        value: "max",
        description: "strongest reasoning",
        keywords: &["max"],
    },
];

const MODEL_SELECTOR_OPTIONS: &[SlashArgumentOption] = &[
    SlashArgumentOption {
        label: "flash",
        value: "deepseek-v4-flash",
        description: "fast default model",
        keywords: &["flash", "v4-flash", "deepseek-v4-flash"],
    },
    SlashArgumentOption {
        label: "pro",
        value: "deepseek-v4-pro",
        description: "stronger reasoning model",
        keywords: &["pro", "v4-pro", "deepseek-v4-pro"],
    },
];

const TOOL_PREVIEW_SELECTOR_OPTIONS: &[SlashArgumentOption] = &[
    SlashArgumentOption {
        label: "brief",
        value: "brief",
        description: "summary only",
        keywords: &["brief", "compact", "summary"],
    },
    SlashArgumentOption {
        label: "full",
        value: "full",
        description: "show full preview body",
        keywords: &["full", "expand", "expanded"],
    },
];

const TOOL_CARD_SELECTOR_OPTIONS: &[SlashArgumentOption] = &[
    SlashArgumentOption {
        label: "latest",
        value: "latest",
        description: "jump to newest tool card",
        keywords: &["latest", "last", "newest"],
    },
    SlashArgumentOption {
        label: "next",
        value: "next",
        description: "select next tool card",
        keywords: &["next", "down", "forward"],
    },
    SlashArgumentOption {
        label: "prev",
        value: "prev",
        description: "select previous tool card",
        keywords: &["prev", "previous", "back"],
    },
    SlashArgumentOption {
        label: "open",
        value: "open",
        description: "expand selected card",
        keywords: &["open", "expand", "show"],
    },
    SlashArgumentOption {
        label: "close",
        value: "close",
        description: "collapse selected card",
        keywords: &["close", "collapse", "hide"],
    },
    SlashArgumentOption {
        label: "toggle",
        value: "toggle",
        description: "toggle selected card",
        keywords: &["toggle", "switch"],
    },
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SetupField {
    TrustCurrentFolder,
    Model,
    ApiKey,
    Save,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ModelPickerTarget {
    Setup,
    Provider,
    ProviderFim,
}

impl ModelPickerTarget {
    fn title(self) -> &'static str {
        match self {
            Self::Setup | Self::Provider => "Model",
            Self::ProviderFim => "FIM Model",
        }
    }

    fn summary(self) -> &'static str {
        match self {
            Self::Setup | Self::Provider => "Choose a known model. Esc to type your own.",
            Self::ProviderFim => "Choose FIM model. Esc to type your own.",
        }
    }
}

#[derive(Debug, Clone)]
struct ModelPickerState {
    target: ModelPickerTarget,
    current: String,
    options: Vec<String>,
    selected: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SecretInputTarget {
    SetupApiKey,
    ConfigProviderApiKey,
}

impl SecretInputTarget {
    fn title(self) -> &'static str {
        match self {
            Self::SetupApiKey | Self::ConfigProviderApiKey => "API Key",
        }
    }

    fn summary(self) -> &'static str {
        match self {
            Self::SetupApiKey => "Saved with setup. TERMQUILL_API_KEY can override at runtime.",
            Self::ConfigProviderApiKey => {
                "Saved on Ctrl-S. TERMQUILL_API_KEY can override at runtime."
            }
        }
    }
}

#[derive(Debug, Clone)]
struct SecretInputState {
    target: SecretInputTarget,
    buffer: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TextInputTarget {
    SetupModel,
    ConfigField(ConfigField),
}

impl TextInputTarget {
    fn title(self) -> &'static str {
        match self {
            Self::SetupModel => "Model ID",
            Self::ConfigField(field) => match field {
                ConfigField::ProviderBaseUrl => "Base URL",
                ConfigField::CompactionSoftThresholdRatio => "Soft Threshold",
                ConfigField::CompactionHardThresholdRatio => "Hard Threshold",
                ConfigField::CompactionContextWindowTokens => "Context Window",
                ConfigField::CompactionTailMessages => "Tail Messages",
                ConfigField::McpName => "MCP Name",
                ConfigField::McpCommand => "MCP Command",
                ConfigField::McpArgsCsv => "MCP Args",
                ConfigField::McpStartupTimeoutSecs => "MCP Timeout",
                _ => "Value",
            },
        }
    }

    fn summary(self) -> &'static str {
        match self {
            Self::SetupModel => "Custom model id.",
            Self::ConfigField(_) => "Edit value.",
        }
    }

    fn prompt_label(self) -> &'static str {
        match self {
            Self::SetupModel => "model",
            Self::ConfigField(field) => field.label(),
        }
    }
}

#[derive(Debug, Clone)]
struct TextInputState {
    target: TextInputTarget,
    buffer: String,
}

#[derive(Debug, Clone)]
enum ModalState {
    ModelPicker(ModelPickerState),
    SecretInput(SecretInputState),
    TextInput(TextInputState),
}

#[derive(Debug, Clone)]
enum ModalOutcome {
    None,
    Dismissed(String),
    ModelSelected {
        target: ModelPickerTarget,
        value: String,
    },
    SecretSubmitted {
        target: SecretInputTarget,
        value: String,
    },
    TextSubmitted {
        target: TextInputTarget,
        value: String,
    },
}

impl SetupField {
    const ORDER: [Self; 4] = [
        Self::TrustCurrentFolder,
        Self::Model,
        Self::ApiKey,
        Self::Save,
    ];

    fn next(self) -> Self {
        let index = Self::ORDER
            .iter()
            .position(|field| *field == self)
            .expect("setup field must exist in the ordered list");
        Self::ORDER[(index + 1) % Self::ORDER.len()]
    }

    fn previous(self) -> Self {
        let index = Self::ORDER
            .iter()
            .position(|field| *field == self)
            .expect("setup field must exist in the ordered list");
        if index == 0 {
            *Self::ORDER.last().expect("setup fields are non-empty")
        } else {
            Self::ORDER[index - 1]
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ConfigSection {
    Provider,
    Permissions,
    Memory,
    Compaction,
    Mcp,
}

impl ConfigSection {
    const FLOW: [Self; 5] = [
        Self::Provider,
        Self::Permissions,
        Self::Memory,
        Self::Compaction,
        Self::Mcp,
    ];

    fn title(self) -> &'static str {
        match self {
            Self::Provider => "Provider",
            Self::Permissions => "Permissions",
            Self::Memory => "Memory",
            Self::Compaction => "Compaction",
            Self::Mcp => "MCP",
        }
    }

    fn summary(self) -> &'static str {
        match self {
            Self::Provider => "provider settings",
            Self::Permissions => "approval rules",
            Self::Memory => "memory status",
            Self::Compaction => "thresholds",
            Self::Mcp => "MCP servers",
        }
    }

    fn next_flow(self) -> Self {
        let index = Self::FLOW
            .iter()
            .position(|section| *section == self)
            .unwrap_or(0);
        Self::FLOW[(index + 1) % Self::FLOW.len()]
    }

    fn previous_flow(self) -> Self {
        let index = Self::FLOW
            .iter()
            .position(|section| *section == self)
            .unwrap_or(0);
        if index == 0 {
            *Self::FLOW
                .last()
                .expect("config flow sections are non-empty")
        } else {
            Self::FLOW[index - 1]
        }
    }

    fn flow_index(self) -> Option<usize> {
        Self::FLOW.iter().position(|section| *section == self)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ConfigField {
    ProviderModel,
    ProviderApiKey,
    ProviderBaseUrl,
    ProviderFimModel,
    PermissionsWriteMode,
    MemoryEnabled,
    CompactionEnabled,
    CompactionSoftThresholdRatio,
    CompactionHardThresholdRatio,
    CompactionContextWindowTokens,
    CompactionTailMessages,
    McpName,
    McpCommand,
    McpArgsCsv,
    McpStartupTimeoutSecs,
}

impl ConfigField {
    const PROVIDER_FIELDS: [Self; 4] = [
        Self::ProviderModel,
        Self::ProviderApiKey,
        Self::ProviderBaseUrl,
        Self::ProviderFimModel,
    ];
    const PERMISSION_FIELDS: [Self; 1] = [Self::PermissionsWriteMode];
    const MEMORY_FIELDS: [Self; 1] = [Self::MemoryEnabled];
    const COMPACTION_FIELDS: [Self; 5] = [
        Self::CompactionEnabled,
        Self::CompactionSoftThresholdRatio,
        Self::CompactionHardThresholdRatio,
        Self::CompactionContextWindowTokens,
        Self::CompactionTailMessages,
    ];
    const MCP_FIELDS: [Self; 4] = [
        Self::McpName,
        Self::McpCommand,
        Self::McpArgsCsv,
        Self::McpStartupTimeoutSecs,
    ];

    fn fields_for_section(section: ConfigSection) -> &'static [Self] {
        match section {
            ConfigSection::Provider => &Self::PROVIDER_FIELDS,
            ConfigSection::Permissions => &Self::PERMISSION_FIELDS,
            ConfigSection::Memory => &Self::MEMORY_FIELDS,
            ConfigSection::Compaction => &Self::COMPACTION_FIELDS,
            ConfigSection::Mcp => &Self::MCP_FIELDS,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::ProviderModel => "model",
            Self::ProviderApiKey => "api_key",
            Self::ProviderBaseUrl => "base_url",
            Self::ProviderFimModel => "fim_model",
            Self::PermissionsWriteMode => "write_mode",
            Self::MemoryEnabled => "enabled",
            Self::CompactionEnabled => "enabled",
            Self::CompactionSoftThresholdRatio => "soft_threshold_ratio",
            Self::CompactionHardThresholdRatio => "hard_threshold_ratio",
            Self::CompactionContextWindowTokens => "context_window_tokens",
            Self::CompactionTailMessages => "tail_messages",
            Self::McpName => "name",
            Self::McpCommand => "command",
            Self::McpArgsCsv => "args_csv",
            Self::McpStartupTimeoutSecs => "startup_timeout_secs",
        }
    }

    fn accepts_text_input(self) -> bool {
        matches!(
            self,
            Self::ProviderModel
                | Self::ProviderBaseUrl
                | Self::ProviderFimModel
                | Self::CompactionSoftThresholdRatio
                | Self::CompactionHardThresholdRatio
                | Self::CompactionContextWindowTokens
                | Self::CompactionTailMessages
                | Self::McpName
                | Self::McpCommand
                | Self::McpArgsCsv
                | Self::McpStartupTimeoutSecs
        )
    }

    fn action_label(self) -> &'static str {
        match self {
            Self::ProviderModel | Self::ProviderFimModel => "Enter choose",
            Self::ProviderApiKey => "Enter input",
            Self::PermissionsWriteMode => "Enter cycle",
            Self::MemoryEnabled | Self::CompactionEnabled => "Enter toggle",
            _ if self.accepts_text_input() => "Enter input",
            _ => "",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ConfigFooterAction {
    Save,
    SaveAndClose,
    Close,
}

impl ConfigFooterAction {
    const ORDER: [Self; 3] = [Self::Save, Self::SaveAndClose, Self::Close];

    fn button_label(self) -> &'static str {
        match self {
            Self::Save => "save",
            Self::SaveAndClose => "save+close",
            Self::Close => "close",
        }
    }

    fn field_label(self) -> &'static str {
        match self {
            Self::Save => "save",
            Self::SaveAndClose => "save_and_close",
            Self::Close => "close",
        }
    }

    fn next(self) -> Self {
        let index = Self::ORDER
            .iter()
            .position(|action| *action == self)
            .expect("footer action must exist in order");
        Self::ORDER[(index + 1) % Self::ORDER.len()]
    }

    fn previous(self) -> Self {
        let index = Self::ORDER
            .iter()
            .position(|action| *action == self)
            .expect("footer action must exist in order");
        if index == 0 {
            *Self::ORDER.last().expect("footer actions are non-empty")
        } else {
            Self::ORDER[index - 1]
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ConfigFieldMove {
    Moved,
    Boundary,
    Unavailable,
}

#[derive(Debug, Clone)]
struct McpServerDraft {
    name: String,
    command: String,
    args_csv: String,
    startup_timeout_secs: String,
}

impl McpServerDraft {
    fn from_config(config: &McpServerConfig) -> Self {
        Self {
            name: config.name.clone(),
            command: config.command.clone(),
            args_csv: config.args.join(", "),
            startup_timeout_secs: config.startup_timeout_secs.to_string(),
        }
    }

    fn to_config(&self, index: usize) -> Result<McpServerConfig> {
        let name = self.name.trim();
        if name.is_empty() {
            bail!("mcp server {} name cannot be empty", index + 1);
        }
        let command = self.command.trim();
        if command.is_empty() {
            bail!("mcp server {} command cannot be empty", index + 1);
        }
        let startup_timeout_secs =
            self.startup_timeout_secs
                .trim()
                .parse::<u64>()
                .map_err(|error| {
                    anyhow!(
                        "mcp server {} startup_timeout_secs must be a positive integer: {error}",
                        index + 1
                    )
                })?;
        if startup_timeout_secs == 0 {
            bail!(
                "mcp server {} startup_timeout_secs must be greater than 0",
                index + 1
            );
        }

        Ok(McpServerConfig {
            name: name.to_owned(),
            command: command.to_owned(),
            args: self
                .args_csv
                .split(',')
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(ToOwned::to_owned)
                .collect(),
            startup_timeout_secs,
        })
    }
}

#[derive(Debug, Clone)]
struct ConfigDraft {
    base_root_config: RootConfig,
    provider_model: String,
    provider_api_key: String,
    provider_base_url: String,
    provider_beta_base_url: String,
    provider_anthropic_base_url: String,
    provider_user_id_strategy: String,
    provider_strict_tools_mode: StrictToolsMode,
    provider_fim_model: String,
    provider_request_timeout_secs: String,
    permission_write_mode: ApprovalMode,
    memory_enabled: bool,
    compaction_enabled: bool,
    compaction_soft_threshold_ratio: String,
    compaction_hard_threshold_ratio: String,
    compaction_context_window_tokens: String,
    compaction_tail_messages: String,
    mcp_servers: Vec<McpServerDraft>,
}

impl ConfigDraft {
    fn from_root_config(root_config: &RootConfig) -> Self {
        let provider = load_deepseek_provider_config(root_config)
            .unwrap_or_else(|| default_deepseek_provider_config(&root_config.agent.model));
        Self {
            base_root_config: root_config.clone(),
            provider_model: provider.model,
            provider_api_key: provider.api_key.unwrap_or_default(),
            provider_base_url: provider.base_url,
            provider_beta_base_url: provider.beta_base_url,
            provider_anthropic_base_url: provider.anthropic_base_url,
            provider_user_id_strategy: provider.user_id_strategy.unwrap_or_default(),
            provider_strict_tools_mode: provider.strict_tools_mode,
            provider_fim_model: provider.fim_model,
            provider_request_timeout_secs: provider.request_timeout_secs.to_string(),
            permission_write_mode: root_config.permission.write_mode,
            memory_enabled: root_config.memory.enabled,
            compaction_enabled: root_config.compaction.enabled,
            compaction_soft_threshold_ratio: root_config
                .compaction
                .soft_threshold_ratio
                .to_string(),
            compaction_hard_threshold_ratio: root_config
                .compaction
                .hard_threshold_ratio
                .to_string(),
            compaction_context_window_tokens: root_config
                .compaction
                .context_window_tokens
                .map(|value| value.to_string())
                .unwrap_or_default(),
            compaction_tail_messages: root_config.compaction.tail_messages.to_string(),
            mcp_servers: root_config
                .mcp_servers
                .iter()
                .map(McpServerDraft::from_config)
                .collect(),
        }
    }

    fn to_root_config(&self) -> Result<RootConfig> {
        let model = self.provider_model.trim();
        if model.is_empty() {
            bail!("model cannot be empty");
        }
        let api_key = self.provider_api_key.trim();
        let base_url = self.provider_base_url.trim();
        if base_url.is_empty() {
            bail!("base_url cannot be empty");
        }
        let beta_base_url = self.provider_beta_base_url.trim();
        if beta_base_url.is_empty() {
            bail!("beta_base_url cannot be empty");
        }
        let anthropic_base_url = self.provider_anthropic_base_url.trim();
        if anthropic_base_url.is_empty() {
            bail!("anthropic_base_url cannot be empty");
        }
        let fim_model = self.provider_fim_model.trim();
        if fim_model.is_empty() {
            bail!("fim_model cannot be empty");
        }

        let request_timeout_secs = self
            .provider_request_timeout_secs
            .trim()
            .parse::<u64>()
            .map_err(|error| anyhow!("request_timeout_secs must be a positive integer: {error}"))?;
        if request_timeout_secs == 0 {
            bail!("request_timeout_secs must be greater than 0");
        }

        let soft_threshold_ratio = self
            .compaction_soft_threshold_ratio
            .trim()
            .parse::<f32>()
            .map_err(|error| anyhow!("soft_threshold_ratio must be a decimal number: {error}"))?;
        let hard_threshold_ratio = self
            .compaction_hard_threshold_ratio
            .trim()
            .parse::<f32>()
            .map_err(|error| anyhow!("hard_threshold_ratio must be a decimal number: {error}"))?;
        if !(0.0..=1.0).contains(&soft_threshold_ratio) {
            bail!("soft_threshold_ratio must be between 0.0 and 1.0");
        }
        if !(0.0..=1.0).contains(&hard_threshold_ratio) {
            bail!("hard_threshold_ratio must be between 0.0 and 1.0");
        }
        if hard_threshold_ratio < soft_threshold_ratio {
            bail!("hard_threshold_ratio must be greater than or equal to soft_threshold_ratio");
        }

        let context_window_tokens = if self.compaction_context_window_tokens.trim().is_empty() {
            None
        } else {
            let parsed = self
                .compaction_context_window_tokens
                .trim()
                .parse::<u32>()
                .map_err(|error| {
                    anyhow!("context_window_tokens must be a positive integer: {error}")
                })?;
            if parsed == 0 {
                bail!("context_window_tokens must be greater than 0");
            }
            Some(parsed)
        };

        let tail_messages = self
            .compaction_tail_messages
            .trim()
            .parse::<usize>()
            .map_err(|error| anyhow!("tail_messages must be a positive integer: {error}"))?;
        if tail_messages == 0 {
            bail!("tail_messages must be greater than 0");
        }

        let mut root_config = self.base_root_config.clone();
        root_config.agent.model = model.to_owned();
        root_config.permission.write_mode = self.permission_write_mode;
        root_config.memory.enabled = self.memory_enabled;
        root_config.compaction.enabled = self.compaction_enabled;
        root_config.compaction.soft_threshold_ratio = soft_threshold_ratio;
        root_config.compaction.hard_threshold_ratio = hard_threshold_ratio;
        root_config.compaction.context_window_tokens = context_window_tokens;
        root_config.compaction.tail_messages = tail_messages;
        root_config.mcp_servers = self
            .mcp_servers
            .iter()
            .enumerate()
            .map(|(index, server)| server.to_config(index))
            .collect::<Result<Vec<_>>>()?;

        let mut provider_config = load_deepseek_provider_config(&root_config)
            .unwrap_or_else(|| default_deepseek_provider_config(model));
        provider_config.model = model.to_owned();
        provider_config.api_key = (!api_key.is_empty()).then(|| api_key.to_owned());
        provider_config.base_url = base_url.to_owned();
        provider_config.beta_base_url = beta_base_url.to_owned();
        provider_config.anthropic_base_url = anthropic_base_url.to_owned();
        provider_config.user_id_strategy = (!self.provider_user_id_strategy.trim().is_empty())
            .then(|| self.provider_user_id_strategy.trim().to_owned());
        provider_config.strict_tools_mode = self.provider_strict_tools_mode;
        provider_config.fim_model = fim_model.to_owned();
        provider_config.request_timeout_secs = request_timeout_secs;

        let provider_value = serialize_deepseek_provider_value(&provider_config)?;
        root_config
            .providers
            .insert("deepseek".to_owned(), provider_value);
        Ok(root_config)
    }
}

#[derive(Debug, Clone)]
struct ConfigState {
    selected_section: ConfigSection,
    selected_field: Option<ConfigField>,
    footer_selected: bool,
    selected_footer_action: ConfigFooterAction,
    selected_mcp_server_index: usize,
    draft: ConfigDraft,
    dirty: bool,
    close_guard_armed: bool,
}

impl ConfigState {
    fn from_root_config(root_config: &RootConfig) -> Self {
        let selected_section = ConfigSection::Provider;
        Self {
            selected_section,
            selected_field: ConfigField::fields_for_section(selected_section)
                .first()
                .copied(),
            footer_selected: false,
            selected_footer_action: ConfigFooterAction::Save,
            selected_mcp_server_index: 0,
            draft: ConfigDraft::from_root_config(root_config),
            dirty: false,
            close_guard_armed: false,
        }
    }

    fn set_section(&mut self, section: ConfigSection) {
        self.selected_section = section;
        self.sync_mcp_selection();
        self.footer_selected = false;
        self.selected_field = self.first_field_for_section(section);
    }

    fn first_field_for_section(&self, section: ConfigSection) -> Option<ConfigField> {
        if section == ConfigSection::Mcp && self.draft.mcp_servers.is_empty() {
            None
        } else {
            ConfigField::fields_for_section(section).first().copied()
        }
    }

    fn last_field_for_current_section(&self) -> Option<ConfigField> {
        if self.selected_section == ConfigSection::Mcp && self.draft.mcp_servers.is_empty() {
            None
        } else {
            ConfigField::fields_for_section(self.selected_section)
                .last()
                .copied()
        }
    }

    fn move_field(&mut self, forward: bool) -> ConfigFieldMove {
        if self.selected_section == ConfigSection::Mcp && self.draft.mcp_servers.is_empty() {
            return ConfigFieldMove::Unavailable;
        }
        let fields = ConfigField::fields_for_section(self.selected_section);
        if fields.is_empty() {
            return ConfigFieldMove::Unavailable;
        }

        let current_index = self
            .selected_field
            .and_then(|field| fields.iter().position(|candidate| *candidate == field))
            .unwrap_or(0);
        let next_index = if forward {
            if current_index + 1 >= fields.len() {
                return ConfigFieldMove::Boundary;
            }
            current_index + 1
        } else {
            if current_index == 0 {
                return ConfigFieldMove::Boundary;
            }
            current_index - 1
        };
        self.selected_field = Some(fields[next_index]);
        self.footer_selected = false;
        ConfigFieldMove::Moved
    }

    fn focus_footer(&mut self, action: ConfigFooterAction) {
        self.footer_selected = true;
        self.selected_footer_action = action;
    }

    fn focus_last_field(&mut self) -> bool {
        let Some(field) = self.last_field_for_current_section() else {
            return false;
        };
        self.selected_field = Some(field);
        self.footer_selected = false;
        true
    }

    fn move_footer_action(&mut self, forward: bool) {
        self.footer_selected = true;
        self.selected_footer_action = if forward {
            self.selected_footer_action.next()
        } else {
            self.selected_footer_action.previous()
        };
    }

    fn sync_mcp_selection(&mut self) {
        if self.draft.mcp_servers.is_empty() {
            self.selected_mcp_server_index = 0;
            if self.selected_section == ConfigSection::Mcp {
                self.selected_field = None;
            }
            return;
        }
        self.selected_mcp_server_index = self
            .selected_mcp_server_index
            .min(self.draft.mcp_servers.len().saturating_sub(1));
    }

    fn selected_mcp_server(&self) -> Option<&McpServerDraft> {
        self.draft.mcp_servers.get(self.selected_mcp_server_index)
    }

    fn selected_mcp_server_mut(&mut self) -> Option<&mut McpServerDraft> {
        self.draft
            .mcp_servers
            .get_mut(self.selected_mcp_server_index)
    }

    fn editing_field(&self) -> Option<ConfigField> {
        None
    }

    fn add_mcp_server(&mut self) {
        let next_index = self.draft.mcp_servers.len() + 1;
        self.draft.mcp_servers.push(McpServerDraft {
            name: format!("server-{next_index}"),
            command: "npx".to_owned(),
            args_csv: String::new(),
            startup_timeout_secs: "10".to_owned(),
        });
        self.selected_mcp_server_index = self.draft.mcp_servers.len() - 1;
        if self.selected_section == ConfigSection::Mcp {
            self.footer_selected = false;
            self.selected_field = Some(ConfigField::McpName);
        }
        self.dirty = true;
    }

    fn remove_selected_mcp_server(&mut self) -> bool {
        if self.draft.mcp_servers.is_empty() {
            return false;
        }
        self.draft
            .mcp_servers
            .remove(self.selected_mcp_server_index);
        self.sync_mcp_selection();
        if self.selected_section == ConfigSection::Mcp && self.draft.mcp_servers.is_empty() {
            self.selected_field = None;
        }
        self.dirty = true;
        true
    }

    fn cycle_mcp_server(&mut self, forward: bool) -> bool {
        if self.draft.mcp_servers.is_empty() {
            return false;
        }
        let len = self.draft.mcp_servers.len();
        if forward {
            self.selected_mcp_server_index = (self.selected_mcp_server_index + 1) % len;
        } else if self.selected_mcp_server_index == 0 {
            self.selected_mcp_server_index = len - 1;
        } else {
            self.selected_mcp_server_index -= 1;
        }
        true
    }

    fn field_text_value(&self, field: ConfigField) -> Option<&str> {
        match field {
            ConfigField::ProviderModel => Some(&self.draft.provider_model),
            ConfigField::ProviderApiKey => Some(&self.draft.provider_api_key),
            ConfigField::ProviderBaseUrl => Some(&self.draft.provider_base_url),
            ConfigField::ProviderFimModel => Some(&self.draft.provider_fim_model),
            ConfigField::CompactionSoftThresholdRatio => {
                Some(&self.draft.compaction_soft_threshold_ratio)
            }
            ConfigField::CompactionHardThresholdRatio => {
                Some(&self.draft.compaction_hard_threshold_ratio)
            }
            ConfigField::CompactionContextWindowTokens => {
                Some(&self.draft.compaction_context_window_tokens)
            }
            ConfigField::CompactionTailMessages => Some(&self.draft.compaction_tail_messages),
            ConfigField::McpName => self
                .selected_mcp_server()
                .map(|server| server.name.as_str()),
            ConfigField::McpCommand => self
                .selected_mcp_server()
                .map(|server| server.command.as_str()),
            ConfigField::McpArgsCsv => self
                .selected_mcp_server()
                .map(|server| server.args_csv.as_str()),
            ConfigField::McpStartupTimeoutSecs => self
                .selected_mcp_server()
                .map(|server| server.startup_timeout_secs.as_str()),
            ConfigField::PermissionsWriteMode
            | ConfigField::MemoryEnabled
            | ConfigField::CompactionEnabled => None,
        }
    }

    fn field_text_value_mut(&mut self, field: ConfigField) -> Option<&mut String> {
        match field {
            ConfigField::ProviderModel => Some(&mut self.draft.provider_model),
            ConfigField::ProviderApiKey => Some(&mut self.draft.provider_api_key),
            ConfigField::ProviderBaseUrl => Some(&mut self.draft.provider_base_url),
            ConfigField::ProviderFimModel => Some(&mut self.draft.provider_fim_model),
            ConfigField::CompactionSoftThresholdRatio => {
                Some(&mut self.draft.compaction_soft_threshold_ratio)
            }
            ConfigField::CompactionHardThresholdRatio => {
                Some(&mut self.draft.compaction_hard_threshold_ratio)
            }
            ConfigField::CompactionContextWindowTokens => {
                Some(&mut self.draft.compaction_context_window_tokens)
            }
            ConfigField::CompactionTailMessages => Some(&mut self.draft.compaction_tail_messages),
            ConfigField::McpName => self
                .selected_mcp_server_mut()
                .map(|server| &mut server.name),
            ConfigField::McpCommand => self
                .selected_mcp_server_mut()
                .map(|server| &mut server.command),
            ConfigField::McpArgsCsv => self
                .selected_mcp_server_mut()
                .map(|server| &mut server.args_csv),
            ConfigField::McpStartupTimeoutSecs => self
                .selected_mcp_server_mut()
                .map(|server| &mut server.startup_timeout_secs),
            ConfigField::PermissionsWriteMode
            | ConfigField::MemoryEnabled
            | ConfigField::CompactionEnabled => None,
        }
    }

    fn display_value(&self, field: ConfigField) -> String {
        let text_value = match field {
            ConfigField::ProviderApiKey => return mask_secret(&self.draft.provider_api_key),
            ConfigField::PermissionsWriteMode => {
                return self.draft.permission_write_mode.as_str().to_owned();
            }
            ConfigField::MemoryEnabled => {
                return setup_bool_label(self.draft.memory_enabled).to_owned();
            }
            ConfigField::CompactionEnabled => {
                return setup_bool_label(self.draft.compaction_enabled).to_owned();
            }
            _ => self.field_text_value(field).unwrap_or_default(),
        };

        match field {
            ConfigField::McpArgsCsv if text_value.trim().is_empty() => "<empty>".to_owned(),
            ConfigField::CompactionContextWindowTokens if text_value.trim().is_empty() => {
                "<empty = n/a>".to_owned()
            }
            _ => text_value.to_owned(),
        }
    }
}

#[derive(Debug, Clone)]
struct SetupState {
    config_path: PathBuf,
    selected_field: SetupField,
    model: String,
    api_key: String,
    trusted_current_folder: bool,
    startup_error: Option<String>,
}

impl SetupState {
    fn new(config_path: PathBuf, startup_error: Option<String>) -> Self {
        Self {
            config_path,
            selected_field: SetupField::TrustCurrentFolder,
            model: "deepseek-v4-flash".to_owned(),
            api_key: String::new(),
            trusted_current_folder: false,
            startup_error,
        }
    }

    fn masked_api_key(&self) -> String {
        if self.api_key.is_empty() {
            "<empty>".to_owned()
        } else {
            "*".repeat(self.api_key.chars().count().max(8))
        }
    }

    fn auth_summary(&self) -> String {
        if !self.api_key.trim().is_empty() {
            return "inline api_key pending save".to_owned();
        }
        if env::var(TERMQUILL_API_KEY_ENV).is_ok() {
            return format!("env {TERMQUILL_API_KEY_ENV}");
        }

        "missing".to_owned()
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
    activity_panel_mode: ActivityPanelMode,
    current_session_entries: Vec<SessionLogEntry>,
    latest_compaction_record: Option<CompactionRecord>,
    compaction_config: CompactionConfig,
    memory_config: MemoryConfig,
    tool_preview_mode: ToolPreviewMode,
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
            activity_panel_mode: ActivityPanelMode::Events,
            current_session_entries: Vec::new(),
            latest_compaction_record: None,
            compaction_config: root_config.compaction.clone(),
            memory_config: root_config.memory.clone(),
            tool_preview_mode: ToolPreviewMode::Brief,
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
            activity_panel_mode: ActivityPanelMode::Events,
            current_session_entries: Vec::new(),
            latest_compaction_record: None,
            compaction_config: CompactionConfig::default(),
            memory_config: MemoryConfig::default(),
            tool_preview_mode: ToolPreviewMode::Brief,
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
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                if self.is_busy {
                    self.last_notice = Some("cancellation requested".to_owned());
                    self.push_timeline(TimelineRole::Notice, "cancel requested");
                    return Ok(Some(AppAction::CancelRun));
                }
                self.should_quit = true;
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

    fn slash_query(prompt: &str) -> Option<(&str, String)> {
        let trimmed = prompt.trim_start();
        if !trimmed.starts_with('/') {
            return None;
        }

        Some(Self::command_token_and_arg(trimmed))
    }

    fn command_token_and_arg(prompt: &str) -> (&str, String) {
        if let Some((token, arg)) = prompt.split_once(char::is_whitespace) {
            return (token, arg.trim().to_owned());
        }

        (prompt, String::new())
    }

    fn slash_command_matches(token: &str) -> Vec<&'static SlashCommandSpec> {
        if token == "/" || token.is_empty() {
            return SLASH_COMMANDS.iter().collect();
        }

        SLASH_COMMANDS
            .iter()
            .filter(|spec| {
                spec.canonical.starts_with(token)
                    || spec.aliases.iter().any(|alias| alias.starts_with(token))
            })
            .collect()
    }

    fn exact_slash_command(token: &str) -> Option<&'static SlashCommandSpec> {
        SLASH_COMMANDS
            .iter()
            .find(|spec| spec.canonical == token || spec.aliases.contains(&token))
    }

    fn slash_option_matches(option: &SlashArgumentOption, query: &str) -> bool {
        if query.is_empty() {
            return true;
        }

        option
            .keywords
            .iter()
            .any(|keyword| keyword.starts_with(query))
    }

    fn slash_command_entries(token: &str, arg: &str) -> Vec<SlashSelectorEntry> {
        Self::slash_command_matches(token)
            .into_iter()
            .map(|spec| {
                let aliases = if spec.aliases.is_empty() {
                    String::new()
                } else {
                    format!("  alias: {}", spec.aliases.join(", "))
                };
                let fill = if arg.is_empty() {
                    let suffix = if spec.completes_with_space { " " } else { "" };
                    format!("{}{}", spec.canonical, suffix)
                } else {
                    format!("{} {arg}", spec.canonical)
                };
                SlashSelectorEntry {
                    fill,
                    label: spec.canonical.to_owned(),
                    description: format!("{}{}", spec.description, aliases),
                    resolved: ResolvedSlashCommand {
                        canonical: spec.canonical,
                        arg: arg.to_owned(),
                    },
                }
            })
            .collect()
    }

    fn slash_argument_entries(
        &self,
        spec: &SlashCommandSpec,
        arg: &str,
    ) -> Option<Vec<SlashSelectorEntry>> {
        match spec.canonical {
            "/effort" => Some(self.effort_selector_entries(arg)),
            "/model" => Some(self.model_selector_entries(arg)),
            "/tool" => Some(self.tool_card_selector_entries(arg)),
            "/tools" => Some(self.tool_preview_selector_entries(arg)),
            _ => None,
        }
    }

    fn effort_selector_entries(&self, arg: &str) -> Vec<SlashSelectorEntry> {
        let query = arg.trim().to_ascii_lowercase();
        let current = self.reasoning_effort.as_str();
        let mut options = EFFORT_SELECTOR_OPTIONS
            .iter()
            .copied()
            .filter(|option| Self::slash_option_matches(option, &query))
            .collect::<Vec<_>>();
        options.sort_by_key(|option| option.value != current);

        options
            .into_iter()
            .map(|option| SlashSelectorEntry {
                fill: format!("/effort {}", option.value),
                label: option.label.to_owned(),
                description: format!("{}  {}", option.value, option.description),
                resolved: ResolvedSlashCommand {
                    canonical: "/effort",
                    arg: option.value.to_owned(),
                },
            })
            .collect()
    }

    fn model_selector_entries(&self, arg: &str) -> Vec<SlashSelectorEntry> {
        let trimmed = arg.trim();
        let query = trimmed.to_ascii_lowercase();
        let current = self.model_name.as_str();
        let current_is_known = MODEL_SELECTOR_OPTIONS
            .iter()
            .any(|option| option.value == current);
        let mut entries = Vec::new();

        if !current_is_known
            && (query.is_empty() || current.to_ascii_lowercase().starts_with(&query))
        {
            entries.push(SlashSelectorEntry {
                fill: format!("/model {current}"),
                label: "current".to_owned(),
                description: format!("{current}  current custom model"),
                resolved: ResolvedSlashCommand {
                    canonical: "/model",
                    arg: current.to_owned(),
                },
            });
        }

        let mut options = MODEL_SELECTOR_OPTIONS
            .iter()
            .copied()
            .filter(|option| Self::slash_option_matches(option, &query))
            .collect::<Vec<_>>();
        options.sort_by_key(|option| option.value != current);

        entries.extend(options.into_iter().map(|option| SlashSelectorEntry {
            fill: format!("/model {}", option.value),
            label: option.label.to_owned(),
            description: format!("{}  {}", option.value, option.description),
            resolved: ResolvedSlashCommand {
                canonical: "/model",
                arg: option.value.to_owned(),
            },
        }));

        if entries.is_empty() && !trimmed.is_empty() {
            let custom = trimmed.to_owned();
            entries.push(SlashSelectorEntry {
                fill: format!("/model {custom}"),
                label: "custom".to_owned(),
                description: format!("{custom}  use typed model id"),
                resolved: ResolvedSlashCommand {
                    canonical: "/model",
                    arg: custom,
                },
            });
        }

        entries
    }

    fn tool_preview_selector_entries(&self, arg: &str) -> Vec<SlashSelectorEntry> {
        let query = arg.trim().to_ascii_lowercase();
        let current = self.tool_preview_mode.as_str();
        let mut options = TOOL_PREVIEW_SELECTOR_OPTIONS
            .iter()
            .copied()
            .filter(|option| Self::slash_option_matches(option, &query))
            .collect::<Vec<_>>();
        options.sort_by_key(|option| option.value != current);

        options
            .into_iter()
            .map(|option| SlashSelectorEntry {
                fill: format!("/tools {}", option.value),
                label: option.label.to_owned(),
                description: format!("{}  {}", option.value, option.description),
                resolved: ResolvedSlashCommand {
                    canonical: "/tools",
                    arg: option.value.to_owned(),
                },
            })
            .collect()
    }

    fn tool_card_selector_entries(&self, arg: &str) -> Vec<SlashSelectorEntry> {
        let query = arg.trim().to_ascii_lowercase();
        TOOL_CARD_SELECTOR_OPTIONS
            .iter()
            .copied()
            .filter(|option| Self::slash_option_matches(option, &query))
            .map(|option| SlashSelectorEntry {
                fill: format!("/tool {}", option.value),
                label: option.label.to_owned(),
                description: format!("{}  {}", option.value, option.description),
                resolved: ResolvedSlashCommand {
                    canonical: "/tool",
                    arg: option.value.to_owned(),
                },
            })
            .collect()
    }

    fn slash_selector_entries(&self) -> Vec<SlashSelectorEntry> {
        let Some((token, arg)) = Self::slash_query(&self.input) else {
            return Vec::new();
        };

        if let Some(spec) = Self::exact_slash_command(token)
            && let Some(entries) = self.slash_argument_entries(spec, &arg)
        {
            return entries;
        }

        Self::slash_command_entries(token, &arg)
    }

    fn selected_slash_entry(&self) -> Option<SlashSelectorEntry> {
        let rows = self.slash_selector_entries();
        if rows.is_empty() {
            return None;
        }

        let index = self.slash_selector_index.min(rows.len().saturating_sub(1));
        rows.get(index).cloned()
    }

    fn resolve_slash_command(&self, prompt: &str) -> Option<ResolvedSlashCommand> {
        let (token, arg) = Self::slash_query(prompt)?;
        if let Some(entry) = self.selected_slash_entry() {
            return Some(entry.resolved);
        }

        Self::exact_slash_command(token).map(|spec| ResolvedSlashCommand {
            canonical: spec.canonical,
            arg,
        })
    }

    fn reset_slash_selector(&mut self) {
        self.slash_selector_index = 0;
    }

    fn move_slash_selector(&mut self, forward: bool) {
        let rows = self.slash_selector_entries();
        if rows.is_empty() {
            return;
        }

        if forward {
            self.slash_selector_index = (self.slash_selector_index + 1) % rows.len();
        } else if self.slash_selector_index == 0 {
            self.slash_selector_index = rows.len() - 1;
        } else {
            self.slash_selector_index -= 1;
        }

        if let Some(entry) = rows.get(self.slash_selector_index) {
            self.last_notice = Some(format!("slash selected {}", entry.label));
        }
    }

    fn accept_slash_selector(&mut self) {
        let Some(entry) = self.selected_slash_entry() else {
            return;
        };
        let trimmed = self.input.trim_start();
        let leading_len = self.input.len().saturating_sub(trimmed.len());
        let leading = self.input[..leading_len].to_owned();
        let completed = format!("{leading}{}", entry.fill);
        self.set_input_and_cursor(completed);
        self.last_notice = Some(format!("slash completed to {}", entry.fill.trim_end()));
        self.reset_input_history_navigation();
        self.reset_slash_selector();
    }

    fn should_accept_slash_selector_on_enter(&self) -> bool {
        let Some(entry) = self.selected_slash_entry() else {
            return false;
        };

        entry.label.starts_with('/') && self.input.trim_start() != entry.fill.trim_end()
    }

    pub fn has_slash_selector(&self) -> bool {
        Self::slash_query(&self.input).is_some()
    }

    pub fn slash_selector_selected_index(&self) -> Option<usize> {
        let rows = self.slash_selector_entries();
        if rows.is_empty() {
            None
        } else {
            Some(self.slash_selector_index.min(rows.len().saturating_sub(1)))
        }
    }

    pub fn slash_selector_rows(&self) -> Vec<(String, String)> {
        self.slash_selector_entries()
            .into_iter()
            .map(|entry| (entry.label, entry.description))
            .collect()
    }

    pub fn slash_selector_empty_message(&self) -> Option<&'static str> {
        let (token, _) = Self::slash_query(&self.input)?;
        if !self.slash_selector_entries().is_empty() {
            return None;
        }

        match Self::exact_slash_command(token).map(|spec| spec.canonical) {
            Some("/effort") => Some("pick effort: low | medium | high | max"),
            Some("/tool") => Some("pick tool card: latest | next | prev | open | close | toggle"),
            Some("/tools") => Some("pick tool preview: brief | full"),
            _ => Some("no slash match"),
        }
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

    pub fn input_cursor_visual_position(&self) -> (u16, u16) {
        let width = self.composer_wrap_width();
        let (row, column) = self.visual_position_for_cursor(self.input_cursor, width);
        (column as u16, row as u16)
    }

    pub(crate) fn composer_input_rows(&self) -> u16 {
        self.input_last_visual_row().saturating_add(1) as u16
    }

    pub(crate) fn slash_selector_visible_rows(&self) -> u16 {
        if self.has_slash_selector() {
            self.slash_selector_rows().len().clamp(1, 8) as u16
        } else {
            0
        }
    }

    pub fn composer_height(&self) -> u16 {
        self.composer_input_rows().saturating_add(3).max(4)
    }

    pub fn restore_latest_session_from_disk(&mut self, root_config: &RootConfig) -> bool {
        self.refresh_session_history();
        let Some(session_log_path) = self.session_history.first().map(|entry| entry.path.clone())
        else {
            return false;
        };

        let Ok(store) = JsonlSessionStore::new(&session_log_path) else {
            return false;
        };
        let Ok(session) = Session::load_from_store(
            root_config.agent.provider.clone(),
            root_config.agent.model.clone(),
            store,
        ) else {
            return false;
        };

        let provider_name = session.provider_name().to_owned();
        let model_name = session.model_name().to_owned();
        let entries = session.entries().to_vec();
        self.restore_session_view(
            session_log_path,
            provider_name,
            model_name,
            entries,
            "restored latest session",
        );
        self.last_notice = Some("restored latest session".to_owned());
        self.refresh_session_history();
        true
    }

    pub fn slash_command_hints(&self) -> Vec<String> {
        let mut hints = self
            .slash_selector_rows()
            .into_iter()
            .map(|(command, description)| format!("{command} - {description}"))
            .collect::<Vec<_>>();
        if hints.is_empty()
            && let Some(message) = self.slash_selector_empty_message()
        {
            hints.push(message.to_owned());
        }
        hints
    }

    fn composer_wrap_width(&self) -> usize {
        let total_width = self.terminal_width.max(24) as usize;
        let sidebar_width = sidebar_width_for_terminal(total_width);
        let composer_width = total_width.saturating_sub(sidebar_width).max(12);
        composer_width.saturating_sub(8).max(1)
    }

    pub(crate) fn footer_strip_height(&self) -> u16 {
        let desired = self.composer_height();
        desired.min(self.terminal_height.saturating_sub(2).max(4))
    }

    fn live_panel_height(&self) -> u16 {
        self.terminal_height
            .saturating_sub(self.footer_strip_height())
            .saturating_sub(1)
            .max(1)
    }

    fn timeline_viewport_rows(&self) -> usize {
        self.live_panel_height()
            .saturating_sub(u16::from(self.live_activity_summary().is_some()))
            .max(1) as usize
    }

    fn max_timeline_scroll_back(&self) -> usize {
        let total = self.effective_timeline_render_len();
        let viewport = self.timeline_viewport_rows().max(1);
        total.saturating_sub(viewport)
    }

    fn effective_timeline_render_len(&self) -> usize {
        self.timeline_render_cache
            .iter()
            .rposition(line_has_visible_content)
            .map(|index| index + 1)
            .unwrap_or(0)
    }

    fn scrollback_cutoff_line(&self) -> usize {
        let cutoff_entry = match self.streaming_assistant_index {
            Some(index) if index + 1 == self.timeline.len() && self.is_busy => index,
            _ => self.timeline.len(),
        };
        if cutoff_entry == 0 {
            0
        } else {
            self.timeline_render_ranges
                .get(cutoff_entry - 1)
                .map(|range| range.end)
                .unwrap_or(self.timeline_render_cache.len())
        }
    }

    fn transcript_page_step(&self) -> usize {
        (self.timeline_viewport_rows() / 2).max(1)
    }

    fn input_char_len(&self) -> usize {
        self.input.chars().count()
    }

    fn set_input_and_cursor(&mut self, input: String) {
        self.input = input;
        self.input_cursor = self.input_char_len();
    }

    fn clamp_input_cursor(&mut self) {
        self.input_cursor = self.input_cursor.min(self.input_char_len());
    }

    fn input_last_visual_row(&self) -> usize {
        self.visual_position_for_cursor(self.input_char_len(), self.composer_wrap_width())
            .0
    }

    fn input_cursor_visual_row(&self) -> usize {
        self.visual_position_for_cursor(self.input_cursor, self.composer_wrap_width())
            .0
    }

    fn insert_input_character(&mut self, character: char) {
        let byte_index = char_to_byte_index(&self.input, self.input_cursor);
        self.input.insert(byte_index, character);
        self.input_cursor += 1;
    }

    fn remove_input_character_before_cursor(&mut self) {
        if self.input_cursor == 0 {
            return;
        }
        let end = char_to_byte_index(&self.input, self.input_cursor);
        let start = char_to_byte_index(&self.input, self.input_cursor - 1);
        self.input.replace_range(start..end, "");
        self.input_cursor -= 1;
    }

    fn move_input_cursor_left(&mut self) {
        self.input_cursor = self.input_cursor.saturating_sub(1);
    }

    fn move_input_cursor_right(&mut self) {
        self.input_cursor = (self.input_cursor + 1).min(self.input_char_len());
    }

    fn move_input_cursor_home(&mut self) {
        self.input_cursor = 0;
    }

    fn move_input_cursor_end(&mut self) {
        self.input_cursor = self.input_char_len();
    }

    fn move_input_cursor_vertical(&mut self, up: bool) -> bool {
        let width = self.composer_wrap_width();
        let (row, column) = self.visual_position_for_cursor(self.input_cursor, width);
        if up {
            if row == 0 {
                return false;
            }
            self.input_cursor = self.cursor_for_visual_position(row - 1, column, width);
            return true;
        }

        let next = self.cursor_for_visual_position(row + 1, column, width);
        if next == self.input_cursor {
            return false;
        }
        self.input_cursor = next;
        true
    }

    fn visual_position_for_cursor(&self, cursor: usize, width: usize) -> (usize, usize) {
        let width = width.max(1);
        let mut row = 0usize;
        let mut column = 0usize;
        for (index, character) in self.input.chars().enumerate() {
            if index == cursor {
                break;
            }
            if character == '\n' {
                row += 1;
                column = 0;
                continue;
            }
            let char_width = UnicodeWidthChar::width(character).unwrap_or(1).max(1);
            if column + char_width > width {
                row += 1;
                column = 0;
            }
            column += char_width;
            if column >= width {
                row += column / width;
                column %= width;
            }
        }
        (row, column)
    }

    fn cursor_for_visual_position(
        &self,
        target_row: usize,
        target_column: usize,
        width: usize,
    ) -> usize {
        let width = width.max(1);
        let mut best_index = self.input_char_len();
        let mut best_distance = usize::MAX;
        for index in 0..=self.input_char_len() {
            let (row, column) = self.visual_position_for_cursor(index, width);
            if row < target_row {
                continue;
            }
            if row > target_row {
                break;
            }
            let distance = column.abs_diff(target_column);
            if distance <= best_distance {
                best_index = index;
                best_distance = distance;
            } else {
                break;
            }
        }
        best_index
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
                "/demo" => {
                    self.inject_demo_run()?;
                    Ok(None)
                }
                "/effort" => self.set_runtime_reasoning_effort_from_command(&command.arg),
                "/model" => self.set_runtime_model_from_command(&command.arg),
                "/tool" => self.set_tool_card_view_from_command(&command.arg),
                "/tools" => self.set_tool_preview_mode_from_command(&command.arg),
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
                "/sessions" => {
                    self.activity_panel_mode = ActivityPanelMode::Sessions;
                    self.refresh_session_history();
                    self.emit_session_history();
                    Ok(None)
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

    fn inject_demo_run(&mut self) -> Result<()> {
        self.push_timeline(TimelineRole::Notice, "rendering demo run...");
        self.handle(RunEvent::Notice("demo run started".to_owned()))?;
        self.handle(RunEvent::ReasoningDelta("scanning workspace...".to_owned()))?;
        self.handle(RunEvent::TextDelta(
            "inspecting workspace first.".to_owned(),
        ))?;
        self.handle(RunEvent::ToolCallStarted(ToolCall {
            id: "demo-call-1".to_owned(),
            name: "ls".to_owned(),
            args_json: r#"{"path":"."}"#.to_owned(),
        }))?;
        self.handle(RunEvent::ToolCallArgsDelta {
            id: "demo-call-1".to_owned(),
            delta: r#"{"path":"."}"#.to_owned(),
        })?;
        self.handle(RunEvent::ToolCallCompleted(ToolCall {
            id: "demo-call-1".to_owned(),
            name: "ls".to_owned(),
            args_json: r#"{"path":"."}"#.to_owned(),
        }))?;
        self.handle(RunEvent::ToolResult(ToolResult {
            call_id: "demo-call-1".to_owned(),
            tool_name: "ls".to_owned(),
            content: "Cargo.toml\ncrates/\ndev/\ntermquill.toml".to_owned(),
            is_error: false,
            metadata: Default::default(),
        }))?;
        self.handle(RunEvent::Usage(UsageStats {
            prompt_tokens: 1200,
            completion_tokens: 180,
            cache_hit_tokens: 900,
            cache_miss_tokens: 300,
            input_cost: 0.0,
            output_cost: 0.0,
            cache_savings: 0.0,
            system_fingerprint: Some("demo-fingerprint".to_owned()),
        }))?;
        self.handle(RunEvent::Control(ControlEntry::Note {
            kind: "demo".to_owned(),
            data: serde_json::json!({"status":"ok"}),
        }))?;
        self.handle(RunEvent::AssistantMessage(ModelMessage::assistant(
            Some("Demo run finished. The next milestone is binding this shell to the actual controller and agent loop.".to_owned()),
            Vec::new(),
        )))?;
        Ok(())
    }

    fn push_timeline(&mut self, role: TimelineRole, text: impl Into<String>) {
        let is_tool = role == TimelineRole::Tool;
        self.timeline.push(TimelineEntry {
            role,
            text: text.into(),
        });
        if self.timeline.len() > 400 {
            self.timeline.remove(0);
            self.reindex_timeline_state_after_front_trim();
        }
        if is_tool {
            self.selected_tool_timeline_entry = self.timeline.len().checked_sub(1);
        }
        self.rebuild_timeline_render_cache();
    }

    fn reindex_timeline_state_after_front_trim(&mut self) {
        self.streaming_assistant_index = self
            .streaming_assistant_index
            .and_then(|index| index.checked_sub(1));
        self.streaming_reasoning_index = self
            .streaming_reasoning_index
            .and_then(|index| index.checked_sub(1));
        self.selected_tool_timeline_entry = self
            .selected_tool_timeline_entry
            .and_then(|index| index.checked_sub(1));
        self.expanded_tool_timeline_entries = self
            .expanded_tool_timeline_entries
            .iter()
            .filter_map(|index| index.checked_sub(1))
            .collect();
    }

    fn push_event(&mut self, label: impl Into<String>, detail: impl Into<String>) {
        self.events.push(EventEntry {
            label: label.into(),
            detail: detail.into(),
        });
        if self.events.len() > 400 {
            self.events.remove(0);
        }
    }

    fn append_assistant_delta(&mut self, delta: &str) {
        self.streaming_reasoning_index = None;
        if let Some(index) = self.streaming_assistant_index
            && let Some(entry) = self.timeline.get_mut(index)
        {
            entry.text.push_str(delta);
            self.rerender_timeline_entry(index);
            return;
        }

        self.push_timeline(TimelineRole::Assistant, delta);
        self.streaming_assistant_index = self.timeline.len().checked_sub(1);
    }

    fn append_reasoning_delta(&mut self, delta: &str) {
        self.streaming_assistant_index = None;
        if let Some(index) = self.streaming_reasoning_index
            && let Some(entry) = self.timeline.get_mut(index)
        {
            entry.text.push_str(delta);
            self.rerender_timeline_entry(index);
            return;
        }

        self.push_timeline(TimelineRole::Thinking, delta);
        self.streaming_reasoning_index = self.timeline.len().checked_sub(1);
    }

    fn push_phase_marker(&mut self, text: impl Into<String>) {
        let text = text.into();
        if self.last_phase_marker.as_deref() == Some(text.as_str()) {
            return;
        }
        self.last_phase_marker = Some(text.clone());
        self.push_event("phase", text);
    }

    fn toggle_thinking_block_mode(&mut self) {
        self.thinking_block_mode = match self.thinking_block_mode {
            ThinkingBlockMode::Collapsed => ThinkingBlockMode::Expanded,
            ThinkingBlockMode::Expanded => ThinkingBlockMode::Collapsed,
        };
        self.rebuild_timeline_render_cache();
        self.last_notice = Some(format!("thinking {}", self.thinking_block_mode.as_str()));
        self.push_event("thinking:view", self.thinking_block_mode.as_str());
    }

    fn rebuild_timeline_render_cache(&mut self) {
        let options = self.timeline_render_options();
        self.timeline_render_cache.clear();
        self.timeline_plain_cache.clear();
        self.timeline_prefix_hashes.clear();
        self.timeline_render_ranges.clear();
        for index in 0..self.timeline.len() {
            let start = self.timeline_render_cache.len();
            let rendered = {
                let entry = &self.timeline[index];
                crate::ui::render_timeline_entry_lines_with_options(entry, &options, index)
            };
            self.extend_timeline_render_buffers(rendered);
            let end = self.timeline_render_cache.len();
            self.timeline_render_ranges.push(start..end);
        }
        self.trim_trailing_timeline_blanks();
        self.timeline_revision = self.timeline_revision.saturating_add(1);
    }

    fn rerender_timeline_entry(&mut self, index: usize) {
        let Some(existing_range) = self.timeline_render_ranges.get(index).cloned() else {
            self.rebuild_timeline_render_cache();
            return;
        };
        let Some(entry) = self.timeline.get(index) else {
            self.rebuild_timeline_render_cache();
            return;
        };
        let options = self.timeline_render_options();
        let new_lines = crate::ui::render_timeline_entry_lines_with_options(entry, &options, index);
        let new_plain = new_lines.iter().map(plain_line_text).collect::<Vec<_>>();
        let old_len = existing_range.end.saturating_sub(existing_range.start);
        let new_len = new_lines.len();
        self.timeline_render_cache
            .splice(existing_range.clone(), new_lines);
        self.timeline_plain_cache
            .splice(existing_range.clone(), new_plain);
        self.timeline_render_ranges[index] =
            existing_range.start..existing_range.start.saturating_add(new_len);
        if new_len != old_len {
            let delta = new_len as isize - old_len as isize;
            for range in self.timeline_render_ranges.iter_mut().skip(index + 1) {
                range.start = range.start.saturating_add_signed(delta);
                range.end = range.end.saturating_add_signed(delta);
            }
        }
        self.rebuild_timeline_prefix_hashes_from(existing_range.start);
        self.trim_trailing_timeline_blanks();
        self.timeline_revision = self.timeline_revision.saturating_add(1);
    }

    fn extend_timeline_render_buffers(&mut self, lines: Vec<Line<'static>>) {
        for line in lines {
            let plain = plain_line_text(&line);
            self.timeline_render_cache.push(line);
            self.timeline_plain_cache.push(plain.clone());
            let hash = hash_timeline_line(
                self.timeline_prefix_hashes.last().copied().unwrap_or(0),
                &plain,
            );
            self.timeline_prefix_hashes.push(hash);
        }
    }

    fn rebuild_timeline_prefix_hashes_from(&mut self, start_line: usize) {
        let truncate_to = start_line.min(self.timeline_plain_cache.len());
        self.timeline_prefix_hashes.truncate(truncate_to);
        let mut hash = if truncate_to == 0 {
            0
        } else {
            self.timeline_prefix_hashes.last().copied().unwrap_or(0)
        };
        for line in self.timeline_plain_cache.iter().skip(truncate_to) {
            hash = hash_timeline_line(hash, line);
            self.timeline_prefix_hashes.push(hash);
        }
    }

    fn trim_trailing_timeline_blanks(&mut self) {
        while self
            .timeline_render_cache
            .last()
            .map(|line| line.spans.is_empty())
            .unwrap_or(false)
        {
            let _ = self.timeline_render_cache.pop();
            let _ = self.timeline_plain_cache.pop();
            let _ = self.timeline_prefix_hashes.pop();
            if let Some(range) = self.timeline_render_ranges.last_mut() {
                if range.end > range.start {
                    range.end -= 1;
                } else {
                    let _ = self.timeline_render_ranges.pop();
                }
            }
        }
    }

    pub fn poll_background_tasks(&mut self) -> bool {
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
            return true;
        }
        false
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

    pub fn approval_preview_lines(&self) -> Vec<String> {
        let Some(pending) = &self.pending_approval else {
            return self.session_view_lines();
        };

        let mut lines = Vec::new();
        if let Some(preview) = &pending.preview {
            if !self.approval_metadata_collapsed {
                lines.push(format!(
                    "tool={}  id={}  mode={}",
                    pending.call.name,
                    pending.call.id,
                    approval_access_label(pending.spec.read_only)
                ));
                lines.push(format!("preview={}", preview.title));
                if !preview.summary.trim().is_empty() {
                    lines.push(preview.summary.clone());
                }
                lines.push(String::new());
            } else {
                lines.push("meta hidden".to_owned());
            }

            if !preview.file_diffs.is_empty() {
                lines.push(format!(
                    "file {}/{}",
                    self.approval_selected_file_index
                        .min(preview.file_diffs.len() - 1)
                        + 1,
                    preview.file_diffs.len()
                ));
                for (index, file) in preview.file_diffs.iter().enumerate() {
                    let selected = if index == self.approval_selected_file_index {
                        ">"
                    } else {
                        " "
                    };
                    lines.push(format!("{selected} {}", file.path));
                }
                lines.push(String::new());
            } else if !preview.changed_files.is_empty() {
                lines.push(format!("changed: {}", preview.changed_files.join(", ")));
                lines.push(String::new());
            }

            let diff = self
                .selected_approval_diff()
                .unwrap_or(preview.body.as_str());
            let diff = self.transform_approval_diff(diff);
            let hunk_positions = self.approval_hunk_positions();
            let active_hunk_line = match self.approval_diff_mode {
                ApprovalDiffMode::Full => hunk_positions
                    .get(self.approval_selected_hunk_index)
                    .copied()
                    .unwrap_or(usize::MAX),
                ApprovalDiffMode::CurrentHunk | ApprovalDiffMode::ChangedOnly => 0,
            };
            lines.push(format!(
                "mode={}  hunk {}/{}  [,] hunk  ,/. file  m meta  v view",
                self.approval_diff_mode.label(),
                if hunk_positions.is_empty() {
                    0
                } else {
                    self.approval_selected_hunk_index + 1
                },
                hunk_positions.len()
            ));
            lines.push(String::new());
            for (index, line) in diff.lines().enumerate() {
                let prefix = if index == active_hunk_line {
                    ">> "
                } else if line.starts_with("@@") {
                    " > "
                } else {
                    "   "
                };
                lines.push(format!("{prefix}{line}"));
            }
        } else {
            lines.push(format!(
                "tool={}  id={}  mode={}",
                pending.call.name,
                pending.call.id,
                approval_access_label(pending.spec.read_only)
            ));
            lines.push(format!("args={}", pending.call.args_json));
        }

        lines.push(String::new());
        lines.push("Y allow  N deny".to_owned());
        lines
    }

    pub(crate) fn approval_modal_view(&self) -> Option<ApprovalModalView> {
        let pending = self.pending_approval.as_ref()?;
        let access_label = approval_access_label(pending.spec.read_only);
        let Some(preview) = pending.preview.as_ref() else {
            return Some(ApprovalModalView {
                tool_name: pending.call.name.clone(),
                call_id: pending.call.id.clone(),
                access_label,
                preview_title: format!("Run {}", pending.call.name),
                preview_summary: "Tool preview unavailable for this call.".to_owned(),
                metadata_collapsed: self.approval_metadata_collapsed,
                file_rows: Vec::new(),
                changed_files: Vec::new(),
                diff_mode_label: self.approval_diff_mode.label(),
                active_hunk_index: 0,
                hunk_total: 0,
                diff_label: pending.call.name.clone(),
                diff_lines: vec![ApprovalDiffLine {
                    text: "No structured diff preview available.".to_owned(),
                    kind: ApprovalDiffLineKind::Context,
                    active_hunk: false,
                }],
            });
        };

        let raw_diff = self
            .selected_approval_diff()
            .unwrap_or(preview.body.as_str());
        let transformed_diff = self.transform_approval_diff(raw_diff);
        let transformed_lines = transformed_diff.lines().collect::<Vec<_>>();
        let hunk_positions = self.approval_hunk_positions();
        let active_hunk_index = if hunk_positions.is_empty() {
            0
        } else {
            self.approval_selected_hunk_index
                .min(hunk_positions.len() - 1)
                + 1
        };
        let active_hunk_line = match self.approval_diff_mode {
            ApprovalDiffMode::Full => hunk_positions
                .get(self.approval_selected_hunk_index)
                .copied()
                .unwrap_or(usize::MAX),
            ApprovalDiffMode::CurrentHunk | ApprovalDiffMode::ChangedOnly => transformed_lines
                .iter()
                .position(|line| line.starts_with("@@"))
                .unwrap_or(0),
        };

        let mut diff_lines = transformed_lines
            .iter()
            .enumerate()
            .map(|(index, line)| ApprovalDiffLine {
                text: (*line).to_owned(),
                kind: approval_diff_line_kind(line),
                active_hunk: index == active_hunk_line && line.starts_with("@@"),
            })
            .collect::<Vec<_>>();
        if diff_lines.is_empty() {
            diff_lines.push(ApprovalDiffLine {
                text: "No preview body available.".to_owned(),
                kind: ApprovalDiffLineKind::Context,
                active_hunk: false,
            });
        }

        let file_rows: Vec<ApprovalFileRow> = if !preview.file_diffs.is_empty() {
            preview
                .file_diffs
                .iter()
                .enumerate()
                .map(|(index, file)| ApprovalFileRow {
                    path: file.path.clone(),
                    selected: index == self.approval_selected_file_index,
                })
                .collect()
        } else {
            preview
                .changed_files
                .iter()
                .enumerate()
                .map(|(index, path)| ApprovalFileRow {
                    path: path.clone(),
                    selected: index == self.approval_selected_file_index,
                })
                .collect()
        };

        let diff_label = file_rows
            .iter()
            .find(|row| row.selected)
            .map(|row| row.path.clone())
            .filter(|path: &String| !path.is_empty())
            .unwrap_or_else(|| preview.title.clone());

        Some(ApprovalModalView {
            tool_name: pending.call.name.clone(),
            call_id: pending.call.id.clone(),
            access_label,
            preview_title: preview.title.clone(),
            preview_summary: preview.summary.clone(),
            metadata_collapsed: self.approval_metadata_collapsed,
            file_rows,
            changed_files: preview.changed_files.clone(),
            diff_mode_label: self.approval_diff_mode.label(),
            active_hunk_index,
            hunk_total: hunk_positions.len(),
            diff_label,
            diff_lines,
        })
    }

    pub fn scrollback_lines(&self) -> Vec<Line<'static>> {
        self.scrollback_lines_from(0)
    }

    pub fn scrollback_lines_from(&self, from_index: usize) -> Vec<Line<'static>> {
        let cutoff_line = self.scrollback_cutoff_line();
        let start = from_index.min(cutoff_line);
        let mut lines = self.timeline_render_cache
            [start..cutoff_line.min(self.timeline_render_cache.len())]
            .to_vec();
        while lines
            .last()
            .map(|line| !line_has_visible_content(line))
            .unwrap_or(false)
        {
            let _ = lines.pop();
        }
        lines
    }

    pub fn scrollback_line_count(&self) -> usize {
        self.scrollback_cutoff_line()
    }

    pub fn scrollback_prefix_hash(&self, line_count: usize) -> u64 {
        let count = line_count.min(self.scrollback_cutoff_line());
        if count == 0 {
            return 0;
        }
        self.timeline_prefix_hashes
            .get(count - 1)
            .copied()
            .unwrap_or(0)
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn transcript_lines(&self, max_lines: usize) -> Vec<Line<'static>> {
        let effective_len = self.effective_timeline_render_len();
        if effective_len == 0 {
            return vec![
                Line::from("no messages yet"),
                Line::from("send a prompt to start"),
            ];
        }
        let viewport = max_lines.max(1);
        let scroll_back = self
            .timeline_scroll_back
            .min(effective_len.saturating_sub(viewport));
        let end = effective_len.saturating_sub(scroll_back);
        let start = end.saturating_sub(viewport);
        self.timeline_render_cache[start..end].to_vec()
    }

    #[allow(dead_code)]
    pub(crate) fn transcript_status_label(&self) -> String {
        if self.timeline_scroll_back == 0 {
            return "live tail".to_owned();
        }
        let hidden_after = self
            .timeline_scroll_back
            .min(self.max_timeline_scroll_back());
        format!("+{hidden_after} above")
    }

    pub fn timeline_revision(&self) -> u64 {
        self.timeline_revision
    }

    fn timeline_render_options(&self) -> crate::ui::TimelineRenderOptions {
        crate::ui::TimelineRenderOptions {
            expand_tool_previews: matches!(self.tool_preview_mode, ToolPreviewMode::Full),
            expand_thinking_blocks: matches!(self.thinking_block_mode, ThinkingBlockMode::Expanded),
            selected_tool_entry: self.selected_tool_timeline_entry,
            expanded_tool_entries: self.expanded_tool_timeline_entries.clone(),
            max_content_width: self.timeline_content_width(),
        }
    }

    fn timeline_content_width(&self) -> usize {
        let total_width = self.terminal_width.max(24) as usize;
        let sidebar_width = sidebar_width_for_terminal(total_width);
        let live_panel_width = total_width
            .saturating_sub(sidebar_width)
            .saturating_sub(2)
            .max(10);
        live_panel_width.saturating_sub(4).max(20)
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

    pub(crate) fn live_activity_summary(&self) -> Option<LiveActivitySummary> {
        if let Some(pending) = &self.pending_approval {
            return Some(LiveActivitySummary {
                label: "approval".to_owned(),
                detail: format!("waiting for decision on {}", pending.call.name),
            });
        }
        if !self.is_busy {
            return None;
        }
        let (label, detail) = match &self.run_phase {
            RunPhase::Idle => ("working", "waiting for next event".to_owned()),
            RunPhase::Thinking => ("thinking", format!("reasoning with {}", self.model_name)),
            RunPhase::Tool(name) => ("tool", format!("running {name}")),
            RunPhase::Streaming => ("streaming", "writing the reply".to_owned()),
        };
        Some(LiveActivitySummary {
            label: label.to_owned(),
            detail,
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
            format!("tools: {}", self.tool_preview_mode.as_str()),
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

    #[cfg_attr(not(test), allow(dead_code))]
    pub fn activity_panel_lines(&self) -> Vec<String> {
        self.activity_panel_rows()
            .into_iter()
            .map(|row| match row {
                ActivityPanelRow::Event { label, detail } => format!("{label} {detail}"),
                ActivityPanelRow::SessionHeader { filter, total } => {
                    format!("filter={filter} total={total}")
                }
                ActivityPanelRow::SessionItem {
                    index,
                    label,
                    current,
                    selected,
                    meta,
                } => format!(
                    "{} {}. {}{} {}",
                    if selected { ">" } else { " " },
                    index,
                    label,
                    if current { " (current)" } else { "" },
                    meta
                ),
                ActivityPanelRow::Empty { text } => text,
            })
            .collect()
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn activity_panel_rows(&self) -> Vec<ActivityPanelRow> {
        match self.activity_panel_mode {
            ActivityPanelMode::Events => self
                .events
                .iter()
                .map(|event| ActivityPanelRow::Event {
                    label: event.label.clone(),
                    detail: truncate_session_view_text(&event.detail, 72),
                })
                .collect(),
            ActivityPanelMode::Sessions => self.recent_session_rows(),
        }
    }

    pub fn is_setup_mode(&self) -> bool {
        self.setup_state.is_some()
    }

    pub fn is_config_mode(&self) -> bool {
        self.config_state.is_some()
    }

    pub fn setup_lines(&self) -> Vec<String> {
        let Some(state) = &self.setup_state else {
            return Vec::new();
        };

        let mut lines = vec![
            "Quick setup".to_owned(),
            "[workspace]".to_owned(),
            render_setup_toggle_row(
                SetupField::TrustCurrentFolder,
                state.selected_field,
                "trust_current_folder",
                state.trusted_current_folder,
            ),
            String::new(),
            "[runtime]".to_owned(),
            render_setup_value_row(
                SetupField::Model,
                state.selected_field,
                "model",
                &state.model,
                Some("Enter choose"),
            ),
            render_setup_value_row(
                SetupField::ApiKey,
                state.selected_field,
                "api_key",
                &state.masked_api_key(),
                Some("Enter input"),
            ),
            render_setup_action_row(SetupField::Save, state.selected_field, "save and start"),
            String::new(),
            "[notes]".to_owned(),
            format!("auth={}", state.auth_summary()),
            "defaults: ask / mem on / compact on".to_owned(),
        ];

        if let Some(error) = &state.startup_error {
            lines.push(String::new());
            lines.push(format!("load failed: {error}"));
        }

        lines.push(String::new());
        lines.push(format!(
            "Tab move  Enter open/toggle  Ctrl-S save  Ctrl-C quit  env={TERMQUILL_API_KEY_ENV}"
        ));
        lines
    }

    pub fn config_section_title(&self) -> Option<&'static str> {
        self.config_state
            .as_ref()
            .map(|state| state.selected_section.title())
    }

    pub fn config_selected_field_label(&self) -> Option<&'static str> {
        self.config_state.as_ref().and_then(|state| {
            if state.footer_selected {
                Some(state.selected_footer_action.field_label())
            } else {
                state.selected_field.map(ConfigField::label)
            }
        })
    }

    pub fn config_selected_footer_action_label(&self) -> Option<&'static str> {
        self.config_state.as_ref().and_then(|state| {
            state
                .footer_selected
                .then_some(state.selected_footer_action.button_label())
        })
    }

    pub fn config_footer_hint(&self) -> String {
        if self.config_is_dirty() {
            "draft has unsaved changes".to_owned()
        } else {
            "all changes saved".to_owned()
        }
    }

    pub fn config_is_editing(&self) -> bool {
        matches!(self.modal_state, Some(ModalState::TextInput(_)))
    }

    pub fn config_editing_field_label(&self) -> Option<&'static str> {
        match self.modal_state.as_ref() {
            Some(ModalState::TextInput(TextInputState {
                target: TextInputTarget::ConfigField(field),
                ..
            })) => Some(field.label()),
            _ => None,
        }
    }

    pub fn config_is_dirty(&self) -> bool {
        self.config_state
            .as_ref()
            .map(|state| state.dirty)
            .unwrap_or(false)
    }

    pub fn has_modal(&self) -> bool {
        self.modal_state.is_some()
    }

    pub fn modal_title(&self) -> Option<&'static str> {
        match self.modal_state.as_ref()? {
            ModalState::ModelPicker(state) => Some(state.target.title()),
            ModalState::SecretInput(state) => Some(state.target.title()),
            ModalState::TextInput(state) => Some(state.target.title()),
        }
    }

    pub fn modal_lines(&self) -> Vec<String> {
        match self.modal_state.as_ref() {
            Some(ModalState::ModelPicker(state)) => {
                let mut lines = vec![
                    state.target.summary().to_owned(),
                    "Up/Down choose  Enter apply  F2 save  F3 save+close  Esc cancel".to_owned(),
                    String::new(),
                ];
                for (index, option) in state.options.iter().enumerate() {
                    let marker = if index == state.selected { ">" } else { " " };
                    let suffix = if option == &state.current {
                        "  [current]"
                    } else {
                        ""
                    };
                    lines.push(format!("{marker} {option}{suffix}"));
                }
                lines
            }
            Some(ModalState::SecretInput(state)) => vec![
                state.target.summary().to_owned(),
                "Enter apply  F2 save  F3 save+close  Esc cancel".to_owned(),
                String::new(),
                format!("api_key: {}|", "*".repeat(state.buffer.chars().count())),
            ],
            Some(ModalState::TextInput(state)) => vec![
                state.target.summary().to_owned(),
                "Enter apply  F2 save  F3 save+close  Esc cancel".to_owned(),
                String::new(),
                format!("{}: {}|", state.target.prompt_label(), state.buffer),
            ],
            None => Vec::new(),
        }
    }

    pub fn modal_input_cursor(&self) -> Option<(&'static str, usize, usize)> {
        match self.modal_state.as_ref()? {
            ModalState::SecretInput(state) => Some(("api_key", state.buffer.chars().count(), 3)),
            ModalState::TextInput(state) => {
                Some((state.target.prompt_label(), state.buffer.chars().count(), 3))
            }
            ModalState::ModelPicker(_) => None,
        }
    }

    pub fn config_nav_lines(&self) -> Vec<String> {
        let Some(state) = &self.config_state else {
            return Vec::new();
        };

        let mut lines = vec!["Config".to_owned(), String::new()];
        for section in ConfigSection::FLOW {
            lines.push(format!(
                "{} {}",
                if section == state.selected_section {
                    ">"
                } else {
                    " "
                },
                section.title()
            ));
        }
        lines.push(String::new());
        lines.push("Tab step  Up/Down field".to_owned());
        lines.push("Down footer  Left/Right action".to_owned());
        lines.push("Enter choose/input/toggle/run".to_owned());
        lines.push("Ctrl-S save  Esc close".to_owned());
        lines.push("MCP: Ctrl-N add  Ctrl-D drop".to_owned());
        lines.push("MCP: PgUp/PgDn switch".to_owned());
        lines
    }

    pub fn config_detail_lines(&self) -> Vec<String> {
        let Some(config_state) = &self.config_state else {
            return Vec::new();
        };
        let section = config_state.selected_section;
        let step_label = ConfigSection::FLOW
            .iter()
            .map(|candidate| {
                if *candidate == section {
                    format!("[{}]", candidate.title().to_lowercase())
                } else {
                    candidate.title().to_lowercase()
                }
            })
            .collect::<Vec<_>>()
            .join(" ");
        let mut lines = vec![match section.flow_index() {
            Some(index) => format!(
                "{} ({}/{})",
                section.title(),
                index + 1,
                ConfigSection::FLOW.len()
            ),
            None => section.title().to_owned(),
        }];
        lines.push(step_label);
        lines.push(section.summary().to_owned());
        lines.push(
            "Tab step  Up/Down field  Down footer  Left/Right action  Enter open/toggle/run"
                .to_owned(),
        );
        lines.push(String::new());

        match section {
            ConfigSection::Provider => {
                lines.push("[runtime]".to_owned());
                lines.push(render_config_value_row(
                    config_state,
                    ConfigField::ProviderModel,
                ));
                lines.push(render_config_value_row(
                    config_state,
                    ConfigField::ProviderApiKey,
                ));
                lines.push(String::new());
                lines.push("[network]".to_owned());
                lines.push(render_config_value_row(
                    config_state,
                    ConfigField::ProviderBaseUrl,
                ));
                lines.push(render_config_value_row(
                    config_state,
                    ConfigField::ProviderFimModel,
                ));
                lines.push(String::new());
                lines.push("[notes]".to_owned());
                lines.push(format!("auth: file api_key or env {TERMQUILL_API_KEY_ENV}"));
                lines.push("advanced provider fields: config file or env".to_owned());
                lines.push("see README for TERMQUILL_* overrides".to_owned());
            }
            ConfigSection::Permissions => {
                lines.push("[default]".to_owned());
                lines.push(render_config_value_row(
                    config_state,
                    ConfigField::PermissionsWriteMode,
                ));
                lines.push(String::new());
                lines.push("[rules]".to_owned());
                lines.push(format!(
                    "overrides: {}",
                    config_state.draft.base_root_config.permission.rules.len()
                ));
                if config_state
                    .draft
                    .base_root_config
                    .permission
                    .rules
                    .is_empty()
                {
                    lines.push("no overrides".to_owned());
                } else {
                    for rule in &config_state.draft.base_root_config.permission.rules {
                        lines.push(format!(
                            "- {}  subject={}  mode={}",
                            rule.tool_name,
                            rule.subject_glob.as_deref().unwrap_or("<none>"),
                            rule.mode.as_str()
                        ));
                    }
                }
            }
            ConfigSection::Memory => {
                lines.push("[memory]".to_owned());
                lines.push(render_config_value_row(
                    config_state,
                    ConfigField::MemoryEnabled,
                ));
                lines.push(format!("docs: {}", self.memory_document_count));
                lines.push(format!("status: {}", self.memory_last_status));
                lines.push(
                    "root docs: TERMQUILL.md AGENTS.md CLAUDE.md TERMQUILL.local.md".to_owned(),
                );
            }
            ConfigSection::Compaction => {
                lines.push("[thresholds]".to_owned());
                lines.push(render_config_value_row(
                    config_state,
                    ConfigField::CompactionEnabled,
                ));
                lines.push(render_config_value_row(
                    config_state,
                    ConfigField::CompactionContextWindowTokens,
                ));
                lines.push(render_config_value_row(
                    config_state,
                    ConfigField::CompactionSoftThresholdRatio,
                ));
                lines.push(render_config_value_row(
                    config_state,
                    ConfigField::CompactionHardThresholdRatio,
                ));
                lines.push(render_config_value_row(
                    config_state,
                    ConfigField::CompactionTailMessages,
                ));
                lines.push(format!("status: {}", self.compaction_status));
            }
            ConfigSection::Mcp => {
                lines.push("[servers]".to_owned());
                lines.push(format!("servers: {}", config_state.draft.mcp_servers.len()));
                if config_state.draft.mcp_servers.is_empty() {
                    lines.push("no MCP servers".to_owned());
                    lines.push("Ctrl-N to add".to_owned());
                } else {
                    lines.push(format!(
                        "selected: {}/{}",
                        config_state.selected_mcp_server_index + 1,
                        config_state.draft.mcp_servers.len()
                    ));
                    if config_state.selected_mcp_server().is_some() {
                        lines.push(render_config_value_row(config_state, ConfigField::McpName));
                        lines.push(render_config_value_row(
                            config_state,
                            ConfigField::McpCommand,
                        ));
                        lines.push(render_config_value_row(
                            config_state,
                            ConfigField::McpArgsCsv,
                        ));
                        lines.push(render_config_value_row(
                            config_state,
                            ConfigField::McpStartupTimeoutSecs,
                        ));
                    }
                }
                lines.push(String::new());
                lines.push("Ctrl-N add  Ctrl-D drop  PgUp/PgDn server".to_owned());
                lines.push("args_csv: comma list".to_owned());
            }
        }

        lines
    }

    fn open_model_picker(&mut self, target: ModelPickerTarget, current: &str) {
        let (options, notice) = self.model_picker_options(target, current);
        let selected = options
            .iter()
            .position(|option| option == current)
            .unwrap_or(0);
        self.modal_state = Some(ModalState::ModelPicker(ModelPickerState {
            target,
            current: current.to_owned(),
            options,
            selected,
        }));
        self.last_notice = Some(notice);
    }

    fn model_picker_options(
        &self,
        target: ModelPickerTarget,
        current: &str,
    ) -> (Vec<String>, String) {
        if cfg!(test) {
            return (
                build_model_picker_options(current, Vec::new()),
                "using local model list".to_owned(),
            );
        }
        let provider_config = match self
            .provider_config_for_model_picker(target, current)
            .resolved()
        {
            Ok(config) => config,
            Err(error) => {
                return (
                    build_model_picker_options(current, Vec::new()),
                    format!("model list unavailable: {error}"),
                );
            }
        };
        match fetch_remote_model_ids(&provider_config) {
            Ok(remote) if !remote.is_empty() => (
                build_model_picker_options(current, remote),
                format!("loaded provider model list ({})", provider_config.base_url),
            ),
            _ => (
                build_model_picker_options(current, Vec::new()),
                "using local model list".to_owned(),
            ),
        }
    }

    fn provider_config_for_model_picker(
        &self,
        target: ModelPickerTarget,
        current: &str,
    ) -> DeepSeekProviderConfig {
        if let Some(state) = &self.config_state {
            return DeepSeekProviderConfig {
                base_url: non_empty_or(&state.draft.provider_base_url, "https://api.deepseek.com"),
                beta_base_url: non_empty_or(
                    &state.draft.provider_beta_base_url,
                    "https://api.deepseek.com/beta",
                ),
                anthropic_base_url: non_empty_or(
                    &state.draft.provider_anthropic_base_url,
                    "https://api.deepseek.com/anthropic",
                ),
                model: match target {
                    ModelPickerTarget::ProviderFim => state.draft.provider_model.clone(),
                    _ => current.trim().to_owned(),
                },
                api_key: (!state.draft.provider_api_key.trim().is_empty())
                    .then(|| state.draft.provider_api_key.trim().to_owned()),
                user_id_strategy: (!state.draft.provider_user_id_strategy.trim().is_empty())
                    .then(|| state.draft.provider_user_id_strategy.trim().to_owned()),
                strict_tools_mode: state.draft.provider_strict_tools_mode,
                fim_model: match target {
                    ModelPickerTarget::ProviderFim => current.trim().to_owned(),
                    _ => state.draft.provider_fim_model.clone(),
                },
                request_timeout_secs: state
                    .draft
                    .provider_request_timeout_secs
                    .trim()
                    .parse::<u64>()
                    .ok()
                    .filter(|value| *value > 0)
                    .unwrap_or(120),
            };
        }

        if let Some(state) = &self.setup_state {
            let mut provider_config = default_deepseek_provider_config(current);
            provider_config.model = current.trim().to_owned();
            provider_config.api_key =
                (!state.api_key.trim().is_empty()).then(|| state.api_key.trim().to_owned());
            return provider_config;
        }

        self.config_snapshot
            .as_ref()
            .and_then(load_deepseek_provider_config)
            .unwrap_or_else(|| default_deepseek_provider_config(current))
    }

    fn open_secret_input(&mut self, target: SecretInputTarget, current: &str) {
        self.modal_state = Some(ModalState::SecretInput(SecretInputState {
            target,
            buffer: current.to_owned(),
        }));
        self.last_notice = Some(format!("editing {}", target.title().to_lowercase()));
    }

    fn open_secret_input_with_char(&mut self, target: SecretInputTarget, character: char) {
        self.modal_state = Some(ModalState::SecretInput(SecretInputState {
            target,
            buffer: character.to_string(),
        }));
        self.last_notice = Some(format!("editing {}", target.title().to_lowercase()));
    }

    fn open_text_input(&mut self, target: TextInputTarget, current: &str) {
        self.modal_state = Some(ModalState::TextInput(TextInputState {
            target,
            buffer: current.to_owned(),
        }));
        self.last_notice = Some(format!("editing {}", target.prompt_label()));
    }

    fn open_text_input_with_char(&mut self, target: TextInputTarget, character: char) {
        self.modal_state = Some(ModalState::TextInput(TextInputState {
            target,
            buffer: character.to_string(),
        }));
        self.last_notice = Some(format!("editing {}", target.prompt_label()));
    }

    fn handle_modal_key_event(&mut self, key: KeyEvent) -> ModalOutcome {
        let Some(modal_state) = self.modal_state.as_mut() else {
            return ModalOutcome::None;
        };

        match modal_state {
            ModalState::ModelPicker(state) => match key.code {
                KeyCode::Esc => {
                    self.modal_state = None;
                    ModalOutcome::Dismissed("closed picker".to_owned())
                }
                KeyCode::Up => {
                    if state.selected == 0 {
                        state.selected = state.options.len().saturating_sub(1);
                    } else {
                        state.selected -= 1;
                    }
                    self.last_notice = Some(format!(
                        "{} {}",
                        state.target.title().to_lowercase(),
                        state
                            .options
                            .get(state.selected)
                            .cloned()
                            .unwrap_or_default()
                    ));
                    ModalOutcome::None
                }
                KeyCode::Down => {
                    if !state.options.is_empty() {
                        state.selected = (state.selected + 1) % state.options.len();
                    }
                    self.last_notice = Some(format!(
                        "{} {}",
                        state.target.title().to_lowercase(),
                        state
                            .options
                            .get(state.selected)
                            .cloned()
                            .unwrap_or_default()
                    ));
                    ModalOutcome::None
                }
                KeyCode::Enter => {
                    let Some(value) = state.options.get(state.selected).cloned() else {
                        self.modal_state = None;
                        return ModalOutcome::Dismissed("closed picker".to_owned());
                    };
                    let target = state.target;
                    self.modal_state = None;
                    ModalOutcome::ModelSelected { target, value }
                }
                _ => ModalOutcome::None,
            },
            ModalState::SecretInput(state) => match key.code {
                KeyCode::Esc => {
                    self.modal_state = None;
                    ModalOutcome::Dismissed("closed secret input".to_owned())
                }
                KeyCode::Backspace => {
                    let _ = state.buffer.pop();
                    self.last_notice = Some("editing api key".to_owned());
                    ModalOutcome::None
                }
                KeyCode::Enter => {
                    let target = state.target;
                    let value = state.buffer.clone();
                    self.modal_state = None;
                    ModalOutcome::SecretSubmitted { target, value }
                }
                KeyCode::Char(character) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                    state.buffer.push(character);
                    self.last_notice = Some("editing api key".to_owned());
                    ModalOutcome::None
                }
                _ => ModalOutcome::None,
            },
            ModalState::TextInput(state) => match key.code {
                KeyCode::Esc => {
                    self.modal_state = None;
                    ModalOutcome::Dismissed("closed text input".to_owned())
                }
                KeyCode::Backspace => {
                    let _ = state.buffer.pop();
                    self.last_notice = Some(format!("editing {}", state.target.prompt_label()));
                    ModalOutcome::None
                }
                KeyCode::Enter => {
                    let target = state.target;
                    let value = state.buffer.clone();
                    self.modal_state = None;
                    ModalOutcome::TextSubmitted { target, value }
                }
                KeyCode::Char(character) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                    if !text_input_target_accepts_char(state.target, character) {
                        self.last_notice = Some(format!(
                            "{} does not accept '{character}'",
                            state.target.prompt_label()
                        ));
                        return ModalOutcome::None;
                    }
                    state.buffer.push(character);
                    self.last_notice = Some(format!("editing {}", state.target.prompt_label()));
                    ModalOutcome::None
                }
                _ => ModalOutcome::None,
            },
        }
    }

    fn submit_modal(&mut self) -> ModalOutcome {
        let Some(modal_state) = self.modal_state.as_ref() else {
            return ModalOutcome::None;
        };

        match modal_state {
            ModalState::ModelPicker(state) => {
                let Some(value) = state.options.get(state.selected).cloned() else {
                    self.modal_state = None;
                    return ModalOutcome::Dismissed("closed picker".to_owned());
                };
                let target = state.target;
                self.modal_state = None;
                ModalOutcome::ModelSelected { target, value }
            }
            ModalState::SecretInput(state) => {
                let target = state.target;
                let value = state.buffer.clone();
                self.modal_state = None;
                ModalOutcome::SecretSubmitted { target, value }
            }
            ModalState::TextInput(state) => {
                let target = state.target;
                let value = state.buffer.clone();
                self.modal_state = None;
                ModalOutcome::TextSubmitted { target, value }
            }
        }
    }

    fn apply_modal_outcome(&mut self, outcome: ModalOutcome) {
        match outcome {
            ModalOutcome::None => {}
            ModalOutcome::Dismissed(message) => {
                self.last_notice = Some(message);
            }
            ModalOutcome::ModelSelected { target, value } => match target {
                ModelPickerTarget::Setup => {
                    if let Some(state) = self.setup_state.as_mut() {
                        state.model = value.clone();
                    }
                    self.last_notice = Some(format!("selected model {value}"));
                }
                ModelPickerTarget::Provider => {
                    if let Some(state) = self.config_state.as_mut() {
                        state.draft.provider_model = value.clone();
                        state.dirty = true;
                    }
                    self.last_notice = Some(format!("selected model {value}"));
                }
                ModelPickerTarget::ProviderFim => {
                    if let Some(state) = self.config_state.as_mut() {
                        state.draft.provider_fim_model = value.clone();
                        state.dirty = true;
                    }
                    self.last_notice = Some(format!("selected fim model {value}"));
                }
            },
            ModalOutcome::SecretSubmitted { target, value } => match target {
                SecretInputTarget::SetupApiKey => {
                    if let Some(state) = self.setup_state.as_mut() {
                        state.api_key = value;
                    }
                    self.last_notice = Some("updated api key".to_owned());
                }
                SecretInputTarget::ConfigProviderApiKey => {
                    if let Some(state) = self.config_state.as_mut() {
                        state.draft.provider_api_key = value;
                        state.dirty = true;
                    }
                    self.last_notice = Some("updated api key".to_owned());
                }
            },
            ModalOutcome::TextSubmitted { target, value } => match target {
                TextInputTarget::SetupModel => {
                    if let Some(state) = self.setup_state.as_mut() {
                        state.model = value.clone();
                    }
                    self.last_notice = Some(format!("updated model {value}"));
                }
                TextInputTarget::ConfigField(field) => {
                    if let Some(state) = self.config_state.as_mut()
                        && let Some(target) = state.field_text_value_mut(field)
                    {
                        let changed = *target != value;
                        *target = value.clone();
                        if changed {
                            state.dirty = true;
                        }
                    }
                    self.last_notice = Some(format!("updated {}", field.label()));
                }
            },
        }
    }

    fn handle_setup_key_event(&mut self, key: KeyEvent) -> Result<Option<AppAction>> {
        if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
            self.should_quit = true;
            return Ok(None);
        }
        if self.has_modal() {
            if key.code == KeyCode::Char('s') && key.modifiers.contains(KeyModifiers::CONTROL) {
                let outcome = self.submit_modal();
                self.apply_modal_outcome(outcome);
                return self.complete_setup();
            }
            let outcome = self.handle_modal_key_event(key);
            self.apply_modal_outcome(outcome);
            return Ok(None);
        }

        let Some(selected_field) = self.setup_state.as_ref().map(|state| state.selected_field)
        else {
            return Ok(None);
        };

        match key.code {
            KeyCode::Char('s') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                return self.complete_setup();
            }
            KeyCode::Tab | KeyCode::Down => {
                let Some(state) = self.setup_state.as_mut() else {
                    return Ok(None);
                };
                state.selected_field = state.selected_field.next();
                self.last_notice = Some(format!(
                    "setup field {}",
                    setup_field_label(state.selected_field)
                ));
                return Ok(None);
            }
            KeyCode::BackTab | KeyCode::Up => {
                let Some(state) = self.setup_state.as_mut() else {
                    return Ok(None);
                };
                state.selected_field = state.selected_field.previous();
                self.last_notice = Some(format!(
                    "setup field {}",
                    setup_field_label(state.selected_field)
                ));
                return Ok(None);
            }
            KeyCode::Left | KeyCode::Right | KeyCode::Enter
                if matches!(selected_field, SetupField::TrustCurrentFolder) =>
            {
                let Some(state) = self.setup_state.as_mut() else {
                    return Ok(None);
                };
                state.trusted_current_folder = !state.trusted_current_folder;
                self.last_notice = Some(format!(
                    "trust current folder {}",
                    setup_bool_label(state.trusted_current_folder)
                ));
                return Ok(None);
            }
            KeyCode::Enter if matches!(selected_field, SetupField::Save) => {
                return self.complete_setup();
            }
            KeyCode::Enter if matches!(selected_field, SetupField::Model) => {
                let current = self
                    .setup_state
                    .as_ref()
                    .map(|state| state.model.clone())
                    .unwrap_or_default();
                self.open_model_picker(ModelPickerTarget::Setup, &current);
                return Ok(None);
            }
            KeyCode::Enter if matches!(selected_field, SetupField::ApiKey) => {
                let current = self
                    .setup_state
                    .as_ref()
                    .map(|state| state.api_key.clone())
                    .unwrap_or_default();
                self.open_secret_input(SecretInputTarget::SetupApiKey, &current);
                return Ok(None);
            }
            KeyCode::Backspace => {
                return Ok(None);
            }
            KeyCode::Char(character) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                if matches!(selected_field, SetupField::ApiKey) {
                    self.open_secret_input_with_char(SecretInputTarget::SetupApiKey, character);
                    return Ok(None);
                }
                if matches!(selected_field, SetupField::Model) {
                    self.open_text_input_with_char(TextInputTarget::SetupModel, character);
                    return Ok(None);
                }
                return Ok(None);
            }
            _ => {}
        }

        Ok(None)
    }

    fn handle_config_key_event(&mut self, key: KeyEvent) -> Result<Option<AppAction>> {
        if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
            self.config_state = None;
            self.should_quit = true;
            return Ok(None);
        }
        if self.has_modal() {
            if key.code == KeyCode::F(2)
                || (key.code == KeyCode::Char('s') && key.modifiers.contains(KeyModifiers::CONTROL))
            {
                let outcome = self.submit_modal();
                self.apply_modal_outcome(outcome);
                return self.save_config_draft();
            }
            if key.code == KeyCode::F(3) {
                let outcome = self.submit_modal();
                self.apply_modal_outcome(outcome);
                return self.save_config_draft_and_close();
            }
            let outcome = self.handle_modal_key_event(key);
            self.apply_modal_outcome(outcome);
            return Ok(None);
        }

        let keep_close_guard = matches!(key.code, KeyCode::Esc)
            || (key.code == KeyCode::Enter
                && self.config_state.as_ref().is_some_and(|state| {
                    state.footer_selected
                        && state.selected_footer_action == ConfigFooterAction::Close
                }));
        if !keep_close_guard && let Some(config_state) = self.config_state.as_mut() {
            config_state.close_guard_armed = false;
        }

        match key.code {
            KeyCode::Esc => {
                return self.attempt_close_config();
            }
            KeyCode::F(2) => {
                return self.save_config_draft();
            }
            KeyCode::F(3) => {
                return self.save_config_draft_and_close();
            }
            KeyCode::Char('s') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                return self.save_config_draft();
            }
            KeyCode::Char('n') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                if let Some(config_state) = self.config_state.as_mut() {
                    if config_state.selected_section == ConfigSection::Mcp {
                        config_state.add_mcp_server();
                        self.last_notice = Some("added MCP server".to_owned());
                    } else {
                        self.last_notice = Some("Ctrl-N: MCP only".to_owned());
                    }
                }
            }
            KeyCode::Char('d') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                if let Some(config_state) = self.config_state.as_mut() {
                    if config_state.selected_section == ConfigSection::Mcp {
                        if config_state.remove_selected_mcp_server() {
                            self.last_notice = Some("removed MCP server".to_owned());
                        } else {
                            self.last_notice = Some("no MCP server".to_owned());
                        }
                    } else {
                        self.last_notice = Some("Ctrl-D: MCP only".to_owned());
                    }
                }
            }
            KeyCode::Tab => {
                if let Some(config_state) = self.config_state.as_mut() {
                    config_state.set_section(config_state.selected_section.next_flow());
                    self.last_notice = Some(format!(
                        "step {}",
                        config_state.selected_section.title().to_lowercase()
                    ));
                }
            }
            KeyCode::BackTab => {
                if let Some(config_state) = self.config_state.as_mut() {
                    config_state.set_section(config_state.selected_section.previous_flow());
                    self.last_notice = Some(format!(
                        "step {}",
                        config_state.selected_section.title().to_lowercase()
                    ));
                }
            }
            KeyCode::Left => {
                if let Some(config_state) = self.config_state.as_mut() {
                    if config_state.footer_selected {
                        config_state.move_footer_action(false);
                        self.last_notice = Some(format!(
                            "action {}",
                            config_state.selected_footer_action.field_label()
                        ));
                    } else {
                        config_state.set_section(config_state.selected_section.previous_flow());
                        self.last_notice = Some(format!(
                            "step {}",
                            config_state.selected_section.title().to_lowercase()
                        ));
                    }
                }
            }
            KeyCode::Right => {
                if let Some(config_state) = self.config_state.as_mut() {
                    if config_state.footer_selected {
                        config_state.move_footer_action(true);
                        self.last_notice = Some(format!(
                            "action {}",
                            config_state.selected_footer_action.field_label()
                        ));
                    } else {
                        config_state.set_section(config_state.selected_section.next_flow());
                        self.last_notice = Some(format!(
                            "step {}",
                            config_state.selected_section.title().to_lowercase()
                        ));
                    }
                }
            }
            KeyCode::PageUp => {
                if let Some(config_state) = self.config_state.as_mut()
                    && config_state.selected_section == ConfigSection::Mcp
                {
                    if config_state.cycle_mcp_server(false) {
                        self.last_notice = Some(format!(
                            "mcp server {}/{}",
                            config_state.selected_mcp_server_index + 1,
                            config_state.draft.mcp_servers.len()
                        ));
                    } else {
                        self.last_notice = Some("no MCP server to select".to_owned());
                    }
                }
            }
            KeyCode::PageDown => {
                if let Some(config_state) = self.config_state.as_mut()
                    && config_state.selected_section == ConfigSection::Mcp
                {
                    if config_state.cycle_mcp_server(true) {
                        self.last_notice = Some(format!(
                            "mcp server {}/{}",
                            config_state.selected_mcp_server_index + 1,
                            config_state.draft.mcp_servers.len()
                        ));
                    } else {
                        self.last_notice = Some("no MCP server to select".to_owned());
                    }
                }
            }
            KeyCode::Up => {
                if let Some(config_state) = self.config_state.as_mut() {
                    if config_state.footer_selected {
                        if config_state.focus_last_field()
                            && let Some(field) = config_state.selected_field
                        {
                            self.last_notice = Some(format!("config field {}", field.label()));
                        }
                    } else if let ConfigFieldMove::Moved = config_state.move_field(false)
                        && let Some(field) = config_state.selected_field
                    {
                        self.last_notice = Some(format!("config field {}", field.label()));
                    }
                }
            }
            KeyCode::Down => {
                if let Some(config_state) = self.config_state.as_mut() {
                    if config_state.footer_selected {
                        return Ok(None);
                    }
                    match config_state.move_field(true) {
                        ConfigFieldMove::Moved => {
                            if let Some(field) = config_state.selected_field {
                                self.last_notice = Some(format!("config field {}", field.label()));
                            }
                        }
                        ConfigFieldMove::Boundary | ConfigFieldMove::Unavailable => {
                            config_state.focus_footer(ConfigFooterAction::Save);
                            self.last_notice = Some("action save".to_owned());
                        }
                    }
                }
            }
            KeyCode::Enter => {
                if let Some(config_state) = self.config_state.as_ref()
                    && config_state.footer_selected
                {
                    return match config_state.selected_footer_action {
                        ConfigFooterAction::Save => self.save_config_draft(),
                        ConfigFooterAction::SaveAndClose => self.save_config_draft_and_close(),
                        ConfigFooterAction::Close => self.attempt_close_config(),
                    };
                }
                let mut open_model_picker = None;
                let mut open_secret_input = None;
                let mut open_text_input = None;

                if let Some(config_state) = self.config_state.as_mut()
                    && let Some(field) = config_state.selected_field
                {
                    match field {
                        ConfigField::ProviderModel => {
                            open_model_picker = Some((
                                ModelPickerTarget::Provider,
                                config_state.draft.provider_model.clone(),
                            ));
                        }
                        ConfigField::ProviderFimModel => {
                            open_model_picker = Some((
                                ModelPickerTarget::ProviderFim,
                                config_state.draft.provider_fim_model.clone(),
                            ));
                        }
                        ConfigField::ProviderApiKey => {
                            open_secret_input = Some((
                                SecretInputTarget::ConfigProviderApiKey,
                                config_state.draft.provider_api_key.clone(),
                            ));
                        }
                        ConfigField::PermissionsWriteMode => {
                            config_state.draft.permission_write_mode =
                                cycle_approval_mode(config_state.draft.permission_write_mode);
                            config_state.dirty = true;
                            self.last_notice = Some(format!("updated {}", field.label()));
                            return Ok(None);
                        }
                        ConfigField::MemoryEnabled => {
                            config_state.draft.memory_enabled = !config_state.draft.memory_enabled;
                            config_state.dirty = true;
                            self.last_notice = Some(format!("updated {}", field.label()));
                            return Ok(None);
                        }
                        ConfigField::CompactionEnabled => {
                            config_state.draft.compaction_enabled =
                                !config_state.draft.compaction_enabled;
                            config_state.dirty = true;
                            self.last_notice = Some(format!("updated {}", field.label()));
                            return Ok(None);
                        }
                        _ if field.accepts_text_input() => {
                            let current = config_state
                                .field_text_value(field)
                                .map(ToOwned::to_owned)
                                .unwrap_or_default();
                            open_text_input = Some((TextInputTarget::ConfigField(field), current));
                        }
                        _ => {}
                    }
                }

                if let Some((target, current)) = open_model_picker {
                    self.open_model_picker(target, &current);
                    return Ok(None);
                }
                if let Some((target, current)) = open_secret_input {
                    self.open_secret_input(target, &current);
                    return Ok(None);
                }
                if let Some((target, current)) = open_text_input {
                    self.open_text_input(target, &current);
                    return Ok(None);
                }
            }
            KeyCode::Backspace => {
                return Ok(None);
            }
            KeyCode::Char(character) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                let Some(selected_field) = self.config_state.as_ref().and_then(|state| {
                    if state.footer_selected {
                        None
                    } else {
                        state.selected_field
                    }
                }) else {
                    return Ok(None);
                };
                match selected_field {
                    ConfigField::ProviderApiKey => {
                        self.open_secret_input_with_char(
                            SecretInputTarget::ConfigProviderApiKey,
                            character,
                        );
                        return Ok(None);
                    }
                    ConfigField::ProviderModel | ConfigField::ProviderFimModel => {
                        self.open_text_input_with_char(
                            TextInputTarget::ConfigField(selected_field),
                            character,
                        );
                        return Ok(None);
                    }
                    field if field.accepts_text_input() => {
                        self.open_text_input_with_char(
                            TextInputTarget::ConfigField(field),
                            character,
                        );
                        return Ok(None);
                    }
                    _ => {}
                }
            }
            _ => {}
        }

        Ok(None)
    }

    fn open_config_panel(&mut self) {
        let Some(root_config) = self.config_snapshot.as_ref() else {
            self.last_notice = Some("config is unavailable in setup mode".to_owned());
            return;
        };

        self.config_state = Some(ConfigState::from_root_config(root_config));
        self.last_notice = Some("opened config".to_owned());
        self.push_event("mode", "config");
    }

    fn attempt_close_config(&mut self) -> Result<Option<AppAction>> {
        let Some(config_state) = self.config_state.as_mut() else {
            return Ok(None);
        };
        if config_state.dirty && !config_state.close_guard_armed {
            config_state.close_guard_armed = true;
            config_state.focus_footer(ConfigFooterAction::Save);
            self.last_notice = Some("unsaved changes; Down footer to save, Esc discard".to_owned());
            return Ok(None);
        }
        let discarded = config_state.dirty;
        self.config_state = None;
        self.last_notice = Some(if discarded {
            "closed config; discarded changes".to_owned()
        } else {
            "closed config".to_owned()
        });
        Ok(None)
    }

    fn save_config_draft(&mut self) -> Result<Option<AppAction>> {
        if self.is_busy {
            self.last_notice = Some("busy; save later".to_owned());
            return Ok(None);
        }
        let Some(config_state) = self.config_state.as_mut() else {
            return Ok(None);
        };

        let root_config = match config_state.draft.to_root_config() {
            Ok(root_config) => root_config,
            Err(error) => {
                self.last_notice = Some(error.to_string());
                self.push_event("config:error", error.to_string());
                return Ok(None);
            }
        };
        persisted_root_config(&root_config).save(&self.config_path)?;
        config_state.dirty = false;
        config_state.close_guard_armed = false;
        config_state.draft = ConfigDraft::from_root_config(&root_config);
        config_state.sync_mcp_selection();
        self.apply_runtime_config_snapshot(&root_config);
        self.last_notice = Some("saved config".to_owned());
        self.push_event("config", format!("saved {}", self.config_path.display()));
        self.push_event(
            "config:model",
            format!(
                "default {}/{}; current session unchanged",
                root_config.agent.provider, root_config.agent.model
            ),
        );
        Ok(Some(AppAction::ConfigSaved {
            root_config: Box::new(root_config),
        }))
    }

    fn save_config_draft_and_close(&mut self) -> Result<Option<AppAction>> {
        let action = self.save_config_draft()?;
        if action.is_some() {
            self.config_state = None;
            self.last_notice = Some("saved config and closed".to_owned());
        }
        Ok(action)
    }

    fn apply_runtime_config_snapshot(&mut self, root_config: &RootConfig) {
        self.config_snapshot = Some(root_config.clone());
        self.permission_write_mode = root_config.permission.write_mode.as_str().to_owned();
        self.memory_config = root_config.memory.clone();
        self.compaction_config = root_config.compaction.clone();
        if self.current_session_entries.is_empty() {
            self.provider_name = root_config.agent.provider.clone();
            self.model_name = root_config.agent.model.clone();
        }
        self.refresh_memory_summary();
        self.recompute_compaction_status(false);
        self.refresh_usage_sidebar_cache();
    }

    fn complete_setup(&mut self) -> Result<Option<AppAction>> {
        let Some(state) = &mut self.setup_state else {
            return Ok(None);
        };

        if let Some(error) = validate_setup_state(state) {
            self.last_notice = Some(error.clone());
            self.push_event("setup:error", error);
            return Ok(None);
        }

        let root_config = match build_setup_root_config(state) {
            Ok(root_config) => {
                let persisted_root_config = persisted_root_config(&root_config);
                persisted_root_config.save(&state.config_path)?;
                root_config
            }
            Err(error) => {
                self.last_notice = Some(error.to_string());
                self.push_event("setup:error", error.to_string());
                return Ok(None);
            }
        };
        self.last_notice = Some(format!("saved config to {}", state.config_path.display()));
        Ok(Some(AppAction::SetupCompleted {
            config_path: state.config_path.clone(),
            root_config: Box::new(root_config),
        }))
    }

    fn session_view_lines(&self) -> Vec<String> {
        let mut lines = vec![
            format!("{} view", self.session_view_mode.label()),
            format!(
                "compact={}  prompt={}  cache={:.0}%",
                self.compaction_status,
                self.stats.last_prompt_tokens,
                self.cache_hit_ratio() * 100.0
            ),
        ];
        if self.is_busy {
            lines.push("running; durable view".to_owned());
        }

        lines.push(String::new());
        lines.extend(match self.session_view_mode {
            SessionViewMode::Provider => self.provider_projection_lines(),
            SessionViewMode::Audit => self.audit_log_lines(),
        });
        lines.push(String::new());
        lines.extend(self.recent_session_lines());
        lines.push(String::new());
        lines.push("V view  type filter  Enter/1-9 resume".to_owned());
        lines.push("Backspace edit  Esc clear  Arrows/Pg move".to_owned());
        lines
    }

    fn provider_projection_lines(&self) -> Vec<String> {
        if self.current_session_entries.is_empty() {
            return vec!["no provider messages".to_owned()];
        }

        let session = Session::from_entries(
            self.provider_name.clone(),
            self.model_name.clone(),
            self.current_session_entries.clone(),
        );
        let messages = session.messages();
        let mut lines = vec!["Provider:".to_owned()];
        if let Some(record) = &self.latest_compaction_record {
            lines.push(format!(
                "  summary: compacted={} tail={}",
                record.compacted_message_count, record.retained_tail_message_count
            ));
        }
        for message in messages {
            lines.push(render_model_message_line(&message));
        }
        if !self.is_busy {
            match session.compaction_preview(&self.compaction_config) {
                Ok(Some(preview)) => {
                    lines.push(String::new());
                    lines.extend(render_compaction_preview_lines(&preview));
                }
                Ok(None) => {
                    lines.push(String::new());
                    lines.push("/compact preview: nothing to fold".to_owned());
                }
                Err(error) => {
                    lines.push(String::new());
                    lines.push(format!("/compact preview unavailable: {error}"));
                }
            }
        }
        lines
    }

    fn audit_log_lines(&self) -> Vec<String> {
        if self.current_session_entries.is_empty() {
            return vec!["no audit entries".to_owned()];
        }

        let mut lines = vec!["Audit:".to_owned()];
        for entry in &self.current_session_entries {
            lines.push(render_session_log_entry(entry));
        }
        lines
    }

    fn recent_session_lines(&self) -> Vec<String> {
        self.recent_session_rows()
            .into_iter()
            .map(|row| match row {
                ActivityPanelRow::SessionHeader { filter, total } => {
                    format!("filter={filter} total={total}")
                }
                ActivityPanelRow::SessionItem {
                    index,
                    label,
                    current,
                    selected,
                    meta,
                } => format!(
                    "{} {}. {}{} {}",
                    if selected { ">" } else { " " },
                    index,
                    label,
                    if current { " (current)" } else { "" },
                    meta
                ),
                ActivityPanelRow::Empty { text } => text,
                ActivityPanelRow::Event { label, detail } => format!("{label} {detail}"),
            })
            .collect()
    }

    fn recent_session_rows(&self) -> Vec<ActivityPanelRow> {
        let filtered_indices = self.filtered_session_indices();
        let mut rows = vec![ActivityPanelRow::SessionHeader {
            filter: if self.session_history_filter.is_empty() {
                "-".to_owned()
            } else {
                self.session_history_filter.clone()
            },
            total: filtered_indices.len(),
        }];
        if filtered_indices.is_empty() {
            rows.push(ActivityPanelRow::Empty {
                text: "no matches".to_owned(),
            });
            return rows;
        }

        let start = self
            .session_history_selected
            .saturating_sub(self.session_history_visible_limit / 2)
            .min(filtered_indices.len().saturating_sub(1));
        let end = (start + self.session_history_visible_limit).min(filtered_indices.len());
        for (filtered_index, entry_index) in filtered_indices
            .iter()
            .enumerate()
            .skip(start)
            .take(end.saturating_sub(start))
        {
            let entry = &self.session_history[*entry_index];
            rows.push(ActivityPanelRow::SessionItem {
                index: filtered_index + 1,
                label: session_history_label(&entry.label),
                current: entry.path == self.session_log_path,
                selected: filtered_index == self.session_history_selected,
                meta: format!(
                    "{} · {}",
                    human_file_size(entry.bytes),
                    relative_age_label(entry.modified_epoch_secs)
                ),
            });
        }
        rows
    }

    fn sync_current_session_state(&mut self, entries: Vec<SessionLogEntry>) {
        self.stats = session_stats_from_entries(&entries);
        self.latest_compaction_record = latest_compaction_record(&entries);
        self.current_session_entries = entries;
        self.refresh_usage_sidebar_cache();
    }

    fn refresh_session_history(&mut self) {
        let mut sessions = Vec::new();
        if let Ok(entries) = fs::read_dir(&self.session_log_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                let is_jsonl = path
                    .extension()
                    .and_then(|value| value.to_str())
                    .map(|value| value.eq_ignore_ascii_case("jsonl"))
                    .unwrap_or(false);
                if !is_jsonl {
                    continue;
                }
                let modified = entry
                    .metadata()
                    .and_then(|metadata| metadata.modified())
                    .unwrap_or(SystemTime::UNIX_EPOCH);
                let label = path
                    .file_name()
                    .and_then(|value| value.to_str())
                    .unwrap_or("unknown")
                    .to_owned();
                let modified_epoch_secs = modified
                    .duration_since(UNIX_EPOCH)
                    .map(|duration| duration.as_secs())
                    .unwrap_or(0);
                let bytes = entry.metadata().map(|metadata| metadata.len()).unwrap_or(0);
                sessions.push((
                    modified,
                    SessionHistoryEntry {
                        path,
                        label,
                        modified_epoch_secs,
                        bytes,
                    },
                ));
            }
        }
        sessions.sort_by(|left, right| right.0.cmp(&left.0));
        self.session_history = sessions.into_iter().map(|(_, entry)| entry).collect();
        let current_index = self
            .session_history
            .iter()
            .position(|entry| entry.path == self.session_log_path)
            .unwrap_or(0);
        self.session_history_selected = self
            .filtered_session_indices()
            .iter()
            .position(|index| *index == current_index)
            .unwrap_or(0)
            .min(self.filtered_session_indices().len().saturating_sub(1));
    }

    fn refresh_memory_summary(&mut self) {
        match inspect_memory_documents(&self.workspace_root, &self.memory_config) {
            Ok(report) => {
                self.memory_enabled = report.enabled;
                self.memory_document_count = report.document_count;
                self.memory_last_status = "ok".to_owned();
            }
            Err(error) => {
                self.memory_enabled = self.memory_config.enabled;
                self.memory_document_count = 0;
                self.memory_last_status = error.to_string();
            }
        }
    }

    fn emit_session_history(&mut self) {
        self.push_timeline(
            TimelineRole::Notice,
            format!("Sessions in {}:", self.session_log_dir.display()),
        );
        if self.session_history.is_empty() {
            self.push_timeline(TimelineRole::Notice, "No saved sessions.");
            return;
        }
        let session_lines = self
            .session_history
            .iter()
            .take(10)
            .enumerate()
            .map(|(index, entry)| {
                format!(
                    "{}. {}{}",
                    index + 1,
                    entry.label,
                    if entry.path == self.session_log_path {
                        " (current)"
                    } else {
                        ""
                    }
                )
            })
            .collect::<Vec<_>>();
        for line in session_lines {
            self.push_timeline(TimelineRole::Notice, line);
        }
    }

    fn resolve_resume_target(&self, selector: &str) -> Option<PathBuf> {
        if self.session_history.is_empty() {
            return None;
        }

        let normalized = if selector.is_empty() {
            "latest"
        } else {
            selector
        };
        if normalized.eq_ignore_ascii_case("latest") {
            return self
                .session_history
                .iter()
                .find(|entry| entry.path != self.session_log_path)
                .or_else(|| self.session_history.first())
                .map(|entry| entry.path.clone());
        }

        normalized
            .parse::<usize>()
            .ok()
            .and_then(|index| index.checked_sub(1))
            .and_then(|index| self.filtered_session_indices().get(index).copied())
            .and_then(|index| self.session_history.get(index))
            .map(|entry| entry.path.clone())
    }

    fn restore_session_view(
        &mut self,
        session_log_path: PathBuf,
        provider_name: String,
        model_name: String,
        entries: Vec<SessionLogEntry>,
        notice: &str,
    ) {
        self.session_log_path = session_log_path;
        self.provider_name = provider_name;
        self.model_name = model_name;
        self.session_id = session_id_from_path(&self.session_log_path)
            .unwrap_or_else(|| Uuid::new_v4().to_string());
        self.sync_current_session_state(entries.clone());
        self.pending_approval = None;
        self.run_phase = RunPhase::Idle;
        self.refresh_memory_summary();
        self.recompute_compaction_status(false);
        self.timeline.clear();
        self.events.clear();
        self.reset_scroll();

        self.push_timeline(
            TimelineRole::System,
            format!("Resumed {}.", self.session_id),
        );
        self.push_timeline(TimelineRole::Notice, notice.to_owned());
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
        self.push_event("restore", format!("entries={}", entries.len()));

        for entry in entries {
            match entry {
                SessionLogEntry::User(message) => {
                    if let Some(content) = message.content {
                        self.push_timeline(TimelineRole::User, content);
                    }
                }
                SessionLogEntry::Assistant(message) => {
                    if let Some(content) = message.content
                        && !content.is_empty()
                    {
                        self.push_timeline(TimelineRole::Assistant, content);
                    }
                }
                SessionLogEntry::ToolResult(message) => {
                    if let Some(content) = message.content {
                        self.push_timeline(TimelineRole::Tool, format_tool_content_block(&content));
                    }
                }
                SessionLogEntry::Control(control) => {
                    self.push_event("control:restore", format!("{control:?}"));
                }
            }
        }

        self.last_notice = Some(notice.to_owned());
        self.refresh_session_history();
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

    fn reset_scroll(&mut self) {
        self.timeline_scroll_back = 0;
        self.approval_scroll_back = 0;
        self.activity_scroll_back = 0;
    }

    fn scroll_timeline(&mut self, delta: usize) {
        self.timeline_scroll_back = self
            .timeline_scroll_back
            .saturating_add(delta)
            .min(self.max_timeline_scroll_back());
    }

    fn unscroll_timeline(&mut self, delta: usize) {
        self.timeline_scroll_back = self.timeline_scroll_back.saturating_sub(delta);
    }

    fn scroll_timeline_to_top(&mut self) {
        self.timeline_scroll_back = self.max_timeline_scroll_back();
    }

    pub fn handle_mouse_scroll(&mut self, upward: bool) {
        let delta = 3;
        if self.pending_approval.is_some() {
            if upward {
                self.approval_scroll_back = self.approval_scroll_back.saturating_sub(delta);
            } else {
                self.approval_scroll_back = self.approval_scroll_back.saturating_add(delta);
            }
            return;
        }

        if upward {
            self.scroll_timeline(delta);
        } else {
            self.unscroll_timeline(delta);
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

    fn set_tool_preview_mode_from_command(&mut self, argument: &str) -> Result<Option<AppAction>> {
        let mode = match argument.trim().to_ascii_lowercase().as_str() {
            "" => self.tool_preview_mode,
            "brief" => ToolPreviewMode::Brief,
            "full" => ToolPreviewMode::Full,
            _ => {
                self.last_notice = Some("usage: /tools <brief|full>".to_owned());
                self.push_timeline(TimelineRole::Notice, "usage: /tools <brief|full>");
                return Ok(None);
            }
        };

        if mode == self.tool_preview_mode {
            self.last_notice = Some(format!("tool preview already {}", mode.as_str()));
            self.push_timeline(
                TimelineRole::Notice,
                format!("tool preview already {}", mode.as_str()),
            );
            return Ok(None);
        }

        self.tool_preview_mode = mode;
        self.rebuild_timeline_render_cache();
        self.refresh_usage_sidebar_cache();
        self.last_notice = Some(format!("tool preview = {}", mode.as_str()));
        self.push_event("tool:view", mode.as_str());
        self.push_timeline(
            TimelineRole::Notice,
            format!("tool preview -> {}", mode.as_str()),
        );
        Ok(None)
    }

    fn set_tool_card_view_from_command(&mut self, argument: &str) -> Result<Option<AppAction>> {
        let command = argument.trim().to_ascii_lowercase();
        let action = if command.is_empty() {
            "latest"
        } else {
            command.as_str()
        };
        let Some(indices) = self.tool_timeline_entry_indices() else {
            self.last_notice = Some("no tool cards yet".to_owned());
            self.push_timeline(TimelineRole::Notice, "no tool cards yet");
            return Ok(None);
        };

        match action {
            "latest" => {
                self.selected_tool_timeline_entry = indices.last().copied();
                self.push_event("tool:select", "latest");
                self.last_notice = Some(self.tool_card_status_line());
            }
            "next" => {
                self.selected_tool_timeline_entry = Some(self.next_tool_entry(&indices, true));
                self.push_event("tool:select", "next");
                self.last_notice = Some(self.tool_card_status_line());
            }
            "prev" => {
                self.selected_tool_timeline_entry = Some(self.next_tool_entry(&indices, false));
                self.push_event("tool:select", "prev");
                self.last_notice = Some(self.tool_card_status_line());
            }
            "open" => {
                let selected = self.ensure_selected_tool_entry(&indices);
                self.expanded_tool_timeline_entries.insert(selected);
                self.rebuild_timeline_render_cache();
                self.push_event("tool:view", "open");
                self.last_notice = Some(self.tool_card_status_line());
            }
            "close" => {
                let selected = self.ensure_selected_tool_entry(&indices);
                self.expanded_tool_timeline_entries.remove(&selected);
                self.rebuild_timeline_render_cache();
                self.push_event("tool:view", "close");
                self.last_notice = Some(self.tool_card_status_line());
            }
            "toggle" => {
                let selected = self.ensure_selected_tool_entry(&indices);
                if !self.expanded_tool_timeline_entries.insert(selected) {
                    self.expanded_tool_timeline_entries.remove(&selected);
                }
                self.rebuild_timeline_render_cache();
                self.push_event("tool:view", "toggle");
                self.last_notice = Some(self.tool_card_status_line());
            }
            _ => {
                self.last_notice =
                    Some("usage: /tool <latest|next|prev|open|close|toggle>".to_owned());
                self.push_timeline(
                    TimelineRole::Notice,
                    "usage: /tool <latest|next|prev|open|close|toggle>",
                );
            }
        }
        Ok(None)
    }

    fn tool_timeline_entry_indices(&self) -> Option<Vec<usize>> {
        let indices = self
            .timeline
            .iter()
            .enumerate()
            .filter_map(|(index, entry)| (entry.role == TimelineRole::Tool).then_some(index))
            .collect::<Vec<_>>();
        (!indices.is_empty()).then_some(indices)
    }

    fn ensure_selected_tool_entry(&mut self, indices: &[usize]) -> usize {
        if let Some(selected) = self
            .selected_tool_timeline_entry
            .filter(|index| indices.contains(index))
        {
            return selected;
        }
        let latest = *indices.last().expect("tool entry indices are non-empty");
        self.selected_tool_timeline_entry = Some(latest);
        latest
    }

    fn next_tool_entry(&mut self, indices: &[usize], forward: bool) -> usize {
        let current = self.ensure_selected_tool_entry(indices);
        let position = indices
            .iter()
            .position(|index| *index == current)
            .unwrap_or(0);
        let next_position = if forward {
            (position + 1) % indices.len()
        } else if position == 0 {
            indices.len() - 1
        } else {
            position - 1
        };
        indices[next_position]
    }

    fn tool_card_status_line(&self) -> String {
        let Some(indices) = self.tool_timeline_entry_indices() else {
            return "tools: none".to_owned();
        };
        let selected = self
            .selected_tool_timeline_entry
            .and_then(|entry| indices.iter().position(|index| *index == entry))
            .map(|position| position + 1)
            .unwrap_or(indices.len());
        let selected_entry = self
            .selected_tool_timeline_entry
            .unwrap_or(*indices.last().unwrap_or(&0));
        let open = self
            .expanded_tool_timeline_entries
            .contains(&selected_entry);
        format!(
            "tool card {selected}/{} {}",
            indices.len(),
            if open { "open" } else { "brief" }
        )
    }

    fn filtered_session_indices(&self) -> Vec<usize> {
        let filter = self.session_history_filter.to_ascii_lowercase();
        self.session_history
            .iter()
            .enumerate()
            .filter_map(|(index, entry)| {
                let include =
                    filter.is_empty() || entry.label.to_ascii_lowercase().contains(&filter);
                include.then_some(index)
            })
            .collect()
    }

    fn selected_approval_diff(&self) -> Option<&str> {
        let preview = self
            .pending_approval
            .as_ref()
            .and_then(|pending| pending.preview.as_ref())?;
        preview
            .file_diffs
            .get(self.approval_selected_file_index)
            .map(|file| file.diff.as_str())
            .or_else(|| (!preview.body.is_empty()).then_some(preview.body.as_str()))
    }

    fn approval_hunk_positions(&self) -> Vec<usize> {
        self.selected_approval_diff()
            .map(|diff| {
                diff.lines()
                    .enumerate()
                    .filter_map(|(index, line)| line.starts_with("@@").then_some(index))
                    .collect()
            })
            .unwrap_or_default()
    }

    fn transform_approval_diff(&self, diff: &str) -> String {
        match self.approval_diff_mode {
            ApprovalDiffMode::Full => diff.to_owned(),
            ApprovalDiffMode::CurrentHunk => self.extract_current_hunk(diff),
            ApprovalDiffMode::ChangedOnly => self.extract_changed_only(diff),
        }
    }

    fn extract_current_hunk(&self, diff: &str) -> String {
        let lines = diff.lines().collect::<Vec<_>>();
        let hunk_positions = self.approval_hunk_positions();
        if hunk_positions.is_empty() {
            return diff.to_owned();
        }
        let hunk_index = self
            .approval_selected_hunk_index
            .min(hunk_positions.len().saturating_sub(1));
        let start = hunk_positions[hunk_index];
        let end = hunk_positions
            .get(hunk_index + 1)
            .copied()
            .unwrap_or(lines.len());

        let mut out = Vec::new();
        let header_limit = start.min(2);
        out.extend(lines.iter().take(header_limit).copied());
        if header_limit < start {
            out.push("...");
        }
        out.extend(lines[start..end].iter().copied());
        out.join("\n")
    }

    fn extract_changed_only(&self, diff: &str) -> String {
        diff.lines()
            .filter(|line| {
                line.starts_with("---")
                    || line.starts_with("+++")
                    || line.starts_with("@@")
                    || (line.starts_with('+') && !line.starts_with("+++"))
                    || (line.starts_with('-') && !line.starts_with("---"))
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn jump_approval_hunk(&mut self, next: bool) {
        let hunk_positions = self.approval_hunk_positions();
        if hunk_positions.is_empty() {
            return;
        }
        if next {
            self.approval_selected_hunk_index =
                (self.approval_selected_hunk_index + 1).min(hunk_positions.len() - 1);
        } else {
            self.approval_selected_hunk_index = self.approval_selected_hunk_index.saturating_sub(1);
        }
        self.approval_scroll_back = hunk_positions[self.approval_selected_hunk_index];
    }

    fn switch_approval_file(&mut self, next: bool) {
        let Some(preview) = self
            .pending_approval
            .as_ref()
            .and_then(|pending| pending.preview.as_ref())
        else {
            return;
        };
        if preview.file_diffs.is_empty() {
            return;
        }

        if next {
            self.approval_selected_file_index =
                (self.approval_selected_file_index + 1).min(preview.file_diffs.len() - 1);
        } else {
            self.approval_selected_file_index = self.approval_selected_file_index.saturating_sub(1);
        }
        self.approval_selected_hunk_index = 0;
        self.approval_scroll_back = 0;
    }

    fn record_input_history(&mut self, prompt: String) {
        if self
            .input_history
            .last()
            .map(|last| last == &prompt)
            .unwrap_or(false)
        {
            return;
        }
        self.input_history.push(prompt);
        if self.input_history.len() > 100 {
            self.input_history.remove(0);
        }
    }

    fn reset_input_history_navigation(&mut self) {
        self.input_history_index = None;
        self.input_history_draft = None;
    }

    fn navigate_input_history(&mut self, older: bool) {
        if self.input_history.is_empty() {
            return;
        }

        if older {
            match self.input_history_index {
                Some(0) => {}
                Some(index) => {
                    self.input_history_index = Some(index - 1);
                }
                None => {
                    self.input_history_draft = Some(self.input.clone());
                    self.input_history_index = Some(self.input_history.len() - 1);
                }
            }
        } else {
            match self.input_history_index {
                Some(index) if index + 1 < self.input_history.len() => {
                    self.input_history_index = Some(index + 1);
                }
                Some(_) => {
                    let draft = self.input_history_draft.take().unwrap_or_default();
                    self.set_input_and_cursor(draft);
                    self.input_history_index = None;
                    self.reset_slash_selector();
                    return;
                }
                None => return,
            }
        }

        if let Some(index) = self.input_history_index
            && let Some(value) = self.input_history.get(index)
        {
            self.set_input_and_cursor(value.clone());
            self.reset_slash_selector();
        }
    }

    fn scroll_active_pane(&mut self, delta: usize) {
        match self.active_pane {
            PaneFocus::Composer => self.scroll_timeline(delta),
            PaneFocus::Activity => {
                if self.pending_approval.is_some() {
                    self.approval_scroll_back = self.approval_scroll_back.saturating_sub(delta);
                } else {
                    self.activity_scroll_back = self.activity_scroll_back.saturating_add(delta);
                }
            }
        }
    }

    fn unscroll_active_pane(&mut self, delta: usize) {
        match self.active_pane {
            PaneFocus::Composer => self.unscroll_timeline(delta),
            PaneFocus::Activity => {
                if self.pending_approval.is_some() {
                    self.approval_scroll_back = self.approval_scroll_back.saturating_add(delta);
                } else {
                    self.activity_scroll_back = self.activity_scroll_back.saturating_sub(delta);
                }
            }
        }
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

fn session_id_from_path(path: &Path) -> Option<String> {
    let stem = path.file_stem()?.to_str()?;
    stem.strip_prefix("session-").map(ToOwned::to_owned)
}

fn current_focus_label(app: &AppState) -> String {
    match app.active_pane {
        PaneFocus::Activity => format!("activity:{}", app.sidebar_selected_card.label()),
        other => other.label().to_owned(),
    }
}

fn session_history_label(label: &str) -> String {
    label
        .strip_prefix("session-")
        .and_then(|value| value.strip_suffix(".jsonl"))
        .map(short_session_token)
        .unwrap_or_else(|| truncate_session_view_text(label, 24))
}

fn short_session_token(token: &str) -> String {
    token.chars().take(8).collect()
}

fn human_file_size(bytes: u64) -> String {
    const KB: f64 = 1024.0;
    const MB: f64 = 1024.0 * 1024.0;
    if bytes < 1024 {
        return format!("{bytes} B");
    }
    if bytes < 1024 * 1024 {
        return format!("{:.1} KB", bytes as f64 / KB);
    }
    format!("{:.1} MB", bytes as f64 / MB)
}

fn relative_age_label(modified_epoch_secs: u64) -> String {
    let now_epoch_secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(modified_epoch_secs);
    let delta = now_epoch_secs.saturating_sub(modified_epoch_secs);
    match delta {
        0..=59 => format!("{delta}s ago"),
        60..=3599 => format!("{}m ago", delta / 60),
        3600..=86399 => format!("{}h ago", delta / 3600),
        _ => format!("{}d ago", delta / 86_400),
    }
}

fn summarize_error(error: &str) -> String {
    error
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && *line != "Caused by:")
        .map(strip_error_chain_prefix)
        .next_back()
        .filter(|line| !line.is_empty())
        .unwrap_or_else(|| error.trim())
        .to_owned()
}

fn strip_error_chain_prefix(line: &str) -> &str {
    if let Some((prefix, rest)) = line.split_once(':')
        && prefix.trim().chars().all(|char| char.is_ascii_digit())
    {
        return rest.trim();
    }
    line.trim()
}

fn render_model_message_line(message: &ModelMessage) -> String {
    let role = match message.role {
        termquill_kernel::MessageRole::System => "system",
        termquill_kernel::MessageRole::User => "user",
        termquill_kernel::MessageRole::Assistant => "assistant",
        termquill_kernel::MessageRole::Tool => "tool",
    };
    if !message.tool_calls.is_empty() {
        let names = message
            .tool_calls
            .iter()
            .map(|call| call.name.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        return format!("[{role}] tool_calls [{names}]");
    }

    let content = truncate_session_view_text(message.content.as_deref().unwrap_or_default(), 160);
    if matches!(message.role, termquill_kernel::MessageRole::Tool) {
        format!(
            "[{role}] {} => {content}",
            message.tool_call_id.as_deref().unwrap_or("unknown")
        )
    } else {
        format!("[{role}] {content}")
    }
}

fn render_session_log_entry(entry: &SessionLogEntry) -> String {
    match entry {
        SessionLogEntry::User(message)
        | SessionLogEntry::Assistant(message)
        | SessionLogEntry::ToolResult(message) => render_model_message_line(message),
        SessionLogEntry::Control(control) => match control {
            ControlEntry::SessionIdentity {
                provider_name,
                model_name,
            } => format!("[ctl] session {provider_name}/{model_name}"),
            ControlEntry::ContinuationStateSaved(state) => format!(
                "[ctl] cont {} msg={}",
                state.state_kind,
                state.message_id.as_deref().unwrap_or("-")
            ),
            ControlEntry::ResponseHandleTracked(handle) => format!(
                "[ctl] response {}",
                truncate_session_view_text(&handle.response_id, 48)
            ),
            ControlEntry::BackgroundTaskTracked(handle) => format!("[ctl] task {}", handle.task_id),
            ControlEntry::PrefixSnapshotCaptured(snapshot) => format!(
                "[ctl] prefix sha={} mem={}",
                truncate_session_view_text(&snapshot.sha256, 16),
                truncate_session_view_text(&snapshot.memory_fingerprint, 16)
            ),
            ControlEntry::UsageSnapshot(usage) => format!(
                "[ctl] usage p={} c={} hit={} miss={}",
                usage.prompt_tokens,
                usage.completion_tokens,
                usage.cache_hit_tokens,
                usage.cache_miss_tokens
            ),
            ControlEntry::CompactionApplied(record) => format!(
                "[ctl] compacted={} tail={}",
                record.compacted_message_count, record.retained_tail_message_count
            ),
            ControlEntry::Note { kind, .. } => format!("[ctl] note {kind}"),
        },
    }
}

fn truncate_session_view_text(text: &str, max_chars: usize) -> String {
    let normalized = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if normalized.chars().count() <= max_chars {
        return normalized;
    }
    let truncated = normalized.chars().take(max_chars).collect::<String>();
    format!("{truncated}...")
}

fn render_compaction_preview_lines(preview: &CompactionPreview) -> Vec<String> {
    let mut lines = vec![
        format!(
            "/compact preview: fold {}",
            preview.record.compacted_message_count
        ),
        "Before:".to_owned(),
    ];
    for message in &preview.folded_messages {
        lines.push(format!("  {}", render_model_message_line(message)));
    }
    lines.push("After:".to_owned());
    for message in &preview.projected_messages {
        lines.push(format!("  {}", render_model_message_line(message)));
    }
    lines
}

fn approval_access_label(read_only: bool) -> &'static str {
    if read_only { "read" } else { "write" }
}

fn approval_diff_line_kind(line: &str) -> ApprovalDiffLineKind {
    if line.starts_with("---")
        || line.starts_with("+++")
        || line.starts_with("diff ")
        || line.starts_with("index ")
    {
        ApprovalDiffLineKind::Header
    } else if line.starts_with("@@") {
        ApprovalDiffLineKind::Hunk
    } else if line.starts_with('+') && !line.starts_with("+++") {
        ApprovalDiffLineKind::Added
    } else if line.starts_with('-') && !line.starts_with("---") {
        ApprovalDiffLineKind::Removed
    } else {
        ApprovalDiffLineKind::Context
    }
}

fn setup_field_label(field: SetupField) -> &'static str {
    match field {
        SetupField::TrustCurrentFolder => "trust_current_folder",
        SetupField::Model => "model",
        SetupField::ApiKey => "api_key",
        SetupField::Save => "save",
    }
}

fn setup_bool_label(enabled: bool) -> &'static str {
    if enabled { "on" } else { "off" }
}

fn render_setup_value_row(
    field: SetupField,
    selected_field: SetupField,
    label: &str,
    value: &str,
    action: Option<&str>,
) -> String {
    if let Some(action) = action.filter(|_| field == selected_field) {
        format!(
            "{} {:<22}: {}  [{}]",
            if field == selected_field { ">" } else { " " },
            label,
            value,
            action
        )
    } else {
        format!(
            "{} {:<22}: {}",
            if field == selected_field { ">" } else { " " },
            label,
            value
        )
    }
}

fn render_setup_toggle_row(
    field: SetupField,
    selected_field: SetupField,
    label: &str,
    enabled: bool,
) -> String {
    render_setup_value_row(
        field,
        selected_field,
        label,
        setup_bool_label(enabled),
        None,
    )
}

fn render_setup_action_row(field: SetupField, selected_field: SetupField, label: &str) -> String {
    format!(
        "{} [{}]",
        if field == selected_field { ">" } else { " " },
        label
    )
}

fn validate_setup_state(state: &SetupState) -> Option<String> {
    if !state.trusted_current_folder {
        return Some("trust the current folder before starting termquill".to_owned());
    }
    if state.model.trim().is_empty() {
        return Some("model cannot be empty".to_owned());
    }
    if state.api_key.trim().is_empty() && env::var(TERMQUILL_API_KEY_ENV).is_err() {
        return Some(format!("provide api_key or export {TERMQUILL_API_KEY_ENV}"));
    }

    None
}

fn build_setup_root_config(state: &SetupState) -> Result<RootConfig> {
    if !state.trusted_current_folder {
        bail!("trust the current folder before starting termquill");
    }
    let model = state.model.trim();
    if model.is_empty() {
        bail!("model cannot be empty");
    }
    if state.api_key.trim().is_empty() && env::var(TERMQUILL_API_KEY_ENV).is_err() {
        bail!("provide api_key or export {TERMQUILL_API_KEY_ENV}");
    }

    let provider_config = DeepSeekProviderConfig {
        base_url: "https://api.deepseek.com".to_owned(),
        beta_base_url: "https://api.deepseek.com/beta".to_owned(),
        anthropic_base_url: "https://api.deepseek.com/anthropic".to_owned(),
        model: model.to_owned(),
        api_key: (!state.api_key.trim().is_empty()).then(|| state.api_key.clone()),
        user_id_strategy: Some("stable_per_end_user".to_owned()),
        strict_tools_mode: StrictToolsMode::Auto,
        fim_model: "deepseek-v4-pro".to_owned(),
        request_timeout_secs: 120,
    };

    let provider_value = serialize_deepseek_provider_value(&provider_config)?;
    Ok(RootConfig {
        workspace: WorkspaceConfig {
            root: ".".to_owned(),
        },
        session: SessionConfig {
            log_dir: ".termquill/sessions".to_owned(),
        },
        agent: AgentConfig {
            provider: "deepseek".to_owned(),
            model: model.to_owned(),
            max_turns: 8,
            tool_timeout_secs: 30,
        },
        permission: PermissionConfig {
            write_mode: ApprovalMode::Ask,
            rules: Vec::new(),
        },
        memory: MemoryConfig { enabled: true },
        compaction: CompactionConfig {
            enabled: true,
            soft_threshold_ratio: 0.5,
            hard_threshold_ratio: 0.8,
            context_window_tokens: Some(128000),
            tail_messages: 6,
        },
        providers: std::collections::BTreeMap::from([("deepseek".to_owned(), provider_value)]),
        mcp_servers: Vec::new(),
    })
}

fn load_deepseek_provider_config(root_config: &RootConfig) -> Option<DeepSeekProviderConfig> {
    root_config
        .providers
        .get("deepseek")
        .cloned()
        .and_then(|value| serde_json::from_value(value).ok())
}

fn default_deepseek_provider_config(model: &str) -> DeepSeekProviderConfig {
    DeepSeekProviderConfig {
        base_url: "https://api.deepseek.com".to_owned(),
        beta_base_url: "https://api.deepseek.com/beta".to_owned(),
        anthropic_base_url: "https://api.deepseek.com/anthropic".to_owned(),
        model: model.to_owned(),
        api_key: None,
        user_id_strategy: Some("stable_per_end_user".to_owned()),
        strict_tools_mode: StrictToolsMode::Auto,
        fim_model: "deepseek-v4-pro".to_owned(),
        request_timeout_secs: 120,
    }
}

fn serialize_deepseek_provider_value(
    provider_config: &DeepSeekProviderConfig,
) -> Result<serde_json::Value> {
    let mut value = serde_json::to_value(provider_config)
        .map_err(|error| anyhow!("failed to serialize deepseek provider config: {error}"))?;
    if let Some(object) = value.as_object_mut() {
        object.retain(|_, entry| !entry.is_null());
    }
    Ok(value)
}

fn cycle_approval_mode(mode: ApprovalMode) -> ApprovalMode {
    match mode {
        ApprovalMode::Allow => ApprovalMode::Ask,
        ApprovalMode::Ask => ApprovalMode::Deny,
        ApprovalMode::Deny => ApprovalMode::Allow,
    }
}

fn parse_reasoning_effort(value: &str) -> Option<ReasoningEffort> {
    match value.trim().to_ascii_lowercase().as_str() {
        "low" => Some(ReasoningEffort::Low),
        "medium" | "med" => Some(ReasoningEffort::Medium),
        "high" => Some(ReasoningEffort::High),
        "max" => Some(ReasoningEffort::Max),
        _ => None,
    }
}

fn sidebar_width_for_terminal(total_width: usize) -> usize {
    let min = if total_width < 72 { 16 } else { 24 };
    let max = if total_width < 72 { 24 } else { 42 };
    ((total_width * 30) / 100).clamp(min, max)
}

fn normalize_runtime_model(value: &str) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return None;
    }

    let normalized = match trimmed.to_ascii_lowercase().as_str() {
        "flash" | "v4-flash" => "deepseek-v4-flash".to_owned(),
        "pro" | "v4-pro" => "deepseek-v4-pro".to_owned(),
        _ => trimmed.to_owned(),
    };
    Some(normalized)
}

fn normalize_command_prefix_character(character: char) -> Option<char> {
    match character {
        '/' | '、' => Some('/'),
        _ => None,
    }
}

fn format_token_count(tokens: u64) -> String {
    let digits = tokens.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (index, character) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            out.push(',');
        }
        out.push(character);
    }
    out
}

fn format_token_compact(tokens: u64) -> String {
    if tokens >= 1_000_000 {
        return format!("{:.1}M", tokens as f64 / 1_000_000.0);
    }
    if tokens >= 1_000 {
        return format!("{:.1}K", tokens as f64 / 1_000.0);
    }
    tokens.to_string()
}

fn line_has_visible_content(line: &Line<'_>) -> bool {
    line.spans.iter().any(|span| {
        !span
            .content
            .as_ref()
            .trim_matches(|character: char| character.is_whitespace() || character == '▌')
            .is_empty()
    })
}

fn plain_line_text(line: &Line<'_>) -> String {
    line.spans
        .iter()
        .map(|span| span.content.as_ref())
        .collect::<String>()
}

fn hash_timeline_line(seed: u64, line: &str) -> u64 {
    let mut hash = seed;
    for byte in line.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(1_099_511_628_211);
    }
    hash ^= 0xff;
    hash.wrapping_mul(1_099_511_628_211)
}

fn ratio_to_percent(ratio: f32) -> u32 {
    (ratio * 100.0).round().clamp(0.0, 999.0) as u32
}

fn format_tool_result_block(result: &ToolResult) -> String {
    format_tool_preview_payload(
        Some(result.call_id.as_str()),
        result.tool_name.as_str(),
        if result.is_error { "error" } else { "ok" },
        &result.content,
        Some(&result.metadata),
    )
}

fn format_tool_content_block(content: &str) -> String {
    format_tool_preview_payload(None, "tool_result", "ok", content, None)
}

fn format_tool_preview_payload(
    call_id: Option<&str>,
    tool_name: &str,
    status: &str,
    content: &str,
    metadata: Option<&ToolResultMeta>,
) -> String {
    let preview_value = tool_preview_value(content);
    let (preview_kind, preview_source) =
        tool_preview_source(tool_name, content, preview_value.as_ref());
    let all_lines = if preview_source.is_empty() {
        Vec::new()
    } else {
        preview_source
            .lines()
            .map(str::to_owned)
            .collect::<Vec<_>>()
    };
    let total_lines = all_lines.len();
    let preview_lines = select_tool_preview_lines(tool_name, &all_lines);
    let hidden_lines = total_lines.saturating_sub(preview_lines.len());
    let bytes = metadata
        .and_then(|value| value.bytes)
        .unwrap_or(content.len() as u64);
    let metadata_line = metadata
        .and_then(render_tool_metadata_summary)
        .filter(|value| !value.is_empty());

    let mut object = serde_json::Map::new();
    if let Some(call_id) = call_id {
        object.insert(
            "call_id".to_owned(),
            serde_json::Value::String(call_id.to_owned()),
        );
    }
    object.insert(
        "tool_name".to_owned(),
        serde_json::Value::String(tool_name.to_owned()),
    );
    object.insert(
        "status".to_owned(),
        serde_json::Value::String(status.to_owned()),
    );
    object.insert(
        "preview_kind".to_owned(),
        serde_json::Value::String(preview_kind.to_owned()),
    );
    object.insert(
        "summary".to_owned(),
        serde_json::Value::String(format_tool_preview_summary(
            tool_name,
            total_lines,
            preview_lines.len(),
            hidden_lines,
            bytes,
        )),
    );
    object.insert(
        "preview_lines".to_owned(),
        serde_json::Value::Array(
            preview_lines
                .iter()
                .cloned()
                .map(serde_json::Value::String)
                .collect(),
        ),
    );
    object.insert(
        "hidden_lines".to_owned(),
        serde_json::Value::Number(hidden_lines.into()),
    );
    if let Some(metadata_line) = metadata_line {
        object.insert(
            "metadata_line".to_owned(),
            serde_json::Value::String(metadata_line),
        );
    }
    if let Some(metadata) = metadata {
        object.insert(
            "metadata".to_owned(),
            serde_json::to_value(metadata).unwrap_or(serde_json::Value::Null),
        );
    }
    if let Some(preview_value) = preview_value {
        object.insert(
            "preview_value".to_owned(),
            compact_preview_value(&preview_value, 0),
        );
    }
    serde_json::to_string(&serde_json::Value::Object(object)).unwrap_or_else(|_| content.to_owned())
}

fn parse_tool_content_value(content: &str) -> serde_json::Value {
    serde_json::from_str(content).unwrap_or_else(|_| serde_json::Value::String(content.to_owned()))
}

fn tool_preview_value(content: &str) -> Option<serde_json::Value> {
    let value = parse_tool_content_value(content);
    matches!(
        value,
        serde_json::Value::Array(_) | serde_json::Value::Object(_)
    )
    .then_some(value)
}

fn tool_preview_source(
    tool_name: &str,
    content: &str,
    preview_value: Option<&serde_json::Value>,
) -> (&'static str, String) {
    if let Some(value) = preview_value {
        let pretty = serde_json::to_string_pretty(value).unwrap_or_else(|_| content.to_owned());
        return ("json", pretty);
    }
    if tool_name == "read_file" || looks_like_markdown_document(content) {
        return ("markdown", content.to_owned());
    }
    ("text", content.to_owned())
}

fn compact_preview_value(value: &serde_json::Value, depth: usize) -> serde_json::Value {
    const MAX_DEPTH: usize = 3;
    const MAX_ITEMS: usize = 10;
    const MAX_STRING_CHARS: usize = 160;

    match value {
        serde_json::Value::Array(items) => {
            if depth >= MAX_DEPTH {
                return serde_json::Value::String(format!("… {} items", items.len()));
            }
            let limit = items.len().min(MAX_ITEMS);
            let mut compacted = items
                .iter()
                .take(limit)
                .map(|item| compact_preview_value(item, depth + 1))
                .collect::<Vec<_>>();
            if items.len() > limit {
                compacted.push(serde_json::Value::String(format!(
                    "… {} more items",
                    items.len() - limit
                )));
            }
            serde_json::Value::Array(compacted)
        }
        serde_json::Value::Object(object) => {
            if depth >= MAX_DEPTH {
                return serde_json::Value::String(format!("… {} keys", object.len()));
            }
            let limit = object.len().min(MAX_ITEMS);
            let mut compacted = serde_json::Map::new();
            for (key, nested) in object.iter().take(limit) {
                compacted.insert(key.clone(), compact_preview_value(nested, depth + 1));
            }
            if object.len() > limit {
                compacted.insert(
                    "…".to_owned(),
                    serde_json::Value::String(format!("{} more keys", object.len() - limit)),
                );
            }
            serde_json::Value::Object(compacted)
        }
        serde_json::Value::String(text) => {
            let truncated = text.chars().take(MAX_STRING_CHARS).collect::<String>();
            if text.chars().count() > MAX_STRING_CHARS {
                serde_json::Value::String(format!("{truncated}..."))
            } else {
                serde_json::Value::String(truncated)
            }
        }
        _ => value.clone(),
    }
}

fn select_tool_preview_lines(tool_name: &str, lines: &[String]) -> Vec<String> {
    let limit = tool_preview_limit(tool_name);
    if lines.len() <= limit {
        return lines.to_vec();
    }
    if tool_name == "bash" {
        return lines[lines.len().saturating_sub(limit)..].to_vec();
    }
    lines[..limit].to_vec()
}

fn tool_preview_limit(tool_name: &str) -> usize {
    match tool_name {
        "bash" => 16,
        "read_file" => 18,
        "grep" | "glob" | "ls" => 14,
        _ => 12,
    }
}

fn format_tool_preview_summary(
    tool_name: &str,
    total_lines: usize,
    shown_lines: usize,
    hidden_lines: usize,
    bytes: u64,
) -> String {
    let line_label = if total_lines == 1 { "line" } else { "lines" };
    let size = format_bytes(bytes);
    if hidden_lines == 0 {
        return format!("{total_lines} {line_label} · {size}");
    }
    if tool_name == "bash" {
        return format!("last {shown_lines}/{total_lines} {line_label} · {size}");
    }
    format!("first {shown_lines}/{total_lines} {line_label} · {size}")
}

fn format_bytes(bytes: u64) -> String {
    if bytes < 1_000 {
        return format!("{bytes} B");
    }
    if bytes < 1_000_000 {
        return format!("{:.1} KB", bytes as f64 / 1_000.0);
    }
    format!("{:.1} MB", bytes as f64 / 1_000_000.0)
}

fn looks_like_markdown_document(content: &str) -> bool {
    let trimmed = content.trim();
    if trimmed.is_empty() {
        return false;
    }
    trimmed.starts_with('#')
        || trimmed.contains("\n#")
        || trimmed.contains("```")
        || trimmed.contains("\n- ")
        || trimmed.contains("\n* ")
        || trimmed.contains("\n1. ")
        || (trimmed.contains('|') && trimmed.contains("---"))
}

fn render_tool_metadata_summary(metadata: &ToolResultMeta) -> Option<String> {
    let mut parts = Vec::new();
    if let Some(exit_code) = metadata.exit_code {
        parts.push(format!("exit={exit_code}"));
    }
    if let Some(bytes) = metadata.bytes {
        parts.push(format!("bytes={bytes}"));
    }
    if metadata.truncated {
        parts.push("truncated".to_owned());
    }
    if !metadata.changed_files.is_empty() {
        parts.push(format!("files={}", metadata.changed_files.len()));
    }
    if parts.is_empty() {
        return None;
    }
    Some(parts.join(" · "))
}

fn render_config_value_row(state: &ConfigState, field: ConfigField) -> String {
    let selected = !state.footer_selected && state.selected_field == Some(field);
    let marker = if selected { ">" } else { " " };
    let action = if selected && state.editing_field() != Some(field) {
        field.action_label()
    } else {
        ""
    };

    if action.is_empty() {
        format!(
            "{marker} {:<22}: {}",
            field.label(),
            state.display_value(field)
        )
    } else {
        format!(
            "{marker} {:<22}: {}  [{}]",
            field.label(),
            state.display_value(field),
            action
        )
    }
}

fn build_model_picker_options(current: &str, remote: Vec<String>) -> Vec<String> {
    let mut options = if remote.is_empty() {
        KNOWN_MODEL_IDS
            .iter()
            .map(|model| (*model).to_owned())
            .collect::<Vec<_>>()
    } else {
        remote
    };
    let trimmed = current.trim();
    if !trimmed.is_empty() && !options.iter().any(|option| option == trimmed) {
        options.push(trimmed.to_owned());
    }
    options
}

fn fetch_remote_model_ids(config: &DeepSeekProviderConfig) -> Result<Vec<String>> {
    let Some(api_key) = resolve_provider_api_key(config) else {
        bail!("missing auth");
    };
    let url = format!("{}/models", config.base_url.trim_end_matches('/'));
    let timeout_secs = config.request_timeout_secs.clamp(1, 5);
    let client = BlockingClient::builder()
        .timeout(Duration::from_secs(timeout_secs))
        .build()
        .map_err(|error| anyhow!("failed to build model-list client: {error}"))?;
    let response = client
        .get(url)
        .bearer_auth(api_key)
        .header("Accept", "application/json")
        .send()
        .and_then(reqwest::blocking::Response::error_for_status)
        .map_err(|error| anyhow!("failed to fetch provider models: {error}"))?;
    let payload = response
        .json::<serde_json::Value>()
        .map_err(|error| anyhow!("failed to decode provider models: {error}"))?;
    let models = parse_remote_model_ids(&payload);
    if models.is_empty() {
        bail!("provider returned no model ids");
    }
    Ok(models)
}

fn fetch_provider_balance_snapshot(config: &DeepSeekProviderConfig) -> Result<BalanceSnapshot> {
    let Some(api_key) = resolve_provider_api_key(config) else {
        bail!("missing auth");
    };
    let url = format!("{}/user/balance", config.base_url.trim_end_matches('/'));
    let timeout_secs = config.request_timeout_secs.clamp(1, 5);
    let client = BlockingClient::builder()
        .timeout(Duration::from_secs(timeout_secs))
        .build()
        .map_err(|error| anyhow!("failed to build balance client: {error}"))?;
    let payload = client
        .get(url)
        .bearer_auth(api_key)
        .header("Accept", "application/json")
        .send()
        .and_then(reqwest::blocking::Response::error_for_status)
        .map_err(|error| anyhow!("failed to fetch balance: {error}"))?
        .json::<serde_json::Value>()
        .map_err(|error| anyhow!("failed to decode balance payload: {error}"))?;

    let available = payload
        .get("is_available")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    let Some(items) = payload
        .get("balance_infos")
        .and_then(serde_json::Value::as_array)
    else {
        bail!("provider returned no balance infos");
    };
    let primary = items
        .iter()
        .filter_map(|item| {
            let currency = item.get("currency")?.as_str()?.to_owned();
            let total = item
                .get("total_balance")?
                .as_str()
                .and_then(|value| value.parse::<f64>().ok())?;
            Some((currency, total))
        })
        .max_by(|left, right| left.1.total_cmp(&right.1));

    let Some((currency, total)) = primary else {
        bail!("provider returned no parseable balances");
    };
    Ok(BalanceSnapshot {
        total: Some(total),
        currency: Some(currency.clone()),
        available,
        status: if available {
            format!("{currency} {total:.2}")
        } else {
            "unavailable".to_owned()
        },
    })
}

fn parse_remote_model_ids(payload: &serde_json::Value) -> Vec<String> {
    let Some(items) = payload.get("data").and_then(serde_json::Value::as_array) else {
        return Vec::new();
    };
    let mut model_ids = Vec::new();
    for item in items {
        let Some(model_id) = item.get("id").and_then(serde_json::Value::as_str) else {
            continue;
        };
        if !model_ids.iter().any(|existing| existing == model_id) {
            model_ids.push(model_id.to_owned());
        }
    }
    model_ids
}

fn resolve_provider_api_key(config: &DeepSeekProviderConfig) -> Option<String> {
    if let Ok(api_key) = env::var(TERMQUILL_API_KEY_ENV) {
        let trimmed = api_key.trim();
        if !trimmed.is_empty() {
            return Some(trimmed.to_owned());
        }
    }
    if let Some(api_key) = config.api_key.as_deref().map(str::trim)
        && !api_key.is_empty()
    {
        return Some(api_key.to_owned());
    }
    None
}

fn non_empty_or(value: &str, fallback: &str) -> String {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        fallback.to_owned()
    } else {
        trimmed.to_owned()
    }
}

fn mask_secret(value: &str) -> String {
    if value.is_empty() {
        "<empty>".to_owned()
    } else {
        "*".repeat(value.chars().count().max(8))
    }
}

fn char_to_byte_index(value: &str, char_index: usize) -> usize {
    value
        .char_indices()
        .nth(char_index)
        .map(|(byte_index, _)| byte_index)
        .unwrap_or(value.len())
}

fn config_field_accepts_char(field: ConfigField, character: char) -> bool {
    match field {
        ConfigField::CompactionContextWindowTokens
        | ConfigField::CompactionTailMessages
        | ConfigField::McpStartupTimeoutSecs => character.is_ascii_digit(),
        ConfigField::CompactionSoftThresholdRatio | ConfigField::CompactionHardThresholdRatio => {
            character.is_ascii_digit() || character == '.'
        }
        ConfigField::ProviderModel
        | ConfigField::ProviderBaseUrl
        | ConfigField::ProviderFimModel
        | ConfigField::McpName
        | ConfigField::McpCommand
        | ConfigField::McpArgsCsv => !character.is_control(),
        ConfigField::ProviderApiKey
        | ConfigField::PermissionsWriteMode
        | ConfigField::MemoryEnabled
        | ConfigField::CompactionEnabled => false,
    }
}

fn text_input_target_accepts_char(target: TextInputTarget, character: char) -> bool {
    match target {
        TextInputTarget::SetupModel => !character.is_control(),
        TextInputTarget::ConfigField(field) => config_field_accepts_char(field, character),
    }
}

fn persisted_root_config(root_config: &RootConfig) -> RootConfig {
    root_config.clone()
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
        AppAction, AppState, ConfigField, ConfigSection, PaneFocus, RunPhase, SetupField,
        TimelineRole, WorkerMessage,
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
    fn demo_command_populates_timeline_and_events() -> Result<()> {
        let mut app = AppState::from_root_config(Path::new("termquill.toml"), &test_config());
        app.input = "/demo".to_owned();
        assert!(app.submit_input()?.is_none());
        assert!(app.events.iter().any(|event| event.label == "tool:start"));
        assert!(
            app.timeline
                .iter()
                .any(|entry| entry.role == TimelineRole::Tool)
        );
        Ok(())
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
        assert_eq!(app.composer_height(), 4);
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
    fn slash_selector_offers_tool_preview_candidates() {
        let mut app = AppState::from_root_config(Path::new("termquill.toml"), &test_config());
        app.input = "/tools".to_owned();

        let rows = app.slash_selector_rows();

        assert!(rows.iter().any(|(label, _)| label == "brief"));
        assert!(rows.iter().any(|(label, _)| label == "full"));
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

        for _ in 0..4 {
            let _ = app.handle_key_event(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE))?;
        }
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

        for _ in 0..3 {
            let _ = app.handle_key_event(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE))?;
        }
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
    fn tools_command_switches_between_brief_and_full_preview() -> Result<()> {
        let mut app = AppState::from_root_config(Path::new("termquill.toml"), &test_config());
        app.push_timeline(
            TimelineRole::Tool,
            r##"{
  "tool_name": "read_file",
  "status": "ok",
  "preview_kind": "markdown",
  "summary": "first 2/2 lines · 24 B",
  "preview_lines": ["# Title", "- Cargo.toml"],
  "hidden_lines": 0
}"##,
        );

        let collapsed = app.transcript_lines(20);
        assert!(collapsed.iter().any(|line| {
            line.spans
                .iter()
                .any(|span| span.content.as_ref().contains("preview hidden"))
        }));
        assert!(!collapsed.iter().any(|line| {
            line.spans
                .iter()
                .any(|span| span.content.as_ref().contains("Cargo.toml"))
        }));

        app.input = "/tools full".to_owned();
        assert!(app.submit_input()?.is_none());
        assert_eq!(app.tool_preview_mode, super::ToolPreviewMode::Full);

        let expanded = app.transcript_lines(20);
        assert!(expanded.iter().any(|line| {
            line.spans
                .iter()
                .any(|span| span.content.as_ref().contains("Cargo.toml"))
        }));

        app.input = "/tools brief".to_owned();
        assert!(app.submit_input()?.is_none());
        assert_eq!(app.tool_preview_mode, super::ToolPreviewMode::Brief);

        let recollapsed = app.transcript_lines(20);
        assert!(recollapsed.iter().any(|line| {
            line.spans
                .iter()
                .any(|span| span.content.as_ref().contains("preview hidden"))
        }));
        Ok(())
    }

    #[test]
    fn tool_command_selects_and_opens_one_card_without_expanding_all() -> Result<()> {
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

        assert_eq!(app.selected_tool_timeline_entry, Some(2));

        app.input = "/tool prev".to_owned();
        assert!(app.submit_input()?.is_none());
        assert_eq!(app.selected_tool_timeline_entry, Some(1));

        app.input = "/tool open".to_owned();
        assert!(app.submit_input()?.is_none());
        assert!(app.expanded_tool_timeline_entries.contains(&1));
        assert!(!app.expanded_tool_timeline_entries.contains(&2));

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
    fn sessions_command_lists_recent_logs() -> Result<()> {
        let temp = tempdir()?;
        let config = RootConfig {
            workspace: WorkspaceConfig {
                root: temp.path().display().to_string(),
            },
            ..test_config()
        };
        let session_dir = temp.path().join(".termquill/sessions");
        std::fs::create_dir_all(&session_dir)?;
        std::fs::write(session_dir.join("session-a.jsonl"), "")?;
        std::fs::write(session_dir.join("session-b.jsonl"), "")?;

        let mut app = AppState::from_root_config(Path::new("termquill.toml"), &config);
        app.input = "/sessions".to_owned();
        assert!(app.submit_input()?.is_none());
        assert!(
            app.timeline
                .iter()
                .any(|entry| entry.text.contains("session-a"))
        );
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
        app.input = "draft123".to_owned();
        app.input_cursor = 5;

        app.handle_key_event(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE))?;

        assert_eq!(app.input, "draft123");
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
    fn sessions_command_marks_sessions_mode_for_diagnostics() -> Result<()> {
        let temp = tempdir()?;
        let config = RootConfig {
            workspace: WorkspaceConfig {
                root: temp.path().display().to_string(),
            },
            ..test_config()
        };
        let session_dir = temp.path().join(".termquill/sessions");
        std::fs::create_dir_all(&session_dir)?;
        std::fs::write(session_dir.join("session-a.jsonl"), "")?;
        std::fs::write(session_dir.join("session-b.jsonl"), "")?;

        let mut app = AppState::from_root_config(Path::new("termquill.toml"), &config);
        app.input = "/sessions".to_owned();

        assert!(app.submit_input()?.is_none());
        assert!(matches!(
            app.activity_panel_mode,
            super::ActivityPanelMode::Sessions
        ));
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
        app.activity_panel_mode = super::ActivityPanelMode::Sessions;
        app.refresh_session_history();
        app.session_history_filter = "b".to_owned();
        let lines = app.activity_panel_lines().join("\n");
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
        app.activity_panel_mode = super::ActivityPanelMode::Sessions;

        let rows = app.activity_panel_rows();
        assert!(rows.iter().any(|row| {
            matches!(
                row,
                super::ActivityPanelRow::SessionItem {
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

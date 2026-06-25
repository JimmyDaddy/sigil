use std::collections::BTreeMap;

use anyhow::{Result, anyhow, bail};
use sigil_kernel::{
    ApprovalMode, CodeIntelStartup, CodeIntelligenceConfig, McpServerConfig,
    PluginManifestSnapshot, RootConfig, SkillDescriptor, SkillRunMode, SyntaxThemeId, ThemeId,
};
use sigil_provider_anthropic::AnthropicProviderConfig;
use sigil_provider_deepseek::{DeepSeekProviderConfig, StrictToolsMode};
use sigil_provider_gemini::GeminiProviderConfig;
use sigil_provider_openai_compat::OpenAiCompatibleProviderConfig;
use sigil_runtime::{ResolvedAgentProfile, provider_config_key};

use crate::ui::theme::{COLOR_TOKEN_GROUPS, COLOR_TOKEN_NAMES, ColorTokenGroup};

pub(crate) const DEEPSEEK_PROVIDER_KEY: &str = "deepseek";
pub(crate) const OPENAI_COMPAT_PROVIDER_KEY: &str = "openai_compat";
pub(crate) const ANTHROPIC_PROVIDER_KEY: &str = "anthropic";
pub(crate) const GEMINI_PROVIDER_KEY: &str = "gemini";

pub(crate) const CONFIG_SECTION_NAV_HINT: &str = "Tab section";
pub(crate) const CONFIG_FIELD_NAV_HINT: &str = "Up/Down field";
pub(crate) const CONFIG_EDIT_OR_TOGGLE_HINT: &str = "Enter edit/toggle";
pub(crate) const CONFIG_SAVE_HINT: &str = "Ctrl-S save";
pub(crate) const CONFIG_HEADER_NOTICE: &str =
    "Tab section · Up/Down field · Enter edit · Ctrl-S save";
pub(crate) const CONFIG_CONTROLS_HINT: &str = "controls: Tab section · Up/Down field · Enter edit";
pub(crate) const CONFIG_ACTIONS_HINT: &str = "actions: Down to actions · Ctrl-S save · Esc close";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ConfigSection {
    Provider,
    Storage,
    Permissions,
    Memory,
    Compaction,
    CodeIntelligence,
    Terminal,
    Appearance,
    Agents,
    Skills,
    Plugins,
    Mcp,
}

impl ConfigSection {
    pub(crate) const FLOW: [Self; 12] = [
        Self::Provider,
        Self::Storage,
        Self::Permissions,
        Self::Memory,
        Self::Compaction,
        Self::CodeIntelligence,
        Self::Terminal,
        Self::Appearance,
        Self::Agents,
        Self::Skills,
        Self::Plugins,
        Self::Mcp,
    ];

    pub(crate) fn title(self) -> &'static str {
        match self {
            Self::Provider => "Provider",
            Self::Storage => "Storage",
            Self::Permissions => "Permissions",
            Self::Memory => "Memory",
            Self::Compaction => "Compaction",
            Self::CodeIntelligence => "Code Intel",
            Self::Terminal => "Terminal",
            Self::Appearance => "Appearance",
            Self::Agents => "Agents",
            Self::Skills => "Skills",
            Self::Plugins => "Plugins",
            Self::Mcp => "MCP",
        }
    }

    pub(crate) fn nav_label(self) -> &'static str {
        match self {
            Self::Provider => "provider",
            Self::Storage => "storage",
            Self::Permissions => "policy",
            Self::Memory => "memory",
            Self::Compaction => "context",
            Self::CodeIntelligence => "code",
            Self::Terminal => "terminal",
            Self::Appearance => "theme",
            Self::Agents => "agents",
            Self::Skills => "skills",
            Self::Plugins => "plugins",
            Self::Mcp => "mcp",
        }
    }

    pub(crate) fn summary(self) -> &'static str {
        match self {
            Self::Provider => "provider settings",
            Self::Storage => "local state paths",
            Self::Permissions => "approval rules",
            Self::Memory => "memory status",
            Self::Compaction => "context and thresholds",
            Self::CodeIntelligence => "LSP readiness",
            Self::Terminal => "terminal integration",
            Self::Appearance => "TUI theme",
            Self::Agents => "agent profiles",
            Self::Skills => "reusable skills",
            Self::Plugins => "plugin trust review",
            Self::Mcp => "MCP servers",
        }
    }

    pub(crate) fn next_flow(self) -> Self {
        let index = Self::FLOW
            .iter()
            .position(|section| *section == self)
            .unwrap_or(0);
        Self::FLOW[(index + 1) % Self::FLOW.len()]
    }

    pub(crate) fn previous_flow(self) -> Self {
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

    pub(crate) fn flow_index(self) -> Option<usize> {
        Self::FLOW.iter().position(|section| *section == self)
    }

    pub(crate) fn from_flow_index(index: usize) -> Option<Self> {
        Self::FLOW.get(index).copied()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ConfigField {
    ProviderName,
    ProviderModel,
    ProviderApiKey,
    ProviderBaseUrl,
    ProviderFimModel,
    PermissionsDefaultMode,
    MemoryEnabled,
    CompactionEnabled,
    CompactionSoftThresholdRatio,
    CompactionHardThresholdRatio,
    CompactionContextWindowTokens,
    CompactionTailMessages,
    CodeIntelEnabled,
    CodeIntelStartup,
    CodeIntelDiscoveryEnabled,
    CodeIntelDiscoveryReportMissing,
    TerminalMouseCapture,
    TerminalOsc52Clipboard,
    TerminalScrollSensitivity,
    AppearanceTheme,
    AppearanceSyntaxTheme,
    AppearanceColorGroup,
    AppearanceColorToken,
    AppearanceColorOverride,
    SkillId,
    PluginId,
    McpName,
    McpCommand,
    McpArgsCsv,
    McpStartupTimeoutSecs,
}

impl ConfigField {
    const PROVIDER_FIELDS: [Self; 5] = [
        Self::ProviderModel,
        Self::ProviderApiKey,
        Self::ProviderBaseUrl,
        Self::ProviderFimModel,
        Self::ProviderName,
    ];
    const STORAGE_FIELDS: [Self; 0] = [];
    const PERMISSION_FIELDS: [Self; 1] = [Self::PermissionsDefaultMode];
    const MEMORY_FIELDS: [Self; 1] = [Self::MemoryEnabled];
    const COMPACTION_FIELDS: [Self; 5] = [
        Self::CompactionEnabled,
        Self::CompactionContextWindowTokens,
        Self::CompactionSoftThresholdRatio,
        Self::CompactionHardThresholdRatio,
        Self::CompactionTailMessages,
    ];
    const CODE_INTELLIGENCE_FIELDS: [Self; 4] = [
        Self::CodeIntelEnabled,
        Self::CodeIntelStartup,
        Self::CodeIntelDiscoveryEnabled,
        Self::CodeIntelDiscoveryReportMissing,
    ];
    const TERMINAL_FIELDS: [Self; 3] = [
        Self::TerminalMouseCapture,
        Self::TerminalOsc52Clipboard,
        Self::TerminalScrollSensitivity,
    ];
    const APPEARANCE_FIELDS: [Self; 5] = [
        Self::AppearanceTheme,
        Self::AppearanceSyntaxTheme,
        Self::AppearanceColorGroup,
        Self::AppearanceColorToken,
        Self::AppearanceColorOverride,
    ];
    const SKILL_FIELDS: [Self; 1] = [Self::SkillId];
    const PLUGIN_FIELDS: [Self; 1] = [Self::PluginId];
    const MCP_FIELDS: [Self; 4] = [
        Self::McpName,
        Self::McpCommand,
        Self::McpArgsCsv,
        Self::McpStartupTimeoutSecs,
    ];

    pub(crate) fn fields_for_section(section: ConfigSection) -> &'static [Self] {
        match section {
            ConfigSection::Provider => &Self::PROVIDER_FIELDS,
            ConfigSection::Storage => &Self::STORAGE_FIELDS,
            ConfigSection::Permissions => &Self::PERMISSION_FIELDS,
            ConfigSection::Memory => &Self::MEMORY_FIELDS,
            ConfigSection::Compaction => &Self::COMPACTION_FIELDS,
            ConfigSection::CodeIntelligence => &Self::CODE_INTELLIGENCE_FIELDS,
            ConfigSection::Terminal => &Self::TERMINAL_FIELDS,
            ConfigSection::Appearance => &Self::APPEARANCE_FIELDS,
            ConfigSection::Agents => &Self::SKILL_FIELDS,
            ConfigSection::Skills => &Self::SKILL_FIELDS,
            ConfigSection::Plugins => &Self::PLUGIN_FIELDS,
            ConfigSection::Mcp => &Self::MCP_FIELDS,
        }
    }

    pub(crate) fn field_for_section_index(section: ConfigSection, index: usize) -> Option<Self> {
        Self::fields_for_section(section).get(index).copied()
    }

    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::ProviderName => "provider",
            Self::ProviderModel => "model",
            Self::ProviderApiKey => "api_key",
            Self::ProviderBaseUrl => "base_url",
            Self::ProviderFimModel => "fim_model",
            Self::PermissionsDefaultMode => "default_mode",
            Self::MemoryEnabled => "enabled",
            Self::CompactionEnabled => "enabled",
            Self::CompactionSoftThresholdRatio => "soft_threshold",
            Self::CompactionHardThresholdRatio => "hard_threshold",
            Self::CompactionContextWindowTokens => "fallback_window",
            Self::CompactionTailMessages => "tail_messages",
            Self::CodeIntelEnabled => "enabled",
            Self::CodeIntelStartup => "startup",
            Self::CodeIntelDiscoveryEnabled => "discovery",
            Self::CodeIntelDiscoveryReportMissing => "report_missing",
            Self::TerminalMouseCapture => "mouse_capture",
            Self::TerminalOsc52Clipboard => "osc52_clipboard",
            Self::TerminalScrollSensitivity => "scroll_sensitivity",
            Self::AppearanceTheme => "theme",
            Self::AppearanceSyntaxTheme => "syntax_theme",
            Self::AppearanceColorGroup => "color_group",
            Self::AppearanceColorToken => "color_token",
            Self::AppearanceColorOverride => "color_override",
            Self::SkillId => "skill",
            Self::PluginId => "plugin",
            Self::McpName => "name",
            Self::McpCommand => "command",
            Self::McpArgsCsv => "args_csv",
            Self::McpStartupTimeoutSecs => "startup_timeout_secs",
        }
    }

    pub(crate) fn display_label(self) -> &'static str {
        match self {
            Self::ProviderName => "Provider",
            Self::ProviderModel => "Model",
            Self::ProviderApiKey => "API key",
            Self::ProviderBaseUrl => "Endpoint",
            Self::ProviderFimModel => "FIM model",
            Self::PermissionsDefaultMode => "Default mode",
            Self::MemoryEnabled => "Memory",
            Self::CompactionEnabled => "Auto compact",
            Self::CompactionSoftThresholdRatio => "Soft threshold",
            Self::CompactionHardThresholdRatio => "Hard threshold",
            Self::CompactionContextWindowTokens => "Fallback window",
            Self::CompactionTailMessages => "Tail messages",
            Self::CodeIntelEnabled => "Code intelligence",
            Self::CodeIntelStartup => "Startup",
            Self::CodeIntelDiscoveryEnabled => "Discovery",
            Self::CodeIntelDiscoveryReportMissing => "Missing reports",
            Self::TerminalMouseCapture => "Mouse capture",
            Self::TerminalOsc52Clipboard => "OSC52 clipboard",
            Self::TerminalScrollSensitivity => "Scroll sensitivity",
            Self::AppearanceTheme => "Theme",
            Self::AppearanceSyntaxTheme => "Syntax theme",
            Self::AppearanceColorGroup => "Color group",
            Self::AppearanceColorToken => "Color token",
            Self::AppearanceColorOverride => "Override",
            Self::SkillId => "Skill",
            Self::PluginId => "Plugin",
            Self::McpName => "Name",
            Self::McpCommand => "Command",
            Self::McpArgsCsv => "Arguments",
            Self::McpStartupTimeoutSecs => "Startup timeout",
        }
    }

    pub(crate) fn help_text(self) -> &'static str {
        match self {
            Self::ProviderName => {
                "Runtime provider used for new sessions. Switching provider starts later runs with that provider."
            }
            Self::ProviderModel => {
                "Chat model used for new runs. Switching the saved default does not rewrite the current session."
            }
            Self::ProviderApiKey => {
                "Saved locally when entered here. Provider-specific environment variables override it at runtime."
            }
            Self::ProviderBaseUrl => {
                "Provider API base URL. Leave this unchanged unless you use a proxy or compatible endpoint."
            }
            Self::ProviderFimModel => {
                "DeepSeek-only model used by prefix/FIM helpers. Chat runs use Model."
            }
            Self::PermissionsDefaultMode => {
                "Fallback approval mode for tool calls not covered by a more specific rule."
            }
            Self::MemoryEnabled => {
                "Loads workspace memory documents once at startup for stable session context."
            }
            Self::CompactionEnabled => {
                "Allows manual compaction and idle hard-threshold auto compaction."
            }
            Self::CompactionSoftThresholdRatio => {
                "Prompt pressure where the UI starts warning that compaction may be useful."
            }
            Self::CompactionHardThresholdRatio => {
                "Prompt pressure where the runner compacts after the current turn returns idle."
            }
            Self::CompactionContextWindowTokens => {
                "Used only when provider/model metadata cannot resolve the model context window."
            }
            Self::CompactionTailMessages => {
                "Recent messages retained verbatim after older history is folded into a summary."
            }
            Self::CodeIntelEnabled => {
                "Registers read-only workspace symbol, definition, reference, and diagnostics tools."
            }
            Self::CodeIntelStartup => {
                "Controls whether code intelligence is off, lazily started, or prepared eagerly."
            }
            Self::CodeIntelDiscoveryEnabled => {
                "Uses safe built-in discovery to add common language servers found on PATH."
            }
            Self::CodeIntelDiscoveryReportMissing => {
                "Shows missing discovered language servers as readiness warnings."
            }
            Self::TerminalMouseCapture => {
                "Requests terminal mouse events for clicks, scrolling, and drag selection."
            }
            Self::TerminalOsc52Clipboard => {
                "Copies selected transcript text through OSC52. Turn off when the terminal blocks clipboard writes."
            }
            Self::TerminalScrollSensitivity => {
                "Mouse wheel rows per tick for transcript and approval diff scrolling."
            }
            Self::AppearanceTheme => {
                "Color palette for the TUI. Draft themes preview immediately; saving persists the choice and does not affect session history."
            }
            Self::AppearanceSyntaxTheme => {
                "Syntax highlighting theme for markdown code blocks and tool previews. Auto follows the selected TUI theme."
            }
            Self::AppearanceColorGroup => {
                "Semantic token group used to narrow color override editing. Press Enter to move to the next group."
            }
            Self::AppearanceColorToken => {
                "Semantic color token selected for override editing. Press Enter to move within the current group."
            }
            Self::AppearanceColorOverride => {
                "Optional #RRGGBB override for the selected color token. Empty value inherits from the current theme."
            }
            Self::SkillId => {
                "Selected reusable skill. Up/Down moves through skills; footer actions load or invoke it."
            }
            Self::PluginId => {
                "Selected plugin manifest. Up/Down moves through plugins; footer actions approve or deny the manifest hash."
            }
            Self::McpName => "Stable local name for this MCP server.",
            Self::McpCommand => "Executable used to start the MCP server process.",
            Self::McpArgsCsv => "Comma-separated startup arguments for the MCP server.",
            Self::McpStartupTimeoutSecs => "Seconds to wait for initialize/tools before failing.",
        }
    }

    pub(crate) fn accepts_text_input(self) -> bool {
        matches!(
            self,
            Self::ProviderModel
                | Self::ProviderBaseUrl
                | Self::ProviderFimModel
                | Self::CompactionSoftThresholdRatio
                | Self::CompactionHardThresholdRatio
                | Self::CompactionContextWindowTokens
                | Self::CompactionTailMessages
                | Self::TerminalScrollSensitivity
                | Self::AppearanceColorOverride
                | Self::McpName
                | Self::McpCommand
                | Self::McpArgsCsv
                | Self::McpStartupTimeoutSecs
        )
    }

    pub(crate) fn action_label(self) -> &'static str {
        match self {
            Self::ProviderModel | Self::ProviderFimModel => "Enter choose",
            Self::ProviderName => "Enter cycle",
            Self::ProviderApiKey => "Enter input",
            Self::PermissionsDefaultMode
            | Self::CodeIntelStartup
            | Self::AppearanceTheme
            | Self::AppearanceSyntaxTheme
            | Self::AppearanceColorGroup
            | Self::AppearanceColorToken => "Enter cycle",
            Self::MemoryEnabled
            | Self::CompactionEnabled
            | Self::CodeIntelEnabled
            | Self::CodeIntelDiscoveryEnabled
            | Self::CodeIntelDiscoveryReportMissing
            | Self::TerminalMouseCapture
            | Self::TerminalOsc52Clipboard => "Enter toggle",
            Self::TerminalScrollSensitivity => "Enter input",
            Self::AppearanceColorOverride => "Enter input",
            Self::SkillId | Self::PluginId => "",
            _ if self.accepts_text_input() => "Enter input",
            _ => "",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ConfigFooterAction {
    Save,
    SaveAndClose,
    ActivateMcp,
    TrustAgent,
    BlockAgent,
    ToggleAgentEnabled,
    ToggleAgentUser,
    ToggleAgentModel,
    LoadSkill,
    InvokeSkill,
    ApprovePlugin,
    DenyPlugin,
    Close,
}

impl ConfigFooterAction {
    const DEFAULT_ORDER: [Self; 3] = [Self::Save, Self::SaveAndClose, Self::Close];
    const MCP_ORDER: [Self; 4] = [
        Self::Save,
        Self::SaveAndClose,
        Self::ActivateMcp,
        Self::Close,
    ];
    const AGENTS_ORDER: [Self; 6] = [
        Self::TrustAgent,
        Self::BlockAgent,
        Self::ToggleAgentEnabled,
        Self::ToggleAgentUser,
        Self::ToggleAgentModel,
        Self::Close,
    ];
    const SKILLS_ORDER: [Self; 3] = [Self::LoadSkill, Self::InvokeSkill, Self::Close];
    const PLUGINS_ORDER: [Self; 3] = [Self::ApprovePlugin, Self::DenyPlugin, Self::Close];

    pub(crate) fn actions_for_section(section: ConfigSection) -> &'static [Self] {
        match section {
            ConfigSection::Mcp => &Self::MCP_ORDER,
            ConfigSection::Agents => &Self::AGENTS_ORDER,
            ConfigSection::Skills => &Self::SKILLS_ORDER,
            ConfigSection::Plugins => &Self::PLUGINS_ORDER,
            ConfigSection::Provider
            | ConfigSection::Storage
            | ConfigSection::Permissions
            | ConfigSection::Memory
            | ConfigSection::Compaction
            | ConfigSection::CodeIntelligence
            | ConfigSection::Terminal
            | ConfigSection::Appearance => &Self::DEFAULT_ORDER,
        }
    }

    pub(crate) fn action_for_section_index(section: ConfigSection, index: usize) -> Option<Self> {
        Self::actions_for_section(section).get(index).copied()
    }

    pub(crate) fn button_label(self) -> &'static str {
        match self {
            Self::Save => "save",
            Self::SaveAndClose => "save+close",
            Self::ActivateMcp => "activate",
            Self::TrustAgent => "trust",
            Self::BlockAgent => "block",
            Self::ToggleAgentEnabled => "enable",
            Self::ToggleAgentUser => "user",
            Self::ToggleAgentModel => "model",
            Self::LoadSkill => "load",
            Self::InvokeSkill => "invoke",
            Self::ApprovePlugin => "approve",
            Self::DenyPlugin => "deny",
            Self::Close => "close",
        }
    }

    pub(crate) fn field_label(self) -> &'static str {
        match self {
            Self::Save => "save",
            Self::SaveAndClose => "save_and_close",
            Self::ActivateMcp => "activate_mcp",
            Self::TrustAgent => "trust_agent",
            Self::BlockAgent => "block_agent",
            Self::ToggleAgentEnabled => "toggle_agent_enabled",
            Self::ToggleAgentUser => "toggle_agent_user",
            Self::ToggleAgentModel => "toggle_agent_model",
            Self::LoadSkill => "load_skill",
            Self::InvokeSkill => "invoke_skill",
            Self::ApprovePlugin => "approve_plugin",
            Self::DenyPlugin => "deny_plugin",
            Self::Close => "close",
        }
    }

    pub(crate) fn next_for_section(self, section: ConfigSection) -> Self {
        let actions = Self::actions_for_section(section);
        let index = actions
            .iter()
            .position(|action| *action == self)
            .unwrap_or(0);
        actions[(index + 1) % actions.len()]
    }

    pub(crate) fn previous_for_section(self, section: ConfigSection) -> Self {
        let actions = Self::actions_for_section(section);
        let index = actions
            .iter()
            .position(|action| *action == self)
            .unwrap_or(0);
        if index == 0 {
            *actions.last().expect("footer actions are non-empty")
        } else {
            actions[index - 1]
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ConfigFieldMove {
    Moved,
    Boundary,
    Unavailable,
}

#[derive(Debug, Clone)]
pub(crate) struct McpServerDraft {
    pub(crate) name: String,
    pub(crate) command: String,
    pub(crate) args_csv: String,
    pub(crate) startup_timeout_secs: String,
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
            ..McpServerConfig::default()
        })
    }
}

#[derive(Debug, Clone)]
pub(crate) struct ConfigDraft {
    pub(crate) base_root_config: RootConfig,
    pub(crate) provider_name: String,
    pub(crate) provider_model: String,
    pub(crate) provider_api_key: String,
    pub(crate) provider_base_url: String,
    provider_drafts: BTreeMap<String, ProviderFieldDraft>,
    pub(crate) provider_beta_base_url: String,
    pub(crate) provider_anthropic_base_url: String,
    pub(crate) provider_user_id_strategy: String,
    pub(crate) provider_strict_tools_mode: StrictToolsMode,
    pub(crate) provider_fim_model: String,
    pub(crate) provider_request_timeout_secs: String,
    pub(crate) permission_default_mode: ApprovalMode,
    pub(crate) memory_enabled: bool,
    pub(crate) compaction_enabled: bool,
    pub(crate) compaction_soft_threshold_ratio: String,
    pub(crate) compaction_hard_threshold_ratio: String,
    pub(crate) compaction_context_window_tokens: String,
    pub(crate) compaction_tail_messages: String,
    pub(crate) code_intelligence_enabled: bool,
    pub(crate) code_intelligence_startup: CodeIntelStartup,
    pub(crate) code_intelligence_discovery_enabled: bool,
    pub(crate) code_intelligence_discovery_report_missing: bool,
    pub(crate) terminal_mouse_capture: bool,
    pub(crate) terminal_osc52_clipboard: bool,
    pub(crate) terminal_scroll_sensitivity: String,
    pub(crate) appearance_theme: ThemeId,
    pub(crate) appearance_syntax_theme: SyntaxThemeId,
    pub(crate) appearance_color_group_index: usize,
    pub(crate) appearance_color_token_index: usize,
    pub(crate) mcp_servers: Vec<McpServerDraft>,
}

#[derive(Debug, Clone)]
struct ProviderFieldDraft {
    model: String,
    api_key: String,
    base_url: String,
    request_timeout_secs: String,
}

impl ProviderFieldDraft {
    fn from_deepseek(config: &DeepSeekProviderConfig) -> Self {
        Self {
            model: config.model.clone(),
            api_key: config.api_key.clone().unwrap_or_default(),
            base_url: config.base_url.clone(),
            request_timeout_secs: config.request_timeout_secs.to_string(),
        }
    }

    fn from_openai_compat(config: &OpenAiCompatibleProviderConfig) -> Self {
        Self {
            model: config.model.clone(),
            api_key: config.api_key.clone().unwrap_or_default(),
            base_url: config.base_url.clone(),
            request_timeout_secs: config.request_timeout_secs.to_string(),
        }
    }

    fn from_anthropic(config: &AnthropicProviderConfig) -> Self {
        Self {
            model: config.model.clone(),
            api_key: config.api_key.clone().unwrap_or_default(),
            base_url: config.base_url.clone(),
            request_timeout_secs: config.request_timeout_secs.to_string(),
        }
    }

    fn from_gemini(config: &GeminiProviderConfig) -> Self {
        Self {
            model: config.model.clone(),
            api_key: config.api_key.clone().unwrap_or_default(),
            base_url: config.base_url.clone(),
            request_timeout_secs: config.request_timeout_secs.to_string(),
        }
    }
}

impl ConfigDraft {
    pub(crate) fn from_root_config(root_config: &RootConfig) -> Self {
        let provider_name = normalize_provider_name(&root_config.agent.provider).to_owned();
        let deepseek_provider = load_deepseek_provider_config(root_config)
            .unwrap_or_else(|| default_deepseek_provider_config(&root_config.agent.model));
        let openai_provider = load_openai_compat_provider_config(root_config)
            .unwrap_or_else(|| default_openai_compat_provider_config(&root_config.agent.model));
        let anthropic_provider = load_anthropic_provider_config(root_config)
            .unwrap_or_else(|| default_anthropic_provider_config(&root_config.agent.model));
        let gemini_provider = load_gemini_provider_config(root_config)
            .unwrap_or_else(|| default_gemini_provider_config(&root_config.agent.model));
        let mut provider_drafts = BTreeMap::new();
        provider_drafts.insert(
            DEEPSEEK_PROVIDER_KEY.to_owned(),
            ProviderFieldDraft::from_deepseek(&deepseek_provider),
        );
        provider_drafts.insert(
            OPENAI_COMPAT_PROVIDER_KEY.to_owned(),
            ProviderFieldDraft::from_openai_compat(&openai_provider),
        );
        provider_drafts.insert(
            ANTHROPIC_PROVIDER_KEY.to_owned(),
            ProviderFieldDraft::from_anthropic(&anthropic_provider),
        );
        provider_drafts.insert(
            GEMINI_PROVIDER_KEY.to_owned(),
            ProviderFieldDraft::from_gemini(&gemini_provider),
        );
        let current_provider_draft = provider_drafts
            .get(provider_name.as_str())
            .cloned()
            .expect("normalized provider has an initialized draft");
        Self {
            base_root_config: root_config.clone(),
            provider_name: provider_name.clone(),
            provider_model: current_provider_draft.model,
            provider_api_key: current_provider_draft.api_key,
            provider_base_url: current_provider_draft.base_url,
            provider_drafts,
            provider_beta_base_url: deepseek_provider.beta_base_url,
            provider_anthropic_base_url: deepseek_provider.anthropic_base_url,
            provider_user_id_strategy: deepseek_provider.user_id_strategy.unwrap_or_default(),
            provider_strict_tools_mode: deepseek_provider.strict_tools_mode,
            provider_fim_model: deepseek_provider.fim_model,
            provider_request_timeout_secs: current_provider_draft.request_timeout_secs,
            permission_default_mode: root_config.permission.default_mode,
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
            code_intelligence_enabled: root_config.code_intelligence.enabled,
            code_intelligence_startup: root_config.code_intelligence.startup,
            code_intelligence_discovery_enabled: root_config.code_intelligence.discovery.enabled,
            code_intelligence_discovery_report_missing: root_config
                .code_intelligence
                .discovery
                .report_missing,
            terminal_mouse_capture: root_config.terminal.mouse_capture,
            terminal_osc52_clipboard: root_config.terminal.osc52_clipboard,
            terminal_scroll_sensitivity: root_config.terminal.scroll_sensitivity.to_string(),
            appearance_theme: root_config.appearance.theme,
            appearance_syntax_theme: root_config.appearance.syntax_theme,
            appearance_color_group_index: first_appearance_color_group_index(root_config),
            appearance_color_token_index: first_appearance_color_token_index(root_config),
            mcp_servers: root_config
                .mcp_servers
                .iter()
                .map(McpServerDraft::from_config)
                .collect(),
        }
    }

    pub(crate) fn cycle_provider(&mut self) {
        self.capture_current_provider_draft();
        let provider_name = cycle_provider_name(&self.provider_name);
        self.provider_name = provider_name.clone();
        self.load_provider_draft(&provider_name);
    }

    fn capture_current_provider_draft(&mut self) {
        let provider_name = normalize_provider_name(&self.provider_name).to_owned();
        self.provider_drafts.insert(
            provider_name,
            ProviderFieldDraft {
                model: self.provider_model.clone(),
                api_key: self.provider_api_key.clone(),
                base_url: self.provider_base_url.clone(),
                request_timeout_secs: self.provider_request_timeout_secs.clone(),
            },
        );
    }

    fn load_provider_draft(&mut self, provider_name: &str) {
        let provider_name = normalize_provider_name(provider_name);
        let draft = self
            .provider_drafts
            .get(provider_name)
            .cloned()
            .unwrap_or_else(|| {
                default_provider_field_draft(provider_name, &self.base_root_config.agent.model)
            });
        self.provider_model = draft.model;
        self.provider_api_key = draft.api_key;
        self.provider_base_url = draft.base_url;
        self.provider_request_timeout_secs = draft.request_timeout_secs;
    }

    pub(crate) fn to_root_config(&self) -> Result<RootConfig> {
        let provider_name = normalize_provider_name(&self.provider_name);
        let model = self.provider_model.trim();
        if model.is_empty() {
            bail!("model cannot be empty");
        }
        let api_key = self.provider_api_key.trim();
        let base_url = self.provider_base_url.trim();
        if base_url.is_empty() {
            bail!("base_url cannot be empty");
        }
        if provider_name == DEEPSEEK_PROVIDER_KEY {
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
                    anyhow!("fallback_context_window_tokens must be a positive integer: {error}")
                })?;
            if parsed == 0 {
                bail!("fallback_context_window_tokens must be greater than 0");
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
        let terminal_scroll_sensitivity = self
            .terminal_scroll_sensitivity
            .trim()
            .parse::<u16>()
            .map_err(|error| anyhow!("scroll_sensitivity must be a positive integer: {error}"))?;
        if terminal_scroll_sensitivity == 0 {
            bail!("scroll_sensitivity must be greater than 0");
        }

        let mut root_config = self.base_root_config.clone();
        root_config.agent.provider = provider_name.to_owned();
        root_config.agent.model = model.to_owned();
        root_config.permission.default_mode = self.permission_default_mode;
        root_config.memory.enabled = self.memory_enabled;
        root_config.compaction.enabled = self.compaction_enabled;
        root_config.compaction.soft_threshold_ratio = soft_threshold_ratio;
        root_config.compaction.hard_threshold_ratio = hard_threshold_ratio;
        root_config.compaction.context_window_tokens = context_window_tokens;
        root_config.compaction.tail_messages = tail_messages;
        root_config.code_intelligence = self.code_intelligence_config();
        root_config.terminal.mouse_capture = self.terminal_mouse_capture;
        root_config.terminal.osc52_clipboard = self.terminal_osc52_clipboard;
        root_config.terminal.scroll_sensitivity = terminal_scroll_sensitivity;
        root_config.appearance.theme = self.appearance_theme;
        root_config.appearance.syntax_theme = self.appearance_syntax_theme;
        root_config.appearance.colors = self.base_root_config.appearance.colors.clone();
        root_config.mcp_servers = self
            .mcp_servers
            .iter()
            .enumerate()
            .map(|(index, server)| server.to_config(index))
            .collect::<Result<Vec<_>>>()?;

        match provider_name {
            OPENAI_COMPAT_PROVIDER_KEY => {
                let mut provider_config = load_openai_compat_provider_config(&root_config)
                    .unwrap_or_else(|| default_openai_compat_provider_config(model));
                provider_config.model = model.to_owned();
                provider_config.api_key = (!api_key.is_empty()).then(|| api_key.to_owned());
                provider_config.base_url = base_url.to_owned();
                provider_config.request_timeout_secs = request_timeout_secs;
                let provider_value = serialize_openai_compat_provider_value(&provider_config)?;
                root_config
                    .providers
                    .insert(OPENAI_COMPAT_PROVIDER_KEY.to_owned(), provider_value);
            }
            ANTHROPIC_PROVIDER_KEY => {
                let mut provider_config = load_anthropic_provider_config(&root_config)
                    .unwrap_or_else(|| default_anthropic_provider_config(model));
                provider_config.model = model.to_owned();
                provider_config.api_key = (!api_key.is_empty()).then(|| api_key.to_owned());
                provider_config.base_url = base_url.to_owned();
                provider_config.request_timeout_secs = request_timeout_secs;
                let provider_value = serialize_anthropic_provider_value(&provider_config)?;
                root_config
                    .providers
                    .insert(ANTHROPIC_PROVIDER_KEY.to_owned(), provider_value);
            }
            GEMINI_PROVIDER_KEY => {
                let mut provider_config = load_gemini_provider_config(&root_config)
                    .unwrap_or_else(|| default_gemini_provider_config(model));
                provider_config.model = model.to_owned();
                provider_config.api_key = (!api_key.is_empty()).then(|| api_key.to_owned());
                provider_config.base_url = base_url.to_owned();
                provider_config.request_timeout_secs = request_timeout_secs;
                let provider_value = serialize_gemini_provider_value(&provider_config)?;
                root_config
                    .providers
                    .insert(GEMINI_PROVIDER_KEY.to_owned(), provider_value);
            }
            _ => {
                let beta_base_url = self.provider_beta_base_url.trim();
                let anthropic_base_url = self.provider_anthropic_base_url.trim();
                let fim_model = self.provider_fim_model.trim();
                let mut provider_config = load_deepseek_provider_config(&root_config)
                    .unwrap_or_else(|| default_deepseek_provider_config(model));
                provider_config.model = model.to_owned();
                provider_config.api_key = (!api_key.is_empty()).then(|| api_key.to_owned());
                provider_config.base_url = base_url.to_owned();
                provider_config.beta_base_url = beta_base_url.to_owned();
                provider_config.anthropic_base_url = anthropic_base_url.to_owned();
                provider_config.user_id_strategy =
                    (!self.provider_user_id_strategy.trim().is_empty())
                        .then(|| self.provider_user_id_strategy.trim().to_owned());
                provider_config.strict_tools_mode = self.provider_strict_tools_mode;
                provider_config.fim_model = fim_model.to_owned();
                provider_config.request_timeout_secs = request_timeout_secs;

                let provider_value = serialize_deepseek_provider_value(&provider_config)?;
                root_config
                    .providers
                    .insert(DEEPSEEK_PROVIDER_KEY.to_owned(), provider_value);
            }
        }
        Ok(root_config)
    }

    pub(crate) fn code_intelligence_config(&self) -> CodeIntelligenceConfig {
        let mut config = self.base_root_config.code_intelligence.clone();
        config.enabled = self.code_intelligence_enabled;
        config.startup = self.code_intelligence_startup;
        config.discovery.enabled = self.code_intelligence_discovery_enabled;
        config.discovery.report_missing = self.code_intelligence_discovery_report_missing;
        config
    }

    pub(crate) fn code_intelligence_preview_root_config(&self) -> RootConfig {
        let mut root_config = self.base_root_config.clone();
        root_config.code_intelligence = self.code_intelligence_config();
        root_config
    }

    pub(crate) fn selected_appearance_color_token(&self) -> &'static str {
        COLOR_TOKEN_NAMES[self
            .appearance_color_token_index
            .min(COLOR_TOKEN_NAMES.len() - 1)]
    }

    pub(crate) fn selected_appearance_color_group(&self) -> ColorTokenGroup {
        COLOR_TOKEN_GROUPS[self
            .appearance_color_group_index
            .min(COLOR_TOKEN_GROUPS.len() - 1)]
    }

    pub(crate) fn cycle_appearance_syntax_theme(&mut self) {
        self.appearance_syntax_theme = self.appearance_syntax_theme.next();
    }

    pub(crate) fn resolved_appearance_syntax_theme(&self) -> SyntaxThemeId {
        self.appearance_syntax_theme
            .resolved_for_theme(self.appearance_theme)
    }

    pub(crate) fn cycle_appearance_color_group(&mut self, forward: bool) {
        let len = COLOR_TOKEN_GROUPS.len();
        if forward {
            self.appearance_color_group_index = (self.appearance_color_group_index + 1) % len;
        } else if self.appearance_color_group_index == 0 {
            self.appearance_color_group_index = len - 1;
        } else {
            self.appearance_color_group_index -= 1;
        }
        self.appearance_color_token_index = COLOR_TOKEN_NAMES
            .iter()
            .position(|token| {
                self.selected_appearance_color_group()
                    .tokens
                    .contains(token)
            })
            .unwrap_or(0);
    }

    pub(crate) fn cycle_appearance_color_token(&mut self, forward: bool) {
        let group = self.selected_appearance_color_group();
        let tokens = group.tokens;
        let current_group_index = tokens
            .iter()
            .position(|token| *token == self.selected_appearance_color_token())
            .unwrap_or(0);
        let next_group_index = if forward {
            (current_group_index + 1) % tokens.len()
        } else if current_group_index == 0 {
            tokens.len() - 1
        } else {
            current_group_index - 1
        };
        let next_token = tokens[next_group_index];
        self.appearance_color_token_index = COLOR_TOKEN_NAMES
            .iter()
            .position(|token| *token == next_token)
            .unwrap_or(0);
    }

    #[cfg(test)]
    pub(crate) fn cycle_all_appearance_color_tokens(&mut self, forward: bool) {
        let len = COLOR_TOKEN_NAMES.len();
        if forward {
            self.appearance_color_token_index = (self.appearance_color_token_index + 1) % len;
        } else if self.appearance_color_token_index == 0 {
            self.appearance_color_token_index = len - 1;
        } else {
            self.appearance_color_token_index -= 1;
        }
        self.appearance_color_group_index =
            appearance_color_group_index_for_token(self.selected_appearance_color_token())
                .unwrap_or(0);
    }

    pub(crate) fn selected_appearance_color_override(&self) -> Option<&str> {
        self.base_root_config
            .appearance
            .colors
            .get(self.selected_appearance_color_token())
    }

    pub(crate) fn set_selected_appearance_color_override(&mut self, value: String) -> Result<bool> {
        let token = self.selected_appearance_color_token();
        let value = value.trim();
        if value.is_empty() {
            return Ok(self.reset_selected_appearance_color_override());
        }
        let normalized = normalize_hex_color_override(value)?;
        let changed =
            self.base_root_config.appearance.colors.get(token) != Some(normalized.as_str());
        if changed {
            self.base_root_config
                .appearance
                .colors
                .insert(token.to_owned(), normalized);
        }
        Ok(changed)
    }

    pub(crate) fn reset_selected_appearance_color_override(&mut self) -> bool {
        let token = self.selected_appearance_color_token();
        self.base_root_config
            .appearance
            .colors
            .remove(token)
            .is_some()
    }

    pub(crate) fn reset_all_appearance_color_overrides(&mut self) -> bool {
        if self.base_root_config.appearance.colors.is_empty() {
            return false;
        }
        self.base_root_config.appearance.colors.clear();
        true
    }

    pub(crate) fn reset_selected_appearance_color_group_overrides(&mut self) -> usize {
        let tokens = self.selected_appearance_color_group().tokens;
        let mut removed = 0usize;
        for token in tokens {
            if self
                .base_root_config
                .appearance
                .colors
                .remove(token)
                .is_some()
            {
                removed += 1;
            }
        }
        removed
    }

    pub(crate) fn selected_appearance_color_group_override_count(&self) -> usize {
        self.selected_appearance_color_group()
            .tokens
            .iter()
            .filter(|token| self.base_root_config.appearance.colors.get(token).is_some())
            .count()
    }
}

fn first_appearance_color_token_index(root_config: &RootConfig) -> usize {
    COLOR_TOKEN_NAMES
        .iter()
        .position(|token| root_config.appearance.colors.get(token).is_some())
        .unwrap_or(0)
}

fn first_appearance_color_group_index(root_config: &RootConfig) -> usize {
    COLOR_TOKEN_NAMES
        .iter()
        .find(|token| root_config.appearance.colors.get(token).is_some())
        .and_then(|token| appearance_color_group_index_for_token(token))
        .unwrap_or(0)
}

fn appearance_color_group_index_for_token(token: &str) -> Option<usize> {
    COLOR_TOKEN_GROUPS
        .iter()
        .position(|group| group.tokens.contains(&token))
}

fn normalize_hex_color_override(value: &str) -> Result<String> {
    let value = value.trim();
    if value.len() != 7
        || !value.starts_with('#')
        || !value[1..]
            .chars()
            .all(|character| character.is_ascii_hexdigit())
    {
        bail!("color override must be #RRGGBB");
    }
    Ok(format!("#{}", value[1..].to_ascii_uppercase()))
}

#[derive(Debug, Clone)]
pub(crate) struct ConfigState {
    pub(crate) selected_section: ConfigSection,
    pub(crate) selected_field: Option<ConfigField>,
    pub(crate) footer_selected: bool,
    pub(crate) selected_footer_action: ConfigFooterAction,
    pub(crate) selected_mcp_server_index: usize,
    pub(crate) selected_agent_index: usize,
    pub(crate) selected_skill_index: usize,
    pub(crate) selected_plugin_index: usize,
    pub(crate) agent_profiles: Vec<ResolvedAgentProfile>,
    pub(crate) agent_warnings: Vec<String>,
    pub(crate) skill_descriptors: Vec<SkillDescriptor>,
    pub(crate) skill_warnings: Vec<String>,
    pub(crate) plugin_manifests: Vec<PluginManifestSnapshot>,
    pub(crate) plugin_warnings: Vec<String>,
    pub(crate) draft: ConfigDraft,
    pub(crate) dirty: bool,
    pub(crate) close_guard_armed: bool,
}

impl ConfigState {
    pub(crate) fn from_root_config(root_config: &RootConfig) -> Self {
        let selected_section = ConfigSection::Provider;
        Self {
            selected_section,
            selected_field: ConfigField::fields_for_section(selected_section)
                .first()
                .copied(),
            footer_selected: false,
            selected_footer_action: ConfigFooterAction::Save,
            selected_mcp_server_index: 0,
            selected_agent_index: 0,
            selected_skill_index: 0,
            selected_plugin_index: 0,
            agent_profiles: Vec::new(),
            agent_warnings: Vec::new(),
            skill_descriptors: Vec::new(),
            skill_warnings: Vec::new(),
            plugin_manifests: Vec::new(),
            plugin_warnings: Vec::new(),
            draft: ConfigDraft::from_root_config(root_config),
            dirty: false,
            close_guard_armed: false,
        }
    }

    pub(crate) fn set_section(&mut self, section: ConfigSection) {
        self.selected_section = section;
        self.sync_mcp_selection();
        self.sync_agent_selection();
        self.sync_skill_selection();
        self.sync_plugin_selection();
        self.footer_selected = false;
        self.selected_field = self.first_field_for_section(section);
    }

    fn first_field_for_section(&self, section: ConfigSection) -> Option<ConfigField> {
        if self.section_collection_is_empty(section) {
            None
        } else {
            ConfigField::fields_for_section(section).first().copied()
        }
    }

    fn last_field_for_current_section(&self) -> Option<ConfigField> {
        if self.section_collection_is_empty(self.selected_section) {
            None
        } else {
            ConfigField::fields_for_section(self.selected_section)
                .last()
                .copied()
        }
    }

    fn section_collection_is_empty(&self, section: ConfigSection) -> bool {
        match section {
            ConfigSection::Mcp => self.draft.mcp_servers.is_empty(),
            ConfigSection::Agents => self.agent_profiles.is_empty(),
            ConfigSection::Skills => {
                skill_display_order_for_section(&self.skill_descriptors, ConfigSection::Skills)
                    .is_empty()
            }
            ConfigSection::Plugins => self.plugin_manifests.is_empty(),
            _ => false,
        }
    }

    pub(crate) fn move_field(&mut self, forward: bool) -> ConfigFieldMove {
        if self.section_collection_is_empty(self.selected_section) {
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

    pub(crate) fn focus_footer(&mut self, action: ConfigFooterAction) {
        self.footer_selected = true;
        self.selected_footer_action = action;
    }

    pub(crate) fn focus_field(&mut self, field: ConfigField) -> bool {
        if self.section_collection_is_empty(self.selected_section) {
            return false;
        }
        if !ConfigField::fields_for_section(self.selected_section).contains(&field) {
            return false;
        }
        self.selected_field = Some(field);
        self.footer_selected = false;
        true
    }

    pub(crate) fn focus_last_field(&mut self) -> bool {
        let Some(field) = self.last_field_for_current_section() else {
            return false;
        };
        self.selected_field = Some(field);
        self.footer_selected = false;
        true
    }

    pub(crate) fn move_footer_action(&mut self, forward: bool) {
        self.footer_selected = true;
        self.selected_footer_action = if forward {
            self.selected_footer_action
                .next_for_section(self.selected_section)
        } else {
            self.selected_footer_action
                .previous_for_section(self.selected_section)
        };
    }

    pub(crate) fn sync_mcp_selection(&mut self) {
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

    pub(crate) fn set_agent_discovery(
        &mut self,
        profiles: Vec<ResolvedAgentProfile>,
        warnings: Vec<String>,
    ) {
        self.agent_profiles = profiles;
        self.agent_warnings = warnings;
        self.sync_agent_selection();
        if self.selected_section == ConfigSection::Agents {
            self.selected_field = self.first_field_for_section(ConfigSection::Agents);
        }
    }

    pub(crate) fn sync_agent_selection(&mut self) {
        if self.agent_profiles.is_empty() {
            self.selected_agent_index = 0;
            if self.selected_section == ConfigSection::Agents {
                self.selected_field = None;
            }
            return;
        }
        self.selected_agent_index = self
            .selected_agent_index
            .min(self.agent_profiles.len().saturating_sub(1));
    }

    pub(crate) fn set_skill_discovery(
        &mut self,
        descriptors: Vec<SkillDescriptor>,
        warnings: Vec<String>,
    ) {
        self.skill_descriptors = descriptors;
        self.skill_warnings = warnings;
        if let Some(first_index) =
            skill_display_order_for_section(&self.skill_descriptors, self.selected_section).first()
        {
            self.selected_skill_index = *first_index;
        } else if let Some(first_index) = skill_display_order(&self.skill_descriptors).first() {
            self.selected_skill_index = *first_index;
        }
        self.sync_skill_selection();
        if self.selected_section == ConfigSection::Skills {
            self.selected_field = self.first_field_for_section(self.selected_section);
        }
    }

    pub(crate) fn sync_skill_selection(&mut self) {
        if self.skill_descriptors.is_empty() {
            self.selected_skill_index = 0;
            if self.selected_section == ConfigSection::Skills {
                self.selected_field = None;
            }
            return;
        }
        let section_order =
            skill_display_order_for_section(&self.skill_descriptors, self.selected_section);
        if !section_order.is_empty() && !section_order.contains(&self.selected_skill_index) {
            self.selected_skill_index = section_order[0];
            return;
        }
        self.selected_skill_index = self
            .selected_skill_index
            .min(self.skill_descriptors.len().saturating_sub(1));
    }

    pub(crate) fn set_plugin_discovery(
        &mut self,
        manifests: Vec<PluginManifestSnapshot>,
        warnings: Vec<String>,
    ) {
        self.plugin_manifests = manifests;
        self.plugin_warnings = warnings;
        self.sync_plugin_selection();
        if self.selected_section == ConfigSection::Plugins {
            self.selected_field = self.first_field_for_section(ConfigSection::Plugins);
        }
    }

    pub(crate) fn sync_plugin_selection(&mut self) {
        if self.plugin_manifests.is_empty() {
            self.selected_plugin_index = 0;
            if self.selected_section == ConfigSection::Plugins {
                self.selected_field = None;
            }
            return;
        }
        self.selected_plugin_index = self
            .selected_plugin_index
            .min(self.plugin_manifests.len().saturating_sub(1));
    }

    pub(crate) fn selected_skill(&self) -> Option<&SkillDescriptor> {
        let skill = self.skill_descriptors.get(self.selected_skill_index)?;
        match self.selected_section {
            ConfigSection::Agents if !skill_is_agent(skill) => None,
            ConfigSection::Skills if skill_is_agent(skill) => None,
            _ => Some(skill),
        }
    }

    pub(crate) fn selected_agent(&self) -> Option<&ResolvedAgentProfile> {
        self.agent_profiles.get(self.selected_agent_index)
    }

    pub(crate) fn cycle_agent(&mut self, forward: bool) -> bool {
        if self.agent_profiles.is_empty() {
            return false;
        }
        let len = self.agent_profiles.len();
        if forward {
            self.selected_agent_index = (self.selected_agent_index + 1) % len;
        } else if self.selected_agent_index == 0 {
            self.selected_agent_index = len - 1;
        } else {
            self.selected_agent_index -= 1;
        }
        true
    }

    pub(crate) fn move_agent(&mut self, forward: bool) -> ConfigFieldMove {
        if self.agent_profiles.is_empty() {
            return ConfigFieldMove::Unavailable;
        }
        if forward {
            if self.selected_agent_index + 1 >= self.agent_profiles.len() {
                return ConfigFieldMove::Boundary;
            }
            self.selected_agent_index += 1;
        } else {
            if self.selected_agent_index == 0 {
                return ConfigFieldMove::Boundary;
            }
            self.selected_agent_index -= 1;
        }
        self.selected_field = Some(ConfigField::SkillId);
        self.footer_selected = false;
        ConfigFieldMove::Moved
    }

    pub(crate) fn cycle_skill(&mut self, forward: bool) -> bool {
        let order = skill_display_order_for_section(&self.skill_descriptors, self.selected_section);
        if order.is_empty() {
            return false;
        }
        let current_position = order
            .iter()
            .position(|index| *index == self.selected_skill_index)
            .unwrap_or(0);
        let next_position = if forward {
            (current_position + 1) % order.len()
        } else if current_position == 0 {
            order.len() - 1
        } else {
            current_position - 1
        };
        self.selected_skill_index = order[next_position];
        true
    }

    pub(crate) fn move_skill(&mut self, forward: bool) -> ConfigFieldMove {
        let order = skill_display_order_for_section(&self.skill_descriptors, self.selected_section);
        if order.is_empty() {
            return ConfigFieldMove::Unavailable;
        }
        let current_position = order
            .iter()
            .position(|index| *index == self.selected_skill_index)
            .unwrap_or(0);
        if forward {
            if current_position + 1 >= order.len() {
                return ConfigFieldMove::Boundary;
            }
            self.selected_skill_index = order[current_position + 1];
        } else {
            if current_position == 0 {
                return ConfigFieldMove::Boundary;
            }
            self.selected_skill_index = order[current_position - 1];
        }
        self.selected_field = Some(ConfigField::SkillId);
        self.footer_selected = false;
        ConfigFieldMove::Moved
    }

    pub(crate) fn selected_plugin(&self) -> Option<&PluginManifestSnapshot> {
        self.plugin_manifests.get(self.selected_plugin_index)
    }

    pub(crate) fn selected_plugin_mut(&mut self) -> Option<&mut PluginManifestSnapshot> {
        self.plugin_manifests.get_mut(self.selected_plugin_index)
    }

    pub(crate) fn cycle_plugin(&mut self, forward: bool) -> bool {
        if self.plugin_manifests.is_empty() {
            return false;
        }
        let len = self.plugin_manifests.len();
        if forward {
            self.selected_plugin_index = (self.selected_plugin_index + 1) % len;
        } else if self.selected_plugin_index == 0 {
            self.selected_plugin_index = len - 1;
        } else {
            self.selected_plugin_index -= 1;
        }
        true
    }

    pub(crate) fn move_plugin(&mut self, forward: bool) -> ConfigFieldMove {
        if self.plugin_manifests.is_empty() {
            return ConfigFieldMove::Unavailable;
        }
        if forward {
            if self.selected_plugin_index + 1 >= self.plugin_manifests.len() {
                return ConfigFieldMove::Boundary;
            }
            self.selected_plugin_index += 1;
        } else {
            if self.selected_plugin_index == 0 {
                return ConfigFieldMove::Boundary;
            }
            self.selected_plugin_index -= 1;
        }
        self.selected_field = Some(ConfigField::PluginId);
        self.footer_selected = false;
        ConfigFieldMove::Moved
    }

    pub(crate) fn selected_mcp_server(&self) -> Option<&McpServerDraft> {
        self.draft.mcp_servers.get(self.selected_mcp_server_index)
    }

    pub(crate) fn selected_mcp_server_mut(&mut self) -> Option<&mut McpServerDraft> {
        self.draft
            .mcp_servers
            .get_mut(self.selected_mcp_server_index)
    }

    pub(crate) fn editing_field(&self) -> Option<ConfigField> {
        None
    }

    pub(crate) fn add_mcp_server(&mut self) {
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

    pub(crate) fn remove_selected_mcp_server(&mut self) -> bool {
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

    pub(crate) fn cycle_mcp_server(&mut self, forward: bool) -> bool {
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

    pub(crate) fn field_text_value(&self, field: ConfigField) -> Option<&str> {
        match field {
            ConfigField::ProviderName => Some(&self.draft.provider_name),
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
            ConfigField::TerminalScrollSensitivity => Some(&self.draft.terminal_scroll_sensitivity),
            ConfigField::SkillId if self.selected_section == ConfigSection::Agents => {
                self.selected_agent().map(|agent| agent.profile.id.as_str())
            }
            ConfigField::SkillId => self.selected_skill().map(|skill| skill.id.as_str()),
            ConfigField::PluginId => self
                .selected_plugin()
                .map(|plugin| plugin.plugin_id.as_str()),
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
            ConfigField::PermissionsDefaultMode
            | ConfigField::MemoryEnabled
            | ConfigField::CompactionEnabled
            | ConfigField::CodeIntelEnabled
            | ConfigField::CodeIntelStartup
            | ConfigField::CodeIntelDiscoveryEnabled
            | ConfigField::CodeIntelDiscoveryReportMissing
            | ConfigField::TerminalMouseCapture
            | ConfigField::TerminalOsc52Clipboard
            | ConfigField::AppearanceTheme
            | ConfigField::AppearanceSyntaxTheme
            | ConfigField::AppearanceColorGroup
            | ConfigField::AppearanceColorToken => None,
            ConfigField::AppearanceColorOverride => self.draft.selected_appearance_color_override(),
        }
    }

    pub(crate) fn field_text_value_mut(&mut self, field: ConfigField) -> Option<&mut String> {
        match field {
            ConfigField::ProviderName => Some(&mut self.draft.provider_name),
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
            ConfigField::TerminalScrollSensitivity => {
                Some(&mut self.draft.terminal_scroll_sensitivity)
            }
            ConfigField::SkillId | ConfigField::PluginId => None,
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
            ConfigField::PermissionsDefaultMode
            | ConfigField::MemoryEnabled
            | ConfigField::CompactionEnabled
            | ConfigField::CodeIntelEnabled
            | ConfigField::CodeIntelStartup
            | ConfigField::CodeIntelDiscoveryEnabled
            | ConfigField::CodeIntelDiscoveryReportMissing
            | ConfigField::TerminalMouseCapture
            | ConfigField::TerminalOsc52Clipboard
            | ConfigField::AppearanceTheme
            | ConfigField::AppearanceSyntaxTheme
            | ConfigField::AppearanceColorGroup
            | ConfigField::AppearanceColorToken
            | ConfigField::AppearanceColorOverride => None,
        }
    }

    pub(crate) fn display_value(&self, field: ConfigField) -> String {
        let text_value = match field {
            ConfigField::ProviderFimModel
                if normalize_provider_name(&self.draft.provider_name) != DEEPSEEK_PROVIDER_KEY =>
            {
                return "not supported".to_owned();
            }
            ConfigField::ProviderApiKey => return mask_secret(&self.draft.provider_api_key),
            ConfigField::SkillId => {
                if self.selected_section == ConfigSection::Agents {
                    return self
                        .selected_agent()
                        .map(|agent| agent.profile.id.as_str().to_owned())
                        .unwrap_or_else(|| "none".to_owned());
                }
                return self
                    .selected_skill()
                    .map(|skill| skill.id.clone())
                    .unwrap_or_else(|| "none".to_owned());
            }
            ConfigField::PluginId => {
                return self
                    .selected_plugin()
                    .map(|plugin| plugin.plugin_id.clone())
                    .unwrap_or_else(|| "none".to_owned());
            }
            ConfigField::PermissionsDefaultMode => {
                return self.draft.permission_default_mode.as_str().to_owned();
            }
            ConfigField::MemoryEnabled => {
                return bool_label(self.draft.memory_enabled).to_owned();
            }
            ConfigField::CompactionEnabled => {
                return bool_label(self.draft.compaction_enabled).to_owned();
            }
            ConfigField::CodeIntelEnabled => {
                return bool_label(self.draft.code_intelligence_enabled).to_owned();
            }
            ConfigField::CodeIntelStartup => {
                return self.draft.code_intelligence_startup.as_str().to_owned();
            }
            ConfigField::CodeIntelDiscoveryEnabled => {
                return bool_label(self.draft.code_intelligence_discovery_enabled).to_owned();
            }
            ConfigField::CodeIntelDiscoveryReportMissing => {
                return bool_label(self.draft.code_intelligence_discovery_report_missing)
                    .to_owned();
            }
            ConfigField::TerminalMouseCapture => {
                return bool_label(self.draft.terminal_mouse_capture).to_owned();
            }
            ConfigField::TerminalOsc52Clipboard => {
                return bool_label(self.draft.terminal_osc52_clipboard).to_owned();
            }
            ConfigField::AppearanceTheme => {
                return self.draft.appearance_theme.as_str().to_owned();
            }
            ConfigField::AppearanceSyntaxTheme => {
                return self.draft.appearance_syntax_theme.as_str().to_owned();
            }
            ConfigField::AppearanceColorGroup => {
                return self.draft.selected_appearance_color_group().key.to_owned();
            }
            ConfigField::AppearanceColorToken => {
                return self.draft.selected_appearance_color_token().to_owned();
            }
            ConfigField::AppearanceColorOverride => {
                return self
                    .draft
                    .selected_appearance_color_override()
                    .map(ToOwned::to_owned)
                    .unwrap_or_else(|| "inherited".to_owned());
            }
            _ => self.field_text_value(field).unwrap_or_default(),
        };

        match field {
            ConfigField::CompactionSoftThresholdRatio
            | ConfigField::CompactionHardThresholdRatio => display_ratio(text_value),
            ConfigField::CompactionTailMessages => format!("{text_value} messages"),
            ConfigField::TerminalScrollSensitivity => format!("{text_value} rows"),
            ConfigField::McpArgsCsv if text_value.trim().is_empty() => "none".to_owned(),
            ConfigField::McpStartupTimeoutSecs => format!("{text_value} seconds"),
            ConfigField::CompactionContextWindowTokens if text_value.trim().is_empty() => {
                "provider/model metadata".to_owned()
            }
            ConfigField::CompactionContextWindowTokens => format!("{text_value} tokens"),
            _ => text_value.to_owned(),
        }
    }
}

pub(crate) fn load_deepseek_provider_config(
    root_config: &RootConfig,
) -> Option<DeepSeekProviderConfig> {
    root_config
        .providers
        .get("deepseek")
        .cloned()
        .and_then(|value| serde_json::from_value(value).ok())
}

pub(crate) fn load_openai_compat_provider_config(
    root_config: &RootConfig,
) -> Option<OpenAiCompatibleProviderConfig> {
    root_config
        .providers
        .get(OPENAI_COMPAT_PROVIDER_KEY)
        .cloned()
        .and_then(|value| serde_json::from_value(value).ok())
}

pub(crate) fn load_anthropic_provider_config(
    root_config: &RootConfig,
) -> Option<AnthropicProviderConfig> {
    root_config
        .providers
        .get(ANTHROPIC_PROVIDER_KEY)
        .cloned()
        .and_then(|value| serde_json::from_value(value).ok())
}

pub(crate) fn load_gemini_provider_config(
    root_config: &RootConfig,
) -> Option<GeminiProviderConfig> {
    root_config
        .providers
        .get(GEMINI_PROVIDER_KEY)
        .cloned()
        .and_then(|value| serde_json::from_value(value).ok())
}

pub(crate) fn default_deepseek_provider_config(model: &str) -> DeepSeekProviderConfig {
    DeepSeekProviderConfig::default_for_model(model)
}

pub(crate) fn default_openai_compat_provider_config(model: &str) -> OpenAiCompatibleProviderConfig {
    OpenAiCompatibleProviderConfig::default_for_model(model)
}

pub(crate) fn default_anthropic_provider_config(model: &str) -> AnthropicProviderConfig {
    AnthropicProviderConfig::default_for_model(model)
}

pub(crate) fn default_gemini_provider_config(model: &str) -> GeminiProviderConfig {
    GeminiProviderConfig::default_for_model(model)
}

fn default_provider_field_draft(provider_name: &str, model: &str) -> ProviderFieldDraft {
    match provider_name {
        OPENAI_COMPAT_PROVIDER_KEY => {
            ProviderFieldDraft::from_openai_compat(&default_openai_compat_provider_config(model))
        }
        ANTHROPIC_PROVIDER_KEY => {
            ProviderFieldDraft::from_anthropic(&default_anthropic_provider_config(model))
        }
        GEMINI_PROVIDER_KEY => {
            ProviderFieldDraft::from_gemini(&default_gemini_provider_config(model))
        }
        _ => ProviderFieldDraft::from_deepseek(&default_deepseek_provider_config(model)),
    }
}

pub(crate) fn serialize_deepseek_provider_value(
    provider_config: &DeepSeekProviderConfig,
) -> Result<serde_json::Value> {
    let mut value = serde_json::to_value(provider_config)
        .map_err(|error| anyhow!("failed to serialize deepseek provider config: {error}"))?;
    if let Some(object) = value.as_object_mut() {
        object.retain(|_, entry| !entry.is_null());
    }
    Ok(value)
}

pub(crate) fn serialize_openai_compat_provider_value(
    provider_config: &OpenAiCompatibleProviderConfig,
) -> Result<serde_json::Value> {
    let mut value = serde_json::to_value(provider_config)
        .map_err(|error| anyhow!("failed to serialize openai_compat provider config: {error}"))?;
    if let Some(object) = value.as_object_mut() {
        object.retain(|_, entry| !entry.is_null());
    }
    Ok(value)
}

pub(crate) fn serialize_anthropic_provider_value(
    provider_config: &AnthropicProviderConfig,
) -> Result<serde_json::Value> {
    let mut value = serde_json::to_value(provider_config)
        .map_err(|error| anyhow!("failed to serialize anthropic provider config: {error}"))?;
    if let Some(object) = value.as_object_mut() {
        object.retain(|_, entry| !entry.is_null());
    }
    Ok(value)
}

pub(crate) fn serialize_gemini_provider_value(
    provider_config: &GeminiProviderConfig,
) -> Result<serde_json::Value> {
    let mut value = serde_json::to_value(provider_config)
        .map_err(|error| anyhow!("failed to serialize gemini provider config: {error}"))?;
    if let Some(object) = value.as_object_mut() {
        object.retain(|_, entry| !entry.is_null());
    }
    Ok(value)
}

pub(crate) fn normalize_provider_name(provider: &str) -> &'static str {
    match provider_config_key(provider) {
        OPENAI_COMPAT_PROVIDER_KEY => OPENAI_COMPAT_PROVIDER_KEY,
        ANTHROPIC_PROVIDER_KEY => ANTHROPIC_PROVIDER_KEY,
        GEMINI_PROVIDER_KEY => GEMINI_PROVIDER_KEY,
        _ => DEEPSEEK_PROVIDER_KEY,
    }
}

pub(crate) fn cycle_provider_name(provider: &str) -> String {
    match normalize_provider_name(provider) {
        DEEPSEEK_PROVIDER_KEY => OPENAI_COMPAT_PROVIDER_KEY.to_owned(),
        OPENAI_COMPAT_PROVIDER_KEY => ANTHROPIC_PROVIDER_KEY.to_owned(),
        ANTHROPIC_PROVIDER_KEY => GEMINI_PROVIDER_KEY.to_owned(),
        _ => DEEPSEEK_PROVIDER_KEY.to_owned(),
    }
}

pub(crate) fn render_config_value_row(state: &ConfigState, field: ConfigField) -> String {
    let selected = !state.footer_selected && state.selected_field == Some(field);
    let marker = if selected { ">" } else { " " };
    let action = if selected && state.editing_field() != Some(field) {
        field.action_label()
    } else {
        ""
    };

    if action.is_empty() {
        format!(
            "{marker} {}: {}",
            field.display_label(),
            state.display_value(field)
        )
    } else {
        format!(
            "{marker} {}: {}  [{}]",
            field.display_label(),
            state.display_value(field),
            action
        )
    }
}

pub(crate) fn render_config_readonly_row(label: &str, value: &str) -> String {
    format!("- {label}: {value}")
}

pub(crate) fn config_field_accepts_char(field: ConfigField, character: char) -> bool {
    match field {
        ConfigField::CompactionContextWindowTokens
        | ConfigField::CompactionTailMessages
        | ConfigField::TerminalScrollSensitivity
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
        ConfigField::AppearanceColorOverride => character == '#' || character.is_ascii_hexdigit(),
        ConfigField::SkillId | ConfigField::PluginId => false,
        ConfigField::ProviderApiKey
        | ConfigField::ProviderName
        | ConfigField::PermissionsDefaultMode
        | ConfigField::MemoryEnabled
        | ConfigField::CompactionEnabled
        | ConfigField::CodeIntelEnabled
        | ConfigField::CodeIntelStartup
        | ConfigField::CodeIntelDiscoveryEnabled
        | ConfigField::CodeIntelDiscoveryReportMissing
        | ConfigField::TerminalMouseCapture
        | ConfigField::TerminalOsc52Clipboard
        | ConfigField::AppearanceTheme
        | ConfigField::AppearanceSyntaxTheme
        | ConfigField::AppearanceColorGroup
        | ConfigField::AppearanceColorToken => false,
    }
}

fn mask_secret(value: &str) -> String {
    if value.is_empty() {
        "not set".to_owned()
    } else {
        "set (hidden)".to_owned()
    }
}

fn bool_label(enabled: bool) -> &'static str {
    if enabled { "yes" } else { "no" }
}

fn display_ratio(value: &str) -> String {
    match value.trim().parse::<f32>() {
        Ok(ratio) if ratio.is_finite() => format!("{}% ({})", (ratio * 100.0).round(), value),
        _ => value.to_owned(),
    }
}

fn skill_display_order(descriptors: &[SkillDescriptor]) -> Vec<usize> {
    let mut agents = Vec::new();
    let mut skills = Vec::new();
    for (index, descriptor) in descriptors.iter().enumerate() {
        if skill_is_agent(descriptor) {
            agents.push(index);
        } else {
            skills.push(index);
        }
    }
    agents.extend(skills);
    agents
}

fn skill_display_order_for_section(
    descriptors: &[SkillDescriptor],
    section: ConfigSection,
) -> Vec<usize> {
    match section {
        ConfigSection::Agents => descriptors
            .iter()
            .enumerate()
            .filter_map(|(index, descriptor)| skill_is_agent(descriptor).then_some(index))
            .collect(),
        ConfigSection::Skills => descriptors
            .iter()
            .enumerate()
            .filter_map(|(index, descriptor)| (!skill_is_agent(descriptor)).then_some(index))
            .collect(),
        _ => skill_display_order(descriptors),
    }
}

fn skill_is_agent(descriptor: &SkillDescriptor) -> bool {
    matches!(descriptor.run_as, SkillRunMode::ChildSession)
}

#[cfg(all(test, not(sigil_tui_test_slice_app_input_flow)))]
#[path = "tests/config_panel_tests.rs"]
mod tests;

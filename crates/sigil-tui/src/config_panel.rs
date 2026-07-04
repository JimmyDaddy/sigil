use std::collections::BTreeMap;

use anyhow::{Result, anyhow, bail};
use sigil_kernel::{
    CodeIntelStartup, CodeIntelligenceConfig, McpServerConfig, PermissionMode,
    PluginManifestSnapshot, RootConfig, SkillDescriptor, SkillRunMode, SyntaxThemeId,
    TerminalKeyboardEnhancement, ThemeId, UsageCostCurrency, VerificationAutoRunPolicy,
};
pub(crate) use sigil_runtime::{
    ANTHROPIC_PROVIDER_KEY, DEEPSEEK_PROVIDER_KEY, GEMINI_PROVIDER_KEY, OPENAI_COMPAT_PROVIDER_KEY,
    normalize_provider_name,
};
use sigil_runtime::{
    DeepSeekProviderConfigFields, ModelRequestConfigFields, ProviderConfigFields,
    ProviderStrictToolsMode, ResolvedAgentProfile, deepseek_provider_config_fields,
    default_provider_config_fields, model_request_config_fields, provider_config_fields,
    set_model_request_config_fields, set_provider_config_fields, supported_provider_name,
};

use crate::ui::theme::{COLOR_TOKEN_GROUPS, COLOR_TOKEN_NAMES, ColorTokenGroup};

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
    pub(crate) const DEFAULT_FLOW: [Self; 6] = [
        Self::Provider,
        Self::Permissions,
        Self::Memory,
        Self::Compaction,
        Self::Mcp,
        Self::Appearance,
    ];

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

    pub(crate) fn visible_flow(show_advanced: bool) -> &'static [Self] {
        if show_advanced {
            &Self::FLOW
        } else {
            &Self::DEFAULT_FLOW
        }
    }

    pub(crate) fn is_default_surface(self) -> bool {
        Self::DEFAULT_FLOW.contains(&self)
    }

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
            Self::Permissions => "permissions",
            Self::Memory => "memory",
            Self::Compaction => "compaction",
            Self::CodeIntelligence => "code intel",
            Self::Terminal => "terminal",
            Self::Appearance => "appearance",
            Self::Agents => "agents",
            Self::Skills => "skills",
            Self::Plugins => "plugins",
            Self::Mcp => "mcp",
        }
    }

    pub(crate) fn step_token(self) -> &'static str {
        match self {
            Self::CodeIntelligence => "code-intel",
            _ => self.nav_label(),
        }
    }

    pub(crate) fn summary(self) -> &'static str {
        match self {
            Self::Provider => "provider settings",
            Self::Storage => "local state paths",
            Self::Permissions => "safety settings",
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

    #[cfg(test)]
    pub(crate) fn next_flow(self) -> Self {
        let index = Self::FLOW
            .iter()
            .position(|section| *section == self)
            .unwrap_or(0);
        Self::FLOW[(index + 1) % Self::FLOW.len()]
    }

    #[cfg(test)]
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
    ModelRequestTimeoutSecs,
    ModelRequestStreamIdleTimeoutSecs,
    // Low-frequency provider endpoint/FIM controls remain part of the persisted
    // draft model, but the default config flow keeps them in sigil.toml.
    #[allow(dead_code)]
    ProviderBaseUrl,
    #[allow(dead_code)]
    ProviderFimModel,
    PermissionMode,
    // Verification auto-run is a policy-file concern. Task status owns
    // immediate run/retry actions in the product surface.
    #[allow(dead_code)]
    VerificationAutoRun,
    MemoryEnabled,
    CompactionEnabled,
    CompactionSoftThresholdRatio,
    CompactionHardThresholdRatio,
    CompactionContextWindowTokens,
    CompactionTailMessages,
    CodeIntelEnabled,
    CodeIntelServerStartup,
    // Discovery details stay in sigil.toml / doctor; the default TUI keeps only
    // the main code-intelligence mode controls.
    #[allow(dead_code)]
    CodeIntelAutoDiscover,
    #[allow(dead_code)]
    CodeIntelReportMissing,
    // Terminal compatibility knobs stay in sigil.toml / doctor guidance rather
    // than the default configuration flow.
    #[allow(dead_code)]
    TerminalMouseCapture,
    #[allow(dead_code)]
    TerminalOsc52Clipboard,
    #[allow(dead_code)]
    TerminalScrollSensitivity,
    AppearanceTheme,
    AppearanceSyntaxTheme,
    AppearanceUsageCostCurrency,
    // Fine-grained color-token editing stays in sigil.toml. The default TUI
    // exposes coarse theme choices and previews.
    #[allow(dead_code)]
    AppearanceColorGroup,
    #[allow(dead_code)]
    AppearanceColorToken,
    #[allow(dead_code)]
    AppearanceColorOverride,
    SkillId,
    PluginId,
    // MCP server editing stays in sigil.toml; these variants remain for
    // config-file draft validation coverage, not the default TUI field list.
    #[allow(dead_code)]
    McpName,
    #[allow(dead_code)]
    McpCommand,
    #[allow(dead_code)]
    McpArgsCsv,
    #[allow(dead_code)]
    McpStartupTimeoutSecs,
}

impl ConfigField {
    const PROVIDER_FIELDS: [Self; 5] = [
        Self::ProviderModel,
        Self::ProviderApiKey,
        Self::ModelRequestTimeoutSecs,
        Self::ModelRequestStreamIdleTimeoutSecs,
        Self::ProviderName,
    ];
    const STORAGE_FIELDS: [Self; 0] = [];
    const PERMISSION_FIELDS: [Self; 1] = [Self::PermissionMode];
    const MEMORY_FIELDS: [Self; 1] = [Self::MemoryEnabled];
    const COMPACTION_FIELDS: [Self; 5] = [
        Self::CompactionEnabled,
        Self::CompactionContextWindowTokens,
        Self::CompactionSoftThresholdRatio,
        Self::CompactionHardThresholdRatio,
        Self::CompactionTailMessages,
    ];
    const CODE_INTELLIGENCE_FIELDS: [Self; 2] =
        [Self::CodeIntelEnabled, Self::CodeIntelServerStartup];
    const TERMINAL_FIELDS: [Self; 0] = [];
    const APPEARANCE_FIELDS: [Self; 3] = [
        Self::AppearanceTheme,
        Self::AppearanceSyntaxTheme,
        Self::AppearanceUsageCostCurrency,
    ];
    const SKILL_FIELDS: [Self; 1] = [Self::SkillId];
    const PLUGIN_FIELDS: [Self; 1] = [Self::PluginId];
    const MCP_FIELDS: [Self; 0] = [];

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
            Self::ModelRequestTimeoutSecs => "request_start_timeout",
            Self::ModelRequestStreamIdleTimeoutSecs => "stream_idle_timeout",
            Self::ProviderBaseUrl => "base_url",
            Self::ProviderFimModel => "fim_model",
            Self::PermissionMode => "mode",
            Self::VerificationAutoRun => "checks",
            Self::MemoryEnabled => "enabled",
            Self::CompactionEnabled => "enabled",
            Self::CompactionSoftThresholdRatio => "soft_threshold",
            Self::CompactionHardThresholdRatio => "hard_threshold",
            Self::CompactionContextWindowTokens => "fallback_window",
            Self::CompactionTailMessages => "tail_messages",
            Self::CodeIntelEnabled => "enabled",
            Self::CodeIntelServerStartup => "server_startup",
            Self::CodeIntelAutoDiscover => "auto_discover",
            Self::CodeIntelReportMissing => "report_missing",
            Self::TerminalMouseCapture => "mouse_capture",
            Self::TerminalOsc52Clipboard => "osc52_clipboard",
            Self::TerminalScrollSensitivity => "scroll_sensitivity",
            Self::AppearanceTheme => "theme",
            Self::AppearanceSyntaxTheme => "syntax_theme",
            Self::AppearanceUsageCostCurrency => "usage_cost_currency",
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
            Self::ModelRequestTimeoutSecs => "Request start timeout",
            Self::ModelRequestStreamIdleTimeoutSecs => "Stream idle timeout",
            Self::ProviderBaseUrl => "Endpoint",
            Self::ProviderFimModel => "FIM model",
            Self::PermissionMode => "Mode",
            Self::VerificationAutoRun => "Checks",
            Self::MemoryEnabled => "Memory",
            Self::CompactionEnabled => "Auto compact",
            Self::CompactionSoftThresholdRatio => "Soft threshold",
            Self::CompactionHardThresholdRatio => "Hard threshold",
            Self::CompactionContextWindowTokens => "Fallback window",
            Self::CompactionTailMessages => "Tail messages",
            Self::CodeIntelEnabled => "Code intelligence",
            Self::CodeIntelServerStartup => "Server startup",
            Self::CodeIntelAutoDiscover => "Auto discover",
            Self::CodeIntelReportMissing => "Missing reports",
            Self::TerminalMouseCapture => "Mouse capture",
            Self::TerminalOsc52Clipboard => "OSC52 clipboard",
            Self::TerminalScrollSensitivity => "Scroll sensitivity",
            Self::AppearanceTheme => "Theme",
            Self::AppearanceSyntaxTheme => "Syntax theme",
            Self::AppearanceUsageCostCurrency => "Cost currency",
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
            Self::ModelRequestTimeoutSecs => {
                "Seconds to wait for the model provider to accept a request and return response headers."
            }
            Self::ModelRequestStreamIdleTimeoutSecs => {
                "Seconds a streaming response may stay idle between chunks before Sigil treats it as failed."
            }
            Self::ProviderBaseUrl => {
                "Provider API base URL. Leave this unchanged unless you use a proxy or compatible endpoint."
            }
            Self::ProviderFimModel => {
                "DeepSeek-only model used by prefix/FIM helpers. Chat runs use Model."
            }
            Self::PermissionMode => {
                "Top-level safety mode: read-only, manual confirmation, automatic workspace edits, or danger full access."
            }
            Self::VerificationAutoRun => {
                "Controls whether trusted project checks may start automatically after writes."
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
            Self::CodeIntelServerStartup => {
                "Controls whether code intelligence is off, lazily started, or prepared eagerly."
            }
            Self::CodeIntelAutoDiscover => {
                "Uses safe built-in discovery to add common language servers found on PATH."
            }
            Self::CodeIntelReportMissing => {
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
            Self::AppearanceUsageCostCurrency => {
                "Display currency for usage cost estimates. Auto follows the provider balance currency when available."
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
                "Selected reusable skill. Up/Down moves through skills; footer action uses it."
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
                | Self::ModelRequestTimeoutSecs
                | Self::ModelRequestStreamIdleTimeoutSecs
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
            Self::PermissionMode
            | Self::VerificationAutoRun
            | Self::CodeIntelServerStartup
            | Self::AppearanceTheme
            | Self::AppearanceSyntaxTheme => "Enter cycle",
            Self::AppearanceUsageCostCurrency => "Enter cycle",
            Self::AppearanceColorGroup | Self::AppearanceColorToken => "Enter cycle",
            Self::MemoryEnabled
            | Self::CompactionEnabled
            | Self::CodeIntelEnabled
            | Self::CodeIntelAutoDiscover
            | Self::CodeIntelReportMissing
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
    #[cfg(test)]
    SaveAndClose,
    CleanMutationArtifacts,
    ActivateMcp,
    TrustAgent,
    BlockAgent,
    #[cfg(test)]
    ToggleAgentEnabled,
    #[cfg(test)]
    ToggleAgentUser,
    #[cfg(test)]
    ToggleAgentModel,
    UseSkill,
    ApprovePlugin,
    DenyPlugin,
    Close,
}

impl ConfigFooterAction {
    const DEFAULT_ORDER: [Self; 2] = [Self::Save, Self::Close];
    const STORAGE_ORDER: [Self; 2] = [Self::CleanMutationArtifacts, Self::Close];
    const PERMISSIONS_ORDER: [Self; 2] = [Self::Save, Self::Close];
    const MCP_ORDER: [Self; 2] = [Self::ActivateMcp, Self::Close];
    const AGENTS_ORDER: [Self; 2] = [Self::TrustAgent, Self::BlockAgent];
    const SKILLS_ORDER: [Self; 2] = [Self::UseSkill, Self::Close];
    const PLUGINS_ORDER: [Self; 2] = [Self::ApprovePlugin, Self::DenyPlugin];

    pub(crate) fn actions_for_section(section: ConfigSection) -> &'static [Self] {
        match section {
            ConfigSection::Mcp => &Self::MCP_ORDER,
            ConfigSection::Storage => &Self::STORAGE_ORDER,
            ConfigSection::Permissions => &Self::PERMISSIONS_ORDER,
            ConfigSection::Agents => &Self::AGENTS_ORDER,
            ConfigSection::Skills => &Self::SKILLS_ORDER,
            ConfigSection::Plugins => &Self::PLUGINS_ORDER,
            ConfigSection::Provider
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
            #[cfg(test)]
            Self::SaveAndClose => "save+close",
            Self::CleanMutationArtifacts => "clean",
            Self::ActivateMcp => "activate",
            Self::TrustAgent => "trust",
            Self::BlockAgent => "disable",
            #[cfg(test)]
            Self::ToggleAgentEnabled => "enable",
            #[cfg(test)]
            Self::ToggleAgentUser => "user",
            #[cfg(test)]
            Self::ToggleAgentModel => "model",
            Self::UseSkill => "use",
            Self::ApprovePlugin => "approve",
            Self::DenyPlugin => "deny",
            Self::Close => "close",
        }
    }

    pub(crate) fn field_label(self) -> &'static str {
        match self {
            Self::Save => "save",
            #[cfg(test)]
            Self::SaveAndClose => "save_and_close",
            Self::CleanMutationArtifacts => "clean_artifacts",
            Self::ActivateMcp => "activate_mcp",
            Self::TrustAgent => "trust_agent",
            Self::BlockAgent => "disable_agent",
            #[cfg(test)]
            Self::ToggleAgentEnabled => "toggle_agent_enabled",
            #[cfg(test)]
            Self::ToggleAgentUser => "toggle_agent_user",
            #[cfg(test)]
            Self::ToggleAgentModel => "toggle_agent_model",
            Self::UseSkill => "use_skill",
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
    pub(crate) provider_strict_tools_mode: ProviderStrictToolsMode,
    pub(crate) provider_fim_model: String,
    pub(crate) model_request_timeout_secs: String,
    pub(crate) model_request_stream_idle_timeout_secs: String,
    pub(crate) permission_mode: PermissionMode,
    pub(crate) verification_auto_run: VerificationAutoRunPolicy,
    pub(crate) memory_enabled: bool,
    pub(crate) compaction_enabled: bool,
    pub(crate) compaction_soft_threshold_ratio: String,
    pub(crate) compaction_hard_threshold_ratio: String,
    pub(crate) compaction_context_window_tokens: String,
    pub(crate) compaction_tail_messages: String,
    pub(crate) code_intelligence_enabled: bool,
    pub(crate) code_intelligence_server_startup: CodeIntelStartup,
    pub(crate) code_intelligence_auto_discover: bool,
    pub(crate) code_intelligence_report_missing: bool,
    pub(crate) terminal_keyboard_enhancement: TerminalKeyboardEnhancement,
    pub(crate) terminal_mouse_capture: bool,
    pub(crate) terminal_osc52_clipboard: bool,
    pub(crate) terminal_scroll_sensitivity: String,
    pub(crate) appearance_theme: ThemeId,
    pub(crate) appearance_syntax_theme: SyntaxThemeId,
    pub(crate) appearance_usage_cost_currency: UsageCostCurrency,
    pub(crate) appearance_color_group_index: usize,
    pub(crate) appearance_color_token_index: usize,
    pub(crate) mcp_servers: Vec<McpServerDraft>,
}

type ProviderFieldDraft = ProviderConfigFields;

impl ConfigDraft {
    pub(crate) fn from_root_config(root_config: &RootConfig) -> Self {
        let provider_name = normalize_provider_name(&root_config.agent.provider).to_owned();
        let deepseek_fields =
            deepseek_provider_config_fields(root_config, &root_config.agent.model);
        let mut provider_drafts = BTreeMap::new();
        provider_drafts.insert(
            DEEPSEEK_PROVIDER_KEY.to_owned(),
            provider_config_fields(root_config, DEEPSEEK_PROVIDER_KEY, &root_config.agent.model),
        );
        provider_drafts.insert(
            OPENAI_COMPAT_PROVIDER_KEY.to_owned(),
            provider_config_fields(
                root_config,
                OPENAI_COMPAT_PROVIDER_KEY,
                &root_config.agent.model,
            ),
        );
        provider_drafts.insert(
            ANTHROPIC_PROVIDER_KEY.to_owned(),
            provider_config_fields(
                root_config,
                ANTHROPIC_PROVIDER_KEY,
                &root_config.agent.model,
            ),
        );
        provider_drafts.insert(
            GEMINI_PROVIDER_KEY.to_owned(),
            provider_config_fields(root_config, GEMINI_PROVIDER_KEY, &root_config.agent.model),
        );
        let current_provider_draft = provider_drafts
            .get(provider_name.as_str())
            .cloned()
            .unwrap_or_else(|| {
                default_provider_field_draft(&provider_name, &root_config.agent.model)
            });
        let model_request_fields = model_request_config_fields(root_config);
        Self {
            base_root_config: root_config.clone(),
            provider_name: provider_name.clone(),
            provider_model: current_provider_draft.model,
            provider_api_key: current_provider_draft.api_key,
            provider_base_url: current_provider_draft.base_url,
            provider_drafts,
            provider_beta_base_url: deepseek_fields.beta_base_url,
            provider_anthropic_base_url: deepseek_fields.anthropic_base_url,
            provider_user_id_strategy: deepseek_fields.user_id_strategy,
            provider_strict_tools_mode: deepseek_fields.strict_tools_mode,
            provider_fim_model: deepseek_fields.fim_model,
            model_request_timeout_secs: model_request_fields.request_timeout_secs,
            model_request_stream_idle_timeout_secs: model_request_fields.stream_idle_timeout_secs,
            permission_mode: root_config.permission.mode,
            verification_auto_run: root_config.verification.auto_run,
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
            code_intelligence_server_startup: root_config.code_intelligence.server_startup,
            code_intelligence_auto_discover: root_config.code_intelligence.auto_discover,
            code_intelligence_report_missing: root_config.code_intelligence.report_missing,
            terminal_keyboard_enhancement: root_config.terminal.keyboard_enhancement,
            terminal_mouse_capture: root_config.terminal.mouse_capture,
            terminal_osc52_clipboard: root_config.terminal.osc52_clipboard,
            terminal_scroll_sensitivity: root_config.terminal.scroll_sensitivity.to_string(),
            appearance_theme: root_config.appearance.theme,
            appearance_syntax_theme: root_config.appearance.syntax_theme,
            appearance_usage_cost_currency: root_config.appearance.usage_cost_currency,
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
    }

    pub(crate) fn to_root_config(&self) -> Result<RootConfig> {
        let provider_name = normalize_provider_name(&self.provider_name);
        supported_provider_name(provider_name)?;
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
        root_config.permission.mode = self.permission_mode;
        root_config.verification.auto_run = self.verification_auto_run;
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
        root_config.appearance.usage_cost_currency = self.appearance_usage_cost_currency;
        root_config.appearance.colors = self.base_root_config.appearance.colors.clone();
        root_config.mcp_servers = self
            .mcp_servers
            .iter()
            .enumerate()
            .map(|(index, server)| server.to_config(index))
            .collect::<Result<Vec<_>>>()?;

        let provider_fields = ProviderConfigFields {
            model: model.to_owned(),
            api_key: api_key.to_owned(),
            base_url: base_url.to_owned(),
        };
        let model_request_fields = ModelRequestConfigFields {
            request_timeout_secs: self.model_request_timeout_secs.clone(),
            stream_idle_timeout_secs: self.model_request_stream_idle_timeout_secs.clone(),
        };
        set_model_request_config_fields(&mut root_config, &model_request_fields)?;
        let deepseek_fields = DeepSeekProviderConfigFields {
            beta_base_url: self.provider_beta_base_url.trim().to_owned(),
            anthropic_base_url: self.provider_anthropic_base_url.trim().to_owned(),
            user_id_strategy: self.provider_user_id_strategy.trim().to_owned(),
            strict_tools_mode: self.provider_strict_tools_mode,
            fim_model: self.provider_fim_model.trim().to_owned(),
        };
        set_provider_config_fields(
            &mut root_config,
            provider_name,
            &provider_fields,
            Some(&deepseek_fields),
        )?;
        Ok(root_config)
    }

    pub(crate) fn code_intelligence_config(&self) -> CodeIntelligenceConfig {
        let mut config = self.base_root_config.code_intelligence.clone();
        config.enabled = self.code_intelligence_enabled;
        config.server_startup = self.code_intelligence_server_startup;
        config.auto_discover = self.code_intelligence_auto_discover;
        config.report_missing = self.code_intelligence_report_missing;
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

    pub(crate) fn cycle_appearance_usage_cost_currency(&mut self) {
        self.appearance_usage_cost_currency = self.appearance_usage_cost_currency.next();
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

    #[allow(dead_code)]
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

    #[allow(dead_code)]
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
    pub(crate) show_advanced: bool,
    pub(crate) selected_field: Option<ConfigField>,
    pub(crate) footer_selected: bool,
    pub(crate) selected_footer_action: ConfigFooterAction,
    pub(crate) selected_mcp_server_index: usize,
    pub(crate) selected_agent_index: usize,
    pub(crate) selected_skill_index: usize,
    pub(crate) selected_plugin_index: usize,
    pub(crate) selected_storage_artifact_index: usize,
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
            show_advanced: false,
            selected_field: ConfigField::fields_for_section(selected_section)
                .first()
                .copied(),
            footer_selected: false,
            selected_footer_action: ConfigFooterAction::Save,
            selected_mcp_server_index: 0,
            selected_agent_index: 0,
            selected_skill_index: 0,
            selected_plugin_index: 0,
            selected_storage_artifact_index: 0,
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
        if !section.is_default_surface() {
            self.show_advanced = true;
        }
        self.selected_section = section;
        self.sync_mcp_selection();
        self.sync_agent_selection();
        self.sync_skill_selection();
        self.sync_plugin_selection();
        self.footer_selected = false;
        self.selected_field = self.first_field_for_section(section);
    }

    pub(crate) fn visible_sections(&self) -> &'static [ConfigSection] {
        ConfigSection::visible_flow(self.show_advanced)
    }

    pub(crate) fn toggle_advanced_surface(&mut self) {
        self.show_advanced = !self.show_advanced;
        if !self.show_advanced && !self.selected_section.is_default_surface() {
            self.set_section(ConfigSection::Provider);
        }
    }

    pub(crate) fn set_next_visible_section(&mut self) {
        let sections = self.visible_sections();
        let index = sections
            .iter()
            .position(|section| *section == self.selected_section)
            .unwrap_or(0);
        self.set_section(sections[(index + 1) % sections.len()]);
    }

    pub(crate) fn set_previous_visible_section(&mut self) {
        let sections = self.visible_sections();
        let index = sections
            .iter()
            .position(|section| *section == self.selected_section)
            .unwrap_or(0);
        let next_index = if index == 0 {
            sections.len().saturating_sub(1)
        } else {
            index - 1
        };
        self.set_section(sections[next_index]);
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

    #[allow(dead_code)]
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
            self.selected_field = None;
        }
        self.dirty = true;
    }

    #[allow(dead_code)]
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
            ConfigField::ModelRequestTimeoutSecs => Some(&self.draft.model_request_timeout_secs),
            ConfigField::ModelRequestStreamIdleTimeoutSecs => {
                Some(&self.draft.model_request_stream_idle_timeout_secs)
            }
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
            ConfigField::PermissionMode
            | ConfigField::VerificationAutoRun
            | ConfigField::MemoryEnabled
            | ConfigField::CompactionEnabled
            | ConfigField::CodeIntelEnabled
            | ConfigField::CodeIntelServerStartup
            | ConfigField::CodeIntelAutoDiscover
            | ConfigField::CodeIntelReportMissing
            | ConfigField::TerminalMouseCapture
            | ConfigField::TerminalOsc52Clipboard
            | ConfigField::AppearanceTheme
            | ConfigField::AppearanceSyntaxTheme
            | ConfigField::AppearanceUsageCostCurrency
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
            ConfigField::ModelRequestTimeoutSecs => {
                Some(&mut self.draft.model_request_timeout_secs)
            }
            ConfigField::ModelRequestStreamIdleTimeoutSecs => {
                Some(&mut self.draft.model_request_stream_idle_timeout_secs)
            }
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
            ConfigField::PermissionMode
            | ConfigField::VerificationAutoRun
            | ConfigField::MemoryEnabled
            | ConfigField::CompactionEnabled
            | ConfigField::CodeIntelEnabled
            | ConfigField::CodeIntelServerStartup
            | ConfigField::CodeIntelAutoDiscover
            | ConfigField::CodeIntelReportMissing
            | ConfigField::TerminalMouseCapture
            | ConfigField::TerminalOsc52Clipboard
            | ConfigField::AppearanceTheme
            | ConfigField::AppearanceSyntaxTheme => None,
            ConfigField::AppearanceUsageCostCurrency => None,
            ConfigField::AppearanceColorGroup
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
            ConfigField::PermissionMode => {
                return permission_mode_label(self.draft.permission_mode).to_owned();
            }
            ConfigField::VerificationAutoRun => {
                return verification_auto_run_label(self.draft.verification_auto_run).to_owned();
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
            ConfigField::CodeIntelServerStartup => {
                return self
                    .draft
                    .code_intelligence_server_startup
                    .as_str()
                    .to_owned();
            }
            ConfigField::CodeIntelAutoDiscover => {
                return bool_label(self.draft.code_intelligence_auto_discover).to_owned();
            }
            ConfigField::CodeIntelReportMissing => {
                return bool_label(self.draft.code_intelligence_report_missing).to_owned();
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
            ConfigField::AppearanceUsageCostCurrency => {
                let currency = self.draft.appearance_usage_cost_currency.as_str();
                return currency.to_owned();
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
            ConfigField::ModelRequestTimeoutSecs
            | ConfigField::ModelRequestStreamIdleTimeoutSecs
            | ConfigField::McpStartupTimeoutSecs => format!("{text_value} seconds"),
            ConfigField::TerminalScrollSensitivity => format!("{text_value} rows"),
            ConfigField::McpArgsCsv if text_value.trim().is_empty() => "none".to_owned(),
            ConfigField::CompactionContextWindowTokens if text_value.trim().is_empty() => {
                "provider/model metadata".to_owned()
            }
            ConfigField::CompactionContextWindowTokens => format!("{text_value} tokens"),
            _ => text_value.to_owned(),
        }
    }
}

fn default_provider_field_draft(provider_name: &str, model: &str) -> ProviderFieldDraft {
    default_provider_config_fields(provider_name, model)
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
        | ConfigField::ModelRequestTimeoutSecs
        | ConfigField::ModelRequestStreamIdleTimeoutSecs
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
        | ConfigField::PermissionMode
        | ConfigField::VerificationAutoRun
        | ConfigField::MemoryEnabled
        | ConfigField::CompactionEnabled
        | ConfigField::CodeIntelEnabled
        | ConfigField::CodeIntelServerStartup
        | ConfigField::CodeIntelAutoDiscover
        | ConfigField::CodeIntelReportMissing
        | ConfigField::TerminalMouseCapture
        | ConfigField::TerminalOsc52Clipboard
        | ConfigField::AppearanceTheme
        | ConfigField::AppearanceSyntaxTheme => false,
        ConfigField::AppearanceUsageCostCurrency => false,
        ConfigField::AppearanceColorGroup | ConfigField::AppearanceColorToken => false,
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

fn permission_mode_label(mode: PermissionMode) -> &'static str {
    match mode {
        PermissionMode::ReadOnly => "read-only",
        PermissionMode::Manual => "manual",
        PermissionMode::AutoEdit => "auto-edit",
        PermissionMode::DangerFullAccess => "danger-full-access",
    }
}

fn verification_auto_run_label(policy: VerificationAutoRunPolicy) -> &'static str {
    match policy {
        VerificationAutoRunPolicy::Manual => "manual",
        VerificationAutoRunPolicy::TrustedOnly => "auto trusted",
        VerificationAutoRunPolicy::Never => "off",
    }
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

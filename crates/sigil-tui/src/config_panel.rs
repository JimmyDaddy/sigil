use anyhow::{Result, anyhow, bail};
use sigil_kernel::{
    ApprovalMode, CodeIntelStartup, CodeIntelligenceConfig, McpServerConfig, RootConfig,
};
use sigil_provider_deepseek::{DeepSeekProviderConfig, StrictToolsMode};
use sigil_provider_openai_compat::OpenAiCompatibleProviderConfig;

pub(crate) const DEEPSEEK_PROVIDER_KEY: &str = "deepseek";
pub(crate) const OPENAI_COMPAT_PROVIDER_KEY: &str = "openai_compat";

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
    Permissions,
    Memory,
    Compaction,
    CodeIntelligence,
    Terminal,
    Mcp,
}

impl ConfigSection {
    pub(crate) const FLOW: [Self; 7] = [
        Self::Provider,
        Self::Permissions,
        Self::Memory,
        Self::Compaction,
        Self::CodeIntelligence,
        Self::Terminal,
        Self::Mcp,
    ];

    pub(crate) fn title(self) -> &'static str {
        match self {
            Self::Provider => "Provider",
            Self::Permissions => "Permissions",
            Self::Memory => "Memory",
            Self::Compaction => "Compaction",
            Self::CodeIntelligence => "Code Intel",
            Self::Terminal => "Terminal",
            Self::Mcp => "MCP",
        }
    }

    pub(crate) fn summary(self) -> &'static str {
        match self {
            Self::Provider => "provider settings",
            Self::Permissions => "approval rules",
            Self::Memory => "memory status",
            Self::Compaction => "context and thresholds",
            Self::CodeIntelligence => "LSP readiness",
            Self::Terminal => "terminal integration",
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
    const MCP_FIELDS: [Self; 4] = [
        Self::McpName,
        Self::McpCommand,
        Self::McpArgsCsv,
        Self::McpStartupTimeoutSecs,
    ];

    pub(crate) fn fields_for_section(section: ConfigSection) -> &'static [Self] {
        match section {
            ConfigSection::Provider => &Self::PROVIDER_FIELDS,
            ConfigSection::Permissions => &Self::PERMISSION_FIELDS,
            ConfigSection::Memory => &Self::MEMORY_FIELDS,
            ConfigSection::Compaction => &Self::COMPACTION_FIELDS,
            ConfigSection::CodeIntelligence => &Self::CODE_INTELLIGENCE_FIELDS,
            ConfigSection::Terminal => &Self::TERMINAL_FIELDS,
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
                "Saved locally when entered here. SIGIL_API_KEY still overrides it at runtime."
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
            Self::PermissionsDefaultMode | Self::CodeIntelStartup => "Enter cycle",
            Self::MemoryEnabled
            | Self::CompactionEnabled
            | Self::CodeIntelEnabled
            | Self::CodeIntelDiscoveryEnabled
            | Self::CodeIntelDiscoveryReportMissing
            | Self::TerminalMouseCapture
            | Self::TerminalOsc52Clipboard => "Enter toggle",
            Self::TerminalScrollSensitivity => "Enter input",
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

    pub(crate) fn actions_for_section(section: ConfigSection) -> &'static [Self] {
        match section {
            ConfigSection::Mcp => &Self::MCP_ORDER,
            ConfigSection::Provider
            | ConfigSection::Permissions
            | ConfigSection::Memory
            | ConfigSection::Compaction
            | ConfigSection::CodeIntelligence
            | ConfigSection::Terminal => &Self::DEFAULT_ORDER,
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
            Self::Close => "close",
        }
    }

    pub(crate) fn field_label(self) -> &'static str {
        match self {
            Self::Save => "save",
            Self::SaveAndClose => "save_and_close",
            Self::ActivateMcp => "activate_mcp",
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
    pub(crate) mcp_servers: Vec<McpServerDraft>,
}

impl ConfigDraft {
    pub(crate) fn from_root_config(root_config: &RootConfig) -> Self {
        let provider_name = normalize_provider_name(&root_config.agent.provider).to_owned();
        let deepseek_provider = load_deepseek_provider_config(root_config)
            .unwrap_or_else(|| default_deepseek_provider_config(&root_config.agent.model));
        let openai_provider = load_openai_compat_provider_config(root_config)
            .unwrap_or_else(|| default_openai_compat_provider_config(&root_config.agent.model));
        Self {
            base_root_config: root_config.clone(),
            provider_name: provider_name.clone(),
            provider_model: if provider_name == OPENAI_COMPAT_PROVIDER_KEY {
                openai_provider.model
            } else {
                deepseek_provider.model
            },
            provider_api_key: if provider_name == OPENAI_COMPAT_PROVIDER_KEY {
                openai_provider.api_key.unwrap_or_default()
            } else {
                deepseek_provider.api_key.unwrap_or_default()
            },
            provider_base_url: if provider_name == OPENAI_COMPAT_PROVIDER_KEY {
                openai_provider.base_url
            } else {
                deepseek_provider.base_url
            },
            provider_beta_base_url: deepseek_provider.beta_base_url,
            provider_anthropic_base_url: deepseek_provider.anthropic_base_url,
            provider_user_id_strategy: deepseek_provider.user_id_strategy.unwrap_or_default(),
            provider_strict_tools_mode: deepseek_provider.strict_tools_mode,
            provider_fim_model: deepseek_provider.fim_model,
            provider_request_timeout_secs: if provider_name == OPENAI_COMPAT_PROVIDER_KEY {
                openai_provider.request_timeout_secs.to_string()
            } else {
                deepseek_provider.request_timeout_secs.to_string()
            },
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
            mcp_servers: root_config
                .mcp_servers
                .iter()
                .map(McpServerDraft::from_config)
                .collect(),
        }
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
        root_config.mcp_servers = self
            .mcp_servers
            .iter()
            .enumerate()
            .map(|(index, server)| server.to_config(index))
            .collect::<Result<Vec<_>>>()?;

        if provider_name == OPENAI_COMPAT_PROVIDER_KEY {
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
        } else {
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
            provider_config.user_id_strategy = (!self.provider_user_id_strategy.trim().is_empty())
                .then(|| self.provider_user_id_strategy.trim().to_owned());
            provider_config.strict_tools_mode = self.provider_strict_tools_mode;
            provider_config.fim_model = fim_model.to_owned();
            provider_config.request_timeout_secs = request_timeout_secs;

            let provider_value = serialize_deepseek_provider_value(&provider_config)?;
            root_config
                .providers
                .insert(DEEPSEEK_PROVIDER_KEY.to_owned(), provider_value);
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
}

#[derive(Debug, Clone)]
pub(crate) struct ConfigState {
    pub(crate) selected_section: ConfigSection,
    pub(crate) selected_field: Option<ConfigField>,
    pub(crate) footer_selected: bool,
    pub(crate) selected_footer_action: ConfigFooterAction,
    pub(crate) selected_mcp_server_index: usize,
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
            draft: ConfigDraft::from_root_config(root_config),
            dirty: false,
            close_guard_armed: false,
        }
    }

    pub(crate) fn set_section(&mut self, section: ConfigSection) {
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

    pub(crate) fn move_field(&mut self, forward: bool) -> ConfigFieldMove {
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

    pub(crate) fn focus_footer(&mut self, action: ConfigFooterAction) {
        self.footer_selected = true;
        self.selected_footer_action = action;
    }

    pub(crate) fn focus_field(&mut self, field: ConfigField) -> bool {
        if self.selected_section == ConfigSection::Mcp && self.draft.mcp_servers.is_empty() {
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
            | ConfigField::TerminalOsc52Clipboard => None,
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
            | ConfigField::TerminalOsc52Clipboard => None,
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

pub(crate) fn default_deepseek_provider_config(model: &str) -> DeepSeekProviderConfig {
    DeepSeekProviderConfig::default_for_model(model)
}

pub(crate) fn default_openai_compat_provider_config(model: &str) -> OpenAiCompatibleProviderConfig {
    OpenAiCompatibleProviderConfig::default_for_model(model)
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

pub(crate) fn normalize_provider_name(provider: &str) -> &'static str {
    match provider {
        "openai-compatible" | "openai_compatible" | "openai_compat" => OPENAI_COMPAT_PROVIDER_KEY,
        _ => DEEPSEEK_PROVIDER_KEY,
    }
}

pub(crate) fn cycle_provider_name(provider: &str) -> String {
    match normalize_provider_name(provider) {
        DEEPSEEK_PROVIDER_KEY => OPENAI_COMPAT_PROVIDER_KEY.to_owned(),
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
        | ConfigField::TerminalOsc52Clipboard => false,
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

#[cfg(test)]
#[path = "tests/config_panel_tests.rs"]
mod tests;

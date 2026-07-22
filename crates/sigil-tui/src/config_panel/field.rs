use super::ConfigSection;

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
    WebEnabled,
    WebNetworkMode,
    WebSearchRoute,
    WebBundledSearchEnabled,
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
    TerminalNotificationsEnabled,
    TerminalNotificationMethod,
    TerminalNotificationMinimumRunDurationMs,
    AppearanceInfoRail,
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
    // McpName is the read-only server selector in the default TUI. Remaining
    // MCP server editing stays in sigil.toml.
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
        Self::ProviderName,
        Self::ProviderModel,
        Self::ProviderApiKey,
        Self::ModelRequestTimeoutSecs,
        Self::ModelRequestStreamIdleTimeoutSecs,
    ];
    const STORAGE_FIELDS: [Self; 0] = [];
    const PERMISSION_FIELDS: [Self; 1] = [Self::PermissionMode];
    const WEB_FIELDS: [Self; 4] = [
        Self::WebEnabled,
        Self::WebNetworkMode,
        Self::WebSearchRoute,
        Self::WebBundledSearchEnabled,
    ];
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
    const TERMINAL_FIELDS: [Self; 3] = [
        Self::TerminalNotificationsEnabled,
        Self::TerminalNotificationMethod,
        Self::TerminalNotificationMinimumRunDurationMs,
    ];
    const APPEARANCE_FIELDS: [Self; 4] = [
        Self::AppearanceInfoRail,
        Self::AppearanceTheme,
        Self::AppearanceSyntaxTheme,
        Self::AppearanceUsageCostCurrency,
    ];
    const SKILL_FIELDS: [Self; 1] = [Self::SkillId];
    const PLUGIN_FIELDS: [Self; 1] = [Self::PluginId];
    const MCP_FIELDS: [Self; 1] = [Self::McpName];

    pub(crate) fn fields_for_section(section: ConfigSection) -> &'static [Self] {
        match section {
            ConfigSection::Provider => &Self::PROVIDER_FIELDS,
            ConfigSection::Storage => &Self::STORAGE_FIELDS,
            ConfigSection::Permissions => &Self::PERMISSION_FIELDS,
            ConfigSection::Web => &Self::WEB_FIELDS,
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
            Self::WebEnabled => "enabled",
            Self::WebNetworkMode => "network_mode",
            Self::WebSearchRoute => "search_route",
            Self::WebBundledSearchEnabled => "bundled_search",
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
            Self::TerminalNotificationsEnabled => "notifications_enabled",
            Self::TerminalNotificationMethod => "notification_method",
            Self::TerminalNotificationMinimumRunDurationMs => "minimum_run_duration_ms",
            Self::AppearanceInfoRail => "info_rail",
            Self::AppearanceTheme => "theme",
            Self::AppearanceSyntaxTheme => "syntax_theme",
            Self::AppearanceUsageCostCurrency => "usage_cost_currency",
            Self::AppearanceColorGroup => "color_group",
            Self::AppearanceColorToken => "color_token",
            Self::AppearanceColorOverride => "color_override",
            Self::SkillId => "skill",
            Self::PluginId => "plugin",
            Self::McpName => "server",
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
            Self::WebEnabled => "Web tools",
            Self::WebNetworkMode => "Network mode",
            Self::WebSearchRoute => "Search route",
            Self::WebBundledSearchEnabled => "Bundled Exa",
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
            Self::TerminalNotificationsEnabled => "Attention notifications",
            Self::TerminalNotificationMethod => "Notification method",
            Self::TerminalNotificationMinimumRunDurationMs => "Long-run threshold",
            Self::AppearanceInfoRail => "Info rail",
            Self::AppearanceTheme => "Theme",
            Self::AppearanceSyntaxTheme => "Syntax theme",
            Self::AppearanceUsageCostCurrency => "Cost currency",
            Self::AppearanceColorGroup => "Color group",
            Self::AppearanceColorToken => "Color token",
            Self::AppearanceColorOverride => "Override",
            Self::SkillId => "Skill",
            Self::PluginId => "Plugin",
            Self::McpName => "Server",
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
                "Local safety mode for Read/Write/Execute. Network effects use an independent policy that local danger mode cannot widen."
            }
            Self::WebEnabled => "Registers public webfetch/websearch product tools for new runs.",
            Self::WebNetworkMode => {
                "Independent network effect policy. Ask requires an explicit user action; Deny sends no Web egress."
            }
            Self::WebSearchRoute => {
                "Selects provider-hosted, configured MCP, bundled Exa, automatic resolution, or disabled search."
            }
            Self::WebBundledSearchEnabled => {
                "Allows the pinned anonymous Exa MCP profile only when no configured search binding is authoritative."
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
            Self::TerminalNotificationsEnabled => {
                "Emits privacy-bounded terminal attention signals for long runs and input-required states. Disabled by default."
            }
            Self::TerminalNotificationMethod => {
                "Selects automatic terminal detection, OSC 9, OSC 777, or an audible terminal bell."
            }
            Self::TerminalNotificationMinimumRunDurationMs => {
                "Completed runs notify only after this duration. Input-required and failure signals do not use this threshold."
            }
            Self::AppearanceInfoRail => {
                "Shows the right info rail by default when terminal width allows it. F2 can hide or show it for the current run."
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
            Self::McpName => {
                "Selected MCP server for lifecycle inspection. Press Enter to view the next server; this does not modify the config."
            }
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
                | Self::TerminalNotificationMinimumRunDurationMs
                | Self::AppearanceColorOverride
                | Self::McpCommand
                | Self::McpArgsCsv
                | Self::McpStartupTimeoutSecs
        )
    }

    pub(crate) fn action_label(self) -> &'static str {
        match self {
            Self::ProviderModel | Self::ProviderFimModel => "Enter choose",
            Self::ProviderName => "Enter switch",
            Self::ProviderApiKey => "Enter input",
            Self::PermissionMode
            | Self::WebNetworkMode
            | Self::WebSearchRoute
            | Self::VerificationAutoRun
            | Self::CodeIntelServerStartup
            | Self::TerminalNotificationMethod
            | Self::AppearanceTheme
            | Self::AppearanceSyntaxTheme => "Enter cycle",
            Self::AppearanceUsageCostCurrency => "Enter cycle",
            Self::AppearanceColorGroup | Self::AppearanceColorToken => "Enter cycle",
            Self::McpName => "Enter cycle",
            Self::MemoryEnabled
            | Self::WebEnabled
            | Self::WebBundledSearchEnabled
            | Self::CompactionEnabled
            | Self::CodeIntelEnabled
            | Self::CodeIntelAutoDiscover
            | Self::CodeIntelReportMissing
            | Self::TerminalMouseCapture
            | Self::TerminalOsc52Clipboard
            | Self::TerminalNotificationsEnabled
            | Self::AppearanceInfoRail => "Enter toggle",
            Self::TerminalScrollSensitivity | Self::TerminalNotificationMinimumRunDurationMs => {
                "Enter input"
            }
            Self::AppearanceColorOverride => "Enter input",
            Self::SkillId | Self::PluginId => "",
            _ if self.accepts_text_input() => "Enter input",
            _ => "",
        }
    }
}

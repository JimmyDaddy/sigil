use sigil_kernel::{PermissionMode, VerificationAutoRunPolicy, WebSearchRoute};
use sigil_runtime::{DEEPSEEK_PROVIDER_KEY, normalize_provider_name};

use super::{ConfigField, ConfigSection, ConfigState};

impl ConfigState {
    pub(crate) fn editing_field(&self) -> Option<ConfigField> {
        None
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
            | ConfigField::WebEnabled
            | ConfigField::WebNetworkMode
            | ConfigField::WebSearchRoute
            | ConfigField::WebBundledSearchEnabled
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
            | ConfigField::WebEnabled
            | ConfigField::WebNetworkMode
            | ConfigField::WebSearchRoute
            | ConfigField::WebBundledSearchEnabled
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
            ConfigField::McpName => {
                return self
                    .selected_mcp_server()
                    .map(|server| {
                        format!(
                            "{} ({}/{})",
                            server.name,
                            self.selected_mcp_server_index + 1,
                            self.draft.mcp_servers.len()
                        )
                    })
                    .unwrap_or_else(|| "none".to_owned());
            }
            ConfigField::PermissionMode => {
                return permission_mode_label(self.draft.permission_mode).to_owned();
            }
            ConfigField::WebEnabled => return bool_label(self.draft.web_enabled).to_owned(),
            ConfigField::WebNetworkMode => {
                return self.draft.web_network_mode.as_str().to_owned();
            }
            ConfigField::WebSearchRoute => {
                return web_search_route_label(self.draft.web_search_route).to_owned();
            }
            ConfigField::WebBundledSearchEnabled => {
                return bool_label(self.draft.web_bundled_search_enabled).to_owned();
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
        | ConfigField::McpCommand
        | ConfigField::McpArgsCsv => !character.is_control(),
        ConfigField::AppearanceColorOverride => character == '#' || character.is_ascii_hexdigit(),
        ConfigField::SkillId | ConfigField::PluginId => false,
        ConfigField::ProviderApiKey
        | ConfigField::ProviderName
        | ConfigField::PermissionMode
        | ConfigField::WebEnabled
        | ConfigField::WebNetworkMode
        | ConfigField::WebSearchRoute
        | ConfigField::WebBundledSearchEnabled
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
        | ConfigField::McpName => false,
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

fn web_search_route_label(route: WebSearchRoute) -> &'static str {
    match route {
        WebSearchRoute::Auto => "auto",
        WebSearchRoute::ProviderHosted => "provider-hosted",
        WebSearchRoute::Mcp => "configured MCP",
        WebSearchRoute::Bundled => "bundled Exa",
        WebSearchRoute::Disabled => "disabled",
    }
}

fn verification_auto_run_label(policy: VerificationAutoRunPolicy) -> &'static str {
    match policy {
        VerificationAutoRunPolicy::Manual => "manual",
        VerificationAutoRunPolicy::TrustedOnly => "auto trusted",
        VerificationAutoRunPolicy::Never => "off",
    }
}

pub(super) fn display_ratio(value: &str) -> String {
    match value.trim().parse::<f32>() {
        Ok(ratio) if ratio.is_finite() => format!("{}% ({})", (ratio * 100.0).round(), value),
        _ => value.to_owned(),
    }
}

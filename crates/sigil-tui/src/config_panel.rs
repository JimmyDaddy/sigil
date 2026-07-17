use std::collections::BTreeMap;

use sigil_kernel::{
    CodeIntelStartup, PermissionMode, PluginManifestSnapshot, RootConfig, SkillDescriptor,
    SyntaxThemeId, TerminalKeyboardEnhancement, TerminalNotificationMethod, ThemeId,
    UsageCostCurrency, VerificationAutoRunPolicy,
};
#[cfg(test)]
pub(crate) use sigil_runtime::{
    ANTHROPIC_PROVIDER_KEY, GEMINI_PROVIDER_KEY, OPENAI_COMPAT_PROVIDER_KEY,
};
pub(crate) use sigil_runtime::{DEEPSEEK_PROVIDER_KEY, normalize_provider_name};
use sigil_runtime::{ProviderStrictToolsMode, ResolvedAgentProfile};

mod appearance;
mod collection;
mod display;
mod draft;
mod field;
mod footer_action;
mod mcp_server;
mod provider;
mod section;
#[cfg(test)]
use display::display_ratio;
pub(crate) use display::{
    config_field_accepts_char, render_config_readonly_row, render_config_value_row,
};
pub(crate) use field::ConfigField;
pub(crate) use footer_action::ConfigFooterAction;
pub(crate) use mcp_server::McpServerDraft;
#[cfg(test)]
pub(crate) use mcp_server::McpTransportDraft;
use provider::ProviderFieldDraft;
#[cfg(test)]
pub(crate) use provider::cycle_provider_name;
#[cfg(test)]
use provider::default_provider_field_draft;
pub(crate) use section::ConfigSection;

pub(crate) const CONFIG_SECTION_NAV_HINT: &str = "Tab section";
pub(crate) const CONFIG_FIELD_NAV_HINT: &str = "Up/Down field";
pub(crate) const CONFIG_EDIT_OR_TOGGLE_HINT: &str = "Enter edit/toggle";
pub(crate) const CONFIG_SAVE_HINT: &str = "Ctrl-S save";
pub(crate) const CONFIG_HEADER_NOTICE: &str =
    "Tab section · Up/Down field · Enter edit · Ctrl-S save";
pub(crate) const CONFIG_CONTROLS_HINT: &str = "controls: Tab section · Up/Down field · Enter edit";
pub(crate) const CONFIG_ACTIONS_HINT: &str = "actions: Down to actions · Ctrl-S save · Esc close";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ConfigFieldMove {
    Moved,
    Boundary,
    Unavailable,
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
    pub(crate) web_enabled: bool,
    pub(crate) web_network_mode: sigil_kernel::NetworkPolicy,
    pub(crate) web_search_route: sigil_kernel::WebSearchRoute,
    pub(crate) web_bundled_search_enabled: bool,
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
    pub(crate) terminal_notifications_enabled: bool,
    pub(crate) terminal_notification_method: TerminalNotificationMethod,
    pub(crate) terminal_notification_minimum_run_duration_ms: String,
    pub(crate) appearance_theme: ThemeId,
    pub(crate) appearance_syntax_theme: SyntaxThemeId,
    pub(crate) appearance_usage_cost_currency: UsageCostCurrency,
    pub(crate) appearance_info_rail: bool,
    pub(crate) appearance_color_group_index: usize,
    pub(crate) appearance_color_token_index: usize,
    pub(crate) mcp_servers: Vec<McpServerDraft>,
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
}

#[cfg(all(test, not(sigil_tui_test_slice_app_input_flow)))]
#[path = "tests/config_panel_tests.rs"]
mod tests;

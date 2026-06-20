use crate::config_panel::{
    ANTHROPIC_PROVIDER_KEY, CONFIG_ACTIONS_HINT, CONFIG_CONTROLS_HINT, CONFIG_EDIT_OR_TOGGLE_HINT,
    CONFIG_FIELD_NAV_HINT, CONFIG_SAVE_HINT, CONFIG_SECTION_NAV_HINT, ConfigDraft, ConfigField,
    ConfigFieldMove, ConfigFooterAction, ConfigSection, ConfigState, GEMINI_PROVIDER_KEY,
    OPENAI_COMPAT_PROVIDER_KEY, config_field_accepts_char, render_config_readonly_row,
    render_config_value_row,
};
use crate::slash::SLASH_COMMANDS;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::Result;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use sigil_kernel::{
    AgentProfileCapturedEntry, AgentProfileId, AgentProfileKind, AgentProfilePolicyEntry,
    AgentProfileSnapshot, AgentProfileSource, AgentProfileTrustEntry, AgentTrustState,
    ApprovalMode, CodeIntelStartup, ControlEntry, JsonlSessionStore, McpServerConfig,
    McpServerStartup, PluginCapability, PluginManifestSnapshot, PluginStateProjection,
    PluginTrustDecision, PluginTrustEntry, RootConfig, SessionLogEntry, SkillDescriptor,
    SkillRunMode, SkillSource, SkillTrustState, ToolRegistryScope, default_user_config_dir,
};
use sigil_provider_anthropic::SIGIL_ANTHROPIC_API_KEY_ENV;
use sigil_provider_deepseek::SIGIL_API_KEY_ENV;
use sigil_provider_gemini::SIGIL_GEMINI_API_KEY_ENV;
use sigil_provider_openai_compat::OPENAI_COMPATIBLE_API_KEY_ENV;
use sigil_runtime::{
    AgentProfileRegistry, ResolvedAgentProfile,
    doctor::{DoctorCheck, DoctorStatus, build_code_intelligence_checks},
    provider_capabilities_for_name, provider_capability_view,
};

use super::{
    AppAction, AppState, McpServerRuntimeStatus, code_intelligence_config_status,
    formatting::{format_token_count, persisted_root_config},
    initial_mcp_server_status, initial_mcp_server_statuses,
    modal_flow::{
        ModalOutcome, ModalState, ModelPickerTarget, SecretInputTarget, TextInputState,
        TextInputTarget,
    },
};
use crate::context_window::{ContextWindowSource, resolve_context_window_tokens};

impl AppState {
    pub fn config_section_title(&self) -> Option<&'static str> {
        self.config_state
            .as_ref()
            .map(|state| state.selected_section.title())
    }

    pub(crate) fn config_selected_section(&self) -> Option<ConfigSection> {
        self.config_state
            .as_ref()
            .map(|state| state.selected_section)
    }

    pub fn config_selected_field_label(&self) -> Option<&'static str> {
        self.config_state.as_ref().and_then(|state| {
            if state.footer_selected {
                Some(state.selected_footer_action.field_label())
            } else {
                state
                    .selected_field
                    .map(|field| config_field_display_label(state, field))
            }
        })
    }

    pub fn config_status_summary(&self) -> String {
        let section = self.config_section_title().unwrap_or("Config");
        let saved = if self.config_is_dirty() {
            "unsaved"
        } else {
            "saved"
        };
        let config_label = self
            .config_path
            .file_name()
            .and_then(|value| value.to_str())
            .unwrap_or("config");
        format!("{section} · {saved} · {config_label}")
    }

    pub fn config_selected_footer_action_label(&self) -> Option<&'static str> {
        self.config_state.as_ref().and_then(|state| {
            state
                .footer_selected
                .then_some(state.selected_footer_action.button_label())
        })
    }

    pub fn config_footer_action_labels(&self) -> Vec<&'static str> {
        let section = self
            .config_state
            .as_ref()
            .map(|state| state.selected_section)
            .unwrap_or(ConfigSection::Provider);
        ConfigFooterAction::actions_for_section(section)
            .iter()
            .map(|action| action.button_label())
            .collect()
    }

    pub fn config_footer_hint(&self) -> String {
        if self.config_close_guard_armed() {
            "status: confirm close - Esc discards".to_owned()
        } else if self.config_is_dirty() {
            "status: unsaved - save before close".to_owned()
        } else {
            "status: saved".to_owned()
        }
    }

    pub fn config_close_guard_armed(&self) -> bool {
        self.config_state
            .as_ref()
            .map(|state| state.close_guard_armed)
            .unwrap_or(false)
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
        lines.push(CONFIG_SECTION_NAV_HINT.to_owned());
        lines.push(CONFIG_FIELD_NAV_HINT.to_owned());
        lines.push(CONFIG_EDIT_OR_TOGGLE_HINT.to_owned());
        lines.push(format!("{CONFIG_SAVE_HINT}  Esc close"));
        if state.selected_section == ConfigSection::Mcp {
            lines.push("MCP: Ctrl-N add".to_owned());
            lines.push("MCP: Ctrl-D drop".to_owned());
            lines.push("MCP: PgUp/PgDn switch".to_owned());
            lines.push("MCP: footer activate lazy".to_owned());
        } else if state.selected_section == ConfigSection::Agents {
            lines.push("Agents: Up/Down select".to_owned());
            lines.push("Agents: PgUp/PgDn wrap".to_owned());
            lines.push("Agents: footer trust/policy".to_owned());
        } else if state.selected_section == ConfigSection::Skills {
            lines.push("Skills: Up/Down select".to_owned());
            lines.push("Skills: PgUp/PgDn wrap".to_owned());
            lines.push("Skills: footer load/invoke".to_owned());
        } else if state.selected_section == ConfigSection::Plugins {
            lines.push("Plugins: Up/Down select".to_owned());
            lines.push("Plugins: PgUp/PgDn wrap".to_owned());
            lines.push("Plugins: footer approve/deny".to_owned());
        }
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
                "{} {}/{} · {}",
                section.title(),
                index + 1,
                ConfigSection::FLOW.len(),
                section.summary()
            ),
            None => section.title().to_owned(),
        }];
        lines.push(step_label);
        lines.push(String::new());

        match section {
            ConfigSection::Provider => {
                lines.push("[runtime]".to_owned());
                lines.push(render_config_value_row(
                    config_state,
                    ConfigField::ProviderName,
                ));
                lines.push(String::new());
                lines.push("[model]".to_owned());
                lines.push(render_config_value_row(
                    config_state,
                    ConfigField::ProviderModel,
                ));
                lines.push(String::new());
                lines.push("[authentication]".to_owned());
                lines.push(render_config_value_row(
                    config_state,
                    ConfigField::ProviderApiKey,
                ));
                lines.push(String::new());
                lines.push("[endpoint]".to_owned());
                lines.push(render_config_value_row(
                    config_state,
                    ConfigField::ProviderBaseUrl,
                ));
                lines.push(String::new());
                lines.push("[advanced]".to_owned());
                lines.push(render_config_value_row(
                    config_state,
                    ConfigField::ProviderFimModel,
                ));
                lines.extend(render_config_selection_details(config_state));
                lines.push(String::new());
                lines.push("[capabilities]".to_owned());
                lines.extend(render_provider_capability_summary(config_state));
            }
            ConfigSection::Permissions => {
                lines.push("[policy]".to_owned());
                lines.push(render_config_value_row(
                    config_state,
                    ConfigField::PermissionsDefaultMode,
                ));
                lines.push(String::new());
                lines.push("[rules]".to_owned());
                lines.extend(render_permission_rule_summary(config_state));
                lines.extend(render_config_selection_details(config_state));
            }
            ConfigSection::Memory => {
                lines.push("[workspace memory]".to_owned());
                lines.push(render_config_value_row(
                    config_state,
                    ConfigField::MemoryEnabled,
                ));
                lines.push(String::new());
                lines.push("[loaded context]".to_owned());
                lines.push(render_config_readonly_row(
                    "Documents",
                    &format!("{} loaded", self.memory_document_count),
                ));
                lines.push(render_config_readonly_row(
                    "Last scan",
                    &self.memory_last_status,
                ));
                lines.push(render_config_readonly_row(
                    "Root files",
                    "SIGIL.md, AGENTS.md, CLAUDE.md, local override",
                ));
                lines.extend(render_config_selection_details(config_state));
            }
            ConfigSection::Compaction => {
                lines.push("[context]".to_owned());
                lines.push(render_config_value_row(
                    config_state,
                    ConfigField::CompactionEnabled,
                ));
                lines.push(render_config_readonly_row(
                    "Effective window",
                    &render_effective_context_window(config_state),
                ));
                lines.push(render_config_value_row(
                    config_state,
                    ConfigField::CompactionContextWindowTokens,
                ));
                lines.push(String::new());
                lines.push("[thresholds]".to_owned());
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
                lines.extend(render_config_selection_details(config_state));
            }
            ConfigSection::CodeIntelligence => {
                lines.push("[controls]".to_owned());
                lines.push(render_config_value_row(
                    config_state,
                    ConfigField::CodeIntelEnabled,
                ));
                lines.push(render_config_value_row(
                    config_state,
                    ConfigField::CodeIntelStartup,
                ));
                lines.push(render_config_value_row(
                    config_state,
                    ConfigField::CodeIntelDiscoveryEnabled,
                ));
                lines.push(render_config_value_row(
                    config_state,
                    ConfigField::CodeIntelDiscoveryReportMissing,
                ));
                lines.push(String::new());
                lines.push("[trust]".to_owned());
                lines.extend(render_code_intelligence_trust_summary());
                lines.push(String::new());
                lines.push("[readiness]".to_owned());
                lines.extend(self.render_code_intelligence_readiness_summary(config_state));
                lines.extend(render_config_selection_details(config_state));
            }
            ConfigSection::Terminal => {
                lines.push("[interaction]".to_owned());
                lines.push(render_config_value_row(
                    config_state,
                    ConfigField::TerminalMouseCapture,
                ));
                lines.push(render_config_value_row(
                    config_state,
                    ConfigField::TerminalOsc52Clipboard,
                ));
                lines.push(render_config_value_row(
                    config_state,
                    ConfigField::TerminalScrollSensitivity,
                ));
                lines.push(String::new());
                lines.push("[compatibility]".to_owned());
                lines.push(render_config_hint_row(
                    "Turn mouse_capture off when your terminal or multiplexer mishandles mouse mode",
                ));
                lines.push(render_config_hint_row(
                    "Turn osc52_clipboard off when clipboard writes are blocked or noisy",
                ));
                lines.extend(render_config_selection_details(config_state));
            }
            ConfigSection::Agents => {
                let (_skill_count, skill_agent_count) = skill_config_counts(config_state);
                let agent_count = config_state.agent_profiles.len();
                lines.push("[discovery]".to_owned());
                lines.push(render_config_readonly_row(
                    "Enabled",
                    bool_summary(config_state.draft.base_root_config.skills.enabled),
                ));
                lines.push(render_config_readonly_row(
                    "Configured",
                    &format!("{} {}", agent_count, pluralize("agent", agent_count)),
                ));
                lines.push(render_config_readonly_row(
                    "Compatibility",
                    &format!(
                        "{} {}",
                        skill_agent_count,
                        pluralize("agent", skill_agent_count)
                    ),
                ));
                lines.push(render_config_readonly_row(
                    "Warnings",
                    &format!("{} warnings", config_state.agent_warnings.len()),
                ));
                if agent_count == 0 {
                    lines.push(render_config_hint_row("No agents discovered"));
                    lines.push(render_config_hint_row(
                        "Agents are discovered from built-ins, workspace profiles, plugins, and compatibility sources",
                    ));
                } else {
                    lines.push(render_config_readonly_row(
                        "Selected",
                        &selected_agent_summary(config_state),
                    ));
                    lines.push(String::new());
                    lines.push("[agents]".to_owned());
                    lines.extend(render_agent_index_lines(config_state));
                    if let Some(agent) = config_state.selected_agent() {
                        lines.push(String::new());
                        lines.push("[agent]".to_owned());
                        lines.push(render_config_readonly_row(
                            "Agent",
                            agent.profile.id.as_str(),
                        ));
                        lines.extend(render_agent_detail_lines(agent));
                    }
                }
                if !config_state.agent_warnings.is_empty() {
                    lines.push(String::new());
                    lines.push("[warnings]".to_owned());
                    for warning in config_state.agent_warnings.iter().take(4) {
                        lines.push(render_config_hint_row(warning));
                    }
                    if config_state.agent_warnings.len() > 4 {
                        lines.push(format!(
                            "... {} more warnings",
                            config_state.agent_warnings.len() - 4
                        ));
                    }
                }
                lines.push(String::new());
                lines.push("Up/Down agent  PgUp/PgDn wrap  footer trust/policy".to_owned());
                lines.extend(render_config_selection_details(config_state));
            }
            ConfigSection::Skills => {
                let (skill_count, agent_count) = skill_config_counts(config_state);
                lines.push("[discovery]".to_owned());
                lines.push(render_config_readonly_row(
                    "Enabled",
                    bool_summary(config_state.draft.base_root_config.skills.enabled),
                ));
                lines.push(render_config_readonly_row(
                    "Configured",
                    &format!("{} {}", skill_count, pluralize("skill", skill_count)),
                ));
                lines.push(render_config_readonly_row(
                    "Agents",
                    &format!("{} {}", agent_count, pluralize("agent", agent_count)),
                ));
                lines.push(render_config_readonly_row(
                    "Warnings",
                    &format!("{} warnings", config_state.skill_warnings.len()),
                ));
                if skill_count == 0 {
                    lines.push(render_config_hint_row("No skills discovered"));
                    lines.push(render_config_hint_row(
                        "Reusable inline skills are discovered from configured skills directories",
                    ));
                } else {
                    lines.push(render_config_readonly_row(
                        "Selected",
                        &selected_skill_summary(config_state),
                    ));
                    lines.push(String::new());
                    lines.push("[skills]".to_owned());
                    lines.extend(render_skill_index_lines(config_state, false));
                    if let Some(skill) = config_state.selected_skill() {
                        lines.push(String::new());
                        lines.push("[skill]".to_owned());
                        lines.push(render_config_readonly_row("Skill", &skill.id));
                        lines.extend(render_skill_detail_lines(skill));
                    }
                }
                if !config_state.skill_warnings.is_empty() {
                    lines.push(String::new());
                    lines.push("[warnings]".to_owned());
                    for warning in config_state.skill_warnings.iter().take(4) {
                        lines.push(render_config_hint_row(warning));
                    }
                    if config_state.skill_warnings.len() > 4 {
                        lines.push(format!(
                            "... {} more warnings",
                            config_state.skill_warnings.len() - 4
                        ));
                    }
                }
                lines.push(String::new());
                lines.push("Up/Down skill  PgUp/PgDn wrap  footer load/invoke".to_owned());
                lines.extend(render_config_selection_details(config_state));
            }
            ConfigSection::Plugins => {
                lines.push("[discovery]".to_owned());
                lines.push(render_config_readonly_row(
                    "Configured",
                    &format!("{} plugins", config_state.plugin_manifests.len()),
                ));
                lines.push(render_config_readonly_row(
                    "Warnings",
                    &format!("{} warnings", config_state.plugin_warnings.len()),
                ));
                if config_state.plugin_manifests.is_empty() {
                    lines.push(render_config_hint_row("No plugin manifests discovered"));
                    lines.push(render_config_hint_row(
                        "Workspace plugins live under .sigil/plugins/<id>/plugin.toml",
                    ));
                } else {
                    lines.push(render_config_readonly_row(
                        "Selected",
                        &format!(
                            "{} of {}",
                            config_state.selected_plugin_index + 1,
                            config_state.plugin_manifests.len()
                        ),
                    ));
                    lines.push(String::new());
                    lines.push("[plugins]".to_owned());
                    lines.extend(render_plugin_index_lines(config_state));
                    if let Some(plugin) = config_state.selected_plugin() {
                        lines.push(String::new());
                        lines.push("[plugin]".to_owned());
                        lines.push(render_config_readonly_row("Plugin", &plugin.plugin_id));
                        lines.extend(render_plugin_detail_lines(plugin));
                    }
                }
                if !config_state.plugin_warnings.is_empty() {
                    lines.push(String::new());
                    lines.push("[warnings]".to_owned());
                    for warning in config_state.plugin_warnings.iter().take(4) {
                        lines.push(render_config_hint_row(warning));
                    }
                    if config_state.plugin_warnings.len() > 4 {
                        lines.push(format!(
                            "... {} more warnings",
                            config_state.plugin_warnings.len() - 4
                        ));
                    }
                }
                lines.push(String::new());
                lines.push("Up/Down plugin  PgUp/PgDn wrap  footer approve/deny".to_owned());
                lines.extend(render_config_selection_details(config_state));
            }
            ConfigSection::Mcp => {
                lines.push("[servers]".to_owned());
                lines.push(render_config_readonly_row(
                    "Configured",
                    &format!("{} servers", config_state.draft.mcp_servers.len()),
                ));
                if config_state.draft.mcp_servers.is_empty() {
                    lines.push(render_config_hint_row("No MCP servers configured"));
                    lines.push(render_config_hint_row(
                        "Ctrl-N adds a required eager self-hosted server",
                    ));
                } else {
                    lines.push(render_config_readonly_row(
                        "Selected",
                        &format!(
                            "{} of {}",
                            config_state.selected_mcp_server_index + 1,
                            config_state.draft.mcp_servers.len()
                        ),
                    ));
                    if config_state.selected_mcp_server().is_some() {
                        lines.push(String::new());
                        lines.push("[server]".to_owned());
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
                        lines.push(String::new());
                        lines.push("[lifecycle]".to_owned());
                        lines.extend(render_mcp_lifecycle_summary(
                            config_state,
                            &self.selected_mcp_runtime_status_label(config_state),
                        ));
                    }
                }
                lines.push(String::new());
                lines.push("Ctrl-N add  Ctrl-D drop  PgUp/PgDn server  footer activate".to_owned());
                lines.extend(render_config_selection_details(config_state));
            }
        }

        lines
    }

    pub(super) fn handle_config_key_event(&mut self, key: KeyEvent) -> Result<Option<AppAction>> {
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
                if let Some(action) = self.apply_config_modal_outcome(outcome)? {
                    return Ok(Some(action));
                }
                return self.save_config_draft();
            }
            if key.code == KeyCode::F(3) {
                let outcome = self.submit_modal();
                if let Some(action) = self.apply_config_modal_outcome(outcome)? {
                    return Ok(Some(action));
                }
                return self.save_config_draft_and_close();
            }
            let outcome = self.handle_modal_key_event(key);
            return self.apply_config_modal_outcome(outcome);
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
                    if config_state.footer_selected
                        && config_state.selected_section == ConfigSection::Mcp
                        && config_state.selected_field.is_none()
                        && config_state.selected_footer_action == ConfigFooterAction::Save
                    {
                        config_state.set_section(config_state.selected_section.previous_flow());
                        self.last_notice = Some(format!(
                            "step {}",
                            config_state.selected_section.title().to_lowercase()
                        ));
                    } else if config_state.footer_selected {
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
                    if config_state.footer_selected
                        && config_state.selected_section == ConfigSection::Mcp
                        && config_state.selected_field.is_none()
                        && config_state.selected_footer_action == ConfigFooterAction::Close
                    {
                        config_state.set_section(config_state.selected_section.next_flow());
                        self.last_notice = Some(format!(
                            "step {}",
                            config_state.selected_section.title().to_lowercase()
                        ));
                    } else if config_state.footer_selected {
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
                if let Some(config_state) = self.config_state.as_mut() {
                    match config_state.selected_section {
                        ConfigSection::Mcp => {
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
                        ConfigSection::Agents => {
                            if config_state.cycle_agent(false) {
                                self.last_notice = Some(selected_agent_summary(config_state));
                            } else {
                                self.last_notice = Some("no agent to select".to_owned());
                            }
                        }
                        ConfigSection::Skills => {
                            if config_state.cycle_skill(false) {
                                self.last_notice = Some(selected_skill_summary(config_state));
                            } else {
                                self.last_notice = Some(format!(
                                    "no {} to select",
                                    skill_section_noun(config_state.selected_section)
                                ));
                            }
                        }
                        ConfigSection::Plugins => {
                            if config_state.cycle_plugin(false) {
                                self.last_notice = Some(format!(
                                    "plugin {}/{}",
                                    config_state.selected_plugin_index + 1,
                                    config_state.plugin_manifests.len()
                                ));
                            } else {
                                self.last_notice = Some("no plugin to select".to_owned());
                            }
                        }
                        _ => {}
                    }
                }
            }
            KeyCode::PageDown => {
                if let Some(config_state) = self.config_state.as_mut() {
                    match config_state.selected_section {
                        ConfigSection::Mcp => {
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
                        ConfigSection::Agents => {
                            if config_state.cycle_agent(true) {
                                self.last_notice = Some(selected_agent_summary(config_state));
                            } else {
                                self.last_notice = Some("no agent to select".to_owned());
                            }
                        }
                        ConfigSection::Skills => {
                            if config_state.cycle_skill(true) {
                                self.last_notice = Some(selected_skill_summary(config_state));
                            } else {
                                self.last_notice = Some(format!(
                                    "no {} to select",
                                    skill_section_noun(config_state.selected_section)
                                ));
                            }
                        }
                        ConfigSection::Plugins => {
                            if config_state.cycle_plugin(true) {
                                self.last_notice = Some(format!(
                                    "plugin {}/{}",
                                    config_state.selected_plugin_index + 1,
                                    config_state.plugin_manifests.len()
                                ));
                            } else {
                                self.last_notice = Some("no plugin to select".to_owned());
                            }
                        }
                        _ => {}
                    }
                }
            }
            KeyCode::Up => {
                if let Some(config_state) = self.config_state.as_mut() {
                    if config_state.footer_selected {
                        if config_state.focus_last_field()
                            && let Some(field) = config_state.selected_field
                        {
                            self.last_notice = config_collection_selection_notice(config_state)
                                .or_else(|| Some(format!("config field {}", field.label())));
                        } else {
                            config_state.footer_selected = false;
                            self.last_notice = Some(format!(
                                "step {}",
                                config_state.selected_section.title().to_lowercase()
                            ));
                        }
                    } else {
                        match move_config_collection_selection(config_state, false) {
                            Some(ConfigFieldMove::Moved) => {
                                self.last_notice = config_collection_selection_notice(config_state);
                            }
                            Some(ConfigFieldMove::Boundary | ConfigFieldMove::Unavailable) => {}
                            None => {
                                if let ConfigFieldMove::Moved = config_state.move_field(false)
                                    && let Some(field) = config_state.selected_field
                                {
                                    self.last_notice =
                                        Some(format!("config field {}", field.label()));
                                }
                            }
                        }
                    }
                }
            }
            KeyCode::Down => {
                if let Some(config_state) = self.config_state.as_mut() {
                    if config_state.footer_selected {
                        return Ok(None);
                    }
                    match move_config_collection_selection(config_state, true) {
                        Some(ConfigFieldMove::Moved) => {
                            self.last_notice = config_collection_selection_notice(config_state);
                        }
                        Some(ConfigFieldMove::Boundary | ConfigFieldMove::Unavailable) => {
                            let action = focus_first_config_footer_action(config_state);
                            self.last_notice = Some(format!("action {}", action.field_label()));
                        }
                        None => match config_state.move_field(true) {
                            ConfigFieldMove::Moved => {
                                if let Some(field) = config_state.selected_field {
                                    self.last_notice =
                                        Some(format!("config field {}", field.label()));
                                }
                            }
                            ConfigFieldMove::Boundary | ConfigFieldMove::Unavailable => {
                                let action = focus_first_config_footer_action(config_state);
                                self.last_notice = Some(format!("action {}", action.field_label()));
                            }
                        },
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
                        ConfigFooterAction::ActivateMcp => self.activate_selected_mcp_server(),
                        ConfigFooterAction::TrustAgent => {
                            self.review_selected_agent(AgentTrustState::Trusted)
                        }
                        ConfigFooterAction::BlockAgent => {
                            self.review_selected_agent(AgentTrustState::Disabled)
                        }
                        ConfigFooterAction::ToggleAgentEnabled => {
                            self.toggle_selected_agent_enabled()
                        }
                        ConfigFooterAction::ToggleAgentUser => self.toggle_selected_agent_user(),
                        ConfigFooterAction::ToggleAgentModel => self.toggle_selected_agent_model(),
                        ConfigFooterAction::LoadSkill => self.load_selected_skill(),
                        ConfigFooterAction::InvokeSkill => self.open_selected_skill_arguments(),
                        ConfigFooterAction::ApprovePlugin => {
                            self.review_selected_plugin(PluginTrustDecision::Trusted)
                        }
                        ConfigFooterAction::DenyPlugin => {
                            self.review_selected_plugin(PluginTrustDecision::Disabled)
                        }
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
                        ConfigField::ProviderName => {
                            config_state.draft.cycle_provider();
                            config_state.dirty = true;
                            self.last_notice =
                                Some(format!("provider -> {}", config_state.draft.provider_name));
                            return Ok(None);
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
                        ConfigField::PermissionsDefaultMode => {
                            config_state.draft.permission_default_mode =
                                cycle_approval_mode(config_state.draft.permission_default_mode);
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
                        ConfigField::CodeIntelEnabled => {
                            config_state.draft.code_intelligence_enabled =
                                !config_state.draft.code_intelligence_enabled;
                            config_state.dirty = true;
                            self.last_notice = Some(format!("updated {}", field.label()));
                            return Ok(None);
                        }
                        ConfigField::CodeIntelStartup => {
                            config_state.draft.code_intelligence_startup = cycle_code_intel_startup(
                                config_state.draft.code_intelligence_startup,
                            );
                            config_state.dirty = true;
                            self.last_notice = Some(format!("updated {}", field.label()));
                            return Ok(None);
                        }
                        ConfigField::CodeIntelDiscoveryEnabled => {
                            config_state.draft.code_intelligence_discovery_enabled =
                                !config_state.draft.code_intelligence_discovery_enabled;
                            config_state.dirty = true;
                            self.last_notice = Some(format!("updated {}", field.label()));
                            return Ok(None);
                        }
                        ConfigField::CodeIntelDiscoveryReportMissing => {
                            let report_missing = &mut config_state
                                .draft
                                .code_intelligence_discovery_report_missing;
                            *report_missing = !*report_missing;
                            config_state.dirty = true;
                            self.last_notice = Some(format!("updated {}", field.label()));
                            return Ok(None);
                        }
                        ConfigField::TerminalMouseCapture => {
                            config_state.draft.terminal_mouse_capture =
                                !config_state.draft.terminal_mouse_capture;
                            config_state.dirty = true;
                            self.last_notice = Some(format!("updated {}", field.label()));
                            return Ok(None);
                        }
                        ConfigField::TerminalOsc52Clipboard => {
                            config_state.draft.terminal_osc52_clipboard =
                                !config_state.draft.terminal_osc52_clipboard;
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

    pub(super) fn handle_config_paste_text(&mut self, text: &str) {
        let Some(config_state) = self.config_state.as_mut() else {
            return;
        };
        if config_state.footer_selected {
            return;
        }
        let Some(field) = config_state.selected_field else {
            return;
        };

        let value = if field == ConfigField::ProviderApiKey {
            text.chars()
                .filter(|character| !character.is_control())
                .collect::<String>()
        } else if field.accepts_text_input() {
            text.chars()
                .filter(|character| {
                    !character.is_control() && config_field_accepts_char(field, *character)
                })
                .collect::<String>()
        } else {
            return;
        };
        if value.is_empty() {
            return;
        }
        let Some(target) = config_state.field_text_value_mut(field) else {
            return;
        };
        let changed = *target != value;
        *target = value;
        if changed {
            config_state.dirty = true;
        }
        self.last_notice = Some(format!("updated {}", field.label()));
    }

    pub(super) fn open_config_panel(&mut self) {
        let Some(root_config) = self.config_snapshot.as_ref() else {
            self.last_notice = Some("config is unavailable in setup mode".to_owned());
            return;
        };

        let mut config_state = ConfigState::from_root_config(root_config);
        let (agents, agent_warnings) = self.discover_config_agents(root_config);
        config_state.set_agent_discovery(agents, agent_warnings);
        let (skills, warnings) = self.discover_config_skills(root_config);
        config_state.set_skill_discovery(skills, warnings);
        let (plugins, plugin_warnings) = self.discover_config_plugins();
        config_state.set_plugin_discovery(plugins, plugin_warnings);
        self.config_state = Some(config_state);
        self.last_notice = Some("opened config".to_owned());
        self.push_event("mode", "config");
    }

    fn apply_config_modal_outcome(&mut self, outcome: ModalOutcome) -> Result<Option<AppAction>> {
        match outcome {
            ModalOutcome::TextSubmitted {
                target: TextInputTarget::SkillArguments,
                value,
            } => self.submit_selected_skill_invocation(value),
            other => {
                self.apply_modal_outcome(other);
                Ok(None)
            }
        }
    }

    fn discover_config_skills(
        &self,
        root_config: &RootConfig,
    ) -> (Vec<SkillDescriptor>, Vec<String>) {
        let user_config_dir = default_user_config_dir().ok();
        match sigil_runtime::discover_skill_index_with_user_dir(
            &self.workspace_root,
            user_config_dir.as_deref(),
            &root_config.skills,
        ) {
            Ok(report) => {
                let warnings = report
                    .warnings
                    .into_iter()
                    .map(|warning| format!("{}: {}", warning.path.display(), warning.message))
                    .collect();
                (report.snapshot.descriptors, warnings)
            }
            Err(error) => (Vec::new(), vec![format!("skill discovery failed: {error}")]),
        }
    }

    fn discover_config_agents(
        &self,
        root_config: &RootConfig,
    ) -> (Vec<ResolvedAgentProfile>, Vec<String>) {
        match AgentProfileRegistry::from_root_config_with_workspace_and_entries(
            root_config,
            &self.workspace_root,
            &self.current_session_entries,
        ) {
            Ok(registry) => (registry.profiles().to_vec(), registry.warnings().to_vec()),
            Err(error) => (Vec::new(), vec![format!("agent discovery failed: {error}")]),
        }
    }

    fn discover_config_plugins(&self) -> (Vec<PluginManifestSnapshot>, Vec<String>) {
        let projection = PluginStateProjection::from_entries(&self.current_session_entries);
        let trust_entries = projection
            .trust_entries
            .into_values()
            .collect::<Vec<PluginTrustEntry>>();
        match sigil_runtime::discover_workspace_plugins(&self.workspace_root, &trust_entries) {
            Ok(report) => {
                let warnings = report
                    .warnings
                    .into_iter()
                    .map(|warning| format!("{}: {}", warning.path.display(), warning.message))
                    .collect();
                (report.manifests, warnings)
            }
            Err(error) => (
                Vec::new(),
                vec![format!("plugin discovery failed: {error}")],
            ),
        }
    }

    fn load_selected_skill(&mut self) -> Result<Option<AppAction>> {
        if self.is_busy {
            self.last_notice = Some("busy; load skill later".to_owned());
            return Ok(None);
        }
        let Some(skill) = self.selected_config_skill() else {
            return Ok(None);
        };
        let item_kind = skill_display_noun(&skill);
        if let Some(reason) = skill_load_unavailable_reason(&skill) {
            self.last_notice = Some(format!("{item_kind} {} {reason}", skill.id));
            return Ok(None);
        }

        let prompt = skill_load_prompt(&skill);
        let skill_id = skill.id;
        self.config_state = None;
        self.last_notice = Some(format!("loading {item_kind} {skill_id}"));
        self.push_event("skill", format!("load {skill_id}"));
        Ok(Some(AppAction::SubmitPrompt(prompt)))
    }

    fn open_selected_skill_arguments(&mut self) -> Result<Option<AppAction>> {
        if self.is_busy {
            self.last_notice = Some("busy; invoke skill later".to_owned());
            return Ok(None);
        }
        let Some(skill) = self.selected_config_skill() else {
            return Ok(None);
        };
        let item_kind = skill_display_noun(&skill);
        if let Some(reason) = skill_invoke_unavailable_reason(&skill) {
            self.last_notice = Some(format!("{item_kind} {} {reason}", skill.id));
            return Ok(None);
        }
        self.open_text_input(
            TextInputTarget::SkillArguments,
            skill.argument_hint.as_deref().unwrap_or_default(),
        );
        Ok(None)
    }

    fn submit_selected_skill_invocation(&mut self, arguments: String) -> Result<Option<AppAction>> {
        if self.is_busy {
            self.last_notice = Some("busy; invoke skill later".to_owned());
            return Ok(None);
        }
        let Some(skill) = self.selected_config_skill() else {
            return Ok(None);
        };
        let item_kind = skill_display_noun(&skill);
        if let Some(reason) = skill_invoke_unavailable_reason(&skill) {
            self.last_notice = Some(format!("{item_kind} {} {reason}", skill.id));
            return Ok(None);
        }

        let prompt = skill_invoke_prompt(&skill, &arguments);
        let skill_id = skill.id;
        self.config_state = None;
        self.last_notice = Some(format!("invoking {item_kind} {skill_id}"));
        self.push_event("skill", format!("invoke {skill_id}"));
        Ok(Some(AppAction::SubmitPrompt(prompt)))
    }

    fn review_selected_agent(&mut self, decision: AgentTrustState) -> Result<Option<AppAction>> {
        if self.is_busy {
            self.last_notice = Some("busy; review agent later".to_owned());
            return Ok(None);
        }
        let Some(cached) = self.selected_config_agent() else {
            return Ok(None);
        };
        let Some((agent, snapshot)) = self.refresh_selected_agent_for_review(&cached) else {
            return Ok(None);
        };
        if agent.source == AgentProfileSource::System {
            self.last_notice = Some(format!(
                "agent {} is system-managed",
                agent.profile.id.as_str()
            ));
            return Ok(None);
        }

        let trust = AgentProfileTrustEntry {
            profile_id: agent.profile.id.clone(),
            source: snapshot.source.clone(),
            source_hash: snapshot.source_hash.clone(),
            profile_hash: snapshot.profile_hash.clone(),
            decision,
            reviewed_at_ms: unix_time_ms(),
        };

        self.ensure_current_session_identity()?;
        self.append_agent_profile_snapshot(snapshot)?;
        self.append_control_to_current_session(ControlEntry::AgentProfileTrustDecision(trust))?;
        self.refresh_config_agents_for_profile(&agent.profile.id);

        let action = match decision {
            AgentTrustState::Trusted => "trusted",
            AgentTrustState::Disabled => "blocked",
            AgentTrustState::NeedsReview => "reviewed",
            AgentTrustState::Unknown => "reviewed",
        };
        self.last_notice = Some(format!("agent {} {action}", agent.profile.id.as_str()));
        self.push_event("agent", format!("{} {action}", agent.profile.id.as_str()));
        Ok(None)
    }

    fn toggle_selected_agent_enabled(&mut self) -> Result<Option<AppAction>> {
        self.update_selected_agent_policy(AgentPolicyToggle::Enabled)
    }

    fn toggle_selected_agent_user(&mut self) -> Result<Option<AppAction>> {
        self.update_selected_agent_policy(AgentPolicyToggle::UserInvocable)
    }

    fn toggle_selected_agent_model(&mut self) -> Result<Option<AppAction>> {
        self.update_selected_agent_policy(AgentPolicyToggle::ModelInvocable)
    }

    fn update_selected_agent_policy(
        &mut self,
        toggle: AgentPolicyToggle,
    ) -> Result<Option<AppAction>> {
        if self.is_busy {
            self.last_notice = Some("busy; update agent policy later".to_owned());
            return Ok(None);
        }
        let Some(cached) = self.selected_config_agent() else {
            return Ok(None);
        };
        let Some((agent, snapshot)) = self.refresh_selected_agent_for_review(&cached) else {
            return Ok(None);
        };
        if agent.source == AgentProfileSource::System {
            self.last_notice = Some(format!(
                "agent {} is system-managed",
                agent.profile.id.as_str()
            ));
            return Ok(None);
        }

        let source_enabled = agent.enabled;
        let source_user = agent.profile.user_invocation_allowed();
        let source_model = agent.profile.model_invocation_allowed();
        let mut target_enabled = agent.effective_enabled();
        let mut target_user = agent.effective_user_invocation_allowed();
        let mut target_model = agent.effective_model_invocation_allowed();
        match toggle {
            AgentPolicyToggle::Enabled => target_enabled = !target_enabled,
            AgentPolicyToggle::UserInvocable => target_user = !target_user,
            AgentPolicyToggle::ModelInvocable => target_model = !target_model,
        }

        let policy = AgentProfilePolicyEntry {
            profile_id: agent.profile.id.clone(),
            source: snapshot.source.clone(),
            source_hash: snapshot.source_hash.clone(),
            profile_hash: snapshot.profile_hash.clone(),
            enabled: policy_override(target_enabled, source_enabled),
            user_invocable: policy_override(target_user, source_user),
            model_invocable: policy_override(target_model, source_model),
            reviewed_at_ms: unix_time_ms(),
        };

        self.ensure_current_session_identity()?;
        self.append_agent_profile_snapshot(snapshot)?;
        self.append_control_to_current_session(ControlEntry::AgentProfilePolicyDecision(policy))?;
        self.refresh_config_agents_for_profile(&agent.profile.id);

        let label = match toggle {
            AgentPolicyToggle::Enabled => "enabled",
            AgentPolicyToggle::UserInvocable => "user",
            AgentPolicyToggle::ModelInvocable => "model",
        };
        self.last_notice = Some(format!(
            "agent {} {label}={}",
            agent.profile.id.as_str(),
            match toggle {
                AgentPolicyToggle::Enabled => bool_summary(target_enabled),
                AgentPolicyToggle::UserInvocable => bool_summary(target_user),
                AgentPolicyToggle::ModelInvocable => bool_summary(target_model),
            }
        ));
        self.push_event(
            "agent",
            format!("{} policy {label}", agent.profile.id.as_str()),
        );
        Ok(None)
    }

    fn selected_config_agent(&mut self) -> Option<ResolvedAgentProfile> {
        let Some(config_state) = self.config_state.as_ref() else {
            self.last_notice = Some("config is unavailable".to_owned());
            return None;
        };
        if config_state.selected_section != ConfigSection::Agents {
            self.last_notice = Some("agent review is available in Agents config".to_owned());
            return None;
        }
        let Some(agent) = config_state.selected_agent() else {
            self.last_notice = Some("no agent selected".to_owned());
            return None;
        };
        Some(agent.clone())
    }

    fn refresh_selected_agent_for_review(
        &mut self,
        cached: &ResolvedAgentProfile,
    ) -> Option<(ResolvedAgentProfile, AgentProfileSnapshot)> {
        let Some(root_config) = self.config_snapshot.clone() else {
            self.last_notice = Some("config is unavailable in setup mode".to_owned());
            return None;
        };
        let profile_id = cached.profile.id.clone();
        let registry = match AgentProfileRegistry::from_root_config_with_workspace_and_entries(
            &root_config,
            &self.workspace_root,
            &self.current_session_entries,
        ) {
            Ok(registry) => registry,
            Err(error) => {
                self.last_notice = Some(format!("agent discovery failed: {error}"));
                return None;
            }
        };
        let refreshed = registry.get(&profile_id).cloned();
        let snapshot = registry.capture_snapshot(&profile_id);
        self.refresh_config_agents_from_registry(registry, &profile_id);

        let Some(refreshed) = refreshed else {
            self.last_notice = Some(format!(
                "agent {} is no longer available; review refreshed",
                profile_id.as_str()
            ));
            return None;
        };
        if refreshed.source_hash != cached.source_hash || refreshed.profile != cached.profile {
            self.last_notice = Some(format!(
                "agent {} changed; review refreshed",
                profile_id.as_str()
            ));
            return None;
        }
        let snapshot = match snapshot {
            Ok(snapshot) => snapshot,
            Err(error) => {
                self.last_notice = Some(format!(
                    "agent {} snapshot failed: {error}",
                    profile_id.as_str()
                ));
                return None;
            }
        };
        Some((refreshed, snapshot))
    }

    fn refresh_config_agents_for_profile(&mut self, profile_id: &AgentProfileId) {
        let Some(root_config) = self.config_snapshot.clone() else {
            return;
        };
        let Ok(registry) = AgentProfileRegistry::from_root_config_with_workspace_and_entries(
            &root_config,
            &self.workspace_root,
            &self.current_session_entries,
        ) else {
            return;
        };
        self.refresh_config_agents_from_registry(registry, profile_id);
    }

    fn refresh_config_agents_from_registry(
        &mut self,
        registry: AgentProfileRegistry,
        profile_id: &AgentProfileId,
    ) {
        let profiles = registry.profiles().to_vec();
        let warnings = registry.warnings().to_vec();
        if let Some(config_state) = self.config_state.as_mut() {
            config_state.set_agent_discovery(profiles, warnings);
            if let Some(index) = config_state
                .agent_profiles
                .iter()
                .position(|agent| agent.profile.id == *profile_id)
            {
                config_state.selected_agent_index = index;
            }
        }
    }

    fn append_agent_profile_snapshot(&mut self, snapshot: AgentProfileSnapshot) -> Result<()> {
        self.append_control_to_current_session(ControlEntry::AgentProfileCaptured(
            AgentProfileCapturedEntry { snapshot },
        ))
    }

    fn selected_config_skill(&mut self) -> Option<SkillDescriptor> {
        let Some(config_state) = self.config_state.as_ref() else {
            self.last_notice = Some("config is unavailable".to_owned());
            return None;
        };
        if !matches!(config_state.selected_section, ConfigSection::Skills) {
            self.last_notice = Some("skill action is available in Skills config".to_owned());
            return None;
        }
        let Some(skill) = config_state.selected_skill() else {
            self.last_notice = Some("no skill selected".to_owned());
            return None;
        };
        Some(skill.clone())
    }

    fn review_selected_plugin(
        &mut self,
        decision: PluginTrustDecision,
    ) -> Result<Option<AppAction>> {
        if self.is_busy {
            self.last_notice = Some("busy; review plugin later".to_owned());
            return Ok(None);
        }
        let Some(plugin) = self.selected_config_plugin() else {
            return Ok(None);
        };
        let Some(plugin) = self.refresh_selected_plugin_for_review(&plugin) else {
            return Ok(None);
        };

        plugin.validate()?;
        let trust = PluginTrustEntry {
            plugin_id: plugin.plugin_id.clone(),
            manifest_path: plugin.manifest_path.clone(),
            manifest_hash: plugin.manifest_hash.clone(),
            decision,
            reviewed_at_ms: unix_time_ms(),
        };
        trust.validate()?;

        self.ensure_current_session_identity()?;
        self.append_plugin_review_entries(plugin.clone(), trust)?;
        if let Some(config_state) = self.config_state.as_mut()
            && let Some(selected) = config_state.selected_plugin_mut()
        {
            selected.trust = decision;
        }

        let action = plugin_review_action_label(decision);
        self.last_notice = Some(format!("plugin {} {action}", plugin.plugin_id));
        self.push_event("plugin", format!("{} {action}", plugin.plugin_id));
        Ok(None)
    }

    fn selected_config_plugin(&mut self) -> Option<PluginManifestSnapshot> {
        let Some(config_state) = self.config_state.as_ref() else {
            self.last_notice = Some("config is unavailable".to_owned());
            return None;
        };
        if config_state.selected_section != ConfigSection::Plugins {
            self.last_notice = Some("plugin review is available in Plugins config".to_owned());
            return None;
        }
        let Some(plugin) = config_state.selected_plugin() else {
            self.last_notice = Some("no plugin selected".to_owned());
            return None;
        };
        Some(plugin.clone())
    }

    fn refresh_selected_plugin_for_review(
        &mut self,
        cached: &PluginManifestSnapshot,
    ) -> Option<PluginManifestSnapshot> {
        let (manifests, warnings) = self.discover_config_plugins();
        let refreshed = manifests
            .iter()
            .find(|plugin| {
                plugin.plugin_id == cached.plugin_id && plugin.manifest_path == cached.manifest_path
            })
            .cloned();
        if let Some(config_state) = self.config_state.as_mut() {
            config_state.set_plugin_discovery(manifests, warnings);
            if let Some(index) = config_state.plugin_manifests.iter().position(|plugin| {
                plugin.plugin_id == cached.plugin_id && plugin.manifest_path == cached.manifest_path
            }) {
                config_state.selected_plugin_index = index;
            }
        }

        let Some(refreshed) = refreshed else {
            self.last_notice = Some(format!(
                "plugin {} is no longer available; review refreshed",
                cached.plugin_id
            ));
            return None;
        };
        if refreshed.manifest_hash != cached.manifest_hash {
            self.last_notice = Some(format!(
                "plugin {} changed; review refreshed",
                cached.plugin_id
            ));
            return None;
        }
        Some(refreshed)
    }

    fn ensure_current_session_identity(&mut self) -> Result<()> {
        if self.current_session_entries.iter().any(|entry| {
            matches!(
                entry,
                SessionLogEntry::Control(ControlEntry::SessionIdentity { .. })
            )
        }) {
            return Ok(());
        }
        self.append_control_to_current_session(ControlEntry::SessionIdentity {
            provider_name: self.provider_name.clone(),
            model_name: self.model_name.clone(),
        })
    }

    fn append_plugin_review_entries(
        &mut self,
        snapshot: PluginManifestSnapshot,
        trust: PluginTrustEntry,
    ) -> Result<()> {
        self.append_control_to_current_session(ControlEntry::PluginManifestCaptured(snapshot))?;
        self.append_control_to_current_session(ControlEntry::PluginTrustDecision(trust))?;
        Ok(())
    }

    pub(super) fn append_control_to_current_session(
        &mut self,
        control: ControlEntry,
    ) -> Result<()> {
        let entry = SessionLogEntry::Control(control.clone());
        let store = JsonlSessionStore::new(&self.session_log_path)?;
        store.append(&entry)?;
        self.append_current_session_control(control);
        Ok(())
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

    fn activate_selected_mcp_server(&mut self) -> Result<Option<AppAction>> {
        if self.is_busy {
            self.last_notice = Some("busy; activate MCP later".to_owned());
            return Ok(None);
        }
        let Some(config_state) = self.config_state.as_ref() else {
            return Ok(None);
        };
        if config_state.selected_section != ConfigSection::Mcp {
            self.last_notice = Some("activate MCP is available in MCP config".to_owned());
            return Ok(None);
        }
        if config_state.dirty {
            self.last_notice = Some("save config before activating MCP".to_owned());
            return Ok(None);
        }
        let Some(root_config) = self.config_snapshot.as_ref() else {
            self.last_notice = Some("config is unavailable".to_owned());
            return Ok(None);
        };
        let Some(server) = root_config
            .mcp_servers
            .get(config_state.selected_mcp_server_index)
        else {
            self.last_notice = Some("no MCP server selected".to_owned());
            return Ok(None);
        };
        if server.startup != McpServerStartup::Lazy {
            self.last_notice = Some(format!(
                "MCP server {} is {}",
                server.name,
                server.startup.as_str()
            ));
            return Ok(None);
        }

        let server_name = server.name.clone();
        self.mcp_server_statuses
            .insert(server_name.clone(), McpServerRuntimeStatus::Activating);
        self.last_notice = Some(format!("activating MCP {server_name}"));
        self.push_event("mcp", format!("activate {server_name}"));
        Ok(Some(AppAction::ActivateLazyMcp {
            server_name: Some(server_name),
        }))
    }

    pub(super) fn apply_runtime_config_snapshot(&mut self, root_config: &RootConfig) {
        self.config_snapshot = Some(root_config.clone());
        self.secret_redactor = sigil_runtime::secret_redactor_for_root_config(root_config);
        self.permission_default_mode = root_config.permission.default_mode.as_str().to_owned();
        self.memory_config = root_config.memory.clone();
        self.compaction_config = root_config.compaction.clone();
        self.code_intelligence_status =
            code_intelligence_config_status(&root_config.code_intelligence);
        self.code_intelligence_server_lines.clear();
        self.code_intelligence_diagnostics_line = None;
        self.code_intelligence_diagnostics_by_path.clear();
        self.mcp_server_statuses = initial_mcp_server_statuses(root_config);
        if self.current_session_entries.is_empty() {
            self.provider_name = root_config.agent.provider.clone();
            self.model_name = root_config.agent.model.clone();
        }
        self.refresh_memory_summary();
        self.recompute_compaction_status(false);
        self.refresh_usage_sidebar_cache();
        let (skills, warnings) = self.discover_config_skills(root_config);
        let (plugins, plugin_warnings) = self.discover_config_plugins();
        if let Some(config_state) = self.config_state.as_mut() {
            config_state.set_skill_discovery(skills, warnings);
            config_state.set_plugin_discovery(plugins, plugin_warnings);
        }
    }

    #[cfg(test)]
    pub(crate) fn mcp_server_runtime_status_label(&self, server_name: &str) -> Option<String> {
        self.mcp_server_statuses
            .get(server_name)
            .map(McpServerRuntimeStatus::label)
    }

    pub(crate) fn mcp_sidebar_lines(&self) -> Vec<String> {
        let Some(root_config) = self.config_snapshot.as_ref() else {
            return Vec::new();
        };

        root_config
            .mcp_servers
            .iter()
            .map(|server| {
                let status = self
                    .mcp_server_statuses
                    .get(&server.name)
                    .cloned()
                    .unwrap_or_else(|| initial_mcp_server_status(server));
                format!("{}: {}", server.name, status.label())
            })
            .collect()
    }

    fn selected_mcp_runtime_status_label(&self, config_state: &ConfigState) -> String {
        let Some(config) = config_state
            .draft
            .base_root_config
            .mcp_servers
            .get(config_state.selected_mcp_server_index)
        else {
            return "unsaved".to_owned();
        };
        self.mcp_server_statuses
            .get(&config.name)
            .cloned()
            .unwrap_or_else(|| initial_mcp_server_status(config))
            .label()
    }

    fn render_code_intelligence_readiness_summary(
        &self,
        config_state: &ConfigState,
    ) -> Vec<String> {
        let root_config = config_state.draft.code_intelligence_preview_root_config();
        let checks = build_code_intelligence_checks(&root_config, &self.workspace_root);
        let mut lines = vec![
            render_config_readonly_row("Saved runtime", &self.code_intelligence_status),
            render_config_readonly_row(
                "Draft status",
                &code_intelligence_config_status(&root_config.code_intelligence),
            ),
            render_config_readonly_row("Readiness", code_intelligence_overall_label(&checks)),
        ];

        for check in checks.iter().take(4) {
            lines.push(render_code_intelligence_check_row(check));
            if let Some(remediation) = &check.remediation {
                lines.push(render_config_hint_row(remediation));
            }
        }
        if checks.len() > 4 {
            lines.push(format!("... {} more checks", checks.len() - 4));
        }
        lines
    }
}

pub(super) fn cycle_approval_mode(mode: ApprovalMode) -> ApprovalMode {
    match mode {
        ApprovalMode::Allow => ApprovalMode::Ask,
        ApprovalMode::Ask => ApprovalMode::Deny,
        ApprovalMode::Deny => ApprovalMode::Allow,
    }
}

fn cycle_code_intel_startup(startup: CodeIntelStartup) -> CodeIntelStartup {
    match startup {
        CodeIntelStartup::Off => CodeIntelStartup::Lazy,
        CodeIntelStartup::Lazy => CodeIntelStartup::Eager,
        CodeIntelStartup::Eager => CodeIntelStartup::Off,
    }
}

fn render_effective_context_window(config_state: &ConfigState) -> String {
    let fallback_tokens = config_state
        .draft
        .compaction_context_window_tokens
        .trim()
        .parse::<u32>()
        .ok()
        .filter(|tokens| *tokens > 0);
    let resolved = resolve_context_window_tokens(
        &config_state.draft.provider_name,
        config_state.draft.provider_model.trim(),
        fallback_tokens,
    );

    match resolved.tokens {
        Some(tokens) if tokens > 0 => format!(
            "{} tokens  source={}",
            format_token_count(tokens as u64),
            config_context_window_source_label(resolved.source)
        ),
        _ => "unknown  source=none".to_owned(),
    }
}

fn config_context_window_source_label(source: ContextWindowSource) -> &'static str {
    match source {
        ContextWindowSource::Provider => "provider",
        ContextWindowSource::Config => "fallback",
        ContextWindowSource::None => "none",
    }
}

#[derive(Debug, Clone, Copy)]
enum AgentPolicyToggle {
    Enabled,
    UserInvocable,
    ModelInvocable,
}

fn policy_override(target: bool, source: bool) -> Option<bool> {
    (target != source).then_some(target)
}

fn move_config_collection_selection(
    config_state: &mut ConfigState,
    forward: bool,
) -> Option<ConfigFieldMove> {
    match config_state.selected_section {
        ConfigSection::Agents => Some(config_state.move_agent(forward)),
        ConfigSection::Skills => Some(config_state.move_skill(forward)),
        ConfigSection::Plugins => Some(config_state.move_plugin(forward)),
        _ => None,
    }
}

fn config_collection_selection_notice(config_state: &ConfigState) -> Option<String> {
    match config_state.selected_section {
        ConfigSection::Agents if config_state.selected_agent().is_some() => {
            Some(selected_agent_summary(config_state))
        }
        ConfigSection::Skills if config_state.selected_skill().is_some() => {
            Some(selected_skill_summary(config_state))
        }
        ConfigSection::Plugins if !config_state.plugin_manifests.is_empty() => Some(format!(
            "plugin {}/{}",
            config_state.selected_plugin_index + 1,
            config_state.plugin_manifests.len()
        )),
        _ => None,
    }
}

fn focus_first_config_footer_action(config_state: &mut ConfigState) -> ConfigFooterAction {
    let action = ConfigFooterAction::actions_for_section(config_state.selected_section)
        .first()
        .copied()
        .unwrap_or(ConfigFooterAction::Close);
    config_state.focus_footer(action);
    action
}

fn config_field_display_label(config_state: &ConfigState, field: ConfigField) -> &'static str {
    if matches!(field, ConfigField::SkillId)
        && config_state.selected_section == ConfigSection::Agents
        && config_state.selected_agent().is_some()
    {
        return "Agent";
    } else if matches!(field, ConfigField::SkillId)
        && let Some(skill) = config_state.selected_skill()
    {
        return skill_display_title(skill);
    } else if matches!(field, ConfigField::SkillId)
        && config_state.selected_section == ConfigSection::Agents
    {
        return "Agent";
    }
    field.display_label()
}

fn config_field_key_label(config_state: &ConfigState, field: ConfigField) -> &'static str {
    if matches!(field, ConfigField::SkillId)
        && config_state.selected_section == ConfigSection::Agents
        && config_state.selected_agent().is_some()
    {
        return "agent";
    } else if matches!(field, ConfigField::SkillId)
        && let Some(skill) = config_state.selected_skill()
    {
        return skill_display_noun(skill);
    } else if matches!(field, ConfigField::SkillId)
        && config_state.selected_section == ConfigSection::Agents
    {
        return "agent";
    }
    field.label()
}

fn config_field_help_text(config_state: &ConfigState, field: ConfigField) -> &'static str {
    if matches!(field, ConfigField::SkillId)
        && config_state.selected_section == ConfigSection::Agents
        && config_state.selected_agent().is_some()
    {
        return "Selected agent profile. Up/Down moves through agents; footer actions write durable trust or policy decisions.";
    } else if matches!(field, ConfigField::SkillId)
        && let Some(skill) = config_state.selected_skill()
        && skill_is_agent(skill)
    {
        return "Selected child-session agent. Up/Down moves through agents; footer actions load or invoke it.";
    } else if matches!(field, ConfigField::SkillId)
        && config_state.selected_section == ConfigSection::Agents
    {
        return "Selected child-session agent. Up/Down moves through agents; footer actions load or invoke it.";
    }
    field.help_text()
}

fn render_config_selection_details(config_state: &ConfigState) -> Vec<String> {
    let Some(field) = config_state.selected_field else {
        let mut lines = vec![
            String::new(),
            "[details]".to_owned(),
            CONFIG_CONTROLS_HINT.to_owned(),
            CONFIG_ACTIONS_HINT.to_owned(),
        ];
        if config_state.selected_section == ConfigSection::Mcp {
            lines.push("mcp: Ctrl-N add · Ctrl-D drop · PgUp/PgDn server".to_owned());
        } else if config_state.selected_section == ConfigSection::Agents {
            lines.push("agents: Up/Down agent · PgUp/PgDn wrap · footer trust/policy".to_owned());
        } else if config_state.selected_section == ConfigSection::Skills {
            lines.push("skills: Up/Down skill · PgUp/PgDn wrap · footer load/invoke".to_owned());
        } else if config_state.selected_section == ConfigSection::Plugins {
            lines.push("plugins: Up/Down plugin · PgUp/PgDn wrap · footer approve/deny".to_owned());
        }
        return lines;
    };
    let mut lines = vec![
        String::new(),
        "[details]".to_owned(),
        format!(
            "selected: {}",
            config_field_display_label(config_state, field)
        ),
        format!("key: {}", config_field_key_label(config_state, field)),
        config_field_help_text(config_state, field).to_owned(),
        String::new(),
        CONFIG_CONTROLS_HINT.to_owned(),
        CONFIG_ACTIONS_HINT.to_owned(),
    ];

    if matches!(field, ConfigField::ProviderApiKey) {
        let env_name = provider_api_key_env_name(&config_state.draft.provider_name);
        lines.push(format!("override: {env_name}"));
        lines.push("storage: saved api_key is plaintext in sigil.toml".to_owned());
    }
    if matches!(field, ConfigField::ProviderFimModel) {
        lines.push("advanced: provider-specific fields remain in config file or env".to_owned());
    }
    if config_state.selected_section == ConfigSection::Mcp {
        lines.push("mcp: Ctrl-N add · Ctrl-D drop · PgUp/PgDn server".to_owned());
    } else if config_state.selected_section == ConfigSection::Agents {
        lines.push("agents: Up/Down agent · PgUp/PgDn wrap · footer trust/policy".to_owned());
    } else if config_state.selected_section == ConfigSection::Skills {
        lines.push("skills: Up/Down skill · PgUp/PgDn wrap · footer load/invoke".to_owned());
    } else if config_state.selected_section == ConfigSection::Plugins {
        lines.push("plugins: Up/Down plugin · PgUp/PgDn wrap · footer approve/deny".to_owned());
    }

    lines
}

fn render_plugin_detail_lines(plugin: &PluginManifestSnapshot) -> Vec<String> {
    let name = if plugin.name.trim().is_empty() {
        plugin.plugin_id.as_str()
    } else {
        plugin.name.as_str()
    };
    let description = plugin.description.as_deref().unwrap_or("none");
    let mut lines = vec![
        render_config_readonly_row("Name", name),
        render_config_readonly_row("Version", &plugin.version),
        render_config_readonly_row("Description", description),
        render_config_readonly_row("Trust", plugin.trust.as_str()),
        render_config_readonly_row("Manifest", &plugin.manifest_path.display().to_string()),
    ];
    push_wrapped_readonly_rows(&mut lines, "Hash", &plugin.manifest_hash);
    lines.push(render_config_readonly_row(
        "Implications",
        &plugin_implication_summary(&plugin.capabilities),
    ));
    lines.extend(render_plugin_skill_lines(&plugin.capabilities));
    lines.extend(render_plugin_hook_lines(&plugin.capabilities));
    lines.extend(render_plugin_mcp_lines(&plugin.capabilities));
    lines.push(render_config_readonly_row(
        "Approve",
        "trusts this manifest hash",
    ));
    lines.push(render_config_readonly_row(
        "Deny",
        "disables this manifest hash",
    ));
    lines
}

fn render_skill_detail_lines(skill: &SkillDescriptor) -> Vec<String> {
    let name = if skill.name.trim().is_empty() {
        skill.id.as_str()
    } else {
        skill.name.as_str()
    };
    let description = if skill.description.trim().is_empty() {
        "none"
    } else {
        skill.description.as_str()
    };
    let argument_hint = skill.argument_hint.as_deref().unwrap_or("none");

    vec![
        render_config_readonly_row("Type", skill_display_noun(skill)),
        render_config_readonly_row("Name", name),
        render_config_readonly_row("Description", description),
        render_config_readonly_row("Enabled", bool_summary(skill.enabled)),
        render_config_readonly_row("Model", bool_summary(skill.model_invocable)),
        render_config_readonly_row("User", bool_summary(skill.user_invocable)),
        render_config_readonly_row("Run mode", skill.run_as.as_str()),
        render_config_readonly_row("Trust", skill.trust.as_str()),
        render_config_readonly_row("Source", &skill_source_summary(&skill.source)),
        render_config_readonly_row("Hash", &short_hash(&skill.sha256)),
        render_config_readonly_row("Entrypoint", &skill.entrypoint.display().to_string()),
        render_config_readonly_row("Root", &skill.root.display().to_string()),
        render_config_readonly_row("Argument hint", argument_hint),
        render_config_readonly_row("Slash", &skill_slash_summary(skill)),
        render_config_readonly_row("Allowed tools", &tool_scope_summary(&skill.allowed_tools)),
        render_config_readonly_row(
            "Disallowed tools",
            &tool_scope_summary(&skill.disallowed_tools),
        ),
        render_config_readonly_row("Paths", &path_pattern_summary(&skill.path_patterns)),
        render_config_readonly_row(
            "Load",
            skill_action_label(skill_load_unavailable_reason(skill)),
        ),
        render_config_readonly_row(
            "Invoke",
            skill_action_label(skill_invoke_unavailable_reason(skill)),
        ),
    ]
}

fn render_agent_detail_lines(agent: &ResolvedAgentProfile) -> Vec<String> {
    let description = if agent.profile.description.trim().is_empty() {
        "none"
    } else {
        agent.profile.description.as_str()
    };
    let provider = agent.profile.provider.as_deref().unwrap_or("session");
    let model = agent.profile.model.as_deref().unwrap_or("session");
    let reasoning = agent
        .profile
        .reasoning_effort
        .as_ref()
        .map(|effort| effort.as_str())
        .unwrap_or("session");
    vec![
        render_config_readonly_row("Kind", agent_profile_kind_label(agent.profile.kind)),
        render_config_readonly_row("Description", description),
        render_config_readonly_row("Enabled", &agent_enabled_summary(agent)),
        render_config_readonly_row("User", &agent_user_invocable_summary(agent)),
        render_config_readonly_row("Model", &agent_model_invocable_summary(agent)),
        render_config_readonly_row("Trust", agent_trust_state_label(agent.trust_state)),
        render_config_readonly_row("Source", &agent_profile_source_summary(&agent.source)),
        render_config_readonly_row("Source hash", &short_hash(&agent.source_hash)),
        render_config_readonly_row("Provider", provider),
        render_config_readonly_row("Model name", model),
        render_config_readonly_row("Reasoning", reasoning),
        render_config_readonly_row("Invocation", agent.profile.invocation_policy.as_str()),
        render_config_readonly_row("Result", agent.profile.result_policy.as_str()),
        render_config_readonly_row("Tools", &tool_scope_summary(&agent.profile.tool_scope)),
        render_config_readonly_row(
            "Permission",
            agent.profile.permission_policy.default_mode.as_str(),
        ),
        render_config_readonly_row("Skills", &list_summary(&agent.profile.skills)),
        render_config_readonly_row("MCP", &list_summary(&agent.profile.mcp_servers)),
        render_config_readonly_row(
            "Nicknames",
            &list_summary(&agent.profile.nickname_candidates),
        ),
    ]
}

fn render_agent_index_lines(config_state: &ConfigState) -> Vec<String> {
    config_state
        .agent_profiles
        .iter()
        .enumerate()
        .map(|(index, agent)| {
            let marker = if index == config_state.selected_agent_index {
                ">"
            } else {
                " "
            };
            format!(
                "{marker} {}: {} · {} · {} · {}",
                agent.profile.id.as_str(),
                agent_trust_state_label(agent.trust_state),
                agent_profile_kind_label(agent.profile.kind),
                agent_profile_source_summary(&agent.source),
                agent_policy_flags(agent)
            )
        })
        .collect()
}

fn render_skill_index_lines(config_state: &ConfigState, agents: bool) -> Vec<String> {
    config_state
        .skill_descriptors
        .iter()
        .enumerate()
        .filter(|(_, skill)| skill_is_agent(skill) == agents)
        .map(|(index, skill)| {
            let marker = if index == config_state.selected_skill_index {
                ">"
            } else {
                " "
            };
            format!(
                "{marker} {}: {} · {} · {} · {}",
                skill.id,
                skill.trust.as_str(),
                skill.run_as.as_str(),
                skill_source_summary(&skill.source),
                skill_slash_summary(skill)
            )
        })
        .collect()
}

fn skill_config_counts(config_state: &ConfigState) -> (usize, usize) {
    let agent_count = config_state
        .skill_descriptors
        .iter()
        .filter(|skill| skill_is_agent(skill))
        .count();
    let skill_count = config_state
        .skill_descriptors
        .len()
        .saturating_sub(agent_count);
    (skill_count, agent_count)
}

fn selected_skill_summary(config_state: &ConfigState) -> String {
    let Some(skill) = config_state.selected_skill() else {
        return "none".to_owned();
    };
    let selected_is_agent = skill_is_agent(skill);
    let total = config_state
        .skill_descriptors
        .iter()
        .filter(|candidate| skill_is_agent(candidate) == selected_is_agent)
        .count();
    let position = config_state
        .skill_descriptors
        .iter()
        .take(config_state.selected_skill_index + 1)
        .filter(|candidate| skill_is_agent(candidate) == selected_is_agent)
        .count();
    format!("{} {position}/{total}", skill_display_noun(skill))
}

fn selected_agent_summary(config_state: &ConfigState) -> String {
    let Some(agent) = config_state.selected_agent() else {
        return "none".to_owned();
    };
    format!(
        "agent {}/{} · {}",
        config_state.selected_agent_index + 1,
        config_state.agent_profiles.len(),
        agent.profile.id.as_str()
    )
}

fn agent_policy_flags(agent: &ResolvedAgentProfile) -> String {
    format!(
        "enabled={} user={} model={}",
        bool_summary(agent.effective_enabled()),
        bool_summary(agent.effective_user_invocation_allowed()),
        bool_summary(agent.effective_model_invocation_allowed())
    )
}

fn agent_enabled_summary(agent: &ResolvedAgentProfile) -> String {
    bool_override_summary(
        agent.effective_enabled(),
        agent.enabled,
        agent.enabled_override,
    )
}

fn agent_user_invocable_summary(agent: &ResolvedAgentProfile) -> String {
    bool_override_summary(
        agent.effective_user_invocation_allowed(),
        agent.profile.user_invocation_allowed(),
        agent.user_invocable_override,
    )
}

fn agent_model_invocable_summary(agent: &ResolvedAgentProfile) -> String {
    bool_override_summary(
        agent.effective_model_invocation_allowed(),
        agent.profile.model_invocation_allowed(),
        agent.model_invocable_override,
    )
}

fn bool_override_summary(effective: bool, source: bool, override_value: Option<bool>) -> String {
    match override_value {
        Some(_) => format!(
            "{} (source {})",
            bool_summary(effective),
            bool_summary(source)
        ),
        None => bool_summary(effective).to_owned(),
    }
}

fn agent_profile_kind_label(kind: AgentProfileKind) -> &'static str {
    match kind {
        AgentProfileKind::Primary => "primary",
        AgentProfileKind::Subagent => "subagent",
        AgentProfileKind::System => "system",
        AgentProfileKind::Unknown => "unknown",
    }
}

fn agent_trust_state_label(state: AgentTrustState) -> &'static str {
    match state {
        AgentTrustState::Trusted => "trusted",
        AgentTrustState::NeedsReview => "needs_review",
        AgentTrustState::Disabled => "disabled",
        AgentTrustState::Unknown => "unknown",
    }
}

fn agent_profile_source_summary(source: &AgentProfileSource) -> String {
    match source {
        AgentProfileSource::Workspace => "workspace".to_owned(),
        AgentProfileSource::User => "user".to_owned(),
        AgentProfileSource::Plugin { plugin_id } => format!("plugin:{plugin_id}"),
        AgentProfileSource::Compatibility { provider } => format!("compat:{provider}"),
        AgentProfileSource::System => "system".to_owned(),
        AgentProfileSource::LegacyTask => "legacy_task".to_owned(),
        AgentProfileSource::Unknown => "unknown".to_owned(),
    }
}

fn skill_section_noun(section: ConfigSection) -> &'static str {
    match section {
        ConfigSection::Agents => "agent",
        ConfigSection::Skills => "skill",
        _ => "skill or agent",
    }
}

fn skill_is_agent(skill: &SkillDescriptor) -> bool {
    matches!(skill.run_as, SkillRunMode::ChildSession)
}

fn skill_display_noun(skill: &SkillDescriptor) -> &'static str {
    if skill_is_agent(skill) {
        "agent"
    } else {
        "skill"
    }
}

fn skill_display_title(skill: &SkillDescriptor) -> &'static str {
    if skill_is_agent(skill) {
        "Agent"
    } else {
        "Skill"
    }
}

fn pluralize(noun: &'static str, count: usize) -> &'static str {
    match (noun, count) {
        ("skill", 1) => "skill",
        ("agent", 1) => "agent",
        ("skill", _) => "skills",
        ("agent", _) => "agents",
        _ => noun,
    }
}

fn render_plugin_index_lines(config_state: &ConfigState) -> Vec<String> {
    config_state
        .plugin_manifests
        .iter()
        .enumerate()
        .map(|(index, plugin)| {
            let marker = if index == config_state.selected_plugin_index {
                ">"
            } else {
                " "
            };
            format!(
                "{marker} {}: {} · {}",
                plugin.plugin_id,
                plugin.trust.as_str(),
                plugin.version
            )
        })
        .collect()
}

fn plugin_implication_summary(capabilities: &[PluginCapability]) -> String {
    let mut parts = Vec::new();
    if capabilities
        .iter()
        .any(|capability| matches!(capability, PluginCapability::Skill { .. }))
    {
        parts.push("skill instructions");
    }
    if capabilities
        .iter()
        .any(|capability| matches!(capability, PluginCapability::Hook { .. }))
    {
        parts.push("hook commands");
    }
    if capabilities
        .iter()
        .any(|capability| matches!(capability, PluginCapability::McpServer { .. }))
    {
        parts.push("MCP server processes");
    }
    if parts.is_empty() {
        "none".to_owned()
    } else {
        parts.join(", ")
    }
}

fn render_plugin_skill_lines(capabilities: &[PluginCapability]) -> Vec<String> {
    let skills = capabilities
        .iter()
        .filter_map(|capability| match capability {
            PluginCapability::Skill { path } => Some(path.display().to_string()),
            _ => None,
        })
        .collect::<Vec<_>>();
    let mut lines = vec![String::new(), "[skills]".to_owned()];
    if skills.is_empty() {
        lines.push(render_config_readonly_row("Skill count", "0"));
        return lines;
    }
    for (index, path) in skills.iter().enumerate() {
        push_wrapped_readonly_rows(&mut lines, &format!("Skill {}", index + 1), path);
    }
    lines
}

fn render_plugin_hook_lines(capabilities: &[PluginCapability]) -> Vec<String> {
    let hooks = capabilities
        .iter()
        .filter_map(|capability| match capability {
            PluginCapability::Hook {
                event,
                command,
                args,
                approval,
            } => Some((event, command, args, approval)),
            _ => None,
        })
        .collect::<Vec<_>>();
    let mut lines = vec![String::new(), "[hooks]".to_owned()];
    if hooks.is_empty() {
        lines.push(render_config_readonly_row("Hook count", "0"));
        return lines;
    }
    for (index, (event, command, args, approval)) in hooks.iter().enumerate() {
        let label = format!("Hook {}", index + 1);
        lines.push(render_config_readonly_row(&label, event.as_str()));
        push_wrapped_readonly_rows(
            &mut lines,
            &format!("{label} command"),
            &command_with_args(command, args),
        );
        lines.push(render_config_readonly_row(
            &format!("{label} approval"),
            approval.as_str(),
        ));
    }
    lines
}

fn render_plugin_mcp_lines(capabilities: &[PluginCapability]) -> Vec<String> {
    let servers = capabilities
        .iter()
        .filter_map(|capability| match capability {
            PluginCapability::McpServer {
                name,
                command,
                args,
                startup,
                required,
            } => Some((name, command, args, startup, *required)),
            _ => None,
        })
        .collect::<Vec<_>>();
    let mut lines = vec![String::new(), "[mcp servers]".to_owned()];
    if servers.is_empty() {
        lines.push(render_config_readonly_row("MCP count", "0"));
        return lines;
    }
    for (index, (name, command, args, startup, required)) in servers.iter().enumerate() {
        let label = format!("MCP {}", index + 1);
        lines.push(render_config_readonly_row(&label, name.as_str()));
        push_wrapped_readonly_rows(
            &mut lines,
            &format!("{label} command"),
            &command_with_args(command, args),
        );
        lines.push(render_config_readonly_row(
            &format!("{label} startup"),
            startup.as_str(),
        ));
        lines.push(render_config_readonly_row(
            &format!("{label} required"),
            bool_summary(*required),
        ));
    }
    lines
}

fn push_wrapped_readonly_rows(lines: &mut Vec<String>, label: &str, value: &str) {
    let value = if value.trim().is_empty() {
        "none"
    } else {
        value
    };
    for (index, segment) in chunk_for_review_display(value).into_iter().enumerate() {
        let row_label = if index == 0 {
            label.to_owned()
        } else {
            format!("{label} part {}", index + 1)
        };
        lines.push(render_config_readonly_row(&row_label, &segment));
    }
}

fn chunk_for_review_display(value: &str) -> Vec<String> {
    const CHUNK_SIZE: usize = 48;
    let chars = value.chars().collect::<Vec<_>>();
    if chars.len() <= CHUNK_SIZE {
        return vec![value.to_owned()];
    }
    chars
        .chunks(CHUNK_SIZE)
        .map(|chunk| chunk.iter().collect::<String>())
        .collect()
}

fn command_with_args(command: &str, args: &[String]) -> String {
    std::iter::once(command.to_owned())
        .chain(args.iter().map(|arg| command_arg_display(arg)))
        .collect::<Vec<_>>()
        .join(" ")
}

fn command_arg_display(arg: &str) -> String {
    if arg.chars().any(char::is_whitespace) {
        format!("{arg:?}")
    } else {
        arg.to_owned()
    }
}

fn plugin_review_action_label(decision: PluginTrustDecision) -> &'static str {
    match decision {
        PluginTrustDecision::Trusted => "approved",
        PluginTrustDecision::Disabled => "denied",
        PluginTrustDecision::NeedsReview => "needs review",
    }
}

fn skill_slash_summary(skill: &SkillDescriptor) -> String {
    let command = format!("/{}", skill.id);
    if SLASH_COMMANDS
        .iter()
        .any(|spec| spec.canonical == command || spec.aliases.contains(&command.as_str()))
    {
        format!("shadowed by native {command}")
    } else if skill.user_invocable {
        command
    } else {
        "not user-invocable".to_owned()
    }
}

fn skill_action_label(reason: Option<&'static str>) -> &'static str {
    match reason {
        Some(reason) => reason,
        None => "available",
    }
}

fn skill_load_unavailable_reason(skill: &SkillDescriptor) -> Option<&'static str> {
    if !skill.enabled {
        return Some("is disabled");
    }
    if skill.trust != SkillTrustState::Trusted {
        return Some("is not trusted");
    }
    if !skill.model_invocable {
        return Some("is not model-invocable");
    }
    None
}

fn skill_invoke_unavailable_reason(skill: &SkillDescriptor) -> Option<&'static str> {
    if let Some(reason) = skill_load_unavailable_reason(skill) {
        return Some(reason);
    }
    if !skill.user_invocable {
        return Some("is not user-invocable");
    }
    None
}

fn skill_load_prompt(skill: &SkillDescriptor) -> String {
    format!(
        "Use the `load_skill` tool to load skill `{}`. Only load the skill instructions into context for this turn.",
        skill.id
    )
}

fn skill_invoke_prompt(skill: &SkillDescriptor, arguments: &str) -> String {
    let trimmed = arguments.trim();
    if trimmed.is_empty() {
        return format!(
            "Use the `load_skill` tool to load skill `{}`, then apply that skill to the current task. No additional arguments were provided.",
            skill.id
        );
    }
    format!(
        "Use the `load_skill` tool to load skill `{}`, then apply that skill to the current task with these arguments:\n\n```text\n{}\n```",
        skill.id, trimmed
    )
}

fn skill_source_summary(source: &SkillSource) -> String {
    match source {
        SkillSource::Workspace => "workspace".to_owned(),
        SkillSource::User => "user".to_owned(),
        SkillSource::Plugin { plugin_id } => format!("plugin:{plugin_id}"),
    }
}

fn short_hash(hash: &str) -> String {
    if hash.trim().is_empty() {
        return "none".to_owned();
    }
    let prefix = hash.chars().take(12).collect::<String>();
    if hash.chars().count() > 12 {
        format!("{prefix}...")
    } else {
        prefix
    }
}

fn tool_scope_summary(scope: &ToolRegistryScope) -> String {
    if scope.allow_all {
        return "all".to_owned();
    }
    let mut parts = Vec::new();
    if !scope.names.is_empty() {
        parts.push(format!(
            "names={}",
            scope.names.iter().cloned().collect::<Vec<_>>().join(",")
        ));
    }
    if !scope.prefixes.is_empty() {
        parts.push(format!("prefixes={}", scope.prefixes.join(",")));
    }
    if parts.is_empty() {
        "none".to_owned()
    } else {
        parts.join(" ")
    }
}

fn list_summary(values: &[String]) -> String {
    if values.is_empty() {
        "none".to_owned()
    } else {
        values.join(",")
    }
}

fn path_pattern_summary(patterns: &[String]) -> String {
    if patterns.is_empty() {
        "none".to_owned()
    } else {
        patterns.join(",")
    }
}

fn render_provider_capability_summary(config_state: &ConfigState) -> Vec<String> {
    let provider_name = config_state.draft.provider_name.as_str();
    let Some(capabilities) = provider_capabilities_for_name(provider_name) else {
        return vec![render_config_hint_row("Unknown provider capabilities")];
    };
    let view = provider_capability_view(provider_name, &capabilities);
    let supported = view
        .rows
        .iter()
        .filter(|row| row.status.as_str() == "supported")
        .count();
    let advanced = view
        .rows
        .iter()
        .filter(|row| row.status.as_str() == "advanced")
        .count();
    vec![
        render_config_readonly_row(
            "Provider matrix",
            &format!(
                "{} supported · {} advanced · {} total",
                supported,
                advanced,
                view.rows.len()
            ),
        ),
        render_config_hint_row("Full capability summary is available in /doctor"),
    ]
}

fn provider_api_key_env_name(provider_name: &str) -> &'static str {
    match provider_name {
        OPENAI_COMPAT_PROVIDER_KEY => OPENAI_COMPATIBLE_API_KEY_ENV,
        ANTHROPIC_PROVIDER_KEY => SIGIL_ANTHROPIC_API_KEY_ENV,
        GEMINI_PROVIDER_KEY => SIGIL_GEMINI_API_KEY_ENV,
        _ => SIGIL_API_KEY_ENV,
    }
}

fn render_permission_rule_summary(config_state: &ConfigState) -> Vec<String> {
    let rules = &config_state.draft.base_root_config.permission.rules;
    let rule_count = if rules.is_empty() {
        "none".to_owned()
    } else {
        format!("{} configured", rules.len())
    };
    let mut lines = vec![render_config_readonly_row("Rule overrides", &rule_count)];

    if rules.is_empty() {
        lines.push(render_config_hint_row(
            "All unmatched tools use the default mode above",
        ));
        return lines;
    }

    for rule in rules.iter().take(4) {
        let tool = rule.tool_name.as_deref().unwrap_or("any tool");
        let subject = rule.subject_glob.as_deref().unwrap_or("any subject");
        lines.push(format!("- {tool} · {} · {subject}", rule.mode.as_str()));
    }
    if rules.len() > 4 {
        lines.push(format!("... {} more rules in config file", rules.len() - 4));
    }

    lines
}

fn render_mcp_lifecycle_summary(config_state: &ConfigState, runtime_status: &str) -> Vec<String> {
    let config = config_state
        .draft
        .base_root_config
        .mcp_servers
        .get(config_state.selected_mcp_server_index)
        .cloned()
        .unwrap_or_default();

    vec![
        render_config_readonly_row("Runtime", runtime_status),
        render_config_readonly_row("Required", bool_summary(config.required)),
        render_config_readonly_row("Startup", config.startup.as_str()),
        render_config_readonly_row("Trust", config.trust.trust_class.as_str()),
        render_config_readonly_row("Approval", config.trust.approval_default.as_str()),
        render_config_readonly_row("Pin", mcp_pin_summary(&config)),
        render_config_readonly_row(
            "Secrets",
            if config.trust.allow_secrets {
                "allowed"
            } else {
                "blocked"
            },
        ),
    ]
}

fn render_code_intelligence_trust_summary() -> Vec<String> {
    vec![
        render_config_readonly_row("Tool access", "read-only"),
        render_config_readonly_row("Server process", "local workspace LSP"),
        render_config_readonly_row("Write actions", "unavailable"),
    ]
}

fn code_intelligence_overall_label(checks: &[DoctorCheck]) -> &'static str {
    if checks
        .iter()
        .any(|check| check.status == DoctorStatus::Error)
    {
        return DoctorStatus::Error.as_str();
    }
    if checks
        .iter()
        .any(|check| check.status == DoctorStatus::Warn)
    {
        return DoctorStatus::Warn.as_str();
    }
    DoctorStatus::Ok.as_str()
}

fn render_code_intelligence_check_row(check: &DoctorCheck) -> String {
    format!(
        "- {}: {} · {}",
        check.name,
        check.status.as_str(),
        check.message
    )
}

fn mcp_pin_summary(config: &McpServerConfig) -> &'static str {
    if !config.trust.pin_version {
        "off"
    } else if config.trust.pinned.is_some() {
        "pinned"
    } else {
        "missing"
    }
}

fn bool_summary(value: bool) -> &'static str {
    if value { "yes" } else { "no" }
}

fn render_config_hint_row(text: &str) -> String {
    format!("i {text}")
}

fn unix_time_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis().min(u128::from(u64::MAX)) as u64)
        .unwrap_or(0)
}

#[cfg(all(test, not(sigil_tui_test_slice_app_input_flow)))]
#[path = "tests/config_flow_detail_tests.rs"]
mod tests;

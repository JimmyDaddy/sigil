use crate::appearance_diagnostics::appearance_doctor_checks;
use crate::config_panel::{
    CONFIG_ACTIONS_HINT, CONFIG_CONTROLS_HINT, CONFIG_EDIT_OR_TOGGLE_HINT, CONFIG_FIELD_NAV_HINT,
    CONFIG_SAVE_HINT, CONFIG_SECTION_NAV_HINT, ConfigDraft, ConfigField, ConfigFieldMove,
    ConfigFooterAction, ConfigSection, ConfigState, config_field_accepts_char,
    render_config_readonly_row, render_config_value_row,
};
use crate::slash::SLASH_COMMANDS;
use std::{
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::Result;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
#[cfg(test)]
use sigil_kernel::AgentProfilePolicyEntry;
use sigil_kernel::{
    AgentProfileCapturedEntry, AgentProfileId, AgentProfileKind, AgentProfileSnapshot,
    AgentProfileSource, AgentProfileTrustEntry, AgentTrustState, AppearanceConfig, ApprovalMode,
    CodeIntelStartup, ControlEntry, DEFAULT_TASK_VERIFICATION_SCOPE_HASH, DiscoveredCheck,
    JsonlSessionStore, McpServerConfig, McpServerStartup, MutationEventRecorder, PluginCapability,
    PluginManifestSnapshot, PluginStateProjection, PluginTrustDecision, PluginTrustEntry,
    RootConfig, SessionLogEntry, SkillDescriptor, SkillRunMode, SkillSource, SkillTrustState,
    SyntaxThemeId, ThemeId, ToolEffect, ToolRegistryScope, VerificationStateProjection,
    WorkspaceTrust, default_user_config_dir, discover_candidate_checks_with_user_config,
    stable_workspace_id,
};
use sigil_runtime::{
    AgentProfileRegistry, ContextWindowSource, ResolvedAgentProfile,
    doctor::{DoctorCheck, DoctorStatus, build_code_intelligence_checks},
    provider_api_key_env_name, provider_capabilities_for_name, provider_capability_view,
    resolve_context_window_tokens,
};

use super::{
    AppAction, AppState, McpServerRuntimeStatus, MutationArtifactRetentionPreview,
    code_intelligence_config_status,
    formatting::{format_token_count, persisted_root_config},
    initial_mcp_server_status, initial_mcp_server_statuses,
    modal_flow::{
        ModalOutcome, ModalState, ModelPickerTarget, SecretInputTarget, TextInputState,
        TextInputTarget,
    },
};

mod agents;
mod appearance;
mod mcp;
mod permissions;
mod plugins;
mod provider;
mod skills;
mod storage;
mod verification;

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

    #[cfg(test)]
    pub(crate) fn select_config_section_for_test(&mut self, section: ConfigSection) {
        if let Some(state) = self.config_state.as_mut() {
            state.set_section(section);
        }
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

    pub(crate) fn config_preview_appearance(&self) -> Option<AppearanceConfig> {
        let config_state = self.config_state.as_ref()?;
        Some(appearance::draft_appearance_config(config_state))
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

        let mut lines = vec![
            if state.show_advanced {
                "Config · advanced"
            } else {
                "Config · simple"
            }
            .to_owned(),
            String::new(),
        ];
        for section in state.visible_sections() {
            lines.push(format!(
                "{} {}",
                if *section == state.selected_section {
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
        lines.push(format!("{CONFIG_SAVE_HINT}  Ctrl-A advanced  Esc close"));
        if state.selected_section == ConfigSection::Storage {
            lines.push("Storage: footer clean artifacts".to_owned());
        } else if state.selected_section == ConfigSection::Mcp {
            lines.push("MCP: PgUp/PgDn switch".to_owned());
            lines.push("MCP: footer activate/refresh".to_owned());
            lines.push("MCP: edit servers in sigil.toml".to_owned());
        } else if state.selected_section == ConfigSection::Agents {
            lines.push("Agents: Up/Down select".to_owned());
            lines.push("Agents: PgUp/PgDn wrap".to_owned());
            lines.push("Agents: footer trust/disable".to_owned());
        } else if state.selected_section == ConfigSection::Skills {
            lines.push("Skills: Up/Down select".to_owned());
            lines.push("Skills: PgUp/PgDn wrap".to_owned());
            lines.push("Skills: footer use".to_owned());
        } else if state.selected_section == ConfigSection::Plugins {
            lines.push("Plugins: Up/Down select".to_owned());
            lines.push("Plugins: PgUp/PgDn wrap".to_owned());
            lines.push("Plugins: footer approve/deny".to_owned());
        } else if state.selected_section == ConfigSection::Permissions {
            lines.push("Permissions: Enter cycle mode".to_owned());
            lines.push("Permissions: task checks run from task status".to_owned());
        } else if state.selected_section == ConfigSection::Appearance {
            lines.push("Appearance: Enter cycle".to_owned());
            lines.push("Appearance: color overrides in sigil.toml".to_owned());
        } else if state.selected_section == ConfigSection::Terminal {
            lines.push("Terminal: compatibility lives in sigil.toml".to_owned());
        } else if state.selected_section == ConfigSection::CodeIntelligence {
            lines.push("Code Intel: Enter cycle mode/startup".to_owned());
        }
        lines
    }

    pub fn config_detail_lines(&self) -> Vec<String> {
        let Some(config_state) = &self.config_state else {
            return Vec::new();
        };
        let section = config_state.selected_section;
        let visible_sections = config_state.visible_sections();
        let step_label = visible_sections
            .iter()
            .map(|candidate| {
                if *candidate == section {
                    format!("[{}]", candidate.step_token())
                } else {
                    candidate.step_token().to_owned()
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
                provider::render_section(&mut lines, config_state);
            }
            ConfigSection::Storage => {
                storage::render_section(self, &mut lines, config_state);
            }
            ConfigSection::Permissions => {
                permissions::render_section(self, &mut lines, config_state);
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
                    &format!("{} loaded", self.runtime.memory_document_count),
                ));
                lines.push(render_config_readonly_row(
                    "Last scan",
                    &self.runtime.memory_last_status,
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
                lines.push(format!("status: {}", self.runtime.compaction_status));
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
                lines.push(render_config_readonly_row(
                    "Discovery",
                    bool_summary(config_state.draft.code_intelligence_discovery_enabled),
                ));
                lines.push(render_config_readonly_row(
                    "Missing reports",
                    bool_summary(
                        config_state
                            .draft
                            .code_intelligence_discovery_report_missing,
                    ),
                ));
                lines.push(String::new());
                lines.push("[trust]".to_owned());
                lines.extend(render_code_intelligence_trust_summary());
                lines.push(String::new());
                lines.push("[readiness]".to_owned());
                lines.extend(self.render_code_intelligence_readiness_summary(config_state));
                lines.push(render_config_hint_row(
                    "LSP discovery details are configured in sigil.toml or surfaced by doctor",
                ));
                lines.extend(render_config_selection_details(config_state));
            }
            ConfigSection::Terminal => {
                lines.push("[interaction]".to_owned());
                lines.push(render_config_readonly_row(
                    "Keyboard enhancement",
                    bool_summary(config_state.draft.terminal_keyboard_enhancement),
                ));
                lines.push(render_config_readonly_row(
                    "Mouse capture",
                    bool_summary(config_state.draft.terminal_mouse_capture),
                ));
                lines.push(render_config_readonly_row(
                    "OSC52 clipboard",
                    bool_summary(config_state.draft.terminal_osc52_clipboard),
                ));
                lines.push(render_config_readonly_row(
                    "Scroll sensitivity",
                    &format!("{} rows", config_state.draft.terminal_scroll_sensitivity),
                ));
                lines.push(String::new());
                lines.push("[compatibility]".to_owned());
                lines.push(render_config_hint_row(
                    "Terminal compatibility settings are edited in sigil.toml or guided by doctor",
                ));
                lines.push(render_config_hint_row(
                    "Use defaults unless your terminal or multiplexer mishandles mouse/clipboard",
                ));
                lines.extend(render_config_selection_details(config_state));
            }
            ConfigSection::Appearance => {
                lines.push("[theme]".to_owned());
                lines.push(render_config_value_row(
                    config_state,
                    ConfigField::AppearanceTheme,
                ));
                lines.push(render_config_readonly_row(
                    "Name",
                    config_state.draft.appearance_theme.display_label(),
                ));
                lines.push(render_config_value_row(
                    config_state,
                    ConfigField::AppearanceSyntaxTheme,
                ));
                lines.push(render_config_readonly_row(
                    "Syntax source",
                    &appearance::render_syntax_theme_source(config_state),
                ));
                lines.push(render_config_value_row(
                    config_state,
                    ConfigField::AppearanceUsageCostCurrency,
                ));
                lines.push(render_config_readonly_row(
                    "Cost source",
                    &appearance::render_usage_cost_currency_source(config_state),
                ));
                let available = ThemeId::all()
                    .iter()
                    .map(|theme| theme.as_str())
                    .collect::<Vec<_>>()
                    .join(", ");
                lines.push(render_config_readonly_row("Built-ins", &available));
                lines.push(render_config_readonly_row(
                    "Overrides",
                    &format!(
                        "{} colors",
                        config_state.draft.base_root_config.appearance.colors.len()
                    ),
                ));
                lines.push(render_config_hint_row(
                    "Fine-grained color token overrides are edited in sigil.toml",
                ));
                lines.push(String::new());
                lines.push("[diagnostics]".to_owned());
                lines.extend(appearance::render_appearance_diagnostic_lines(config_state));
                lines.push(String::new());
                lines.push("[preview]".to_owned());
                lines.extend(appearance::render_appearance_preview_lines(config_state));
                lines.push(String::new());
                lines.push("[scope]".to_owned());
                lines.push(render_config_hint_row(
                    "Theme choices affect only the TUI and are not written to session history",
                ));
                lines.push(render_config_hint_row(
                    "Theme draft previews immediately; Ctrl-S persists it",
                ));
                lines.extend(render_config_selection_details(config_state));
            }
            ConfigSection::Agents => {
                agents::render_section(&mut lines, config_state);
            }
            ConfigSection::Skills => {
                skills::render_section(&mut lines, config_state);
            }
            ConfigSection::Plugins => {
                plugins::render_section(&mut lines, config_state);
            }
            ConfigSection::Mcp => {
                mcp::render_section(self, &mut lines, config_state);
            }
        }

        lines
    }

    fn render_verification_trust_summary(&self, config_state: &ConfigState) -> Vec<String> {
        let (workspace_id, trust, _) = match self.verification_trust_context() {
            Ok(context) => context,
            Err(error) => {
                return vec![
                    render_config_readonly_row("Workspace trust", "unknown"),
                    render_config_hint_row(&format!(
                        "Verification discovery unavailable: {}",
                        truncate_config_detail(&format!("{error:#}"), 72)
                    )),
                ];
            }
        };
        let user_check_count = config_state
            .draft
            .base_root_config
            .verification
            .checks
            .len();
        let mut lines = vec![
            render_config_readonly_row("Workspace", &truncate_config_detail(&workspace_id, 48)),
            render_config_readonly_row("Workspace trust", workspace_trust_label(trust)),
            render_config_readonly_row("User checks", &format!("{user_check_count} configured")),
            render_config_readonly_row(
                "Repo instructions",
                &repo_instruction_trust_summary(
                    workspace_instruction_files(&self.workspace_root).len(),
                    trust,
                ),
            ),
        ];
        match self.repo_verification_candidates(config_state) {
            Ok(repo_candidates) => {
                lines.push(render_config_readonly_row(
                    "Repo checks",
                    &repo_verification_candidate_summary(repo_candidates.len(), trust),
                ));
                lines.push(render_config_hint_row(
                    "Task status owns run/retry actions; config only sets the long-term policy",
                ));
            }
            Err(error) => {
                lines.push(render_config_readonly_row("Repo checks", "unavailable"));
                lines.push(render_config_hint_row(&format!(
                    "Verification discovery failed: {}",
                    truncate_config_detail(&format!("{error:#}"), 72)
                )));
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
            KeyCode::Char('a') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                if let Some(config_state) = self.config_state.as_mut() {
                    config_state.toggle_advanced_surface();
                    self.last_notice = Some(if config_state.show_advanced {
                        "config advanced surface".to_owned()
                    } else {
                        "config simple surface".to_owned()
                    });
                }
            }
            KeyCode::Char('n') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                if let Some(config_state) = self.config_state.as_mut() {
                    if config_state.selected_section == ConfigSection::Mcp {
                        self.last_notice = Some("edit MCP servers in sigil.toml".to_owned());
                    } else {
                        self.last_notice = Some("MCP server editing uses sigil.toml".to_owned());
                    }
                }
            }
            KeyCode::Char('d') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                if let Some(config_state) = self.config_state.as_mut() {
                    if config_state.selected_section == ConfigSection::Mcp {
                        self.last_notice = Some("edit MCP servers in sigil.toml".to_owned());
                    } else {
                        self.last_notice = Some("MCP server editing uses sigil.toml".to_owned());
                    }
                }
            }
            KeyCode::Char('r') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                if let Some(config_state) = self.config_state.as_mut() {
                    if config_state.selected_section == ConfigSection::Appearance {
                        self.last_notice =
                            Some("color overrides are edited in sigil.toml".to_owned());
                    } else {
                        self.last_notice = Some("Ctrl-R: Appearance only".to_owned());
                    }
                }
            }
            KeyCode::Tab => {
                if let Some(config_state) = self.config_state.as_mut() {
                    config_state.set_next_visible_section();
                    self.last_notice = Some(format!(
                        "step {}",
                        config_state.selected_section.title().to_lowercase()
                    ));
                }
            }
            KeyCode::BackTab => {
                if let Some(config_state) = self.config_state.as_mut() {
                    config_state.set_previous_visible_section();
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
                        config_state.set_previous_visible_section();
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
                        config_state.set_previous_visible_section();
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
                        config_state.set_next_visible_section();
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
                        config_state.set_next_visible_section();
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
                let storage_artifact_count = self.mutation_artifact_inventory_count();
                if let Some(config_state) = self.config_state.as_mut() {
                    if config_state.footer_selected {
                        if config_state.focus_last_field()
                            && let Some(field) = config_state.selected_field
                        {
                            self.last_notice = config_collection_selection_notice(
                                config_state,
                                storage_artifact_count,
                            )
                            .or_else(|| Some(format!("config field {}", field.label())));
                        } else {
                            config_state.footer_selected = false;
                            self.last_notice = Some(format!(
                                "step {}",
                                config_state.selected_section.title().to_lowercase()
                            ));
                        }
                    } else {
                        match move_config_collection_selection(
                            config_state,
                            false,
                            storage_artifact_count,
                        ) {
                            Some(ConfigFieldMove::Moved) => {
                                self.last_notice = config_collection_selection_notice(
                                    config_state,
                                    storage_artifact_count,
                                );
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
                let storage_artifact_count = self.mutation_artifact_inventory_count();
                if let Some(config_state) = self.config_state.as_mut() {
                    if config_state.footer_selected {
                        return Ok(None);
                    }
                    match move_config_collection_selection(
                        config_state,
                        true,
                        storage_artifact_count,
                    ) {
                        Some(ConfigFieldMove::Moved) => {
                            self.last_notice = config_collection_selection_notice(
                                config_state,
                                storage_artifact_count,
                            );
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
                        #[cfg(test)]
                        ConfigFooterAction::SaveAndClose => self.save_config_draft_and_close(),
                        ConfigFooterAction::CleanMutationArtifacts => {
                            self.clean_selected_mutation_artifacts()
                        }
                        ConfigFooterAction::ActivateMcp => self.activate_selected_mcp_server(),
                        ConfigFooterAction::TrustAgent => {
                            self.review_selected_agent(AgentTrustState::Trusted)
                        }
                        ConfigFooterAction::BlockAgent => {
                            self.review_selected_agent(AgentTrustState::Disabled)
                        }
                        #[cfg(test)]
                        ConfigFooterAction::ToggleAgentEnabled => {
                            self.toggle_selected_agent_enabled()
                        }
                        #[cfg(test)]
                        ConfigFooterAction::ToggleAgentUser => self.toggle_selected_agent_user(),
                        #[cfg(test)]
                        ConfigFooterAction::ToggleAgentModel => self.toggle_selected_agent_model(),
                        ConfigFooterAction::UseSkill => self.open_selected_skill_arguments(),
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
                        ConfigField::VerificationAutoRun => {
                            config_state.draft.verification_auto_run =
                                config_state.draft.verification_auto_run.next();
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
                        ConfigField::AppearanceTheme => {
                            config_state.draft.appearance_theme =
                                config_state.draft.appearance_theme.next();
                            config_state.dirty = true;
                            self.last_notice = Some(format!(
                                "theme -> {}",
                                config_state.draft.appearance_theme.as_str()
                            ));
                            return Ok(None);
                        }
                        ConfigField::AppearanceSyntaxTheme => {
                            config_state.draft.cycle_appearance_syntax_theme();
                            config_state.dirty = true;
                            self.last_notice = Some(format!(
                                "syntax theme -> {}",
                                config_state.draft.appearance_syntax_theme.as_str()
                            ));
                            return Ok(None);
                        }
                        ConfigField::AppearanceUsageCostCurrency => {
                            config_state.draft.cycle_appearance_usage_cost_currency();
                            config_state.dirty = true;
                            self.last_notice = Some(format!(
                                "cost currency -> {}",
                                config_state.draft.appearance_usage_cost_currency.as_str()
                            ));
                            return Ok(None);
                        }
                        ConfigField::AppearanceColorGroup => {
                            config_state.draft.cycle_appearance_color_group(true);
                            self.last_notice = Some(format!(
                                "color group -> {}",
                                config_state.draft.selected_appearance_color_group().key
                            ));
                            return Ok(None);
                        }
                        ConfigField::AppearanceColorToken => {
                            config_state.draft.cycle_appearance_color_token(true);
                            self.last_notice = Some(format!(
                                "color token -> {}",
                                config_state.draft.selected_appearance_color_token()
                            ));
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
                self.reset_selected_appearance_color_selection();
                return Ok(None);
            }
            KeyCode::Delete => {
                self.reset_selected_appearance_color_selection();
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

    pub(super) fn reset_selected_appearance_color_selection(&mut self) {
        let Some(config_state) = self.config_state.as_mut() else {
            return;
        };
        if config_state.footer_selected {
            return;
        }
        match config_state.selected_field {
            Some(ConfigField::AppearanceColorGroup) => {
                let group = config_state.draft.selected_appearance_color_group();
                let removed = config_state
                    .draft
                    .reset_selected_appearance_color_group_overrides();
                if removed > 0 {
                    config_state.dirty = true;
                    self.last_notice =
                        Some(format!("reset {removed} color overrides in {}", group.key));
                } else {
                    self.last_notice = Some(format!("color group {} already inherits", group.key));
                }
            }
            Some(ConfigField::AppearanceColorToken | ConfigField::AppearanceColorOverride) => {
                let token = config_state.draft.selected_appearance_color_token();
                if config_state
                    .draft
                    .reset_selected_appearance_color_override()
                {
                    config_state.dirty = true;
                    self.last_notice = Some(format!("reset color {token}"));
                } else {
                    self.last_notice = Some(format!("color {token} already inherits"));
                }
            }
            _ => {}
        }
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
        let changed = if field == ConfigField::AppearanceColorOverride {
            match config_state
                .draft
                .set_selected_appearance_color_override(value)
            {
                Ok(changed) => changed,
                Err(error) => {
                    self.last_notice = Some(format!("invalid color override: {error}"));
                    return;
                }
            }
        } else {
            let Some(target) = config_state.field_text_value_mut(field) else {
                return;
            };
            let changed = *target != value;
            *target = value;
            changed
        };
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
        self.refresh_mutation_artifact_retention_preview();
        self.last_notice = Some("opened config".to_owned());
        self.push_event("mode", "config");
    }

    pub(super) fn refresh_mutation_artifact_retention_preview(&mut self) {
        let Some(root_config) = self.config_snapshot.as_ref() else {
            self.runtime.mutation_artifact_retention_preview =
                MutationArtifactRetentionPreview::Unavailable("config is unavailable".to_owned());
            return;
        };
        let store = match JsonlSessionStore::new(&self.session_log_path) {
            Ok(store) => store,
            Err(error) => {
                self.runtime.mutation_artifact_retention_preview =
                    MutationArtifactRetentionPreview::Unavailable(format!(
                        "failed to open mutation artifact recorder: {error:#}"
                    ));
                return;
            }
        };
        let recorder = MutationEventRecorder::new(store);
        self.runtime.mutation_artifact_retention_preview = match recorder.preview_artifact_cleanup(
            &sigil_kernel::MutationArtifactCleanupTarget::Recommended,
            &root_config.storage.mutation_artifact_retention.to_policy(),
        ) {
            Ok(report) => match recorder.list_mutation_artifacts() {
                Ok(artifacts) => {
                    if let Some(config_state) = self.config_state.as_mut() {
                        config_state.selected_storage_artifact_index = config_state
                            .selected_storage_artifact_index
                            .min(artifacts.len().saturating_sub(1));
                    }
                    MutationArtifactRetentionPreview::Ready { report, artifacts }
                }
                Err(error) => MutationArtifactRetentionPreview::Unavailable(format!(
                    "failed to list mutation artifacts: {error:#}"
                )),
            },
            Err(error) => MutationArtifactRetentionPreview::Unavailable(format!(
                "failed to preview mutation artifacts: {error:#}"
            )),
        };
    }

    fn mutation_artifact_inventory_count(&self) -> usize {
        match &self.runtime.mutation_artifact_retention_preview {
            MutationArtifactRetentionPreview::Ready { artifacts, .. } => artifacts.len(),
            MutationArtifactRetentionPreview::Pending
            | MutationArtifactRetentionPreview::Unavailable(_) => 0,
        }
    }

    fn clean_selected_mutation_artifacts(&mut self) -> Result<Option<AppAction>> {
        self.last_notice = Some("cleaning recommended mutation artifacts".to_owned());
        Ok(Some(AppAction::CleanMutationArtifacts {
            target: sigil_kernel::MutationArtifactCleanupTarget::Recommended,
        }))
    }

    fn repo_verification_candidates(
        &self,
        config_state: &ConfigState,
    ) -> Result<Vec<DiscoveredCheck>> {
        let (_, _, trust_snapshot_id) = self.verification_trust_context()?;
        let discovered = discover_candidate_checks_with_user_config(
            &self.workspace_root,
            trust_snapshot_id,
            "config-preview",
            &config_state.draft.base_root_config.verification,
        )?;
        Ok(discovered
            .into_iter()
            .filter(|check| check.candidate.source.requires_trust_promotion())
            .collect())
    }

    fn verification_trust_context(&self) -> Result<(String, WorkspaceTrust, String)> {
        let projection =
            VerificationStateProjection::from_entries(&self.session_browser.current_entries);
        let workspace_id = stable_workspace_id(&self.workspace_root)?;
        let trust_entry = projection.workspace_trust.get(&workspace_id);
        let trust = trust_entry
            .map(|entry| entry.trust)
            .unwrap_or(WorkspaceTrust::Unknown);
        let trust_snapshot_id = trust_entry
            .map(|entry| entry.workspace_trust_snapshot_id.clone())
            .unwrap_or_else(|| "unknown".to_owned());
        Ok((workspace_id, trust, trust_snapshot_id))
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
        match sigil_runtime::discover_skill_index_with_project_assets_root(
            &self.workspace_root,
            &self.sigil_paths.project_assets_root,
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
            &self.session_browser.current_entries,
        ) {
            Ok(registry) => (registry.profiles().to_vec(), registry.warnings().to_vec()),
            Err(error) => (Vec::new(), vec![format!("agent discovery failed: {error}")]),
        }
    }

    fn discover_config_plugins(&self) -> (Vec<PluginManifestSnapshot>, Vec<String>) {
        let projection = PluginStateProjection::from_entries(&self.session_browser.current_entries);
        let trust_entries = projection
            .trust_entries
            .into_values()
            .collect::<Vec<PluginTrustEntry>>();
        match sigil_runtime::discover_workspace_plugins_with_project_assets_root(
            &self.workspace_root,
            &self.sigil_paths.project_assets_root,
            &trust_entries,
        ) {
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

    fn open_selected_skill_arguments(&mut self) -> Result<Option<AppAction>> {
        if self.runtime.is_busy {
            self.last_notice = Some("busy; use skill later".to_owned());
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
        if self.runtime.is_busy {
            self.last_notice = Some("busy; use skill later".to_owned());
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
        self.last_notice = Some(format!("using {item_kind} {skill_id}"));
        self.push_event("skill", format!("use {skill_id}"));
        Ok(Some(AppAction::SubmitPrompt(prompt)))
    }

    fn review_selected_agent(&mut self, decision: AgentTrustState) -> Result<Option<AppAction>> {
        if self.runtime.is_busy {
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
            AgentTrustState::Disabled => "disabled",
            AgentTrustState::NeedsReview => "reviewed",
            AgentTrustState::Unknown => "reviewed",
        };
        self.last_notice = Some(format!("agent {} {action}", agent.profile.id.as_str()));
        self.push_event("agent", format!("{} {action}", agent.profile.id.as_str()));
        Ok(None)
    }

    #[cfg(test)]
    fn toggle_selected_agent_enabled(&mut self) -> Result<Option<AppAction>> {
        self.update_selected_agent_policy(AgentPolicyToggle::Enabled)
    }

    #[cfg(test)]
    fn toggle_selected_agent_user(&mut self) -> Result<Option<AppAction>> {
        self.update_selected_agent_policy(AgentPolicyToggle::UserInvocable)
    }

    #[cfg(test)]
    fn toggle_selected_agent_model(&mut self) -> Result<Option<AppAction>> {
        self.update_selected_agent_policy(AgentPolicyToggle::ModelInvocable)
    }

    #[cfg(test)]
    fn update_selected_agent_policy(
        &mut self,
        toggle: AgentPolicyToggle,
    ) -> Result<Option<AppAction>> {
        if self.runtime.is_busy {
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
            &self.session_browser.current_entries,
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
            &self.session_browser.current_entries,
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
        if self.runtime.is_busy {
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
        let trust = PluginTrustEntry::for_snapshot(&plugin, decision, unix_time_ms())?;
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
        if self.session_browser.current_entries.iter().any(|entry| {
            matches!(
                entry,
                SessionLogEntry::Control(ControlEntry::SessionIdentity { .. })
            )
        }) {
            return Ok(());
        }
        self.append_control_to_current_session(ControlEntry::SessionIdentity {
            provider_name: self.runtime.provider_name.clone(),
            model_name: self.runtime.model_name.clone(),
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
        if self.runtime.is_busy {
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
        if self.runtime.is_busy {
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
        let server_name = server.name.clone();
        let current_status = self
            .runtime
            .mcp_server_statuses
            .get(&server_name)
            .cloned()
            .unwrap_or_else(|| initial_mcp_server_status(server));
        if server.startup == McpServerStartup::Lazy
            && matches!(current_status, McpServerRuntimeStatus::Deferred)
        {
            self.runtime
                .mcp_server_statuses
                .insert(server_name.clone(), McpServerRuntimeStatus::Activating);
            self.last_notice = Some(format!("activating MCP {server_name}"));
            self.push_event("mcp", format!("activate {server_name}"));
            return Ok(Some(AppAction::ActivateLazyMcp {
                server_name: Some(server_name),
            }));
        }

        self.runtime
            .mcp_server_statuses
            .insert(server_name.clone(), McpServerRuntimeStatus::Refreshing);
        self.last_notice = Some(format!("refreshing MCP {server_name}"));
        self.push_event("mcp", format!("refresh {server_name}"));
        Ok(Some(AppAction::RefreshMcpServer { server_name }))
    }

    pub(super) fn apply_runtime_config_snapshot(&mut self, root_config: &RootConfig) {
        let appearance_changed = self
            .config_snapshot
            .as_ref()
            .is_some_and(|snapshot| snapshot.appearance != root_config.appearance);
        let sigil_paths = sigil_runtime::resolve_sigil_paths(
            &root_config.storage,
            &root_config.session,
            &self.workspace_root,
        );
        self.sigil_paths = sigil_paths;
        self.session_log_dir = self.sigil_paths.session_log_dir.clone();
        self.config_snapshot = Some(root_config.clone());
        self.secret_redactor = sigil_runtime::secret_redactor_for_root_config(root_config);
        self.runtime.permission_default_mode =
            root_config.permission.default_mode.as_str().to_owned();
        self.memory_config = root_config.memory.clone();
        self.compaction_config = root_config.compaction.clone();
        self.refresh_session_view_cache();
        self.runtime.code_intelligence_status =
            code_intelligence_config_status(&root_config.code_intelligence);
        self.runtime.code_intelligence_server_lines.clear();
        self.runtime.code_intelligence_diagnostics_line = None;
        self.runtime.code_intelligence_diagnostics_by_path.clear();
        self.runtime.mcp_server_statuses = initial_mcp_server_statuses(root_config);
        if self.session_browser.current_entries.is_empty() {
            self.runtime.provider_name = root_config.agent.provider.clone();
            self.runtime.model_name = root_config.agent.model.clone();
        }
        self.refresh_memory_summary();
        self.load_input_history();
        self.recompute_compaction_status(false);
        self.refresh_mutation_artifact_retention_preview();
        self.refresh_usage_sidebar_cache();
        self.refresh_session_history();
        let (agents, agent_warnings) = self.discover_config_agents(root_config);
        let (skills, warnings) = self.discover_config_skills(root_config);
        let (plugins, plugin_warnings) = self.discover_config_plugins();
        if let Some(config_state) = self.config_state.as_mut() {
            config_state.set_agent_discovery(agents, agent_warnings);
            config_state.set_skill_discovery(skills, warnings);
            config_state.set_plugin_discovery(plugins, plugin_warnings);
        }
        if appearance_changed {
            self.rebuild_timeline_render_cache();
        }
    }

    #[cfg(test)]
    pub(crate) fn mcp_server_runtime_status_label(&self, server_name: &str) -> Option<String> {
        self.runtime
            .mcp_server_statuses
            .get(server_name)
            .map(|status| status.label_for_server(Some(server_name)))
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
                    .runtime
                    .mcp_server_statuses
                    .get(&server.name)
                    .cloned()
                    .unwrap_or_else(|| initial_mcp_server_status(server));
                format!(
                    "{}: {}",
                    server.name,
                    status.label_for_server(Some(&server.name))
                )
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
        self.runtime
            .mcp_server_statuses
            .get(&config.name)
            .cloned()
            .unwrap_or_else(|| initial_mcp_server_status(config))
            .label_for_server(Some(&config.name))
    }

    pub(super) fn selected_mcp_boundary_label(&self, config_state: &ConfigState) -> Option<String> {
        let root_config = &config_state.draft.base_root_config;
        let server = root_config
            .mcp_servers
            .get(config_state.selected_mcp_server_index)?;
        Some(sigil_runtime::mcp_stdio_boundary_summary(
            root_config,
            &self.workspace_root,
            server,
        ))
    }

    fn render_code_intelligence_readiness_summary(
        &self,
        config_state: &ConfigState,
    ) -> Vec<String> {
        let root_config = config_state.draft.code_intelligence_preview_root_config();
        let checks = build_code_intelligence_checks(&root_config, &self.workspace_root);
        let mut lines = vec![
            render_config_readonly_row("Saved runtime", &self.runtime.code_intelligence_status),
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

#[cfg(test)]
#[derive(Debug, Clone, Copy)]
enum AgentPolicyToggle {
    Enabled,
    UserInvocable,
    ModelInvocable,
}

#[cfg(test)]
fn policy_override(target: bool, source: bool) -> Option<bool> {
    (target != source).then_some(target)
}

fn move_config_collection_selection(
    config_state: &mut ConfigState,
    forward: bool,
    storage_artifact_count: usize,
) -> Option<ConfigFieldMove> {
    match config_state.selected_section {
        ConfigSection::Agents => Some(config_state.move_agent(forward)),
        ConfigSection::Skills => Some(config_state.move_skill(forward)),
        ConfigSection::Plugins => Some(config_state.move_plugin(forward)),
        ConfigSection::Storage => Some(move_storage_artifact_selection(
            config_state,
            forward,
            storage_artifact_count,
        )),
        _ => None,
    }
}

fn move_storage_artifact_selection(
    config_state: &mut ConfigState,
    forward: bool,
    artifact_count: usize,
) -> ConfigFieldMove {
    if artifact_count == 0 {
        return ConfigFieldMove::Unavailable;
    }
    let current = config_state
        .selected_storage_artifact_index
        .min(artifact_count.saturating_sub(1));
    let next = if forward {
        if current + 1 >= artifact_count {
            return ConfigFieldMove::Boundary;
        }
        current + 1
    } else {
        if current == 0 {
            return ConfigFieldMove::Boundary;
        }
        current - 1
    };
    config_state.selected_storage_artifact_index = next;
    ConfigFieldMove::Moved
}

fn config_collection_selection_notice(
    config_state: &ConfigState,
    storage_artifact_count: usize,
) -> Option<String> {
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
        ConfigSection::Storage if storage_artifact_count > 0 => Some(format!(
            "artifact {}/{}",
            config_state.selected_storage_artifact_index + 1,
            storage_artifact_count
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
        return "Selected agent profile. Up/Down moves through agents; footer actions trust or disable it.";
    } else if matches!(field, ConfigField::SkillId)
        && let Some(skill) = config_state.selected_skill()
        && skill_is_agent(skill)
    {
        return "Selected child-session agent. Up/Down moves through agents; footer action uses it.";
    } else if matches!(field, ConfigField::SkillId)
        && config_state.selected_section == ConfigSection::Agents
    {
        return "Selected child-session agent. Up/Down moves through agents; footer action uses it.";
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
            lines.push("mcp: PgUp/PgDn server · footer activate/refresh".to_owned());
        } else if config_state.selected_section == ConfigSection::Agents {
            lines.push("agents: Up/Down agent · PgUp/PgDn wrap · footer trust/disable".to_owned());
        } else if config_state.selected_section == ConfigSection::Skills {
            lines.push("skills: Up/Down skill · PgUp/PgDn wrap · footer use".to_owned());
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
    if matches!(field, ConfigField::AppearanceSyntaxTheme) {
        lines.push("appearance: auto follows the selected TUI theme for code blocks".to_owned());
    }
    if matches!(field, ConfigField::AppearanceColorGroup) {
        lines.push("advanced: color token groups are edited in sigil.toml".to_owned());
    }
    if matches!(field, ConfigField::AppearanceColorToken) {
        lines.push("advanced: color token selection is edited in sigil.toml".to_owned());
    }
    if matches!(field, ConfigField::AppearanceColorOverride) {
        lines.push("advanced: color overrides are edited in sigil.toml".to_owned());
    }
    if config_state.selected_section == ConfigSection::Mcp {
        lines.push("mcp: PgUp/PgDn server · footer activate/refresh".to_owned());
    } else if config_state.selected_section == ConfigSection::Agents {
        lines.push("agents: Up/Down agent · PgUp/PgDn wrap · footer trust/disable".to_owned());
    } else if config_state.selected_section == ConfigSection::Skills {
        lines.push("skills: Up/Down skill · PgUp/PgDn wrap · footer use".to_owned());
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
    lines.extend(render_plugin_agent_lines(&plugin.capabilities));
    lines.extend(render_plugin_skill_lines(&plugin.capabilities));
    lines.extend(render_plugin_hook_lines(&plugin.capabilities));
    lines.extend(render_plugin_mcp_lines(&plugin.capabilities));
    lines.push(render_config_readonly_row(
        "Approve",
        "trusts this reviewed manifest",
    ));
    lines.push(render_config_readonly_row(
        "Deny",
        "disables this reviewed manifest",
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
            "Use",
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
        render_config_readonly_row("Aliases", &list_summary(&agent.profile.aliases)),
        render_config_readonly_row("Slash", &agent_slash_name_summary(agent)),
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

fn agent_slash_name_summary(agent: &ResolvedAgentProfile) -> String {
    if agent.profile.slash_names.is_empty() {
        return "none".to_owned();
    }
    agent
        .profile
        .slash_names
        .iter()
        .map(|name| format!("/{name}"))
        .collect::<Vec<_>>()
        .join(",")
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
        .any(|capability| matches!(capability, PluginCapability::Agent { .. }))
    {
        parts.push("agent profiles");
    }
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

fn render_plugin_agent_lines(capabilities: &[PluginCapability]) -> Vec<String> {
    let agents = capabilities
        .iter()
        .filter_map(|capability| match capability {
            PluginCapability::Agent { path } => Some(path.display().to_string()),
            _ => None,
        })
        .collect::<Vec<_>>();
    let mut lines = vec![String::new(), "[agents]".to_owned()];
    if agents.is_empty() {
        lines.push(render_config_readonly_row("Agent count", "0"));
        return lines;
    }
    for (index, path) in agents.iter().enumerate() {
        push_wrapped_readonly_rows(&mut lines, &format!("Agent {}", index + 1), path);
    }
    lines
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
                hook_kind,
                declared_effect,
                ..
            } => Some((*hook_kind, *declared_effect)),
            _ => None,
        })
        .collect::<Vec<_>>();
    let mut lines = vec![String::new(), "[hooks]".to_owned()];
    if hooks.is_empty() {
        lines.push(render_config_readonly_row("Hook count", "0"));
        return lines;
    }
    lines.push(render_config_readonly_row(
        "Hook count",
        &hooks.len().to_string(),
    ));
    lines.push(render_config_readonly_row(
        "Hook kinds",
        &plugin_hook_kind_summary(&hooks),
    ));
    lines.push(render_config_readonly_row(
        "Hook effects",
        &plugin_hook_effect_summary(&hooks),
    ));
    lines.push(render_config_readonly_row(
        "Runtime",
        "trusted hooks run through execution backend",
    ));
    lines.push(render_config_readonly_row(
        "Evidence",
        "mutating hooks record workspace evidence",
    ));
    lines.push(render_config_readonly_row(
        "Inspect",
        "run /doctor for command and issue details",
    ));
    lines
}

fn plugin_hook_kind_summary(hooks: &[(sigil_kernel::PluginHookKind, ToolEffect)]) -> String {
    let mut context = 0;
    let mut compaction = 0;
    let mut verification = 0;
    let mut event = 0;
    for (kind, _) in hooks {
        match kind {
            sigil_kernel::PluginHookKind::Context => context += 1,
            sigil_kernel::PluginHookKind::Compaction => compaction += 1,
            sigil_kernel::PluginHookKind::Verification => verification += 1,
            sigil_kernel::PluginHookKind::Event => event += 1,
        }
    }
    format!("context={context} compaction={compaction} verification={verification} event={event}")
}

fn plugin_hook_effect_summary(hooks: &[(sigil_kernel::PluginHookKind, ToolEffect)]) -> String {
    let mut read_only = 0;
    let mut workspace_write = 0;
    let mut external_write = 0;
    let mut network = 0;
    let mut unknown = 0;
    for (_, effect) in hooks {
        match effect {
            ToolEffect::ReadOnly => read_only += 1,
            ToolEffect::WorkspaceWrite => workspace_write += 1,
            ToolEffect::ExternalWrite => external_write += 1,
            ToolEffect::Network => network += 1,
            ToolEffect::Unknown => unknown += 1,
        }
    }
    format!(
        "read_only={read_only} workspace_write={workspace_write} external_write={external_write} network={network} unknown={unknown}"
    )
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
                approval,
                egress_logging,
                allow_secrets,
            } => Some((
                name,
                command,
                args,
                startup,
                *required,
                approval,
                egress_logging,
                allow_secrets,
            )),
            _ => None,
        })
        .collect::<Vec<_>>();
    let mut lines = vec![String::new(), "[mcp servers]".to_owned()];
    if servers.is_empty() {
        lines.push(render_config_readonly_row("MCP count", "0"));
        return lines;
    }
    for (
        index,
        (name, command, args, startup, required, approval, egress_logging, allow_secrets),
    ) in servers.iter().enumerate()
    {
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
        lines.push(render_config_readonly_row(
            &format!("{label} policy"),
            &plugin_capability_policy_summary(approval, **egress_logging, **allow_secrets),
        ));
    }
    lines
}

fn plugin_capability_policy_summary(
    approval: &ApprovalMode,
    egress_logging: bool,
    allow_secrets: bool,
) -> String {
    format!(
        "approval={} egress={} secrets={}",
        approval.as_str(),
        bool_summary(egress_logging),
        secrets_summary(allow_secrets)
    )
}

fn secrets_summary(allow_secrets: bool) -> &'static str {
    if allow_secrets { "allowed" } else { "blocked" }
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

fn render_mutation_artifact_retention_summary(
    config_state: &ConfigState,
    preview: &MutationArtifactRetentionPreview,
) -> Vec<String> {
    let retention = &config_state
        .draft
        .base_root_config
        .storage
        .mutation_artifact_retention;
    let mut lines = vec![
        render_config_readonly_row(
            "Max artifacts",
            &optional_count_summary(retention.max_artifacts),
        ),
        render_config_readonly_row("Max bytes", &optional_bytes_summary(retention.max_bytes)),
        render_config_readonly_row(
            "Expire older than",
            &optional_duration_ms_summary(retention.expire_older_than_ms),
        ),
    ];
    match preview {
        MutationArtifactRetentionPreview::Pending => {
            lines.push(render_config_readonly_row("Preview", "pending"));
        }
        MutationArtifactRetentionPreview::Ready { report, artifacts } => {
            lines.push(render_config_readonly_row(
                "Current artifacts",
                &format!(
                    "{} ({})",
                    report.scanned_artifacts,
                    optional_bytes_summary(Some(report.scanned_bytes))
                ),
            ));
            lines.push(render_config_readonly_row(
                "Cleanup preview",
                &format!(
                    "expire {}, delete {}, unavailable {}",
                    report.expired_artifacts,
                    report.deleted_artifacts,
                    report.unavailable_artifacts
                ),
            ));
            if report.has_cleanup_candidates() {
                lines.push(render_config_readonly_row(
                    "Maintenance",
                    &format!(
                        "clean recommended ({} artifacts, {})",
                        report.cleanup_candidate_artifacts(),
                        optional_bytes_summary(Some(report.cleanup_candidate_bytes()))
                    ),
                ));
            }
            lines.push(render_config_readonly_row(
                "Cleanup bytes",
                &format!(
                    "expire {}, delete {}",
                    optional_bytes_summary(Some(report.expired_bytes)),
                    optional_bytes_summary(Some(report.deleted_bytes))
                ),
            ));
            lines.extend(render_mutation_artifact_inventory_summary(
                artifacts,
                config_state.selected_storage_artifact_index,
            ));
            lines.extend(render_selected_mutation_artifact_detail(
                artifacts,
                config_state.selected_storage_artifact_index,
            ));
        }
        MutationArtifactRetentionPreview::Unavailable(error) => {
            lines.push(render_config_readonly_row("Preview", "unavailable"));
            lines.push(render_config_hint_row(&truncate_config_detail(error, 72)));
        }
    }
    lines
}

const WORKSPACE_INSTRUCTION_FILES: &[&str] =
    &["SIGIL.md", "AGENTS.md", "CLAUDE.md", "SIGIL.local.md"];

fn workspace_instruction_files(workspace_root: &Path) -> Vec<PathBuf> {
    WORKSPACE_INSTRUCTION_FILES
        .iter()
        .map(|file| workspace_root.join(file))
        .filter(|path| path.is_file())
        .map(|path| {
            path.strip_prefix(workspace_root)
                .map(Path::to_path_buf)
                .unwrap_or(path)
        })
        .collect()
}

fn repo_instruction_trust_summary(count: usize, trust: WorkspaceTrust) -> String {
    let label = repo_instruction_trust_label(trust);
    if count == 1 {
        format!("1 file · {label}")
    } else {
        format!("{count} files · {label}")
    }
}

fn repo_verification_candidate_summary(count: usize, trust: WorkspaceTrust) -> String {
    if count == 0 {
        return "none found".to_owned();
    }
    let policy = if trust == WorkspaceTrust::Trusted {
        "available to task checks"
    } else {
        "review required"
    };
    format!("{count} found · {policy}")
}

fn repo_instruction_trust_label(trust: WorkspaceTrust) -> &'static str {
    match trust {
        WorkspaceTrust::Trusted => "trusted instructions",
        WorkspaceTrust::Unknown | WorkspaceTrust::Restricted | WorkspaceTrust::Denied => {
            "untrusted data"
        }
    }
}

fn render_mutation_artifact_inventory_summary(
    artifacts: &[sigil_kernel::MutationArtifactInventoryItem],
    selected_index: usize,
) -> Vec<String> {
    const MAX_ARTIFACT_ROWS: usize = 3;
    if artifacts.is_empty() {
        return vec![render_config_hint_row("No mutation artifacts found")];
    }
    let mut lines = vec!["[artifact list]".to_owned()];
    lines.extend(
        artifacts
            .iter()
            .enumerate()
            .take(MAX_ARTIFACT_ROWS)
            .map(|(index, artifact)| {
                render_mutation_artifact_inventory_row(artifact, index == selected_index)
            }),
    );
    let hidden = artifacts.len().saturating_sub(MAX_ARTIFACT_ROWS);
    if hidden > 0 {
        lines.push(format!("... {hidden} more mutation artifacts"));
    }
    lines
}

fn render_mutation_artifact_inventory_row(
    artifact: &sigil_kernel::MutationArtifactInventoryItem,
    selected: bool,
) -> String {
    let source = artifact_source_summary(artifact);
    let status = if artifact.blob_available {
        "available"
    } else {
        "unavailable"
    };
    let marker = if selected { ">" } else { "-" };
    format!(
        "{marker} {} · {} · {}",
        source,
        optional_bytes_summary(Some(artifact.size)),
        status
    )
}

fn render_selected_mutation_artifact_detail(
    artifacts: &[sigil_kernel::MutationArtifactInventoryItem],
    selected_index: usize,
) -> Vec<String> {
    let Some(artifact) = artifacts.get(selected_index.min(artifacts.len().saturating_sub(1)))
    else {
        return Vec::new();
    };
    let availability = if artifact.blob_available {
        "available"
    } else {
        "unavailable"
    };
    let mut lines = vec![
        String::new(),
        "[selected artifact]".to_owned(),
        render_config_readonly_row(
            "Selected",
            &format!(
                "{} of {}",
                selected_index.min(artifacts.len().saturating_sub(1)) + 1,
                artifacts.len()
            ),
        ),
        render_config_readonly_row("Size", &optional_bytes_summary(Some(artifact.size))),
        render_config_readonly_row("Availability", availability),
        render_config_readonly_row(
            "Restore impact",
            if artifact.blob_available {
                "snapshot content available"
            } else {
                "snapshot content unavailable"
            },
        ),
    ];
    if artifact.source_paths.is_empty() {
        lines.push(render_config_readonly_row("Source count", "0"));
    } else {
        for (index, source_path) in artifact.source_paths.iter().take(3).enumerate() {
            push_wrapped_readonly_rows(
                &mut lines,
                &format!("Source {}", index + 1),
                &source_path.display().to_string(),
            );
        }
        if artifact.source_paths.len() > 3 {
            lines.push(format!(
                "... {} more artifact sources",
                artifact.source_paths.len() - 3
            ));
        }
    }
    lines
}

fn artifact_source_summary(artifact: &sigil_kernel::MutationArtifactInventoryItem) -> String {
    let Some(first) = artifact.source_paths.first() else {
        return "unknown source".to_owned();
    };
    let first = truncate_config_detail(&first.display().to_string(), 28);
    let hidden = artifact.source_paths.len().saturating_sub(1);
    if hidden == 0 {
        first
    } else {
        format!("{first} +{hidden}")
    }
}

fn optional_count_summary(value: Option<usize>) -> String {
    value
        .map(|count| count.to_string())
        .unwrap_or_else(|| "unlimited".to_owned())
}

fn optional_bytes_summary(value: Option<u64>) -> String {
    let Some(bytes) = value else {
        return "unlimited".to_owned();
    };
    const GIB: u64 = 1024 * 1024 * 1024;
    const MIB: u64 = 1024 * 1024;
    if bytes >= GIB && bytes % GIB == 0 {
        return format!("{} GiB", bytes / GIB);
    }
    if bytes >= MIB && bytes % MIB == 0 {
        return format!("{} MiB", bytes / MIB);
    }
    format!("{bytes} bytes")
}

fn optional_duration_ms_summary(value: Option<u64>) -> String {
    let Some(ms) = value else {
        return "never".to_owned();
    };
    const DAY_MS: u64 = 24 * 60 * 60 * 1000;
    const HOUR_MS: u64 = 60 * 60 * 1000;
    const MINUTE_MS: u64 = 60 * 1000;
    if ms >= DAY_MS && ms % DAY_MS == 0 {
        let days = ms / DAY_MS;
        return format!("{} {}", days, if days == 1 { "day" } else { "days" });
    }
    if ms >= HOUR_MS && ms % HOUR_MS == 0 {
        let hours = ms / HOUR_MS;
        return format!("{} {}", hours, if hours == 1 { "hour" } else { "hours" });
    }
    if ms >= MINUTE_MS && ms % MINUTE_MS == 0 {
        let minutes = ms / MINUTE_MS;
        return format!(
            "{} {}",
            minutes,
            if minutes == 1 { "minute" } else { "minutes" }
        );
    }
    format!("{ms} ms")
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

fn workspace_trust_label(trust: WorkspaceTrust) -> &'static str {
    match trust {
        WorkspaceTrust::Unknown => "unknown",
        WorkspaceTrust::Trusted => "trusted",
        WorkspaceTrust::Restricted => "restricted",
        WorkspaceTrust::Denied => "denied",
    }
}

#[cfg(test)]
pub(super) fn repo_check_promotion_requirement(effect: sigil_kernel::ToolEffect) -> &'static str {
    if effect.may_mutate_workspace() {
        "workspace-trust/approval+rerun-readonly-check"
    } else {
        "workspace-trust/approval"
    }
}

fn truncate_config_detail(value: &str, max_chars: usize) -> String {
    if value.chars().count() <= max_chars {
        return value.to_owned();
    }
    if max_chars <= 3 {
        return value.chars().take(max_chars).collect();
    }
    let prefix = value.chars().take(max_chars - 3).collect::<String>();
    format!("{prefix}...")
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

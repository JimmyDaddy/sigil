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
    JsonlSessionStore, McpServerConfig, McpServerStartup, MutationEventRecorder, PermissionMode,
    PluginCapability, PluginManifestSnapshot, PluginStateProjection, PluginTrustDecision,
    PluginTrustEntry, RootConfig, SessionLogEntry, SkillDescriptor, SkillRunMode, SkillSource,
    SkillTrustState, SyntaxThemeId, ThemeId, ToolEffect, ToolRegistryScope,
    VerificationStateProjection, WebSearchRoute, WorkspaceTrust, default_user_config_dir,
    discover_candidate_checks_with_user_config, stable_workspace_id,
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

mod agent_detail;
mod agents;
mod appearance;
mod code_intel_detail;
mod code_intelligence_section;
mod compaction;
mod detail;
mod mcp;
mod mcp_detail;
mod memory;
mod navigation;
mod permission_detail;
mod permissions;
mod plugin_detail;
mod plugins;
mod provider;
mod shared;
mod skill_detail;
mod skills;
mod storage;
mod storage_detail;
mod terminal;
mod verification;
mod verification_detail;
mod web;

use agent_detail::*;
use code_intel_detail::*;
use detail::*;
use mcp_detail::*;
use navigation::*;
use permission_detail::*;
use plugin_detail::*;
#[cfg(test)]
pub(super) use shared::cycle_approval_mode;
use shared::*;
use skill_detail::*;
use storage_detail::*;
#[cfg(test)]
pub(crate) use verification_detail::repo_check_promotion_requirement;
use verification_detail::*;

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
        navigation::render_config_nav_lines(self.config_state.as_ref())
    }

    pub fn config_detail_lines(&self) -> Vec<String> {
        let Some(config_state) = &self.config_state else {
            return Vec::new();
        };
        let section = config_state.selected_section;
        let mut lines = render_config_detail_header(config_state);

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
            ConfigSection::Web => {
                web::render_section(&mut lines, config_state);
            }
            ConfigSection::Memory => {
                memory::render_section(self, &mut lines, config_state);
            }
            ConfigSection::Compaction => {
                compaction::render_section(self, &mut lines, config_state);
            }
            ConfigSection::CodeIntelligence => {
                code_intelligence_section::render_section(self, &mut lines, config_state);
            }
            ConfigSection::Terminal => {
                terminal::render_section(&mut lines, config_state);
            }
            ConfigSection::Appearance => {
                appearance::render_section(&mut lines, config_state);
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
                if self
                    .config_state
                    .as_ref()
                    .is_some_and(|config_state| config_state.selected_section == ConfigSection::Mcp)
                {
                    self.cycle_selected_mcp_server(false);
                } else if let Some(config_state) = self.config_state.as_mut() {
                    match config_state.selected_section {
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
                        ConfigSection::Mcp => {}
                        _ => {}
                    }
                }
            }
            KeyCode::PageDown => {
                if self
                    .config_state
                    .as_ref()
                    .is_some_and(|config_state| config_state.selected_section == ConfigSection::Mcp)
                {
                    self.cycle_selected_mcp_server(true);
                } else if let Some(config_state) = self.config_state.as_mut() {
                    match config_state.selected_section {
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
                        ConfigSection::Mcp => {}
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
                if let Some(config_state) = self.config_state.as_ref()
                    && config_state.selected_section == ConfigSection::Mcp
                {
                    if config_state.selected_field == Some(ConfigField::McpName) {
                        self.cycle_selected_mcp_server(true);
                    } else {
                        self.last_notice = Some("no MCP server selected".to_owned());
                    }
                    return Ok(None);
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
                        ConfigField::PermissionMode => {
                            config_state.draft.permission_mode =
                                cycle_permission_mode(config_state.draft.permission_mode);
                            config_state.dirty = true;
                            self.last_notice = Some(format!("updated {}", field.label()));
                            return Ok(None);
                        }
                        ConfigField::WebEnabled => {
                            config_state.draft.web_enabled = !config_state.draft.web_enabled;
                            config_state.dirty = true;
                            self.last_notice = Some(format!("updated {}", field.label()));
                            return Ok(None);
                        }
                        ConfigField::WebNetworkMode => {
                            config_state.draft.web_network_mode =
                                cycle_network_policy(config_state.draft.web_network_mode);
                            config_state.dirty = true;
                            self.last_notice = Some(format!("updated {}", field.label()));
                            return Ok(None);
                        }
                        ConfigField::WebSearchRoute => {
                            config_state.draft.web_search_route =
                                cycle_web_search_route(config_state.draft.web_search_route);
                            config_state.dirty = true;
                            self.last_notice = Some(format!("updated {}", field.label()));
                            return Ok(None);
                        }
                        ConfigField::WebBundledSearchEnabled => {
                            config_state.draft.web_bundled_search_enabled =
                                !config_state.draft.web_bundled_search_enabled;
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
                        ConfigField::CodeIntelServerStartup => {
                            config_state.draft.code_intelligence_server_startup =
                                cycle_code_intel_startup(
                                    config_state.draft.code_intelligence_server_startup,
                                );
                            config_state.dirty = true;
                            self.last_notice = Some(format!("updated {}", field.label()));
                            return Ok(None);
                        }
                        ConfigField::CodeIntelAutoDiscover => {
                            config_state.draft.code_intelligence_auto_discover =
                                !config_state.draft.code_intelligence_auto_discover;
                            config_state.dirty = true;
                            self.last_notice = Some(format!("updated {}", field.label()));
                            return Ok(None);
                        }
                        ConfigField::CodeIntelReportMissing => {
                            let report_missing =
                                &mut config_state.draft.code_intelligence_report_missing;
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

    fn cycle_selected_mcp_server(&mut self, forward: bool) {
        let Some(config_state) = self.config_state.as_mut() else {
            return;
        };
        if config_state.cycle_mcp_server(forward) {
            self.last_notice = Some(format!(
                "mcp server {}/{}",
                config_state.selected_mcp_server_index + 1,
                config_state.draft.mcp_servers.len()
            ));
        } else {
            self.last_notice = Some("no MCP server to select".to_owned());
        }
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
        match sigil_runtime::discover_workspace_plugins(&self.workspace_root, &trust_entries) {
            Ok(report) => {
                let warnings = report
                    .warnings
                    .into_iter()
                    .map(|warning| {
                        let remediation = warning
                            .remediation
                            .as_deref()
                            .map(|value| format!("; fix: {value}"))
                            .unwrap_or_default();
                        format!(
                            "{} [{}]: {}{}",
                            warning.path.display(),
                            warning.kind.code(),
                            warning.message,
                            remediation
                        )
                    })
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
            AgentPolicyToggle::ModelInvocable => "model_visibility",
        };
        let value = match toggle {
            AgentPolicyToggle::Enabled => bool_summary(target_enabled).to_owned(),
            AgentPolicyToggle::UserInvocable => bool_summary(target_user).to_owned(),
            AgentPolicyToggle::ModelInvocable if target_model => "model allowed".to_owned(),
            AgentPolicyToggle::ModelInvocable => "manual only".to_owned(),
        };
        self.last_notice = Some(format!(
            "agent {} {label}={value}",
            agent.profile.id.as_str()
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
        let Some(is_dirty) = self.config_state.as_ref().map(|state| state.dirty) else {
            return Ok(None);
        };
        if !is_dirty {
            if let Some(config_state) = self.config_state.as_mut() {
                config_state.close_guard_armed = false;
            }
            self.last_notice = Some("saved config".to_owned());
            return Ok(None);
        }
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
        let was_clean = self
            .config_state
            .as_ref()
            .is_some_and(|config_state| !config_state.dirty);
        let action = self.save_config_draft()?;
        if action.is_some() || was_clean {
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
        match current_status {
            McpServerRuntimeStatus::Deferred if server.startup == McpServerStartup::Lazy => {
                self.runtime
                    .mcp_server_statuses
                    .insert(server_name.clone(), McpServerRuntimeStatus::Activating);
                self.last_notice = Some(format!("activating MCP {server_name}"));
                self.push_event("mcp", format!("activate {server_name}"));
                return Ok(Some(AppAction::ActivateLazyMcp {
                    server_name: Some(server_name),
                }));
            }
            McpServerRuntimeStatus::Activating => {
                self.last_notice = Some(format!("MCP {server_name} is already activating"));
                return Ok(None);
            }
            McpServerRuntimeStatus::Refreshing => {
                self.last_notice = Some(format!("MCP {server_name} is already refreshing"));
                return Ok(None);
            }
            McpServerRuntimeStatus::Deferred
            | McpServerRuntimeStatus::Stale { .. }
            | McpServerRuntimeStatus::Ready { .. }
            | McpServerRuntimeStatus::Failed { .. } => {}
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
        self.runtime.permission_mode = root_config.permission.mode.as_str().to_owned();
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
            self.rebuild_timeline_render_store();
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
            .map(|server| format!("{}: {}", server.name, self.mcp_runtime_status_label(server)))
            .collect()
    }

    fn mcp_runtime_status_label(&self, server: &McpServerConfig) -> String {
        self.runtime
            .mcp_server_statuses
            .get(&server.name)
            .cloned()
            .unwrap_or_else(|| initial_mcp_server_status(server))
            .label_for_server(Some(&server.name))
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
        self.mcp_runtime_status_label(config)
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

pub(super) fn cycle_permission_mode(mode: PermissionMode) -> PermissionMode {
    match mode {
        PermissionMode::ReadOnly => PermissionMode::Manual,
        PermissionMode::Manual => PermissionMode::AutoEdit,
        PermissionMode::AutoEdit => PermissionMode::DangerFullAccess,
        PermissionMode::DangerFullAccess => PermissionMode::ReadOnly,
    }
}

pub(super) fn cycle_network_policy(
    policy: sigil_kernel::NetworkPolicy,
) -> sigil_kernel::NetworkPolicy {
    match policy {
        sigil_kernel::NetworkPolicy::Allow => sigil_kernel::NetworkPolicy::Ask,
        sigil_kernel::NetworkPolicy::Ask => sigil_kernel::NetworkPolicy::Deny,
        sigil_kernel::NetworkPolicy::Deny => sigil_kernel::NetworkPolicy::Allow,
    }
}

pub(super) fn cycle_web_search_route(route: WebSearchRoute) -> WebSearchRoute {
    match route {
        WebSearchRoute::Auto => WebSearchRoute::ProviderHosted,
        WebSearchRoute::ProviderHosted => WebSearchRoute::Mcp,
        WebSearchRoute::Mcp => WebSearchRoute::Bundled,
        WebSearchRoute::Bundled => WebSearchRoute::Disabled,
        WebSearchRoute::Disabled => WebSearchRoute::Auto,
    }
}

#[cfg(all(test, not(sigil_tui_test_slice_app_input_flow)))]
#[path = "tests/config_flow_detail_tests.rs"]
mod tests;

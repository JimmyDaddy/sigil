use crate::config_panel::{
    CONFIG_ACTIONS_HINT, CONFIG_CONTROLS_HINT, CONFIG_EDIT_OR_TOGGLE_HINT, CONFIG_FIELD_NAV_HINT,
    CONFIG_SAVE_HINT, CONFIG_SECTION_NAV_HINT, ConfigDraft, ConfigField, ConfigFieldMove,
    ConfigFooterAction, ConfigSection, ConfigState, render_config_readonly_row,
    render_config_value_row,
};
use anyhow::Result;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use sigil_kernel::{ApprovalMode, McpServerConfig, McpServerStartup, RootConfig};
use sigil_provider_deepseek::SIGIL_API_KEY_ENV;

use super::{
    AppAction, AppState, McpServerRuntimeStatus, code_intelligence_config_status,
    formatting::{format_token_count, persisted_root_config},
    initial_mcp_server_status, initial_mcp_server_statuses,
    modal_flow::{
        ModalState, ModelPickerTarget, SecretInputTarget, TextInputState, TextInputTarget,
    },
};
use crate::context_window::{ContextWindowSource, resolve_context_window_tokens};

impl AppState {
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
                state.selected_field.map(ConfigField::display_label)
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
                        } else {
                            config_state.footer_selected = false;
                            self.last_notice = Some(format!(
                                "step {}",
                                config_state.selected_section.title().to_lowercase()
                            ));
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
                        ConfigFooterAction::ActivateMcp => self.activate_selected_mcp_server(),
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

    pub(super) fn open_config_panel(&mut self) {
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
}

pub(super) fn cycle_approval_mode(mode: ApprovalMode) -> ApprovalMode {
    match mode {
        ApprovalMode::Allow => ApprovalMode::Ask,
        ApprovalMode::Ask => ApprovalMode::Deny,
        ApprovalMode::Deny => ApprovalMode::Allow,
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
        &config_state.draft.base_root_config.agent.provider,
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
        }
        return lines;
    };
    let mut lines = vec![
        String::new(),
        "[details]".to_owned(),
        format!("selected: {}", field.display_label()),
        format!("key: {}", field.label()),
        field.help_text().to_owned(),
        String::new(),
        CONFIG_CONTROLS_HINT.to_owned(),
        CONFIG_ACTIONS_HINT.to_owned(),
    ];

    if matches!(field, ConfigField::ProviderApiKey) {
        lines.push(format!("override: {SIGIL_API_KEY_ENV}"));
        lines.push("storage: saved api_key is plaintext in sigil.toml".to_owned());
    }
    if matches!(field, ConfigField::ProviderFimModel) {
        lines.push("advanced: provider-specific fields remain in config file or env".to_owned());
    }
    if config_state.selected_section == ConfigSection::Mcp {
        lines.push("mcp: Ctrl-N add · Ctrl-D drop · PgUp/PgDn server".to_owned());
    }

    lines
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

#[cfg(test)]
#[path = "tests/config_flow_detail_tests.rs"]
mod tests;

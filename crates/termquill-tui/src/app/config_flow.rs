use super::*;

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
                state.selected_field.map(ConfigField::label)
            }
        })
    }

    pub fn config_selected_footer_action_label(&self) -> Option<&'static str> {
        self.config_state.as_ref().and_then(|state| {
            state
                .footer_selected
                .then_some(state.selected_footer_action.button_label())
        })
    }

    pub fn config_footer_hint(&self) -> String {
        if self.config_is_dirty() {
            "draft has unsaved changes".to_owned()
        } else {
            "all changes saved".to_owned()
        }
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
        lines.push("Tab step  Up/Down field".to_owned());
        lines.push("Down footer  Left/Right action".to_owned());
        lines.push("Enter choose/input/toggle/run".to_owned());
        lines.push("Ctrl-S save  Esc close".to_owned());
        lines.push("MCP: Ctrl-N add  Ctrl-D drop".to_owned());
        lines.push("MCP: PgUp/PgDn switch".to_owned());
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
                "{} ({}/{})",
                section.title(),
                index + 1,
                ConfigSection::FLOW.len()
            ),
            None => section.title().to_owned(),
        }];
        lines.push(step_label);
        lines.push(section.summary().to_owned());
        lines.push(
            "Tab step  Up/Down field  Down footer  Left/Right action  Enter open/toggle/run"
                .to_owned(),
        );
        lines.push(String::new());

        match section {
            ConfigSection::Provider => {
                lines.push("[runtime]".to_owned());
                lines.push(render_config_value_row(
                    config_state,
                    ConfigField::ProviderModel,
                ));
                lines.push(render_config_value_row(
                    config_state,
                    ConfigField::ProviderApiKey,
                ));
                lines.push(String::new());
                lines.push("[network]".to_owned());
                lines.push(render_config_value_row(
                    config_state,
                    ConfigField::ProviderBaseUrl,
                ));
                lines.push(render_config_value_row(
                    config_state,
                    ConfigField::ProviderFimModel,
                ));
                lines.push(String::new());
                lines.push("[notes]".to_owned());
                lines.push(format!("auth: file api_key or env {TERMQUILL_API_KEY_ENV}"));
                lines.push("advanced provider fields: config file or env".to_owned());
                lines.push("see README for TERMQUILL_* overrides".to_owned());
            }
            ConfigSection::Permissions => {
                lines.push("[default]".to_owned());
                lines.push(render_config_value_row(
                    config_state,
                    ConfigField::PermissionsDefaultMode,
                ));
                lines.push(String::new());
                lines.push("[rules]".to_owned());
                lines.push(format!(
                    "overrides: {}",
                    config_state.draft.base_root_config.permission.rules.len()
                ));
                if config_state
                    .draft
                    .base_root_config
                    .permission
                    .rules
                    .is_empty()
                {
                    lines.push("no overrides".to_owned());
                } else {
                    for rule in &config_state.draft.base_root_config.permission.rules {
                        lines.push(format!(
                            "- {}  subject={}  mode={}",
                            rule.tool_name.as_deref().unwrap_or("*"),
                            rule.subject_glob.as_deref().unwrap_or("<none>"),
                            rule.mode.as_str()
                        ));
                    }
                }
            }
            ConfigSection::Memory => {
                lines.push("[memory]".to_owned());
                lines.push(render_config_value_row(
                    config_state,
                    ConfigField::MemoryEnabled,
                ));
                lines.push(format!("docs: {}", self.memory_document_count));
                lines.push(format!("status: {}", self.memory_last_status));
                lines.push(
                    "root docs: TERMQUILL.md AGENTS.md CLAUDE.md TERMQUILL.local.md".to_owned(),
                );
            }
            ConfigSection::Compaction => {
                lines.push("[thresholds]".to_owned());
                lines.push(render_config_value_row(
                    config_state,
                    ConfigField::CompactionEnabled,
                ));
                lines.push(render_config_value_row(
                    config_state,
                    ConfigField::CompactionContextWindowTokens,
                ));
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
            }
            ConfigSection::Mcp => {
                lines.push("[servers]".to_owned());
                lines.push(format!("servers: {}", config_state.draft.mcp_servers.len()));
                if config_state.draft.mcp_servers.is_empty() {
                    lines.push("no MCP servers".to_owned());
                    lines.push("Ctrl-N to add".to_owned());
                } else {
                    lines.push(format!(
                        "selected: {}/{}",
                        config_state.selected_mcp_server_index + 1,
                        config_state.draft.mcp_servers.len()
                    ));
                    if config_state.selected_mcp_server().is_some() {
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
                    }
                }
                lines.push(String::new());
                lines.push("Ctrl-N add  Ctrl-D drop  PgUp/PgDn server".to_owned());
                lines.push("args_csv: comma list".to_owned());
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
                    if config_state.footer_selected {
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
                    if config_state.footer_selected {
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

    pub(super) fn apply_runtime_config_snapshot(&mut self, root_config: &RootConfig) {
        self.config_snapshot = Some(root_config.clone());
        self.permission_default_mode = root_config.permission.default_mode.as_str().to_owned();
        self.memory_config = root_config.memory.clone();
        self.compaction_config = root_config.compaction.clone();
        self.code_intelligence_status =
            super::code_intelligence_config_status(&root_config.code_intelligence);
        if self.current_session_entries.is_empty() {
            self.provider_name = root_config.agent.provider.clone();
            self.model_name = root_config.agent.model.clone();
        }
        self.refresh_memory_summary();
        self.recompute_compaction_status(false);
        self.refresh_usage_sidebar_cache();
    }
}

pub(super) fn cycle_approval_mode(mode: ApprovalMode) -> ApprovalMode {
    match mode {
        ApprovalMode::Allow => ApprovalMode::Ask,
        ApprovalMode::Ask => ApprovalMode::Deny,
        ApprovalMode::Deny => ApprovalMode::Allow,
    }
}

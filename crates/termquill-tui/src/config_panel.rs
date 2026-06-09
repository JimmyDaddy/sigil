use anyhow::{Result, anyhow, bail};
use termquill_kernel::{ApprovalMode, McpServerConfig, RootConfig};
use termquill_provider_deepseek::{DeepSeekProviderConfig, StrictToolsMode};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ConfigSection {
    Provider,
    Permissions,
    Memory,
    Compaction,
    Mcp,
}

impl ConfigSection {
    pub(crate) const FLOW: [Self; 5] = [
        Self::Provider,
        Self::Permissions,
        Self::Memory,
        Self::Compaction,
        Self::Mcp,
    ];

    pub(crate) fn title(self) -> &'static str {
        match self {
            Self::Provider => "Provider",
            Self::Permissions => "Permissions",
            Self::Memory => "Memory",
            Self::Compaction => "Compaction",
            Self::Mcp => "MCP",
        }
    }

    pub(crate) fn summary(self) -> &'static str {
        match self {
            Self::Provider => "provider settings",
            Self::Permissions => "approval rules",
            Self::Memory => "memory status",
            Self::Compaction => "thresholds",
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
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ConfigField {
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
    McpName,
    McpCommand,
    McpArgsCsv,
    McpStartupTimeoutSecs,
}

impl ConfigField {
    const PROVIDER_FIELDS: [Self; 4] = [
        Self::ProviderModel,
        Self::ProviderApiKey,
        Self::ProviderBaseUrl,
        Self::ProviderFimModel,
    ];
    const PERMISSION_FIELDS: [Self; 1] = [Self::PermissionsDefaultMode];
    const MEMORY_FIELDS: [Self; 1] = [Self::MemoryEnabled];
    const COMPACTION_FIELDS: [Self; 5] = [
        Self::CompactionEnabled,
        Self::CompactionSoftThresholdRatio,
        Self::CompactionHardThresholdRatio,
        Self::CompactionContextWindowTokens,
        Self::CompactionTailMessages,
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
            ConfigSection::Mcp => &Self::MCP_FIELDS,
        }
    }

    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::ProviderModel => "model",
            Self::ProviderApiKey => "api_key",
            Self::ProviderBaseUrl => "base_url",
            Self::ProviderFimModel => "fim_model",
            Self::PermissionsDefaultMode => "default_mode",
            Self::MemoryEnabled => "enabled",
            Self::CompactionEnabled => "enabled",
            Self::CompactionSoftThresholdRatio => "soft_threshold_ratio",
            Self::CompactionHardThresholdRatio => "hard_threshold_ratio",
            Self::CompactionContextWindowTokens => "context_window_tokens",
            Self::CompactionTailMessages => "tail_messages",
            Self::McpName => "name",
            Self::McpCommand => "command",
            Self::McpArgsCsv => "args_csv",
            Self::McpStartupTimeoutSecs => "startup_timeout_secs",
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
                | Self::McpName
                | Self::McpCommand
                | Self::McpArgsCsv
                | Self::McpStartupTimeoutSecs
        )
    }

    pub(crate) fn action_label(self) -> &'static str {
        match self {
            Self::ProviderModel | Self::ProviderFimModel => "Enter choose",
            Self::ProviderApiKey => "Enter input",
            Self::PermissionsDefaultMode => "Enter cycle",
            Self::MemoryEnabled | Self::CompactionEnabled => "Enter toggle",
            _ if self.accepts_text_input() => "Enter input",
            _ => "",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ConfigFooterAction {
    Save,
    SaveAndClose,
    Close,
}

impl ConfigFooterAction {
    const ORDER: [Self; 3] = [Self::Save, Self::SaveAndClose, Self::Close];

    pub(crate) fn button_label(self) -> &'static str {
        match self {
            Self::Save => "save",
            Self::SaveAndClose => "save+close",
            Self::Close => "close",
        }
    }

    pub(crate) fn field_label(self) -> &'static str {
        match self {
            Self::Save => "save",
            Self::SaveAndClose => "save_and_close",
            Self::Close => "close",
        }
    }

    pub(crate) fn next(self) -> Self {
        let index = Self::ORDER
            .iter()
            .position(|action| *action == self)
            .expect("footer action must exist in order");
        Self::ORDER[(index + 1) % Self::ORDER.len()]
    }

    pub(crate) fn previous(self) -> Self {
        let index = Self::ORDER
            .iter()
            .position(|action| *action == self)
            .expect("footer action must exist in order");
        if index == 0 {
            *Self::ORDER.last().expect("footer actions are non-empty")
        } else {
            Self::ORDER[index - 1]
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
    pub(crate) mcp_servers: Vec<McpServerDraft>,
}

impl ConfigDraft {
    pub(crate) fn from_root_config(root_config: &RootConfig) -> Self {
        let provider = load_deepseek_provider_config(root_config)
            .unwrap_or_else(|| default_deepseek_provider_config(&root_config.agent.model));
        Self {
            base_root_config: root_config.clone(),
            provider_model: provider.model,
            provider_api_key: provider.api_key.unwrap_or_default(),
            provider_base_url: provider.base_url,
            provider_beta_base_url: provider.beta_base_url,
            provider_anthropic_base_url: provider.anthropic_base_url,
            provider_user_id_strategy: provider.user_id_strategy.unwrap_or_default(),
            provider_strict_tools_mode: provider.strict_tools_mode,
            provider_fim_model: provider.fim_model,
            provider_request_timeout_secs: provider.request_timeout_secs.to_string(),
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
            mcp_servers: root_config
                .mcp_servers
                .iter()
                .map(McpServerDraft::from_config)
                .collect(),
        }
    }

    pub(crate) fn to_root_config(&self) -> Result<RootConfig> {
        let model = self.provider_model.trim();
        if model.is_empty() {
            bail!("model cannot be empty");
        }
        let api_key = self.provider_api_key.trim();
        let base_url = self.provider_base_url.trim();
        if base_url.is_empty() {
            bail!("base_url cannot be empty");
        }
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
                    anyhow!("context_window_tokens must be a positive integer: {error}")
                })?;
            if parsed == 0 {
                bail!("context_window_tokens must be greater than 0");
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

        let mut root_config = self.base_root_config.clone();
        root_config.agent.model = model.to_owned();
        root_config.permission.default_mode = self.permission_default_mode;
        root_config.memory.enabled = self.memory_enabled;
        root_config.compaction.enabled = self.compaction_enabled;
        root_config.compaction.soft_threshold_ratio = soft_threshold_ratio;
        root_config.compaction.hard_threshold_ratio = hard_threshold_ratio;
        root_config.compaction.context_window_tokens = context_window_tokens;
        root_config.compaction.tail_messages = tail_messages;
        root_config.mcp_servers = self
            .mcp_servers
            .iter()
            .enumerate()
            .map(|(index, server)| server.to_config(index))
            .collect::<Result<Vec<_>>>()?;

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
            .insert("deepseek".to_owned(), provider_value);
        Ok(root_config)
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
            self.selected_footer_action.next()
        } else {
            self.selected_footer_action.previous()
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
            | ConfigField::CompactionEnabled => None,
        }
    }

    pub(crate) fn field_text_value_mut(&mut self, field: ConfigField) -> Option<&mut String> {
        match field {
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
            | ConfigField::CompactionEnabled => None,
        }
    }

    pub(crate) fn display_value(&self, field: ConfigField) -> String {
        let text_value = match field {
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
            _ => self.field_text_value(field).unwrap_or_default(),
        };

        match field {
            ConfigField::McpArgsCsv if text_value.trim().is_empty() => "<empty>".to_owned(),
            ConfigField::CompactionContextWindowTokens if text_value.trim().is_empty() => {
                "<empty = n/a>".to_owned()
            }
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

pub(crate) fn default_deepseek_provider_config(model: &str) -> DeepSeekProviderConfig {
    DeepSeekProviderConfig {
        base_url: "https://api.deepseek.com".to_owned(),
        beta_base_url: "https://api.deepseek.com/beta".to_owned(),
        anthropic_base_url: "https://api.deepseek.com/anthropic".to_owned(),
        model: model.to_owned(),
        api_key: None,
        user_id_strategy: Some("stable_per_end_user".to_owned()),
        strict_tools_mode: StrictToolsMode::Auto,
        fim_model: "deepseek-v4-pro".to_owned(),
        request_timeout_secs: 120,
    }
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
            "{marker} {:<22}: {}",
            field.label(),
            state.display_value(field)
        )
    } else {
        format!(
            "{marker} {:<22}: {}  [{}]",
            field.label(),
            state.display_value(field),
            action
        )
    }
}

pub(crate) fn config_field_accepts_char(field: ConfigField, character: char) -> bool {
    match field {
        ConfigField::CompactionContextWindowTokens
        | ConfigField::CompactionTailMessages
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
        | ConfigField::PermissionsDefaultMode
        | ConfigField::MemoryEnabled
        | ConfigField::CompactionEnabled => false,
    }
}

fn mask_secret(value: &str) -> String {
    if value.is_empty() {
        "<empty>".to_owned()
    } else {
        "*".repeat(value.chars().count().max(8))
    }
}

fn bool_label(enabled: bool) -> &'static str {
    if enabled { "yes" } else { "no" }
}

#[cfg(test)]
#[path = "tests/config_panel_tests.rs"]
mod tests;

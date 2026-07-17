use anyhow::{Result, anyhow, bail};
use sigil_kernel::{CodeIntelligenceConfig, RootConfig};
use sigil_runtime::{
    DeepSeekProviderConfigFields, ModelRequestConfigFields, ProviderConfigFields,
    deepseek_provider_config_fields, model_request_config_fields, set_model_request_config_fields,
    set_provider_config_fields, supported_provider_name,
};

use super::appearance::{first_appearance_color_group_index, first_appearance_color_token_index};
use super::provider::{current_provider_field_draft, provider_drafts_from_root_config};
use super::{ConfigDraft, DEEPSEEK_PROVIDER_KEY, McpServerDraft, normalize_provider_name};

impl ConfigDraft {
    pub(crate) fn from_root_config(root_config: &RootConfig) -> Self {
        let provider_name = normalize_provider_name(&root_config.agent.provider).to_owned();
        let deepseek_fields =
            deepseek_provider_config_fields(root_config, &root_config.agent.model);
        let provider_drafts = provider_drafts_from_root_config(root_config);
        let current_provider_draft =
            current_provider_field_draft(root_config, &provider_name, &provider_drafts);
        let model_request_fields = model_request_config_fields(root_config);
        Self {
            base_root_config: root_config.clone(),
            provider_name: provider_name.clone(),
            provider_model: current_provider_draft.model,
            provider_api_key: current_provider_draft.api_key,
            provider_base_url: current_provider_draft.base_url,
            provider_drafts,
            provider_beta_base_url: deepseek_fields.beta_base_url,
            provider_anthropic_base_url: deepseek_fields.anthropic_base_url,
            provider_user_id_strategy: deepseek_fields.user_id_strategy,
            provider_strict_tools_mode: deepseek_fields.strict_tools_mode,
            provider_fim_model: deepseek_fields.fim_model,
            model_request_timeout_secs: model_request_fields.request_timeout_secs,
            model_request_stream_idle_timeout_secs: model_request_fields.stream_idle_timeout_secs,
            permission_mode: root_config.permission.mode,
            web_enabled: root_config.web.enabled,
            web_network_mode: root_config.web.network_mode,
            web_search_route: root_config.web.search_route,
            web_bundled_search_enabled: root_config.web.bundled_search.enabled,
            verification_auto_run: root_config.verification.auto_run,
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
            code_intelligence_server_startup: root_config.code_intelligence.server_startup,
            code_intelligence_auto_discover: root_config.code_intelligence.auto_discover,
            code_intelligence_report_missing: root_config.code_intelligence.report_missing,
            terminal_keyboard_enhancement: root_config.terminal.keyboard_enhancement,
            terminal_mouse_capture: root_config.terminal.mouse_capture,
            terminal_osc52_clipboard: root_config.terminal.osc52_clipboard,
            terminal_scroll_sensitivity: root_config.terminal.scroll_sensitivity.to_string(),
            terminal_notifications_enabled: root_config.terminal.notifications.enabled,
            terminal_notification_method: root_config.terminal.notifications.method,
            terminal_notification_minimum_run_duration_ms: root_config
                .terminal
                .notifications
                .minimum_run_duration_ms
                .to_string(),
            appearance_theme: root_config.appearance.theme,
            appearance_syntax_theme: root_config.appearance.syntax_theme,
            appearance_usage_cost_currency: root_config.appearance.usage_cost_currency,
            appearance_info_rail: root_config.appearance.info_rail,
            appearance_color_group_index: first_appearance_color_group_index(root_config),
            appearance_color_token_index: first_appearance_color_token_index(root_config),
            mcp_servers: root_config
                .mcp_servers
                .iter()
                .map(McpServerDraft::from_config)
                .collect(),
        }
    }

    pub(crate) fn to_root_config(&self) -> Result<RootConfig> {
        let provider_name = normalize_provider_name(&self.provider_name);
        supported_provider_name(provider_name)?;
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
        let terminal_notification_minimum_run_duration_ms = self
            .terminal_notification_minimum_run_duration_ms
            .trim()
            .parse::<u64>()
            .map_err(|error| {
                anyhow!(
                    "terminal.notifications.minimum_run_duration_ms must be a positive integer: {error}"
                )
            })?;

        let mut root_config = self.base_root_config.clone();
        root_config.agent.provider = provider_name.to_owned();
        root_config.agent.model = model.to_owned();
        root_config.permission.mode = self.permission_mode;
        root_config.web.enabled = self.web_enabled;
        root_config.web.network_mode = self.web_network_mode;
        root_config.web.search_route = self.web_search_route;
        root_config.web.bundled_search.enabled = self.web_bundled_search_enabled;
        root_config.verification.auto_run = self.verification_auto_run;
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
        root_config.terminal.notifications.enabled = self.terminal_notifications_enabled;
        root_config.terminal.notifications.method = self.terminal_notification_method;
        root_config.terminal.notifications.minimum_run_duration_ms =
            terminal_notification_minimum_run_duration_ms;
        root_config
            .terminal
            .notifications
            .validate()
            .map_err(anyhow::Error::msg)?;
        root_config.appearance.theme = self.appearance_theme;
        root_config.appearance.syntax_theme = self.appearance_syntax_theme;
        root_config.appearance.usage_cost_currency = self.appearance_usage_cost_currency;
        root_config.appearance.info_rail = self.appearance_info_rail;
        root_config.appearance.colors = self.base_root_config.appearance.colors.clone();
        root_config.mcp_servers = self
            .mcp_servers
            .iter()
            .enumerate()
            .map(|(index, server)| server.to_config(index))
            .collect::<Result<Vec<_>>>()?;

        let provider_fields = ProviderConfigFields {
            model: model.to_owned(),
            api_key: api_key.to_owned(),
            base_url: base_url.to_owned(),
        };
        let model_request_fields = ModelRequestConfigFields {
            request_timeout_secs: self.model_request_timeout_secs.clone(),
            stream_idle_timeout_secs: self.model_request_stream_idle_timeout_secs.clone(),
        };
        set_model_request_config_fields(&mut root_config, &model_request_fields)?;
        let deepseek_fields = DeepSeekProviderConfigFields {
            beta_base_url: self.provider_beta_base_url.trim().to_owned(),
            anthropic_base_url: self.provider_anthropic_base_url.trim().to_owned(),
            user_id_strategy: self.provider_user_id_strategy.trim().to_owned(),
            strict_tools_mode: self.provider_strict_tools_mode,
            fim_model: self.provider_fim_model.trim().to_owned(),
        };
        set_provider_config_fields(
            &mut root_config,
            provider_name,
            &provider_fields,
            Some(&deepseek_fields),
        )?;
        Ok(root_config)
    }

    pub(crate) fn code_intelligence_config(&self) -> CodeIntelligenceConfig {
        let mut config = self.base_root_config.code_intelligence.clone();
        config.enabled = self.code_intelligence_enabled;
        config.server_startup = self.code_intelligence_server_startup;
        config.auto_discover = self.code_intelligence_auto_discover;
        config.report_missing = self.code_intelligence_report_missing;
        config
    }

    pub(crate) fn code_intelligence_preview_root_config(&self) -> RootConfig {
        let mut root_config = self.base_root_config.clone();
        root_config.code_intelligence = self.code_intelligence_config();
        root_config
    }
}

use super::*;

fn test_root_config() -> RootConfig {
    RootConfig {
        workspace: Default::default(),
        storage: Default::default(),
        session: Default::default(),
        agent: sigil_kernel::AgentConfig {
            provider: "deepseek".to_owned(),
            model: "deepseek-v4-flash".to_owned(),
            max_turns: None,
            tool_timeout_secs: 30,
        },
        model_request: Default::default(),
        permission: Default::default(),
        memory: Default::default(),
        skills: Default::default(),
        compaction: Default::default(),
        code_intelligence: Default::default(),
        terminal: Default::default(),
        execution: Default::default(),
        verification: Default::default(),
        appearance: Default::default(),
        task: Default::default(),
        providers: Default::default(),
        mcp_servers: Vec::new(),
    }
}

fn test_skill(id: &str) -> sigil_kernel::SkillDescriptor {
    sigil_kernel::SkillDescriptor {
        id: id.to_owned(),
        name: id.to_owned(),
        description: "test skill".to_owned(),
        when_to_use: None,
        root: format!(".sigil/skills/{id}").into(),
        entrypoint: format!(".sigil/skills/{id}/SKILL.md").into(),
        source: sigil_kernel::SkillSource::Workspace,
        sha256: format!("{id}-sha"),
        enabled: true,
        trust: sigil_kernel::SkillTrustState::Trusted,
        model_invocable: true,
        user_invocable: true,
        run_as: sigil_kernel::SkillRunMode::Inline,
        agent: None,
        argument_hint: Some("target path".to_owned()),
        allowed_tools: Default::default(),
        disallowed_tools: Default::default(),
        path_patterns: vec!["crates/**".to_owned()],
    }
}

fn test_agent(id: &str) -> sigil_runtime::ResolvedAgentProfile {
    sigil_runtime::ResolvedAgentProfile {
        profile: sigil_kernel::AgentProfile {
            id: sigil_kernel::AgentProfileId::new(id).expect("agent id should parse"),
            kind: sigil_kernel::AgentProfileKind::Subagent,
            description: "test agent".to_owned(),
            instructions: "inspect only".to_owned(),
            model: None,
            provider: None,
            reasoning_effort: None,
            tool_scope: Default::default(),
            permission_policy: Default::default(),
            invocation_policy: sigil_kernel::AgentInvocationPolicy::ModelAllowed,
            result_policy: sigil_kernel::AgentResultPolicy::SummaryWithPageRef,
            user_invocable: true,
            model_invocable: true,
            skills: Vec::new(),
            mcp_servers: Vec::new(),
            nickname_candidates: vec![id.to_owned()],
            aliases: Vec::new(),
            slash_names: Vec::new(),
        },
        enabled: true,
        enabled_override: None,
        user_invocable_override: None,
        model_invocable_override: None,
        source: sigil_kernel::AgentProfileSource::Workspace,
        source_hash: format!("{id}-source-sha"),
        trust_state: sigil_kernel::AgentTrustState::Trusted,
    }
}

fn test_plugin(id: &str) -> sigil_kernel::PluginManifestSnapshot {
    sigil_kernel::PluginManifestSnapshot {
        plugin_id: id.to_owned(),
        name: id.to_owned(),
        version: "0.1.0".to_owned(),
        description: Some("test plugin".to_owned()),
        manifest_path: format!(".sigil/plugins/{id}/plugin.toml").into(),
        manifest_hash: format!("{id}-sha"),
        capabilities: vec![sigil_kernel::PluginCapability::Skill {
            path: "skills/review/SKILL.md".into(),
        }],
        trust: sigil_kernel::PluginTrustDecision::NeedsReview,
    }
}

#[test]
fn config_section_flow_wraps() {
    assert_eq!(ConfigSection::Provider.next_flow(), ConfigSection::Storage);
    assert_eq!(
        ConfigSection::Storage.next_flow(),
        ConfigSection::Permissions
    );
    assert_eq!(
        ConfigSection::Compaction.next_flow(),
        ConfigSection::CodeIntelligence
    );
    assert_eq!(
        ConfigSection::CodeIntelligence.next_flow(),
        ConfigSection::Terminal
    );
    assert_eq!(
        ConfigSection::Terminal.next_flow(),
        ConfigSection::Appearance
    );
    assert_eq!(ConfigSection::Appearance.next_flow(), ConfigSection::Agents);
    assert_eq!(ConfigSection::Agents.next_flow(), ConfigSection::Skills);
    assert_eq!(ConfigSection::Skills.next_flow(), ConfigSection::Plugins);
    assert_eq!(ConfigSection::Plugins.next_flow(), ConfigSection::Mcp);
    assert_eq!(ConfigSection::Mcp.next_flow(), ConfigSection::Provider);
    assert_eq!(ConfigSection::Provider.previous_flow(), ConfigSection::Mcp);
    assert_eq!(
        ConfigSection::Permissions.previous_flow(),
        ConfigSection::Storage
    );
}

#[test]
fn config_footer_action_navigation_wraps() {
    assert_eq!(
        ConfigFooterAction::Save.next_for_section(ConfigSection::Provider),
        ConfigFooterAction::Close
    );
    assert_eq!(
        ConfigFooterAction::Save.previous_for_section(ConfigSection::Provider),
        ConfigFooterAction::Close
    );
    assert_eq!(
        ConfigFooterAction::CleanMutationArtifacts.next_for_section(ConfigSection::Storage),
        ConfigFooterAction::Close
    );
    assert_eq!(
        ConfigFooterAction::Close.previous_for_section(ConfigSection::Storage),
        ConfigFooterAction::CleanMutationArtifacts
    );
    assert_eq!(
        ConfigFooterAction::Close.next_for_section(ConfigSection::Permissions),
        ConfigFooterAction::Save
    );
    assert_eq!(
        ConfigFooterAction::ActivateMcp.next_for_section(ConfigSection::Mcp),
        ConfigFooterAction::Close
    );
    assert_eq!(
        ConfigFooterAction::Close.previous_for_section(ConfigSection::Mcp),
        ConfigFooterAction::ActivateMcp
    );
    assert_eq!(
        ConfigFooterAction::UseSkill.next_for_section(ConfigSection::Skills),
        ConfigFooterAction::Close
    );
    assert_eq!(
        ConfigFooterAction::Close.next_for_section(ConfigSection::Skills),
        ConfigFooterAction::UseSkill
    );
    assert_eq!(
        ConfigFooterAction::TrustAgent.next_for_section(ConfigSection::Agents),
        ConfigFooterAction::BlockAgent
    );
    assert_eq!(
        ConfigFooterAction::BlockAgent.next_for_section(ConfigSection::Agents),
        ConfigFooterAction::TrustAgent
    );
    assert_eq!(
        ConfigFooterAction::ApprovePlugin.next_for_section(ConfigSection::Plugins),
        ConfigFooterAction::DenyPlugin
    );
    assert_eq!(
        ConfigFooterAction::DenyPlugin.next_for_section(ConfigSection::Plugins),
        ConfigFooterAction::ApprovePlugin
    );
}

#[test]
fn compaction_context_field_uses_short_fallback_label() {
    assert_eq!(
        ConfigField::CompactionContextWindowTokens.label(),
        "fallback_window"
    );
    assert_eq!(
        ConfigField::CompactionContextWindowTokens.display_label(),
        "Fallback window"
    );

    let state = ConfigState::from_root_config(&RootConfig {
        workspace: Default::default(),
        storage: Default::default(),
        session: Default::default(),
        agent: sigil_kernel::AgentConfig {
            provider: "deepseek".to_owned(),
            model: "deepseek-v4-pro".to_owned(),
            max_turns: None,
            tool_timeout_secs: 30,
        },
        model_request: Default::default(),
        permission: Default::default(),
        memory: Default::default(),
        skills: Default::default(),
        compaction: Default::default(),
        code_intelligence: Default::default(),
        terminal: Default::default(),
        execution: Default::default(),
        verification: Default::default(),
        appearance: Default::default(),
        task: Default::default(),
        providers: Default::default(),
        mcp_servers: Vec::new(),
    });

    assert_eq!(
        state.display_value(ConfigField::CompactionContextWindowTokens),
        "provider/model metadata"
    );
}

#[test]
fn config_rows_do_not_pre_pad_labels() {
    let state = ConfigState::from_root_config(&RootConfig {
        workspace: Default::default(),
        storage: Default::default(),
        session: Default::default(),
        agent: sigil_kernel::AgentConfig {
            provider: "deepseek".to_owned(),
            model: "deepseek-v4-flash".to_owned(),
            max_turns: None,
            tool_timeout_secs: 30,
        },
        model_request: Default::default(),
        permission: Default::default(),
        memory: Default::default(),
        skills: Default::default(),
        compaction: Default::default(),
        code_intelligence: Default::default(),
        terminal: Default::default(),
        execution: Default::default(),
        verification: Default::default(),
        appearance: Default::default(),
        task: Default::default(),
        providers: Default::default(),
        mcp_servers: Vec::new(),
    });

    assert_eq!(
        render_config_value_row(&state, ConfigField::ProviderModel),
        "> Model: deepseek-v4-flash  [Enter choose]"
    );
    assert_eq!(
        render_config_readonly_row("Root files", "SIGIL.md"),
        "- Root files: SIGIL.md"
    );
}

#[test]
fn api_key_display_uses_status_without_secret_length() {
    let mut config = RootConfig {
        workspace: Default::default(),
        storage: Default::default(),
        session: Default::default(),
        agent: sigil_kernel::AgentConfig {
            provider: "deepseek".to_owned(),
            model: "deepseek-v4-flash".to_owned(),
            max_turns: None,
            tool_timeout_secs: 30,
        },
        model_request: Default::default(),
        permission: Default::default(),
        memory: Default::default(),
        skills: Default::default(),
        compaction: Default::default(),
        code_intelligence: Default::default(),
        terminal: Default::default(),
        execution: Default::default(),
        verification: Default::default(),
        appearance: Default::default(),
        task: Default::default(),
        providers: Default::default(),
        mcp_servers: Vec::new(),
    };

    let empty_state = ConfigState::from_root_config(&config);
    assert_eq!(
        empty_state.display_value(ConfigField::ProviderApiKey),
        "not set"
    );

    config.providers.insert(
        "deepseek".to_owned(),
        serde_json::json!({
            "api_key": "short",
        }),
    );
    let short_state = ConfigState::from_root_config(&config);
    assert_eq!(
        short_state.display_value(ConfigField::ProviderApiKey),
        "set (hidden)"
    );

    config.providers.insert(
        "deepseek".to_owned(),
        serde_json::json!({
            "api_key": "a-very-very-long-api-key-value",
        }),
    );
    let long_state = ConfigState::from_root_config(&config);
    assert_eq!(
        long_state.display_value(ConfigField::ProviderApiKey),
        short_state.display_value(ConfigField::ProviderApiKey)
    );
}

#[test]
fn provider_cycle_keeps_per_provider_field_drafts_separate() -> anyhow::Result<()> {
    let mut config = test_root_config();
    config.providers.insert(
        "deepseek".to_owned(),
        serde_json::json!({
            "api_key": "deepseek-key",
            "base_url": "https://deepseek.example.com",
            "beta_base_url": "https://deepseek.example.com/beta",
            "anthropic_base_url": "https://deepseek.example.com/anthropic",
            "fim_model": "deepseek-fim"
        }),
    );
    config.providers.insert(
        "openai_compat".to_owned(),
        serde_json::json!({
            "api_key": "openai-key",
            "base_url": "https://openai.example.com/v1"
        }),
    );
    config.providers.insert(
        "anthropic".to_owned(),
        serde_json::json!({
            "api_key": "anthropic-key",
            "base_url": "https://anthropic.example.com"
        }),
    );
    config.providers.insert(
        "gemini".to_owned(),
        serde_json::json!({
            "api_key": "gemini-key",
            "base_url": "https://gemini.example.com/v1beta"
        }),
    );

    let mut state = ConfigState::from_root_config(&config);
    assert_eq!(state.draft.provider_model, config.agent.model);
    assert_eq!(state.draft.provider_api_key, "deepseek-key");

    state.draft.cycle_provider();
    assert_eq!(state.draft.provider_name, OPENAI_COMPAT_PROVIDER_KEY);
    assert_eq!(state.draft.provider_model, config.agent.model);
    state.draft.provider_model = "openai-edited".to_owned();
    state.draft.provider_api_key = "openai-edited-key".to_owned();

    state.draft.cycle_provider();
    assert_eq!(state.draft.provider_name, ANTHROPIC_PROVIDER_KEY);
    assert_eq!(state.draft.provider_model, config.agent.model);
    assert_eq!(state.draft.provider_api_key, "anthropic-key");
    let saved = state.draft.to_root_config()?;
    assert_eq!(saved.agent.provider, ANTHROPIC_PROVIDER_KEY);
    assert_eq!(saved.agent.model, config.agent.model);
    assert_eq!(
        saved.providers["anthropic"]["api_key"],
        serde_json::Value::String("anthropic-key".to_owned())
    );
    assert_ne!(
        saved.providers["anthropic"]["api_key"],
        serde_json::Value::String("deepseek-key".to_owned())
    );

    state.draft.cycle_provider();
    assert_eq!(state.draft.provider_name, GEMINI_PROVIDER_KEY);
    assert_eq!(state.draft.provider_model, config.agent.model);
    state.draft.cycle_provider();
    assert_eq!(state.draft.provider_name, DEEPSEEK_PROVIDER_KEY);
    assert_eq!(state.draft.provider_model, config.agent.model);
    state.draft.cycle_provider();
    assert_eq!(state.draft.provider_name, OPENAI_COMPAT_PROVIDER_KEY);
    assert_eq!(state.draft.provider_model, "openai-edited");
    assert_eq!(state.draft.provider_api_key, "openai-edited-key");
    Ok(())
}

#[test]
fn provider_cycle_loads_default_draft_when_provider_cache_is_missing() {
    let mut state = ConfigState::from_root_config(&test_root_config());
    state
        .draft
        .provider_drafts
        .remove(OPENAI_COMPAT_PROVIDER_KEY);

    state.draft.cycle_provider();

    assert_eq!(state.draft.provider_name, OPENAI_COMPAT_PROVIDER_KEY);
    assert_eq!(state.draft.provider_model, "deepseek-v4-flash");
    assert!(!state.draft.provider_base_url.is_empty());
}

#[test]
fn default_provider_field_draft_uses_provider_specific_defaults() {
    for provider_name in [
        OPENAI_COMPAT_PROVIDER_KEY,
        ANTHROPIC_PROVIDER_KEY,
        GEMINI_PROVIDER_KEY,
        DEEPSEEK_PROVIDER_KEY,
    ] {
        let draft = default_provider_field_draft(provider_name, "provider-model");

        assert_eq!(draft.model, "provider-model");
        assert!(!draft.base_url.is_empty());
    }
}

#[test]
fn config_field_metadata_covers_all_user_facing_fields() {
    assert_eq!(ConfigSection::Permissions.summary(), "safety settings");
    assert_eq!(ConfigSection::Storage.summary(), "local state paths");
    assert_eq!(ConfigSection::Provider.flow_index(), Some(0));
    assert_eq!(ConfigSection::Storage.flow_index(), Some(1));
    assert_eq!(ConfigSection::CodeIntelligence.flow_index(), Some(5));
    assert_eq!(ConfigSection::Terminal.flow_index(), Some(6));
    assert_eq!(ConfigSection::Appearance.flow_index(), Some(7));
    assert_eq!(ConfigSection::Agents.flow_index(), Some(8));
    assert_eq!(ConfigSection::Skills.flow_index(), Some(9));
    assert_eq!(ConfigSection::Plugins.flow_index(), Some(10));
    assert_eq!(ConfigSection::Mcp.flow_index(), Some(11));
    assert_eq!(ConfigField::fields_for_section(ConfigSection::Storage), &[]);
    assert_eq!(
        ConfigField::fields_for_section(ConfigSection::Permissions),
        &[
            ConfigField::PermissionsPreset,
            ConfigField::PermissionsDefaultMode
        ]
    );
    assert_eq!(
        ConfigField::fields_for_section(ConfigSection::CodeIntelligence),
        &[
            ConfigField::CodeIntelEnabled,
            ConfigField::CodeIntelServerStartup
        ]
    );
    assert_eq!(
        ConfigField::fields_for_section(ConfigSection::Compaction),
        &[
            ConfigField::CompactionEnabled,
            ConfigField::CompactionContextWindowTokens,
            ConfigField::CompactionSoftThresholdRatio,
            ConfigField::CompactionHardThresholdRatio,
            ConfigField::CompactionTailMessages,
        ]
    );
    assert_eq!(
        ConfigField::fields_for_section(ConfigSection::Terminal),
        &[]
    );
    assert_eq!(
        ConfigField::fields_for_section(ConfigSection::Appearance),
        &[
            ConfigField::AppearanceTheme,
            ConfigField::AppearanceSyntaxTheme,
            ConfigField::AppearanceUsageCostCurrency,
        ]
    );
    assert_eq!(ConfigField::fields_for_section(ConfigSection::Mcp), &[]);
    assert_eq!(
        ConfigField::fields_for_section(ConfigSection::Agents),
        &[ConfigField::SkillId]
    );
    assert_eq!(
        ConfigField::fields_for_section(ConfigSection::Skills),
        &[ConfigField::SkillId]
    );
    assert_eq!(
        ConfigField::fields_for_section(ConfigSection::Plugins),
        &[ConfigField::PluginId]
    );

    assert_eq!(ConfigField::McpCommand.label(), "command");
    assert_eq!(ConfigField::PermissionsPreset.label(), "preset");
    assert_eq!(ConfigField::PermissionsDefaultMode.label(), "mode");
    assert_eq!(ConfigField::VerificationAutoRun.label(), "checks");
    assert_eq!(ConfigField::SkillId.label(), "skill");
    assert_eq!(ConfigField::PluginId.label(), "plugin");
    assert_eq!(ConfigField::McpArgsCsv.label(), "args_csv");
    assert_eq!(
        ConfigField::CodeIntelServerStartup.label(),
        "server_startup"
    );
    assert_eq!(
        ConfigField::TerminalScrollSensitivity.label(),
        "scroll_sensitivity"
    );
    assert_eq!(ConfigField::AppearanceTheme.label(), "theme");
    assert_eq!(ConfigField::AppearanceSyntaxTheme.label(), "syntax_theme");
    assert_eq!(
        ConfigField::AppearanceUsageCostCurrency.label(),
        "usage_cost_currency"
    );
    assert_eq!(ConfigField::AppearanceColorGroup.label(), "color_group");
    assert_eq!(ConfigField::AppearanceColorToken.label(), "color_token");
    assert_eq!(
        ConfigField::AppearanceColorOverride.label(),
        "color_override"
    );
    assert_eq!(ConfigField::ProviderApiKey.action_label(), "Enter input");
    assert_eq!(
        ConfigField::VerificationAutoRun.action_label(),
        "Enter cycle"
    );
    assert_eq!(
        ConfigField::CodeIntelServerStartup.action_label(),
        "Enter cycle"
    );
    assert_eq!(ConfigField::CodeIntelEnabled.action_label(), "Enter toggle");
    assert_eq!(
        ConfigField::TerminalMouseCapture.action_label(),
        "Enter toggle"
    );
    assert_eq!(
        ConfigField::TerminalScrollSensitivity.action_label(),
        "Enter input"
    );
    assert_eq!(ConfigField::AppearanceTheme.action_label(), "Enter cycle");
    assert_eq!(
        ConfigField::AppearanceSyntaxTheme.action_label(),
        "Enter cycle"
    );
    assert_eq!(
        ConfigField::AppearanceUsageCostCurrency.action_label(),
        "Enter cycle"
    );
    assert_eq!(
        ConfigField::AppearanceColorGroup.action_label(),
        "Enter cycle"
    );
    assert_eq!(
        ConfigField::AppearanceColorToken.action_label(),
        "Enter cycle"
    );
    assert_eq!(
        ConfigField::AppearanceColorOverride.action_label(),
        "Enter input"
    );
    assert_eq!(ConfigField::McpCommand.action_label(), "Enter input");
    assert_eq!(ConfigField::SkillId.action_label(), "");
    assert_eq!(ConfigFooterAction::ActivateMcp.button_label(), "activate");
    assert_eq!(
        ConfigFooterAction::CleanMutationArtifacts.button_label(),
        "clean"
    );
    assert_eq!(
        ConfigFooterAction::CleanMutationArtifacts.field_label(),
        "clean_artifacts"
    );
    assert_eq!(ConfigFooterAction::TrustAgent.button_label(), "trust");
    assert_eq!(ConfigFooterAction::TrustAgent.field_label(), "trust_agent");
    assert_eq!(ConfigFooterAction::BlockAgent.button_label(), "disable");
    assert_eq!(
        ConfigFooterAction::BlockAgent.field_label(),
        "disable_agent"
    );
    assert_eq!(
        ConfigFooterAction::ToggleAgentEnabled.field_label(),
        "toggle_agent_enabled"
    );
    assert_eq!(
        ConfigFooterAction::ToggleAgentUser.field_label(),
        "toggle_agent_user"
    );
    assert_eq!(
        ConfigFooterAction::ToggleAgentModel.field_label(),
        "toggle_agent_model"
    );
    assert_eq!(ConfigFooterAction::UseSkill.button_label(), "use");
    assert_eq!(ConfigFooterAction::UseSkill.field_label(), "use_skill");
    assert_eq!(ConfigFooterAction::ApprovePlugin.button_label(), "approve");
    assert_eq!(ConfigFooterAction::DenyPlugin.field_label(), "deny_plugin");
    assert_eq!(
        ConfigFooterAction::SaveAndClose.field_label(),
        "save_and_close"
    );
    assert!(
        ConfigField::ProviderApiKey
            .help_text()
            .contains("environment variables")
    );
    assert!(
        ConfigField::TerminalScrollSensitivity
            .help_text()
            .contains("Mouse wheel rows")
    );
    assert!(
        ConfigField::CompactionSoftThresholdRatio
            .help_text()
            .contains("Prompt pressure")
    );
    assert!(
        ConfigField::McpArgsCsv
            .help_text()
            .contains("Comma-separated")
    );
    assert!(ConfigField::PluginId.help_text().contains("manifest hash"));
    assert!(
        ConfigField::CodeIntelAutoDiscover
            .help_text()
            .contains("language servers")
    );
    assert!(
        ConfigField::CodeIntelServerStartup
            .help_text()
            .contains("lazily started")
    );
    assert!(
        ConfigField::CodeIntelReportMissing
            .help_text()
            .contains("readiness warnings")
    );
    assert!(
        ConfigField::TerminalOsc52Clipboard
            .help_text()
            .contains("OSC52")
    );
    assert!(
        ConfigField::AppearanceTheme
            .help_text()
            .contains("session history")
    );
    assert!(
        ConfigField::AppearanceSyntaxTheme
            .help_text()
            .contains("code blocks")
    );
    assert!(
        ConfigField::AppearanceColorGroup
            .help_text()
            .contains("token group")
    );
    assert!(
        ConfigField::AppearanceColorToken
            .help_text()
            .contains("current group")
    );
    assert!(
        ConfigField::AppearanceColorOverride
            .help_text()
            .contains("#RRGGBB")
    );
}

#[test]
fn appearance_theme_draft_roundtrips() -> anyhow::Result<()> {
    let mut config = test_root_config();
    config.appearance.theme = sigil_kernel::ThemeId::SolarizedLight;
    config.appearance.syntax_theme = sigil_kernel::SyntaxThemeId::SolarizedDark;
    config.appearance.usage_cost_currency = sigil_kernel::UsageCostCurrency::Usd;
    let mut state = ConfigState::from_root_config(&config);

    state.set_section(ConfigSection::Appearance);
    assert_eq!(state.selected_field, Some(ConfigField::AppearanceTheme));
    assert_eq!(
        state.display_value(ConfigField::AppearanceTheme),
        "solarized_light"
    );
    assert_eq!(state.field_text_value(ConfigField::AppearanceTheme), None);
    assert_eq!(
        state.display_value(ConfigField::AppearanceSyntaxTheme),
        "solarized_dark"
    );
    assert_eq!(
        state.field_text_value(ConfigField::AppearanceSyntaxTheme),
        None
    );
    assert_eq!(
        state.display_value(ConfigField::AppearanceUsageCostCurrency),
        "usd"
    );
    assert_eq!(
        state.field_text_value(ConfigField::AppearanceUsageCostCurrency),
        None
    );
    assert!(
        state
            .field_text_value_mut(ConfigField::AppearanceUsageCostCurrency)
            .is_none()
    );
    assert!(!config_field_accepts_char(
        ConfigField::AppearanceUsageCostCurrency,
        'u'
    ));
    assert_eq!(
        ConfigField::AppearanceUsageCostCurrency.action_label(),
        "Enter cycle"
    );
    assert!(
        state
            .field_text_value_mut(ConfigField::AppearanceTheme)
            .is_none()
    );
    assert!(!config_field_accepts_char(
        ConfigField::AppearanceTheme,
        'n'
    ));

    state.draft.appearance_theme = sigil_kernel::ThemeId::Nord;
    state.draft.cycle_appearance_syntax_theme();
    state.draft.cycle_appearance_usage_cost_currency();
    let saved = state.draft.to_root_config()?;

    assert_eq!(saved.appearance.theme, sigil_kernel::ThemeId::Nord);
    assert_eq!(
        saved.appearance.syntax_theme,
        sigil_kernel::SyntaxThemeId::SolarizedLight
    );
    assert_eq!(
        saved.appearance.usage_cost_currency,
        sigil_kernel::UsageCostCurrency::Cny
    );
    Ok(())
}

#[test]
fn appearance_color_override_draft_tracks_tokens_and_empty_reset() -> anyhow::Result<()> {
    let mut config = test_root_config();
    let mut colors = std::collections::BTreeMap::new();
    colors.insert("surface_rail".to_owned(), "#010203".to_owned());
    config.appearance.colors = sigil_kernel::ThemeColorOverrides::new(colors);
    let mut state = ConfigState::from_root_config(&config);

    state.set_section(ConfigSection::Appearance);

    assert_eq!(
        state.draft.selected_appearance_color_group().key,
        "surfaces"
    );
    assert_eq!(
        state.display_value(ConfigField::AppearanceColorGroup),
        "surfaces"
    );
    assert_eq!(
        state.draft.selected_appearance_color_token(),
        "surface_rail"
    );
    assert_eq!(
        state.field_text_value(ConfigField::AppearanceColorOverride),
        Some("#010203")
    );
    assert!(config_field_accepts_char(
        ConfigField::AppearanceColorOverride,
        '#'
    ));
    assert!(config_field_accepts_char(
        ConfigField::AppearanceColorOverride,
        'F'
    ));
    assert!(!config_field_accepts_char(
        ConfigField::AppearanceColorOverride,
        'g'
    ));
    assert!(!config_field_accepts_char(
        ConfigField::AppearanceColorGroup,
        's'
    ));
    assert!(!config_field_accepts_char(
        ConfigField::AppearanceColorToken,
        's'
    ));

    state.draft.cycle_appearance_color_token(false);
    assert_eq!(
        state.draft.selected_appearance_color_token(),
        "surface_base"
    );
    state.draft.cycle_appearance_color_token(false);
    assert_eq!(
        state.draft.selected_appearance_color_token(),
        "surface_code"
    );
    state.draft.cycle_appearance_color_token(true);
    assert_eq!(
        state.draft.selected_appearance_color_token(),
        "surface_base"
    );
    state.draft.cycle_appearance_color_group(true);
    assert_eq!(state.draft.selected_appearance_color_group().key, "borders");
    assert_eq!(
        state.draft.selected_appearance_color_token(),
        "border_subtle"
    );
    state.draft.cycle_all_appearance_color_tokens(false);
    assert_eq!(
        state.draft.selected_appearance_color_token(),
        "surface_code"
    );
    assert_eq!(
        state.draft.selected_appearance_color_group().key,
        "surfaces"
    );

    assert!(
        state
            .draft
            .set_selected_appearance_color_override("#aabbcc".to_owned())?
    );
    assert_eq!(
        state.draft.selected_appearance_color_override(),
        Some("#AABBCC")
    );
    assert!(
        state
            .draft
            .set_selected_appearance_color_override(" ".to_owned())?
    );
    assert!(state.draft.selected_appearance_color_override().is_none());

    let mut empty_draft = ConfigDraft::from_root_config(&test_root_config());
    assert!(!empty_draft.reset_all_appearance_color_overrides());
    assert_eq!(
        empty_draft.reset_selected_appearance_color_group_overrides(),
        0
    );
    Ok(())
}

#[test]
fn config_state_handles_mcp_collection_navigation_and_mutation() {
    let mut state = ConfigState::from_root_config(&test_root_config());

    state.set_section(ConfigSection::Mcp);
    assert_eq!(state.selected_field, None);
    assert_eq!(state.move_field(true), ConfigFieldMove::Unavailable);
    assert!(!state.focus_last_field());
    assert!(!state.remove_selected_mcp_server());
    assert!(!state.cycle_mcp_server(true));

    state.add_mcp_server();
    assert!(state.dirty);
    assert_eq!(state.selected_field, None);
    assert_eq!(state.selected_mcp_server_index, 0);
    assert_eq!(
        state.field_text_value(ConfigField::McpStartupTimeoutSecs),
        Some("10")
    );
    assert_eq!(
        state.display_value(ConfigField::McpArgsCsv),
        "none".to_owned()
    );

    *state
        .field_text_value_mut(ConfigField::McpCommand)
        .expect("mcp command field should be mutable") = "node".to_owned();
    *state
        .field_text_value_mut(ConfigField::McpArgsCsv)
        .expect("mcp args field should be mutable") = "--stdio, --verbose".to_owned();
    assert_eq!(
        state.field_text_value(ConfigField::McpCommand),
        Some("node")
    );
    assert_eq!(
        state.field_text_value(ConfigField::CodeIntelReportMissing),
        None
    );
    assert!(
        state
            .field_text_value_mut(ConfigField::CodeIntelReportMissing)
            .is_none()
    );
    assert_eq!(
        state.display_value(ConfigField::McpStartupTimeoutSecs),
        "10 seconds"
    );

    state.add_mcp_server();
    assert_eq!(state.selected_mcp_server_index, 1);
    assert!(state.cycle_mcp_server(true));
    assert_eq!(state.selected_mcp_server_index, 0);
    assert!(state.cycle_mcp_server(false));
    assert_eq!(state.selected_mcp_server_index, 1);
    assert!(state.remove_selected_mcp_server());
    assert_eq!(state.selected_mcp_server_index, 0);
}

#[test]
fn config_state_handles_skill_collection_navigation() {
    let mut state = ConfigState::from_root_config(&test_root_config());

    state.set_section(ConfigSection::Skills);
    assert_eq!(state.selected_field, None);
    assert_eq!(state.move_field(true), ConfigFieldMove::Unavailable);
    assert_eq!(state.move_skill(true), ConfigFieldMove::Unavailable);
    assert!(!state.focus_last_field());
    assert!(!state.cycle_skill(true));

    let mut agent = test_skill("audit-agent");
    agent.run_as = sigil_kernel::SkillRunMode::ChildSession;
    state.set_skill_discovery(
        vec![test_skill("review"), agent, test_skill("release")],
        vec!["invalid skill ignored".to_owned()],
    );
    assert_eq!(state.selected_field, Some(ConfigField::SkillId));
    assert_eq!(state.selected_skill_index, 0);
    assert_eq!(
        state.skill_warnings,
        vec!["invalid skill ignored".to_owned()]
    );
    assert_eq!(state.field_text_value(ConfigField::SkillId), Some("review"));
    assert_eq!(state.display_value(ConfigField::SkillId), "review");
    assert!(state.field_text_value_mut(ConfigField::SkillId).is_none());
    assert!(!config_field_accepts_char(ConfigField::SkillId, 'x'));

    assert_eq!(state.move_skill(true), ConfigFieldMove::Moved);
    assert_eq!(state.selected_skill_index, 2);
    assert_eq!(
        state.field_text_value(ConfigField::SkillId),
        Some("release")
    );
    assert_eq!(state.move_skill(true), ConfigFieldMove::Boundary);
    assert!(state.cycle_skill(true));
    assert_eq!(state.selected_skill_index, 0);
    assert!(state.cycle_skill(false));
    assert_eq!(state.selected_skill_index, 2);
    assert_eq!(state.move_skill(false), ConfigFieldMove::Moved);
    assert_eq!(state.selected_skill_index, 0);
    assert_eq!(state.move_skill(false), ConfigFieldMove::Boundary);

    state.set_section(ConfigSection::Agents);
    assert_eq!(state.selected_field, None);
    state.selected_skill_index = 0;
    assert!(state.selected_skill().is_none());
    state.selected_skill_index = 1;
    assert_eq!(
        state.selected_skill().map(|skill| skill.id.as_str()),
        Some("audit-agent")
    );
}

#[test]
fn config_state_handles_agent_collection_navigation() {
    let mut state = ConfigState::from_root_config(&test_root_config());

    state.set_section(ConfigSection::Agents);
    assert_eq!(state.selected_field, None);
    assert_eq!(state.move_field(true), ConfigFieldMove::Unavailable);
    assert_eq!(state.move_agent(true), ConfigFieldMove::Unavailable);
    assert!(!state.focus_last_field());
    assert!(!state.cycle_agent(true));

    state.set_agent_discovery(
        vec![test_agent("explore"), test_agent("review")],
        vec!["invalid agent ignored".to_owned()],
    );
    assert_eq!(state.selected_field, Some(ConfigField::SkillId));
    assert_eq!(state.selected_agent_index, 0);
    assert_eq!(
        state.agent_warnings,
        vec!["invalid agent ignored".to_owned()]
    );
    assert_eq!(
        state.field_text_value(ConfigField::SkillId),
        Some("explore")
    );
    assert_eq!(state.display_value(ConfigField::SkillId), "explore");
    assert!(state.field_text_value_mut(ConfigField::SkillId).is_none());

    assert_eq!(state.move_agent(true), ConfigFieldMove::Moved);
    assert_eq!(state.selected_agent_index, 1);
    assert_eq!(state.field_text_value(ConfigField::SkillId), Some("review"));
    assert_eq!(state.move_agent(true), ConfigFieldMove::Boundary);
    assert!(state.cycle_agent(true));
    assert_eq!(state.selected_agent_index, 0);
    assert!(state.cycle_agent(false));
    assert_eq!(state.selected_agent_index, 1);
    assert_eq!(state.move_agent(false), ConfigFieldMove::Moved);
    assert_eq!(state.selected_agent_index, 0);
    assert_eq!(state.move_agent(false), ConfigFieldMove::Boundary);
}

#[test]
fn config_state_handles_plugin_collection_navigation() {
    let mut state = ConfigState::from_root_config(&test_root_config());

    state.set_section(ConfigSection::Plugins);
    assert_eq!(state.selected_field, None);
    assert_eq!(state.move_field(true), ConfigFieldMove::Unavailable);
    assert_eq!(state.move_plugin(true), ConfigFieldMove::Unavailable);
    assert!(!state.focus_last_field());
    assert!(!state.cycle_plugin(true));

    state.set_plugin_discovery(
        vec![test_plugin("repo-review"), test_plugin("policy")],
        vec!["invalid plugin ignored".to_owned()],
    );
    assert_eq!(state.selected_field, Some(ConfigField::PluginId));
    assert_eq!(state.selected_plugin_index, 0);
    assert_eq!(
        state.plugin_warnings,
        vec!["invalid plugin ignored".to_owned()]
    );
    assert_eq!(
        state.field_text_value(ConfigField::PluginId),
        Some("repo-review")
    );
    assert_eq!(state.display_value(ConfigField::PluginId), "repo-review");
    assert!(state.field_text_value_mut(ConfigField::PluginId).is_none());
    assert!(!config_field_accepts_char(ConfigField::PluginId, 'x'));

    assert_eq!(state.move_plugin(true), ConfigFieldMove::Moved);
    assert_eq!(state.selected_plugin_index, 1);
    assert_eq!(state.move_plugin(true), ConfigFieldMove::Boundary);
    assert_eq!(state.move_plugin(false), ConfigFieldMove::Moved);
    assert_eq!(state.selected_plugin_index, 0);
    assert_eq!(state.move_plugin(false), ConfigFieldMove::Boundary);
    assert!(state.cycle_plugin(true));
    assert_eq!(state.selected_plugin_index, 1);
    assert_eq!(
        state.field_text_value(ConfigField::PluginId),
        Some("policy")
    );
    assert!(state.cycle_plugin(false));
    assert_eq!(state.selected_plugin_index, 0);
}

#[test]
fn config_state_moves_fields_and_footer_boundaries() {
    let mut state = ConfigState::from_root_config(&test_root_config());

    assert_eq!(state.selected_field, Some(ConfigField::ProviderModel));
    assert_eq!(state.move_field(false), ConfigFieldMove::Boundary);
    assert!(state.focus_field(ConfigField::ProviderApiKey));
    assert_eq!(state.selected_field, Some(ConfigField::ProviderApiKey));
    assert!(!state.focus_field(ConfigField::McpName));
    assert_eq!(state.selected_field, Some(ConfigField::ProviderApiKey));
    assert_eq!(state.move_field(true), ConfigFieldMove::Moved);
    assert_eq!(
        state.selected_field,
        Some(ConfigField::ModelRequestTimeoutSecs)
    );
    state.focus_footer(ConfigFooterAction::Close);
    assert!(state.footer_selected);
    state.move_footer_action(false);
    assert_eq!(state.selected_footer_action, ConfigFooterAction::Save);
    assert!(state.focus_last_field());
    assert_eq!(state.selected_field, Some(ConfigField::ProviderName));
    assert!(!state.footer_selected);

    state.set_section(ConfigSection::Mcp);
    assert!(!state.focus_field(ConfigField::McpName));
}

#[test]
fn config_draft_serializes_provider_compaction_and_mcp_servers() -> anyhow::Result<()> {
    let mut draft = ConfigDraft::from_root_config(&test_root_config());
    draft.provider_model = " deepseek-v4-pro ".to_owned();
    draft.provider_api_key = " ".to_owned();
    draft.provider_base_url = " https://proxy.example.test ".to_owned();
    draft.provider_beta_base_url = " https://proxy.example.test/beta ".to_owned();
    draft.provider_anthropic_base_url = " https://proxy.example.test/anthropic ".to_owned();
    draft.provider_user_id_strategy = " ".to_owned();
    draft.provider_fim_model = " deepseek-v4-pro ".to_owned();
    draft.model_request_timeout_secs = "60".to_owned();
    draft.model_request_stream_idle_timeout_secs = "90".to_owned();
    draft.permission_preset = sigil_kernel::PermissionPreset::ReadOnly;
    draft.permission_default_mode = sigil_kernel::ApprovalMode::Deny;
    draft.memory_enabled = true;
    draft.compaction_enabled = true;
    draft.compaction_soft_threshold_ratio = "0.5".to_owned();
    draft.compaction_hard_threshold_ratio = "0.75".to_owned();
    draft.compaction_context_window_tokens = "128000".to_owned();
    draft.compaction_tail_messages = "8".to_owned();
    draft.mcp_servers = vec![McpServerDraft {
        name: "test-mcp".to_owned(),
        command: "node".to_owned(),
        args_csv: "server.js, --stdio, ".to_owned(),
        startup_timeout_secs: "15".to_owned(),
    }];

    let config = draft.to_root_config()?;
    let provider = config.providers["deepseek"]
        .as_object()
        .expect("provider should serialize as object");

    assert_eq!(config.agent.model, "deepseek-v4-pro");
    assert_eq!(
        config.permission.preset,
        sigil_kernel::PermissionPreset::ReadOnly
    );
    assert_eq!(
        config.permission.default_mode,
        sigil_kernel::ApprovalMode::Deny
    );
    assert_eq!(config.compaction.context_window_tokens, Some(128000));
    assert_eq!(config.compaction.tail_messages, 8);
    assert_eq!(config.model_request.request_timeout_secs, 60);
    assert_eq!(config.model_request.stream_idle_timeout_secs, 90);
    assert!(provider.get("api_key").is_none());
    assert_eq!(provider["base_url"], "https://proxy.example.test");
    assert!(provider.get("request_timeout_secs").is_none());
    assert!(provider.get("user_id_strategy").is_none());
    assert_eq!(config.mcp_servers.len(), 1);
    assert_eq!(config.mcp_servers[0].args, vec!["server.js", "--stdio"]);
    assert_eq!(config.mcp_servers[0].startup_timeout_secs, 15);
    Ok(())
}

#[test]
fn config_draft_serializes_openai_compat_provider() -> anyhow::Result<()> {
    let mut root_config = test_root_config();
    root_config.agent.provider = OPENAI_COMPAT_PROVIDER_KEY.to_owned();
    root_config.agent.model = "gpt-old".to_owned();
    root_config.providers.insert(
        "openai_compat".to_owned(),
        serde_json::json!({
            "base_url": "https://openai.example.com/v1",
            "api_key": "old-key"
        }),
    );

    let mut state = ConfigState::from_root_config(&root_config);
    assert_eq!(state.draft.provider_name, OPENAI_COMPAT_PROVIDER_KEY);
    assert_eq!(
        state.display_value(ConfigField::ProviderFimModel),
        "not supported"
    );

    state.draft.provider_model = " gpt-new ".to_owned();
    state.draft.provider_api_key = " new-key ".to_owned();
    state.draft.provider_base_url = " https://proxy.example.test/v1 ".to_owned();
    state.draft.provider_fim_model = " ".to_owned();
    state.draft.model_request_timeout_secs = "45".to_owned();

    let config = state.draft.to_root_config()?;
    let provider = config.providers[OPENAI_COMPAT_PROVIDER_KEY]
        .as_object()
        .expect("openai_compat should serialize as object");

    assert_eq!(config.agent.provider, OPENAI_COMPAT_PROVIDER_KEY);
    assert_eq!(config.agent.model, "gpt-new");
    assert!(provider.get("model").is_none());
    assert_eq!(provider["api_key"], "new-key");
    assert_eq!(provider["base_url"], "https://proxy.example.test/v1");
    assert_eq!(config.model_request.request_timeout_secs, 45);
    assert!(provider.get("request_timeout_secs").is_none());
    Ok(())
}

#[test]
fn config_draft_serializes_anthropic_provider() -> anyhow::Result<()> {
    let mut root_config = test_root_config();
    root_config.agent.provider = "anthropic".to_owned();
    root_config.agent.model = "claude-old".to_owned();
    root_config.providers.insert(
        "anthropic".to_owned(),
        serde_json::json!({
            "base_url": "https://anthropic.example.com",
            "api_key": "old-key",
            "anthropic_version": "2023-06-01",
            "max_tokens": 1024
        }),
    );

    let mut state = ConfigState::from_root_config(&root_config);
    assert_eq!(state.draft.provider_name, ANTHROPIC_PROVIDER_KEY);
    assert_eq!(
        state.display_value(ConfigField::ProviderFimModel),
        "not supported"
    );

    state.draft.provider_model = " claude-new ".to_owned();
    state.draft.provider_api_key = " new-key ".to_owned();
    state.draft.provider_base_url = " https://proxy.example.test ".to_owned();
    state.draft.model_request_timeout_secs = "45".to_owned();

    let config = state.draft.to_root_config()?;
    let provider = config.providers[ANTHROPIC_PROVIDER_KEY]
        .as_object()
        .expect("anthropic should serialize as object");

    assert_eq!(config.agent.provider, ANTHROPIC_PROVIDER_KEY);
    assert_eq!(config.agent.model, "claude-new");
    assert!(provider.get("model").is_none());
    assert_eq!(provider["api_key"], "new-key");
    assert_eq!(provider["base_url"], "https://proxy.example.test");
    assert_eq!(provider["anthropic_version"], "2023-06-01");
    assert_eq!(provider["max_tokens"], 1024);
    assert_eq!(config.model_request.request_timeout_secs, 45);
    assert!(provider.get("request_timeout_secs").is_none());
    Ok(())
}

#[test]
fn config_draft_serializes_gemini_provider() -> anyhow::Result<()> {
    let mut root_config = test_root_config();
    root_config.agent.provider = "gemini".to_owned();
    root_config.agent.model = "gemini-old".to_owned();
    root_config.providers.insert(
        "gemini".to_owned(),
        serde_json::json!({
            "base_url": "https://gemini.example.com/v1beta",
            "api_key": "old-key"
        }),
    );

    let mut state = ConfigState::from_root_config(&root_config);
    assert_eq!(state.draft.provider_name, GEMINI_PROVIDER_KEY);
    assert_eq!(
        state.display_value(ConfigField::ProviderFimModel),
        "not supported"
    );

    state.draft.provider_model = " gemini-new ".to_owned();
    state.draft.provider_api_key = " new-key ".to_owned();
    state.draft.provider_base_url = " https://proxy.example.test/v1beta ".to_owned();
    state.draft.model_request_timeout_secs = "46".to_owned();

    let config = state.draft.to_root_config()?;
    let provider = config.providers[GEMINI_PROVIDER_KEY]
        .as_object()
        .expect("gemini should serialize as object");

    assert_eq!(config.agent.provider, GEMINI_PROVIDER_KEY);
    assert_eq!(config.agent.model, "gemini-new");
    assert!(provider.get("model").is_none());
    assert_eq!(provider["api_key"], "new-key");
    assert_eq!(provider["base_url"], "https://proxy.example.test/v1beta");
    assert_eq!(config.model_request.request_timeout_secs, 46);
    assert!(provider.get("request_timeout_secs").is_none());
    Ok(())
}

#[test]
fn provider_name_helpers_preserve_unknown_names_and_cycle_known_providers() {
    assert_eq!(normalize_provider_name("deepseek"), "deepseek");
    assert_eq!(normalize_provider_name("openai_compat"), "openai_compat");
    assert_eq!(normalize_provider_name("anthropic"), "anthropic");
    assert_eq!(normalize_provider_name("gemini"), "gemini");
    assert_eq!(
        normalize_provider_name("openai-compatible"),
        "openai-compatible"
    );
    assert_eq!(normalize_provider_name("claude"), "claude");
    assert_eq!(normalize_provider_name("google"), "google");
    assert_eq!(cycle_provider_name("deepseek"), "openai_compat");
    assert_eq!(cycle_provider_name("openai_compat"), "anthropic");
    assert_eq!(cycle_provider_name("anthropic"), "gemini");
    assert_eq!(cycle_provider_name("gemini"), "deepseek");
}

#[test]
fn config_draft_validates_provider_and_compaction_values() {
    let base = ConfigDraft::from_root_config(&test_root_config());

    for (draft, expected) in [
        {
            let mut draft = base.clone();
            draft.provider_model = " ".to_owned();
            (draft, "model cannot be empty")
        },
        {
            let mut draft = base.clone();
            draft.provider_base_url = " ".to_owned();
            (draft, "base_url cannot be empty")
        },
        {
            let mut draft = base.clone();
            draft.provider_name = "claude".to_owned();
            (
                draft,
                "unsupported provider claude; expected one of deepseek, openai_compat, anthropic, or gemini",
            )
        },
        {
            let mut draft = base.clone();
            draft.provider_beta_base_url = " ".to_owned();
            (draft, "beta_base_url cannot be empty")
        },
        {
            let mut draft = base.clone();
            draft.provider_anthropic_base_url = " ".to_owned();
            (draft, "anthropic_base_url cannot be empty")
        },
        {
            let mut draft = base.clone();
            draft.provider_fim_model = " ".to_owned();
            (draft, "fim_model cannot be empty")
        },
        {
            let mut draft = base.clone();
            draft.model_request_timeout_secs = "abc".to_owned();
            (
                draft,
                "model_request.request_timeout_secs must be a positive integer",
            )
        },
        {
            let mut draft = base.clone();
            draft.model_request_timeout_secs = "0".to_owned();
            (
                draft,
                "model_request.request_timeout_secs must be greater than 0",
            )
        },
        {
            let mut draft = base.clone();
            draft.compaction_soft_threshold_ratio = "not-a-ratio".to_owned();
            (draft, "soft_threshold_ratio must be a decimal number")
        },
        {
            let mut draft = base.clone();
            draft.compaction_hard_threshold_ratio = "not-a-ratio".to_owned();
            (draft, "hard_threshold_ratio must be a decimal number")
        },
        {
            let mut draft = base.clone();
            draft.compaction_soft_threshold_ratio = "1.5".to_owned();
            (draft, "soft_threshold_ratio must be between 0.0 and 1.0")
        },
        {
            let mut draft = base.clone();
            draft.compaction_soft_threshold_ratio = "0.8".to_owned();
            draft.compaction_hard_threshold_ratio = "0.5".to_owned();
            (
                draft,
                "hard_threshold_ratio must be greater than or equal to soft_threshold_ratio",
            )
        },
        {
            let mut draft = base.clone();
            draft.compaction_context_window_tokens = "abc".to_owned();
            (
                draft,
                "fallback_context_window_tokens must be a positive integer",
            )
        },
        {
            let mut draft = base.clone();
            draft.compaction_context_window_tokens = "0".to_owned();
            (
                draft,
                "fallback_context_window_tokens must be greater than 0",
            )
        },
        {
            let mut draft = base.clone();
            draft.compaction_tail_messages = "abc".to_owned();
            (draft, "tail_messages must be a positive integer")
        },
        {
            let mut draft = base.clone();
            draft.compaction_tail_messages = "0".to_owned();
            (draft, "tail_messages must be greater than 0")
        },
        {
            let mut draft = base.clone();
            draft.terminal_scroll_sensitivity = "0".to_owned();
            (draft, "scroll_sensitivity must be greater than 0")
        },
    ] {
        let error = draft.to_root_config().expect_err(expected);
        assert!(
            error.to_string().contains(expected),
            "{error:#} should contain {expected}"
        );
    }
}

#[test]
fn config_draft_validates_mcp_server_values() {
    let base = ConfigDraft::from_root_config(&test_root_config());

    for (draft, expected) in [
        {
            let mut draft = base.clone();
            draft.mcp_servers = vec![McpServerDraft {
                name: " ".to_owned(),
                command: "node".to_owned(),
                args_csv: String::new(),
                startup_timeout_secs: "10".to_owned(),
            }];
            (draft, "mcp server 1 name cannot be empty")
        },
        {
            let mut draft = base.clone();
            draft.mcp_servers = vec![McpServerDraft {
                name: "server".to_owned(),
                command: " ".to_owned(),
                args_csv: String::new(),
                startup_timeout_secs: "10".to_owned(),
            }];
            (draft, "mcp server 1 command cannot be empty")
        },
        {
            let mut draft = base.clone();
            draft.mcp_servers = vec![McpServerDraft {
                name: "server".to_owned(),
                command: "node".to_owned(),
                args_csv: String::new(),
                startup_timeout_secs: "abc".to_owned(),
            }];
            (
                draft,
                "mcp server 1 startup_timeout_secs must be a positive integer",
            )
        },
        {
            let mut draft = base.clone();
            draft.mcp_servers = vec![McpServerDraft {
                name: "server".to_owned(),
                command: "node".to_owned(),
                args_csv: String::new(),
                startup_timeout_secs: "0".to_owned(),
            }];
            (
                draft,
                "mcp server 1 startup_timeout_secs must be greater than 0",
            )
        },
    ] {
        let error = draft.to_root_config().expect_err(expected);
        assert!(
            error.to_string().contains(expected),
            "{error:#} should contain {expected}"
        );
    }
}

#[test]
fn config_field_character_filter_matches_field_kind() {
    assert!(config_field_accepts_char(
        ConfigField::CompactionContextWindowTokens,
        '7'
    ));
    assert!(config_field_accepts_char(
        ConfigField::TerminalScrollSensitivity,
        '7'
    ));
    assert!(!config_field_accepts_char(
        ConfigField::TerminalScrollSensitivity,
        '.'
    ));
    assert!(!config_field_accepts_char(
        ConfigField::CompactionContextWindowTokens,
        '.'
    ));
    assert!(config_field_accepts_char(
        ConfigField::CompactionSoftThresholdRatio,
        '.'
    ));
    assert!(!config_field_accepts_char(
        ConfigField::CompactionSoftThresholdRatio,
        'x'
    ));
    assert!(config_field_accepts_char(ConfigField::McpArgsCsv, ','));
    assert!(!config_field_accepts_char(ConfigField::McpArgsCsv, '\n'));
    assert!(!config_field_accepts_char(ConfigField::ProviderApiKey, 'x'));
    assert!(!config_field_accepts_char(ConfigField::MemoryEnabled, '1'));
}

#[test]
fn config_display_helpers_cover_bool_ratio_and_serialized_defaults() -> anyhow::Result<()> {
    let serialized = sigil_runtime::deepseek_provider_value_for_setup("deepseek-v4-test", None)?;
    assert!(serialized.get("model").is_none());
    assert_eq!(serialized["fim_model"], "deepseek-v4-pro");
    assert!(
        !serialized
            .as_object()
            .expect("provider object")
            .contains_key("api_key")
    );

    let mut config = test_root_config();
    config.memory.enabled = true;
    config.compaction.soft_threshold_ratio = 0.25;
    config.compaction.hard_threshold_ratio = 0.5;
    config.compaction.context_window_tokens = Some(64000);
    let mut state = ConfigState::from_root_config(&config);

    assert_eq!(state.display_value(ConfigField::MemoryEnabled), "yes");
    assert_eq!(
        state.display_value(ConfigField::CompactionSoftThresholdRatio),
        "25% (0.25)"
    );
    assert_eq!(
        state.display_value(ConfigField::CompactionContextWindowTokens),
        "64000 tokens"
    );
    assert_eq!(state.display_value(ConfigField::TerminalMouseCapture), "no");
    assert_eq!(
        state.display_value(ConfigField::TerminalOsc52Clipboard),
        "yes"
    );
    assert_eq!(
        state.display_value(ConfigField::TerminalScrollSensitivity),
        "3 rows"
    );
    *state
        .field_text_value_mut(ConfigField::TerminalScrollSensitivity)
        .expect("terminal scroll sensitivity should be mutable") = "9".to_owned();
    assert_eq!(
        state.field_text_value(ConfigField::TerminalScrollSensitivity),
        Some("9")
    );

    let mut draft = state.draft.clone();
    draft.terminal_mouse_capture = false;
    draft.terminal_osc52_clipboard = false;
    draft.terminal_scroll_sensitivity = "7".to_owned();
    let root_config = draft.to_root_config()?;
    assert!(!root_config.terminal.mouse_capture);
    assert!(!root_config.terminal.osc52_clipboard);
    assert_eq!(root_config.terminal.scroll_sensitivity, 7);

    assert_eq!(display_ratio("not-a-number"), "not-a-number");
    Ok(())
}

#[test]
fn config_display_helpers_cover_permission_and_verification_labels() {
    assert!(
        ConfigField::VerificationAutoRun
            .help_text()
            .contains("trusted project checks")
    );

    let mut config = test_root_config();
    config.permission.preset = sigil_kernel::PermissionPreset::ReadOnly;
    config.permission.default_mode = sigil_kernel::ApprovalMode::Allow;
    config.verification.auto_run = sigil_kernel::VerificationAutoRunPolicy::TrustedOnly;
    let mut state = ConfigState::from_root_config(&config);

    assert_eq!(
        state.display_value(ConfigField::PermissionsPreset),
        "read only"
    );
    assert_eq!(
        state.display_value(ConfigField::PermissionsDefaultMode),
        "full access"
    );
    assert_eq!(
        state.display_value(ConfigField::VerificationAutoRun),
        "auto trusted"
    );

    state.draft.permission_preset = sigil_kernel::PermissionPreset::Balanced;
    state.draft.permission_default_mode = sigil_kernel::ApprovalMode::Deny;
    state.draft.verification_auto_run = sigil_kernel::VerificationAutoRunPolicy::Never;
    assert_eq!(
        state.display_value(ConfigField::PermissionsPreset),
        "balanced"
    );
    assert_eq!(
        state.display_value(ConfigField::PermissionsDefaultMode),
        "locked down"
    );
    assert_eq!(state.display_value(ConfigField::VerificationAutoRun), "off");
}

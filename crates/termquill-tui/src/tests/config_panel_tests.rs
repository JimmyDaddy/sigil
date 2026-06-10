use super::*;

#[test]
fn config_section_flow_wraps() {
    assert_eq!(
        ConfigSection::Provider.next_flow(),
        ConfigSection::Permissions
    );
    assert_eq!(ConfigSection::Mcp.next_flow(), ConfigSection::Provider);
    assert_eq!(ConfigSection::Provider.previous_flow(), ConfigSection::Mcp);
}

#[test]
fn config_footer_action_navigation_wraps() {
    assert_eq!(
        ConfigFooterAction::Save.next(),
        ConfigFooterAction::SaveAndClose
    );
    assert_eq!(ConfigFooterAction::Close.next(), ConfigFooterAction::Save);
    assert_eq!(
        ConfigFooterAction::Save.previous(),
        ConfigFooterAction::Close
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
        session: Default::default(),
        agent: termquill_kernel::AgentConfig {
            provider: "deepseek".to_owned(),
            model: "deepseek-v4-pro".to_owned(),
            max_turns: None,
            tool_timeout_secs: 30,
        },
        permission: Default::default(),
        memory: Default::default(),
        compaction: Default::default(),
        code_intelligence: Default::default(),
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
        session: Default::default(),
        agent: termquill_kernel::AgentConfig {
            provider: "deepseek".to_owned(),
            model: "deepseek-v4-flash".to_owned(),
            max_turns: None,
            tool_timeout_secs: 30,
        },
        permission: Default::default(),
        memory: Default::default(),
        compaction: Default::default(),
        code_intelligence: Default::default(),
        providers: Default::default(),
        mcp_servers: Vec::new(),
    });

    assert_eq!(
        render_config_value_row(&state, ConfigField::ProviderModel),
        "> Model: deepseek-v4-flash  [Enter choose]"
    );
    assert_eq!(
        render_config_readonly_row("Root files", "TERMQUILL.md"),
        "- Root files: TERMQUILL.md"
    );
}

#[test]
fn api_key_display_uses_status_without_secret_length() {
    let mut config = RootConfig {
        workspace: Default::default(),
        session: Default::default(),
        agent: termquill_kernel::AgentConfig {
            provider: "deepseek".to_owned(),
            model: "deepseek-v4-flash".to_owned(),
            max_turns: None,
            tool_timeout_secs: 30,
        },
        permission: Default::default(),
        memory: Default::default(),
        compaction: Default::default(),
        code_intelligence: Default::default(),
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

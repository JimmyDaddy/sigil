use super::*;

fn test_root_config() -> RootConfig {
    RootConfig {
        workspace: Default::default(),
        session: Default::default(),
        agent: sigil_kernel::AgentConfig {
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
    }
}

#[test]
fn config_section_flow_wraps() {
    assert_eq!(
        ConfigSection::Provider.next_flow(),
        ConfigSection::Permissions
    );
    assert_eq!(
        ConfigSection::Compaction.next_flow(),
        ConfigSection::CodeIntelligence
    );
    assert_eq!(
        ConfigSection::CodeIntelligence.next_flow(),
        ConfigSection::Mcp
    );
    assert_eq!(ConfigSection::Mcp.next_flow(), ConfigSection::Provider);
    assert_eq!(ConfigSection::Provider.previous_flow(), ConfigSection::Mcp);
}

#[test]
fn config_footer_action_navigation_wraps() {
    assert_eq!(
        ConfigFooterAction::Save.next_for_section(ConfigSection::Provider),
        ConfigFooterAction::SaveAndClose
    );
    assert_eq!(
        ConfigFooterAction::Close.next_for_section(ConfigSection::Provider),
        ConfigFooterAction::Save
    );
    assert_eq!(
        ConfigFooterAction::Save.previous_for_section(ConfigSection::Provider),
        ConfigFooterAction::Close
    );
    assert_eq!(
        ConfigFooterAction::SaveAndClose.next_for_section(ConfigSection::Mcp),
        ConfigFooterAction::ActivateMcp
    );
    assert_eq!(
        ConfigFooterAction::ActivateMcp.next_for_section(ConfigSection::Mcp),
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
        agent: sigil_kernel::AgentConfig {
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
        agent: sigil_kernel::AgentConfig {
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
        render_config_readonly_row("Root files", "SIGIL.md"),
        "- Root files: SIGIL.md"
    );
}

#[test]
fn api_key_display_uses_status_without_secret_length() {
    let mut config = RootConfig {
        workspace: Default::default(),
        session: Default::default(),
        agent: sigil_kernel::AgentConfig {
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

#[test]
fn config_field_metadata_covers_all_user_facing_fields() {
    assert_eq!(ConfigSection::Permissions.summary(), "approval rules");
    assert_eq!(ConfigSection::Provider.flow_index(), Some(0));
    assert_eq!(ConfigSection::CodeIntelligence.flow_index(), Some(4));
    assert_eq!(ConfigSection::Mcp.flow_index(), Some(5));
    assert_eq!(
        ConfigField::fields_for_section(ConfigSection::CodeIntelligence),
        &[
            ConfigField::CodeIntelEnabled,
            ConfigField::CodeIntelStartup,
            ConfigField::CodeIntelDiscoveryEnabled,
            ConfigField::CodeIntelDiscoveryReportMissing,
        ]
    );
    assert_eq!(
        ConfigField::fields_for_section(ConfigSection::Mcp),
        &[
            ConfigField::McpName,
            ConfigField::McpCommand,
            ConfigField::McpArgsCsv,
            ConfigField::McpStartupTimeoutSecs,
        ]
    );

    assert_eq!(ConfigField::McpCommand.label(), "command");
    assert_eq!(ConfigField::McpArgsCsv.label(), "args_csv");
    assert_eq!(ConfigField::CodeIntelStartup.label(), "startup");
    assert_eq!(ConfigField::ProviderApiKey.action_label(), "Enter input");
    assert_eq!(ConfigField::CodeIntelStartup.action_label(), "Enter cycle");
    assert_eq!(ConfigField::CodeIntelEnabled.action_label(), "Enter toggle");
    assert_eq!(ConfigField::McpCommand.action_label(), "Enter input");
    assert_eq!(ConfigFooterAction::ActivateMcp.button_label(), "activate");
    assert_eq!(
        ConfigFooterAction::SaveAndClose.field_label(),
        "save_and_close"
    );
    assert!(
        ConfigField::ProviderApiKey
            .help_text()
            .contains("SIGIL_API_KEY")
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
    assert!(
        ConfigField::CodeIntelDiscoveryEnabled
            .help_text()
            .contains("language servers")
    );
    assert!(
        ConfigField::CodeIntelStartup
            .help_text()
            .contains("lazily started")
    );
    assert!(
        ConfigField::CodeIntelDiscoveryReportMissing
            .help_text()
            .contains("readiness warnings")
    );
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
    assert_eq!(state.selected_field, Some(ConfigField::McpName));
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
        state.field_text_value(ConfigField::CodeIntelDiscoveryReportMissing),
        None
    );
    assert!(
        state
            .field_text_value_mut(ConfigField::CodeIntelDiscoveryReportMissing)
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
fn config_state_moves_fields_and_footer_boundaries() {
    let mut state = ConfigState::from_root_config(&test_root_config());

    assert_eq!(state.selected_field, Some(ConfigField::ProviderModel));
    assert_eq!(state.move_field(false), ConfigFieldMove::Boundary);
    assert_eq!(state.move_field(true), ConfigFieldMove::Moved);
    assert_eq!(state.selected_field, Some(ConfigField::ProviderApiKey));
    state.focus_footer(ConfigFooterAction::Close);
    assert!(state.footer_selected);
    state.move_footer_action(false);
    assert_eq!(
        state.selected_footer_action,
        ConfigFooterAction::SaveAndClose
    );
    assert!(state.focus_last_field());
    assert_eq!(state.selected_field, Some(ConfigField::ProviderFimModel));
    assert!(!state.footer_selected);
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
    draft.provider_request_timeout_secs = "60".to_owned();
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
    let provider = load_deepseek_provider_config(&config).expect("provider should serialize");

    assert_eq!(config.agent.model, "deepseek-v4-pro");
    assert_eq!(
        config.permission.default_mode,
        sigil_kernel::ApprovalMode::Deny
    );
    assert_eq!(config.compaction.context_window_tokens, Some(128000));
    assert_eq!(config.compaction.tail_messages, 8);
    assert_eq!(provider.api_key, None);
    assert_eq!(provider.base_url, "https://proxy.example.test");
    assert_eq!(
        provider.user_id_strategy.as_deref(),
        Some("stable_per_end_user")
    );
    assert!(
        config.providers["deepseek"]
            .as_object()
            .expect("provider should serialize as object")
            .get("user_id_strategy")
            .is_none()
    );
    assert_eq!(config.mcp_servers.len(), 1);
    assert_eq!(config.mcp_servers[0].args, vec!["server.js", "--stdio"]);
    assert_eq!(config.mcp_servers[0].startup_timeout_secs, 15);
    Ok(())
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
            draft.provider_request_timeout_secs = "abc".to_owned();
            (draft, "request_timeout_secs must be a positive integer")
        },
        {
            let mut draft = base.clone();
            draft.provider_request_timeout_secs = "0".to_owned();
            (draft, "request_timeout_secs must be greater than 0")
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
    let provider = default_deepseek_provider_config("deepseek-v4-test");
    let serialized = serialize_deepseek_provider_value(&provider)?;
    assert_eq!(serialized["model"], "deepseek-v4-test");
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
    let state = ConfigState::from_root_config(&config);

    assert_eq!(state.display_value(ConfigField::MemoryEnabled), "yes");
    assert_eq!(
        state.display_value(ConfigField::CompactionSoftThresholdRatio),
        "25% (0.25)"
    );
    assert_eq!(
        state.display_value(ConfigField::CompactionContextWindowTokens),
        "64000 tokens"
    );
    assert_eq!(display_ratio("not-a-number"), "not-a-number");
    Ok(())
}

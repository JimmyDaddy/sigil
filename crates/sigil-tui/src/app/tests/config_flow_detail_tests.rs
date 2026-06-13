use super::*;
use crate::app::tests::common::test_config;

#[test]
fn detail_helpers_cover_selection_rows_and_hint_rendering() {
    let root_config = test_config();
    let mut state = ConfigState::from_root_config(&root_config);

    state.selected_field = None;
    let empty_details = render_config_selection_details(&state);
    assert!(empty_details.contains(&CONFIG_CONTROLS_HINT.to_owned()));
    assert!(empty_details.contains(&CONFIG_ACTIONS_HINT.to_owned()));

    state.selected_section = ConfigSection::Provider;
    state.selected_field = Some(ConfigField::ProviderApiKey);
    let api_key_details = render_config_selection_details(&state).join("\n");
    assert!(api_key_details.contains("override: SIGIL_API_KEY"));
    assert!(api_key_details.contains("storage: saved api_key is plaintext in sigil.toml"));

    state.selected_field = Some(ConfigField::ProviderFimModel);
    let fim_details = render_config_selection_details(&state).join("\n");
    assert!(fim_details.contains("advanced: provider-specific fields remain"));

    state.selected_section = ConfigSection::Mcp;
    state.selected_field = None;
    let mcp_details = render_config_selection_details(&state).join("\n");
    assert!(mcp_details.contains("mcp: Ctrl-N add"));

    assert_eq!(cycle_approval_mode(ApprovalMode::Allow), ApprovalMode::Ask);
    assert_eq!(cycle_approval_mode(ApprovalMode::Ask), ApprovalMode::Deny);
    assert_eq!(cycle_approval_mode(ApprovalMode::Deny), ApprovalMode::Allow);
    assert_eq!(
        config_context_window_source_label(ContextWindowSource::Provider),
        "provider"
    );
    assert_eq!(
        config_context_window_source_label(ContextWindowSource::Config),
        "fallback"
    );
    assert_eq!(
        config_context_window_source_label(ContextWindowSource::None),
        "none"
    );
    assert_eq!(bool_summary(true), "yes");
    assert_eq!(bool_summary(false), "no");
    assert_eq!(render_config_hint_row("Missing"), "i Missing");
    assert_eq!(
        cycle_code_intel_startup(sigil_kernel::CodeIntelStartup::Off),
        sigil_kernel::CodeIntelStartup::Lazy
    );
    assert_eq!(
        cycle_code_intel_startup(sigil_kernel::CodeIntelStartup::Eager),
        sigil_kernel::CodeIntelStartup::Off
    );
}

#[test]
fn code_intelligence_detail_helpers_cover_status_edges_and_overflow() {
    let ok_check = sigil_runtime::doctor::DoctorCheck {
        status: sigil_runtime::doctor::DoctorStatus::Ok,
        name: "code_intelligence".to_owned(),
        message: "disabled".to_owned(),
        remediation: None,
    };
    let error_check = sigil_runtime::doctor::DoctorCheck {
        status: sigil_runtime::doctor::DoctorStatus::Error,
        name: "lsp:bad".to_owned(),
        message: "command=empty".to_owned(),
        remediation: Some("set command".to_owned()),
    };

    assert_eq!(code_intelligence_overall_label(&[ok_check]), "ok");
    assert_eq!(
        code_intelligence_overall_label(std::slice::from_ref(&error_check)),
        "error"
    );
    assert_eq!(
        render_code_intelligence_check_row(&error_check),
        "- lsp:bad: error · command=empty"
    );

    let mut config = test_config();
    config.code_intelligence.enabled = true;
    config.code_intelligence.discovery.enabled = false;
    config.code_intelligence.servers = (0..5)
        .map(|index| sigil_kernel::LanguageServerConfig {
            name: format!("missing-{index}"),
            languages: vec!["rust".to_owned()],
            command: format!("./missing-{index}"),
            args: Vec::new(),
            env: Default::default(),
            root_markers: vec!["Cargo.toml".to_owned()],
            file_extensions: vec!["rs".to_owned()],
            initialization_options: Default::default(),
            trust_required: true,
            startup_timeout_ms: 5_000,
        })
        .collect();
    let mut app = AppState::from_root_config(std::path::Path::new("sigil.toml"), &config);
    app.open_config_panel();
    app.config_state
        .as_mut()
        .expect("config state should exist")
        .set_section(ConfigSection::CodeIntelligence);

    let detail = app.config_detail_lines().join("\n");

    assert!(detail.contains("... 1 more checks"));
}

#[test]
fn detail_helpers_cover_permission_rule_and_mcp_summaries() {
    let mut root_config = test_config();
    root_config.permission.rules = vec![
        sigil_kernel::PermissionRule {
            tool_name: Some("read_file".to_owned()),
            subject_glob: Some("src/**".to_owned()),
            mode: ApprovalMode::Allow,
        },
        sigil_kernel::PermissionRule {
            tool_name: Some("write_file".to_owned()),
            subject_glob: Some("docs/**".to_owned()),
            mode: ApprovalMode::Ask,
        },
        sigil_kernel::PermissionRule {
            tool_name: Some("bash".to_owned()),
            subject_glob: Some("tests/**".to_owned()),
            mode: ApprovalMode::Deny,
        },
        sigil_kernel::PermissionRule {
            tool_name: Some("glob".to_owned()),
            subject_glob: Some("**/*.rs".to_owned()),
            mode: ApprovalMode::Allow,
        },
        sigil_kernel::PermissionRule {
            tool_name: Some("grep".to_owned()),
            subject_glob: Some("**/*.md".to_owned()),
            mode: ApprovalMode::Ask,
        },
    ];
    root_config.mcp_servers.push(sigil_kernel::McpServerConfig {
        name: "filesystem".to_owned(),
        command: "mcp-filesystem".to_owned(),
        required: false,
        trust: sigil_kernel::McpServerTrustPolicy {
            allow_secrets: true,
            pin_version: true,
            pinned: Some(sigil_kernel::McpServerPinnedIdentity {
                command_fingerprint: "sha256:abc".to_owned(),
                protocol_version: "2024-11-05".to_owned(),
                server_name: "filesystem".to_owned(),
                server_version: "1.0.0".to_owned(),
            }),
            ..Default::default()
        },
        ..Default::default()
    });
    let state = ConfigState::from_root_config(&root_config);

    let rule_lines = render_permission_rule_summary(&state).join("\n");
    assert!(rule_lines.contains("Rule overrides"));
    assert!(rule_lines.contains("... 1 more rules in config file"));

    let lifecycle = render_mcp_lifecycle_summary(&state, "ready").join("\n");
    assert!(lifecycle.contains("- Runtime: ready"));
    assert!(lifecycle.contains("- Required: no"));
    assert!(lifecycle.contains("- Pin: pinned"));
    assert!(lifecycle.contains("- Secrets: allowed"));
    assert_eq!(
        mcp_pin_summary(&sigil_kernel::McpServerConfig::default()),
        "off"
    );
}

#[test]
fn effective_context_window_helper_prefers_provider_then_fallback() {
    let root_config = test_config();
    let mut state = ConfigState::from_root_config(&root_config);
    assert_eq!(
        render_effective_context_window(&state),
        "1,000,000 tokens  source=provider"
    );

    state.draft.provider_model = "custom-model".to_owned();
    state.draft.compaction_context_window_tokens = "2048".to_owned();
    assert_eq!(
        render_effective_context_window(&state),
        "2,048 tokens  source=fallback"
    );

    state.draft.compaction_context_window_tokens = "0".to_owned();
    assert_eq!(
        render_effective_context_window(&state),
        "unknown  source=none"
    );
}

#[test]
fn config_private_helpers_cover_missing_snapshot_and_save_guards() -> anyhow::Result<()> {
    let root_config = test_config();
    let mut app = AppState::from_root_config(std::path::Path::new("sigil.toml"), &root_config);

    app.config_snapshot = None;
    app.open_config_panel();
    assert_eq!(
        app.last_notice.as_deref(),
        Some("config is unavailable in setup mode")
    );

    assert!(app.config_nav_lines().is_empty());
    assert!(app.config_detail_lines().is_empty());
    assert!(!app.config_is_dirty());
    assert_eq!(app.config_editing_field_label(), None);
    assert!(app.attempt_close_config()?.is_none());
    assert!(app.save_config_draft()?.is_none());

    app.config_state = Some(ConfigState::from_root_config(&root_config));
    app.config_state
        .as_mut()
        .expect("config state should exist")
        .dirty = true;
    assert!(app.config_status_summary().contains("unsaved"));

    app.modal_state = Some(ModalState::TextInput(TextInputState {
        target: TextInputTarget::ConfigField(ConfigField::ProviderBaseUrl),
        buffer: "https://api.deepseek.com".to_owned(),
    }));
    assert_eq!(app.config_editing_field_label(), Some("base_url"));

    app.modal_state = None;
    app.config_state = Some(ConfigState::from_root_config(&root_config));
    app.config_state
        .as_mut()
        .expect("config state should exist")
        .draft
        .provider_model
        .clear();
    assert!(app.save_config_draft()?.is_none());
    assert_eq!(app.last_notice.as_deref(), Some("model cannot be empty"));
    assert!(app.events.iter().any(|event| {
        event.label == "config:error" && event.detail.contains("model cannot be empty")
    }));
    Ok(())
}

#[test]
fn activate_selected_mcp_server_guard_paths_cover_busy_section_snapshot_and_selection()
-> anyhow::Result<()> {
    let mut root_config = test_config();
    root_config.mcp_servers.push(sigil_kernel::McpServerConfig {
        name: "filesystem".to_owned(),
        command: "mcp-filesystem".to_owned(),
        startup: sigil_kernel::McpServerStartup::Lazy,
        ..Default::default()
    });
    let mut app = AppState::from_root_config(std::path::Path::new("sigil.toml"), &root_config);
    app.config_state = Some(ConfigState::from_root_config(&root_config));

    app.is_busy = true;
    assert!(app.activate_selected_mcp_server()?.is_none());
    assert_eq!(app.last_notice.as_deref(), Some("busy; activate MCP later"));

    app.is_busy = false;
    app.config_state
        .as_mut()
        .expect("config state should exist")
        .set_section(ConfigSection::Provider);
    assert!(app.activate_selected_mcp_server()?.is_none());
    assert_eq!(
        app.last_notice.as_deref(),
        Some("activate MCP is available in MCP config")
    );

    app.config_state
        .as_mut()
        .expect("config state should exist")
        .set_section(ConfigSection::Mcp);
    app.config_snapshot = None;
    assert!(app.activate_selected_mcp_server()?.is_none());
    assert_eq!(app.last_notice.as_deref(), Some("config is unavailable"));

    app.config_snapshot = Some(test_config());
    app.config_state = Some(ConfigState::from_root_config(&test_config()));
    app.config_state
        .as_mut()
        .expect("config state should exist")
        .set_section(ConfigSection::Mcp);
    assert!(app.activate_selected_mcp_server()?.is_none());
    assert_eq!(app.last_notice.as_deref(), Some("no MCP server selected"));
    Ok(())
}

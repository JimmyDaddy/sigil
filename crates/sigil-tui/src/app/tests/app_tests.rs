use super::*;

#[test]
fn from_root_config_initializes_mcp_statuses_from_startup_mode() {
    let mut config = test_config();
    config.mcp_servers.push(sigil_kernel::McpServerConfig {
        name: "eager".to_owned(),
        command: "mcp-eager".to_owned(),
        startup: McpServerStartup::Eager,
        ..Default::default()
    });
    config.mcp_servers.push(sigil_kernel::McpServerConfig {
        name: "lazy".to_owned(),
        command: "mcp-lazy".to_owned(),
        startup: McpServerStartup::Lazy,
        required: false,
        ..Default::default()
    });

    let app = AppState::from_root_config(Path::new("sigil.toml"), &config);

    assert_eq!(
        app.mcp_server_runtime_status_label("eager").as_deref(),
        Some("ready")
    );
    assert_eq!(
        app.mcp_server_runtime_status_label("lazy").as_deref(),
        Some("deferred")
    );
    assert_eq!(
        app.mcp_sidebar_lines(),
        vec!["eager: ready".to_owned(), "lazy: deferred".to_owned()]
    );
}

#[test]
fn mcp_sidebar_lines_are_empty_before_runtime_config_loads() -> Result<()> {
    let temp = tempdir()?;
    let app = AppState::from_setup(
        temp.path().join("sigil.toml"),
        temp.path().to_path_buf(),
        Some("missing config".to_owned()),
    );

    assert!(app.mcp_sidebar_lines().is_empty());
    Ok(())
}

#[test]
fn code_intelligence_sidebar_sorts_diagnostics_and_collapses_overflow() {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.code_intelligence_server_lines.insert(
        "rust-analyzer".to_owned(),
        "rust-analyzer: ready".to_owned(),
    );
    app.code_intelligence_diagnostics_line = Some("diagnostics: 8".to_owned());
    app.code_intelligence_diagnostics_by_path = std::collections::BTreeMap::from([
        (
            "src/a.rs".to_owned(),
            ApprovalDiagnosticSummary {
                errors: 1,
                warnings: 0,
            },
        ),
        (
            "src/b.rs".to_owned(),
            ApprovalDiagnosticSummary {
                errors: 3,
                warnings: 0,
            },
        ),
        (
            "src/c.rs".to_owned(),
            ApprovalDiagnosticSummary {
                errors: 3,
                warnings: 2,
            },
        ),
        (
            "src/d.rs".to_owned(),
            ApprovalDiagnosticSummary {
                errors: 1,
                warnings: 5,
            },
        ),
        (
            "src/e.rs".to_owned(),
            ApprovalDiagnosticSummary {
                errors: 0,
                warnings: 1,
            },
        ),
    ]);

    let lines = app.code_intelligence_sidebar_lines();
    let diagnostics_index = lines
        .iter()
        .position(|line| line == "latest diagnostics: 5 files")
        .expect("diagnostics header should be present");

    assert_eq!(
        lines.first().map(String::as_str),
        Some("rust-analyzer: ready")
    );
    assert_eq!(lines.get(1).map(String::as_str), Some("diagnostics: 8"));
    assert_eq!(
        lines.get(diagnostics_index + 1).map(String::as_str),
        Some("src/c.rs: 3 errors 2 warnings")
    );
    assert_eq!(
        lines.get(diagnostics_index + 2).map(String::as_str),
        Some("src/b.rs: 3 errors")
    );
    assert_eq!(
        lines.get(diagnostics_index + 3).map(String::as_str),
        Some("src/d.rs: 1 error 5 warnings")
    );
    assert_eq!(
        lines.get(diagnostics_index + 4).map(String::as_str),
        Some("src/a.rs: 1 error")
    );
    assert_eq!(lines.last().map(String::as_str), Some("+1 more files"));
}

#[test]
fn activity_pane_sidebar_keys_cover_permission_agents_usage_and_noop_paths() -> Result<()> {
    let temp = tempdir()?;
    let config_path = temp.path().join("sigil.toml");
    let config = test_config();
    config.save(&config_path)?;
    let mut app = AppState::from_root_config(&config_path, &config);
    app.active_pane = PaneFocus::Activity;
    app.sidebar_selected_card = SidebarCard::Permission;

    let action = app.handle_key_event(KeyEvent::new(KeyCode::BackTab, KeyModifiers::NONE))?;
    assert!(matches!(
        action,
        Some(AppAction::RuntimeConfigUpdated { .. })
    ));
    assert_eq!(app.permission_default_mode, "deny");

    app.active_pane = PaneFocus::Activity;
    app.sidebar_selected_card = SidebarCard::Permission;
    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE))?;
    assert_eq!(app.sidebar_selected_card, SidebarCard::Usage);
    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE))?;
    assert_eq!(app.sidebar_selected_card, SidebarCard::Agents);
    assert_eq!(app.sidebar_agent_selected, 0);
    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE))?;
    assert_eq!(app.sidebar_agent_selected, 1);
    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE))?;
    assert_eq!(app.sidebar_selected_card, SidebarCard::Usage);

    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Home, KeyModifiers::NONE))?;
    assert_eq!(app.sidebar_selected_card, SidebarCard::Permission);
    assert_eq!(app.sidebar_agent_selected, 0);
    let _ = app.handle_key_event(KeyEvent::new(KeyCode::End, KeyModifiers::NONE))?;
    assert_eq!(app.sidebar_selected_card, SidebarCard::Usage);

    app.sidebar_selected_card = SidebarCard::Agents;
    app.sidebar_agent_selected = 99;
    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))?;
    assert_eq!(app.last_notice(), Some("no agent selected"));
    assert!(
        app.timeline
            .iter()
            .any(|entry| entry.role == TimelineRole::Notice && entry.text == "no agent selected")
    );

    let before_input = app.input.clone();
    for key in [
        KeyCode::Char('x'),
        KeyCode::Backspace,
        KeyCode::Left,
        KeyCode::Right,
    ] {
        let _ = app.handle_key_event(KeyEvent::new(key, KeyModifiers::NONE))?;
        assert_eq!(app.input, before_input);
        assert_eq!(app.active_pane, PaneFocus::Activity);
    }

    app.is_busy = true;
    app.sidebar_selected_card = SidebarCard::Permission;
    let action = app.handle_key_event(KeyEvent::new(KeyCode::BackTab, KeyModifiers::NONE))?;
    assert!(action.is_none());
    assert_eq!(app.last_notice(), Some("busy; permission locked"));
    assert!(app.timeline.iter().any(|entry| {
        entry.role == TimelineRole::Notice && entry.text == "busy; permission mode stays unchanged"
    }));
    Ok(())
}

#[test]
fn composer_top_level_keys_cover_empty_submit_cursor_scroll_and_escape_paths() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());

    assert!(app.submit_input()?.is_none());

    app.input = "/".to_owned();
    let row_count = app.slash_selector_rows().len();
    assert!(row_count > 1);
    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE))?;
    let selected_after_down = app
        .slash_selector_selected_index()
        .expect("slash selector should have selected row");
    let _ = app.handle_key_event(KeyEvent::new(KeyCode::BackTab, KeyModifiers::SHIFT))?;
    assert_eq!(
        app.slash_selector_selected_index(),
        Some((selected_after_down + row_count - 1) % row_count)
    );
    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE))?;
    assert!(app.slash_selector_selected_index().is_some());

    app.input = "line one\nline two".to_owned();
    let first_line_cursor = "line".chars().count();
    app.input_cursor = first_line_cursor;
    app.active_pane = PaneFocus::Composer;
    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE))?;
    assert!(app.input_cursor > first_line_cursor);
    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Home, KeyModifiers::NONE))?;
    assert_eq!(app.input_cursor, 0);
    let _ = app.handle_key_event(KeyEvent::new(KeyCode::End, KeyModifiers::NONE))?;
    assert_eq!(app.input_cursor, app.input.chars().count());

    for index in 0..12 {
        app.push_timeline(TimelineRole::Assistant, format!("message {index}"));
    }
    app.set_terminal_size(80, 12);
    let _ = app.handle_key_event(KeyEvent::new(KeyCode::PageUp, KeyModifiers::NONE))?;
    assert!(app.timeline_scroll_back > 0);
    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Home, KeyModifiers::NONE))?;
    assert!(app.timeline_scroll_back > 0);
    let _ = app.handle_key_event(KeyEvent::new(KeyCode::PageDown, KeyModifiers::NONE))?;
    let _ = app.handle_key_event(KeyEvent::new(KeyCode::End, KeyModifiers::NONE))?;
    assert_eq!(app.timeline_scroll_back, 0);

    app.input = "abc".to_owned();
    app.input_cursor = 2;
    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Left, KeyModifiers::NONE))?;
    assert_eq!(app.input_cursor, 1);
    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE))?;
    assert_eq!(app.input_cursor, 2);
    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE))?;
    assert_eq!(app.input, "ac");
    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE))?;
    assert!(app.input.is_empty());
    assert_eq!(app.input_cursor, 0);

    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Char('\n'), KeyModifiers::SHIFT))?;
    assert_eq!(app.input, "\n");
    Ok(())
}

#[test]
fn slash_and_status_helpers_cover_usage_no_match_and_no_config_guards() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.run_phase = RunPhase::Streaming;
    assert_eq!(app.run_phase_label(), "streaming");

    app.provider_name = "custom".to_owned();
    app.model_name = "unknown".to_owned();
    app.compaction_config.context_window_tokens = None;
    assert_eq!(app.context_usage_line(), "ctx: n/a · 0 tok");
    assert!(app.compaction_policy_line().starts_with("policy: soft"));
    assert!(app.footer_status_line().contains("ctx n/a"));

    app.input = "/resume definitely-missing".to_owned();
    assert!(app.submit_input()?.is_none());
    assert!(
        app.timeline
            .iter()
            .any(|entry| entry.role == TimelineRole::Notice && entry.text == "no matching session")
    );

    let action = app.execute_slash_command(
        crate::slash::ResolvedSlashCommand {
            canonical: "/model",
            arg: String::new(),
        },
        "/model".to_owned(),
    )?;
    assert!(action.is_none());
    assert_eq!(app.last_notice(), Some("usage: /model <flash|pro|id>"));

    let action = app.execute_slash_command(
        crate::slash::ResolvedSlashCommand {
            canonical: "/bogus",
            arg: String::new(),
        },
        "/bogus".to_owned(),
    )?;
    assert!(action.is_none());
    assert!(
        app.timeline.iter().any(
            |entry| entry.role == TimelineRole::Notice && entry.text == "unknown slash command"
        )
    );

    let mut setup_app = AppState::from_setup(
        Path::new("sigil.toml").to_path_buf(),
        Path::new(".").to_path_buf(),
        None,
    );
    let action = setup_app.execute_slash_command(
        crate::slash::ResolvedSlashCommand {
            canonical: "/model",
            arg: "pro".to_owned(),
        },
        "/model pro".to_owned(),
    )?;
    assert!(action.is_none());
    assert!(setup_app.is_setup_mode());

    setup_app.active_pane = PaneFocus::Composer;
    let action = setup_app.handle_key_event(KeyEvent::new(KeyCode::BackTab, KeyModifiers::NONE))?;
    assert!(action.is_none());
    Ok(())
}

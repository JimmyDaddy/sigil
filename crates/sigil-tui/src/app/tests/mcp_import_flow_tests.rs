use super::*;
use anyhow::Context;

fn import_fixture() -> Result<(tempfile::TempDir, AppState)> {
    let dir = tempdir()?;
    let mut config = test_config();
    config.workspace.root = dir.path().display().to_string();
    config
        .connections
        .get_mut("deepseek-default")
        .context("connection")?["credential"] =
        json!({"source": "environment", "name": "SIGIL_API_KEY"});
    assert!(
        std::env::var_os("SIGIL_API_KEY").is_none(),
        "isolated fixture needs no configured provider key"
    );
    let config_path = dir.path().join("sigil.toml");
    config.save(&config_path)?;
    std::fs::write(
        dir.path().join("import.json"),
        serde_json::to_vec(&json!({
            "mcpServers": {
                "alpha": {"command":"must-not-start-during-import", "args":["--token", "argument-secret"],
                    "env":{"TOKEN":"inline-env-secret"}},
                "beta": {"url":"https://example.com/mcp", "headers":{"Authorization":"header-secret"}},
                "invalid": {"type":"unavailable-transport"}
            }
        }))?,
    )?;
    let mut app = AppState::from_root_config(&config_path, &config);
    app.open_config_panel();
    app.config_state
        .as_mut()
        .context("settings")?
        .set_section(ConfigSection::Mcp);
    Ok((dir, app))
}

fn read_preview(app: &mut AppState) -> Result<()> {
    app.handle_key_event(KeyEvent::new(KeyCode::Char('n'), KeyModifiers::CONTROL))?;
    assert_eq!(app.modal_title(), Some("Import MCP Configuration"));
    let outcome = app.handle_modal_paste_text("import.json");
    app.apply_modal_outcome(outcome);
    app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))?;
    join_preview(app)?;
    assert_eq!(app.modal_title(), Some("Import MCP Servers"));
    Ok(())
}

fn join_preview(app: &mut AppState) -> Result<()> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while app.mcp_import_task.is_some() {
        app.poll_mcp_import();
        anyhow::ensure!(
            std::time::Instant::now() < deadline,
            "import reader did not finish"
        );
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    Ok(())
}

#[test]
fn mcp_import_tui_previews_without_secrets_and_saves_selected_draft_without_model_key() -> Result<()>
{
    let (dir, mut app) = import_fixture()?;
    let config_path = dir.path().join("sigil.toml");
    let before = std::fs::read(&config_path)?;
    read_preview(&mut app)?;
    let preview = app.modal_lines().join("\n");
    for secret in ["inline-env-secret", "header-secret", "argument-secret"] {
        assert!(!preview.contains(secret));
    }
    assert!(preview.contains("Command arguments WILL be saved"));
    assert!(preview.contains("alpha"));
    assert_eq!(std::fs::read(&config_path)?, before);
    assert!(
        app.config_state
            .as_ref()
            .context("settings")?
            .draft
            .mcp_servers
            .is_empty()
    );
    app.handle_key_event(KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE))?;
    assert!(
        app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))?
            .is_none()
    );
    assert!(!app.has_modal());
    let draft = &app.config_state.as_ref().context("settings")?.draft;
    assert_eq!(draft.mcp_servers.len(), 1);
    assert_eq!(draft.mcp_servers[0].name, "alpha");
    assert_eq!(
        draft.mcp_servers[0].base_config.startup,
        McpServerStartup::Lazy
    );
    assert!(!draft.mcp_servers[0].base_config.required);
    assert!(app.config_is_dirty());
    assert_eq!(std::fs::read(&config_path)?, before);
    assert!(app.drain_pending_worker_commands().is_empty());

    // The established settings transaction still performs its real config CAS; no model call
    // or credential resolution is required to persist imported server declarations.
    let saved = app.handle_key_event(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL))?;
    assert!(matches!(saved, Some(AppAction::ConfigSaved { .. })));
    let persisted = RootConfig::load(&config_path)?;
    assert_eq!(persisted.mcp_servers.len(), 1);
    assert_eq!(persisted.mcp_servers[0].name, "alpha");
    assert!(!std::fs::read_to_string(&config_path)?.contains("inline-env-secret"));
    assert!(!app.config_is_dirty());
    Ok(())
}

#[test]
fn mcp_import_tui_conflicts_preserve_current_draft_and_config_cas() -> Result<()> {
    let (dir, mut app) = import_fixture()?;
    read_preview(&mut app)?;
    app.handle_key_event(KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE))?;
    app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))?;
    read_preview(&mut app)?;
    app.handle_key_event(KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE))?;
    app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))?;
    assert!(app.mcp_import_modal_open());
    assert!(
        app.modal_lines()
            .join("\n")
            .contains("conflicts with an existing server name")
    );
    assert_eq!(
        app.config_state
            .as_ref()
            .context("settings")?
            .draft
            .mcp_servers
            .len(),
        1
    );
    app.handle_key_event(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE))?;

    let config_path = dir.path().join("sigil.toml");
    let mut concurrent = RootConfig::load(&config_path)?;
    concurrent.agent.tool_timeout_secs += 1;
    concurrent.save(&config_path)?;
    assert!(
        app.handle_key_event(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL))?
            .is_none()
    );
    assert!(app.config_is_dirty());
    assert!(RootConfig::load(&config_path)?.mcp_servers.is_empty());
    assert_eq!(
        RootConfig::load(&config_path)?.agent.tool_timeout_secs,
        concurrent.agent.tool_timeout_secs
    );
    Ok(())
}

#[test]
fn mcp_import_tui_dismissed_or_replaced_draft_does_not_accept_late_preview() -> Result<()> {
    let (_dir, mut app) = import_fixture()?;
    app.start_mcp_import_preview("import.json".to_owned());
    app.handle_key_event(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE))?;
    assert!(app.mcp_import_task.is_some());
    join_preview(&mut app)?;
    assert!(!app.has_modal());
    assert!(!app.config_is_dirty());

    app.start_mcp_import_preview("import.json".to_owned());
    app.config_state = Some(crate::config_panel::ConfigState::from_root_config(
        &test_config(),
    ));
    join_preview(&mut app)?;
    app.handle_key_event(KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE))?;
    app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))?;
    assert!(
        app.config_state
            .as_ref()
            .context("settings")?
            .draft
            .mcp_servers
            .is_empty()
    );
    assert!(!app.config_is_dirty());
    Ok(())
}

#[test]
fn mcp_import_tui_invalid_or_unselected_entries_never_enter_draft() -> Result<()> {
    let (_dir, mut app) = import_fixture()?;
    read_preview(&mut app)?;
    app.handle_key_event(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE))?;
    app.handle_key_event(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE))?;
    app.handle_key_event(KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE))?;
    app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))?;
    assert!(
        app.modal_lines()
            .join("\n")
            .contains("select at least one valid server")
    );
    assert!(
        app.config_state
            .as_ref()
            .context("settings")?
            .draft
            .mcp_servers
            .is_empty()
    );
    app.handle_key_event(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE))?;
    app.handle_key_event(KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE))?;
    app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))?;
    let draft = &app.config_state.as_ref().context("settings")?.draft;
    assert_eq!(draft.mcp_servers.len(), 1);
    assert_eq!(draft.mcp_servers[0].name, "beta");
    Ok(())
}

#[test]
fn mcp_import_tui_preserves_unrelated_incomplete_server_draft() -> Result<()> {
    let (_dir, mut app) = import_fixture()?;
    let mut existing = crate::config_panel::McpServerDraft::from_config(&McpServerConfig {
        name: "existing".to_owned(),
        ..McpServerConfig::default()
    });
    existing.command = "editing command".to_owned();
    existing.args_csv = "editing, arguments".to_owned();
    existing.startup_timeout_secs = "not finished yet".to_owned();
    app.config_state
        .as_mut()
        .context("settings")?
        .draft
        .mcp_servers
        .push(existing);
    read_preview(&mut app)?;
    app.handle_key_event(KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE))?;
    app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))?;
    assert!(!app.has_modal());
    let draft = &app.config_state.as_ref().context("settings")?.draft;
    assert_eq!(draft.mcp_servers.len(), 2);
    assert_eq!(draft.mcp_servers[0].name, "existing");
    assert_eq!(draft.mcp_servers[0].command, "editing command");
    assert_eq!(draft.mcp_servers[0].args_csv, "editing, arguments");
    assert_eq!(
        draft.mcp_servers[0].startup_timeout_secs,
        "not finished yet"
    );
    assert_eq!(draft.mcp_servers[1].name, "alpha");
    Ok(())
}

use super::*;

fn config_for_workspace(workspace_root: &Path) -> RootConfig {
    let mut config = test_config();
    config.workspace.root = workspace_root.display().to_string();
    config
}

fn write_workspace_skill(workspace_root: &Path, id: &str, body: &str) -> Result<()> {
    let path = workspace_root
        .join(".sigil")
        .join("skills")
        .join(id)
        .join("SKILL.md");
    std::fs::create_dir_all(path.parent().expect("skill path should have parent"))?;
    std::fs::write(path, body)?;
    Ok(())
}

fn write_workspace_agent(workspace_root: &Path, id: &str, body: &str) -> Result<()> {
    let path = workspace_root
        .join(".sigil")
        .join("agents")
        .join(id)
        .join("agent.toml");
    std::fs::create_dir_all(path.parent().expect("agent path should have parent"))?;
    std::fs::write(path, body)?;
    Ok(())
}

fn write_workspace_plugin(workspace_root: &Path, id: &str, version: &str) -> Result<()> {
    let plugin_root = workspace_root.join(".sigil").join("plugins").join(id);
    std::fs::create_dir_all(plugin_root.join("agents/reviewer"))?;
    std::fs::write(
        plugin_root.join("agents/reviewer/agent.toml"),
        r#"description = "Plugin review agent."
instructions = "Review repository changes."
trust = "trusted"
"#,
    )?;
    std::fs::create_dir_all(plugin_root.join("skills/review"))?;
    std::fs::write(
        plugin_root.join("skills/review/SKILL.md"),
        r#"---
id: review
description: Review repositories.
trust: trusted
---

# Review
"#,
    )?;
    std::fs::write(
        plugin_root.join("plugin.toml"),
        format!(
            r#"id = "{id}"
name = "Repository Review"
version = "{version}"
description = "Reusable review pack."

[[agents]]
path = "agents/reviewer/agent.toml"

[[skills]]
path = "skills/review/SKILL.md"

[[hooks]]
event = "pre_tool_use"
command = "scripts/check-tool-policy.sh"
args = ["--policy", "strict"]
approval = "ask"

[[mcp_servers]]
name = "repo-tools"
command = "node"
args = ["server.js"]
startup = "lazy"
required = false
"#
        ),
    )?;
    Ok(())
}

fn write_invalid_workspace_plugin(workspace_root: &Path, id: &str) -> Result<()> {
    let plugin_root = workspace_root.join(".sigil").join("plugins").join(id);
    std::fs::create_dir_all(&plugin_root)?;
    std::fs::write(plugin_root.join("plugin.toml"), "id = ")?;
    Ok(())
}

#[test]
fn config_command_opens_first_editable_step() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.input = "/config".to_owned();

    let action = app.submit_input()?;

    assert!(action.is_none());
    assert!(app.is_config_mode());
    assert_eq!(app.config_section_title(), Some("Provider"));
    assert_eq!(app.config_selected_field_label(), Some("Model"));
    assert_eq!(app.config_status_summary(), "Provider · saved · sigil.toml");
    let nav = app.config_nav_lines().join("\n");
    assert!(nav.contains("Tab section"));
    assert!(!nav.contains("Tab/Left/Right section"));
    Ok(())
}

#[test]
fn config_up_down_moves_between_fields_in_current_step() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.open_config_panel();

    assert_eq!(app.config_section_title(), Some("Provider"));
    assert_eq!(app.config_selected_field_label(), Some("Model"));

    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE))?;
    assert_eq!(app.config_selected_field_label(), Some("API key"));

    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE))?;
    assert_eq!(app.config_selected_field_label(), Some("Model"));
    Ok(())
}

#[test]
fn config_enter_on_provider_name_cycles_provider() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.open_config_panel();
    app.config_state
        .as_mut()
        .expect("config state should still exist")
        .selected_field = Some(ConfigField::ProviderName);

    let action = app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))?;

    assert!(action.is_none());
    let state = app
        .config_state
        .as_ref()
        .expect("config state should still exist");
    assert_eq!(state.draft.provider_name, "openai_compat");
    assert!(state.dirty);
    assert_eq!(app.last_notice(), Some("provider -> openai_compat"));
    Ok(())
}

#[test]
fn config_down_to_footer_focuses_actions() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.open_config_panel();
    app.config_state
        .as_mut()
        .expect("config state should still exist")
        .selected_field = Some(ConfigField::ProviderName);

    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE))?;
    assert_eq!(app.config_selected_field_label(), Some("save"));

    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE))?;
    assert_eq!(app.config_selected_field_label(), Some("save_and_close"));

    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE))?;
    assert_eq!(app.config_selected_field_label(), Some("close"));

    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE))?;
    assert_eq!(app.config_selected_field_label(), Some("Provider"));
    Ok(())
}

#[test]
fn config_empty_mcp_footer_can_leave_bottom_focus() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.open_config_panel();
    app.config_state
        .as_mut()
        .expect("config state should still exist")
        .set_section(ConfigSection::Mcp);
    assert_eq!(app.config_selected_field_label(), None);

    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE))?;
    assert_eq!(app.config_selected_field_label(), Some("save"));

    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE))?;
    assert_eq!(app.config_section_title(), Some("MCP"));
    assert_eq!(app.config_selected_field_label(), None);
    let state = app
        .config_state
        .as_ref()
        .expect("config state should still exist");
    assert!(!state.footer_selected);
    assert_eq!(state.selected_field, None);

    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Left, KeyModifiers::NONE))?;
    assert_eq!(app.config_section_title(), Some("Plugins"));
    assert_eq!(app.config_selected_field_label(), None);

    app.config_state
        .as_mut()
        .expect("config state should still exist")
        .set_section(ConfigSection::Mcp);
    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE))?;
    assert_eq!(app.config_selected_field_label(), Some("save"));
    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Left, KeyModifiers::NONE))?;
    assert_eq!(app.config_section_title(), Some("Plugins"));
    assert_eq!(app.config_selected_field_label(), None);

    app.config_state
        .as_mut()
        .expect("config state should still exist")
        .set_section(ConfigSection::Mcp);
    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE))?;
    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE))?;
    assert_eq!(app.config_selected_field_label(), Some("save_and_close"));
    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE))?;
    assert_eq!(app.config_selected_field_label(), Some("activate_mcp"));
    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE))?;
    assert_eq!(app.config_selected_field_label(), Some("close"));
    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE))?;
    assert_eq!(app.config_section_title(), Some("Provider"));
    assert_eq!(app.config_selected_field_label(), Some("Model"));
    Ok(())
}

#[test]
fn config_mcp_footer_activate_returns_lazy_activation_action() -> Result<()> {
    let mut config = test_config();
    config.mcp_servers.push(sigil_kernel::McpServerConfig {
        name: "filesystem".to_owned(),
        command: "mcp-filesystem".to_owned(),
        required: false,
        startup: McpServerStartup::Lazy,
        ..Default::default()
    });
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &config);
    app.open_config_panel();
    app.config_state
        .as_mut()
        .expect("config state should still exist")
        .set_section(ConfigSection::Mcp);
    app.config_state
        .as_mut()
        .expect("config state should still exist")
        .selected_field = Some(ConfigField::McpStartupTimeoutSecs);

    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE))?;
    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE))?;
    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE))?;
    assert_eq!(app.config_selected_field_label(), Some("activate_mcp"));
    let detail = app.config_detail_lines().join("\n");
    assert!(detail.contains("Runtime"));
    assert!(detail.contains("deferred"));

    let action = app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))?;

    assert!(matches!(
        action,
        Some(AppAction::ActivateLazyMcp {
            server_name: Some(ref server_name)
        }) if server_name == "filesystem"
    ));
    assert_eq!(app.last_notice(), Some("activating MCP filesystem"));
    assert_eq!(
        app.mcp_server_runtime_status_label("filesystem").as_deref(),
        Some("activating")
    );
    let detail = app.config_detail_lines().join("\n");
    assert!(detail.contains("activating"));
    Ok(())
}

#[test]
fn config_mcp_lifecycle_updates_from_worker_activation_status() -> Result<()> {
    let mut config = test_config();
    config.mcp_servers.push(sigil_kernel::McpServerConfig {
        name: "filesystem".to_owned(),
        command: "mcp-filesystem".to_owned(),
        required: false,
        startup: McpServerStartup::Lazy,
        ..Default::default()
    });
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &config);
    app.open_config_panel();
    app.config_state
        .as_mut()
        .expect("config state should still exist")
        .set_section(ConfigSection::Mcp);

    app.handle_worker_message(WorkerMessage::McpActivationStatus {
        server_name: Some("filesystem".to_owned()),
        status: McpActivationStatus::Ready { added_tools: 3 },
    })?;

    assert_eq!(
        app.mcp_server_runtime_status_label("filesystem").as_deref(),
        Some("ready 3 tools")
    );
    let detail = app.config_detail_lines().join("\n");
    assert!(detail.contains("ready 3 tools"));

    app.handle_worker_message(WorkerMessage::McpActivationStatus {
        server_name: Some("filesystem".to_owned()),
        status: McpActivationStatus::Failed {
            error: "MCP server filesystem tools/list failed: bad response".to_owned(),
        },
    })?;

    let label = app
        .mcp_server_runtime_status_label("filesystem")
        .expect("status should exist");
    assert!(label.contains("failed:"));
    assert!(label.contains("bad response"));
    let detail = app.config_detail_lines().join("\n");
    assert!(detail.contains("failed:"));
    Ok(())
}

#[test]
fn config_mcp_footer_activate_requires_saved_lazy_server() -> Result<()> {
    let mut config = test_config();
    config.mcp_servers.push(sigil_kernel::McpServerConfig {
        name: "eager".to_owned(),
        command: "mcp-eager".to_owned(),
        startup: McpServerStartup::Eager,
        ..Default::default()
    });
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &config);
    app.open_config_panel();
    let state = app
        .config_state
        .as_mut()
        .expect("config state should still exist");
    state.set_section(ConfigSection::Mcp);
    state.focus_footer(ConfigFooterAction::ActivateMcp);

    let action = app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))?;

    assert!(action.is_none());
    assert_eq!(app.last_notice(), Some("MCP server eager is eager"));

    let state = app
        .config_state
        .as_mut()
        .expect("config state should still exist");
    state.dirty = true;
    state.focus_footer(ConfigFooterAction::ActivateMcp);

    let action = app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))?;

    assert!(action.is_none());
    assert_eq!(app.last_notice(), Some("save config before activating MCP"));
    Ok(())
}

#[test]
fn config_left_right_switches_steps() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.open_config_panel();

    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE))?;
    assert_eq!(app.config_section_title(), Some("Permissions"));
    assert_eq!(app.config_selected_field_label(), Some("Default mode"));

    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Left, KeyModifiers::NONE))?;
    assert_eq!(app.config_section_title(), Some("Provider"));
    assert_eq!(app.config_selected_field_label(), Some("Model"));
    Ok(())
}

#[test]
fn config_enter_starts_and_commits_text_edit() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.open_config_panel();

    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))?;
    assert!(app.has_modal());
    assert_eq!(app.modal_title(), Some("Model"));

    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE))?;
    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))?;

    let state = app
        .config_state
        .as_ref()
        .expect("config state should still exist");
    assert_eq!(state.draft.provider_model, "deepseek-v4-pro");
    assert!(state.dirty);
    Ok(())
}

#[test]
fn config_direct_typing_replaces_selected_text_value() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.open_config_panel();

    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Char('g'), KeyModifiers::NONE))?;
    assert!(app.has_modal());
    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Char('p'), KeyModifiers::NONE))?;
    let detail = app.modal_lines().join("\n");
    assert!(detail.contains("key: model"));
    assert!(detail.contains("value: gp|"));
    assert!(!detail.contains("deepseek-v4-flashg"));

    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))?;
    let state = app
        .config_state
        .as_ref()
        .expect("config state should still exist");
    assert_eq!(state.draft.provider_model, "gp");
    Ok(())
}

#[test]
fn config_provider_flow_hides_advanced_provider_fields() {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.open_config_panel();

    let lines = app.config_detail_lines();
    let detail = lines.join("\n");

    assert_eq!(lines[0], "Provider 1/11 · provider settings");
    assert_eq!(
        lines[1],
        "[provider] permissions memory compaction code intel terminal appearance agents skills plugins mcp"
    );
    assert_eq!(lines[2], "");
    assert!(detail.contains("[model]"));
    assert!(detail.contains("[authentication]"));
    assert!(detail.contains("[endpoint]"));
    assert!(detail.contains("[advanced]"));
    assert!(detail.contains("[details]"));
    assert!(detail.contains("selected: Model"));
    assert!(detail.contains("key: model"));
    assert!(detail.contains("controls: Tab section · Up/Down field · Enter edit"));
    assert!(detail.contains("actions: Down to actions · Ctrl-S save · Esc close"));
    assert!(!lines.iter().take(3).any(|line| line.contains("Tab")));
    assert!(!detail.contains("beta_base_url"));
    assert!(!detail.contains("user_id_strategy"));
    assert!(!detail.contains("anthropic_base_url"));
    assert!(!detail.contains("strict_tools_mode"));
    assert!(!detail.contains("request_timeout_secs"));
}

#[test]
fn config_permissions_step_uses_policy_summary_and_details() {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.open_config_panel();
    app.config_state
        .as_mut()
        .expect("config state should still exist")
        .set_section(ConfigSection::Permissions);

    let detail = app.config_detail_lines().join("\n");

    assert!(detail.contains("[policy]"));
    assert!(detail.contains("Default mode"));
    assert!(detail.contains("[rules]"));
    assert!(detail.contains("- Rule overrides"));
    assert!(detail.contains("i All unmatched tools use the default mode above"));
    assert!(detail.contains("[details]"));
    assert!(detail.contains("selected: Default mode"));
    assert!(detail.contains("key: default_mode"));
    assert!(detail.contains("controls: Tab section"));
    assert!(!detail.lines().any(|line| line.starts_with("overrides:")));
    assert!(!detail.contains("subject="));
}

#[test]
fn config_memory_step_uses_loaded_context_summary() {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.open_config_panel();
    app.config_state
        .as_mut()
        .expect("config state should still exist")
        .set_section(ConfigSection::Memory);

    let detail = app.config_detail_lines().join("\n");

    assert!(detail.contains("[workspace memory]"));
    assert!(detail.contains("Memory"));
    assert!(detail.contains("[loaded context]"));
    assert!(detail.contains("- Documents"));
    assert!(detail.contains("- Last scan"));
    assert!(detail.contains("- Root files"));
    assert!(detail.contains("[details]"));
    assert!(detail.contains("selected: Memory"));
    assert!(!detail.contains("docs:"));
    assert!(!detail.contains("root docs:"));
}

#[test]
fn config_mcp_step_uses_server_summary_when_empty() {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.open_config_panel();
    app.config_state
        .as_mut()
        .expect("config state should still exist")
        .set_section(ConfigSection::Mcp);

    let detail = app.config_detail_lines().join("\n");

    assert!(detail.contains("[servers]"));
    assert!(detail.contains("- Configured"));
    assert!(detail.contains("0 servers"));
    assert!(detail.contains("i No MCP servers configured"));
    assert!(detail.contains("i Ctrl-N adds a required eager self-hosted server"));
    assert!(detail.contains("mcp: Ctrl-N add · Ctrl-D drop · PgUp/PgDn server"));
    assert!(!detail.contains("servers:"));
    assert!(!detail.contains("args_csv:"));

    let nav = app.config_nav_lines().join("\n");
    assert!(nav.contains("MCP: Ctrl-N add"));
    assert!(nav.contains("MCP: Ctrl-D drop"));
    assert!(nav.contains("MCP: PgUp/PgDn switch"));
    assert!(nav.contains("MCP: footer activate lazy"));
}

#[test]
fn config_compaction_step_shows_effective_context_window_source() {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.open_config_panel();
    app.config_state
        .as_mut()
        .expect("config state should still exist")
        .set_section(ConfigSection::Compaction);

    let detail = app.config_detail_lines().join("\n");

    assert!(detail.contains("[context]"));
    assert!(detail.contains("- Effective window"));
    assert!(detail.contains("1,000,000 tokens  source=provider"));
    assert!(detail.contains("Fallback window"));
    assert!(detail.contains("[details]"));
    assert!(detail.contains("selected: Auto compact"));
    assert!(!detail.contains("context_window_tokens"));
}

#[test]
fn config_compaction_step_uses_fallback_for_unknown_provider_window() {
    let mut config = test_config();
    config.agent.provider = "custom".to_owned();
    config.agent.model = "custom-model".to_owned();
    config.compaction.context_window_tokens = Some(128_000);
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &config);
    app.open_config_panel();
    app.config_state
        .as_mut()
        .expect("config state should still exist")
        .set_section(ConfigSection::Compaction);

    let detail = app.config_detail_lines().join("\n");

    assert!(detail.contains("128,000 tokens  source=fallback"));
}

#[test]
fn config_code_intelligence_step_shows_trust_and_readiness() {
    let mut config = test_config();
    config.code_intelligence.enabled = true;
    config.code_intelligence.discovery.enabled = false;
    config.code_intelligence.servers = vec![sigil_kernel::LanguageServerConfig {
        name: "missing-lsp".to_owned(),
        languages: vec!["rust".to_owned()],
        command: "./missing-lsp".to_owned(),
        args: Vec::new(),
        env: Default::default(),
        root_markers: vec!["Cargo.toml".to_owned()],
        file_extensions: vec!["rs".to_owned()],
        initialization_options: Default::default(),
        trust_required: true,
        startup_timeout_ms: 5_000,
    }];
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &config);
    app.open_config_panel();
    app.config_state
        .as_mut()
        .expect("config state should still exist")
        .set_section(ConfigSection::CodeIntelligence);

    let detail = app.config_detail_lines().join("\n");

    assert!(detail.contains("Code Intel 5/11 · LSP readiness"));
    assert!(detail.contains("[controls]"));
    assert!(detail.contains("Code intelligence: yes"));
    assert!(detail.contains("Startup: lazy"));
    assert!(detail.contains("Discovery: no"));
    assert!(detail.contains("Missing reports: yes"));
    assert!(detail.contains("[trust]"));
    assert!(detail.contains("- Tool access: read-only"));
    assert!(detail.contains("- Server process: local workspace LSP"));
    assert!(detail.contains("- Write actions: unavailable"));
    assert!(detail.contains("[readiness]"));
    assert!(detail.contains("- Saved runtime: lazy"));
    assert!(detail.contains("- Draft status: lazy"));
    assert!(detail.contains("- Readiness: warn"));
    assert!(detail.contains("- lsp:missing-lsp: warn"));
    assert!(detail.contains("command=missing"));
    assert!(detail.contains("i install or configure the missing-lsp language server"));
    assert!(detail.contains("selected: Code intelligence"));
}

#[test]
fn config_terminal_step_shows_controls_and_compatibility() {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.open_config_panel();
    app.config_state
        .as_mut()
        .expect("config state should still exist")
        .set_section(ConfigSection::Terminal);

    let detail = app.config_detail_lines().join("\n");

    assert!(detail.contains("Terminal 6/11 · terminal integration"));
    assert!(detail.contains("[interaction]"));
    assert!(detail.contains("Mouse capture: yes"));
    assert!(detail.contains("OSC52 clipboard: yes"));
    assert!(detail.contains("Scroll sensitivity: 3 rows"));
    assert!(detail.contains("[compatibility]"));
    assert!(detail.contains("Turn mouse_capture off"));
    assert!(detail.contains("Turn osc52_clipboard off"));
    assert!(detail.contains("Requests terminal mouse events"));
}

#[test]
fn config_appearance_step_shows_theme_and_scope() {
    let mut config = test_config();
    config.appearance.theme = sigil_kernel::ThemeId::GruvboxDark;
    let mut colors = std::collections::BTreeMap::new();
    colors.insert("surface_base".to_owned(), "#282828".to_owned());
    config.appearance.colors = sigil_kernel::ThemeColorOverrides::new(colors);
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &config);
    app.open_config_panel();
    app.config_state
        .as_mut()
        .expect("config state should still exist")
        .set_section(ConfigSection::Appearance);

    let detail = app.config_detail_lines().join("\n");
    let nav = app.config_nav_lines().join("\n");

    assert!(detail.contains("Appearance 7/11 · TUI theme"));
    assert!(nav.contains("Appearance: Enter cycle"));
    assert!(nav.contains("Appearance: Backspace reset"));
    assert!(nav.contains("Appearance: Ctrl-R clear all"));
    assert!(detail.contains("[theme]"));
    assert!(detail.contains("Theme: gruvbox_dark  [Enter cycle]"));
    assert!(detail.contains("- Name: Gruvbox Dark"));
    assert!(detail.contains("Syntax theme: auto"));
    assert!(detail.contains("- Syntax source: auto -> Gruvbox Dark"));
    assert!(detail.contains("sigil_dark, solarized_dark, solarized_light"));
    assert!(detail.contains("- Overrides: 1 colors"));
    assert!(detail.contains("Color group: surfaces"));
    assert!(detail.contains("- Group overrides: 1 of 12"));
    assert!(detail.contains("Color token: surface_base"));
    assert!(detail.contains("Override: #282828"));
    assert!(detail.contains("Backspace/Delete clears the selected token override"));
    assert!(detail.contains("Ctrl-R clears all overrides"));
    assert!(detail.contains("[diagnostics]"));
    assert!(detail.contains("- Status: ok"));
    assert!(detail.contains("[preview]"));
    assert!(detail.contains("preview compare: current gruvbox_dark -> draft gruvbox_dark (saved)"));
    assert!(detail.contains("preview syntax: auto -> Gruvbox Dark"));
    assert!(detail.contains("preview page: rail timeline composer tool modal"));
    assert!(detail.contains("preview shell: rail live composer footer"));
    assert!(detail.contains("preview composer: Build"));
    assert!(detail.contains("preview tool: read_file"));
    assert!(detail.contains("preview modal: Review Tool Call"));
    assert!(detail.contains("preview token: surface_base #282828"));
    assert!(detail.contains("preview text: primary secondary muted"));
    assert!(detail.contains("preview status: success warning error pending"));
    assert!(detail.contains("preview diff: +added -removed @@ hunk"));
    assert!(detail.contains("preview markdown: heading link code"));
    assert!(detail.contains("[scope]"));
    assert!(detail.contains("not written to session history"));
    assert!(detail.contains("Theme draft previews immediately"));
}

#[test]
fn config_appearance_step_shows_live_diagnostics_for_bad_draft() {
    let mut config = test_config();
    let mut colors = std::collections::BTreeMap::new();
    colors.insert("surface_base".to_owned(), "#101010".to_owned());
    colors.insert("text_primary".to_owned(), "#111111".to_owned());
    config.appearance.colors = sigil_kernel::ThemeColorOverrides::new(colors);
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &config);
    app.open_config_panel();
    app.config_state
        .as_mut()
        .expect("config state should still exist")
        .set_section(ConfigSection::Appearance);

    let detail = app.config_detail_lines().join("\n");

    assert!(detail.contains("[diagnostics]"));
    assert!(detail.contains("- Status:"));
    assert!(detail.contains("warnings"));
    assert!(detail.contains("contrast:text-base"));
    assert!(detail.contains("text_primary on surface_base contrast"));
    assert!(detail.contains("run /config Appearance to preview"));
}

#[test]
fn config_appearance_color_override_edit_save_and_reset() -> Result<()> {
    let temp = tempdir()?;
    let config_path = temp.path().join("sigil.toml");
    let config = test_config();
    let mut app = AppState::from_root_config(&config_path, &config);
    app.open_config_panel();
    {
        let state = app
            .config_state
            .as_mut()
            .expect("config state should still exist");
        state.set_section(ConfigSection::Appearance);
        assert!(state.focus_field(ConfigField::AppearanceColorOverride));
    }

    app.handle_config_paste_text("#010203");

    assert_eq!(app.last_notice(), Some("updated color_override"));
    let state = app
        .config_state
        .as_ref()
        .expect("config state should still exist");
    assert_eq!(
        state.draft.selected_appearance_color_override(),
        Some("#010203")
    );
    assert!(state.dirty);

    let action = app.handle_key_event(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL))?;
    let Some(AppAction::ConfigSaved { root_config }) = action else {
        panic!("color override save should return config saved action");
    };
    assert_eq!(
        root_config.appearance.colors.get("surface_base"),
        Some("#010203")
    );
    let rendered = std::fs::read_to_string(&config_path)?;
    assert!(rendered.contains("[appearance.colors]"));
    assert!(rendered.contains("surface_base = \"#010203\""));

    {
        let state = app
            .config_state
            .as_mut()
            .expect("config state should still exist");
        assert!(state.focus_field(ConfigField::AppearanceColorOverride));
    }
    let action = app.handle_key_event(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE))?;

    assert!(action.is_none());
    assert_eq!(app.last_notice(), Some("reset color surface_base"));
    assert!(
        app.config_state
            .as_ref()
            .expect("config state should still exist")
            .draft
            .selected_appearance_color_override()
            .is_none()
    );

    Ok(())
}

#[test]
fn config_appearance_rejects_invalid_color_override() {
    let config = test_config();
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &config);
    app.open_config_panel();
    {
        let state = app
            .config_state
            .as_mut()
            .expect("config state should still exist");
        state.set_section(ConfigSection::Appearance);
        assert!(state.focus_field(ConfigField::AppearanceColorOverride));
    }

    app.handle_config_paste_text("#123");

    assert_eq!(
        app.last_notice(),
        Some("invalid color override: color override must be #RRGGBB")
    );
    assert!(
        app.config_state
            .as_ref()
            .expect("config state should still exist")
            .draft
            .selected_appearance_color_override()
            .is_none()
    );
}

#[test]
fn config_appearance_ctrl_r_resets_all_color_overrides() -> Result<()> {
    let mut config = test_config();
    let mut colors = std::collections::BTreeMap::new();
    colors.insert("surface_base".to_owned(), "#010203".to_owned());
    colors.insert("text_primary".to_owned(), "#F0F1F2".to_owned());
    config.appearance.colors = sigil_kernel::ThemeColorOverrides::new(colors);
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &config);
    app.open_config_panel();
    app.config_state
        .as_mut()
        .expect("config state should still exist")
        .set_section(ConfigSection::Appearance);

    let action = app.handle_key_event(KeyEvent::new(KeyCode::Char('r'), KeyModifiers::CONTROL))?;

    assert!(action.is_none());
    assert_eq!(app.last_notice(), Some("reset all color overrides"));
    let state = app
        .config_state
        .as_ref()
        .expect("config state should still exist");
    assert!(state.draft.base_root_config.appearance.colors.is_empty());
    assert!(state.dirty);
    Ok(())
}

#[test]
fn config_appearance_color_shortcuts_cover_noop_and_token_edges() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.open_config_panel();

    let action = app.handle_key_event(KeyEvent::new(KeyCode::Char('r'), KeyModifiers::CONTROL))?;

    assert!(action.is_none());
    assert_eq!(app.last_notice(), Some("Ctrl-R: Appearance only"));
    assert!(
        !app.config_state
            .as_ref()
            .expect("config state should still exist")
            .dirty
    );

    {
        let state = app
            .config_state
            .as_mut()
            .expect("config state should still exist");
        state.set_section(ConfigSection::Appearance);
        assert!(state.focus_field(ConfigField::AppearanceColorToken));
    }

    let action = app.handle_key_event(KeyEvent::new(KeyCode::Char('r'), KeyModifiers::CONTROL))?;

    assert!(action.is_none());
    assert_eq!(app.last_notice(), Some("color overrides already empty"));
    assert!(
        !app.config_state
            .as_ref()
            .expect("config state should still exist")
            .dirty
    );

    let action = app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))?;

    assert!(action.is_none());
    assert_eq!(app.last_notice(), Some("color token -> surface_rail"));
    assert_eq!(
        app.config_state
            .as_ref()
            .expect("config state should still exist")
            .draft
            .selected_appearance_color_token(),
        "surface_rail"
    );

    let action = app.handle_key_event(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE))?;

    assert!(action.is_none());
    assert_eq!(
        app.last_notice(),
        Some("color surface_rail already inherits")
    );

    {
        let state = app
            .config_state
            .as_mut()
            .expect("config state should still exist");
        state
            .draft
            .set_selected_appearance_color_override("#010203".to_owned())?;
    }
    let action = app.handle_key_event(KeyEvent::new(KeyCode::Delete, KeyModifiers::NONE))?;

    assert!(action.is_none());
    assert_eq!(app.last_notice(), Some("reset color surface_rail"));
    let state = app
        .config_state
        .as_ref()
        .expect("config state should still exist");
    assert!(state.draft.selected_appearance_color_override().is_none());
    assert!(state.dirty);

    let action = app.handle_key_event(KeyEvent::new(KeyCode::Delete, KeyModifiers::NONE))?;

    assert!(action.is_none());
    assert_eq!(
        app.last_notice(),
        Some("color surface_rail already inherits")
    );

    {
        let state = app
            .config_state
            .as_mut()
            .expect("config state should still exist");
        assert!(state.focus_field(ConfigField::AppearanceColorGroup));
        state
            .draft
            .set_selected_appearance_color_override("#010203".to_owned())?;
    }
    let action = app.handle_key_event(KeyEvent::new(KeyCode::Delete, KeyModifiers::NONE))?;

    assert!(action.is_none());
    assert_eq!(
        app.last_notice(),
        Some("reset 1 color overrides in surfaces")
    );

    let action = app.handle_key_event(KeyEvent::new(KeyCode::Delete, KeyModifiers::NONE))?;

    assert!(action.is_none());
    assert_eq!(
        app.last_notice(),
        Some("color group surfaces already inherits")
    );
    Ok(())
}

#[test]
fn config_appearance_group_enter_and_reset_guards_are_noops() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());

    app.reset_selected_appearance_color_selection();
    assert!(app.config_state.is_none());
    assert_eq!(app.last_notice(), None);

    app.open_config_panel();
    {
        let state = app
            .config_state
            .as_mut()
            .expect("config state should still exist");
        state.set_section(ConfigSection::Appearance);
        assert!(state.focus_field(ConfigField::AppearanceColorGroup));
    }

    let action = app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))?;

    assert!(action.is_none());
    assert_eq!(app.last_notice(), Some("color group -> borders"));
    assert_eq!(
        app.config_state
            .as_ref()
            .expect("config state should still exist")
            .draft
            .selected_appearance_color_group()
            .key,
        "borders"
    );

    {
        let state = app
            .config_state
            .as_mut()
            .expect("config state should still exist");
        state.footer_selected = true;
    }
    let previous_notice = app.last_notice().map(ToOwned::to_owned);
    let action = app.handle_key_event(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE))?;

    assert!(action.is_none());
    assert_eq!(app.last_notice(), previous_notice.as_deref());
    Ok(())
}

#[test]
fn config_appearance_color_field_context_lines_explain_token_and_override() {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.open_config_panel();
    {
        let state = app
            .config_state
            .as_mut()
            .expect("config state should still exist");
        state.set_section(ConfigSection::Appearance);
        assert!(state.focus_field(ConfigField::AppearanceColorToken));
    }

    let token_detail = app.config_detail_lines().join("\n");

    assert!(token_detail.contains("appearance: Enter cycles token in group"));

    {
        let state = app
            .config_state
            .as_mut()
            .expect("config state should still exist");
        assert!(state.focus_field(ConfigField::AppearanceColorGroup));
    }

    let group_detail = app.config_detail_lines().join("\n");

    assert!(group_detail.contains("appearance: Enter cycles group"));

    {
        let state = app
            .config_state
            .as_mut()
            .expect("config state should still exist");
        assert!(state.focus_field(ConfigField::AppearanceColorOverride));
    }

    let override_detail = app.config_detail_lines().join("\n");

    assert!(override_detail.contains("appearance: empty value inherits"));

    {
        let state = app
            .config_state
            .as_mut()
            .expect("config state should still exist");
        assert!(state.focus_field(ConfigField::AppearanceSyntaxTheme));
    }

    let syntax_detail = app.config_detail_lines().join("\n");

    assert!(syntax_detail.contains("auto follows the selected TUI theme"));
}

#[test]
fn config_appearance_theme_enter_cycles_and_save_updates_snapshot() -> Result<()> {
    let temp = tempdir()?;
    let config_path = temp.path().join("sigil.toml");
    let config = test_config();
    let mut app = AppState::from_root_config(&config_path, &config);
    app.push_timeline(TimelineRole::User, "hello theme");
    let initial_theme_bg = app
        .timeline_render_cache
        .iter()
        .flat_map(|line| line.spans.iter())
        .find_map(|span| span.style.bg)
        .expect("user bubble should have a themed background");
    app.open_config_panel();
    app.config_state
        .as_mut()
        .expect("config state should still exist")
        .set_section(ConfigSection::Appearance);
    let initial_control_entries = app.current_session_entries.len();

    let action = app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))?;

    assert!(action.is_none());
    assert_eq!(app.last_notice(), Some("theme -> solarized_dark"));
    assert_eq!(
        app.config_state
            .as_ref()
            .expect("config state should still exist")
            .draft
            .appearance_theme,
        sigil_kernel::ThemeId::SolarizedDark
    );
    assert_eq!(
        app.root_config_snapshot()
            .expect("runtime config should exist")
            .appearance
            .theme,
        sigil_kernel::ThemeId::SigilDark
    );

    let action = app.handle_key_event(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL))?;
    let Some(AppAction::ConfigSaved { root_config }) = action else {
        panic!("theme save should return config saved action");
    };

    assert_eq!(
        root_config.appearance.theme,
        sigil_kernel::ThemeId::SolarizedDark
    );
    assert_eq!(
        app.root_config_snapshot()
            .expect("runtime config should exist")
            .appearance
            .theme,
        sigil_kernel::ThemeId::SolarizedDark
    );
    let updated_theme_bg = app
        .timeline_render_cache
        .iter()
        .flat_map(|line| line.spans.iter())
        .find_map(|span| span.style.bg)
        .expect("timeline cache should rebuild with themed background");
    assert_ne!(initial_theme_bg, updated_theme_bg);
    assert_eq!(app.current_session_entries.len(), initial_control_entries);
    let rendered = std::fs::read_to_string(&config_path)?;
    assert!(rendered.contains("theme = \"solarized_dark\""));
    Ok(())
}

#[test]
fn config_appearance_syntax_theme_enter_cycles_and_saves() -> Result<()> {
    let temp = tempdir()?;
    let config_path = temp.path().join("sigil.toml");
    let config = test_config();
    let mut app = AppState::from_root_config(&config_path, &config);
    app.open_config_panel();
    {
        let state = app
            .config_state
            .as_mut()
            .expect("config state should still exist");
        state.set_section(ConfigSection::Appearance);
        assert!(state.focus_field(ConfigField::AppearanceSyntaxTheme));
    }

    let action = app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))?;

    assert!(action.is_none());
    assert_eq!(app.last_notice(), Some("syntax theme -> catppuccin_mocha"));
    assert!(
        app.config_detail_lines()
            .join("\n")
            .contains("- Syntax source: manual -> Catppuccin Mocha")
    );

    let action = app.handle_key_event(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL))?;
    let Some(AppAction::ConfigSaved { root_config }) = action else {
        panic!("syntax theme save should return config saved action");
    };

    assert_eq!(
        root_config.appearance.syntax_theme,
        sigil_kernel::SyntaxThemeId::CatppuccinMocha
    );
    let rendered = std::fs::read_to_string(&config_path)?;
    assert!(rendered.contains("syntax_theme = \"catppuccin_mocha\""));
    Ok(())
}

#[test]
fn config_agents_step_discovers_and_renders_agent_profile_metadata() -> Result<()> {
    let temp = tempdir()?;
    let workspace = temp.path().join("workspace");
    std::fs::create_dir_all(&workspace)?;
    write_workspace_agent(
        &workspace,
        "review",
        r#"
description = "Review this repository."
instructions = "Review with grep and read_file."
trust = "trusted"
invocation_policy = "model_allowed"
allowed_tools = ["read_file", "grep"]
nickname_candidates = ["Repo Review"]
aliases = ["rr"]
slash_names = ["review-agent"]
"#,
    )?;
    let config = config_for_workspace(&workspace);
    let mut app = AppState::from_root_config(&temp.path().join("sigil.toml"), &config);
    app.open_config_panel();
    {
        let state = app
            .config_state
            .as_mut()
            .expect("config state should still exist");
        state.set_section(ConfigSection::Agents);
        state.selected_agent_index = state
            .agent_profiles
            .iter()
            .position(|agent| agent.profile.id.as_str() == "review")
            .expect("review agent should be discovered");
    }

    let detail = app.config_detail_lines().join("\n");

    assert!(detail.contains("Agents 8/11 · agent profiles"));
    assert!(detail.contains("[discovery]"));
    assert!(detail.contains("- Configured: 5 agents"));
    assert!(detail.contains("- Compatibility: 0 agents"));
    assert!(detail.contains("- Selected: agent 4/5 · review"));
    assert!(detail.contains("[agents]"));
    assert!(
        detail
            .contains("> review: trusted · subagent · workspace · enabled=yes user=yes model=yes")
    );
    assert!(detail.contains("[agent]"));
    assert!(detail.contains("Agent: review"));
    assert!(detail.contains("- Description: Review this repository."));
    assert!(detail.contains("- Kind: subagent"));
    assert!(detail.contains("- Enabled: yes"));
    assert!(detail.contains("- User: yes"));
    assert!(detail.contains("- Model: yes"));
    assert!(detail.contains("- Trust: trusted"));
    assert!(detail.contains("- Source: workspace"));
    assert!(detail.contains("- Source hash:"));
    assert!(detail.contains("- Invocation: model_allowed"));
    assert!(detail.contains("- Tools: names=grep,read_file"));
    assert!(detail.contains("- Nicknames: Repo Review"));
    assert!(detail.contains("- Aliases: rr"));
    assert!(detail.contains("- Slash: /review-agent"));
    assert!(detail.contains("agents: Up/Down agent · PgUp/PgDn wrap · footer trust/policy"));

    let nav = app.config_nav_lines().join("\n");
    assert!(nav.contains("Agents: Up/Down select"));
    assert!(nav.contains("Agents: PgUp/PgDn wrap"));
    assert!(nav.contains("Agents: footer trust/policy"));
    assert_eq!(
        app.config_footer_action_labels(),
        vec!["trust", "block", "enable", "user", "model", "close"]
    );

    let _ = app.handle_key_event(KeyEvent::new(KeyCode::PageUp, KeyModifiers::NONE))?;
    assert!(
        app.last_notice()
            .is_some_and(|notice| notice.starts_with("agent "))
    );
    let _ = app.handle_key_event(KeyEvent::new(KeyCode::PageDown, KeyModifiers::NONE))?;
    assert!(
        app.last_notice()
            .is_some_and(|notice| notice.starts_with("agent "))
    );
    Ok(())
}

#[test]
fn config_agents_step_renders_empty_and_warning_states() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.open_config_panel();
    {
        let state = app
            .config_state
            .as_mut()
            .expect("config state should still exist");
        state.set_section(ConfigSection::Agents);
        state.agent_profiles.clear();
        state.agent_warnings = vec![
            "warning one".to_owned(),
            "warning two".to_owned(),
            "warning three".to_owned(),
            "warning four".to_owned(),
            "warning five".to_owned(),
        ];
    }

    let detail = app.config_detail_lines().join("\n");

    assert!(detail.contains("No agents discovered"));
    assert!(detail.contains("Agents are discovered from built-ins"));
    assert!(detail.contains("[warnings]"));
    assert!(detail.contains("i warning one"));
    assert!(detail.contains("... 1 more warnings"));
    let nav = app.config_nav_lines().join("\n");
    assert!(nav.contains("Agents: Up/Down select"));

    let _ = app.handle_key_event(KeyEvent::new(KeyCode::PageUp, KeyModifiers::NONE))?;
    assert_eq!(app.last_notice(), Some("no agent to select"));
    let _ = app.handle_key_event(KeyEvent::new(KeyCode::PageDown, KeyModifiers::NONE))?;
    assert_eq!(app.last_notice(), Some("no agent to select"));

    app.config_state
        .as_mut()
        .expect("config state should still exist")
        .focus_footer(ConfigFooterAction::TrustAgent);
    let action = app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))?;
    assert!(action.is_none());
    assert_eq!(app.last_notice(), Some("no agent selected"));

    app.config_state
        .as_mut()
        .expect("config state should still exist")
        .focus_footer(ConfigFooterAction::ToggleAgentEnabled);
    let action = app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))?;
    assert!(action.is_none());
    assert_eq!(app.last_notice(), Some("no agent selected"));
    Ok(())
}

#[test]
fn config_agents_footer_refuses_system_managed_and_busy_updates() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.open_config_panel();
    {
        let state = app
            .config_state
            .as_mut()
            .expect("config state should still exist");
        state.set_section(ConfigSection::Agents);
        state.selected_agent_index = state
            .agent_profiles
            .iter()
            .position(|agent| agent.source == sigil_kernel::AgentProfileSource::System)
            .expect("built-in system agent should exist");
        state.focus_footer(ConfigFooterAction::TrustAgent);
    }

    let action = app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))?;
    assert!(action.is_none());
    assert!(
        app.last_notice()
            .is_some_and(|notice| notice.contains("system-managed"))
    );

    app.config_state
        .as_mut()
        .expect("config state should still exist")
        .focus_footer(ConfigFooterAction::ToggleAgentModel);
    let action = app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))?;
    assert!(action.is_none());
    assert!(
        app.last_notice()
            .is_some_and(|notice| notice.contains("system-managed"))
    );

    app.is_busy = true;
    app.config_state
        .as_mut()
        .expect("config state should still exist")
        .focus_footer(ConfigFooterAction::TrustAgent);
    let action = app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))?;
    assert!(action.is_none());
    assert_eq!(app.last_notice(), Some("busy; review agent later"));

    app.config_state
        .as_mut()
        .expect("config state should still exist")
        .focus_footer(ConfigFooterAction::ToggleAgentEnabled);
    let action = app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))?;
    assert!(action.is_none());
    assert_eq!(app.last_notice(), Some("busy; update agent policy later"));
    Ok(())
}

#[test]
fn config_agents_footer_writes_durable_trust_and_policy_entries() -> Result<()> {
    let temp = tempdir()?;
    let workspace = temp.path().join("workspace");
    std::fs::create_dir_all(&workspace)?;
    write_workspace_agent(
        &workspace,
        "review",
        r#"
description = "Review this repository."
instructions = "Review with grep."
invocation_policy = "model_allowed"
allowed_tools = ["grep"]
"#,
    )?;
    let config = config_for_workspace(&workspace);
    let mut app = AppState::from_root_config(&temp.path().join("sigil.toml"), &config);
    app.open_config_panel();
    {
        let state = app
            .config_state
            .as_mut()
            .expect("config state should still exist");
        state.set_section(ConfigSection::Agents);
        state.selected_agent_index = state
            .agent_profiles
            .iter()
            .position(|agent| agent.profile.id.as_str() == "review")
            .expect("review agent should be discovered");
        state.focus_footer(ConfigFooterAction::TrustAgent);
    }

    let action = app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))?;

    assert!(action.is_none());
    assert_eq!(app.last_notice(), Some("agent review trusted"));
    assert!(app.current_session_entries.iter().any(|entry| matches!(
        entry,
        SessionLogEntry::Control(ControlEntry::AgentProfileCaptured(_))
    )));
    assert!(app.current_session_entries.iter().any(|entry| matches!(
        entry,
        SessionLogEntry::Control(ControlEntry::AgentProfileTrustDecision(trust))
            if trust.profile_id.as_str() == "review"
                && trust.decision == sigil_kernel::AgentTrustState::Trusted
    )));
    assert_eq!(
        app.config_state
            .as_ref()
            .and_then(|state| state.selected_agent())
            .map(|agent| agent.trust_state),
        Some(sigil_kernel::AgentTrustState::Trusted)
    );

    app.config_state
        .as_mut()
        .expect("config state should still exist")
        .focus_footer(ConfigFooterAction::ToggleAgentModel);
    let action = app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))?;

    assert!(action.is_none());
    assert_eq!(app.last_notice(), Some("agent review model=no"));
    assert!(app.current_session_entries.iter().any(|entry| matches!(
        entry,
        SessionLogEntry::Control(ControlEntry::AgentProfilePolicyDecision(policy))
            if policy.profile_id.as_str() == "review"
                && policy.model_invocable == Some(false)
                && policy.enabled.is_none()
                && policy.user_invocable.is_none()
    )));
    assert_eq!(
        app.config_state
            .as_ref()
            .and_then(|state| state.selected_agent())
            .map(|agent| agent.effective_model_invocation_allowed()),
        Some(false)
    );

    app.config_state
        .as_mut()
        .expect("config state should still exist")
        .focus_footer(ConfigFooterAction::BlockAgent);
    let action = app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))?;
    assert!(action.is_none());
    assert_eq!(app.last_notice(), Some("agent review blocked"));
    assert!(app.current_session_entries.iter().any(|entry| matches!(
        entry,
        SessionLogEntry::Control(ControlEntry::AgentProfileTrustDecision(trust))
            if trust.profile_id.as_str() == "review"
                && trust.decision == sigil_kernel::AgentTrustState::Disabled
    )));

    app.config_state
        .as_mut()
        .expect("config state should still exist")
        .focus_footer(ConfigFooterAction::ToggleAgentEnabled);
    let action = app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))?;
    assert!(action.is_none());
    assert_eq!(app.last_notice(), Some("agent review enabled=no"));
    assert!(app.current_session_entries.iter().any(|entry| matches!(
        entry,
        SessionLogEntry::Control(ControlEntry::AgentProfilePolicyDecision(policy))
            if policy.profile_id.as_str() == "review" && policy.enabled == Some(false)
    )));

    app.config_state
        .as_mut()
        .expect("config state should still exist")
        .focus_footer(ConfigFooterAction::ToggleAgentUser);
    let action = app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))?;
    assert!(action.is_none());
    assert_eq!(app.last_notice(), Some("agent review user=no"));
    assert!(app.current_session_entries.iter().any(|entry| matches!(
        entry,
        SessionLogEntry::Control(ControlEntry::AgentProfilePolicyDecision(policy))
            if policy.profile_id.as_str() == "review" && policy.user_invocable == Some(false)
    )));

    Ok(())
}

#[test]
fn config_skills_page_keys_cycle_discovered_skills() -> Result<()> {
    let temp = tempdir()?;
    let workspace = temp.path().join("workspace");
    std::fs::create_dir_all(&workspace)?;
    for id in ["alpha", "beta"] {
        write_workspace_skill(
            &workspace,
            id,
            r#"---
trust: trusted
---

# Skill
"#,
        )?;
    }
    let config = config_for_workspace(&workspace);
    let mut app = AppState::from_root_config(&temp.path().join("sigil.toml"), &config);
    app.open_config_panel();
    app.config_state
        .as_mut()
        .expect("config state should exist")
        .set_section(ConfigSection::Skills);
    let initial_detail = app.config_detail_lines().join("\n");
    assert!(initial_detail.contains("> alpha: trusted · inline · workspace · /alpha"));
    assert!(initial_detail.contains("  beta: trusted · inline · workspace · /beta"));

    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE))?;
    assert_eq!(
        app.config_state
            .as_ref()
            .expect("config state should exist")
            .selected_skill_index,
        1
    );
    assert_eq!(app.last_notice(), Some("skill 2/2"));
    let next_detail = app.config_detail_lines().join("\n");
    assert!(next_detail.contains("  alpha: trusted · inline · workspace · /alpha"));
    assert!(next_detail.contains("> beta: trusted · inline · workspace · /beta"));

    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE))?;
    assert_eq!(app.config_selected_footer_action_label(), Some("load"));
    assert_eq!(app.last_notice(), Some("action load_skill"));

    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE))?;
    assert_eq!(
        app.config_state
            .as_ref()
            .expect("config state should exist")
            .selected_skill_index,
        1
    );
    assert_eq!(app.last_notice(), Some("skill 2/2"));

    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE))?;
    assert_eq!(
        app.config_state
            .as_ref()
            .expect("config state should exist")
            .selected_skill_index,
        0
    );
    assert_eq!(app.last_notice(), Some("skill 1/2"));

    let _ = app.handle_key_event(KeyEvent::new(KeyCode::PageUp, KeyModifiers::NONE))?;
    assert_eq!(
        app.config_state
            .as_ref()
            .expect("config state should exist")
            .selected_skill_index,
        1
    );
    assert_eq!(app.last_notice(), Some("skill 2/2"));

    let _ = app.handle_key_event(KeyEvent::new(KeyCode::PageDown, KeyModifiers::NONE))?;
    assert_eq!(
        app.config_state
            .as_ref()
            .expect("config state should exist")
            .selected_skill_index,
        0
    );
    assert_eq!(app.last_notice(), Some("skill 1/2"));

    let empty_workspace = temp.path().join("empty-workspace");
    std::fs::create_dir_all(&empty_workspace)?;
    let mut empty_app = AppState::from_root_config(
        &temp.path().join("empty-sigil.toml"),
        &config_for_workspace(&empty_workspace),
    );
    empty_app.open_config_panel();
    empty_app
        .config_state
        .as_mut()
        .expect("config state should exist")
        .set_section(ConfigSection::Skills);
    let _ = empty_app.handle_key_event(KeyEvent::new(KeyCode::PageUp, KeyModifiers::NONE))?;
    assert_eq!(empty_app.last_notice(), Some("no skill to select"));
    let _ = empty_app.handle_key_event(KeyEvent::new(KeyCode::PageDown, KeyModifiers::NONE))?;
    assert_eq!(empty_app.last_notice(), Some("no skill to select"));

    let mut provider_app = AppState::from_root_config(
        &temp.path().join("provider-sigil.toml"),
        &config_for_workspace(&workspace),
    );
    provider_app.open_config_panel();
    provider_app
        .config_state
        .as_mut()
        .expect("config state should exist")
        .set_section(ConfigSection::Provider);
    let _ = provider_app.handle_key_event(KeyEvent::new(KeyCode::PageUp, KeyModifiers::NONE))?;
    let _ = provider_app.handle_key_event(KeyEvent::new(KeyCode::PageDown, KeyModifiers::NONE))?;
    assert_eq!(provider_app.config_section_title(), Some("Provider"));
    Ok(())
}

#[test]
fn config_skills_step_renders_empty_discovery_and_warnings() -> Result<()> {
    let temp = tempdir()?;
    let workspace = temp.path().join("workspace");
    std::fs::create_dir_all(&workspace)?;
    for index in 0..5 {
        write_workspace_skill(&workspace, &format!("bad skill {index}"), "# Bad")?;
    }
    let config = config_for_workspace(&workspace);
    let mut app = AppState::from_root_config(&temp.path().join("sigil.toml"), &config);
    app.open_config_panel();
    app.config_state
        .as_mut()
        .expect("config state should exist")
        .set_section(ConfigSection::Skills);

    let detail = app.config_detail_lines().join("\n");

    assert!(detail.contains("- Configured: 0 skills"));
    assert!(detail.contains("- Agents: 0 agents"));
    assert!(detail.contains("- Warnings: 5 warnings"));
    assert!(detail.contains("i No skills discovered"));
    assert!(detail.contains("Reusable inline skills are discovered"));
    assert!(detail.contains("[warnings]"));
    assert!(detail.contains("... 1 more warnings"));
    assert!(detail.contains("skills: Up/Down skill · PgUp/PgDn wrap · footer load/invoke"));
    Ok(())
}

#[test]
fn config_skills_load_footer_submits_load_prompt() -> Result<()> {
    let temp = tempdir()?;
    let workspace = temp.path().join("workspace");
    std::fs::create_dir_all(&workspace)?;
    write_workspace_skill(
        &workspace,
        "review",
        r#"---
trust: trusted
---

# Review
"#,
    )?;
    let config = config_for_workspace(&workspace);
    let mut app = AppState::from_root_config(&temp.path().join("sigil.toml"), &config);
    app.open_config_panel();
    let state = app
        .config_state
        .as_mut()
        .expect("config state should exist");
    state.set_section(ConfigSection::Skills);
    state.focus_footer(ConfigFooterAction::LoadSkill);

    let action = app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))?;

    let Some(AppAction::SubmitPrompt(prompt)) = action else {
        panic!("expected load prompt action");
    };
    assert!(prompt.contains("`load_skill`"));
    assert!(prompt.contains("`review`"));
    assert_eq!(app.last_notice(), Some("loading skill review"));
    assert!(!app.is_config_mode());
    Ok(())
}

#[test]
fn config_skills_footer_guards_busy_wrong_section_and_empty_selection() -> Result<()> {
    let temp = tempdir()?;
    let workspace = temp.path().join("workspace");
    std::fs::create_dir_all(&workspace)?;
    write_workspace_skill(
        &workspace,
        "review",
        r#"---
trust: trusted
---

# Review
"#,
    )?;
    let config = config_for_workspace(&workspace);
    let mut app = AppState::from_root_config(&temp.path().join("sigil.toml"), &config);
    app.open_config_panel();
    app.is_busy = true;
    {
        let state = app
            .config_state
            .as_mut()
            .expect("config state should exist");
        state.set_section(ConfigSection::Skills);
        state.focus_footer(ConfigFooterAction::LoadSkill);
    }
    let action = app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))?;
    assert!(action.is_none());
    assert_eq!(app.last_notice(), Some("busy; load skill later"));

    app.is_busy = false;
    {
        let state = app
            .config_state
            .as_mut()
            .expect("config state should exist");
        state.set_section(ConfigSection::Provider);
        state.focus_footer(ConfigFooterAction::LoadSkill);
    }
    let action = app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))?;
    assert!(action.is_none());
    assert_eq!(
        app.last_notice(),
        Some("skill action is available in Skills config")
    );

    let empty_workspace = temp.path().join("empty-workspace");
    std::fs::create_dir_all(&empty_workspace)?;
    let mut empty_app = AppState::from_root_config(
        &temp.path().join("empty-sigil.toml"),
        &config_for_workspace(&empty_workspace),
    );
    empty_app.open_config_panel();
    {
        let state = empty_app
            .config_state
            .as_mut()
            .expect("config state should exist");
        state.set_section(ConfigSection::Skills);
        state.focus_footer(ConfigFooterAction::LoadSkill);
    }
    let action = empty_app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))?;
    assert!(action.is_none());
    assert_eq!(empty_app.last_notice(), Some("no skill selected"));
    Ok(())
}

#[test]
fn config_skills_invoke_footer_collects_arguments_and_submits_prompt() -> Result<()> {
    let temp = tempdir()?;
    let workspace = temp.path().join("workspace");
    std::fs::create_dir_all(&workspace)?;
    write_workspace_skill(
        &workspace,
        "review",
        r#"---
trust: trusted
---

# Review
"#,
    )?;
    let config = config_for_workspace(&workspace);
    let mut app = AppState::from_root_config(&temp.path().join("sigil.toml"), &config);
    app.open_config_panel();
    let state = app
        .config_state
        .as_mut()
        .expect("config state should exist");
    state.set_section(ConfigSection::Skills);
    state.focus_footer(ConfigFooterAction::InvokeSkill);

    let action = app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))?;
    assert!(action.is_none());
    assert_eq!(app.modal_title(), Some("Skill Arguments"));
    let modal = app.modal_lines().join("\n");
    assert!(modal.contains("Arguments passed to the selected skill invocation."));
    assert!(modal.contains("arguments: |"));
    assert!(!modal.contains("key:"));

    for character in "crates/sigil-tui".chars() {
        let _ =
            app.handle_key_event(KeyEvent::new(KeyCode::Char(character), KeyModifiers::NONE))?;
    }
    let action = app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))?;

    let Some(AppAction::SubmitPrompt(prompt)) = action else {
        panic!("expected invoke prompt action");
    };
    assert!(prompt.contains("`load_skill`"));
    assert!(prompt.contains("`review`"));
    assert!(prompt.contains("crates/sigil-tui"));
    assert_eq!(app.last_notice(), Some("invoking skill review"));
    assert!(!app.is_config_mode());
    Ok(())
}

#[test]
fn config_skills_invoke_empty_arguments_submits_no_argument_prompt() -> Result<()> {
    let temp = tempdir()?;
    let workspace = temp.path().join("workspace");
    std::fs::create_dir_all(&workspace)?;
    write_workspace_skill(
        &workspace,
        "review",
        r#"---
trust: trusted
---

# Review
"#,
    )?;
    let config = config_for_workspace(&workspace);
    let mut app = AppState::from_root_config(&temp.path().join("sigil.toml"), &config);
    app.open_config_panel();
    let state = app
        .config_state
        .as_mut()
        .expect("config state should exist");
    state.set_section(ConfigSection::Skills);
    state.focus_footer(ConfigFooterAction::InvokeSkill);

    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))?;
    let action = app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))?;

    let Some(AppAction::SubmitPrompt(prompt)) = action else {
        panic!("expected invoke prompt action");
    };
    assert!(prompt.contains("No additional arguments were provided."));
    assert_eq!(app.last_notice(), Some("invoking skill review"));
    Ok(())
}

#[test]
fn config_skills_invoke_modal_shortcuts_submit_prompt_actions() -> Result<()> {
    for (key_code, expected_notice) in [
        (KeyCode::F(2), "invoking skill review"),
        (KeyCode::F(3), "invoking skill review"),
    ] {
        let temp = tempdir()?;
        let workspace = temp.path().join("workspace");
        std::fs::create_dir_all(&workspace)?;
        write_workspace_skill(
            &workspace,
            "review",
            r#"---
trust: trusted
---

# Review
"#,
        )?;
        let config = config_for_workspace(&workspace);
        let mut app = AppState::from_root_config(&temp.path().join("sigil.toml"), &config);
        app.open_config_panel();
        let state = app
            .config_state
            .as_mut()
            .expect("config state should exist");
        state.set_section(ConfigSection::Skills);
        state.focus_footer(ConfigFooterAction::InvokeSkill);

        let action = app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))?;
        assert!(action.is_none());
        for character in "target module".chars() {
            let _ =
                app.handle_key_event(KeyEvent::new(KeyCode::Char(character), KeyModifiers::NONE))?;
        }

        let action = app.handle_key_event(KeyEvent::new(key_code, KeyModifiers::NONE))?;

        let Some(AppAction::SubmitPrompt(prompt)) = action else {
            panic!("expected invoke prompt action");
        };
        assert!(prompt.contains("target module"));
        assert_eq!(app.last_notice(), Some(expected_notice));
        assert!(!app.is_config_mode());
    }
    Ok(())
}

#[test]
fn config_skills_footer_refuses_untrusted_skill_actions() -> Result<()> {
    let temp = tempdir()?;
    let workspace = temp.path().join("workspace");
    std::fs::create_dir_all(&workspace)?;
    write_workspace_skill(
        &workspace,
        "draft",
        r#"---
description: Needs review before use.
---

# Draft
"#,
    )?;
    let config = config_for_workspace(&workspace);
    let mut app = AppState::from_root_config(&temp.path().join("sigil.toml"), &config);
    app.open_config_panel();
    let state = app
        .config_state
        .as_mut()
        .expect("config state should exist");
    state.set_section(ConfigSection::Skills);
    state.focus_footer(ConfigFooterAction::LoadSkill);

    let action = app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))?;

    assert!(action.is_none());
    assert_eq!(app.last_notice(), Some("skill draft is not trusted"));
    assert!(app.is_config_mode());
    Ok(())
}

#[test]
fn config_plugins_step_discovers_and_renders_trust_review_details() -> Result<()> {
    let temp = tempdir()?;
    let workspace = temp.path().join("workspace");
    std::fs::create_dir_all(&workspace)?;
    write_workspace_plugin(&workspace, "repo-review", "0.1.0")?;
    let config = config_for_workspace(&workspace);
    let mut app = AppState::from_root_config(&temp.path().join("sigil.toml"), &config);
    app.open_config_panel();
    app.config_state
        .as_mut()
        .expect("config state should still exist")
        .set_section(ConfigSection::Plugins);

    let detail = app.config_detail_lines().join("\n");

    assert!(detail.contains("Plugins 10/11 · plugin trust review"));
    assert!(detail.contains("[discovery]"));
    assert!(detail.contains("- Configured: 1 plugins"));
    assert!(detail.contains("- Selected: 1 of 1"));
    assert!(detail.contains("[plugins]"));
    assert!(detail.contains("> repo-review: needs_review · 0.1.0"));
    assert!(detail.contains("[plugin]"));
    assert!(detail.contains("Plugin: repo-review"));
    assert!(detail.contains("- Name: Repository Review"));
    assert!(detail.contains("- Version: 0.1.0"));
    assert!(detail.contains("- Description: Reusable review pack."));
    assert!(detail.contains("- Trust: needs_review"));
    assert!(detail.contains("- Manifest: .sigil/plugins/repo-review/plugin.toml"));
    let manifest_hash = app
        .config_state
        .as_ref()
        .and_then(|state| state.selected_plugin())
        .map(|plugin| plugin.manifest_hash.clone())
        .expect("plugin should be selected");
    assert!(detail.contains(&format!("- Hash: {}", &manifest_hash[..48])));
    assert!(detail.contains(&format!("- Hash part 2: {}", &manifest_hash[48..])));
    assert!(detail.contains(
        "- Implications: agent profiles, skill instructions, hook commands, MCP server processes"
    ));
    assert!(detail.contains("[agents]"));
    assert!(detail.contains("- Agent 1: agents/reviewer/agent.toml"));
    assert!(detail.contains("[skills]"));
    assert!(detail.contains("- Skill 1: skills/review/SKILL.md"));
    assert!(detail.contains("[hooks]"));
    assert!(detail.contains("- Hook 1: pre_tool_use"));
    assert!(detail.contains("- Hook 1 command: scripts/check-tool-policy.sh --policy strict"));
    assert!(detail.contains("- Hook 1 approval: ask"));
    assert!(detail.contains("[mcp servers]"));
    assert!(detail.contains("- MCP 1: repo-tools"));
    assert!(detail.contains("- MCP 1 command: node server.js"));
    assert!(detail.contains("- MCP 1 startup: lazy"));
    assert!(detail.contains("- MCP 1 required: no"));
    assert!(detail.contains("- Approve: trusts this manifest hash"));
    assert!(detail.contains("- Deny: disables this manifest hash"));
    assert!(detail.contains("plugins: Up/Down plugin · PgUp/PgDn wrap · footer approve/deny"));

    let nav = app.config_nav_lines().join("\n");
    assert!(nav.contains("Plugins: Up/Down select"));
    assert!(nav.contains("Plugins: PgUp/PgDn wrap"));
    assert!(nav.contains("Plugins: footer approve/deny"));
    assert_eq!(
        app.config_footer_action_labels(),
        vec!["approve", "deny", "close"]
    );
    Ok(())
}

#[test]
fn config_plugins_step_renders_empty_discovery_and_warning_overflow() -> Result<()> {
    let temp = tempdir()?;
    let workspace = temp.path().join("workspace");
    std::fs::create_dir_all(&workspace)?;
    for index in 0..5 {
        write_invalid_workspace_plugin(&workspace, &format!("bad-{index}"))?;
    }
    let config = config_for_workspace(&workspace);
    let mut app = AppState::from_root_config(&temp.path().join("sigil.toml"), &config);
    app.open_config_panel();
    app.config_state
        .as_mut()
        .expect("config state should still exist")
        .set_section(ConfigSection::Plugins);

    let detail = app.config_detail_lines().join("\n");

    assert!(detail.contains("- Configured: 0 plugins"));
    assert!(detail.contains("- Warnings: 5 warnings"));
    assert!(detail.contains("No plugin manifests discovered"));
    assert!(detail.contains("Workspace plugins live under .sigil/plugins/<id>/plugin.toml"));
    assert!(detail.contains("[warnings]"));
    assert!(detail.contains("... 1 more warnings"));
    Ok(())
}

#[test]
fn config_plugins_review_renders_complete_command_surface() -> Result<()> {
    let temp = tempdir()?;
    let workspace = temp.path().join("workspace");
    let plugin_root = workspace.join(".sigil/plugins/command-pack");
    std::fs::create_dir_all(&plugin_root)?;
    std::fs::write(
        plugin_root.join("plugin.toml"),
        r#"id = "command-pack"
name = "Command Pack"
version = "0.1.0"

[[hooks]]
event = "pre_tool_use"
command = "scripts/hook-1.sh"
args = ["--flag-1"]
approval = "ask"

[[hooks]]
event = "post_tool_use"
command = "scripts/hook-2.sh"
args = ["--flag-2"]
approval = "ask"

[[hooks]]
event = "session_start"
command = "scripts/hook-3.sh"
args = ["--flag-3"]
approval = "deny"

[[hooks]]
event = "session_stop"
command = "scripts/hook-4.sh"
args = ["--flag-4", "two words"]
approval = "allow"

[[mcp_servers]]
name = "tools-1"
command = "node"
args = ["server-1.js"]
startup = "lazy"
required = false

[[mcp_servers]]
name = "tools-2"
command = "node"
args = ["server-2.js"]
startup = "lazy"
required = false

[[mcp_servers]]
name = "tools-3"
command = "node"
args = ["server-3.js"]
startup = "eager"
required = true

[[mcp_servers]]
name = "tools-4"
command = "node"
args = ["server-4.js"]
startup = "eager"
required = true
"#,
    )?;
    let config = config_for_workspace(&workspace);
    let mut app = AppState::from_root_config(&temp.path().join("sigil.toml"), &config);
    app.open_config_panel();
    app.config_state
        .as_mut()
        .expect("config state should still exist")
        .set_section(ConfigSection::Plugins);

    let detail = app.config_detail_lines().join("\n");

    for expected in [
        "- Hook 1: pre_tool_use",
        "- Hook 1 command: scripts/hook-1.sh --flag-1",
        "- Hook 1 approval: ask",
        "- Hook 2: post_tool_use",
        "- Hook 2 command: scripts/hook-2.sh --flag-2",
        "- Hook 2 approval: ask",
        "- Hook 3: session_start",
        "- Hook 3 command: scripts/hook-3.sh --flag-3",
        "- Hook 3 approval: deny",
        "- Hook 4: session_stop",
        r#"- Hook 4 command: scripts/hook-4.sh --flag-4 "two words""#,
        "- Hook 4 approval: allow",
        "- MCP 1: tools-1",
        "- MCP 1 command: node server-1.js",
        "- MCP 1 startup: lazy",
        "- MCP 1 required: no",
        "- MCP 2: tools-2",
        "- MCP 2 command: node server-2.js",
        "- MCP 2 startup: lazy",
        "- MCP 2 required: no",
        "- MCP 3: tools-3",
        "- MCP 3 command: node server-3.js",
        "- MCP 3 startup: eager",
        "- MCP 3 required: yes",
        "- MCP 4: tools-4",
        "- MCP 4 command: node server-4.js",
        "- MCP 4 startup: eager",
        "- MCP 4 required: yes",
    ] {
        assert!(detail.contains(expected), "missing {expected}");
    }
    assert!(!detail.contains("- Hooks:"));
    assert!(!detail.contains("- MCP:"));
    Ok(())
}

#[test]
fn config_plugins_page_keys_cycle_discovered_plugins() -> Result<()> {
    let temp = tempdir()?;
    let workspace = temp.path().join("workspace");
    std::fs::create_dir_all(&workspace)?;
    write_workspace_plugin(&workspace, "alpha", "0.1.0")?;
    write_workspace_plugin(&workspace, "beta", "0.1.0")?;
    let config = config_for_workspace(&workspace);
    let mut app = AppState::from_root_config(&temp.path().join("sigil.toml"), &config);
    app.open_config_panel();
    app.config_state
        .as_mut()
        .expect("config state should exist")
        .set_section(ConfigSection::Plugins);

    let initial_detail = app.config_detail_lines().join("\n");
    assert!(initial_detail.contains("> alpha: needs_review · 0.1.0"));
    assert!(initial_detail.contains("  beta: needs_review · 0.1.0"));

    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE))?;
    assert_eq!(
        app.config_state
            .as_ref()
            .expect("config state should exist")
            .selected_plugin_index,
        1
    );
    assert_eq!(app.last_notice(), Some("plugin 2/2"));
    let next_detail = app.config_detail_lines().join("\n");
    assert!(next_detail.contains("  alpha: needs_review · 0.1.0"));
    assert!(next_detail.contains("> beta: needs_review · 0.1.0"));

    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE))?;
    assert_eq!(app.config_selected_footer_action_label(), Some("approve"));
    assert_eq!(app.last_notice(), Some("action approve_plugin"));

    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE))?;
    assert_eq!(
        app.config_state
            .as_ref()
            .expect("config state should exist")
            .selected_plugin_index,
        1
    );
    assert_eq!(app.last_notice(), Some("plugin 2/2"));

    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE))?;
    assert_eq!(
        app.config_state
            .as_ref()
            .expect("config state should exist")
            .selected_plugin_index,
        0
    );
    assert_eq!(app.last_notice(), Some("plugin 1/2"));

    let _ = app.handle_key_event(KeyEvent::new(KeyCode::PageUp, KeyModifiers::NONE))?;
    assert_eq!(
        app.config_state
            .as_ref()
            .expect("config state should exist")
            .selected_plugin_index,
        1
    );
    assert_eq!(app.last_notice(), Some("plugin 2/2"));

    let _ = app.handle_key_event(KeyEvent::new(KeyCode::PageDown, KeyModifiers::NONE))?;
    assert_eq!(
        app.config_state
            .as_ref()
            .expect("config state should exist")
            .selected_plugin_index,
        0
    );
    assert_eq!(app.last_notice(), Some("plugin 1/2"));

    let empty_workspace = temp.path().join("empty-workspace");
    std::fs::create_dir_all(&empty_workspace)?;
    let mut empty_app = AppState::from_root_config(
        &temp.path().join("empty-sigil.toml"),
        &config_for_workspace(&empty_workspace),
    );
    empty_app.open_config_panel();
    empty_app
        .config_state
        .as_mut()
        .expect("config state should exist")
        .set_section(ConfigSection::Plugins);
    let _ = empty_app.handle_key_event(KeyEvent::new(KeyCode::PageUp, KeyModifiers::NONE))?;
    assert_eq!(empty_app.last_notice(), Some("no plugin to select"));
    let _ = empty_app.handle_key_event(KeyEvent::new(KeyCode::PageDown, KeyModifiers::NONE))?;
    assert_eq!(empty_app.last_notice(), Some("no plugin to select"));
    Ok(())
}

#[test]
fn config_plugins_footer_writes_append_only_trust_entry() -> Result<()> {
    let temp = tempdir()?;
    let workspace = temp.path().join("workspace");
    std::fs::create_dir_all(&workspace)?;
    write_workspace_plugin(&workspace, "repo-review", "0.1.0")?;
    let config = config_for_workspace(&workspace);
    let mut app = AppState::from_root_config(&temp.path().join("sigil.toml"), &config);
    app.open_config_panel();
    let session_log_path = app.session_log_path.clone();
    let state = app
        .config_state
        .as_mut()
        .expect("config state should exist");
    state.set_section(ConfigSection::Plugins);
    state.focus_footer(ConfigFooterAction::ApprovePlugin);

    let action = app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))?;

    assert!(action.is_none());
    assert_eq!(app.last_notice(), Some("plugin repo-review approved"));
    let state = app
        .config_state
        .as_ref()
        .expect("config state should exist");
    assert_eq!(
        state.selected_plugin().map(|plugin| plugin.trust),
        Some(sigil_kernel::PluginTrustDecision::Trusted)
    );
    let entries = JsonlSessionStore::read_entries(&session_log_path)?;
    assert_eq!(entries.len(), 3);
    assert!(matches!(
        entries[0],
        SessionLogEntry::Control(ControlEntry::SessionIdentity { .. })
    ));
    let manifest = match &entries[1] {
        SessionLogEntry::Control(ControlEntry::PluginManifestCaptured(manifest)) => manifest,
        other => panic!("expected manifest capture, got {other:?}"),
    };
    assert_eq!(manifest.plugin_id, "repo-review");
    assert_eq!(
        manifest.trust,
        sigil_kernel::PluginTrustDecision::NeedsReview
    );
    assert!(manifest.capabilities.iter().any(|capability| matches!(
        capability,
        sigil_kernel::PluginCapability::Hook { args, .. }
            if args == &vec!["--policy".to_owned(), "strict".to_owned()]
    )));
    let trust = match &entries[2] {
        SessionLogEntry::Control(ControlEntry::PluginTrustDecision(trust)) => trust,
        other => panic!("expected trust decision, got {other:?}"),
    };
    assert_eq!(trust.plugin_id, "repo-review");
    assert_eq!(trust.manifest_hash, manifest.manifest_hash);
    assert_eq!(trust.decision, sigil_kernel::PluginTrustDecision::Trusted);
    Ok(())
}

#[test]
fn config_plugins_footer_denies_and_guards_busy_wrong_section_and_empty_selection() -> Result<()> {
    let temp = tempdir()?;
    let workspace = temp.path().join("workspace");
    std::fs::create_dir_all(&workspace)?;
    write_workspace_plugin(&workspace, "repo-review", "0.1.0")?;
    let config = config_for_workspace(&workspace);
    let mut app = AppState::from_root_config(&temp.path().join("sigil.toml"), &config);
    app.open_config_panel();
    app.is_busy = true;
    {
        let state = app
            .config_state
            .as_mut()
            .expect("config state should exist");
        state.set_section(ConfigSection::Plugins);
        state.focus_footer(ConfigFooterAction::DenyPlugin);
    }
    let action = app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))?;
    assert!(action.is_none());
    assert_eq!(app.last_notice(), Some("busy; review plugin later"));

    app.is_busy = false;
    {
        let state = app
            .config_state
            .as_mut()
            .expect("config state should exist");
        state.set_section(ConfigSection::Provider);
        state.focus_footer(ConfigFooterAction::DenyPlugin);
    }
    let action = app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))?;
    assert!(action.is_none());
    assert_eq!(
        app.last_notice(),
        Some("plugin review is available in Plugins config")
    );

    let empty_workspace = temp.path().join("empty-workspace");
    std::fs::create_dir_all(&empty_workspace)?;
    let mut empty_app = AppState::from_root_config(
        &temp.path().join("empty-sigil.toml"),
        &config_for_workspace(&empty_workspace),
    );
    empty_app.open_config_panel();
    {
        let state = empty_app
            .config_state
            .as_mut()
            .expect("config state should exist");
        state.set_section(ConfigSection::Plugins);
        state.focus_footer(ConfigFooterAction::DenyPlugin);
    }
    let action = empty_app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))?;
    assert!(action.is_none());
    assert_eq!(empty_app.last_notice(), Some("no plugin selected"));

    let mut deny_app = AppState::from_root_config(
        &temp.path().join("deny-sigil.toml"),
        &config_for_workspace(&workspace),
    );
    deny_app.open_config_panel();
    let state = deny_app
        .config_state
        .as_mut()
        .expect("config state should exist");
    state.set_section(ConfigSection::Plugins);
    state.focus_footer(ConfigFooterAction::DenyPlugin);
    let action = deny_app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))?;
    assert!(action.is_none());
    assert_eq!(deny_app.last_notice(), Some("plugin repo-review denied"));
    let entries = JsonlSessionStore::read_entries(&deny_app.session_log_path)?;
    assert!(entries.iter().any(|entry| matches!(
        entry,
        SessionLogEntry::Control(ControlEntry::PluginTrustDecision(trust))
            if trust.decision == sigil_kernel::PluginTrustDecision::Disabled
    )));
    Ok(())
}

#[test]
fn config_plugins_footer_reloads_manifest_before_reviewing_hash() -> Result<()> {
    let temp = tempdir()?;
    let workspace = temp.path().join("workspace");
    std::fs::create_dir_all(&workspace)?;
    write_workspace_plugin(&workspace, "repo-review", "0.1.0")?;
    let config = config_for_workspace(&workspace);
    let mut app = AppState::from_root_config(&temp.path().join("sigil.toml"), &config);
    app.open_config_panel();
    let session_log_path = app.session_log_path.clone();
    let state = app
        .config_state
        .as_mut()
        .expect("config state should exist");
    state.set_section(ConfigSection::Plugins);
    let old_hash = state
        .selected_plugin()
        .expect("plugin should be discovered")
        .manifest_hash
        .clone();
    state.focus_footer(ConfigFooterAction::ApprovePlugin);
    write_workspace_plugin(&workspace, "repo-review", "0.2.0")?;

    let action = app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))?;

    assert!(action.is_none());
    assert_eq!(
        app.last_notice(),
        Some("plugin repo-review changed; review refreshed")
    );
    let state = app
        .config_state
        .as_ref()
        .expect("config state should exist");
    let refreshed = state
        .selected_plugin()
        .expect("plugin should still be selected");
    assert_eq!(refreshed.version, "0.2.0");
    assert_ne!(refreshed.manifest_hash, old_hash);
    assert_eq!(
        refreshed.trust,
        sigil_kernel::PluginTrustDecision::NeedsReview
    );
    assert!(JsonlSessionStore::read_entries(&session_log_path)?.is_empty());
    Ok(())
}

#[test]
fn config_plugins_footer_reloads_manifest_before_reviewing_missing_plugin() -> Result<()> {
    let temp = tempdir()?;
    let workspace = temp.path().join("workspace");
    std::fs::create_dir_all(&workspace)?;
    write_workspace_plugin(&workspace, "repo-review", "0.1.0")?;
    let config = config_for_workspace(&workspace);
    let mut app = AppState::from_root_config(&temp.path().join("sigil.toml"), &config);
    app.open_config_panel();
    app.config_state
        .as_mut()
        .expect("config state should still exist")
        .set_section(ConfigSection::Plugins);
    app.config_state
        .as_mut()
        .expect("config state should still exist")
        .focus_footer(ConfigFooterAction::ApprovePlugin);
    std::fs::remove_file(
        workspace
            .join(".sigil")
            .join("plugins")
            .join("repo-review")
            .join("plugin.toml"),
    )?;

    let action = app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))?;

    assert!(action.is_none());
    assert_eq!(
        app.last_notice(),
        Some("plugin repo-review is no longer available; review refreshed")
    );
    assert!(!app.current_session_entries.iter().any(|entry| matches!(
        entry,
        SessionLogEntry::Control(ControlEntry::PluginTrustDecision(_))
    )));
    Ok(())
}

#[test]
fn config_mode_closes_on_escape() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.open_config_panel();

    let action = app.handle_key_event(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE))?;

    assert!(action.is_none());
    assert!(!app.is_config_mode());
    Ok(())
}

#[test]
fn config_save_persists_draft_and_returns_reload_action() -> Result<()> {
    let temp = tempdir()?;
    let config_path = temp.path().join("sigil.toml");
    test_config().save(&config_path)?;

    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.config_path = config_path.clone();
    app.open_config_panel();
    let state = app
        .config_state
        .as_mut()
        .expect("config state should exist after opening /config");
    state.set_section(ConfigSection::Provider);
    state.selected_field = Some(ConfigField::ProviderModel);
    state.draft.provider_model = "deepseek-v4-pro".to_owned();
    state.draft.provider_base_url = "https://example.invalid/api".to_owned();
    state.draft.provider_user_id_strategy = "stable_per_workspace".to_owned();
    state.draft.provider_fim_model = "deepseek-v4-flash".to_owned();
    state.draft.permission_default_mode = ApprovalMode::Allow;
    state.draft.memory_enabled = false;
    state.draft.compaction_soft_threshold_ratio = "0.40".to_owned();
    state.draft.compaction_hard_threshold_ratio = "0.75".to_owned();
    state.draft.compaction_context_window_tokens = "64000".to_owned();
    state.draft.code_intelligence_enabled = true;
    state.draft.code_intelligence_startup = sigil_kernel::CodeIntelStartup::Eager;
    state.draft.code_intelligence_discovery_enabled = false;
    state.draft.code_intelligence_discovery_report_missing = true;
    state.draft.terminal_mouse_capture = false;
    state.draft.terminal_osc52_clipboard = false;
    state.draft.terminal_scroll_sensitivity = "6".to_owned();
    state.dirty = true;

    let action = app.handle_key_event(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL))?;

    let Some(AppAction::ConfigSaved { root_config }) = action else {
        panic!("expected config save action");
    };
    assert_eq!(root_config.agent.model, "deepseek-v4-pro");
    assert_eq!(root_config.permission.default_mode, ApprovalMode::Allow);
    assert!(!root_config.memory.enabled);
    assert_eq!(root_config.compaction.soft_threshold_ratio, 0.40);
    assert_eq!(root_config.compaction.hard_threshold_ratio, 0.75);
    assert_eq!(root_config.compaction.context_window_tokens, Some(64_000));
    assert!(root_config.code_intelligence.enabled);
    assert_eq!(
        root_config.code_intelligence.startup,
        sigil_kernel::CodeIntelStartup::Eager
    );
    assert!(!root_config.code_intelligence.discovery.enabled);
    assert!(root_config.code_intelligence.discovery.report_missing);
    assert!(!root_config.terminal.mouse_capture);
    assert!(!root_config.terminal.osc52_clipboard);
    assert_eq!(root_config.terminal.scroll_sensitivity, 6);
    assert!(!app.config_is_dirty());
    assert_eq!(app.permission_default_mode, "allow");
    assert!(!app.memory_enabled);

    let saved = RootConfig::load(&config_path)?;
    assert_eq!(saved.agent.model, "deepseek-v4-pro");
    assert_eq!(saved.permission.default_mode, ApprovalMode::Allow);
    assert!(!saved.memory.enabled);
    assert_eq!(saved.compaction.soft_threshold_ratio, 0.40);
    assert_eq!(saved.compaction.hard_threshold_ratio, 0.75);
    assert_eq!(saved.compaction.context_window_tokens, Some(64_000));
    assert!(saved.code_intelligence.enabled);
    assert_eq!(
        saved.code_intelligence.startup,
        sigil_kernel::CodeIntelStartup::Eager
    );
    assert!(!saved.code_intelligence.discovery.enabled);
    assert!(saved.code_intelligence.discovery.report_missing);
    assert!(!saved.terminal.mouse_capture);
    assert!(!saved.terminal.osc52_clipboard);
    assert_eq!(saved.terminal.scroll_sensitivity, 6);
    let saved_raw = std::fs::read_to_string(&config_path)?;
    assert!(saved_raw.contains("fallback_context_window_tokens = 64000"));
    assert!(
        !saved_raw
            .lines()
            .any(|line| line.trim_start().starts_with("context_window_tokens ="))
    );
    assert_eq!(
        saved
            .providers
            .get("deepseek")
            .and_then(|value| value.get("model"))
            .and_then(|value| value.as_str()),
        Some("deepseek-v4-pro")
    );
    assert_eq!(
        saved
            .providers
            .get("deepseek")
            .and_then(|value| value.get("base_url"))
            .and_then(|value| value.as_str()),
        Some("https://example.invalid/api")
    );
    assert_eq!(
        saved
            .providers
            .get("deepseek")
            .and_then(|value| value.get("user_id_strategy"))
            .and_then(|value| value.as_str()),
        Some("stable_per_workspace")
    );
    assert_eq!(
        saved
            .providers
            .get("deepseek")
            .and_then(|value| value.get("fim_model"))
            .and_then(|value| value.as_str()),
        Some("deepseek-v4-flash")
    );
    Ok(())
}

#[test]
fn runtime_permission_toggle_persists_default_mode_to_config() -> Result<()> {
    let temp = tempdir()?;
    let config_path = temp.path().join("sigil.toml");
    let root_config = test_config();
    root_config.save(&config_path)?;

    let mut app = AppState::from_root_config(&config_path, &root_config);

    let action = app.handle_key_event(KeyEvent::new(KeyCode::BackTab, KeyModifiers::NONE))?;

    let Some(AppAction::RuntimeConfigUpdated { root_config }) = action else {
        panic!("expected runtime config update action");
    };
    assert_eq!(root_config.permission.default_mode, ApprovalMode::Deny);
    assert_eq!(app.permission_default_mode, "deny");

    let saved = RootConfig::load(&config_path)?;
    assert_eq!(saved.permission.default_mode, ApprovalMode::Deny);
    let reopened = AppState::from_root_config(&config_path, &saved);
    assert_eq!(reopened.permission_default_mode, "deny");
    Ok(())
}

#[test]
fn config_can_add_and_persist_mcp_server() -> Result<()> {
    let temp = tempdir()?;
    let config_path = temp.path().join("sigil.toml");
    test_config().save(&config_path)?;

    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.config_path = config_path.clone();
    app.open_config_panel();
    {
        let state = app
            .config_state
            .as_mut()
            .expect("config state should exist after opening /config");
        state.set_section(ConfigSection::Mcp);
    }

    let action = app.handle_key_event(KeyEvent::new(KeyCode::Char('n'), KeyModifiers::CONTROL))?;
    assert!(action.is_none());
    let state = app
        .config_state
        .as_mut()
        .expect("config state should still exist");
    assert_eq!(state.draft.mcp_servers.len(), 1);
    state.draft.mcp_servers[0].name = "filesystem".to_owned();
    state.draft.mcp_servers[0].command = "npx".to_owned();
    state.draft.mcp_servers[0].args_csv =
        "-y, @modelcontextprotocol/server-filesystem, .".to_owned();
    state.draft.mcp_servers[0].startup_timeout_secs = "15".to_owned();

    let detail = app.config_detail_lines().join("\n");
    assert!(detail.contains("[server]"));
    assert!(detail.contains("Name"));
    assert!(detail.contains("Command"));
    assert!(detail.contains("Arguments"));
    assert!(detail.contains("[lifecycle]"));
    assert!(detail.contains("Required"));
    assert!(detail.contains("Startup"));
    assert!(detail.contains("Trust"));
    assert!(detail.contains("Secrets"));
    assert!(!detail.contains("args_csv:"));

    let action = app.handle_key_event(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL))?;

    let Some(AppAction::ConfigSaved { root_config }) = action else {
        panic!("expected config save action");
    };
    assert_eq!(root_config.mcp_servers.len(), 1);
    assert_eq!(root_config.mcp_servers[0].name, "filesystem");
    assert_eq!(root_config.mcp_servers[0].command, "npx");
    assert_eq!(
        root_config.mcp_servers[0].args,
        vec![
            "-y".to_owned(),
            "@modelcontextprotocol/server-filesystem".to_owned(),
            ".".to_owned()
        ]
    );
    assert_eq!(root_config.mcp_servers[0].startup_timeout_secs, 15);
    assert!(root_config.mcp_servers[0].required);
    assert_eq!(root_config.mcp_servers[0].startup, McpServerStartup::Eager);
    assert_eq!(
        root_config.mcp_servers[0].trust.trust_class,
        McpTrustClass::SelfHosted
    );
    assert_eq!(
        root_config.mcp_servers[0].trust.approval_default,
        ApprovalMode::Ask
    );

    let saved = RootConfig::load(&config_path)?;
    assert_eq!(saved.mcp_servers.len(), 1);
    assert_eq!(saved.mcp_servers[0].name, "filesystem");
    assert_eq!(saved.mcp_servers[0].command, "npx");
    assert_eq!(
        saved.mcp_servers[0].args,
        vec![
            "-y".to_owned(),
            "@modelcontextprotocol/server-filesystem".to_owned(),
            ".".to_owned()
        ]
    );
    assert_eq!(saved.mcp_servers[0].startup_timeout_secs, 15);
    assert!(saved.mcp_servers[0].required);
    assert_eq!(saved.mcp_servers[0].startup, McpServerStartup::Eager);
    assert_eq!(
        saved.mcp_servers[0].trust.trust_class,
        McpTrustClass::SelfHosted
    );
    assert_eq!(
        saved.mcp_servers[0].trust.approval_default,
        ApprovalMode::Ask
    );
    Ok(())
}

#[test]
fn config_save_is_blocked_while_busy() -> Result<()> {
    let temp = tempdir()?;
    let config_path = temp.path().join("sigil.toml");
    test_config().save(&config_path)?;

    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.config_path = config_path.clone();
    app.open_config_panel();
    let state = app
        .config_state
        .as_mut()
        .expect("config state should exist after opening /config");
    state.set_section(ConfigSection::Provider);
    state.selected_field = Some(ConfigField::ProviderModel);
    state.draft.provider_model = "deepseek-v4-pro".to_owned();
    state.dirty = true;
    app.is_busy = true;

    let action = app.handle_key_event(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL))?;

    assert!(action.is_none());
    assert_eq!(app.last_notice(), Some("busy; save later"));
    let saved = RootConfig::load(&config_path)?;
    assert_eq!(saved.agent.model, "deepseek-v4-flash");
    Ok(())
}

#[test]
fn config_close_requires_second_escape_when_dirty() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.open_config_panel();
    let state = app
        .config_state
        .as_mut()
        .expect("config state should exist after opening /config");
    state.draft.provider_model = "deepseek-v4-pro".to_owned();
    state.dirty = true;
    assert_eq!(
        app.config_footer_hint(),
        "status: unsaved - save before close"
    );

    let first = app.handle_key_event(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE))?;
    assert!(first.is_none());
    assert!(app.is_config_mode());
    assert_eq!(app.config_selected_field_label(), Some("save"));
    assert!(app.config_close_guard_armed());
    assert_eq!(
        app.config_footer_hint(),
        "status: confirm close - Esc discards"
    );
    assert_eq!(
        app.last_notice(),
        Some("unsaved changes; Down footer to save, Esc discard")
    );

    let second = app.handle_key_event(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE))?;
    assert!(second.is_none());
    assert!(!app.is_config_mode());
    assert_eq!(app.last_notice(), Some("closed config; discarded changes"));
    Ok(())
}

#[test]
fn config_f2_saves_and_keeps_config_open() -> Result<()> {
    let temp = tempdir()?;
    let config_path = temp.path().join("sigil.toml");
    test_config().save(&config_path)?;

    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.config_path = config_path.clone();
    app.open_config_panel();
    let state = app
        .config_state
        .as_mut()
        .expect("config state should exist after opening /config");
    state.draft.provider_api_key = "saved-from-f2".to_owned();
    state.dirty = true;

    let action = app.handle_key_event(KeyEvent::new(KeyCode::F(2), KeyModifiers::NONE))?;

    let Some(AppAction::ConfigSaved { root_config }) = action else {
        panic!("expected config save action");
    };
    assert!(app.is_config_mode());
    assert_eq!(app.last_notice(), Some("saved config"));
    assert_eq!(
        root_config
            .providers
            .get("deepseek")
            .and_then(|value| value.get("api_key"))
            .and_then(|value| value.as_str()),
        Some("saved-from-f2")
    );

    let saved = RootConfig::load(&config_path)?;
    assert_eq!(
        saved
            .providers
            .get("deepseek")
            .and_then(|value| value.get("api_key"))
            .and_then(|value| value.as_str()),
        Some("saved-from-f2")
    );
    Ok(())
}

#[test]
fn config_footer_enter_saves_without_function_keys() -> Result<()> {
    let temp = tempdir()?;
    let config_path = temp.path().join("sigil.toml");
    test_config().save(&config_path)?;

    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.config_path = config_path.clone();
    app.open_config_panel();
    let state = app
        .config_state
        .as_mut()
        .expect("config state should exist after opening /config");
    state.selected_field = Some(ConfigField::ProviderName);
    state.draft.provider_api_key = "saved-from-footer".to_owned();
    state.dirty = true;

    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE))?;
    let action = app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))?;

    let Some(AppAction::ConfigSaved { root_config }) = action else {
        panic!("expected config save action");
    };
    assert!(app.is_config_mode());
    assert_eq!(app.last_notice(), Some("saved config"));
    assert_eq!(
        root_config
            .providers
            .get("deepseek")
            .and_then(|value| value.get("api_key"))
            .and_then(|value| value.as_str()),
        Some("saved-from-footer")
    );

    let saved = RootConfig::load(&config_path)?;
    assert_eq!(
        saved
            .providers
            .get("deepseek")
            .and_then(|value| value.get("api_key"))
            .and_then(|value| value.as_str()),
        Some("saved-from-footer")
    );
    Ok(())
}

#[test]
fn config_f3_saves_and_closes_without_switching_step() -> Result<()> {
    let temp = tempdir()?;
    let config_path = temp.path().join("sigil.toml");
    test_config().save(&config_path)?;

    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.config_path = config_path.clone();
    app.open_config_panel();
    let state = app
        .config_state
        .as_mut()
        .expect("config state should exist after opening /config");
    state.draft.provider_api_key = "saved-from-f3".to_owned();
    state.dirty = true;
    state.set_section(ConfigSection::Provider);
    state.selected_field = Some(ConfigField::ProviderApiKey);

    let action = app.handle_key_event(KeyEvent::new(KeyCode::F(3), KeyModifiers::NONE))?;

    let Some(AppAction::ConfigSaved { root_config }) = action else {
        panic!("expected config save action");
    };
    assert!(!app.is_config_mode());
    assert_eq!(app.last_notice(), Some("saved config and closed"));
    assert_eq!(
        root_config
            .providers
            .get("deepseek")
            .and_then(|value| value.get("api_key"))
            .and_then(|value| value.as_str()),
        Some("saved-from-f3")
    );
    Ok(())
}

#[test]
fn config_footer_save_and_close_works_without_function_keys() -> Result<()> {
    let temp = tempdir()?;
    let config_path = temp.path().join("sigil.toml");
    test_config().save(&config_path)?;

    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.config_path = config_path.clone();
    app.open_config_panel();
    let state = app
        .config_state
        .as_mut()
        .expect("config state should exist after opening /config");
    state.selected_field = Some(ConfigField::ProviderName);
    state.draft.provider_api_key = "saved-from-footer-close".to_owned();
    state.dirty = true;

    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE))?;
    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE))?;
    let action = app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))?;

    let Some(AppAction::ConfigSaved { root_config }) = action else {
        panic!("expected config save action");
    };
    assert!(!app.is_config_mode());
    assert_eq!(app.last_notice(), Some("saved config and closed"));
    assert_eq!(
        root_config
            .providers
            .get("deepseek")
            .and_then(|value| value.get("api_key"))
            .and_then(|value| value.as_str()),
        Some("saved-from-footer-close")
    );

    let saved = RootConfig::load(&config_path)?;
    assert_eq!(
        saved
            .providers
            .get("deepseek")
            .and_then(|value| value.get("api_key"))
            .and_then(|value| value.as_str()),
        Some("saved-from-footer-close")
    );
    Ok(())
}

#[test]
fn setup_mode_saves_config_and_returns_runtime_boot_action() -> Result<()> {
    let temp = tempdir()?;
    let config_path = temp.path().join("config").join("sigil.toml");
    let workspace_root = temp.path().join("workspace");
    let mut app = AppState::from_setup(config_path.clone(), workspace_root.clone(), None);

    assert!(app.is_setup_mode());

    let action = app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))?;
    assert!(action.is_none());
    app.setup_state
        .as_mut()
        .expect("setup state should exist in setup mode")
        .selected_field = SetupField::Model;
    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))?;
    assert!(app.has_modal());
    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE))?;
    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))?;
    app.setup_state
        .as_mut()
        .expect("setup state should exist in setup mode")
        .selected_field = SetupField::ApiKey;
    for character in "test-inline-key".chars() {
        let _ =
            app.handle_key_event(KeyEvent::new(KeyCode::Char(character), KeyModifiers::NONE))?;
    }
    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))?;
    app.setup_state
        .as_mut()
        .expect("setup state should exist in setup mode")
        .selected_field = SetupField::Save;
    let action = app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))?;

    let Some(AppAction::SetupCompleted {
        config_path: saved_path,
        root_config,
    }) = action
    else {
        panic!("expected setup completion action");
    };
    assert_eq!(saved_path, config_path);
    assert_eq!(root_config.workspace.root, ".");
    assert_eq!(root_config.agent.model, "deepseek-v4-pro");
    let saved = RootConfig::load(&saved_path)?;
    assert_eq!(saved.agent.provider, "deepseek");
    assert_eq!(saved.agent.model, "deepseek-v4-pro");
    assert_eq!(
        saved
            .providers
            .get("deepseek")
            .and_then(|value| value.get("api_key"))
            .and_then(|value| value.as_str()),
        Some("test-inline-key")
    );
    assert_eq!(
        root_config
            .providers
            .get("deepseek")
            .and_then(|value| value.get("api_key"))
            .and_then(|value| value.as_str()),
        Some("test-inline-key")
    );
    assert!(saved.memory.enabled);
    assert!(saved.compaction.enabled);
    assert_eq!(saved.compaction.context_window_tokens, None);
    let saved_raw = std::fs::read_to_string(&saved_path)?;
    assert!(!saved_raw.contains("fallback_context_window_tokens"));
    assert!(
        !saved_raw
            .lines()
            .any(|line| line.trim_start().starts_with("context_window_tokens ="))
    );
    Ok(())
}

#[test]
fn setup_save_requires_credentials() -> Result<()> {
    let temp = tempdir()?;
    let config_path = temp.path().join("config").join("sigil.toml");
    let workspace_root = temp.path().join("workspace");
    let mut app = AppState::from_setup(config_path, workspace_root, None);
    let state = app
        .setup_state
        .as_mut()
        .expect("setup state should exist in setup mode");
    state.selected_field = SetupField::Save;
    state.trusted_current_folder = true;
    state.api_key.clear();

    let action = app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))?;

    assert!(action.is_none());
    let state = app
        .setup_state
        .as_ref()
        .expect("setup state should exist in setup mode");
    assert_eq!(state.selected_field, SetupField::Save);
    assert_eq!(
        app.last_notice(),
        Some("provide api_key or export SIGIL_API_KEY")
    );
    Ok(())
}

#[test]
fn config_compaction_details_render_provider_fallback_and_unknown_windows() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.open_config_panel();
    let state = app
        .config_state
        .as_mut()
        .expect("config state should exist after opening /config");
    state.set_section(ConfigSection::Compaction);

    let provider_lines = app.config_detail_lines().join("\n");
    assert!(provider_lines.contains("1,000,000 tokens  source=provider"));

    let state = app
        .config_state
        .as_mut()
        .expect("config state should still exist");
    state.draft.provider_model = "custom-model".to_owned();
    state.draft.compaction_context_window_tokens = "2048".to_owned();

    let fallback_lines = app.config_detail_lines().join("\n");
    assert!(fallback_lines.contains("2,048 tokens  source=fallback"));

    let state = app
        .config_state
        .as_mut()
        .expect("config state should still exist");
    state.draft.compaction_context_window_tokens = "0".to_owned();

    let unknown_lines = app.config_detail_lines().join("\n");
    assert!(unknown_lines.contains("unknown  source=none"));
    Ok(())
}

#[test]
fn config_permission_and_mcp_details_render_rule_and_pin_summaries() -> Result<()> {
    let mut config = test_config();
    config.permission.rules = vec![
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
            tool_name: Some("grep".to_owned()),
            subject_glob: Some("**/*.rs".to_owned()),
            mode: ApprovalMode::Allow,
        },
        sigil_kernel::PermissionRule {
            tool_name: Some("glob".to_owned()),
            subject_glob: Some("**/*.md".to_owned()),
            mode: ApprovalMode::Ask,
        },
    ];
    config.mcp_servers = vec![
        sigil_kernel::McpServerConfig {
            name: "off".to_owned(),
            command: "mcp-off".to_owned(),
            trust: sigil_kernel::McpServerTrustPolicy {
                pin_version: false,
                allow_secrets: false,
                ..Default::default()
            },
            ..Default::default()
        },
        sigil_kernel::McpServerConfig {
            name: "pinned".to_owned(),
            command: "mcp-pinned".to_owned(),
            trust: sigil_kernel::McpServerTrustPolicy {
                pin_version: true,
                pinned: Some(sigil_kernel::McpServerPinnedIdentity {
                    command_fingerprint: "sha256:abc".to_owned(),
                    protocol_version: "2024-11-05".to_owned(),
                    server_name: "pinned".to_owned(),
                    server_version: "1.0.0".to_owned(),
                }),
                allow_secrets: true,
                ..Default::default()
            },
            ..Default::default()
        },
        sigil_kernel::McpServerConfig {
            name: "missing".to_owned(),
            command: "mcp-missing".to_owned(),
            trust: sigil_kernel::McpServerTrustPolicy {
                pin_version: true,
                pinned: None,
                allow_secrets: false,
                ..Default::default()
            },
            ..Default::default()
        },
    ];

    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &config);
    app.open_config_panel();
    app.config_state
        .as_mut()
        .expect("config state should still exist")
        .set_section(ConfigSection::Permissions);
    let permission_lines = app.config_detail_lines().join("\n");
    assert!(permission_lines.contains("Rule overrides"));
    assert!(permission_lines.contains("5 configured"));
    assert!(permission_lines.contains("... 1 more rules in config file"));

    let state = app
        .config_state
        .as_mut()
        .expect("config state should still exist");
    state.set_section(ConfigSection::Mcp);
    state.selected_mcp_server_index = 0;
    assert!(app.config_detail_lines().join("\n").contains("Pin: off"));

    let state = app
        .config_state
        .as_mut()
        .expect("config state should still exist");
    state.selected_mcp_server_index = 1;
    let pinned_lines = app.config_detail_lines().join("\n");
    assert!(pinned_lines.contains("Pin: pinned"));
    assert!(pinned_lines.contains("Secrets: allowed"));

    let state = app
        .config_state
        .as_mut()
        .expect("config state should still exist");
    state.selected_mcp_server_index = 2;
    let missing_lines = app.config_detail_lines().join("\n");
    assert!(missing_lines.contains("Pin: missing"));
    assert!(missing_lines.contains("Secrets: blocked"));
    Ok(())
}

#[test]
fn config_ctrl_c_quits_from_panel() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.open_config_panel();

    let action = app.handle_key_event(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL))?;

    assert!(action.is_none());
    assert!(app.should_quit);
    assert!(!app.is_config_mode());
    Ok(())
}

#[test]
fn config_ctrl_shortcuts_and_page_navigation_cover_edge_branches() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.open_config_panel();

    let action = app.handle_key_event(KeyEvent::new(KeyCode::Char('n'), KeyModifiers::CONTROL))?;
    assert!(action.is_none());
    assert_eq!(app.last_notice(), Some("Ctrl-N: MCP only"));

    let action = app.handle_key_event(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL))?;
    assert!(action.is_none());
    assert_eq!(app.last_notice(), Some("Ctrl-D: MCP only"));

    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE))?;
    assert_eq!(app.config_section_title(), Some("Permissions"));

    let _ = app.handle_key_event(KeyEvent::new(KeyCode::BackTab, KeyModifiers::SHIFT))?;
    assert_eq!(app.config_section_title(), Some("Provider"));

    let mut config = test_config();
    config.mcp_servers.push(sigil_kernel::McpServerConfig {
        name: "filesystem".to_owned(),
        command: "mcp-filesystem".to_owned(),
        ..Default::default()
    });
    config.mcp_servers.push(sigil_kernel::McpServerConfig {
        name: "git".to_owned(),
        command: "mcp-git".to_owned(),
        ..Default::default()
    });
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &config);
    app.open_config_panel();
    app.config_state
        .as_mut()
        .expect("config state should exist")
        .set_section(ConfigSection::Mcp);

    let _ = app.handle_key_event(KeyEvent::new(KeyCode::PageDown, KeyModifiers::NONE))?;
    assert_eq!(
        app.config_state
            .as_ref()
            .expect("config state should exist")
            .selected_mcp_server_index,
        1
    );
    assert_eq!(app.last_notice(), Some("mcp server 2/2"));

    let _ = app.handle_key_event(KeyEvent::new(KeyCode::PageUp, KeyModifiers::NONE))?;
    assert_eq!(
        app.config_state
            .as_ref()
            .expect("config state should exist")
            .selected_mcp_server_index,
        0
    );
    assert_eq!(app.last_notice(), Some("mcp server 1/2"));

    let mut empty_app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    empty_app.open_config_panel();
    empty_app
        .config_state
        .as_mut()
        .expect("config state should exist")
        .set_section(ConfigSection::Mcp);

    let _ = empty_app.handle_key_event(KeyEvent::new(KeyCode::PageDown, KeyModifiers::NONE))?;
    assert_eq!(empty_app.last_notice(), Some("no MCP server to select"));
    Ok(())
}

#[test]
fn config_enter_toggles_fields_and_opens_additional_modals() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.open_config_panel();

    {
        let state = app
            .config_state
            .as_mut()
            .expect("config state should exist");
        state.set_section(ConfigSection::Permissions);
        state.selected_field = Some(ConfigField::PermissionsDefaultMode);
    }
    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))?;
    let state = app
        .config_state
        .as_ref()
        .expect("config state should exist");
    assert_eq!(state.draft.permission_default_mode, ApprovalMode::Deny);
    assert!(state.dirty);

    {
        let state = app
            .config_state
            .as_mut()
            .expect("config state should exist");
        state.set_section(ConfigSection::Memory);
        state.selected_field = Some(ConfigField::MemoryEnabled);
    }
    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))?;
    assert!(
        !app.config_state
            .as_ref()
            .expect("config state should exist")
            .draft
            .memory_enabled
    );

    {
        let state = app
            .config_state
            .as_mut()
            .expect("config state should exist");
        state.set_section(ConfigSection::Compaction);
        state.selected_field = Some(ConfigField::CompactionEnabled);
    }
    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))?;
    assert!(
        !app.config_state
            .as_ref()
            .expect("config state should exist")
            .draft
            .compaction_enabled
    );

    {
        let state = app
            .config_state
            .as_mut()
            .expect("config state should exist");
        state.set_section(ConfigSection::CodeIntelligence);
        state.selected_field = Some(ConfigField::CodeIntelEnabled);
    }
    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))?;
    assert!(
        app.config_state
            .as_ref()
            .expect("config state should exist")
            .draft
            .code_intelligence_enabled
    );

    app.config_state
        .as_mut()
        .expect("config state should exist")
        .selected_field = Some(ConfigField::CodeIntelStartup);
    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))?;
    assert_eq!(
        app.config_state
            .as_ref()
            .expect("config state should exist")
            .draft
            .code_intelligence_startup,
        sigil_kernel::CodeIntelStartup::Eager
    );

    app.config_state
        .as_mut()
        .expect("config state should exist")
        .selected_field = Some(ConfigField::CodeIntelDiscoveryEnabled);
    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))?;
    assert!(
        !app.config_state
            .as_ref()
            .expect("config state should exist")
            .draft
            .code_intelligence_discovery_enabled
    );

    app.config_state
        .as_mut()
        .expect("config state should exist")
        .selected_field = Some(ConfigField::CodeIntelDiscoveryReportMissing);
    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))?;
    assert!(
        !app.config_state
            .as_ref()
            .expect("config state should exist")
            .draft
            .code_intelligence_discovery_report_missing
    );

    {
        let state = app
            .config_state
            .as_mut()
            .expect("config state should exist");
        state.set_section(ConfigSection::Terminal);
        state.selected_field = Some(ConfigField::TerminalMouseCapture);
    }
    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))?;
    assert!(
        !app.config_state
            .as_ref()
            .expect("config state should exist")
            .draft
            .terminal_mouse_capture
    );

    app.config_state
        .as_mut()
        .expect("config state should exist")
        .selected_field = Some(ConfigField::TerminalOsc52Clipboard);
    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))?;
    assert!(
        !app.config_state
            .as_ref()
            .expect("config state should exist")
            .draft
            .terminal_osc52_clipboard
    );

    {
        let state = app
            .config_state
            .as_mut()
            .expect("config state should exist");
        state.set_section(ConfigSection::Provider);
        state.selected_field = Some(ConfigField::ProviderFimModel);
    }
    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))?;
    assert_eq!(app.modal_title(), Some("FIM Model"));
    app.modal_state = None;

    app.config_state
        .as_mut()
        .expect("config state should exist")
        .selected_field = Some(ConfigField::ProviderApiKey);
    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE))?;
    assert_eq!(app.modal_title(), Some("API Key"));
    let ModalState::SecretInput(state) =
        app.modal_state.as_ref().expect("secret modal should open")
    else {
        panic!("expected secret input modal");
    };
    assert_eq!(state.buffer, "x");
    app.modal_state = None;

    app.config_state
        .as_mut()
        .expect("config state should exist")
        .selected_field = Some(ConfigField::ProviderModel);
    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Char('z'), KeyModifiers::NONE))?;
    assert_eq!(app.modal_title(), Some("Model"));
    let ModalState::TextInput(state) = app.modal_state.as_ref().expect("text modal should open")
    else {
        panic!("expected text input modal");
    };
    assert_eq!(state.buffer, "z");
    app.modal_state = None;

    app.config_state
        .as_mut()
        .expect("config state should exist")
        .selected_field = Some(ConfigField::CompactionTailMessages);
    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE))?;
    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Char('5'), KeyModifiers::NONE))?;
    let ModalState::TextInput(state) = app.modal_state.as_ref().expect("text modal should open")
    else {
        panic!("expected text input modal");
    };
    assert_eq!(state.buffer, "5");
    Ok(())
}

#[test]
fn config_modal_f3_saves_and_closes_from_text_input() -> Result<()> {
    let temp = tempdir()?;
    let config_path = temp.path().join("sigil.toml");
    test_config().save(&config_path)?;

    let mut app = AppState::from_root_config(&config_path, &test_config());
    app.open_config_panel();
    app.config_state
        .as_mut()
        .expect("config state should exist")
        .selected_field = Some(ConfigField::ProviderBaseUrl);

    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))?;
    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE))?;

    let action = app.handle_key_event(KeyEvent::new(KeyCode::F(3), KeyModifiers::NONE))?;

    assert!(matches!(action, Some(AppAction::ConfigSaved { .. })));
    assert!(!app.is_config_mode());
    assert_eq!(app.last_notice(), Some("saved config and closed"));
    Ok(())
}

#[test]
fn config_command_is_unavailable_in_setup_mode() -> Result<()> {
    let mut app = AppState::from_setup(
        Path::new("sigil.toml").to_path_buf(),
        Path::new(".").to_path_buf(),
        None,
    );

    app.input = "/config".to_owned();
    let action = app.submit_input()?;

    assert!(action.is_none());
    assert!(!app.is_config_mode());
    assert_eq!(
        app.last_notice(),
        Some("config is unavailable in setup mode")
    );
    Ok(())
}

#[test]
fn config_mcp_paging_without_servers_reports_empty_state() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.open_config_panel();
    app.config_state
        .as_mut()
        .expect("config state should still exist")
        .set_section(ConfigSection::Mcp);

    let _ = app.handle_key_event(KeyEvent::new(KeyCode::PageUp, KeyModifiers::NONE))?;
    assert_eq!(app.last_notice(), Some("no MCP server to select"));

    let _ = app.handle_key_event(KeyEvent::new(KeyCode::PageDown, KeyModifiers::NONE))?;
    assert_eq!(app.last_notice(), Some("no MCP server to select"));
    Ok(())
}

#[test]
fn config_mcp_shortcuts_outside_mcp_section_show_guidance() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.open_config_panel();
    assert_eq!(app.config_section_title(), Some("Provider"));

    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Char('n'), KeyModifiers::CONTROL))?;
    assert_eq!(app.last_notice(), Some("Ctrl-N: MCP only"));

    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL))?;
    assert_eq!(app.last_notice(), Some("Ctrl-D: MCP only"));
    Ok(())
}

#[test]
fn config_remaining_edge_branches_cover_footer_guards_and_mcp_empty_paths() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.open_config_panel();

    app.config_state
        .as_mut()
        .expect("config state should exist")
        .set_section(ConfigSection::Mcp);
    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL))?;
    assert_eq!(app.last_notice(), Some("no MCP server"));

    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Char('n'), KeyModifiers::CONTROL))?;
    assert_eq!(app.last_notice(), Some("added MCP server"));
    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL))?;
    assert_eq!(app.last_notice(), Some("removed MCP server"));

    {
        let state = app
            .config_state
            .as_mut()
            .expect("config state should exist");
        state.set_section(ConfigSection::Provider);
        state.focus_footer(ConfigFooterAction::SaveAndClose);
    }
    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Left, KeyModifiers::NONE))?;
    assert_eq!(app.config_selected_field_label(), Some("save"));
    assert_eq!(app.last_notice(), Some("action save"));

    {
        let state = app
            .config_state
            .as_mut()
            .expect("config state should exist");
        state.set_section(ConfigSection::Provider);
        state.focus_footer(ConfigFooterAction::ActivateMcp);
    }
    let action = app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))?;
    assert!(action.is_none());
    assert_eq!(
        app.last_notice(),
        Some("activate MCP is available in MCP config")
    );

    {
        let state = app
            .config_state
            .as_mut()
            .expect("config state should exist");
        state.set_section(ConfigSection::Provider);
        state.selected_field = Some(ConfigField::ProviderName);
    }
    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE))?;
    assert_eq!(app.config_selected_field_label(), Some("save"));
    let before_notice = app.last_notice().map(ToOwned::to_owned);
    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE))?;
    assert_eq!(app.last_notice(), before_notice.as_deref());

    app.config_state = None;
    assert!(
        app.handle_config_key_event(KeyEvent::new(KeyCode::Char('n'), KeyModifiers::CONTROL))?
            .is_none()
    );
    assert!(
        app.handle_config_key_event(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL))?
            .is_none()
    );
    assert!(
        app.handle_config_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))?
            .is_none()
    );
    Ok(())
}

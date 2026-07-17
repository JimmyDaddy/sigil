use super::*;
use crate::app::MutationArtifactRetentionPreview;

fn config_for_workspace(workspace_root: &Path) -> RootConfig {
    let mut config = test_config();
    config.workspace.root = workspace_root.display().to_string();
    config
}

#[test]
fn config_storage_section_shows_resolved_paths_readonly() {
    let temp = tempfile::tempdir().expect("tempdir should create");
    let mut config = config_for_workspace(temp.path());
    config.storage.state_root =
        sigil_kernel::StorageRoot::Path(temp.path().join("state").display().to_string());
    config.storage.cache_root =
        sigil_kernel::StorageRoot::Path(temp.path().join("cache").display().to_string());
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &config);
    app.open_config_panel();
    app.config_state
        .as_mut()
        .expect("config state should exist")
        .set_section(ConfigSection::Storage);

    let detail = app.config_detail_lines().join("\n");

    assert!(detail.contains("Storage"));
    assert!(detail.contains("[roots]"));
    assert!(detail.contains("State root"));
    assert!(detail.contains("Cache root"));
    assert!(detail.contains("Workspace state"));
    assert!(detail.contains("Project assets"));
    assert!(detail.contains("Workspace skills"));
    assert!(detail.contains("Workspace commands"));
    assert!(detail.contains("Workspace agents"));
    assert!(detail.contains("Workspace plugins"));
    assert!(detail.contains("[files]"));
    assert!(detail.contains("Session logs"));
    assert!(detail.contains("Input history"));
    assert!(detail.contains("Scratch"));
    assert!(detail.contains("[artifact retention]"));
    assert!(detail.contains("Max artifacts: 10000"));
    assert!(detail.contains("Max bytes: 512 MiB"));
    assert!(detail.contains("Expire older than: 30 days"));
    assert!(detail.contains("Current artifacts: 0 (0 bytes)"));
    assert!(detail.contains("Cleanup preview: expire 0, delete 0, unavailable 0"));
    assert!(detail.contains("Cleanup bytes: expire 0 bytes, delete 0 bytes"));
    assert!(!detail.contains("Maintenance: clean recommended"));
    assert!(detail.contains("i No mutation artifacts found"));
    assert!(
        detail
            .contains("i footer clean records lifecycle events; artifact details are audit/debug")
    );
    assert!(
        detail.contains(
            "i read-only; state/cache roots can be overridden, project assets are fixed under workspace .sigil"
        )
    );
    assert_eq!(app.config_selected_field_label(), None);
}

#[test]
fn config_storage_footer_dispatches_mutation_artifact_cleanup() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.open_config_panel();
    let state = app
        .config_state
        .as_mut()
        .expect("config state should still exist");
    state.set_section(ConfigSection::Storage);
    state.focus_footer(ConfigFooterAction::CleanMutationArtifacts);

    assert_eq!(
        app.config_footer_action_labels(),
        vec!["clean", "sessions", "save+close", "close"]
    );
    assert_eq!(app.config_selected_field_label(), Some("clean_artifacts"));
    assert!(
        app.config_nav_lines()
            .join("\n")
            .contains("Storage: footer clean artifacts")
    );

    let action = app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))?;

    assert!(matches!(
        action,
        Some(AppAction::CleanMutationArtifacts {
            target: sigil_kernel::MutationArtifactCleanupTarget::Recommended,
        })
    ));
    assert_eq!(
        app.last_notice(),
        Some("cleaning recommended mutation artifacts")
    );
    Ok(())
}

#[test]
fn config_storage_footer_keeps_artifact_delete_out_of_primary_actions() -> Result<()> {
    let temp = tempfile::tempdir().expect("tempdir should create");
    let workspace = temp.path().join("workspace");
    std::fs::create_dir(&workspace)?;
    let target = workspace.join("note.txt");
    std::fs::write(&target, "old")?;
    let config = config_for_workspace(&workspace);
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &config);
    let store = JsonlSessionStore::new(app.session_log_path.clone())?;
    let recorder = sigil_kernel::MutationEventRecorder::new(store);
    let coordinator = recorder.coordinator(&workspace, "tool-call-delete", None)?;
    let new_content = b"new";
    let prepared = coordinator.prepare_file(
        "note.txt",
        target.clone(),
        Some(sigil_kernel::bytes_hash(new_content)),
    )?;
    coordinator.commit_write(&prepared, new_content)?;

    app.open_config_panel();
    let state = app
        .config_state
        .as_mut()
        .expect("config state should still exist");
    state.set_section(ConfigSection::Storage);

    let detail = app.config_detail_lines().join("\n");
    assert!(detail.contains("> note.txt · 3 bytes · available"));
    assert!(
        detail
            .contains("i footer clean records lifecycle events; artifact details are audit/debug")
    );
    assert_eq!(
        app.config_footer_action_labels(),
        vec!["clean", "sessions", "save+close", "close"]
    );
    assert!(!app.config_footer_action_labels().contains(&"delete"));
    Ok(())
}

#[test]
fn config_storage_section_shows_artifact_retention_overrides() {
    let temp = tempfile::tempdir().expect("tempdir should create");
    let mut config = config_for_workspace(temp.path());
    config.storage.mutation_artifact_retention.max_artifacts = Some(42);
    config.storage.mutation_artifact_retention.max_bytes = Some(1024 * 1024);
    config
        .storage
        .mutation_artifact_retention
        .expire_older_than_ms = Some(60 * 60 * 1000);
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &config);
    app.open_config_panel();
    app.config_state
        .as_mut()
        .expect("config state should exist")
        .set_section(ConfigSection::Storage);

    let detail = app.config_detail_lines().join("\n");

    assert!(detail.contains("Max artifacts: 42"));
    assert!(detail.contains("Max bytes: 1 MiB"));
    assert!(detail.contains("Expire older than: 1 hour"));

    app.runtime.mutation_artifact_retention_preview = MutationArtifactRetentionPreview::Pending;
    let pending = app.config_detail_lines().join("\n");
    assert!(pending.contains("Preview: pending"));

    app.runtime.mutation_artifact_retention_preview =
        MutationArtifactRetentionPreview::Unavailable("preview failed".to_owned());
    let unavailable = app.config_detail_lines().join("\n");
    assert!(unavailable.contains("Preview: unavailable"));
    assert!(unavailable.contains("i preview failed"));

    let action = app
        .handle_key_event(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE))
        .expect("storage down should be handled");
    assert!(action.is_none());
    assert_eq!(app.last_notice(), Some("action clean_artifacts"));
}

#[test]
fn config_storage_clean_keeps_target_selection_out_of_primary_flow() -> Result<()> {
    let temp = tempfile::tempdir().expect("tempdir should create");
    let workspace = temp.path().join("workspace");
    std::fs::create_dir(&workspace)?;
    let target = workspace.join("note.txt");
    std::fs::write(&target, "old")?;
    let config = config_for_workspace(&workspace);
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &config);
    let store = JsonlSessionStore::new(app.session_log_path.clone())?;
    let recorder = sigil_kernel::MutationEventRecorder::new(store);
    let coordinator = recorder.coordinator(&workspace, "tool-call-target", None)?;
    let new_content = b"new";
    let prepared = coordinator.prepare_file(
        "note.txt",
        target.clone(),
        Some(sigil_kernel::bytes_hash(new_content)),
    )?;
    coordinator.commit_write(&prepared, new_content)?;

    app.open_config_panel();
    app.config_state
        .as_mut()
        .expect("config state should exist")
        .set_section(ConfigSection::Storage);

    let detail = app.config_detail_lines().join("\n");
    assert!(detail.contains("Cleanup preview: expire 0, delete 0, unavailable 0"));
    assert!(!detail.contains("Cleanup target"));
    assert_eq!(app.config_selected_field_label(), None);
    app.config_state
        .as_mut()
        .expect("config state should exist")
        .focus_footer(ConfigFooterAction::CleanMutationArtifacts);

    let action = app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))?;

    assert!(matches!(
        action,
        Some(AppAction::CleanMutationArtifacts {
            target: sigil_kernel::MutationArtifactCleanupTarget::Recommended,
        })
    ));
    assert_eq!(
        app.last_notice(),
        Some("cleaning recommended mutation artifacts")
    );
    Ok(())
}

#[test]
fn config_storage_section_shows_artifact_retention_preview() -> Result<()> {
    let temp = tempfile::tempdir().expect("tempdir should create");
    let workspace = temp.path().join("workspace");
    std::fs::create_dir(&workspace)?;
    let target = workspace.join("note.txt");
    std::fs::write(&target, "old")?;
    let mut config = config_for_workspace(&workspace);
    config.storage.mutation_artifact_retention.max_artifacts = Some(0);
    config.storage.mutation_artifact_retention.max_bytes = None;
    config
        .storage
        .mutation_artifact_retention
        .expire_older_than_ms = None;
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &config);
    let store = JsonlSessionStore::new(app.session_log_path.clone())?;
    let recorder = sigil_kernel::MutationEventRecorder::new(store);
    let coordinator = recorder.coordinator(&workspace, "tool-call-preview", None)?;
    let new_content = b"new";
    let prepared = coordinator.prepare_file(
        "note.txt",
        target.clone(),
        Some(sigil_kernel::bytes_hash(new_content)),
    )?;
    coordinator.commit_write(&prepared, new_content)?;

    app.open_config_panel();
    app.config_state
        .as_mut()
        .expect("config state should exist")
        .set_section(ConfigSection::Storage);

    let detail = app.config_detail_lines().join("\n");

    assert!(detail.contains("Current artifacts: 1 (3 bytes)"));
    assert!(detail.contains("Cleanup preview: expire 1, delete 0, unavailable 0"));
    assert!(detail.contains("Maintenance: clean recommended (1 artifacts, 3 bytes)"));
    assert!(detail.contains("Cleanup bytes: expire 3 bytes, delete 0 bytes"));
    assert!(detail.contains("[artifact list]"));
    assert!(detail.contains("> note.txt · 3 bytes · available"));
    assert!(detail.contains("[selected artifact]"));
    assert!(detail.contains("Selected: 1 of 1"));
    assert!(detail.contains("Size: 3 bytes"));
    assert!(detail.contains("Availability: available"));
    assert!(detail.contains("Restore impact: snapshot content available"));
    assert!(detail.contains("Source 1: note.txt"));
    Ok(())
}

#[test]
fn config_storage_up_down_moves_artifact_selection() -> Result<()> {
    let temp = tempfile::tempdir().expect("tempdir should create");
    let workspace = temp.path().join("workspace");
    std::fs::create_dir(&workspace)?;
    let first = workspace.join("first.txt");
    let second = workspace.join("second.txt");
    std::fs::write(&first, "one")?;
    std::fs::write(&second, "two")?;
    let config = config_for_workspace(&workspace);
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &config);
    let store = JsonlSessionStore::new(app.session_log_path.clone())?;
    let recorder = sigil_kernel::MutationEventRecorder::new(store);
    let coordinator = recorder.coordinator(&workspace, "tool-call-select", None)?;
    let new_content = b"new";
    let first_prepared = coordinator.prepare_file(
        "first.txt",
        first.clone(),
        Some(sigil_kernel::bytes_hash(new_content)),
    )?;
    coordinator.commit_write(&first_prepared, new_content)?;
    std::thread::sleep(std::time::Duration::from_millis(2));
    let second_prepared = coordinator.prepare_file(
        "second.txt",
        second.clone(),
        Some(sigil_kernel::bytes_hash(new_content)),
    )?;
    coordinator.commit_write(&second_prepared, new_content)?;

    app.open_config_panel();
    app.config_state
        .as_mut()
        .expect("config state should still exist")
        .set_section(ConfigSection::Storage);

    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE))?;
    assert_eq!(
        app.config_state
            .as_ref()
            .expect("config state should exist")
            .selected_storage_artifact_index,
        0
    );

    let action = app.handle_key_event(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE))?;

    assert!(action.is_none());
    assert_eq!(app.last_notice(), Some("artifact 2/2"));
    let detail = app.config_detail_lines().join("\n");
    assert!(detail.contains("- first.txt · 3 bytes · available"));
    assert!(detail.contains("> second.txt · 3 bytes · available"));
    assert!(detail.contains("Selected: 2 of 2"));
    assert!(detail.contains("Source 1: second.txt"));

    let action = app.handle_key_event(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE))?;

    assert!(action.is_none());
    assert_eq!(app.last_notice(), Some("action clean_artifacts"));
    Ok(())
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
transport = "stdio"
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
    app.composer.input = "/config".to_owned();

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

    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE))?;
    assert_eq!(app.config_selected_field_label(), Some("save"));

    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE))?;
    assert_eq!(app.config_selected_field_label(), Some("Provider"));
    Ok(())
}

#[test]
fn config_empty_mcp_reports_no_selection_and_preserves_explicit_footer_navigation() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.open_config_panel();
    app.config_state
        .as_mut()
        .expect("config state should still exist")
        .set_section(ConfigSection::Mcp);
    assert_eq!(app.config_selected_field_label(), None);

    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE))?;
    assert_eq!(app.config_selected_field_label(), Some("activate_mcp"));
    assert_eq!(app.last_notice(), Some("action activate_mcp"));
    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE))?;
    assert_eq!(app.config_selected_field_label(), Some("save_and_close"));
    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE))?;
    assert_eq!(app.config_selected_field_label(), Some("close"));
    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE))?;
    assert_eq!(app.config_selected_field_label(), None);
    let action = app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))?;
    assert!(action.is_none());
    assert_eq!(app.last_notice(), Some("no MCP server selected"));

    app.config_state
        .as_mut()
        .expect("config state should still exist")
        .focus_footer(ConfigFooterAction::ActivateMcp);

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
    assert_eq!(app.config_section_title(), Some("Compaction"));
    assert_eq!(app.config_selected_field_label(), Some("Auto compact"));

    app.config_state
        .as_mut()
        .expect("config state should still exist")
        .set_section(ConfigSection::Mcp);
    app.config_state
        .as_mut()
        .expect("config state should still exist")
        .focus_footer(ConfigFooterAction::ActivateMcp);
    assert_eq!(app.config_selected_field_label(), Some("activate_mcp"));
    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Left, KeyModifiers::NONE))?;
    assert_eq!(app.config_section_title(), Some("MCP"));
    assert_eq!(app.config_selected_field_label(), Some("close"));

    app.config_state
        .as_mut()
        .expect("config state should still exist")
        .set_section(ConfigSection::Mcp);
    app.config_state
        .as_mut()
        .expect("config state should still exist")
        .focus_footer(ConfigFooterAction::ActivateMcp);
    assert_eq!(app.config_selected_field_label(), Some("activate_mcp"));
    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE))?;
    assert_eq!(app.config_selected_field_label(), Some("save_and_close"));
    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE))?;
    assert_eq!(app.config_selected_field_label(), Some("close"));
    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE))?;
    assert_eq!(app.config_section_title(), Some("Appearance"));
    assert_eq!(app.config_selected_field_label(), Some("Theme"));
    Ok(())
}

#[test]
fn config_mcp_footer_activate_returns_lazy_activation_action() -> Result<()> {
    let mut config = test_config();
    config.mcp_servers.push(mcp_server_config! {
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
        .focus_footer(ConfigFooterAction::ActivateMcp);
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
fn config_mcp_oauth_activation_opens_exclusive_authentication_modal() -> Result<()> {
    let mut config = test_config();
    config.mcp_servers.push(McpServerConfig {
        name: "remote-oauth".to_owned(),
        transport: sigil_kernel::McpServerTransportConfig::StreamableHttp(
            sigil_kernel::McpStreamableHttpConfig {
                url: "https://mcp.example/public/mcp".to_owned(),
                http_headers: Default::default(),
                env_http_headers: Default::default(),
                bearer_token_env_var: None,
                oauth: Some(sigil_kernel::config::McpOAuthConfig {
                    client_id: Some("sigil-client".to_owned()),
                    scopes: vec!["files:read".to_owned(), "files:write".to_owned()],
                }),
                client_capabilities: Default::default(),
            },
        ),
        startup: McpServerStartup::Lazy,
        ..McpServerConfig::default()
    });
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &config);
    app.open_config_panel();
    app.config_state
        .as_mut()
        .expect("config state should exist")
        .set_section(ConfigSection::Mcp);
    app.config_state
        .as_mut()
        .expect("config state should exist")
        .focus_footer(ConfigFooterAction::ActivateMcp);

    let detail = app.config_detail_lines().join("\n");
    assert!(detail.contains("sigil-client · scopes files:read files:write"));
    let action = app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))?;

    assert!(matches!(
        action,
        Some(AppAction::McpOAuth {
            ref server_name,
            action: McpOAuthUserAction::Inspect,
        }) if server_name == "remote-oauth"
    ));
    assert!(app.mcp_oauth_modal_open());
    assert!(
        app.modal_lines()
            .join("\n")
            .contains("checking system store")
    );

    let secret_action = AppAction::OpenSecretExternalUrl {
        url: SecretString::new("https://auth.example/authorize?code=secret-canary"),
    };
    assert!(!format!("{secret_action:?}").contains("secret-canary"));
    Ok(())
}

#[test]
fn config_mcp_lifecycle_updates_from_worker_activation_status() -> Result<()> {
    let mut config = test_config();
    config.mcp_servers.push(mcp_server_config! {
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
        status: McpActivationStatus::Ready {
            added_tools: 3,
            process_coverage: None,
        },
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
    let sidebar = app.mcp_sidebar_lines();
    assert_eq!(sidebar.len(), 1);
    assert!(sidebar[0].starts_with("filesystem: failed:"));
    assert!(!sidebar[0].contains("filesystem: failed: MCP server filesystem"));
    let detail = app.config_detail_lines().join("\n");
    assert!(detail.contains("failed:"));
    Ok(())
}

#[test]
fn config_mcp_footer_refreshes_saved_eager_server() -> Result<()> {
    let mut config = test_config();
    config.mcp_servers.push(mcp_server_config! {
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
    app.handle_worker_message(WorkerMessage::McpActivationStatus {
        server_name: Some("eager".to_owned()),
        status: McpActivationStatus::Ready {
            added_tools: 1,
            process_coverage: None,
        },
    })?;

    let action = app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))?;

    assert!(matches!(
        action,
        Some(AppAction::RefreshMcpServer { ref server_name }) if server_name == "eager"
    ));
    assert_eq!(app.last_notice(), Some("refreshing MCP eager"));
    assert_eq!(
        app.mcp_server_runtime_status_label("eager").as_deref(),
        Some("refreshing")
    );
    let action = app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))?;
    assert!(action.is_none());
    assert_eq!(app.last_notice(), Some("MCP eager is already refreshing"));

    let state = app
        .config_state
        .as_mut()
        .expect("config state should still exist");
    state.dirty = true;

    let action = app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))?;

    assert!(action.is_none());
    assert_eq!(app.last_notice(), Some("save config before activating MCP"));
    Ok(())
}

#[test]
fn config_mcp_footer_activate_refreshes_failed_lazy_server() -> Result<()> {
    let mut config = test_config();
    config.mcp_servers.push(mcp_server_config! {
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
        .focus_footer(ConfigFooterAction::ActivateMcp);
    app.handle_worker_message(WorkerMessage::McpActivationStatus {
        server_name: Some("filesystem".to_owned()),
        status: McpActivationStatus::Failed {
            error: "MCP server filesystem initialize timed out".to_owned(),
        },
    })?;

    let action = app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))?;

    assert!(matches!(
        action,
        Some(AppAction::RefreshMcpServer { ref server_name }) if server_name == "filesystem"
    ));
    assert_eq!(app.last_notice(), Some("refreshing MCP filesystem"));
    assert_eq!(
        app.mcp_server_runtime_status_label("filesystem").as_deref(),
        Some("refreshing")
    );
    Ok(())
}

#[test]
fn config_left_right_switches_steps() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.open_config_panel();

    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE))?;
    assert_eq!(app.config_section_title(), Some("Permissions"));
    assert_eq!(app.config_selected_field_label(), Some("Mode"));

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

    assert_eq!(lines[0], "Provider 1/13 · provider settings");
    assert_eq!(
        lines[1],
        "[provider] permissions web memory compaction mcp appearance"
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

    assert!(detail.contains("[permissions]"));
    assert!(detail.contains("Mode: manual"));
    assert!(detail.contains("Local boundary: read-only blocks local write + execute"));
    assert!(detail.contains("Network boundary: independent policy; deny/ask cannot be widened"));
    assert!(!detail.contains("Checks: manual"));
    assert!(detail.contains("[workspace]"));
    assert!(detail.contains("Workspace trust: unknown"));
    assert!(detail.contains("User checks: 0 configured"));
    assert!(detail.contains("Repo instructions: 0 files · untrusted data"));
    assert!(detail.contains("Repo checks:"));
    assert!(
        detail.contains(
            "i Task status owns run/retry actions; config only sets the long-term policy"
        )
    );
    assert!(detail.contains("[advanced]"));
    assert!(detail.contains("- Rule overrides"));
    assert!(detail.contains("i All unmatched tools use the default mode above"));
    assert!(detail.contains("Profile: auto (recommended build/cache excludes)"));
    assert!(detail.contains("Key excludes: target/**, node_modules/**"));
    assert!(detail.contains("Generated roots: none"));
    assert!(detail.contains("Advanced overrides: 0 excludes, 0 generated roots"));
    assert!(detail.contains("[details]"));
    assert!(detail.contains("selected: Mode"));
    assert!(detail.contains("key: mode"));
    assert!(detail.contains("controls: Tab section"));
    assert!(!detail.lines().any(|line| line.starts_with("overrides:")));
    assert!(!detail.contains("subject="));
    let nav = app.config_nav_lines().join("\n");
    assert!(nav.contains("Permissions: Enter cycle mode"));
    assert!(nav.contains("Permissions: task checks run from task status"));
    assert!(!nav.contains("footer approve"));
}

#[test]
fn config_permissions_footer_does_not_expose_repo_check_approval() -> Result<()> {
    let temp = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        temp.path().join("Cargo.toml"),
        "[package]\nname = \"demo\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
    )?;
    let config = config_for_workspace(temp.path());
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &config);
    app.open_config_panel();
    let state = app
        .config_state
        .as_mut()
        .expect("config state should still exist");
    state.set_section(ConfigSection::Permissions);

    assert!(
        !ConfigFooterAction::actions_for_section(ConfigSection::Permissions)
            .iter()
            .any(|action| action.field_label() == "approve_check")
    );

    Ok(())
}

#[test]
fn config_permissions_step_shows_repo_verification_trust_promotion() {
    let temp = tempfile::tempdir().expect("tempdir");
    std::fs::write(temp.path().join("AGENTS.md"), "repo instructions\n").expect("write AGENTS.md");
    std::fs::write(temp.path().join("SIGIL.md"), "sigil instructions\n").expect("write SIGIL.md");
    std::fs::write(
        temp.path().join("Cargo.toml"),
        "[package]\nname = \"demo\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
    )
    .expect("write Cargo.toml");
    let config = config_for_workspace(temp.path());
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &config);
    app.open_config_panel();
    app.config_state
        .as_mut()
        .expect("config state should still exist")
        .set_section(ConfigSection::Permissions);

    let untrusted_detail = app.config_detail_lines().join("\n");

    assert!(untrusted_detail.contains("Workspace trust: unknown"));
    assert!(untrusted_detail.contains("Repo instructions: 2 files · untrusted data"));
    assert!(untrusted_detail.contains("Repo checks: 1 found · review required"));
    assert!(
        untrusted_detail.contains(
            "i Task status owns run/retry actions; config only sets the long-term policy"
        )
    );
    assert!(!untrusted_detail.contains("> cargo-test · cargo · cargo test"));

    let workspace_id = sigil_kernel::stable_workspace_id(temp.path()).expect("workspace id");
    app.session_browser
        .current_entries
        .push(SessionLogEntry::Control(
            ControlEntry::WorkspaceTrustDecision(sigil_kernel::WorkspaceTrustDecisionEntry {
                workspace_id,
                workspace_trust_snapshot_id: "trust-1".to_owned(),
                trust: sigil_kernel::WorkspaceTrust::Trusted,
                decided_by_event_id: Some("event-trust".to_owned()),
                reason: Some("test trusted workspace".to_owned()),
            }),
        ));

    let trusted_detail = app.config_detail_lines().join("\n");

    assert!(trusted_detail.contains("Workspace trust: trusted"));
    assert!(trusted_detail.contains("Repo instructions: 2 files · trusted instructions"));
    assert!(trusted_detail.contains("Repo checks: 1 found · available to task checks"));
    assert!(!trusted_detail.contains("effect=read_only · cwd=workspace"));
}

#[test]
fn config_permissions_step_handles_empty_and_many_repo_verification_candidates() {
    let empty = tempfile::tempdir().expect("empty workspace");
    let empty_config = config_for_workspace(empty.path());
    let mut empty_app = AppState::from_root_config(Path::new("sigil.toml"), &empty_config);
    empty_app.open_config_panel();
    empty_app
        .config_state
        .as_mut()
        .expect("config state should still exist")
        .set_section(ConfigSection::Permissions);

    let empty_detail = empty_app.config_detail_lines().join("\n");
    assert!(empty_detail.contains("Repo instructions: 0 files · untrusted data"));
    assert!(empty_detail.contains("Repo checks: none found"));

    let repo = tempfile::tempdir().expect("repo workspace");
    std::fs::write(repo.path().join("SIGIL.md"), "sigil rules").expect("sigil instructions");
    std::fs::write(repo.path().join("AGENTS.md"), "agent rules").expect("agent instructions");
    std::fs::write(repo.path().join("CLAUDE.md"), "claude rules").expect("claude instructions");
    std::fs::write(repo.path().join("SIGIL.local.md"), "local rules").expect("local instructions");
    std::fs::create_dir_all(repo.path().join(".sigil")).expect("sigil dir");
    std::fs::write(
        repo.path().join(".sigil/verification.toml"),
        r#"
            [[checks]]
            id = "docs-check"
            command = "cargo"
            args = ["test", "-p", "sigil-kernel"]
        "#,
    )
    .expect("verification file");
    std::fs::create_dir_all(repo.path().join(".github/workflows")).expect("workflow dir");
    std::fs::write(
        repo.path().join(".github/workflows/ci.yml"),
        "jobs:\n  test:\n    steps:\n      - run: \"cargo test --workspace\"\n      - run: 'npm test -- --runInBand'\n      - run: make test\n",
    )
    .expect("ci file");
    std::fs::write(
        repo.path().join("package.json"),
        r#"{"scripts":{"test":"vitest","check":"tsc --noEmit","lint":"eslint .","build":"vite build"}}"#,
    )
    .expect("package file");
    std::fs::write(
        repo.path().join("Cargo.toml"),
        "[workspace]\nmembers = []\n",
    )
    .expect("cargo file");
    std::fs::write(repo.path().join("Makefile"), "test:\n\tcargo test\n").expect("makefile");
    let repo_config = config_for_workspace(repo.path());
    let mut repo_app = AppState::from_root_config(Path::new("sigil.toml"), &repo_config);
    repo_app.open_config_panel();
    repo_app
        .config_state
        .as_mut()
        .expect("config state should still exist")
        .set_section(ConfigSection::Permissions);

    let repo_detail = repo_app.config_detail_lines().join("\n");
    assert!(repo_detail.contains("Repo instructions: 4 files · untrusted data"));
    assert!(repo_detail.contains("Repo checks: 10 found · review required"));
    assert!(!repo_detail.contains("> docs-check · .sigil/verification"));
}

#[test]
fn config_permissions_step_reports_workspace_verification_discovery_error() {
    let temp = tempfile::tempdir().expect("missing workspace parent");
    let missing_workspace = temp.path().join("missing-workspace");
    let config = config_for_workspace(&missing_workspace);
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &config);
    app.open_config_panel();
    app.config_state
        .as_mut()
        .expect("config state should still exist")
        .set_section(ConfigSection::Permissions);

    let detail = app.config_detail_lines().join("\n");

    assert!(detail.contains("Workspace trust: unknown"));
    assert!(detail.contains("Verification discovery unavailable:"));
    assert!(detail.contains("failed to canonicalize"));

    let malformed = tempfile::tempdir().expect("malformed workspace");
    std::fs::create_dir_all(malformed.path().join(".sigil")).expect("sigil dir");
    std::fs::write(
        malformed.path().join(".sigil/verification.toml"),
        "checks = ",
    )
    .expect("malformed verification config");
    let malformed_config = config_for_workspace(malformed.path());
    let mut malformed_app = AppState::from_root_config(Path::new("sigil.toml"), &malformed_config);
    malformed_app.open_config_panel();
    malformed_app
        .config_state
        .as_mut()
        .expect("config state should still exist")
        .set_section(ConfigSection::Permissions);

    let malformed_detail = malformed_app.config_detail_lines().join("\n");
    assert!(malformed_detail.contains("Repo checks: unavailable"));
    assert!(malformed_detail.contains("Verification discovery failed:"));
}

#[test]
fn config_storage_preview_reports_unavailable_artifact_states() {
    let temp = tempfile::tempdir().expect("tempdir should create");
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());

    app.config_snapshot = None;
    app.refresh_mutation_artifact_retention_preview();
    assert!(matches!(
        app.runtime.mutation_artifact_retention_preview,
        MutationArtifactRetentionPreview::Unavailable(ref reason)
            if reason == "config is unavailable"
    ));

    app.config_snapshot = Some(test_config());
    let blocked_parent = temp.path().join("blocked-parent");
    std::fs::write(&blocked_parent, "not a directory").expect("blocked parent file");
    app.session_log_path = blocked_parent.join("session.jsonl");
    app.refresh_mutation_artifact_retention_preview();
    assert!(matches!(
        app.runtime.mutation_artifact_retention_preview,
        MutationArtifactRetentionPreview::Unavailable(ref reason)
            if reason.contains("failed to open mutation artifact recorder")
    ));

    let artifact_file_root = temp.path().join("artifact-file-root");
    std::fs::create_dir_all(artifact_file_root.join("artifacts")).expect("artifact parent");
    std::fs::write(
        artifact_file_root.join("artifacts/mutations"),
        "not a directory",
    )
    .expect("artifact root file");
    app.session_log_path = artifact_file_root.join("sessions/session.jsonl");
    app.refresh_mutation_artifact_retention_preview();
    assert!(matches!(
        app.runtime.mutation_artifact_retention_preview,
        MutationArtifactRetentionPreview::Unavailable(ref reason)
            if reason.contains("failed to preview mutation artifacts")
    ));
}

#[test]
fn repo_verification_candidate_promotion_flags_mutating_checks() {
    assert_eq!(
        crate::app::config_flow::repo_check_promotion_requirement(
            sigil_kernel::ToolEffect::WorkspaceWrite
        ),
        "workspace-trust/approval+rerun-readonly-check"
    );
    assert_eq!(
        crate::app::config_flow::repo_check_promotion_requirement(
            sigil_kernel::ToolEffect::ReadOnly
        ),
        "workspace-trust/approval"
    );
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
    assert!(detail.contains("i Add MCP servers in ~/.sigil/sigil.toml"));
    assert!(detail.contains("controls: Tab section · Down actions"));
    assert!(detail.contains("mcp: no configured server to inspect"));
    assert!(!detail.contains("servers:"));
    assert!(!detail.contains("args_csv:"));

    let nav = app.config_nav_lines().join("\n");
    assert!(nav.contains("MCP: Enter next server"));
    assert!(nav.contains("MCP: Down -> footer activate/refresh"));
    assert!(nav.contains("MCP: edit servers in sigil.toml"));
    assert!(!nav.contains("Up/Down field"));
    assert!(!nav.contains("Enter edit/toggle"));
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
    assert!(detail.contains("Soft threshold"));
    assert!(detail.contains("Hard threshold"));
    assert!(detail.contains("Tail messages"));
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
    config.code_intelligence.auto_discover = false;
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

    assert!(detail.contains("Code Intel 7/13 · LSP readiness"));
    assert!(detail.contains("[controls]"));
    assert!(detail.contains("Code intelligence: yes"));
    assert!(detail.contains("Server startup: lazy"));
    assert!(detail.contains("Auto discover: no"));
    assert!(detail.contains("Missing reports: yes"));
    assert!(detail.contains("[trust]"));
    assert!(detail.contains("- Tool access: read + approval-gated write"));
    assert!(detail.contains("- Server process: per-server trust_required"));
    assert!(detail.contains("- Write actions: diff approval required"));
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

    assert!(detail.contains("Terminal 8/13 · terminal integration"));
    assert!(detail.contains("[attention signals]"));
    assert!(detail.contains("> Attention notifications: no  [Enter toggle]"));
    assert!(detail.contains("Notification method: auto"));
    assert!(detail.contains("Long-run threshold: 10000 ms"));
    assert!(detail.contains("[interaction]"));
    assert!(detail.contains("- Keyboard enhancement: auto"));
    assert!(detail.contains("- Mouse capture: yes"));
    assert!(detail.contains("- OSC52 clipboard: yes"));
    assert!(detail.contains("- Scroll sensitivity: 3 rows"));
    assert!(detail.contains("[compatibility]"));
    assert!(detail.contains("Terminal compatibility settings are edited in sigil.toml"));
    assert!(detail.contains("Use defaults unless your terminal"));
    assert!(!detail.contains("Requests terminal mouse events"));
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

    assert!(detail.contains("Appearance 9/13 · TUI theme"));
    assert!(nav.contains("Appearance: Enter cycle"));
    assert!(nav.contains("Appearance: color overrides in sigil.toml"));
    assert!(detail.contains("[theme]"));
    assert!(detail.contains("Theme: gruvbox_dark  [Enter cycle]"));
    assert!(detail.contains("- Name: Gruvbox Dark"));
    assert!(detail.contains("Syntax theme: auto"));
    assert!(detail.contains("- Syntax source: auto -> Gruvbox Dark"));
    assert!(detail.contains("sigil_dark, solarized_dark, solarized_light"));
    assert!(detail.contains("- Overrides: 1 colors"));
    assert!(detail.contains("Fine-grained color token overrides are edited in sigil.toml"));
    assert!(!detail.contains("Color group:"));
    assert!(!detail.contains("Color token:"));
    assert!(!detail.contains("Override:"));
    assert!(!detail.contains("Ctrl-R clears all overrides"));
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
fn config_appearance_keeps_color_token_editing_out_of_primary_flow() {
    let config = test_config();
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &config);
    app.open_config_panel();
    {
        let state = app
            .config_state
            .as_mut()
            .expect("config state should still exist");
        state.set_section(ConfigSection::Appearance);
        assert!(!state.focus_field(ConfigField::AppearanceColorGroup));
        assert!(!state.focus_field(ConfigField::AppearanceColorToken));
        assert!(!state.focus_field(ConfigField::AppearanceColorOverride));
    }

    app.handle_config_paste_text("#010203");

    assert_ne!(app.last_notice(), Some("updated color_override"));
    assert_ne!(
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
fn config_appearance_ctrl_r_does_not_reset_color_overrides() -> Result<()> {
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
    assert_eq!(
        app.last_notice(),
        Some("color overrides are edited in sigil.toml")
    );
    let state = app
        .config_state
        .as_ref()
        .expect("config state should still exist");
    assert_eq!(
        state
            .draft
            .base_root_config
            .appearance
            .colors
            .get("surface_base"),
        Some("#010203")
    );
    assert!(!state.dirty);
    Ok(())
}

#[test]
fn config_appearance_color_shortcuts_cover_noop_paths() -> Result<()> {
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
        assert!(state.focus_field(ConfigField::AppearanceTheme));
    }

    let action = app.handle_key_event(KeyEvent::new(KeyCode::Char('r'), KeyModifiers::CONTROL))?;

    assert!(action.is_none());
    assert_eq!(
        app.last_notice(),
        Some("color overrides are edited in sigil.toml")
    );
    assert!(
        !app.config_state
            .as_ref()
            .expect("config state should still exist")
            .dirty
    );

    let action = app.handle_key_event(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE))?;

    assert!(action.is_none());
    assert_eq!(
        app.last_notice(),
        Some("color overrides are edited in sigil.toml")
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
        assert!(!state.focus_field(ConfigField::AppearanceColorGroup));
    }

    let action = app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))?;

    assert!(action.is_none());
    assert_ne!(app.last_notice(), Some("color group -> borders"));
    assert_eq!(
        app.config_state
            .as_ref()
            .expect("config state should still exist")
            .draft
            .selected_appearance_color_group()
            .key,
        "surfaces"
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
fn config_appearance_field_context_lines_explain_supported_theme_controls() {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.open_config_panel();
    {
        let state = app
            .config_state
            .as_mut()
            .expect("config state should still exist");
        state.set_section(ConfigSection::Appearance);
        assert!(state.focus_field(ConfigField::AppearanceSyntaxTheme));
    }

    let syntax_detail = app.config_detail_lines().join("\n");

    assert!(syntax_detail.contains("auto follows the selected TUI theme"));
    assert!(syntax_detail.contains("Fine-grained color token overrides are edited in sigil.toml"));

    {
        let state = app
            .config_state
            .as_mut()
            .expect("config state should still exist");
        assert!(!state.focus_field(ConfigField::AppearanceColorOverride));
    }
}

#[test]
fn config_appearance_theme_enter_cycles_and_save_updates_snapshot() -> Result<()> {
    let temp = tempdir()?;
    let config_path = temp.path().join("sigil.toml");
    let config = test_config();
    let mut app = AppState::from_root_config(&config_path, &config);
    app.push_timeline(TimelineRole::User, "hello theme");
    let initial_theme_bg = app
        .timeline_render_lines()
        .iter()
        .flat_map(|line| line.spans.iter())
        .find_map(|span| span.style.bg)
        .expect("user bubble should have a themed background");
    app.open_config_panel();
    app.config_state
        .as_mut()
        .expect("config state should still exist")
        .set_section(ConfigSection::Appearance);
    let initial_control_entries = app.session_browser.current_entries.len();

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
        .timeline_render_lines()
        .iter()
        .flat_map(|line| line.spans.iter())
        .find_map(|span| span.style.bg)
        .expect("timeline cache should rebuild with themed background");
    assert_ne!(initial_theme_bg, updated_theme_bg);
    assert_eq!(
        app.session_browser.current_entries.len(),
        initial_control_entries
    );
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
fn config_appearance_usage_cost_currency_enter_cycles_and_saves() -> Result<()> {
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
        assert!(state.focus_field(ConfigField::AppearanceUsageCostCurrency));
    }

    let action = app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))?;

    assert!(action.is_none());
    assert_eq!(app.last_notice(), Some("cost currency -> usd"));
    assert!(
        app.config_detail_lines()
            .join("\n")
            .contains("- Cost source: manual -> USD")
    );

    let action = app.handle_key_event(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL))?;
    let Some(AppAction::ConfigSaved { root_config }) = action else {
        panic!("cost currency save should return config saved action");
    };

    assert_eq!(
        root_config.appearance.usage_cost_currency,
        sigil_kernel::UsageCostCurrency::Usd
    );
    let rendered = std::fs::read_to_string(&config_path)?;
    assert!(rendered.contains("usage_cost_currency = \"usd\""));
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

    assert!(detail.contains("Agents 10/13 · agent profiles"));
    assert!(detail.contains("[discovery]"));
    assert!(detail.contains("- Configured: 5 agents"));
    assert!(detail.contains("- Compatibility: 0 agents"));
    assert!(detail.contains("- Selected: agent 4/5 · review"));
    assert!(detail.contains("[agents]"));
    assert!(detail.contains(
        "> review: trusted · subagent · workspace · enabled=yes user=yes model_visibility=model allowed"
    ));
    assert!(detail.contains("[agent]"));
    assert!(detail.contains("Agent: review"));
    assert!(detail.contains("- Description: Review this repository."));
    assert!(detail.contains("- Kind: subagent"));
    assert!(detail.contains("- Enabled: yes"));
    assert!(detail.contains("- User: yes"));
    assert!(detail.contains("- Model visibility: model allowed"));
    assert!(detail.contains("- Write policy: not write-capable"));
    assert!(detail.contains("- Trust: trusted"));
    assert!(detail.contains("- Source: workspace"));
    assert!(detail.contains("- Source hash:"));
    assert!(detail.contains("- Invocation: model_allowed"));
    assert!(detail.contains("- Tools: names=grep,read_file"));
    assert!(detail.contains(
        "- Permission: mode=manual commands=0 tools=0 rules=0 external=off external_rules=0"
    ));
    assert!(detail.contains("- Nicknames: Repo Review"));
    assert!(detail.contains("- Aliases: rr"));
    assert!(detail.contains("- Slash: /review-agent"));
    assert!(detail.contains("agents: Up/Down agent · PgUp/PgDn wrap · footer trust/disable"));

    let nav = app.config_nav_lines().join("\n");
    assert!(nav.contains("Agents: Up/Down select"));
    assert!(nav.contains("Agents: PgUp/PgDn wrap"));
    assert!(nav.contains("Agents: footer trust/disable"));
    assert_eq!(
        app.config_footer_action_labels(),
        vec!["trust", "disable", "save+close"]
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

    app.runtime.is_busy = true;
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
    assert!(
        app.session_browser
            .current_entries
            .iter()
            .any(|entry| matches!(
                entry,
                SessionLogEntry::Control(ControlEntry::AgentProfileCaptured(_))
            ))
    );
    assert!(
        app.session_browser
            .current_entries
            .iter()
            .any(|entry| matches!(
                entry,
                SessionLogEntry::Control(ControlEntry::AgentProfileTrustDecision(trust))
                    if trust.profile_id.as_str() == "review"
                        && trust.decision == sigil_kernel::AgentTrustState::Trusted
            ))
    );
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
    assert_eq!(
        app.last_notice(),
        Some("agent review model_visibility=manual only")
    );
    assert!(
        app.session_browser
            .current_entries
            .iter()
            .any(|entry| matches!(
                entry,
                SessionLogEntry::Control(ControlEntry::AgentProfilePolicyDecision(policy))
                    if policy.profile_id.as_str() == "review"
                        && policy.model_invocable == Some(false)
                        && policy.enabled.is_none()
                        && policy.user_invocable.is_none()
            ))
    );
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
    assert_eq!(app.last_notice(), Some("agent review disabled"));
    assert!(
        app.session_browser
            .current_entries
            .iter()
            .any(|entry| matches!(
                entry,
                SessionLogEntry::Control(ControlEntry::AgentProfileTrustDecision(trust))
                    if trust.profile_id.as_str() == "review"
                        && trust.decision == sigil_kernel::AgentTrustState::Disabled
            ))
    );

    app.config_state
        .as_mut()
        .expect("config state should still exist")
        .focus_footer(ConfigFooterAction::ToggleAgentEnabled);
    let action = app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))?;
    assert!(action.is_none());
    assert_eq!(app.last_notice(), Some("agent review enabled=no"));
    assert!(
        app.session_browser
            .current_entries
            .iter()
            .any(|entry| matches!(
                entry,
                SessionLogEntry::Control(ControlEntry::AgentProfilePolicyDecision(policy))
                    if policy.profile_id.as_str() == "review" && policy.enabled == Some(false)
            ))
    );

    app.config_state
        .as_mut()
        .expect("config state should still exist")
        .focus_footer(ConfigFooterAction::ToggleAgentUser);
    let action = app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))?;
    assert!(action.is_none());
    assert_eq!(app.last_notice(), Some("agent review user=no"));
    assert!(app.session_browser.current_entries.iter().any(|entry| matches!(
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
    assert_eq!(app.config_selected_footer_action_label(), Some("use"));
    assert_eq!(app.last_notice(), Some("action use_skill"));

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
    assert!(detail.contains("skills: Up/Down skill · PgUp/PgDn wrap · footer use"));
    Ok(())
}

#[test]
fn config_skills_use_footer_opens_optional_instruction_modal() -> Result<()> {
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
    state.focus_footer(ConfigFooterAction::UseSkill);

    let action = app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))?;

    assert!(action.is_none());
    assert_eq!(app.modal_title(), Some("Use Skill"));
    assert!(app.is_config_mode());
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
    app.runtime.is_busy = true;
    {
        let state = app
            .config_state
            .as_mut()
            .expect("config state should exist");
        state.set_section(ConfigSection::Skills);
        state.focus_footer(ConfigFooterAction::UseSkill);
    }
    let action = app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))?;
    assert!(action.is_none());
    assert_eq!(app.last_notice(), Some("busy; use skill later"));

    app.runtime.is_busy = false;
    {
        let state = app
            .config_state
            .as_mut()
            .expect("config state should exist");
        state.set_section(ConfigSection::Provider);
        state.focus_footer(ConfigFooterAction::UseSkill);
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
        state.focus_footer(ConfigFooterAction::UseSkill);
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
    state.focus_footer(ConfigFooterAction::UseSkill);

    let action = app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))?;
    assert!(action.is_none());
    assert_eq!(app.modal_title(), Some("Use Skill"));
    let modal = app.modal_lines().join("\n");
    assert!(modal.contains("Optional instructions for how to use the selected skill."));
    assert!(modal.contains("instructions: |"));
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
    assert_eq!(app.last_notice(), Some("using skill review"));
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
    state.focus_footer(ConfigFooterAction::UseSkill);

    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))?;
    let action = app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))?;

    let Some(AppAction::SubmitPrompt(prompt)) = action else {
        panic!("expected invoke prompt action");
    };
    assert!(prompt.contains("No additional arguments were provided."));
    assert_eq!(app.last_notice(), Some("using skill review"));
    Ok(())
}

#[test]
fn config_skills_invoke_modal_shortcuts_submit_prompt_actions() -> Result<()> {
    for (key_code, expected_notice) in [
        (KeyCode::F(2), "using skill review"),
        (KeyCode::F(3), "using skill review"),
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
        state.focus_footer(ConfigFooterAction::UseSkill);

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
    state.focus_footer(ConfigFooterAction::UseSkill);

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

    assert!(detail.contains("Plugins 12/13 · plugin trust review"));
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
    assert!(detail.contains("- Hook count: 1"));
    assert!(detail.contains("- Hook kinds: context=0 compaction=0 verification=0 event=1"));
    assert!(detail.contains(
        "- Hook effects: read_only=0 workspace_write=0 external_write=0 network=0 unknown=1"
    ));
    assert!(detail.contains("- Runtime: trusted hooks run through execution backend"));
    assert!(detail.contains("- Evidence: mutating hooks record workspace evidence"));
    assert!(detail.contains("- Audit: session records backend, profile, network"));
    assert!(detail.contains("- Inspect: run /doctor for command and issue details"));
    assert!(!detail.contains("- Hook 1 command:"));
    assert!(!detail.contains("- Hook 1 policy:"));
    assert!(detail.contains("[mcp servers]"));
    assert!(detail.contains("- MCP 1: repo-tools"));
    assert!(detail.contains("- MCP 1 command: node server.js"));
    assert!(detail.contains("- MCP 1 startup: lazy"));
    assert!(detail.contains("- MCP 1 required: no"));
    assert!(detail.contains(
        "- MCP 1 policy: local=execute network=unknown source=ask egress=yes secrets=blocked"
    ));
    assert!(detail.contains("- Approve: trusts this reviewed manifest"));
    assert!(detail.contains("- Deny: disables this reviewed manifest"));
    assert!(detail.contains("plugins: Up/Down plugin · PgUp/PgDn wrap · footer approve/deny"));

    let nav = app.config_nav_lines().join("\n");
    assert!(nav.contains("Plugins: Up/Down select"));
    assert!(nav.contains("Plugins: PgUp/PgDn wrap"));
    assert!(nav.contains("Plugins: footer approve/deny"));
    assert_eq!(
        app.config_footer_action_labels(),
        vec!["approve", "deny", "save+close"]
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
fn config_plugins_review_renders_coarse_hook_surface_and_detailed_mcp_surface() -> Result<()> {
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
transport = "stdio"
command = "node"
args = ["server-1.js"]
startup = "lazy"
required = false

[[mcp_servers]]
name = "tools-2"
transport = "stdio"
command = "node"
args = ["server-2.js"]
startup = "lazy"
required = false

[[mcp_servers]]
name = "tools-3"
transport = "stdio"
command = "node"
args = ["server-3.js"]
startup = "eager"
required = true

[[mcp_servers]]
name = "tools-4"
transport = "stdio"
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
        "- Hook count: 4",
        "- Hook kinds: context=0 compaction=0 verification=0 event=4",
        "- Hook effects: read_only=0 workspace_write=0 external_write=0 network=0 unknown=4",
        "- Runtime: trusted hooks run through execution backend",
        "- Evidence: mutating hooks record workspace evidence",
        "- Audit: session records backend, profile, network",
        "- Inspect: run /doctor for command and issue details",
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
    assert!(!detail.contains("- Hook 1 command:"));
    assert!(!detail.contains("- Hook 4 command:"));
    assert!(!detail.contains("- Hook 1 policy:"));
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
    app.runtime.is_busy = true;
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

    app.runtime.is_busy = false;
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
    assert!(
        !app.session_browser
            .current_entries
            .iter()
            .any(|entry| matches!(
                entry,
                SessionLogEntry::Control(ControlEntry::PluginTrustDecision(_))
            ))
    );
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
    state.draft.permission_mode = sigil_kernel::PermissionMode::AutoEdit;
    state.draft.memory_enabled = false;
    state.draft.compaction_soft_threshold_ratio = "0.40".to_owned();
    state.draft.compaction_hard_threshold_ratio = "0.75".to_owned();
    state.draft.compaction_context_window_tokens = "64000".to_owned();
    state.draft.code_intelligence_enabled = true;
    state.draft.code_intelligence_server_startup = sigil_kernel::CodeIntelStartup::Eager;
    state.draft.code_intelligence_auto_discover = false;
    state.draft.code_intelligence_report_missing = true;
    state.draft.terminal_mouse_capture = false;
    state.draft.terminal_osc52_clipboard = false;
    state.draft.terminal_scroll_sensitivity = "6".to_owned();
    state.draft.terminal_notifications_enabled = true;
    state.draft.terminal_notification_method = sigil_kernel::TerminalNotificationMethod::Osc9;
    state.draft.terminal_notification_minimum_run_duration_ms = "15000".to_owned();
    state.dirty = true;

    let action = app.handle_key_event(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL))?;

    let Some(AppAction::ConfigSaved { root_config }) = action else {
        panic!("expected config save action");
    };
    assert_eq!(root_config.agent.model, "deepseek-v4-pro");
    assert_eq!(
        root_config.permission.mode,
        sigil_kernel::PermissionMode::AutoEdit
    );
    assert!(!root_config.memory.enabled);
    assert_eq!(root_config.compaction.soft_threshold_ratio, 0.40);
    assert_eq!(root_config.compaction.hard_threshold_ratio, 0.75);
    assert_eq!(root_config.compaction.context_window_tokens, Some(64_000));
    assert!(root_config.code_intelligence.enabled);
    assert_eq!(
        root_config.code_intelligence.server_startup,
        sigil_kernel::CodeIntelStartup::Eager
    );
    assert!(!root_config.code_intelligence.auto_discover);
    assert!(root_config.code_intelligence.report_missing);
    assert!(!root_config.terminal.mouse_capture);
    assert!(!root_config.terminal.osc52_clipboard);
    assert_eq!(root_config.terminal.scroll_sensitivity, 6);
    assert!(root_config.terminal.notifications.enabled);
    assert_eq!(
        root_config.terminal.notifications.method,
        sigil_kernel::TerminalNotificationMethod::Osc9
    );
    assert_eq!(
        root_config.terminal.notifications.minimum_run_duration_ms,
        15_000
    );
    assert!(!app.config_is_dirty());
    assert_eq!(app.runtime.permission_mode, "auto-edit");
    assert!(!app.runtime.memory_enabled);

    let saved = RootConfig::load(&config_path)?;
    assert_eq!(saved.agent.model, "deepseek-v4-pro");
    assert_eq!(
        saved.permission.mode,
        sigil_kernel::PermissionMode::AutoEdit
    );
    assert!(!saved.memory.enabled);
    assert_eq!(saved.compaction.soft_threshold_ratio, 0.40);
    assert_eq!(saved.compaction.hard_threshold_ratio, 0.75);
    assert_eq!(saved.compaction.context_window_tokens, Some(64_000));
    assert!(saved.code_intelligence.enabled);
    assert_eq!(
        saved.code_intelligence.server_startup,
        sigil_kernel::CodeIntelStartup::Eager
    );
    assert!(!saved.code_intelligence.auto_discover);
    assert!(saved.code_intelligence.report_missing);
    assert!(!saved.terminal.mouse_capture);
    assert!(!saved.terminal.osc52_clipboard);
    assert_eq!(saved.terminal.scroll_sensitivity, 6);
    assert!(saved.terminal.notifications.enabled);
    assert_eq!(
        saved.terminal.notifications.method,
        sigil_kernel::TerminalNotificationMethod::Osc9
    );
    assert_eq!(saved.terminal.notifications.minimum_run_duration_ms, 15_000);
    let saved_raw = std::fs::read_to_string(&config_path)?;
    assert!(saved_raw.contains("fallback_context_window_tokens = 64000"));
    assert!(
        !saved_raw
            .lines()
            .any(|line| line.trim_start().starts_with("context_window_tokens ="))
    );
    assert_eq!(saved.agent.model, "deepseek-v4-pro");
    assert!(
        saved
            .providers
            .get("deepseek")
            .is_some_and(|value| value.get("model").is_none())
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
fn runtime_permission_toggle_persists_mode_to_config() -> Result<()> {
    let temp = tempdir()?;
    let config_path = temp.path().join("sigil.toml");
    let root_config = test_config();
    root_config.save(&config_path)?;

    let mut app = AppState::from_root_config(&config_path, &root_config);

    let action = app.handle_key_event(KeyEvent::new(KeyCode::BackTab, KeyModifiers::NONE))?;

    let Some(AppAction::RuntimeConfigUpdated { root_config }) = action else {
        panic!("expected runtime config update action");
    };
    assert_eq!(
        root_config.permission.mode,
        sigil_kernel::PermissionMode::AutoEdit
    );
    assert_eq!(app.runtime.permission_mode, "auto-edit");

    let saved = RootConfig::load(&config_path)?;
    assert_eq!(
        saved.permission.mode,
        sigil_kernel::PermissionMode::AutoEdit
    );
    let reopened = AppState::from_root_config(&config_path, &saved);
    assert_eq!(reopened.runtime.permission_mode, "auto-edit");
    Ok(())
}

#[test]
fn config_verification_auto_run_persists_to_config() -> Result<()> {
    let temp = tempdir()?;
    let config_path = temp.path().join("sigil.toml");
    let root_config = test_config();
    root_config.save(&config_path)?;

    let mut app = AppState::from_root_config(&config_path, &root_config);
    app.open_config_panel();
    {
        let state = app
            .config_state
            .as_mut()
            .expect("config state should exist");
        state.set_section(ConfigSection::Permissions);
        state.selected_field = Some(ConfigField::VerificationAutoRun);
    }

    let action = app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))?;
    assert!(action.is_none());
    let action = app.handle_key_event(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL))?;

    let Some(AppAction::ConfigSaved { root_config }) = action else {
        panic!("expected config save action");
    };
    assert_eq!(
        root_config.verification.auto_run,
        sigil_kernel::VerificationAutoRunPolicy::TrustedOnly
    );
    let saved = RootConfig::load(&config_path)?;
    assert_eq!(
        saved.verification.auto_run,
        sigil_kernel::VerificationAutoRunPolicy::TrustedOnly
    );
    Ok(())
}

#[test]
fn config_mcp_server_creation_stays_config_file_only() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
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
    assert_eq!(app.last_notice(), Some("edit MCP servers in sigil.toml"));
    let state = app
        .config_state
        .as_ref()
        .expect("config state should still exist");
    assert!(state.draft.mcp_servers.is_empty());

    let detail = app.config_detail_lines().join("\n");
    assert!(detail.contains("No MCP servers configured"));
    assert!(detail.contains("Add MCP servers in ~/.sigil/sigil.toml"));
    assert!(detail.contains("Transport-specific fields are edited in the config file"));
    assert!(!detail.contains("Command"));
    assert!(!detail.contains("Arguments"));
    assert!(!detail.contains("args_csv:"));
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
    app.runtime.is_busy = true;

    let action = app.handle_key_event(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL))?;

    assert!(action.is_none());
    assert_eq!(app.last_notice(), Some("busy; save later"));
    let saved = RootConfig::load(&config_path)?;
    assert_eq!(saved.agent.model, "deepseek-v4-flash");
    Ok(())
}

#[test]
fn config_clean_save_skips_worker_restart_even_when_busy() -> Result<()> {
    let temp = tempdir()?;
    let config_path = temp.path().join("sigil.toml");
    test_config().save(&config_path)?;

    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.config_path = config_path;
    app.open_config_panel();
    app.runtime.is_busy = true;
    app.config_state
        .as_mut()
        .expect("config state should exist after opening /config")
        .close_guard_armed = true;

    let action = app.handle_key_event(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL))?;

    assert!(action.is_none());
    assert!(app.is_config_mode());
    assert!(!app.config_is_dirty());
    assert!(!app.config_close_guard_armed());
    assert_eq!(app.last_notice(), Some("saved config"));
    Ok(())
}

#[test]
fn config_clean_footer_save_and_close_closes_without_worker_restart() -> Result<()> {
    let temp = tempdir()?;
    let config_path = temp.path().join("sigil.toml");
    test_config().save(&config_path)?;

    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.config_path = config_path;
    app.open_config_panel();
    app.runtime.is_busy = true;
    app.config_state
        .as_mut()
        .expect("config state should exist after opening /config")
        .focus_footer(ConfigFooterAction::SaveAndClose);

    let action = app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))?;

    assert!(action.is_none());
    assert!(!app.is_config_mode());
    assert_eq!(app.last_notice(), Some("saved config and closed"));
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
    state.focus_footer(ConfigFooterAction::SaveAndClose);

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
    assert!(saved_raw.contains("[model_request]"));
    assert!(saved_raw.contains("stream_idle_timeout_secs = 180"));
    assert!(
        !saved_raw
            .split("[providers.deepseek]")
            .nth(1)
            .unwrap_or_default()
            .lines()
            .any(|line| line.trim_start().starts_with("request_timeout_secs ="))
    );
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
    let _env_guard = crate::test_env::lock();
    let _api_key = crate::test_env::EnvScope::unset("SIGIL_API_KEY");
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
        mcp_server_config! {
            name: "off".to_owned(),
            command: "mcp-off".to_owned(),
            trust: sigil_kernel::McpServerTrustPolicy {
                pin_version: false,
                allow_secrets: false,
                ..Default::default()
            },
            ..Default::default()
        },
        mcp_server_config! {
            name: "pinned".to_owned(),
            command: "mcp-pinned".to_owned(),
            trust: sigil_kernel::McpServerTrustPolicy {
                pin_version: true,
                pinned: Some(sigil_kernel::McpServerPinnedIdentity {
                    transport_fingerprint:
                        "sha256:abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789"
                            .to_owned(),
                    protocol_version: "2024-11-05".to_owned(),
                    server_name: "pinned".to_owned(),
                    server_version: "1.0.0".to_owned(),
                }),
                allow_secrets: true,
                ..Default::default()
            },
            ..Default::default()
        },
        mcp_server_config! {
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
    let off_lines = app.config_detail_lines().join("\n");
    assert!(off_lines.contains("Pin: off"));
    assert!(off_lines.contains("Boundary: local stdio outside local sandbox"));
    assert!(off_lines.contains("Source policy: ask"));
    assert!(off_lines.contains("Tool local access: read"));
    assert!(off_lines.contains("Tool network effect: unknown"));
    assert!(off_lines.contains("Server launch: execute · network unknown"));

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
    assert_eq!(
        app.last_notice(),
        Some("MCP server editing uses sigil.toml")
    );

    let action = app.handle_key_event(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL))?;
    assert!(action.is_none());
    assert_eq!(
        app.last_notice(),
        Some("MCP server editing uses sigil.toml")
    );

    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE))?;
    assert_eq!(app.config_section_title(), Some("Permissions"));

    let _ = app.handle_key_event(KeyEvent::new(KeyCode::BackTab, KeyModifiers::SHIFT))?;
    assert_eq!(app.config_section_title(), Some("Provider"));

    let mut config = test_config();
    config.mcp_servers.push(mcp_server_config! {
        name: "filesystem".to_owned(),
        command: "mcp-filesystem".to_owned(),
        ..Default::default()
    });
    config.mcp_servers.push(mcp_server_config! {
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
    app.config_state
        .as_mut()
        .expect("config state should exist")
        .focus_footer(ConfigFooterAction::ActivateMcp);

    let _ = app.handle_key_event(KeyEvent::new(KeyCode::PageUp, KeyModifiers::NONE))?;
    assert_eq!(
        app.config_state
            .as_ref()
            .expect("config state should exist")
            .selected_mcp_server_index,
        1
    );
    assert!(
        !app.config_state
            .as_ref()
            .expect("config state should exist")
            .footer_selected
    );
    assert_eq!(app.config_selected_field_label(), Some("Server"));
    assert_eq!(app.last_notice(), Some("mcp server 2/2"));

    let _ = app.handle_key_event(KeyEvent::new(KeyCode::PageDown, KeyModifiers::NONE))?;
    assert_eq!(
        app.config_state
            .as_ref()
            .expect("config state should exist")
            .selected_mcp_server_index,
        0
    );
    assert!(
        !app.config_state
            .as_ref()
            .expect("config state should exist")
            .footer_selected
    );
    assert_eq!(app.last_notice(), Some("mcp server 1/2"));

    let _ = app.handle_key_event(KeyEvent::new(KeyCode::PageDown, KeyModifiers::NONE))?;
    assert_eq!(
        app.config_state
            .as_ref()
            .expect("config state should exist")
            .selected_mcp_server_index,
        1
    );
    assert_eq!(app.config_selected_field_label(), Some("Server"));

    let _ = app.handle_key_event(KeyEvent::new(KeyCode::PageUp, KeyModifiers::NONE))?;
    assert_eq!(
        app.config_state
            .as_ref()
            .expect("config state should exist")
            .selected_mcp_server_index,
        0
    );
    assert!(
        !app.config_state
            .as_ref()
            .expect("config state should exist")
            .footer_selected
    );

    let mut empty_app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    empty_app.open_config_panel();
    empty_app
        .config_state
        .as_mut()
        .expect("config state should exist")
        .set_section(ConfigSection::Mcp);

    let _ = empty_app.handle_key_event(KeyEvent::new(KeyCode::PageDown, KeyModifiers::NONE))?;
    assert_eq!(empty_app.config_selected_field_label(), None);
    assert_eq!(empty_app.last_notice(), Some("no MCP server to select"));
    Ok(())
}

#[test]
fn config_mcp_enter_cycles_server_and_footer_activates_selected_server() -> Result<()> {
    let mut config = test_config();
    for name in ["env-ready", "env-missing"] {
        config.mcp_servers.push(mcp_server_config! {
            name: name.to_owned(),
            command: "mcp-probe".to_owned(),
            startup: McpServerStartup::Lazy,
            ..Default::default()
        });
    }
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &config);
    app.open_config_panel();
    app.config_state
        .as_mut()
        .expect("config state should exist")
        .set_section(ConfigSection::Mcp);

    let detail = app.config_detail_lines().join("\n");
    assert!(detail.contains("> Server: env-ready (1/2)  [Enter cycle]"));
    assert!(detail.contains("Runtime"));
    assert!(detail.contains("deferred"));

    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE))?;
    let state = app
        .config_state
        .as_ref()
        .expect("config state should exist");
    assert_eq!(state.selected_mcp_server_index, 0);
    assert!(!state.footer_selected);
    assert_eq!(state.selected_field, Some(ConfigField::McpName));

    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE))?;
    let state = app
        .config_state
        .as_ref()
        .expect("config state should exist");
    assert_eq!(state.selected_mcp_server_index, 0);
    assert!(state.footer_selected);
    assert_eq!(
        state.selected_footer_action,
        ConfigFooterAction::ActivateMcp
    );
    assert_eq!(app.last_notice(), Some("action activate_mcp"));

    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE))?;
    assert_eq!(app.config_selected_field_label(), Some("save_and_close"));
    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE))?;
    let state = app
        .config_state
        .as_ref()
        .expect("config state should exist");
    assert_eq!(state.selected_mcp_server_index, 0);
    assert!(!state.footer_selected);
    assert_eq!(state.selected_field, Some(ConfigField::McpName));

    let action = app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))?;
    assert!(action.is_none());
    let state = app
        .config_state
        .as_ref()
        .expect("config state should exist");
    assert_eq!(state.selected_mcp_server_index, 1);
    assert!(!state.dirty);
    assert_eq!(app.last_notice(), Some("mcp server 2/2"));
    let detail = app.config_detail_lines().join("\n");
    assert!(detail.contains("> Server: env-missing (2/2)  [Enter cycle]"));

    let action = app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))?;
    assert!(action.is_none());
    assert_eq!(
        app.config_state
            .as_ref()
            .expect("config state should exist")
            .selected_mcp_server_index,
        0
    );
    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))?;
    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE))?;
    let action = app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))?;
    assert!(matches!(
        action,
        Some(AppAction::ActivateLazyMcp {
            server_name: Some(ref server_name)
        }) if server_name == "env-missing"
    ));
    let action = app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))?;
    assert!(action.is_none());
    assert_eq!(
        app.last_notice(),
        Some("MCP env-missing is already activating")
    );
    let state = app
        .config_state
        .as_ref()
        .expect("config state should exist");
    assert_eq!(state.selected_mcp_server_index, 1);
    assert!(state.footer_selected);
    let detail = app.config_detail_lines().join("\n");
    assert!(detail.contains("Server: env-missing (2/2)"));
    assert!(detail.contains("activating"));
    Ok(())
}

#[test]
fn config_mcp_server_selector_renders_selected_position() {
    let mut config = test_config();
    for index in 0..8 {
        config.mcp_servers.push(mcp_server_config! {
            name: format!("mcp-{index}"),
            command: "mcp-probe".to_owned(),
            startup: McpServerStartup::Lazy,
            ..Default::default()
        });
    }
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &config);
    app.open_config_panel();
    let state = app
        .config_state
        .as_mut()
        .expect("config state should exist");
    state.set_section(ConfigSection::Mcp);
    state.selected_mcp_server_index = 6;

    let detail = app.config_detail_lines().join("\n");
    assert!(detail.contains("> Server: mcp-6 (7/8)  [Enter cycle]"));
    assert!(!detail.contains("Server window"));
    assert!(!detail.contains("mcp-0"));
    assert!(detail.contains("deferred"));

    app.config_state
        .as_mut()
        .expect("config state should exist")
        .selected_mcp_server_index = 1;
    let detail = app.config_detail_lines().join("\n");
    assert!(detail.contains("> Server: mcp-1 (2/8)  [Enter cycle]"));
    assert!(!detail.contains("mcp-7"));
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
        state.selected_field = Some(ConfigField::PermissionMode);
    }
    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))?;
    let state = app
        .config_state
        .as_ref()
        .expect("config state should exist");
    assert_eq!(
        state.draft.permission_mode,
        sigil_kernel::PermissionMode::AutoEdit
    );
    assert!(state.dirty);

    {
        let state = app
            .config_state
            .as_mut()
            .expect("config state should exist");
        state.set_section(ConfigSection::Permissions);
        state.selected_field = Some(ConfigField::VerificationAutoRun);
    }
    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))?;
    let state = app
        .config_state
        .as_ref()
        .expect("config state should exist");
    assert_eq!(
        state.draft.verification_auto_run,
        sigil_kernel::VerificationAutoRunPolicy::TrustedOnly
    );

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
        .selected_field = Some(ConfigField::CodeIntelServerStartup);
    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))?;
    assert_eq!(
        app.config_state
            .as_ref()
            .expect("config state should exist")
            .draft
            .code_intelligence_server_startup,
        sigil_kernel::CodeIntelStartup::Eager
    );
    {
        let state = app
            .config_state
            .as_mut()
            .expect("config state should exist");
        assert!(!state.focus_field(ConfigField::CodeIntelAutoDiscover));
        assert!(!state.focus_field(ConfigField::CodeIntelReportMissing));
    }

    {
        let state = app
            .config_state
            .as_mut()
            .expect("config state should exist");
        state.set_section(ConfigSection::Terminal);
        assert_eq!(
            state.selected_field,
            Some(ConfigField::TerminalNotificationsEnabled)
        );
        assert!(!state.focus_field(ConfigField::TerminalMouseCapture));
        assert!(!state.focus_field(ConfigField::TerminalOsc52Clipboard));
    }
    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))?;
    assert!(
        app.config_state
            .as_ref()
            .expect("config state should exist")
            .draft
            .terminal_notifications_enabled
    );
    app.config_state
        .as_mut()
        .expect("config state should exist")
        .selected_field = Some(ConfigField::TerminalNotificationMethod);
    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))?;
    assert_eq!(
        app.config_state
            .as_ref()
            .expect("config state should exist")
            .draft
            .terminal_notification_method,
        sigil_kernel::TerminalNotificationMethod::Osc9
    );
    assert!(
        app.config_state
            .as_ref()
            .expect("config state should exist")
            .draft
            .terminal_mouse_capture
    );
    assert!(
        app.config_state
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
        .selected_field = Some(ConfigField::ProviderBaseUrl);
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

    app.composer.input = "/config".to_owned();
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
    assert_eq!(app.config_selected_field_label(), None);
    assert_eq!(app.last_notice(), Some("no MCP server to select"));

    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE))?;
    assert_eq!(app.config_selected_field_label(), Some("activate_mcp"));
    assert_eq!(app.last_notice(), Some("action activate_mcp"));
    Ok(())
}

#[test]
fn config_mcp_shortcuts_outside_mcp_section_show_guidance() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.open_config_panel();
    assert_eq!(app.config_section_title(), Some("Provider"));

    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Char('n'), KeyModifiers::CONTROL))?;
    assert_eq!(
        app.last_notice(),
        Some("MCP server editing uses sigil.toml")
    );

    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL))?;
    assert_eq!(
        app.last_notice(),
        Some("MCP server editing uses sigil.toml")
    );
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
    assert_eq!(app.last_notice(), Some("edit MCP servers in sigil.toml"));

    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Char('n'), KeyModifiers::CONTROL))?;
    assert_eq!(app.last_notice(), Some("edit MCP servers in sigil.toml"));
    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL))?;
    assert_eq!(app.last_notice(), Some("edit MCP servers in sigil.toml"));

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

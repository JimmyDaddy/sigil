use std::{collections::BTreeMap, path::Path};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{
    Terminal,
    backend::{Backend, TestBackend},
};
use sigil_kernel::{
    AgentConfig, CompactionConfig, MemoryConfig, PermissionConfig, RootConfig, SessionConfig,
    WorkspaceConfig,
};

use crate::{app::AppState, ui::theme::Theme};

use super::*;

fn test_config() -> RootConfig {
    RootConfig {
        config_version: 2,
        composition: Default::default(),
        workspace: WorkspaceConfig {
            root: ".".to_owned(),
        },
        storage: Default::default(),
        session: SessionConfig {
            log_dir: Some(".sigil/sessions".to_owned()),
            retention: Default::default(),
        },
        agent: AgentConfig {
            runtime_provider: "deepseek".to_owned(),
            connection: Some(
                sigil_kernel::ConnectionId::new("deepseek-default").expect("test connection id"),
            ),
            model: "deepseek-v4-flash".to_owned(),
            max_turns: None,
            tool_timeout_secs: 30,
        },
        model_request: Default::default(),
        permission: PermissionConfig::default(),
        memory: MemoryConfig::with_enabled(true),
        skills: Default::default(),
        compaction: CompactionConfig::default(),
        code_intelligence: Default::default(),
        terminal: Default::default(),
        execution: Default::default(),
        verification: Default::default(),
        appearance: Default::default(),
        task: Default::default(),
        connections: BTreeMap::from([(
            "deepseek-default".to_owned(),
            serde_json::json!({
                "label": "DeepSeek",
                "provider": "deepseek",
                "protocol": "deepseek",
                "base_url": "https://api.deepseek.com",
                "credential": {
                    "source": "environment",
                    "name": "SIGIL_API_KEY"
                }
            }),
        )]),
        web: Default::default(),
        mcp_servers: Vec::new(),
    }
}

#[test]
fn modal_visual_uses_config_palette_when_config_panel_is_open() -> anyhow::Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.composer.input = "/config".to_owned();
    let _ = app.submit_input()?;

    let visual = modal_visual(&app);
    assert_eq!(visual.accent, super::theme::config_primary());
    assert_eq!(visual.border, super::theme::config_border());
    assert_eq!(visual.label, super::theme::config_detail());
    assert_eq!(visual.command_bg, super::theme::config_tab_bg());
    Ok(())
}

#[test]
fn modal_visual_uses_setup_title_palettes_for_model_and_api_key() -> anyhow::Result<()> {
    let _environment_lock = crate::test_env::lock();
    let _deepseek_environment = crate::test_env::EnvScope::unset("SIGIL_API_KEY");
    let mut model_app = AppState::from_setup(
        Path::new("sigil.toml").to_path_buf(),
        Path::new(".").to_path_buf(),
        None,
    );
    model_app
        .setup_state_mut()
        .expect("setup")
        .credential_source = crate::setup::SetupCredentialSource::Environment;
    let _ = model_app.handle_key_event(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE))?;
    let _ = model_app.handle_key_event(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE))?;
    let _ = model_app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))?;
    let _ = model_app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))?;
    assert_eq!(model_app.modal_title(), Some("Model"));
    let theme = Theme::default();

    let model_visual = modal_visual(&model_app);
    assert_eq!(model_visual.accent, theme.palette.accent_info);
    assert_eq!(model_visual.command_bg, theme.palette.modal_command_bg);

    let mut api_app = AppState::from_setup(
        Path::new("sigil.toml").to_path_buf(),
        Path::new(".").to_path_buf(),
        None,
    );
    let _ = api_app.handle_key_event(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE))?;
    let _ = api_app.handle_key_event(KeyEvent::new(KeyCode::Char('k'), KeyModifiers::NONE))?;
    assert_eq!(api_app.modal_title(), Some("API Key"));

    let api_visual = modal_visual(&api_app);
    assert_eq!(api_visual.accent, theme.palette.accent_warning);
    assert_eq!(api_visual.backdrop_border, theme.palette.accent_warning);
    Ok(())
}

#[test]
fn novice_text_modal_surface_keeps_unicode_cursor_selection_and_long_value_visible_at_80x12()
-> anyhow::Result<()> {
    let temp = tempfile::tempdir()?;
    let mut app =
        AppState::from_setup(temp.path().join("sigil.toml"), temp.path().to_owned(), None);
    app.setup_state_mut().expect("setup").selected_field = crate::setup::SetupField::Model;
    app.handle_key_event(KeyEvent::new(KeyCode::Char('中'), KeyModifiers::NONE))?;
    app.handle_paste_text(&format!("{}END", "界".repeat(80)));
    app.set_terminal_size(80, 12);
    let mut terminal = Terminal::new(TestBackend::new(80, 12))?;
    let render = |terminal: &mut Terminal<TestBackend>, app: &AppState| -> anyhow::Result<()> {
        let surface = crate::surface_adapter::build_surface_model(
            Rect::new(0, 0, 80, 12),
            app,
            crate::surface::SurfaceState {
                frame_generation: 1,
                terminal_epoch: 1,
            },
        );
        terminal.draw(|frame| crate::ui::render_surface(frame, &surface))?;
        Ok(())
    };
    render(&mut terminal, &app)?;
    let cursor = terminal.backend_mut().get_cursor_position()?;
    let geometry = modal_geometry(Rect::new(0, 0, 80, 12), &app);
    assert!(geometry.content.contains(cursor));
    assert_eq!(cursor.y, geometry.content.bottom() - 1);
    let row = (0..80)
        .map(|x| terminal.backend().buffer()[(x, cursor.y)].symbol())
        .collect::<String>();
    assert!(
        row.contains("END"),
        "viewport must reveal the end rather than clamp a hidden caret"
    );
    app.handle_key_event(KeyEvent::new(KeyCode::Home, KeyModifiers::NONE))?;
    render(&mut terminal, &app)?;
    let home = terminal.backend_mut().get_cursor_position()?;
    assert_eq!(terminal.backend().buffer()[(home.x, home.y)].symbol(), "中");
    app.handle_key_event(KeyEvent::new(KeyCode::Right, KeyModifiers::SHIFT))?;
    render(&mut terminal, &app)?;
    assert_eq!(terminal.backend_mut().get_cursor_position()?.x, home.x + 2);
    assert_eq!(
        terminal.backend().buffer()[(home.x, home.y)].bg,
        modal_visual(&app).accent
    );
    assert_eq!(app.modal_input_view().expect("input").selection, Some(0..1));
    Ok(())
}

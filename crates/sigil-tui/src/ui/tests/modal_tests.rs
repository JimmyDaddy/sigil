use std::{collections::BTreeMap, path::Path};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use sigil_kernel::{
    AgentConfig, CompactionConfig, MemoryConfig, PermissionConfig, RootConfig, SessionConfig,
    WorkspaceConfig,
};

use crate::{app::AppState, ui::theme::Theme};

use super::*;

fn test_config() -> RootConfig {
    RootConfig {
        workspace: WorkspaceConfig {
            root: ".".to_owned(),
        },
        storage: Default::default(),
        session: SessionConfig {
            log_dir: Some(".sigil/sessions".to_owned()),
        },
        agent: AgentConfig {
            provider: "deepseek".to_owned(),
            model: "deepseek-v4-flash".to_owned(),
            max_turns: None,
            tool_timeout_secs: 30,
        },
        permission: PermissionConfig::default(),
        memory: MemoryConfig { enabled: true },
        skills: Default::default(),
        compaction: CompactionConfig::default(),
        code_intelligence: Default::default(),
        terminal: Default::default(),
        execution: Default::default(),
        verification: Default::default(),
        appearance: Default::default(),
        task: Default::default(),
        providers: BTreeMap::new(),
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
    let mut model_app = AppState::from_setup(
        Path::new("sigil.toml").to_path_buf(),
        Path::new(".").to_path_buf(),
        None,
    );
    let _ = model_app.handle_key_event(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE))?;
    let _ = model_app.handle_key_event(KeyEvent::new(KeyCode::Char('g'), KeyModifiers::NONE))?;
    assert_eq!(model_app.modal_title(), Some("Model ID"));
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
    let _ = api_app.handle_key_event(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE))?;
    let _ = api_app.handle_key_event(KeyEvent::new(KeyCode::Char('k'), KeyModifiers::NONE))?;
    assert_eq!(api_app.modal_title(), Some("API Key"));

    let api_visual = modal_visual(&api_app);
    assert_eq!(api_visual.accent, theme.palette.accent_warning);
    assert_eq!(api_visual.backdrop_border, theme.palette.accent_warning);
    Ok(())
}

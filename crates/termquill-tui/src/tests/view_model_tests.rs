use std::{collections::BTreeMap, path::Path};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use termquill_kernel::{
    AgentConfig, CompactionConfig, MemoryConfig, PermissionConfig, RootConfig, SessionConfig,
    WorkspaceConfig,
};

use super::*;

fn test_config() -> RootConfig {
    RootConfig {
        workspace: WorkspaceConfig {
            root: ".".to_owned(),
        },
        session: SessionConfig {
            log_dir: ".termquill/sessions".to_owned(),
        },
        agent: AgentConfig {
            provider: "deepseek".to_owned(),
            model: "deepseek-v4-flash".to_owned(),
            max_turns: 8,
            tool_timeout_secs: 30,
        },
        permission: PermissionConfig::default(),
        memory: MemoryConfig { enabled: true },
        compaction: CompactionConfig::default(),
        providers: BTreeMap::new(),
        mcp_servers: Vec::new(),
    }
}

#[test]
fn ui_view_model_projects_info_rail_and_composer_state() {
    let app = AppState::from_root_config(Path::new("/tmp/termquill.toml"), &test_config());
    let view_model = UiViewModel::from_app(&app);

    assert_eq!(view_model.composer.model_name, "deepseek-v4-flash");
    assert_eq!(view_model.composer.input_rows, 1);
    assert!(!view_model.info_rail.workspace_label.is_empty());
    assert!(
        view_model
            .info_rail
            .session_lines
            .iter()
            .any(|line| line == "provider: deepseek")
    );
    assert!(
        view_model
            .info_rail
            .controls
            .iter()
            .any(|line| line == "F1: keyboard help")
    );
    assert!(
        view_model
            .info_rail
            .controls
            .iter()
            .any(|line| line == "Ctrl-C: quit")
    );
}

#[test]
fn composer_view_model_tracks_input_cursor() -> anyhow::Result<()> {
    let mut app = AppState::from_root_config(Path::new("/tmp/termquill.toml"), &test_config());
    app.handle_key_event(KeyEvent::new(KeyCode::Char('你'), KeyModifiers::NONE))?;
    app.handle_key_event(KeyEvent::new(KeyCode::Char('好'), KeyModifiers::NONE))?;

    let view_model = UiViewModel::from_app(&app);

    assert_eq!(view_model.composer.input, "你好");
    assert_eq!(view_model.composer.cursor_position, (4, 0));
    Ok(())
}

#[test]
fn live_panel_view_model_projects_activity_and_transcript_rows() -> anyhow::Result<()> {
    let mut app = AppState::from_root_config(Path::new("/tmp/termquill.toml"), &test_config());
    app.set_terminal_size(120, 30);
    app.handle_key_event(KeyEvent::new(KeyCode::Char('你'), KeyModifiers::NONE))?;
    app.handle_key_event(KeyEvent::new(KeyCode::Char('好'), KeyModifiers::NONE))?;
    let _ = app.submit_input()?;

    let view_model = LivePanelViewModel::from_app(&app, 2);

    assert_eq!(
        view_model.activity,
        Some(LiveActivityViewModel {
            label: "thinking".to_owned(),
            detail: "reasoning with deepseek-v4-flash".to_owned(),
        })
    );
    assert!(view_model.transcript_lines.len() <= 2);
    Ok(())
}

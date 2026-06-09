use std::{collections::BTreeMap, path::Path};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{Terminal, backend::TestBackend};
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
            max_turns: None,
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
fn render_main_screen_shows_keyboard_help_modal() -> anyhow::Result<()> {
    let mut app = AppState::from_root_config(Path::new("termquill.toml"), &test_config());
    let _ = app.handle_key_event(KeyEvent::new(KeyCode::F(1), KeyModifiers::NONE))?;
    let backend = TestBackend::new(96, 24);
    let mut terminal = Terminal::new(backend)?;

    terminal.draw(|frame| render(frame, &app))?;

    let rendered = terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .map(|cell| cell.symbol())
        .collect::<String>();
    assert!(rendered.contains("Core shortcuts"));
    Ok(())
}

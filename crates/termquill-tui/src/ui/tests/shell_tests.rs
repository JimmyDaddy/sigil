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
        code_intelligence: Default::default(),
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

#[test]
fn render_main_screen_collapses_info_rail_on_narrow_terminals() -> anyhow::Result<()> {
    let app = AppState::from_root_config(Path::new("termquill.toml"), &test_config());
    let backend = TestBackend::new(80, 24);
    let mut terminal = Terminal::new(backend)?;

    terminal.draw(|frame| render(frame, &app))?;

    let rendered = terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .map(|cell| cell.symbol())
        .collect::<String>();
    assert!(!rendered.contains("info"));
    assert!(rendered.contains("Build"));
    assert!(rendered.contains("ctx"));
    Ok(())
}

#[test]
fn render_main_screen_keeps_info_rail_on_wide_terminals() -> anyhow::Result<()> {
    let app = AppState::from_root_config(Path::new("termquill.toml"), &test_config());
    let backend = TestBackend::new(140, 24);
    let mut terminal = Terminal::new(backend)?;

    terminal.draw(|frame| render(frame, &app))?;

    let rendered = terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .map(|cell| cell.symbol())
        .collect::<String>();
    assert!(rendered.contains("info"));
    assert!(rendered.contains("session"));
    assert!(rendered.contains("LSP"));
    Ok(())
}

#[test]
fn render_main_screen_places_cursor_on_new_composer_line() -> anyhow::Result<()> {
    let mut app = AppState::from_root_config(Path::new("termquill.toml"), &test_config());
    app.set_terminal_size(80, 12);
    app.handle_key_event(KeyEvent::new(KeyCode::Char('h'), KeyModifiers::NONE))?;
    app.handle_key_event(KeyEvent::new(KeyCode::Char('i'), KeyModifiers::NONE))?;
    app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::SHIFT))?;
    let backend = TestBackend::new(80, 12);
    let mut terminal = Terminal::new(backend)?;

    terminal.draw(|frame| render(frame, &app))?;

    terminal.backend_mut().assert_cursor_position((3, 9));
    Ok(())
}

#[test]
fn render_main_screen_keeps_composer_text_visible() -> anyhow::Result<()> {
    let mut app = AppState::from_root_config(Path::new("termquill.toml"), &test_config());
    app.set_terminal_size(80, 12);
    for character in "visible text".chars() {
        app.handle_key_event(KeyEvent::new(KeyCode::Char(character), KeyModifiers::NONE))?;
    }
    let backend = TestBackend::new(80, 12);
    let mut terminal = Terminal::new(backend)?;

    terminal.draw(|frame| render(frame, &app))?;

    let rendered = terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .map(|cell| cell.symbol())
        .collect::<String>();
    assert!(rendered.contains("visible text"));
    Ok(())
}

#[test]
fn render_main_screen_shows_esc_interrupt_for_running_turn() -> anyhow::Result<()> {
    let mut app = AppState::from_root_config(Path::new("termquill.toml"), &test_config());
    app.handle_key_event(KeyEvent::new(KeyCode::Char('h'), KeyModifiers::NONE))?;
    let _ = app.submit_input()?;
    let backend = TestBackend::new(120, 24);
    let mut terminal = Terminal::new(backend)?;

    terminal.draw(|frame| render(frame, &app))?;

    let rendered = terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .map(|cell| cell.symbol())
        .collect::<String>();
    assert!(rendered.contains("Esc interrupt"));
    assert!(rendered.contains("reasoning with deepseek-v4-flash"));
    Ok(())
}

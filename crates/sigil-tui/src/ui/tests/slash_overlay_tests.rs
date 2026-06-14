use std::{collections::BTreeMap, path::Path};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{Terminal, backend::TestBackend, layout::Rect};
use sigil_kernel::{
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
            log_dir: ".sigil/sessions".to_owned(),
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
        terminal: Default::default(),
        task: Default::default(),
        providers: BTreeMap::new(),
        mcp_servers: Vec::new(),
    }
}

#[test]
fn slash_selector_overlay_rect_tracks_composer_width() {
    let live = Rect::new(0, 0, 120, 24);
    let composer = Rect::new(0, 20, 120, 4);

    assert_eq!(
        slash_selector_overlay_rect(live, composer, 6),
        Some(Rect::new(1, 14, 118, 6))
    );
}

#[test]
fn render_slash_selector_overlay_shows_empty_message() -> anyhow::Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.handle_key_event(KeyEvent::new(KeyCode::Char('/'), KeyModifiers::NONE))?;
    app.handle_key_event(KeyEvent::new(KeyCode::Char('z'), KeyModifiers::NONE))?;
    app.handle_key_event(KeyEvent::new(KeyCode::Char('z'), KeyModifiers::NONE))?;
    let backend = TestBackend::new(96, 24);
    let mut terminal = Terminal::new(backend)?;

    terminal.draw(|frame| {
        render_slash_selector_overlay(
            frame,
            Rect::new(0, 0, 96, 18),
            Rect::new(0, 18, 96, 6),
            &app,
        )
    })?;

    let rendered = terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .map(|cell| cell.symbol())
        .collect::<String>();
    assert!(rendered.contains("no slash match"));
    Ok(())
}

#[test]
fn render_slash_selector_overlay_marks_selected_command() -> anyhow::Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.handle_key_event(KeyEvent::new(KeyCode::Char('/'), KeyModifiers::NONE))?;
    app.handle_key_event(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE))?;
    let backend = TestBackend::new(96, 24);
    let mut terminal = Terminal::new(backend)?;

    terminal.draw(|frame| {
        render_slash_selector_overlay(
            frame,
            Rect::new(0, 0, 96, 18),
            Rect::new(0, 18, 96, 6),
            &app,
        )
    })?;

    let rendered = terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .map(|cell| cell.symbol())
        .collect::<String>();
    assert!(rendered.contains("› "));
    assert!(rendered.contains("resume"));
    Ok(())
}

#[test]
fn render_slash_selector_overlay_keeps_title_when_no_candidate_space() -> anyhow::Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.handle_key_event(KeyEvent::new(KeyCode::Char('/'), KeyModifiers::NONE))?;
    app.handle_key_event(KeyEvent::new(KeyCode::Char('r'), KeyModifiers::NONE))?;
    app.handle_key_event(KeyEvent::new(KeyCode::Char('e'), KeyModifiers::NONE))?;
    app.handle_key_event(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::NONE))?;
    app.handle_key_event(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::NONE))?;
    app.handle_key_event(KeyEvent::new(KeyCode::Char('m'), KeyModifiers::NONE))?;
    app.handle_key_event(KeyEvent::new(KeyCode::Char('e'), KeyModifiers::NONE))?;
    app.set_terminal_size(80, 8);

    let backend = TestBackend::new(80, 8);
    let mut terminal = Terminal::new(backend)?;

    terminal.draw(|frame| {
        render_slash_selector_overlay(frame, Rect::new(0, 0, 80, 8), Rect::new(0, 6, 76, 2), &app)
    })?;

    let rendered = terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .map(|cell| cell.symbol())
        .collect::<String>();

    assert!(rendered.contains("Resume session"));
    assert!(!rendered.contains("up-to-date"));
    Ok(())
}

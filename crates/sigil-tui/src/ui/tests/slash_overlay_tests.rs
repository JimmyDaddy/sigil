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
                sigil_kernel::ConnectionId::new("deepseek-default").expect("valid test connection"),
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
                "credential": {"source": "environment", "name": "SIGIL_API_KEY"}
            }),
        )]),
        web: Default::default(),
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
    assert_eq!(app.slash_selector_selected_index(), Some(1));
    let rows = app.slash_selector_rows();
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
    assert!(rendered.contains(&format!("› {}", rows[1].0)));
    assert!(rendered.contains(&rows[0].0));
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

#[test]
fn resume_selector_shows_typed_legacy_navigation_without_hiding_current_candidates()
-> anyhow::Result<()> {
    use sigil_runtime::{LegacySharedSessionHistory, LegacySharedSessionHistorySourceState};

    for state in [
        LegacySharedSessionHistorySourceState::Present,
        LegacySharedSessionHistorySourceState::Unverified,
    ] {
        let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
        app.set_terminal_size(140, 24);
        app.composer.input = "/resume".to_owned();
        app.session_browser.history.clear();
        app.session_browser.legacy_shared_history = Some(LegacySharedSessionHistory {
            workspace_id: "original-workspace-id".to_owned(),
            workspace_label: "原工作区".to_owned(),
            source_state: state,
        });
        assert_eq!(app.slash_selector_notice_lines().len(), 2);
        let mut terminal = Terminal::new(TestBackend::new(140, 24))?;
        terminal.draw(|frame| {
            render_slash_selector_overlay(
                frame,
                Rect::new(0, 0, 140, 18),
                Rect::new(0, 18, 140, 6),
                &app,
            )
        })?;
        let rendered = rendered_terminal_text(&terminal);
        assert!(
            rendered.contains("原工作区"),
            "rendered buffer: {rendered:?}"
        );
        assert!(rendered.contains("no saved sessions"));
        assert!(!rendered.contains("original-workspace-id"));
        match state {
            LegacySharedSessionHistorySourceState::Present => {
                assert!(rendered.contains("打开该工作区查看"))
            }
            LegacySharedSessionHistorySourceState::Unverified => {
                assert!(rendered.contains("待核实"))
            }
        }
        app.composer.input = "/model".to_owned();
        assert!(app.slash_selector_notice_lines().is_empty());
        app.composer.input = "/resume".to_owned();
        app.session_browser.legacy_shared_history = None;
        assert!(app.slash_selector_notice_lines().is_empty());
    }
    Ok(())
}

#[test]
fn legacy_selector_notice_preserves_one_candidate_row_on_short_terminals() {
    assert_eq!(slash_selector_header_row_count(4, true, 2), 3);
    assert_eq!(slash_selector_header_row_count(3, true, 2), 2);
    assert_eq!(slash_selector_header_row_count(2, true, 2), 1);
    assert_eq!(slash_selector_header_row_count(1, true, 2), 1);
    assert_eq!(slash_selector_header_row_count(0, true, 2), 0);
}

fn rendered_terminal_text(terminal: &Terminal<TestBackend>) -> String {
    let buffer = terminal.backend().buffer();
    let mut text = String::new();
    for row in buffer.area.y..buffer.area.bottom() {
        let mut column = buffer.area.x;
        while column < buffer.area.right() {
            let cell = buffer.cell((column, row)).expect("terminal cell in bounds");
            text.push_str(cell.symbol());
            // A wide grapheme occupies its following buffer cells; those placeholders
            // are not literal spaces inserted into the visible text.
            column =
                column.saturating_add(crate::ui::terminal_cell_width(cell.symbol()).max(1) as u16);
        }
        text.push('\n');
    }
    text
}

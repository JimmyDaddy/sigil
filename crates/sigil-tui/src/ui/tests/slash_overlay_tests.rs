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
fn task_availability_hint_is_visible_in_an_eighty_column_palette() -> anyhow::Result<()> {
    let root = tempfile::tempdir()?;
    let mut config = test_config();
    config.workspace.root = root.path().display().to_string();
    config.composition = sigil_kernel::RuntimeCompositionConfig::core();
    let mut app = AppState::from_root_config(&root.path().join("sigil.toml"), &config);
    app.set_terminal_size(80, 24);
    app.handle_paste_text("/task");
    let surface = crate::surface_adapter::build_surface_model(
        Rect::new(0, 0, 80, 24),
        &app,
        crate::surface::SurfaceState {
            frame_generation: 1,
            terminal_epoch: 1,
        },
    );
    let mut terminal = Terminal::new(TestBackend::new(80, 24))?;

    terminal.draw(|frame| crate::ui::render_surface(frame, &surface))?;

    let rendered = terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .map(|cell| cell.symbol())
        .collect::<String>();
    assert!(rendered.contains("/task"));
    assert!(rendered.contains("Task unavailable · see sigil.toml"));
    assert!(!rendered.contains("start or continue a durable task"));
    Ok(())
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

#[test]
fn model_picker_shows_each_action_once_with_fill_and_switch_request_semantics() -> anyhow::Result<()>
{
    let _env_guard = crate::test_env::lock();
    let _key = crate::test_env::EnvScope::set("SIGIL_API_KEY", "model-picker-test-key");
    let root = tempfile::tempdir()?;
    let mut config = test_config();
    config.workspace.root = root.path().display().to_string();
    let mut app = AppState::from_root_config(&root.path().join("sigil.toml"), &config);
    app.set_terminal_size(80, 24);
    app.handle_paste_text("/model");
    let surface = crate::surface_adapter::build_surface_model(
        Rect::new(0, 0, 80, 24),
        &app,
        crate::surface::SurfaceState {
            frame_generation: 1,
            terminal_epoch: 1,
        },
    );
    let mut terminal = Terminal::new(TestBackend::new(80, 24))?;
    terminal.draw(|frame| crate::ui::render_surface(frame, &surface))?;
    let text = terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .map(|cell| cell.symbol())
        .collect::<String>();
    assert_eq!(text.matches("D saves default").count(), 1);
    assert!(text.contains("current session unchanged"));
    assert!(surface.footer.hints.contains("Tab fill"));
    assert!(surface.footer.hints.contains("Enter request switch"));
    assert!(!text.contains("Enter choose") && !surface.footer.hints.contains("Enter run"));
    Ok(())
}

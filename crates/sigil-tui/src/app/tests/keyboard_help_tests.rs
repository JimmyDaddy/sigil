use anyhow::Result;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::{
    app::{AppAction, AppState, TimelineRole, tests::common::test_config},
    commands::{
        COMMAND_SPECS, KeyboardHelpPage, UiCommand, keyboard_help_lines, metadata_slash_commands,
        metadata_slash_help_lines,
    },
    slash::SLASH_COMMANDS,
};

fn app_fixture() -> Result<(tempfile::TempDir, AppState)> {
    let root = tempfile::tempdir()?;
    let mut config = test_config();
    config.workspace.root = root.path().display().to_string();
    let app = AppState::from_root_config(&root.path().join("sigil.toml"), &config);
    Ok((root, app))
}

fn press(app: &mut AppState, code: KeyCode) -> Result<Option<AppAction>> {
    app.handle_key_event(KeyEvent::new(code, KeyModifiers::NONE))
}

#[test]
fn keyboard_help_common_uses_command_metadata_and_all_retains_the_full_directory() -> Result<()> {
    let (_root, mut app) = app_fixture()?;
    app.set_terminal_size(80, 24);
    app.push_timeline(TimelineRole::Tool, r#"{"tool_name":"ls","status":"ok","preview_kind":"json","summary":"one item","preview_lines":["done"],"preview_value":["done"],"hidden_lines":0}"#);
    press(&mut app, KeyCode::F(1))?;
    let common = app.modal_lines();
    assert!(common.len() <= 16);
    assert_eq!(
        app.keyboard_help_view().expect("help view").page,
        KeyboardHelpPage::Common
    );
    for command in [
        UiCommand::SubmitPrompt,
        UiCommand::CopyTranscript,
        UiCommand::ToggleThinking,
        UiCommand::ToggleWriteMode,
        UiCommand::CancelOrQuit,
    ] {
        let key = COMMAND_SPECS
            .iter()
            .find(|spec| spec.command == command)
            .expect("command metadata")
            .keys[0]
            .label;
        assert!(
            common
                .iter()
                .any(|line| line.starts_with(&format!("{key}:")))
        );
    }
    assert!(!common.iter().any(|line| line.starts_with("/compact:")));
    press(&mut app, KeyCode::Tab)?;
    let all = app.modal_lines();
    assert_eq!(
        app.keyboard_help_view().expect("help view").page,
        KeyboardHelpPage::All
    );
    for line in keyboard_help_lines(true) {
        assert!(all.contains(&line), "missing full keyboard help: {line}");
    }
    for line in metadata_slash_help_lines(None) {
        assert!(all.contains(&line), "missing metadata slash help: {line}");
    }
    let metadata = metadata_slash_commands().collect::<Vec<_>>();
    for spec in SLASH_COMMANDS
        .iter()
        .filter(|spec| !metadata.contains(&spec.canonical))
    {
        assert!(
            all.iter()
                .any(|line| line.starts_with(&format!("{}:", spec.canonical))),
            "missing slash command {}",
            spec.canonical
        );
    }
    app.handle_key_event(KeyEvent::new(KeyCode::BackTab, KeyModifiers::SHIFT))?;
    assert_eq!(app.modal_lines(), common);
    Ok(())
}

#[test]
fn task_availability_palette_and_full_help_share_the_current_session_hint() -> Result<()> {
    let root = tempfile::tempdir()?;
    let mut config = test_config();
    config.workspace.root = root.path().display().to_string();
    config.composition = sigil_kernel::RuntimeCompositionConfig::core();
    let mut app = AppState::from_root_config(&root.path().join("sigil.toml"), &config);
    app.set_terminal_size(80, 24);
    app.composer.input = "/task".to_owned();
    let rows = app.slash_selector_rows();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].0, "/task");
    assert_eq!(rows[0].1, "Task unavailable · see sigil.toml");
    press(&mut app, KeyCode::F(1))?;
    press(&mut app, KeyCode::Tab)?;
    let help = app.modal_lines();
    assert!(help.contains(&format!("/task: {}", rows[0].1)));
    assert!(
        !help
            .iter()
            .any(|line| line == "/task: Start or continue a durable multi-step task.")
    );
    assert!(help.iter().any(|line| line.starts_with("/new:")));
    assert!(help.iter().any(|line| line.starts_with("/config:")));
    press(&mut app, KeyCode::Esc)?;
    assert_eq!(app.composer.input, "/task");
    Ok(())
}

#[test]
fn keyboard_help_navigation_preserves_draft_cursor_and_underlying_scroll() -> Result<()> {
    let (_root, mut app) = app_fixture()?;
    app.set_terminal_size(80, 24);
    app.composer.input = "/config draft 中文".to_owned();
    app.composer.input_cursor = 3;
    app.timeline_scroll_back = 7;
    let permission_mode = app.runtime.permission_mode.clone();
    press(&mut app, KeyCode::F(1))?;
    for code in [
        KeyCode::Tab,
        KeyCode::Down,
        KeyCode::PageDown,
        KeyCode::End,
        KeyCode::Up,
        KeyCode::PageUp,
        KeyCode::Home,
        KeyCode::BackTab,
    ] {
        assert!(press(&mut app, code)?.is_none());
    }
    assert_eq!(app.composer.input, "/config draft 中文");
    assert_eq!(app.composer.input_cursor, 3);
    assert_eq!(app.timeline_scroll_back, 7);
    assert_eq!(app.runtime.permission_mode, permission_mode);
    assert!(!app.should_quit);
    assert!(press(&mut app, KeyCode::Enter)?.is_none());
    assert!(!app.has_modal());
    assert_eq!(app.composer.input, "/config draft 中文");
    Ok(())
}

#[test]
fn keyboard_help_scroll_is_bounded_by_visual_rows_and_reclamped_after_resize() -> Result<()> {
    let (_root, mut app) = app_fixture()?;
    app.set_terminal_size(80, 24);
    press(&mut app, KeyCode::F(1))?;
    press(&mut app, KeyCode::Tab)?;
    let (max_scroll, page_rows) =
        crate::ui::keyboard_help_scroll_bounds(80, 24, &app.modal_lines());
    assert!(max_scroll > page_rows && page_rows > 0);
    press(&mut app, KeyCode::Down)?;
    assert_eq!(app.keyboard_help_view().expect("view").scroll_rows, 1);
    press(&mut app, KeyCode::PageDown)?;
    assert_eq!(
        app.keyboard_help_view().expect("view").scroll_rows,
        1 + page_rows
    );
    press(&mut app, KeyCode::End)?;
    press(&mut app, KeyCode::Down)?;
    assert_eq!(
        app.keyboard_help_view().expect("view").scroll_rows,
        max_scroll
    );
    app.set_terminal_size(144, 44);
    let (wide_max, _) = crate::ui::keyboard_help_scroll_bounds(144, 44, &app.modal_lines());
    assert!(wide_max < max_scroll);
    assert_eq!(
        app.keyboard_help_view().expect("view").scroll_rows,
        wide_max
    );
    press(&mut app, KeyCode::Up)?;
    assert_eq!(
        app.keyboard_help_view().expect("view").scroll_rows,
        wide_max.saturating_sub(1)
    );
    press(&mut app, KeyCode::Home)?;
    press(&mut app, KeyCode::Up)?;
    assert_eq!(app.keyboard_help_view().expect("view").scroll_rows, 0);
    Ok(())
}

#[test]
fn keyboard_help_busy_escape_and_enter_dismiss_without_cancelling_or_sending() -> Result<()> {
    for close in [KeyCode::Esc, KeyCode::Enter] {
        let (_root, mut app) = app_fixture()?;
        app.runtime.is_busy = true;
        app.composer.input = "unsent follow-up".to_owned();
        press(&mut app, KeyCode::F(1))?;
        assert!(press(&mut app, close)?.is_none());
        assert!(!app.has_modal());
        assert!(app.runtime.is_busy);
        assert_eq!(app.composer.input, "unsent follow-up");
        assert!(
            !app.timeline
                .iter()
                .any(|entry| entry.text.contains("cancel requested"))
        );
    }
    Ok(())
}

#[test]
fn keyboard_help_control_c_keeps_the_existing_cancel_and_idle_quit_semantics() -> Result<()> {
    let (_busy_root, mut busy) = app_fixture()?;
    busy.runtime.is_busy = true;
    press(&mut busy, KeyCode::F(1))?;
    assert!(matches!(
        busy.handle_key_event(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL))?,
        Some(AppAction::CancelRun)
    ));
    assert!(!busy.has_modal());
    assert!(!busy.should_quit);
    let (_idle_root, mut idle) = app_fixture()?;
    press(&mut idle, KeyCode::F(1))?;
    idle.handle_key_event(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL))?;
    assert!(idle.should_quit);
    Ok(())
}

#[test]
fn keyboard_help_common_literal_hints_match_the_existing_input_and_scroll_keys() -> Result<()> {
    let (_root, mut app) = app_fixture()?;
    app.set_terminal_size(80, 24);
    app.set_input_and_cursor("draft".to_owned());
    app.handle_key_event(KeyEvent::new(KeyCode::Char('j'), KeyModifiers::CONTROL))?;
    assert_eq!(app.composer.input, "draft\n");
    app.composer.input_history = vec!["previous prompt".to_owned()];
    app.set_input_and_cursor(String::new());
    press(&mut app, KeyCode::Up)?;
    assert_eq!(app.composer.input, "previous prompt");
    press(&mut app, KeyCode::Down)?;
    assert_eq!(app.composer.input, "");
    press(&mut app, KeyCode::Char('/'))?;
    assert!(app.has_slash_selector());
    app.set_input_and_cursor(String::new());
    for index in 0..60 {
        app.push_timeline(TimelineRole::Assistant, format!("Reply {index}"));
    }
    press(&mut app, KeyCode::PageUp)?;
    let scroll = app.timeline_scroll_back;
    assert!(scroll > 0);
    press(&mut app, KeyCode::PageDown)?;
    assert!(app.timeline_scroll_back < scroll);
    Ok(())
}

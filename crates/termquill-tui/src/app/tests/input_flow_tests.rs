use super::*;

#[test]
fn cjk_input_cursor_visual_position_uses_display_width() {
    let mut app = AppState::from_root_config(Path::new("termquill.toml"), &test_config());
    app.set_terminal_size(40, 12);
    app.set_input_and_cursor("你好".to_owned());

    assert_eq!(app.input_cursor_visual_position(), (4, 0));
}

#[test]
fn shift_enter_inserts_newline_without_submitting() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("termquill.toml"), &test_config());
    app.input = "hello".to_owned();
    app.input_cursor = app.input.chars().count();
    let timeline_len = app.timeline.len();

    let action = app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::SHIFT))?;

    assert!(action.is_none());
    assert_eq!(app.input, "hello\n");
    assert_eq!(app.timeline.len(), timeline_len);
    assert_eq!(app.composer_input_rows(), 2);
    Ok(())
}

#[test]
fn composer_up_down_scroll_transcript_when_input_is_empty() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("termquill.toml"), &test_config());
    app.set_terminal_size(80, 12);
    for index in 0..8 {
        app.push_timeline(TimelineRole::Assistant, format!("message {index}"));
    }
    app.input.clear();
    app.input_cursor = 0;

    app.handle_key_event(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE))?;
    assert!(app.timeline_scroll_back > 0);

    app.handle_key_event(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE))?;
    assert_eq!(app.timeline_scroll_back, 0);
    Ok(())
}

#[test]
fn ctrl_p_and_ctrl_n_navigate_prompt_history() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("termquill.toml"), &test_config());
    app.input_history = vec!["first".to_owned(), "second".to_owned()];
    app.input.clear();
    app.input_cursor = 0;

    app.handle_key_event(KeyEvent::new(KeyCode::Char('p'), KeyModifiers::CONTROL))?;
    assert_eq!(app.input, "second");

    app.handle_key_event(KeyEvent::new(KeyCode::Char('n'), KeyModifiers::CONTROL))?;
    assert!(app.input.is_empty());
    Ok(())
}

#[test]
fn composer_up_down_navigates_input_history() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("termquill.toml"), &test_config());
    app.input = "first".to_owned();
    assert!(matches!(
        app.submit_input()?,
        Some(AppAction::SubmitPrompt(prompt)) if prompt == "first"
    ));
    app.is_busy = false;

    app.input = "second".to_owned();
    assert!(matches!(
        app.submit_input()?,
        Some(AppAction::SubmitPrompt(prompt)) if prompt == "second"
    ));
    app.is_busy = false;

    app.input = "draft".to_owned();
    app.active_pane = PaneFocus::Composer;
    app.handle_key_event(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE))?;
    assert_eq!(app.input, "second");
    app.handle_key_event(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE))?;
    assert_eq!(app.input, "first");
    app.handle_key_event(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE))?;
    assert_eq!(app.input, "second");
    app.handle_key_event(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE))?;
    assert_eq!(app.input, "draft");
    Ok(())
}

#[test]
fn composer_up_inside_wrapped_input_moves_cursor_before_history() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("termquill.toml"), &test_config());
    app.input = "first".to_owned();
    assert!(matches!(
        app.submit_input()?,
        Some(AppAction::SubmitPrompt(prompt)) if prompt == "first"
    ));
    app.is_busy = false;

    app.active_pane = PaneFocus::Composer;
    app.set_terminal_size(6, 20);
    app.input = "draft123456".to_owned();
    app.input_cursor = 7;

    app.handle_key_event(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE))?;

    assert_eq!(app.input, "draft123456");
    assert_eq!(app.input_cursor, 1);
    assert_eq!(app.input_history_index, None);
    Ok(())
}

#[test]
fn composer_down_at_bottom_row_navigates_history() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("termquill.toml"), &test_config());
    app.input = "first".to_owned();
    assert!(matches!(
        app.submit_input()?,
        Some(AppAction::SubmitPrompt(prompt)) if prompt == "first"
    ));
    app.is_busy = false;

    app.input = "second".to_owned();
    assert!(matches!(
        app.submit_input()?,
        Some(AppAction::SubmitPrompt(prompt)) if prompt == "second"
    ));
    app.is_busy = false;

    app.active_pane = PaneFocus::Composer;
    app.set_terminal_size(6, 20);
    app.input = "draft123".to_owned();
    app.input_cursor = 1;

    app.handle_key_event(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE))?;
    assert_eq!(app.input, "second");
    app.input_cursor = app.input.chars().count();

    app.handle_key_event(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE))?;
    assert_eq!(app.input, "draft123");
    Ok(())
}

use super::*;

#[test]
fn cjk_input_cursor_visual_position_uses_display_width() {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.set_terminal_size(40, 12);
    app.set_input_and_cursor("你好".to_owned());

    assert_eq!(app.input_cursor_visual_position(), (4, 0));
}

#[test]
fn shift_enter_inserts_newline_without_submitting() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
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
fn shifted_line_feed_key_inserts_newline_without_submitting() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.input = "hello".to_owned();
    app.input_cursor = app.input.chars().count();
    let timeline_len = app.timeline.len();

    let action = app.handle_key_event(KeyEvent::new(KeyCode::Char('\n'), KeyModifiers::SHIFT))?;

    assert!(action.is_none());
    assert_eq!(app.input, "hello\n");
    assert_eq!(app.timeline.len(), timeline_len);
    assert_eq!(app.composer_input_rows(), 2);
    Ok(())
}

#[test]
fn shifted_carriage_return_key_normalizes_to_newline() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.input = "hello".to_owned();
    app.input_cursor = app.input.chars().count();

    let action = app.handle_key_event(KeyEvent::new(KeyCode::Char('\r'), KeyModifiers::SHIFT))?;

    assert!(action.is_none());
    assert_eq!(app.input, "hello\n");
    assert_eq!(app.composer_input_rows(), 2);
    Ok(())
}

#[test]
fn composer_ignores_non_printing_control_characters() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    let action =
        app.handle_key_event(KeyEvent::new(KeyCode::Char('\u{1b}'), KeyModifiers::NONE))?;

    assert!(action.is_none());
    assert!(app.input.is_empty());
    assert_eq!(app.input_cursor_visual_position(), (0, 0));
    Ok(())
}

#[test]
fn carriage_return_key_submits_instead_of_entering_invisible_text() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.input = "hello".to_owned();
    app.input_cursor = app.input.chars().count();

    let action = app.handle_key_event(KeyEvent::new(KeyCode::Char('\r'), KeyModifiers::NONE))?;

    assert!(matches!(
        action,
        Some(AppAction::SubmitPrompt(prompt)) if prompt == "hello"
    ));
    Ok(())
}

#[test]
fn composer_up_down_navigates_history_when_input_is_empty_without_scrolling() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.set_terminal_size(80, 12);
    for index in 0..8 {
        app.push_timeline(TimelineRole::Assistant, format!("message {index}"));
    }
    app.input_history = vec!["first".to_owned(), "second".to_owned()];
    app.input.clear();
    app.input_cursor = 0;

    app.handle_key_event(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE))?;
    assert_eq!(app.input, "second");
    assert_eq!(app.timeline_scroll_back, 0);

    app.handle_key_event(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE))?;
    assert!(app.input.is_empty());
    assert_eq!(app.timeline_scroll_back, 0);
    Ok(())
}

#[test]
fn composer_up_down_without_history_do_not_scroll_transcript() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.set_terminal_size(80, 12);
    for index in 0..8 {
        app.push_timeline(TimelineRole::Assistant, format!("message {index}"));
    }
    app.input.clear();
    app.input_cursor = 0;

    app.handle_key_event(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE))?;
    app.handle_key_event(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE))?;

    assert!(app.input.is_empty());
    assert_eq!(app.timeline_scroll_back, 0);
    Ok(())
}

#[test]
fn ctrl_p_and_ctrl_n_navigate_prompt_history() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
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
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
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
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.input = "first".to_owned();
    assert!(matches!(
        app.submit_input()?,
        Some(AppAction::SubmitPrompt(prompt)) if prompt == "first"
    ));
    app.is_busy = false;

    app.active_pane = PaneFocus::Composer;
    app.set_terminal_size(96, 20);
    app.input = "draft".repeat(20);
    app.input_cursor = 70;
    assert!(app.input_cursor_visual_position().1 > 0);

    app.handle_key_event(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE))?;

    assert_eq!(app.input, "draft".repeat(20));
    assert_eq!(app.input_cursor_visual_position().1, 0);
    assert_eq!(app.input_history_index, None);
    Ok(())
}

#[test]
fn composer_down_at_bottom_row_navigates_history() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
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

#[test]
fn busy_submit_keeps_existing_input_and_emits_notice() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.is_busy = true;
    app.input = "queued".to_owned();

    let action = app.submit_input()?;

    assert!(action.is_none());
    assert_eq!(app.input, "queued");
    assert!(
        app.timeline
            .iter()
            .any(|entry| entry.role == TimelineRole::Notice && entry.text == "busy; submit later")
    );
    assert!(
        app.events
            .iter()
            .any(|event| event.label == "notice" && event.detail == "submit ignored while busy")
    );
    Ok(())
}

#[test]
fn input_history_is_capped_at_one_hundred_entries() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());

    for index in 0..101 {
        app.input = format!("prompt {index}");
        assert!(matches!(
            app.submit_input()?,
            Some(AppAction::SubmitPrompt(prompt)) if prompt == format!("prompt {index}")
        ));
        app.is_busy = false;
    }

    assert_eq!(app.input_history.len(), 100);
    assert_eq!(
        app.input_history.first().map(String::as_str),
        Some("prompt 1")
    );
    assert_eq!(
        app.input_history.last().map(String::as_str),
        Some("prompt 100")
    );
    Ok(())
}

#[test]
fn input_helpers_edit_and_navigate_multiline_text() {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.set_terminal_size(80, 12);
    app.set_input_and_cursor("ab\ncd".to_owned());

    assert_eq!(app.input_char_len(), 5);
    assert_eq!(app.composer_input_rows(), 2);
    assert_eq!(app.composer_height(), 6);
    assert_eq!(app.visual_position_for_cursor(5, 4), (1, 2));
    assert_eq!(app.cursor_for_visual_position(1, 1, 4), 4);

    app.input_cursor = usize::MAX;
    app.clamp_input_cursor();
    assert_eq!(app.input_cursor, 5);

    app.move_input_cursor_home();
    assert_eq!(app.input_cursor, 0);
    assert!(!app.move_input_cursor_vertical(true));

    app.remove_input_character_before_cursor();
    assert_eq!(app.input, "ab\ncd");

    app.move_input_cursor_right();
    app.insert_input_character('X');
    assert_eq!(app.input, "aXb\ncd");
    assert_eq!(app.input_cursor, 2);

    app.remove_input_character_before_cursor();
    assert_eq!(app.input, "ab\ncd");

    app.move_input_cursor_end();
    assert_eq!(app.input_cursor_visual_row(), 1);
    assert!(app.move_input_cursor_vertical(true));
    assert_eq!(app.input_cursor, 2);
    assert!(app.move_input_cursor_vertical(false));
    assert_eq!(app.input_cursor, 5);
    assert!(!app.move_input_cursor_vertical(false));

    app.move_input_cursor_left();
    app.move_input_cursor_left();
    assert_eq!(app.input_cursor, 3);
    app.move_input_cursor_home();
    app.move_input_cursor_left();
    assert_eq!(app.input_cursor, 0);
    app.move_input_cursor_end();
    app.move_input_cursor_right();
    assert_eq!(app.input_cursor, app.input_char_len());
}

#[test]
fn input_history_recording_deduplicates_caps_and_restores_draft() {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    for index in 0..=100 {
        app.record_input_history(format!("prompt-{index}"));
    }
    assert_eq!(app.input_history.len(), 100);
    assert_eq!(
        app.input_history.first().map(String::as_str),
        Some("prompt-1")
    );

    app.record_input_history("prompt-100".to_owned());
    assert_eq!(app.input_history.len(), 100);

    app.input = "draft".to_owned();
    app.navigate_input_history(true);
    assert_eq!(app.input, "prompt-100");

    for _ in 0..200 {
        app.navigate_input_history(true);
    }
    assert_eq!(app.input, "prompt-1");
    assert_eq!(app.input_history_index, Some(0));

    app.navigate_input_history(true);
    assert_eq!(app.input, "prompt-1");

    for _ in 0..200 {
        app.navigate_input_history(false);
    }
    assert_eq!(app.input, "draft");
    assert_eq!(app.input_history_index, None);
    assert_eq!(app.input_history_draft, None);
}

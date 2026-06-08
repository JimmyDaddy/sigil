use super::*;

#[test]
fn tool_card_shortcuts_focus_and_toggle_one_card() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("termquill.toml"), &test_config());
    app.push_timeline(
        TimelineRole::Tool,
        r##"{
  "tool_name": "ls",
  "status": "ok",
  "preview_kind": "json",
  "summary": "first 1/1 lines · 8 B",
  "preview_lines": ["[\".git\"]"],
  "preview_value": [".git"],
  "hidden_lines": 0
}"##,
    );
    app.push_timeline(
        TimelineRole::Tool,
        r##"{
  "tool_name": "ls",
  "status": "ok",
  "preview_kind": "json",
  "summary": "first 1/1 lines · 11 B",
  "preview_lines": ["[\"src/lib.rs\"]"],
  "preview_value": ["src/lib.rs"],
  "hidden_lines": 0
}"##,
    );
    let tool_indices = app
        .tool_timeline_entry_indices()
        .expect("test app should contain two tool timeline entries");
    let first_tool = tool_indices[0];
    let second_tool = tool_indices[1];

    assert_eq!(app.selected_tool_timeline_entry, Some(second_tool));

    app.selected_tool_timeline_entry = None;
    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Char('g'), KeyModifiers::CONTROL))?;
    assert_eq!(app.selected_tool_timeline_entry, Some(second_tool));

    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Char('k'), KeyModifiers::ALT))?;
    assert_eq!(app.selected_tool_timeline_entry, Some(first_tool));

    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Char('j'), KeyModifiers::ALT))?;
    assert_eq!(app.selected_tool_timeline_entry, Some(second_tool));

    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Char('k'), KeyModifiers::ALT))?;
    assert_eq!(app.selected_tool_timeline_entry, Some(first_tool));

    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Char('o'), KeyModifiers::CONTROL))?;
    assert!(app.expanded_tool_timeline_entries.contains(&first_tool));
    assert!(!app.expanded_tool_timeline_entries.contains(&second_tool));

    let lines = app.transcript_lines(40);
    assert!(lines.iter().any(|line| {
        line.spans
            .iter()
            .any(|span| span.content.as_ref().contains(".git"))
    }));
    assert!(lines.iter().any(|line| {
        line.spans
            .iter()
            .any(|span| span.content.as_ref().contains("hidden"))
    }));

    app.input = "draft".to_owned();
    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Char('j'), KeyModifiers::ALT))?;
    assert_eq!(app.input, "draft");
    assert_eq!(app.selected_tool_timeline_entry, Some(first_tool));

    app.input.clear();
    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE))?;
    assert_eq!(app.selected_tool_timeline_entry, None);
    assert_eq!(app.last_notice(), Some("tool focus cleared"));
    Ok(())
}

use super::*;

#[test]
fn tool_card_shortcuts_focus_and_toggle_one_card() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("termquill.toml"), &test_config());
    app.push_timeline(
        TimelineRole::Tool,
        r##"{
  "call_id": "call-first",
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
  "call_id": "call-second",
  "tool_name": "ls",
  "status": "ok",
  "preview_kind": "json",
  "summary": "first 1/1 lines · 11 B",
  "preview_lines": ["[\"src/lib.rs\"]"],
  "preview_value": ["src/lib.rs"],
  "hidden_lines": 0
}"##,
    );
    assert_eq!(
        app.tool_timeline_entry_indices()
            .expect("test app should contain two tool timeline entries")
            .len(),
        2
    );
    let first_key = "call:call-first".to_owned();
    let second_key = "call:call-second".to_owned();

    assert_eq!(app.selected_tool_activity_key, Some(second_key.clone()));

    app.selected_tool_activity_key = None;
    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Char('g'), KeyModifiers::CONTROL))?;
    assert_eq!(app.selected_tool_activity_key, Some(second_key.clone()));

    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Char('k'), KeyModifiers::ALT))?;
    assert_eq!(app.selected_tool_activity_key, Some(first_key.clone()));

    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Char('j'), KeyModifiers::ALT))?;
    assert_eq!(app.selected_tool_activity_key, Some(second_key.clone()));

    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Char('k'), KeyModifiers::ALT))?;
    assert_eq!(app.selected_tool_activity_key, Some(first_key.clone()));

    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Char('o'), KeyModifiers::CONTROL))?;
    assert!(app.expanded_tool_activity_keys.contains(&first_key));
    assert!(!app.expanded_tool_activity_keys.contains(&second_key));

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
    assert_eq!(app.selected_tool_activity_key, Some(first_key.clone()));

    app.input.clear();
    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE))?;
    assert_eq!(app.selected_tool_activity_key, None);
    assert_eq!(app.last_notice(), Some("tool focus cleared"));
    Ok(())
}

#[test]
fn file_diff_tool_card_defaults_open_and_can_toggle_closed() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("termquill.toml"), &test_config());
    app.push_timeline(
        TimelineRole::Tool,
        r#"{
  "call_id": "call-diff",
  "tool_name": "write_file",
  "status": "ok",
  "summary": "1 line · 14 B · diff +1 -1 · 1 file",
  "metadata": {"changed_files": ["note.txt"]},
  "preview_kind": "text",
  "preview_lines": ["wrote note.txt"],
  "hidden_lines": 0,
  "diff": {
    "summary": "+1 -1 · 1 file",
    "truncated": false,
    "original_line_count": 5,
    "rendered_line_count": 5,
    "files": [{
      "path": "note.txt",
      "lines": ["--- current/note.txt", "+++ proposed/note.txt", "@@ -1 +1 @@", "-old", "+new"],
      "truncated": false,
      "original_line_count": 5,
      "rendered_line_count": 5
    }]
  }
}"#,
    );
    assert!(app.tool_timeline_entry_indices().is_some());
    let tool_key = "call:call-diff".to_owned();

    assert!(!app.expanded_tool_activity_keys.contains(&tool_key));
    assert!(!app.collapsed_tool_activity_keys.contains(&tool_key));
    assert!(app.tool_card_status_line().contains("open"));
    let default_open = app.transcript_lines(40);
    assert!(default_open.iter().any(|line| {
        line.spans
            .iter()
            .any(|span| span.content.as_ref().contains("--- current/note.txt"))
    }));

    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Char('o'), KeyModifiers::CONTROL))?;

    assert!(app.collapsed_tool_activity_keys.contains(&tool_key));
    assert!(app.tool_card_status_line().contains("brief"));
    let collapsed = app.transcript_lines(40);
    assert!(collapsed.iter().any(|line| {
        line.spans
            .iter()
            .any(|span| span.content.as_ref().contains("diff hidden"))
    }));
    assert!(!collapsed.iter().any(|line| {
        line.spans
            .iter()
            .any(|span| span.content.as_ref().contains("--- current/note.txt"))
    }));

    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Char('o'), KeyModifiers::CONTROL))?;

    assert!(!app.collapsed_tool_activity_keys.contains(&tool_key));
    assert!(app.tool_card_status_line().contains("open"));
    Ok(())
}

#[test]
fn appending_tool_card_moves_focus_marker_to_latest_card() {
    let mut app = AppState::from_root_config(Path::new("termquill.toml"), &test_config());
    app.push_timeline(
        TimelineRole::Tool,
        r#"{
  "tool_name": "write_file",
  "status": "ok",
  "summary": "1 line · 9 B",
  "metadata": {
    "changed_files": ["first.txt"],
    "details": {"call": {"summary": "path=first.txt"}}
  },
  "preview_kind": "text",
  "preview_lines": ["wrote first.txt"],
  "hidden_lines": 0
}"#,
    );
    app.push_timeline(
        TimelineRole::Tool,
        r#"{
  "tool_name": "write_file",
  "status": "ok",
  "summary": "1 line · 10 B",
  "metadata": {
    "changed_files": ["second.txt"],
    "details": {"call": {"summary": "path=second.txt"}}
  },
  "preview_kind": "text",
  "preview_lines": ["wrote second.txt"],
  "hidden_lines": 0
}"#,
    );

    let rendered = app
        .transcript_lines(40)
        .into_iter()
        .map(|line| {
            line.spans
                .into_iter()
                .map(|span| span.content.into_owned())
                .collect::<String>()
        })
        .collect::<Vec<_>>();
    let focus_lines = rendered
        .iter()
        .filter(|line| line.contains("●"))
        .collect::<Vec<_>>();

    assert_eq!(focus_lines.len(), 1);
    assert!(focus_lines[0].contains("Wrote second.txt"));
    assert!(!focus_lines[0].contains("Wrote first.txt"));
    assert!(!rendered.iter().any(|line| line.contains("path=second.txt")));
}

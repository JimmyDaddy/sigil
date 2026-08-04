use super::*;
use crate::{
    app::modal_flow::{ModalOutcome, TextInputTarget},
    runner::ToolArtifactDisplayReadFailure,
};

fn full_plain_timeline(app: &AppState) -> String {
    app.timeline_plain_lines().join("\n")
}

fn tool_artifact_ref() -> sigil_kernel::ToolArtifactRefV1 {
    sigil_kernel::ToolArtifactRefV1 {
        artifact_id: format!("ta1_{}", "a".repeat(32)),
    }
}

fn tool_artifact_card(availability: &str, has_more: bool) -> String {
    serde_json::json!({
        "call_id": "call-artifact",
        "tool_name": "bash",
        "status": "ok",
        "summary": "saved tool output",
        "preview_kind": "text",
        "preview_lines": ["bounded preview"],
        "hidden_lines": 1,
        "artifact": {
            "artifact_ref": tool_artifact_ref(),
            "availability": availability,
            "observed_bytes": 131_072,
            "persisted_bytes": 65_536,
            "has_more": has_more,
            "next_selector": has_more.then(|| serde_json::json!({
                "kind": "line_page",
                "start_line": 0,
                "line_count": 200
            }))
        }
    })
    .to_string()
}

#[test]
fn tool_card_shortcuts_focus_and_toggle_one_card() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
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

    assert_eq!(
        app.timeline_state.selected_tool_activity_key,
        Some(second_key.clone())
    );

    app.timeline_state.selected_tool_activity_key = None;
    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Char('g'), KeyModifiers::CONTROL))?;
    assert_eq!(
        app.timeline_state.selected_tool_activity_key,
        Some(second_key.clone())
    );

    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Char('k'), KeyModifiers::ALT))?;
    assert_eq!(
        app.timeline_state.selected_tool_activity_key,
        Some(first_key.clone())
    );

    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Char('j'), KeyModifiers::ALT))?;
    assert_eq!(
        app.timeline_state.selected_tool_activity_key,
        Some(second_key.clone())
    );

    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Char('k'), KeyModifiers::ALT))?;
    assert_eq!(
        app.timeline_state.selected_tool_activity_key,
        Some(first_key.clone())
    );

    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Char('o'), KeyModifiers::CONTROL))?;
    assert!(
        app.timeline_state
            .expanded_tool_activity_keys
            .contains(&first_key)
    );
    assert!(
        !app.timeline_state
            .expanded_tool_activity_keys
            .contains(&second_key)
    );

    let lines = app.transcript_lines(40);
    assert!(lines.iter().any(|line| {
        line.spans
            .iter()
            .any(|span| span.content.as_ref().contains(".git"))
    }));
    assert!(lines.iter().any(|line| {
        line.spans
            .iter()
            .any(|span| span.content.as_ref().contains("src/lib.rs"))
    }));

    app.composer.input = "draft".to_owned();
    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Char('j'), KeyModifiers::ALT))?;
    assert_eq!(app.composer.input, "draft");
    assert_eq!(
        app.timeline_state.selected_tool_activity_key,
        Some(first_key.clone())
    );

    app.composer.input.clear();
    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE))?;
    assert_eq!(app.timeline_state.selected_tool_activity_key, None);
    assert_eq!(app.last_notice(), Some("activity focus cleared"));
    Ok(())
}

#[test]
fn live_tool_artifact_card_shows_truncation_and_bounded_actions() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    let mut result = ToolResult::ok(
        "call-artifact",
        "bash",
        "bounded preview",
        ToolResultMeta::default(),
    );
    result.metadata.details = serde_json::json!({
        "artifact_ref": tool_artifact_ref(),
        "observed_bytes": 131_072,
        "persisted_bytes": 65_536,
        "has_more": true,
        "display_capabilities": ["read_next_page", "search_literal"]
    });

    app.handle(RunEvent::ToolResult(result))?;

    let rendered = full_plain_timeline(&app);
    assert!(rendered.contains("Saved output truncated"));
    assert!(rendered.contains("64.0 KiB of 128.0 KiB"));
    assert!(rendered.contains("Alt-N next"));
    assert!(rendered.contains("Alt-F search"));
    Ok(())
}

#[test]
fn tool_artifact_next_page_and_literal_search_emit_typed_worker_commands() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.push_timeline(TimelineRole::Tool, tool_artifact_card("available", true));
    let _ = app.drain_pending_worker_commands();

    assert!(app.request_selected_tool_artifact_next_page());
    let commands = app.drain_pending_worker_commands();
    assert!(matches!(
        commands.as_slice(),
        [WorkerCommand::ReadToolArtifactPage {
            artifact_ref,
            selector: sigil_kernel::ToolArtifactSelectorV1::LinePage {
                start_line: 0,
                line_count: 200,
            },
            ..
        }] if artifact_ref == &tool_artifact_ref()
    ));

    assert!(app.open_selected_tool_artifact_search());
    assert_eq!(app.modal_title(), Some("Search Full Tool Output"));
    app.apply_modal_outcome(ModalOutcome::TextSubmitted {
        target: TextInputTarget::ToolArtifactSearch,
        value: "literal needle".to_owned(),
    });
    let commands = app.drain_pending_worker_commands();
    assert!(matches!(
        commands.as_slice(),
        [WorkerCommand::ReadToolArtifactPage {
            artifact_ref,
            selector: sigil_kernel::ToolArtifactSelectorV1::SearchLiteral {
                query,
                start_offset: 0,
                max_matches: 20,
                context_lines: 2,
            },
            ..
        }] if artifact_ref == &tool_artifact_ref() && query == "literal needle"
    ));
    Ok(())
}

#[test]
fn tool_artifact_page_response_advances_selector_and_replaces_preview() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.push_timeline(TimelineRole::Tool, tool_artifact_card("available", true));

    app.handle_worker_message(WorkerMessage::ToolArtifactPageRead {
        request_id: 7,
        page: sigil_kernel::ToolArtifactPageV1 {
            artifact_ref: tool_artifact_ref(),
            selector: sigil_kernel::ToolArtifactSelectorV1::LinePage {
                start_line: 0,
                line_count: 200,
            },
            body: "page line one\npage line two".to_owned(),
            body_encoding: sigil_kernel::ToolArtifactPageEncoding::Utf8,
            returned_bytes: 27,
            page_sha256: format!("sha256:{}", "b".repeat(64)),
            artifact_sha256: format!("sha256:{}", "c".repeat(64)),
            eof: false,
            match_count: 0,
            next_selector: Some(sigil_kernel::ToolArtifactSelectorV1::LinePage {
                start_line: 200,
                line_count: 200,
            }),
        },
        entries: Vec::new(),
    })?;

    let payload: serde_json::Value = serde_json::from_str(
        &app.timeline
            .iter()
            .find(|entry| entry.role == TimelineRole::Tool)
            .expect("tool card")
            .text,
    )?;
    assert_eq!(payload["preview_lines"][0], "page line one");
    assert_eq!(payload["preview_lines"][1], "page line two");
    assert_eq!(payload["artifact"]["next_selector"]["start_line"], 200);
    assert_eq!(payload["artifact"]["has_more"], true);
    assert!(full_plain_timeline(&app).contains("Alt-N next"));
    Ok(())
}

#[test]
fn unavailable_tool_artifact_keeps_auditable_summary_and_blocks_reads() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.push_timeline(TimelineRole::Tool, tool_artifact_card("available", true));
    let _ = app.drain_pending_worker_commands();

    app.handle_worker_message(WorkerMessage::ToolArtifactPageReadFailed {
        request_id: 9,
        artifact_ref: tool_artifact_ref(),
        failure: ToolArtifactDisplayReadFailure::Unavailable(
            sigil_kernel::ToolArtifactAvailability::Missing,
        ),
        entries: Vec::new(),
    })?;

    let rendered = full_plain_timeline(&app);
    assert!(rendered.contains("Full output unavailable (missing)"));
    assert!(rendered.contains("saved summary remains auditable"));
    assert!(!app.request_selected_tool_artifact_next_page());
    assert!(app.drain_pending_worker_commands().is_empty());
    assert_eq!(
        app.last_notice(),
        Some("full tool output is unavailable (missing); the saved summary remains auditable")
    );
    Ok(())
}

#[test]
fn restored_tool_artifact_card_reconciles_physical_availability() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let session_path = temp.path().join("session-artifact.jsonl");
    let durable_store = sigil_kernel::JsonlSessionStore::new(&session_path)?;
    let artifact_store = sigil_kernel::ToolArtifactStore::for_session_store(&durable_store);
    let (recorded, _) = sigil_kernel::ToolResultRecordedV3::capture(
        &ToolResult::ok(
            "call-restored-artifact",
            "bash",
            "durable full output",
            ToolResultMeta::default(),
        ),
        Some(&artifact_store),
        sigil_kernel::ToolArtifactSensitivity::Ordinary,
    )?;
    let mut session =
        sigil_kernel::Session::new("deepseek", "deepseek-v4-flash").with_store(durable_store);
    session.append(SessionLogEntry::ToolResultV3(recorded))?;
    let entries = session.entries().to_vec();

    let mut available_app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    available_app.restore_session_view(
        session_path.clone(),
        "deepseek".to_owned(),
        "deepseek-v4-flash".to_owned(),
        entries.clone(),
        "restored artifact",
    );
    assert!(full_plain_timeline(&available_app).contains("Saved full output"));
    assert!(full_plain_timeline(&available_app).contains("Alt-F search"));

    std::fs::remove_dir_all(artifact_store.root().join("blobs"))?;
    let mut missing_app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    missing_app.restore_session_view(
        session_path,
        "deepseek".to_owned(),
        "deepseek-v4-flash".to_owned(),
        entries,
        "restored missing artifact",
    );
    assert!(full_plain_timeline(&missing_app).contains("Full output unavailable (missing)"));
    Ok(())
}

#[test]
fn file_diff_tool_card_defaults_open_and_can_toggle_closed() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
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

    assert!(
        !app.timeline_state
            .expanded_tool_activity_keys
            .contains(&tool_key)
    );
    assert!(
        !app.timeline_state
            .collapsed_tool_activity_keys
            .contains(&tool_key)
    );
    assert!(app.tool_card_status_line().contains("open"));
    let default_open = app.transcript_lines(40);
    assert!(default_open.iter().any(|line| {
        line.spans
            .iter()
            .any(|span| span.content.as_ref().contains("--- current/note.txt"))
    }));

    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Char('t'), KeyModifiers::CONTROL))?;

    assert!(
        app.timeline_state
            .collapsed_tool_activity_keys
            .contains(&tool_key)
    );
    assert!(app.tool_card_status_line().contains("brief"));
    let collapsed = app.transcript_lines(40);
    assert!(collapsed.iter().any(|line| {
        line.spans
            .iter()
            .any(|span| span.content.as_ref().contains("note.txt"))
    }));
    assert!(collapsed.iter().any(|line| {
        line.spans
            .iter()
            .any(|span| span.content.as_ref().contains("more lines hidden"))
    }));
    assert!(!collapsed.iter().any(|line| {
        line.spans
            .iter()
            .any(|span| span.content.as_ref().contains("--- current/note.txt"))
    }));

    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Char('o'), KeyModifiers::CONTROL))?;

    assert!(
        !app.timeline_state
            .collapsed_tool_activity_keys
            .contains(&tool_key)
    );
    assert!(app.tool_card_status_line().contains("open"));
    Ok(())
}

#[test]
fn non_default_tool_card_toggle_pages_large_preview_before_closing() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    let preview_lines = (0..140)
        .map(|index| format!("line-{index:03}"))
        .collect::<Vec<_>>();
    app.push_timeline(
        TimelineRole::Tool,
        serde_json::json!({
            "call_id": "call-large",
            "tool_name": "diagnostic_dump",
            "status": "ok",
            "summary": "140 lines · 1 KB",
            "preview_kind": "text",
            "preview_lines": preview_lines,
            "hidden_lines": 0
        })
        .to_string(),
    );
    let tool_key = "call:call-large".to_owned();
    assert_eq!(
        app.timeline_state.selected_tool_activity_key,
        Some(tool_key.clone())
    );

    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Char('o'), KeyModifiers::CONTROL))?;
    assert_eq!(
        app.timeline_state.tool_activity_visible_rows.get(&tool_key),
        Some(&64)
    );
    let first_page = full_plain_timeline(&app);
    assert!(first_page.contains("line-062"));
    assert!(!first_page.contains("line-063"));
    assert!(first_page.contains("more lines hidden"));

    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Char('o'), KeyModifiers::CONTROL))?;
    assert_eq!(
        app.timeline_state.tool_activity_visible_rows.get(&tool_key),
        Some(&128)
    );
    let second_page = full_plain_timeline(&app);
    assert!(second_page.contains("line-126"));
    assert!(!second_page.contains("line-127"));

    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Char('o'), KeyModifiers::CONTROL))?;
    assert_eq!(
        app.timeline_state.tool_activity_visible_rows.get(&tool_key),
        Some(&192)
    );
    let full_page = full_plain_timeline(&app);
    assert!(full_page.contains("line-139"));
    assert!(!full_page.contains("more lines hidden"));

    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Char('o'), KeyModifiers::CONTROL))?;
    assert_eq!(
        app.timeline_state.tool_activity_visible_rows.get(&tool_key),
        None
    );
    assert!(
        !app.timeline_state
            .expanded_tool_activity_keys
            .contains(&tool_key)
    );
    let closed = full_plain_timeline(&app);
    assert!(!closed.contains("line-139"));
    Ok(())
}

#[test]
fn terminal_task_tool_card_pages_from_safe_log_artifact() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.sigil_paths.terminal_tasks_root = temp.path().join("tasks");
    let task_dir = app.sigil_paths.terminal_tasks_root.join("terminal-big");
    std::fs::create_dir_all(&task_dir)?;
    std::fs::write(
        task_dir.join("output.log"),
        (0..90)
            .map(|index| format!("log-line-{index:03}"))
            .collect::<Vec<_>>()
            .join("\n"),
    )?;
    app.push_timeline(
        TimelineRole::Tool,
        serde_json::json!({
            "call_id": "call-terminal",
            "tool_name": "terminal_task",
            "status": "ok",
            "summary": "running · ./scripts/check-touched.sh --tier quick",
            "preview_kind": "text",
            "preview_lines": ["tail-only"],
            "hidden_lines": 1,
            "metadata": {
                "details": {
                    "terminal_task": {
                        "task_id": "terminal-big",
                        "status": "exited",
                        "status_detail": { "state": "exited", "exit_code": 0 },
                        "command": "./scripts/check-touched.sh --tier quick",
                        "cwd": ".",
                        "shell": "zsh",
                        "log_path": "state/artifacts/tasks/terminal-big/output.log",
                        "created_at_ms": 1,
                        "updated_at_ms": 2
                    }
                }
            }
        })
        .to_string(),
    );
    let tool_key = app
        .timeline_state
        .selected_tool_activity_key
        .clone()
        .expect("terminal tool card should be selected");
    let entry_index = app
        .timeline_entry_index_for_activity_key(&tool_key)
        .expect("terminal tool card should have a timeline entry");

    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Char('o'), KeyModifiers::CONTROL))?;
    assert_eq!(
        app.timeline_state.tool_activity_visible_rows.get(&tool_key),
        Some(&64)
    );
    let payload: serde_json::Value = serde_json::from_str(&app.timeline[entry_index].text)?;
    let preview_lines = payload["preview_lines"]
        .as_array()
        .expect("terminal card should keep preview lines");
    assert_eq!(preview_lines.len(), 64);
    assert_eq!(preview_lines[0], "log-line-000");
    assert_eq!(preview_lines[63], "log-line-063");
    assert_eq!(payload["hidden_lines"], 1);
    let first_page = full_plain_timeline(&app);
    assert!(first_page.contains("log-line-000"));
    assert!(first_page.contains("more lines hidden"));

    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Char('o'), KeyModifiers::CONTROL))?;
    assert_eq!(
        app.timeline_state.tool_activity_visible_rows.get(&tool_key),
        Some(&128)
    );
    let payload: serde_json::Value = serde_json::from_str(&app.timeline[entry_index].text)?;
    let preview_lines = payload["preview_lines"]
        .as_array()
        .expect("terminal card should keep expanded preview lines");
    assert_eq!(preview_lines.len(), 90);
    assert_eq!(preview_lines[89], "log-line-089");
    assert_eq!(payload["hidden_lines"], 0);
    let second_page = full_plain_timeline(&app);
    assert!(second_page.contains("log-line-089"));
    assert!(!second_page.contains("more lines hidden"));
    Ok(())
}

#[test]
fn ctrl_t_tool_toggle_preserves_live_tail_when_already_at_latest() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.set_terminal_size(80, 8);
    app.push_timeline(
        TimelineRole::Tool,
        r##"{
  "call_id": "call-old",
  "tool_name": "ls",
  "status": "ok",
  "preview_kind": "json",
  "summary": "first 1/1 lines · 8 B",
  "preview_lines": ["[\".git\"]"],
  "preview_value": [".git"],
  "hidden_lines": 0
}"##,
    );
    for index in 0..24 {
        app.push_timeline(TimelineRole::Assistant, format!("tail message {index}"));
    }
    let tool_key = "call:call-old".to_owned();
    assert_eq!(
        app.timeline_state.selected_tool_activity_key,
        Some(tool_key.clone())
    );
    assert_eq!(app.timeline_scroll_back, 0);
    assert!(
        app.max_timeline_scroll_back() > 0,
        "test setup should have scrollback"
    );

    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Char('t'), KeyModifiers::CONTROL))?;

    assert!(
        app.timeline_state
            .expanded_tool_activity_keys
            .contains(&tool_key)
    );
    assert_eq!(app.timeline_scroll_back, 0);
    let lines = app.transcript_lines(20);
    assert!(lines.iter().any(|line| {
        line.spans
            .iter()
            .any(|span| span.content.as_ref().contains("tail message 23"))
    }));
    Ok(())
}

#[test]
fn appending_tool_card_moves_focus_marker_to_latest_card() {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
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

    let selection_bg = crate::ui::theme::default_palette().surface_selection;
    let rendered = app.transcript_lines(40);
    let line_text = |line: &ratatui::text::Line<'static>| {
        line.spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect::<String>()
    };
    let selected_lines = rendered
        .iter()
        .filter(|line| {
            line.spans
                .iter()
                .any(|span| span.style.bg == Some(selection_bg))
        })
        .map(line_text)
        .collect::<Vec<_>>();

    assert!(
        selected_lines
            .iter()
            .any(|line| line.contains("Wrote second.txt"))
    );
    assert!(
        !selected_lines
            .iter()
            .any(|line| line.contains("Wrote first.txt"))
    );
    assert!(
        !rendered
            .iter()
            .map(line_text)
            .any(|line| line.contains("path=second.txt"))
    );
}

#[test]
fn tool_activity_metadata_is_cached_after_append() {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.push_timeline(
        TimelineRole::Tool,
        r#"{
  "call_id": "call-cache",
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

    let activity = app
        .timeline_state
        .tool_activity_cache
        .first()
        .expect("tool activity should be cached")
        .clone();
    assert_eq!(activity.key, "call:call-cache");
    assert!(activity.defaults_expanded);

    app.timeline[activity.index].text = "not json anymore".to_owned();

    assert_eq!(
        app.timeline_entry_index_for_activity_key("call:call-cache"),
        Some(activity.index)
    );
    assert_eq!(
        app.tool_timeline_entry_indices(),
        Some(vec![activity.index])
    );
    assert!(app.tool_card_status_line().contains("open"));
}

#[test]
fn tool_card_interaction_commands_are_noop_without_tool_cards() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    assert_eq!(app.tool_card_status_line(), "activities: none");
    assert_eq!(app.timeline_state.selected_tool_activity_key, None);

    assert!(!app.has_tool_cards());
    assert!(app.tool_activity_entry_indices().is_empty());
    assert!(!app.select_tool_activity_entry(0));
    assert!(!app.focus_latest_tool_card());
    assert_eq!(app.last_notice(), Some("no activities yet"));
    assert!(!app.select_adjacent_tool_card(true));
    assert_eq!(app.last_notice(), Some("no activities yet"));
    assert!(!app.toggle_selected_tool_card());
    assert_eq!(app.last_notice(), Some("no activities yet"));

    assert!(
        app.handle_key_event(KeyEvent::new(KeyCode::Char('o'), KeyModifiers::CONTROL))?
            .is_none()
    );
    assert!(
        app.handle_key_event(KeyEvent::new(KeyCode::Char('t'), KeyModifiers::ALT))?
            .is_none()
    );
    assert!(
        app.handle_key_event(KeyEvent::new(KeyCode::Char('k'), KeyModifiers::ALT))?
            .is_none()
    );
    assert_eq!(app.timeline_state.selected_tool_activity_key, None);
    Ok(())
}

#[test]
fn tool_card_interaction_private_helpers_cover_stale_selection_and_reveal_guards() {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    let entries = vec![(2, "first".to_owned()), (5, "second".to_owned())];

    app.timeline_state.selected_tool_activity_key = Some("missing".to_owned());
    assert_eq!(
        app.ensure_selected_tool_entry(&entries),
        (5, "second".to_owned())
    );
    assert_eq!(
        app.timeline_state.selected_tool_activity_key.as_deref(),
        Some("second")
    );

    app.timeline_state.selected_tool_activity_key = Some("first".to_owned());
    assert_eq!(
        app.next_tool_entry(&entries, false),
        (5, "second".to_owned())
    );
    assert_eq!(
        app.next_tool_entry(&entries, true),
        (5, "second".to_owned())
    );

    app.reveal_timeline_entry(99);
    assert_eq!(app.timeline_scroll_back, 0);

    app.rerender_tool_selection_change(Some(0), 0);
    assert!(!app.timeline_render_lines().is_empty());
}

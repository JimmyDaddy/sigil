use super::*;
use crate::{
    mouse::{AppMouseOutcome, MouseInput, MouseInputKind},
    ui::LayoutSnapshot,
};
use ratatui::{
    layout::Rect,
    style::{Color, Style},
};

#[test]
fn short_transcript_stays_in_live_panel_instead_of_terminal_scrollback() {
    let mut app = AppState::from_root_config(Path::new("termquill.toml"), &test_config());
    app.set_terminal_size(120, 32);
    app.push_timeline(TimelineRole::User, "hello");
    app.push_timeline(TimelineRole::Assistant, "latest answer");

    assert_eq!(app.scrollback_line_count(), 0);
    let live = app
        .transcript_lines(app.timeline_viewport_rows())
        .into_iter()
        .flat_map(|line| line.spans.into_iter().map(|span| span.content.into_owned()))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(live.contains("hello"));
    assert!(live.contains("latest answer"));
}

#[test]
fn long_transcript_keeps_render_cache_consistent_without_front_trim() {
    let mut app = AppState::from_root_config(Path::new("termquill.toml"), &test_config());

    for index in 0..450 {
        app.push_timeline(TimelineRole::Notice, format!("notice {index}"));
    }

    assert!(app.timeline.len() >= 450);
    assert_eq!(app.timeline_render_ranges.len(), app.timeline.len());
    let rendered = app.timeline_plain_cache.join("\n");
    assert!(rendered.contains("notice 0"));
    assert!(rendered.contains("notice 449"));
}

#[test]
fn reasoning_delta_creates_collapsed_thinking_block() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("termquill.toml"), &test_config());

    app.handle(RunEvent::ReasoningDelta("planning step 1".to_owned()))?;
    app.handle(RunEvent::ReasoningDelta("\nplanning step 2".to_owned()))?;

    assert!(
        !app.timeline
            .iter()
            .any(|entry| entry.role == TimelineRole::Phase)
    );
    assert!(
        app.events.iter().any(|event| {
            event.label == "phase" && event.detail == "thinking|deepseek-v4-flash"
        })
    );
    assert!(app.timeline.iter().any(|entry| {
        entry.role == TimelineRole::Thinking && entry.text == "planning step 1\nplanning step 2"
    }));
    let collapsed = app.transcript_lines(20);
    assert!(collapsed.iter().any(|line| {
        line.spans
            .iter()
            .any(|span| span.content.as_ref().contains("Ctrl-T expand"))
    }));
    assert!(collapsed.iter().any(|line| {
        line.spans
            .iter()
            .any(|span| span.content.as_ref().contains("planning step 2"))
    }));
    Ok(())
}

#[test]
fn ctrl_t_toggles_thinking_block_expansion() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("termquill.toml"), &test_config());

    app.handle(RunEvent::ReasoningDelta("planning step 1".to_owned()))?;
    app.handle(RunEvent::ReasoningDelta("\nplanning step 2".to_owned()))?;
    app.handle(RunEvent::ReasoningDelta(
        "\nplanning step 3\nplanning step 4".to_owned(),
    ))?;

    let collapsed = app.transcript_lines(20);
    assert!(collapsed.iter().any(|line| {
        line.spans
            .iter()
            .any(|span| span.content.as_ref().contains("Ctrl-T expand"))
    }));
    assert!(collapsed.iter().any(|line| {
        line.spans
            .iter()
            .any(|span| span.content.as_ref().contains("planning step 2"))
    }));
    assert!(!collapsed.iter().any(|line| {
        line.spans
            .iter()
            .any(|span| span.content.as_ref().contains("planning step 4"))
    }));

    app.handle_key_event(KeyEvent::new(KeyCode::Char('t'), KeyModifiers::CONTROL))?;

    let expanded = app.transcript_lines(20);
    assert!(expanded.iter().any(|line| {
        line.spans
            .iter()
            .any(|span| span.content.as_ref().contains("Ctrl-T collapse"))
    }));
    assert!(expanded.iter().any(|line| {
        line.spans
            .iter()
            .any(|span| span.content.as_ref().contains("planning step 4"))
    }));
    assert_eq!(app.last_notice(), Some("thinking expanded"));

    app.handle_key_event(KeyEvent::new(KeyCode::Char('t'), KeyModifiers::CONTROL))?;

    let recollapsed = app.transcript_lines(20);
    assert!(recollapsed.iter().any(|line| {
        line.spans
            .iter()
            .any(|span| span.content.as_ref().contains("Ctrl-T expand"))
    }));
    assert!(!recollapsed.iter().any(|line| {
        line.spans
            .iter()
            .any(|span| span.content.as_ref().contains("planning step 4"))
    }));
    assert_eq!(app.last_notice(), Some("thinking collapsed"));
    Ok(())
}

#[test]
fn tool_result_is_rendered_as_multiline_json_block() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("termquill.toml"), &test_config());

    app.handle(RunEvent::ToolResult(termquill_kernel::ToolResult::ok(
        "call-1".to_owned(),
        "ls".to_owned(),
        "[\".git\",\"Cargo.toml\"]".to_owned(),
        termquill_kernel::ToolResultMeta::default(),
    )))?;

    let entry = app.timeline.last().expect("expected tool timeline entry");
    let rendered: serde_json::Value = serde_json::from_str(&entry.text)?;
    assert_eq!(entry.role, TimelineRole::Tool);
    assert_eq!(rendered["tool_name"], "ls");
    assert_eq!(rendered["preview_kind"], "json");
    assert_eq!(rendered["status"], "ok");
    assert!(rendered["preview_lines"].as_array().is_some_and(|lines| {
        lines
            .iter()
            .any(|line| line.as_str().is_some_and(|text| text.contains(".git")))
    }));
    Ok(())
}

#[test]
fn tool_result_card_redacts_configured_secret_from_display_payloads() -> Result<()> {
    let mut config = test_config();
    config.providers.insert(
        "deepseek".to_owned(),
        json!({
            "api_key": "sk-ui-secret"
        }),
    );
    let mut app = AppState::from_root_config(Path::new("termquill.toml"), &config);
    let preview = ToolPreview {
        title: "Update secrets.txt".to_owned(),
        summary: "Preview summary".to_owned(),
        body: "--- current/secrets.txt\n+++ proposed/secrets.txt\n@@ -1 +1 @@\n-old\n+sk-ui-secret"
            .to_owned(),
        changed_files: vec!["secrets.txt".to_owned()],
        file_diffs: vec![termquill_kernel::ToolPreviewFile {
            path: "secrets.txt".to_owned(),
            diff: "--- current/secrets.txt\n+++ proposed/secrets.txt\n@@ -1 +1 @@\n-old\n+sk-ui-secret"
                .to_owned(),
        }],
    };
    let snapshot = ToolPreviewSnapshot::from_preview(
        "call-secret",
        "write_file",
        &preview,
        Default::default(),
        None,
    );

    app.handle(RunEvent::Control(ControlEntry::ToolPreviewCaptured(
        snapshot,
    )))?;
    app.handle(RunEvent::ToolResult(ToolResult::ok(
        "call-secret",
        "write_file",
        r#"{"token":"sk-ui-secret","value":"visible"}"#,
        ToolResultMeta {
            bytes: Some(36),
            changed_files: vec!["secrets.txt".to_owned()],
            details: json!({
                "api_key": "sk-ui-secret",
                "note": "visible"
            }),
            ..ToolResultMeta::default()
        },
    )))?;

    let entry = app.timeline.last().expect("expected tool timeline entry");
    let rendered: serde_json::Value = serde_json::from_str(&entry.text)?;
    let serialized = serde_json::to_string(&rendered)?;

    assert!(!serialized.contains("sk-ui-secret"));
    assert!(serialized.contains(termquill_kernel::REDACTED_SECRET));
    assert_eq!(
        rendered["metadata"]["details"]["api_key"],
        termquill_kernel::REDACTED_SECRET
    );
    assert_eq!(
        rendered["preview_value"]["token"],
        termquill_kernel::REDACTED_SECRET
    );
    Ok(())
}

#[test]
fn large_tool_result_display_is_bounded_and_redacted() -> Result<()> {
    let mut config = test_config();
    config.providers.insert(
        "deepseek".to_owned(),
        json!({
            "api_key": "sk-large-secret"
        }),
    );
    let mut app = AppState::from_root_config(Path::new("termquill.toml"), &config);
    let content = format!(
        "Authorization: Bearer sk-large-secret\n{}",
        "x".repeat(70 * 1024)
    );

    app.handle(RunEvent::ToolResult(ToolResult::ok(
        "call-large",
        "bash",
        content,
        ToolResultMeta::default(),
    )))?;

    let entry = app.timeline.last().expect("expected tool timeline entry");
    let rendered: serde_json::Value = serde_json::from_str(&entry.text)?;
    let serialized = serde_json::to_string(&rendered)?;
    assert_eq!(rendered["display_truncated"], true);
    assert!(
        rendered["summary"]
            .as_str()
            .is_some_and(|summary| { summary.contains("display truncated") })
    );
    assert!(!serialized.contains("sk-large-secret"));
    assert!(serialized.contains(termquill_kernel::REDACTED_SECRET));
    Ok(())
}

#[test]
fn batched_streaming_text_deltas_rerender_once_after_drain() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("termquill.toml"), &test_config());

    app.begin_timeline_render_batch();
    app.handle(RunEvent::TextDelta("```rust\n".to_owned()))?;
    let revision_after_first_delta = app.timeline_revision();
    app.handle(RunEvent::TextDelta("fn main() {}\n".to_owned()))?;
    app.handle(RunEvent::TextDelta("```\n".to_owned()))?;

    let rendered_before_flush = app.timeline_plain_cache.join("\n");
    assert!(!rendered_before_flush.contains("fn main"));
    assert_eq!(app.timeline_revision(), revision_after_first_delta);

    assert!(app.flush_timeline_render_batch());

    let rendered_after_flush = app.timeline_plain_cache.join("\n");
    assert!(rendered_after_flush.contains("fn main"));
    assert!(app.timeline_revision() > revision_after_first_delta);
    Ok(())
}

#[test]
fn streaming_assistant_defers_code_highlight_until_finished() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("termquill.toml"), &test_config());
    let plain_code_style = Style::default()
        .fg(Color::Rgb(236, 240, 246))
        .bg(Color::Rgb(28, 33, 41));

    app.handle(RunEvent::TextDelta("```rust\n".to_owned()))?;
    app.handle(RunEvent::TextDelta("fn main() {}\n```\n".to_owned()))?;

    let streaming_style =
        timeline_span_style_containing(&app, "fn main").expect("streaming fn should render");
    assert_eq!(streaming_style, plain_code_style);

    app.handle(RunEvent::ToolCallStarted(ToolCall {
        id: "call-1".to_owned(),
        name: "read_file".to_owned(),
        args_json: "{}".to_owned(),
    }))?;

    let finished_style =
        timeline_span_style_containing(&app, "fn").expect("finished fn should render");
    assert_ne!(finished_style, plain_code_style);
    Ok(())
}

#[test]
fn streaming_deltas_do_not_fill_ui_event_log() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("termquill.toml"), &test_config());
    let initial_events = app.events.len();

    for _ in 0..32 {
        app.handle(RunEvent::TextDelta("chunk ".to_owned()))?;
    }

    assert!(
        app.events
            .iter()
            .any(|event| event.label == "phase" && event.detail == "streaming")
    );
    assert!(!app.events.iter().any(|event| event.label == "text"));
    let after_text_events = app.events.len();
    assert_eq!(after_text_events, initial_events + 1);

    for _ in 0..32 {
        app.handle(RunEvent::ReasoningDelta("thought ".to_owned()))?;
    }

    assert!(
        app.events.iter().any(|event| {
            event.label == "phase" && event.detail == "thinking|deepseek-v4-flash"
        })
    );
    assert!(!app.events.iter().any(|event| event.label == "reasoning"));
    assert_eq!(app.events.len(), after_text_events + 1);

    for _ in 0..32 {
        app.handle(RunEvent::ToolCallArgsDelta {
            id: "call-1".to_owned(),
            delta: r#"{"path":"src/lib.rs"}"#.to_owned(),
        })?;
    }

    assert!(!app.events.iter().any(|event| event.label == "tool:args"));
    assert_eq!(app.events.len(), after_text_events + 1);
    Ok(())
}

fn timeline_span_style_containing(app: &AppState, text: &str) -> Option<Style> {
    app.timeline_render_cache
        .iter()
        .flat_map(|line| line.spans.iter())
        .find(|span| span.content.contains(text))
        .map(|span| span.style)
}

#[test]
fn tool_result_uses_live_approval_preview_snapshot_for_diff_card() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("termquill.toml"), &test_config());
    inject_write_file_approval(&mut app, sample_approval_preview())?;
    app.handle(RunEvent::ToolApprovalResolved {
        call_id: "call-1".to_owned(),
        approved: true,
        reason: None,
    })?;

    app.handle(RunEvent::ToolResult(ToolResult::ok(
        "call-1",
        "write_file",
        "wrote note.txt",
        ToolResultMeta {
            bytes: Some(14),
            changed_files: vec!["note.txt".to_owned()],
            ..ToolResultMeta::default()
        },
    )))?;

    let entry = app.timeline.last().expect("expected tool timeline entry");
    let rendered: serde_json::Value = serde_json::from_str(&entry.text)?;
    assert_eq!(rendered["tool_name"], "write_file");
    assert!(
        rendered["summary"].as_str().is_some_and(|summary| {
            summary.contains("diff +1 -1") && summary.contains("1 file")
        })
    );
    assert_eq!(rendered["diff"]["files"][0]["path"], "note.txt");
    assert!(
        rendered["diff"]["files"][0]["lines"]
            .as_array()
            .is_some_and(|lines| {
                lines
                    .iter()
                    .any(|line| line.as_str().is_some_and(|text| text == "+gamma"))
            })
    );

    Ok(())
}

#[test]
fn control_preview_snapshot_event_caches_diff_for_tool_result() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("termquill.toml"), &test_config());
    let snapshot = ToolPreviewSnapshot::from_preview(
        "call-1",
        "write_file",
        &sample_approval_preview(),
        Default::default(),
        Some("preview-hash".to_owned()),
    );

    app.handle(RunEvent::Control(ControlEntry::ToolPreviewCaptured(
        snapshot,
    )))?;
    app.handle(RunEvent::ToolResult(ToolResult::ok(
        "call-1",
        "write_file",
        "wrote note.txt",
        ToolResultMeta {
            bytes: Some(14),
            changed_files: vec!["note.txt".to_owned()],
            ..ToolResultMeta::default()
        },
    )))?;

    let entry = app.timeline.last().expect("expected tool timeline entry");
    let rendered: serde_json::Value = serde_json::from_str(&entry.text)?;
    assert_eq!(rendered["diff"]["summary"], "+1 -1 · 1 file");
    assert!(app.events.iter().any(|event| {
        event.label == "control"
            && event
                .detail
                .contains("preview call-1 write_file files=1 +1 -1")
    }));
    Ok(())
}

#[test]
fn delete_file_tool_result_uses_preview_snapshot_for_diff_card() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("termquill.toml"), &test_config());
    let snapshot = ToolPreviewSnapshot::from_preview(
        "call-delete-1",
        "delete_file",
        &sample_delete_approval_preview(),
        Default::default(),
        Some("delete-preview-hash".to_owned()),
    );

    app.handle(RunEvent::Control(ControlEntry::ToolPreviewCaptured(
        snapshot,
    )))?;
    app.handle(RunEvent::ToolResult(ToolResult::ok(
        "call-delete-1",
        "delete_file",
        "deleted /workspace/note.txt",
        ToolResultMeta {
            bytes: Some(11),
            changed_files: vec!["note.txt".to_owned()],
            details: json!({
                "action": "delete",
                "call": {
                    "summary": "path=note.txt"
                }
            }),
            ..ToolResultMeta::default()
        },
    )))?;

    let entry = app.timeline.last().expect("expected tool timeline entry");
    let rendered: serde_json::Value = serde_json::from_str(&entry.text)?;
    assert_eq!(rendered["tool_name"], "delete_file");
    assert!(
        rendered["summary"].as_str().is_some_and(|summary| {
            summary.contains("diff +0 -2") && summary.contains("1 file")
        })
    );
    assert_eq!(rendered["metadata"]["details"]["action"], "delete");
    assert!(
        rendered["diff"]["files"][0]["lines"]
            .as_array()
            .is_some_and(|lines| {
                lines
                    .iter()
                    .any(|line| line.as_str().is_some_and(|text| text == "-alpha"))
                    && lines
                        .iter()
                        .any(|line| line.as_str().is_some_and(|text| text == "-beta"))
            })
    );

    Ok(())
}

#[test]
fn error_tool_result_does_not_render_cached_preview_as_applied_diff() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("termquill.toml"), &test_config());
    inject_write_file_approval(&mut app, sample_approval_preview())?;
    app.handle(RunEvent::ToolApprovalResolved {
        call_id: "call-1".to_owned(),
        approved: false,
        reason: Some("denied".to_owned()),
    })?;

    app.handle(RunEvent::ToolResult(ToolResult::error(
        "call-1",
        "write_file",
        ToolErrorKind::ApprovalDenied,
        "tool execution denied by user: denied",
    )))?;

    let entry = app.timeline.last().expect("expected tool timeline entry");
    let rendered: serde_json::Value = serde_json::from_str(&entry.text)?;
    assert_eq!(rendered["status"], "error");
    assert!(rendered.get("diff").is_none());
    Ok(())
}

#[test]
fn ctrl_u_and_ctrl_d_scroll_transcript_history() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("termquill.toml"), &test_config());
    app.set_terminal_size(80, 12);
    for index in 0..8 {
        app.push_timeline(TimelineRole::Assistant, format!("message {index}"));
    }

    let bottom = app.transcript_lines(4);
    app.handle_key_event(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL))?;
    let scrolled = app.transcript_lines(4);

    assert!(app.timeline_scroll_back > 0);
    assert_ne!(bottom, scrolled);

    app.handle_key_event(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL))?;
    assert_eq!(app.timeline_scroll_back, 0);
    Ok(())
}

#[test]
fn ctrl_home_and_ctrl_end_jump_transcript_between_oldest_and_newest() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("termquill.toml"), &test_config());
    app.set_terminal_size(80, 12);
    for index in 0..8 {
        app.push_timeline(TimelineRole::Assistant, format!("message {index}"));
    }

    app.handle_key_event(KeyEvent::new(KeyCode::Home, KeyModifiers::CONTROL))?;
    assert_eq!(app.timeline_scroll_back, app.max_timeline_scroll_back());

    app.handle_key_event(KeyEvent::new(KeyCode::End, KeyModifiers::CONTROL))?;
    assert_eq!(app.timeline_scroll_back, 0);
    Ok(())
}

#[test]
fn scrolling_transcript_to_top_reaches_earliest_message() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("termquill.toml"), &test_config());
    app.set_terminal_size(80, 12);
    for index in 0..20 {
        app.push_timeline(TimelineRole::Assistant, format!("message {index}"));
    }

    app.handle_key_event(KeyEvent::new(KeyCode::Home, KeyModifiers::CONTROL))?;
    let top = app.transcript_lines(app.timeline_viewport_rows());

    assert!(top.iter().any(|line| {
        line.spans
            .iter()
            .any(|span| span.content.as_ref().contains("message 0"))
    }));
    assert_eq!(app.timeline_scroll_back, app.max_timeline_scroll_back());
    Ok(())
}

#[test]
fn transcript_live_tail_ignores_trailing_gap_rows() {
    let mut app = AppState::from_root_config(Path::new("termquill.toml"), &test_config());
    app.set_terminal_size(80, 12);
    app.push_timeline(TimelineRole::User, "hello");

    let tail = app.transcript_lines(1);
    let rendered = tail
        .iter()
        .flat_map(|line| line.spans.iter())
        .map(|span| span.content.as_ref())
        .collect::<String>();

    assert!(rendered.contains("hello"));
}

#[test]
fn inspection_tool_entries_render_as_individual_activities() {
    let mut app = AppState::from_root_config(Path::new("termquill.toml"), &test_config());
    app.push_timeline(
        TimelineRole::Tool,
        r#"{
  "call_id": "call-ls",
  "tool_name": "ls",
  "status": "ok",
  "preview_kind": "json",
  "preview_lines": ["[\"src/main.rs\"]"],
  "preview_value": ["src/main.rs"],
  "hidden_lines": 0,
  "metadata": {"details": {"call": {"summary": "path=crates"}}}
}"#,
    );
    app.push_timeline(
        TimelineRole::Tool,
        r#"{
  "call_id": "call-search",
  "tool_name": "bash",
  "status": "ok",
  "preview_kind": "text",
  "preview_lines": ["src/main.rs:needle"],
  "hidden_lines": 0,
  "metadata": {"details": {"call": {"summary": "command=grep -n needle src/main.rs"}}}
}"#,
    );
    app.push_timeline(
        TimelineRole::Tool,
        r#"{
  "call_id": "call-read",
  "tool_name": "read_file",
  "status": "ok",
  "preview_kind": "text",
  "preview_lines": ["hello"],
  "hidden_lines": 0,
  "metadata": {"details": {"call": {"summary": "path=README.md"}}}
}"#,
    );

    let rendered = app.timeline_plain_cache.join("\n");
    let indices = app
        .tool_timeline_entry_indices()
        .expect("expected tool entries");
    let ranges = indices
        .iter()
        .map(|index| app.timeline_render_ranges[*index].clone())
        .collect::<Vec<_>>();

    assert_eq!(ranges.len(), 3);
    assert_ne!(ranges[0], ranges[1]);
    assert_ne!(ranges[1], ranges[2]);
    assert!(!rendered.contains("Inspected"));
    assert!(rendered.contains("Listed crates"));
    assert!(rendered.contains("Searched needle in src/main.rs"));
    assert!(rendered.contains("Read README.md"));
}

#[test]
fn permission_notices_between_inspection_tools_remain_visible() {
    let mut app = AppState::from_root_config(Path::new("termquill.toml"), &test_config());
    app.push_timeline(
        TimelineRole::Notice,
        "permission ls subject=crates mode=allow",
    );
    app.push_timeline(
        TimelineRole::Tool,
        r#"{
  "call_id": "call-ls",
  "tool_name": "ls",
  "status": "ok",
  "preview_kind": "json",
  "preview_lines": ["[\"src/main.rs\"]"],
  "preview_value": ["src/main.rs"],
  "hidden_lines": 0,
  "metadata": {"details": {"call": {"summary": "path=crates"}}}
}"#,
    );
    app.push_timeline(
        TimelineRole::Notice,
        "permission read_file subject=README.md mode=allow",
    );
    app.push_timeline(
        TimelineRole::Tool,
        r#"{
  "call_id": "call-read",
  "tool_name": "read_file",
  "status": "ok",
  "preview_kind": "text",
  "preview_lines": ["hello"],
  "hidden_lines": 0,
  "metadata": {"details": {"call": {"summary": "path=README.md"}}}
}"#,
    );

    let rendered = app.timeline_plain_cache.join("\n");

    assert!(!rendered.contains("Inspected"));
    assert!(rendered.contains("permission ls subject=crates mode=allow"));
    assert!(rendered.contains("permission read_file subject=README.md mode=allow"));
    assert!(rendered.contains("Listed crates"));
    assert!(rendered.contains("Read README.md"));
}

#[test]
fn file_changes_and_complex_bash_do_not_create_inspected_group() {
    let mut app = AppState::from_root_config(Path::new("termquill.toml"), &test_config());
    app.push_timeline(
        TimelineRole::Tool,
        r#"{
  "call_id": "call-ls",
  "tool_name": "ls",
  "status": "ok",
  "preview_kind": "json",
  "preview_lines": ["[\"src/main.rs\"]"],
  "preview_value": ["src/main.rs"],
  "hidden_lines": 0,
  "metadata": {"details": {"call": {"summary": "path=crates"}}}
}"#,
    );
    app.push_timeline(
        TimelineRole::Tool,
        r#"{
  "call_id": "call-write",
  "tool_name": "write_file",
  "status": "ok",
  "preview_kind": "text",
  "preview_lines": ["wrote note.txt"],
  "hidden_lines": 0,
  "metadata": {
    "changed_files": ["note.txt"],
    "details": {"call": {"summary": "path=note.txt"}}
  }
}"#,
    );
    app.push_timeline(
        TimelineRole::Tool,
        r#"{
  "call_id": "call-complex",
  "tool_name": "bash",
  "status": "ok",
  "preview_kind": "text",
  "preview_lines": ["src/main.rs:needle"],
  "hidden_lines": 0,
  "metadata": {"details": {"call": {"summary": "command=grep needle src/main.rs | head"}}}
}"#,
    );
    app.push_timeline(
        TimelineRole::Tool,
        r#"{
  "call_id": "call-read",
  "tool_name": "read_file",
  "status": "ok",
  "preview_kind": "text",
  "preview_lines": ["hello"],
  "hidden_lines": 0,
  "metadata": {"details": {"call": {"summary": "path=README.md"}}}
}"#,
    );

    let rendered = app.timeline_plain_cache.join("\n");

    assert!(!rendered.contains("Inspected"));
    assert!(rendered.contains("Listed crates"));
    assert!(rendered.contains("Wrote note.txt"));
    assert!(rendered.contains("Ran grep needle src/main.rs | head"));
    assert!(rendered.contains("Read README.md"));
}

#[test]
fn mouse_scroll_moves_transcript() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("termquill.toml"), &test_config());
    app.set_terminal_size(80, 12);
    for index in 0..8 {
        app.push_timeline(TimelineRole::Assistant, format!("message {index}"));
    }

    let layout = LayoutSnapshot::from_app(Rect::new(0, 0, 80, 12), &app);
    let outcome = app.handle_mouse_event(
        MouseInput {
            column: 1,
            row: 1,
            kind: MouseInputKind::ScrollUp,
            modifiers: KeyModifiers::NONE,
        },
        &layout,
    )?;
    assert!(matches!(outcome, AppMouseOutcome::Redraw));
    assert!(app.timeline_scroll_back > 0);

    let outcome = app.handle_mouse_event(
        MouseInput {
            column: 1,
            row: 1,
            kind: MouseInputKind::ScrollDown,
            modifiers: KeyModifiers::NONE,
        },
        &layout,
    )?;
    assert!(matches!(outcome, AppMouseOutcome::Redraw));
    assert_eq!(app.timeline_scroll_back, 0);
    Ok(())
}

#[test]
fn default_open_large_diff_stays_stable_when_new_output_arrives() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("termquill.toml"), &test_config());
    app.set_terminal_size(120, 18);
    let snapshot = ToolPreviewSnapshot::from_preview(
        "call-delete-1",
        "delete_file",
        &sample_delete_approval_preview(),
        Default::default(),
        Some("delete-preview-hash".to_owned()),
    );
    app.handle(RunEvent::Control(ControlEntry::ToolPreviewCaptured(
        snapshot,
    )))?;
    app.handle(RunEvent::ToolResult(ToolResult::ok(
        "call-delete-1",
        "delete_file",
        "deleted /workspace/note.txt",
        ToolResultMeta {
            bytes: Some(11),
            changed_files: vec!["note.txt".to_owned()],
            details: json!({
                "action": "delete",
                "call": {
                    "summary": "path=note.txt"
                }
            }),
            ..ToolResultMeta::default()
        },
    )))?;
    let first_revision = app.timeline_revision();

    for index in 0..5 {
        app.push_timeline(TimelineRole::Notice, format!("notice {index}"));
    }
    app.handle(RunEvent::TextDelta("stream one".to_owned()))?;
    app.handle(RunEvent::TextDelta("\nstream two".to_owned()))?;

    let rendered = app.timeline_plain_cache.join("\n");
    assert_eq!(rendered.matches("--- current/note.txt").count(), 1);
    assert_eq!(rendered.matches("-alpha").count(), 1);
    assert_eq!(rendered.matches("Deleted note.txt").count(), 1);
    assert_eq!(rendered.matches("path=note.txt").count(), 0);
    assert!(rendered.contains("stream one"));
    assert!(rendered.contains("stream two"));
    assert!(app.timeline_revision() > first_revision);
    Ok(())
}

#[test]
fn compaction_status_tracks_latest_prompt_tokens_instead_of_cumulative_totals() -> Result<()> {
    let mut config = test_config();
    config.agent.provider = "planned".to_owned();
    config.agent.model = "planned-model".to_owned();
    config.compaction.context_window_tokens = Some(100);
    config.compaction.soft_threshold_ratio = 0.5;
    config.compaction.hard_threshold_ratio = 0.8;
    let mut app = AppState::from_root_config(Path::new("termquill.toml"), &config);

    app.handle(RunEvent::Usage(UsageStats {
        prompt_tokens: 70,
        completion_tokens: 0,
        cache_hit_tokens: 0,
        cache_miss_tokens: 70,
        input_cost: 0.0,
        output_cost: 0.0,
        cache_savings: 0.0,
        system_fingerprint: None,
    }))?;
    assert_eq!(app.compaction_status, "soft");

    app.handle(RunEvent::Usage(UsageStats {
        prompt_tokens: 20,
        completion_tokens: 0,
        cache_hit_tokens: 0,
        cache_miss_tokens: 20,
        input_cost: 0.0,
        output_cost: 0.0,
        cache_savings: 0.0,
        system_fingerprint: None,
    }))?;

    assert_eq!(app.compaction_status, "ready");
    Ok(())
}

#[test]
fn context_usage_and_compaction_policy_share_effective_window() -> Result<()> {
    let mut config = test_config();
    config.agent.model = "deepseek-v4-pro".to_owned();
    config.compaction.context_window_tokens = Some(128_000);
    config.compaction.soft_threshold_ratio = 0.5;
    config.compaction.hard_threshold_ratio = 0.8;
    let mut app = AppState::from_root_config(Path::new("termquill.toml"), &config);

    app.handle(RunEvent::Usage(UsageStats {
        prompt_tokens: 90_354,
        completion_tokens: 0,
        cache_hit_tokens: 0,
        cache_miss_tokens: 90_354,
        input_cost: 0.0,
        output_cost: 0.0,
        cache_savings: 0.0,
        system_fingerprint: None,
    }))?;

    assert_eq!(app.context_usage_line(), "ctx: 9% · 90.4K / 1.0M tok");
    assert_eq!(app.compaction_status, "ready");
    assert!(app.footer_status_line().contains("tok 90.4K"));
    assert!(app.footer_status_line().contains("ctx 9%"));
    assert!(
        app.usage_sidebar_lines()
            .iter()
            .any(|line| line == "policy: 1,000,000 provider · soft 50% · hard 80%")
    );
    Ok(())
}

#[test]
fn live_activity_summary_tracks_busy_phase() {
    let mut app = AppState::from_root_config(Path::new("termquill.toml"), &test_config());
    assert!(app.live_activity_summary().is_none());

    app.is_busy = true;
    app.run_phase = RunPhase::Tool("read_file".to_owned());

    let summary = app.live_activity_summary().expect("expected live summary");
    assert_eq!(summary.label, "tool");
    assert_eq!(summary.detail, "running read_file");
}

use super::super::timeline_flow::{selected_timeline_line_columns, text_by_display_columns};
use super::*;
use crate::{
    mouse::{AppMouseOutcome, MouseInput, MouseInputKind},
    timeline::TimelineEntry,
    ui::LayoutSnapshot,
};
use ratatui::{
    layout::Rect,
    style::{Color, Style},
    text::{Line, Span},
};
use std::path::PathBuf;

fn sync_child_agent_for_transcript_tests(app: &mut AppState) -> Result<()> {
    let task_id = sigil_kernel::TaskId::new("task_1")?;
    let step_id = sigil_kernel::TaskStepId::new("step_1")?;
    app.sync_current_session_state(vec![
        SessionLogEntry::Control(ControlEntry::TaskRun(sigil_kernel::TaskRunEntry {
            task_id: task_id.clone(),
            parent_session_ref: sigil_kernel::SessionRef::new_relative("parent.jsonl")?,
            objective: "review workspace".to_owned(),
            status: sigil_kernel::TaskRunStatus::Running,
            reason: None,
        })),
        SessionLogEntry::Control(ControlEntry::TaskPlan(sigil_kernel::TaskPlanEntry {
            task_id: task_id.clone(),
            plan_version: 1,
            status: sigil_kernel::TaskPlanStatus::Accepted,
            steps: vec![sigil_kernel::TaskStepSpec {
                step_id: step_id.clone(),
                title: "inspect".to_owned(),
                display_name: Some("repo read".to_owned()),
                detail: None,
                role: sigil_kernel::AgentRole::SubagentRead,
                depends_on: Vec::new(),
                mode: None,
                isolation: None,
            }],
            reason: None,
        })),
        SessionLogEntry::Control(ControlEntry::TaskChildSession(
            sigil_kernel::TaskChildSessionEntry {
                task_id,
                plan_version: 1,
                step_id,
                child_task_id: sigil_kernel::TaskId::new("child_1")?,
                child_session_ref: sigil_kernel::SessionRef::new_relative(
                    "children/task_1/step_1-child_1.jsonl",
                )?,
                role: sigil_kernel::AgentRole::SubagentRead,
                status: sigil_kernel::TaskChildSessionStatus::Started,
                summary_hash: None,
            },
        )),
    ]);
    app.activate_agent_from_command("child_1")?;
    Ok(())
}

fn transcript_plain(lines: Vec<Line<'static>>) -> String {
    lines
        .into_iter()
        .flat_map(|line| {
            line.spans
                .into_iter()
                .map(|span| span.content.into_owned())
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn transcript_plain_lines(lines: Vec<Line<'static>>) -> Vec<String> {
    lines
        .into_iter()
        .map(|line| {
            line.spans
                .into_iter()
                .map(|span| span.content.into_owned())
                .collect::<String>()
        })
        .collect()
}

#[test]
fn column_selection_helpers_cover_empty_and_zero_width_edges() {
    let unchanged = selected_timeline_line_columns(Line::from(Span::raw("abc")), 2..2);
    assert_eq!(unchanged.spans.len(), 1);
    assert_eq!(unchanged.spans[0].content.as_ref(), "abc");

    let selected = selected_timeline_line_columns(Line::from(Span::raw("\u{0301}a")), 0..1);
    let selected_text = selected
        .spans
        .iter()
        .map(|span| span.content.as_ref())
        .collect::<String>();
    assert_eq!(selected_text, "\u{0301}a");
    assert!(
        selected
            .spans
            .iter()
            .any(|span| span.style.bg == Some(Color::Rgb(242, 171, 122)))
    );

    assert_eq!(text_by_display_columns("abc", 2, 2), "");
    assert_eq!(text_by_display_columns("\u{0301}a", 0, 1), "\u{0301}a");
}

#[test]
fn short_transcript_stays_in_live_panel_instead_of_terminal_scrollback() {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
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
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());

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
fn reasoning_delta_keeps_latest_thinking_expanded_until_tool_starts() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());

    app.handle(RunEvent::ReasoningDelta("planning step 1".to_owned()))?;
    app.handle(RunEvent::ReasoningDelta(
        "\nplanning step 2\nplanning step 3\nplanning step 4".to_owned(),
    ))?;

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
        entry.role == TimelineRole::Thinking
            && entry.text == "planning step 1\nplanning step 2\nplanning step 3\nplanning step 4"
    }));
    let streaming = app.transcript_lines(20);
    let streaming_plain = transcript_plain(streaming.clone());
    assert!(streaming_plain.contains("thinking"));
    assert!(!streaming_plain.contains("thought"));
    assert!(app.collapsible_thinking_entry_indices().is_empty());
    assert!(streaming.iter().any(|line| {
        line.spans
            .iter()
            .any(|span| span.content.as_ref().contains("Ctrl-T collapse"))
    }));
    assert!(streaming.iter().any(|line| {
        line.spans
            .iter()
            .any(|span| span.content.as_ref().contains("planning step 4"))
    }));

    app.handle(RunEvent::ToolCallStarted(ToolCall {
        id: "call-1".to_owned(),
        name: "read_file".to_owned(),
        args_json: "{}".to_owned(),
    }))?;

    let collapsed = app.transcript_lines(20);
    let collapsed_plain = transcript_plain(collapsed.clone());
    assert!(collapsed_plain.contains("thought"));
    assert!(!collapsed_plain.contains("thinking"));
    assert_eq!(app.collapsible_thinking_entry_indices().len(), 1);
    assert!(collapsed.iter().any(|line| {
        line.spans
            .iter()
            .any(|span| span.content.as_ref().contains("Ctrl-T expand"))
    }));
    assert!(!collapsed.iter().any(|line| {
        line.spans
            .iter()
            .any(|span| span.content.as_ref().contains("planning step 4"))
    }));
    Ok(())
}

#[test]
fn empty_reasoning_delta_does_not_create_empty_thinking_block() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());

    app.handle(RunEvent::ReasoningDelta(String::new()))?;
    app.handle(RunEvent::ReasoningDelta("\n  \t".to_owned()))?;

    assert!(
        !app.timeline
            .iter()
            .any(|entry| entry.role == TimelineRole::Thinking)
    );

    app.handle(RunEvent::ReasoningDelta("Still".to_owned()))?;
    app.handle(RunEvent::ReasoningDelta(" ".to_owned()))?;
    app.handle(RunEvent::ReasoningDelta("running".to_owned()))?;

    let thinking = app
        .timeline
        .iter()
        .filter(|entry| entry.role == TimelineRole::Thinking)
        .collect::<Vec<_>>();
    assert_eq!(thinking.len(), 1);
    assert_eq!(thinking[0].text, "Still running");
    Ok(())
}

#[test]
fn ctrl_t_toggles_thinking_block_expansion() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());

    app.handle(RunEvent::ReasoningDelta("planning step 1".to_owned()))?;
    app.handle(RunEvent::ReasoningDelta("\nplanning step 2".to_owned()))?;
    app.handle(RunEvent::ReasoningDelta(
        "\nplanning step 3\nplanning step 4".to_owned(),
    ))?;
    app.handle(RunEvent::ToolCallStarted(ToolCall {
        id: "call-1".to_owned(),
        name: "read_file".to_owned(),
        args_json: "{}".to_owned(),
    }))?;

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
fn ctrl_t_toggles_thinking_from_activity_without_tool_selection() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());

    app.handle(RunEvent::ReasoningDelta(
        "planning step 1\nplanning step 2\nplanning step 3\nplanning step 4".to_owned(),
    ))?;
    app.handle(RunEvent::ToolCallStarted(ToolCall {
        id: "call-1".to_owned(),
        name: "read_file".to_owned(),
        args_json: "{}".to_owned(),
    }))?;
    app.active_pane = PaneFocus::Activity;
    app.selected_tool_activity_key = None;

    app.handle_key_event(KeyEvent::new(KeyCode::Char('t'), KeyModifiers::CONTROL))?;

    assert_eq!(app.last_notice(), Some("thinking expanded"));
    let expanded = transcript_plain(app.transcript_lines(20));
    assert!(expanded.contains("Ctrl-T collapse"));
    assert!(expanded.contains("planning step 4"));
    Ok(())
}

#[test]
fn ctrl_t_ignores_thinking_without_hidden_content() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());

    app.handle(RunEvent::ReasoningDelta("single visible step".to_owned()))?;
    app.handle(RunEvent::ToolCallStarted(ToolCall {
        id: "call-1".to_owned(),
        name: "read_file".to_owned(),
        args_json: "{}".to_owned(),
    }))?;
    let rendered = transcript_plain(app.transcript_lines(20));
    assert!(rendered.contains("single visible step"));
    assert!(!rendered.contains("Ctrl-T"));
    let thinking_view_events_before = app
        .events
        .iter()
        .filter(|event| event.label == "thinking:view")
        .count();

    app.handle_key_event(KeyEvent::new(KeyCode::Char('t'), KeyModifiers::CONTROL))?;

    let rendered_after = transcript_plain(app.transcript_lines(20));
    assert!(rendered_after.contains("single visible step"));
    assert!(!rendered_after.contains("Ctrl-T"));
    assert_eq!(
        app.events
            .iter()
            .filter(|event| event.label == "thinking:view")
            .count(),
        thinking_view_events_before
    );
    assert_ne!(app.last_notice(), Some("thinking expanded"));
    assert_ne!(app.last_notice(), Some("thinking collapsed"));
    Ok(())
}

#[test]
fn thinking_entry_toggle_handles_missing_uncollapsible_and_global_override() -> Result<()> {
    let mut short_app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    short_app.handle(RunEvent::ReasoningDelta("single visible step".to_owned()))?;
    short_app.handle(RunEvent::ToolCallStarted(ToolCall {
        id: "call-1".to_owned(),
        name: "read_file".to_owned(),
        args_json: "{}".to_owned(),
    }))?;
    let short_index = short_app
        .timeline
        .iter()
        .position(|entry| entry.role == TimelineRole::Thinking)
        .expect("expected thinking entry");

    assert!(!short_app.toggle_thinking_entry(usize::MAX));
    assert!(!short_app.toggle_thinking_entry(short_index));

    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.handle(RunEvent::ReasoningDelta(
        "planning step 1\nplanning step 2\nplanning step 3\nplanning step 4".to_owned(),
    ))?;
    app.handle(RunEvent::ToolCallStarted(ToolCall {
        id: "call-2".to_owned(),
        name: "read_file".to_owned(),
        args_json: "{}".to_owned(),
    }))?;
    let entry_index = app.collapsible_thinking_entry_indices()[0];

    app.toggle_thinking_block_mode();
    let expanded = transcript_plain(app.transcript_lines(20));
    assert!(expanded.contains("Ctrl-T collapse"));
    assert!(expanded.contains("planning step 4"));

    assert!(app.toggle_thinking_entry(entry_index));
    let collapsed = transcript_plain(app.transcript_lines(20));
    assert!(collapsed.contains("Ctrl-T expand"));
    assert!(!collapsed.contains("planning step 4"));
    Ok(())
}

#[test]
fn ctrl_t_expands_thinking_when_tool_selection_is_stale_in_composer() -> Result<()> {
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
    let tool_key = "call:call-first".to_owned();
    assert_eq!(app.selected_tool_activity_key, Some(tool_key.clone()));
    assert_eq!(app.active_pane, PaneFocus::Composer);

    app.handle(RunEvent::ReasoningDelta("planning step 1".to_owned()))?;
    app.handle(RunEvent::ReasoningDelta(
        "\nplanning step 2\nplanning step 3\nplanning step 4".to_owned(),
    ))?;
    app.handle(RunEvent::ToolCallStarted(ToolCall {
        id: "call-2".to_owned(),
        name: "read_file".to_owned(),
        args_json: "{}".to_owned(),
    }))?;
    let collapsed = app.transcript_lines(20);
    assert!(collapsed.iter().any(|line| {
        line.spans
            .iter()
            .any(|span| span.content.as_ref().contains("Ctrl-T expand"))
    }));

    app.handle_key_event(KeyEvent::new(KeyCode::Char('t'), KeyModifiers::CONTROL))?;

    assert_eq!(app.last_notice(), Some("thinking expanded"));
    assert!(!app.expanded_tool_activity_keys.contains(&tool_key));
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
    Ok(())
}

#[test]
fn tool_result_is_rendered_as_multiline_json_block() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());

    app.handle(RunEvent::ToolResult(sigil_kernel::ToolResult::ok(
        "call-1".to_owned(),
        "ls".to_owned(),
        "[\".git\",\"Cargo.toml\"]".to_owned(),
        sigil_kernel::ToolResultMeta::default(),
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
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &config);
    let preview = ToolPreview {
        title: "Update secrets.txt".to_owned(),
        summary: "Preview summary".to_owned(),
        body: "--- current/secrets.txt\n+++ proposed/secrets.txt\n@@ -1 +1 @@\n-old\n+sk-ui-secret"
            .to_owned(),
        changed_files: vec!["secrets.txt".to_owned()],
        file_diffs: vec![sigil_kernel::ToolPreviewFile {
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
    assert!(serialized.contains(sigil_kernel::REDACTED_SECRET));
    assert_eq!(
        rendered["metadata"]["details"]["api_key"],
        sigil_kernel::REDACTED_SECRET
    );
    assert_eq!(
        rendered["preview_value"]["token"],
        sigil_kernel::REDACTED_SECRET
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
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &config);
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
    assert!(serialized.contains(sigil_kernel::REDACTED_SECRET));
    Ok(())
}

#[test]
fn batched_streaming_text_deltas_rerender_once_after_drain() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());

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
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
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
fn agent_tool_pre_tool_streaming_text_is_thinking_not_assistant() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());

    app.handle(RunEvent::TextDelta("parent pre-tool analysis".to_owned()))?;
    app.handle(RunEvent::ToolCallStarted(ToolCall {
        id: "call-agent-1".to_owned(),
        name: "spawn_agent".to_owned(),
        args_json: "{}".to_owned(),
    }))?;

    let entry = app
        .timeline
        .iter()
        .find(|entry| entry.text == "parent pre-tool analysis")
        .expect("streaming entry should remain");
    assert_eq!(entry.role, TimelineRole::Thinking);
    assert_eq!(entry.text, "parent pre-tool analysis");
    Ok(())
}

#[test]
fn assistant_message_before_tool_remains_visible() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());

    app.handle(RunEvent::AssistantMessage(ModelMessage::assistant(
        Some("checking provider shape".to_owned()),
        Vec::new(),
    )))?;
    let before_tool = transcript_plain_lines(app.transcript_lines(app.timeline_viewport_rows()));
    assert!(
        before_tool
            .iter()
            .any(|line| line.contains("checking provider shape"))
    );
    assert!(
        !before_tool
            .iter()
            .any(|line| line.contains("• checking provider shape"))
    );

    app.handle(RunEvent::ToolResult(ToolResult::ok(
        "call-1".to_owned(),
        "read_file".to_owned(),
        "file contents",
        ToolResultMeta::default(),
    )))?;

    let after_tool = transcript_plain_lines(app.transcript_lines(app.timeline_viewport_rows()));
    assert!(
        after_tool
            .iter()
            .any(|line| line.contains("checking provider shape"))
    );
    assert!(
        after_tool
            .iter()
            .any(|line| line.contains("• checking provider shape"))
    );

    app.handle(RunEvent::AssistantMessage(ModelMessage::assistant(
        Some("final answer".to_owned()),
        Vec::new(),
    )))?;

    let after_final = transcript_plain_lines(app.transcript_lines(app.timeline_viewport_rows()));
    assert!(after_final.iter().any(|line| line.contains("final answer")));
    assert!(
        !after_final
            .iter()
            .any(|line| line.contains("• final answer"))
    );
    Ok(())
}

#[test]
fn streaming_deltas_do_not_fill_ui_event_log() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
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

#[test]
fn timeline_cache_and_scroll_edges_cover_empty_and_guard_paths() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.timeline.clear();
    app.timeline_render_cache.clear();
    app.timeline_plain_cache.clear();
    app.timeline_prefix_hashes.clear();
    app.timeline_render_ranges.clear();

    assert_eq!(app.effective_timeline_render_len(), 0);
    assert_eq!(app.scrollback_prefix_hash(0), 0);
    assert_eq!(app.visible_timeline_render_range(10), 0..0);
    assert_eq!(
        app.transcript_lines(10)
            .into_iter()
            .map(|line| line
                .spans
                .into_iter()
                .map(|span| span.content.into_owned())
                .collect::<String>())
            .collect::<Vec<_>>(),
        vec![
            "no messages yet".to_owned(),
            "send a prompt to start".to_owned()
        ]
    );

    app.rerender_timeline_entry(99);
    app.append_timeline_render_cache_entry(0);
    app.extend_last_render_block_range_by_one_line();

    app.timeline.push(crate::timeline::TimelineEntry {
        role: TimelineRole::Notice,
        text: "manual notice".to_owned(),
    });
    app.append_timeline_render_cache_entry(2);
    assert_eq!(app.timeline_render_ranges.len(), app.timeline.len());

    app.timeline_render_cache = vec![Line::raw("visible")];
    app.timeline_plain_cache = vec!["visible".to_owned()];
    app.timeline_prefix_hashes = vec![99];
    app.rebuild_timeline_prefix_hashes_from(0);
    assert_ne!(app.timeline_prefix_hashes[0], 99);
    let hash_after_zero_rebuild = app.timeline_prefix_hashes[0];
    app.rebuild_timeline_prefix_hashes_from(1);
    assert_eq!(app.timeline_prefix_hashes[0], hash_after_zero_rebuild);

    app.timeline_render_cache.push(Line::default());
    app.timeline_plain_cache.push(String::new());
    app.timeline_prefix_hashes.push(0);
    app.timeline_render_ranges = std::iter::once(0..2).collect();
    app.trim_trailing_timeline_blanks();
    assert_eq!(
        app.timeline_render_ranges,
        std::iter::once(0..1).collect::<Vec<_>>()
    );

    app.timeline_render_cache.push(Line::default());
    app.timeline_plain_cache.push(String::new());
    app.timeline_prefix_hashes.push(0);
    app.timeline_render_ranges = std::iter::once(1..1).collect();
    app.trim_trailing_timeline_blanks();
    assert!(app.timeline_render_ranges.is_empty());

    app.timeline.clear();
    app.timeline_render_cache.clear();
    app.timeline_plain_cache.clear();
    app.timeline_prefix_hashes.clear();
    app.timeline_render_ranges.clear();
    app.push_timeline(TimelineRole::Assistant, "streaming answer");
    app.streaming_assistant_index = Some(0);
    app.runtime.is_busy = true;
    assert_eq!(app.scrollback_cutoff_line(), 0);
    Ok(())
}

#[test]
fn parent_scrollback_clamps_stale_parent_cache_while_child_view_is_active() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    sync_child_agent_for_transcript_tests(&mut app)?;
    app.set_terminal_size(80, 1);
    app.timeline = vec![TimelineEntry {
        role: TimelineRole::User,
        text: "parent prompt".to_owned(),
    }];
    app.timeline_render_cache = vec![Line::raw("parent prompt")];
    app.timeline_plain_cache = vec!["parent prompt".to_owned()];
    app.timeline_prefix_hashes = vec![1];
    app.timeline_render_ranges = std::iter::once(0..2).collect();
    app.active_agent_child_transcript = Some(super::super::ActiveAgentChildTranscript {
        path: PathBuf::from("children/task_1/step_1-child_1.jsonl"),
        file_signature: super::super::ChildTranscriptFileSignature::empty(),
        timeline_entries: Vec::new(),
        rendered_body_lines: (0..8)
            .map(|index| Line::from(format!("child line {index}")))
            .collect(),
        total_timeline_entries: 8,
        transcript_truncated: false,
        load_error: None,
    });

    assert_eq!(
        app.scrollback_cutoff_line(),
        app.timeline_render_cache.len()
    );
    assert_eq!(
        app.scrollback_lines().len(),
        app.timeline_render_cache.len()
    );
    assert!(app.visible_timeline_render_range(4).end <= app.timeline_render_cache.len());
    assert!(transcript_plain(app.transcript_lines(12)).contains("child line"));
    Ok(())
}

#[test]
fn child_agent_transcript_lines_cover_load_states_and_viewport_edges() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    sync_child_agent_for_transcript_tests(&mut app)?;

    let header_only = transcript_plain(app.transcript_lines(1));
    assert!(header_only.contains("agent view"));
    assert!(!header_only.contains("session:"));

    app.active_agent_child_transcript = None;
    let unloaded = transcript_plain(app.transcript_lines(8));
    assert!(unloaded.contains("repo read"));
    assert!(unloaded.contains("child session not loaded"));

    app.active_agent_child_transcript = Some(super::super::ActiveAgentChildTranscript {
        path: PathBuf::from("children/task_1/step_1-child_1.jsonl"),
        file_signature: super::super::ChildTranscriptFileSignature::empty(),
        timeline_entries: Vec::new(),
        rendered_body_lines: Vec::new(),
        total_timeline_entries: 0,
        transcript_truncated: false,
        load_error: Some("permission denied opening child session".to_owned()),
    });
    let load_error = transcript_plain(app.transcript_lines(8));
    assert!(load_error.contains("load error: permission denied"));
    assert!(load_error.contains("path: children/task_1/step_1-child_1.jsonl"));

    app.active_agent_child_transcript = Some(super::super::ActiveAgentChildTranscript {
        path: PathBuf::from("children/task_1/step_1-child_1.jsonl"),
        file_signature: super::super::ChildTranscriptFileSignature::empty(),
        timeline_entries: Vec::new(),
        rendered_body_lines: Vec::new(),
        total_timeline_entries: 0,
        transcript_truncated: false,
        load_error: None,
    });
    let empty = transcript_plain(app.transcript_lines(8));
    assert!(empty.contains("child session has no transcript messages yet"));

    app.active_agent_child_transcript = Some(super::super::ActiveAgentChildTranscript {
        path: PathBuf::from("children/task_1/step_1-child_1.jsonl"),
        file_signature: super::super::ChildTranscriptFileSignature::empty(),
        timeline_entries: vec![
            TimelineEntry {
                role: TimelineRole::User,
                text: "child prompt".to_owned(),
            },
            TimelineEntry {
                role: TimelineRole::Assistant,
                text: "child answer".to_owned(),
            },
        ],
        rendered_body_lines: vec![Line::from("child prompt"), Line::from("child answer")],
        total_timeline_entries: 2,
        transcript_truncated: false,
        load_error: None,
    });
    let restored = transcript_plain(app.transcript_lines(12));
    assert!(restored.contains("child prompt"));
    assert!(restored.contains("child answer"));
    Ok(())
}

#[test]
fn running_child_agent_transcript_keeps_latest_thinking_active() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    sync_child_agent_for_transcript_tests(&mut app)?;
    let timeline_entries = vec![TimelineEntry {
        role: TimelineRole::Thinking,
        text: "child step 1\nchild step 2\nchild step 3\nchild step 4".to_owned(),
    }];
    let rendered_body_lines = app.render_child_timeline_body_lines(&timeline_entries);
    app.active_agent_child_transcript = Some(super::super::ActiveAgentChildTranscript {
        path: PathBuf::from("children/task_1/step_1-child_1.jsonl"),
        file_signature: super::super::ChildTranscriptFileSignature::empty(),
        timeline_entries,
        rendered_body_lines,
        total_timeline_entries: 1,
        transcript_truncated: false,
        load_error: None,
    });

    let rendered = transcript_plain(app.transcript_lines(16));

    assert!(rendered.contains("thinking"));
    assert!(!rendered.contains("thought"));
    assert!(rendered.contains("child step 4"));
    Ok(())
}

#[test]
fn child_agent_transcript_uses_cached_bounded_timeline_entries() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    sync_child_agent_for_transcript_tests(&mut app)?;
    let entries = (0..96)
        .map(|index| TimelineEntry {
            role: TimelineRole::Assistant,
            text: format!("child entry {index}"),
        })
        .collect::<Vec<_>>();
    app.active_agent_child_transcript = Some(super::super::ActiveAgentChildTranscript {
        path: PathBuf::from("children/task_1/step_1-child_1.jsonl"),
        file_signature: super::super::ChildTranscriptFileSignature::empty(),
        timeline_entries: entries[16..].to_vec(),
        rendered_body_lines: entries[16..]
            .iter()
            .map(|entry| Line::from(entry.text.clone()))
            .collect(),
        total_timeline_entries: entries.len(),
        transcript_truncated: false,
        load_error: None,
    });

    let rendered = transcript_plain(app.transcript_lines(16));

    assert!(rendered.contains("showing latest 80 of 96 child transcript entries"));
    assert!(!rendered.contains("child entry 0"));
    assert!(rendered.contains("child entry 95"));
    Ok(())
}

#[test]
fn child_agent_transcript_reload_uses_tail_and_skips_unchanged_files() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.session_log_path = temp.path().join("parent.jsonl");
    sync_child_agent_for_transcript_tests(&mut app)?;
    let child_path = temp.path().join("children/task_1/step_1-child_1.jsonl");
    std::fs::create_dir_all(child_path.parent().expect("child path has parent"))?;
    let mut child_log = String::new();
    for index in 0..1500 {
        child_log.push_str(&serde_json::to_string(&SessionLogEntry::Assistant(
            ModelMessage::assistant(Some(format!("child message {index}")), Vec::new()),
        ))?);
        child_log.push('\n');
    }
    std::fs::write(&child_path, child_log)?;

    app.reload_active_agent_child_transcript();
    let rendered = transcript_plain(app.transcript_lines(16));

    assert!(rendered.contains("showing latest 80 child transcript entries"));
    assert!(!rendered.contains("child message 0"));
    assert!(rendered.contains("child message 1499"));

    let transcript = app
        .active_agent_child_transcript
        .as_mut()
        .expect("child transcript");
    transcript.rendered_body_lines = vec![Line::from("cached sentinel")];
    app.reload_active_agent_child_transcript();
    let unchanged = transcript_plain(app.transcript_lines(16));

    assert!(unchanged.contains("cached sentinel"));
    Ok(())
}

#[test]
fn running_child_agent_parent_sync_does_not_reload_changing_transcript() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.session_log_path = temp.path().join("parent.jsonl");
    sync_child_agent_for_transcript_tests(&mut app)?;
    let child_path = temp.path().join("children/task_1/step_1-child_1.jsonl");
    let store = sigil_kernel::JsonlSessionStore::new(&child_path)?;
    store.append(&SessionLogEntry::Assistant(ModelMessage::assistant(
        Some("first child line".to_owned()),
        Vec::new(),
    )))?;
    app.reload_active_agent_child_transcript();
    let transcript = app
        .active_agent_child_transcript
        .as_mut()
        .expect("child transcript");
    transcript.rendered_body_lines = vec![Line::from("running cached transcript")];

    store.append(&SessionLogEntry::Assistant(ModelMessage::assistant(
        Some("second child line".to_owned()),
        Vec::new(),
    )))?;
    app.refresh_active_agent_view_after_parent_sync();
    let rendered = transcript_plain(app.transcript_lines(16));

    assert!(rendered.contains("running cached transcript"));
    assert!(!rendered.contains("second child line"));
    assert!(app.poll_background_tasks());
    let refreshed = transcript_plain(app.transcript_lines(16));
    assert!(refreshed.contains("second child line"));
    assert!(!app.poll_background_tasks());
    Ok(())
}

#[test]
fn missing_child_agent_transcript_load_error_is_cached() -> Result<()> {
    let temp = tempfile::tempdir()?;
    std::fs::write(temp.path().join("children"), "not a directory")?;
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.session_log_path = temp.path().join("parent.jsonl");
    app.active_agent_view = super::super::AgentView::Child {
        child_task_id: "missing_child".to_owned(),
        child_session_ref: sigil_kernel::SessionRef::new_relative("children/missing.jsonl")?,
    };

    assert!(app.reload_active_agent_child_transcript());
    let transcript = app
        .active_agent_child_transcript
        .as_ref()
        .expect("missing child transcript should record load error");
    assert!(transcript.load_error.is_some());
    assert_eq!(
        transcript.file_signature,
        super::super::ChildTranscriptFileSignature::empty()
    );
    assert!(transcript.timeline_entries.is_empty());
    assert!(transcript.rendered_body_lines.is_empty());

    assert!(!app.reload_active_agent_child_transcript());
    Ok(())
}

#[test]
fn timeline_scroll_and_live_summary_edges_cover_pending_and_busy_states() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.set_terminal_size(80, 6);
    for index in 0..12 {
        app.push_timeline(TimelineRole::Notice, format!("notice {index}"));
    }

    app.handle_mouse_scroll(true);
    assert!(app.timeline_scroll_back > 0);
    app.handle_mouse_scroll(false);
    assert_eq!(app.timeline_scroll_back, 0);

    inject_write_file_approval(&mut app, sample_approval_preview())?;
    app.handle_mouse_scroll(false);
    assert_eq!(app.approval.scroll_back, 3);
    app.handle_mouse_scroll(true);
    assert_eq!(app.approval.scroll_back, 0);

    app.active_pane = PaneFocus::Activity;
    app.scroll_active_pane(2);
    assert_eq!(app.approval.scroll_back, 0);
    app.unscroll_active_pane(4);
    assert_eq!(app.approval.scroll_back, 4);
    app.approval.pending = None;
    app.scroll_active_pane(5);
    assert_eq!(app.activity_scroll_back, 5);
    app.unscroll_active_pane(3);
    assert_eq!(app.activity_scroll_back, 2);

    assert!(app.live_activity_summary().is_none());
    app.runtime.is_busy = true;
    app.runtime.run_phase = RunPhase::Idle;
    assert_eq!(
        app.live_activity_summary()
            .map(|summary| (summary.label, summary.detail)),
        Some(("working".to_owned(), "waiting for next event".to_owned()))
    );
    app.runtime.run_phase = RunPhase::Tool("bash".to_owned());
    assert_eq!(
        app.live_activity_summary()
            .map(|summary| (summary.label, summary.detail)),
        Some(("tool".to_owned(), "running bash".to_owned()))
    );
    app.runtime.run_phase = RunPhase::Streaming;
    assert_eq!(
        app.live_activity_summary()
            .map(|summary| (summary.label, summary.detail)),
        Some(("streaming".to_owned(), "writing the reply".to_owned()))
    );
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
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
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
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
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
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
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
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
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
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
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
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
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
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
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
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
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
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
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
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
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
    let live = transcript_plain(app.transcript_lines(app.timeline_viewport_rows()));

    assert!(!rendered.contains("Inspected"));
    assert!(rendered.contains("notice"));
    assert!(rendered.contains("permission ls subject=crates mode=allow"));
    assert!(rendered.contains("permission read_file subject=README.md mode=allow"));
    assert!(rendered.contains("Listed crates"));
    assert!(rendered.contains("Read README.md"));
    assert!(live.contains("notice"));
    assert!(live.contains("permission read_file subject=README.md mode=allow"));
}

#[test]
fn file_changes_and_complex_bash_do_not_create_inspected_group() {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
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
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
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
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
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
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &config);

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
    assert_eq!(app.runtime.compaction_status, "soft");

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

    assert_eq!(app.runtime.compaction_status, "ready");
    Ok(())
}

#[test]
fn context_usage_and_compaction_policy_share_effective_window() -> Result<()> {
    let mut config = test_config();
    config.agent.model = "deepseek-v4-pro".to_owned();
    config.compaction.context_window_tokens = Some(128_000);
    config.compaction.soft_threshold_ratio = 0.5;
    config.compaction.hard_threshold_ratio = 0.8;
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &config);

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

    assert_eq!(
        app.context_usage_line(),
        "ctx: 9% · prompt 90.4K / 1.0M provider · soft at 500.0K"
    );
    assert_eq!(app.runtime.compaction_status, "ready");
    assert!(app.footer_status_line().contains("tok 90.4K"));
    assert!(app.footer_status_line().contains("ctx 9%"));
    assert!(
        app.usage_sidebar_lines().iter().any(
            |line| line == "policy: provider 1,000,000 · soft 50% (500.0K) · hard 80% (800.0K)"
        )
    );

    config.agent.provider = "custom".to_owned();
    config.agent.model = "custom-model".to_owned();
    let mut fallback_app = AppState::from_root_config(Path::new("sigil.toml"), &config);
    fallback_app.handle(RunEvent::Usage(UsageStats {
        prompt_tokens: 64_000,
        completion_tokens: 0,
        cache_hit_tokens: 0,
        cache_miss_tokens: 64_000,
        input_cost: 0.0,
        output_cost: 0.0,
        cache_savings: 0.0,
        system_fingerprint: None,
    }))?;
    assert_eq!(
        fallback_app.context_usage_line(),
        "ctx: 50% · prompt 64.0K / 128.0K fallback · soft; /compact"
    );
    Ok(())
}

#[test]
fn usage_display_shows_session_and_delta_costs() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());

    app.handle(RunEvent::Usage(UsageStats {
        prompt_tokens: 100,
        completion_tokens: 40,
        cache_hit_tokens: 75,
        cache_miss_tokens: 25,
        input_cost: 0.12,
        output_cost: 0.03,
        cache_savings: 0.45,
        system_fingerprint: None,
    }))?;

    assert!(
        app.usage_sidebar_lines()
            .iter()
            .any(|line| line == "session tok: input 100 · output 40")
    );
    assert!(
        app.usage_sidebar_lines()
            .iter()
            .any(|line| line == "cache: 75% · save USD 0.4500")
    );
    assert!(
        app.usage_sidebar_lines()
            .iter()
            .any(|line| line == "total spent: USD 0.1500")
    );
    assert!(
        app.usage_sidebar_lines()
            .iter()
            .any(|line| line == "spent since opening: USD 0.1500")
    );
    assert!(
        app.footer_status_line()
            .contains("spent USD 0.1500 since opening / USD 0.1500 total")
    );
    assert!(
        !app.usage_sidebar_lines()
            .iter()
            .any(|line| line.contains('$'))
    );
    Ok(())
}

#[test]
fn session_delta_stats_reset_on_session_switch_and_follow_balance_currency() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.runtime.balance_snapshot = sigil_runtime::BalanceSnapshot {
        total: Some(12.34),
        currency: Some("CNY".to_owned()),
        available: true,
        status: "CNY 12.34".to_owned(),
    };
    let restored_path = app
        .workspace_root
        .join(".sigil/sessions/session-restored.jsonl");

    app.handle(RunEvent::Usage(UsageStats {
        prompt_tokens: 100,
        completion_tokens: 10,
        cache_hit_tokens: 50,
        cache_miss_tokens: 50,
        input_cost: 0.20,
        output_cost: 0.05,
        cache_savings: 0.10,
        system_fingerprint: None,
    }))?;
    assert_eq!(app.runtime.session_delta_stats.input_cost, 0.20);

    app.handle_worker_message(WorkerMessage::SessionSwitched {
        session_log_path: restored_path,
        provider_name: "deepseek".to_owned(),
        model_name: "deepseek-v4-flash".to_owned(),
        entries: vec![SessionLogEntry::Control(ControlEntry::UsageSnapshot(
            UsageStats {
                prompt_tokens: 200,
                completion_tokens: 20,
                cache_hit_tokens: 120,
                cache_miss_tokens: 80,
                input_cost: 1.00,
                output_cost: 0.50,
                cache_savings: 2.00,
                system_fingerprint: None,
            },
        ))],
    })?;

    assert_eq!(
        app.runtime.stats.input_cost + app.runtime.stats.output_cost,
        1.50
    );
    assert_eq!(
        app.runtime.session_delta_stats.input_cost + app.runtime.session_delta_stats.output_cost,
        0.0
    );
    assert!(
        app.usage_sidebar_lines()
            .iter()
            .any(|line| line == "total spent: CNY 10.8000")
    );
    assert!(
        app.usage_sidebar_lines()
            .iter()
            .any(|line| line == "spent since opening: CNY 0.0000")
    );

    app.handle(RunEvent::Usage(UsageStats {
        prompt_tokens: 100,
        completion_tokens: 40,
        cache_hit_tokens: 75,
        cache_miss_tokens: 25,
        input_cost: 0.12,
        output_cost: 0.03,
        cache_savings: 0.45,
        system_fingerprint: None,
    }))?;

    assert!(
        app.usage_sidebar_lines()
            .iter()
            .any(|line| line == "total spent: CNY 11.8800")
    );
    assert!(
        app.usage_sidebar_lines()
            .iter()
            .any(|line| line == "spent since opening: CNY 1.0800")
    );
    assert!(
        app.footer_status_line()
            .contains("spent CNY 1.0800 since opening / CNY 11.8800 total")
    );
    Ok(())
}

#[test]
fn usage_display_prefers_configured_cost_currency_over_balance_currency() -> Result<()> {
    let mut config = test_config();
    config.appearance.usage_cost_currency = sigil_kernel::UsageCostCurrency::Cny;
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &config);
    app.runtime.balance_snapshot = sigil_runtime::BalanceSnapshot {
        total: Some(3.25),
        currency: Some("USD".to_owned()),
        available: true,
        status: "USD 3.25".to_owned(),
    };

    app.handle(RunEvent::Usage(UsageStats {
        prompt_tokens: 100,
        completion_tokens: 40,
        cache_hit_tokens: 75,
        cache_miss_tokens: 25,
        input_cost: 0.12,
        output_cost: 0.03,
        cache_savings: 0.45,
        system_fingerprint: None,
    }))?;

    assert!(
        app.usage_sidebar_lines()
            .iter()
            .any(|line| line == "total spent: CNY 1.0800")
    );
    assert!(
        app.footer_status_line()
            .contains("spent CNY 1.0800 since opening / CNY 1.0800 total")
    );
    Ok(())
}

#[test]
fn activity_pane_keymap_preserves_composer_shortcuts_and_sidebar_navigation() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.active_pane = PaneFocus::Activity;

    app.handle_key_event(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE))?;
    let rows = app.agent_sidebar_rows();
    assert!(rows.iter().any(|row| row.label == "main" && row.selected));

    app.handle_key_event(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE))?;
    assert_eq!(app.sidebar_selected_card, SidebarCard::Usage);

    sync_child_agent_for_transcript_tests(&mut app)?;
    app.active_pane = PaneFocus::Activity;
    app.sidebar_selected_card = SidebarCard::Agents;
    app.sidebar_agent_selected = 0;
    app.handle_key_event(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE))?;
    assert_eq!(app.sidebar_selected_card, SidebarCard::Agents);
    assert_eq!(app.sidebar_agent_selected, 1);

    app.handle_key_event(KeyEvent::new(KeyCode::PageDown, KeyModifiers::NONE))?;
    app.handle_key_event(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE))?;
    assert_eq!(app.active_pane, PaneFocus::Activity);

    app.handle_key_event(KeyEvent::new(KeyCode::Char('/'), KeyModifiers::NONE))?;
    assert_eq!(app.active_pane, PaneFocus::Composer);
    assert_eq!(app.composer.input, "/");

    app.active_pane = PaneFocus::Activity;
    app.handle_key_event(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE))?;
    assert_eq!(app.active_pane, PaneFocus::Composer);
    Ok(())
}

#[test]
fn busy_escape_requests_cancel_without_discarding_composer_text() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.runtime.is_busy = true;
    app.composer.input = "keep draft".to_owned();
    app.composer.input_cursor = app.composer.input.chars().count();

    let action = app.handle_key_event(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE))?;

    assert!(matches!(action, Some(AppAction::CancelRun)));
    assert_eq!(app.composer.input, "keep draft");
    assert_eq!(app.last_notice(), Some("cancellation requested"));
    assert!(
        app.timeline
            .iter()
            .any(|entry| entry.role == TimelineRole::Notice && entry.text == "cancel requested")
    );
    Ok(())
}

#[test]
fn slash_command_busy_and_unknown_paths_leave_tui_responsive() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());

    app.composer.input = "/unknown".to_owned();
    assert!(app.submit_input()?.is_none());
    assert_eq!(app.last_notice(), Some("unknown slash command"));

    app.runtime.is_busy = true;
    app.composer.input = "/compact".to_owned();
    assert!(app.submit_input()?.is_none());
    assert!(
        app.timeline
            .iter()
            .any(|entry| entry.role == TimelineRole::Notice && entry.text == "busy; compact later")
    );

    app.composer.input = "/resume missing".to_owned();
    assert!(app.submit_input()?.is_none());
    assert!(
        app.timeline
            .iter()
            .any(|entry| entry.role == TimelineRole::Notice && entry.text == "busy; resume later")
    );
    Ok(())
}

#[test]
fn app_status_helpers_cover_empty_balance_context_and_session_title() {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.runtime.balance_snapshot.available = true;
    app.runtime.balance_snapshot.total = None;
    app.runtime.balance_snapshot.currency = None;
    app.runtime.balance_snapshot.status = "checking".to_owned();
    app.runtime.stats.last_prompt_tokens = 1234;

    assert_eq!(app.balance_sidebar_line(), "balance: checking");
    assert_eq!(
        app.context_usage_line(),
        "ctx: 0% · prompt 1.2K / 1.0M provider · soft at 500.0K"
    );
    let policy = app.compaction_policy_line();
    assert!(policy.starts_with("policy: "));
    assert!(policy.contains("provider"));
    assert!(policy.contains("soft"));
    assert!(policy.contains("hard"));
    assert_eq!(app.permission_card_lines()[2], "scope: saved default");
    assert!(app.session_display_title().contains("deepseek-v4-flash"));

    app.push_timeline(TimelineRole::User, "\n\nfirst line\nsecond line");
    assert_eq!(
        app.latest_user_prompt_preview(),
        Some("first line  +1 more".to_owned())
    );
    assert_eq!(app.session_display_title(), "first line");

    app.runtime.is_busy = true;
    assert_eq!(app.permission_card_lines()[2], "busy: locked during run");
    assert!(app.footer_status_line().contains("Ctrl-C cancel"));
}

#[test]
fn live_activity_summary_tracks_busy_phase() {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    assert!(app.live_activity_summary().is_none());

    app.runtime.is_busy = true;
    app.runtime.run_phase = RunPhase::Tool("read_file".to_owned());

    let summary = app.live_activity_summary().expect("expected live summary");
    assert_eq!(summary.label, "tool");
    assert_eq!(summary.detail, "running read_file");
}

#[test]
fn child_agent_view_live_activity_overrides_parent_busy_phase() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    sync_child_agent_for_transcript_tests(&mut app)?;
    app.runtime.is_busy = true;
    app.runtime.run_phase = RunPhase::Tool("wait_agent".to_owned());

    let summary = app.live_activity_summary().expect("child live summary");

    assert_eq!(summary.label, "agent");
    assert!(summary.detail.contains("repo read"));
    assert!(summary.detail.contains("started"));
    assert!(!summary.detail.contains("wait_agent"));
    assert!(matches!(app.live_panel_phase(), RunPhase::Agent(_)));
    Ok(())
}

#[test]
fn terminal_child_agent_view_does_not_render_working_progress() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    let thread_id = sigil_kernel::AgentThreadId::new("agent_chat_terminal")?;
    let profile_id = sigil_kernel::AgentProfileId::new("explore")?;
    let snapshot_id = sigil_kernel::AgentProfileSnapshotId::new("profile_snapshot_terminal")?;
    let session_ref = sigil_kernel::SessionRef::new_relative("children/agent_chat_terminal.jsonl")?;
    app.sync_current_session_state(vec![
        SessionLogEntry::Control(ControlEntry::AgentProfileCaptured(
            sigil_kernel::AgentProfileCapturedEntry {
                snapshot: sigil_kernel::AgentProfileSnapshot {
                    snapshot_id: snapshot_id.clone(),
                    profile_id: profile_id.clone(),
                    source: sigil_kernel::AgentProfileSource::System,
                    source_hash: "sha256:source".to_owned(),
                    profile_hash: "sha256:profile".to_owned(),
                    resolved_tool_scope_hash: "sha256:tools".to_owned(),
                    resolved_permission_policy_hash: "sha256:permissions".to_owned(),
                    resolved_mcp_scope_hash: "sha256:mcp".to_owned(),
                    resolved_skill_hashes: Vec::new(),
                    trust_state: sigil_kernel::AgentTrustState::Trusted,
                },
            },
        )),
        SessionLogEntry::Control(ControlEntry::AgentThreadStarted(
            sigil_kernel::AgentThreadStartedEntry {
                thread_id: thread_id.clone(),
                parent_thread_id: Some(sigil_kernel::AgentThreadId::new("main")?),
                parent_session_ref: sigil_kernel::SessionRef::new_relative("parent.jsonl")?,
                thread_session_ref: session_ref.clone(),
                profile_id,
                profile_snapshot_id: snapshot_id.clone(),
                run_context: sigil_kernel::AgentRunContextSnapshot {
                    profile_snapshot_id: snapshot_id.clone(),
                    provider: "deepseek".to_owned(),
                    model: "deepseek-v4-pro".to_owned(),
                    reasoning_effort: None,
                    workspace_root: sigil_kernel::WorkspaceRootSnapshot::new(
                        "/tmp/workspace".to_owned(),
                    )?,
                    effective_tool_scope_hash: "sha256:tools".to_owned(),
                    effective_permission_policy_hash: "sha256:permissions".to_owned(),
                    effective_mcp_scope_hash: "sha256:mcp".to_owned(),
                    provider_capability_hash: "sha256:provider".to_owned(),
                    model_visible_agent_index_hash: Some("sha256:index".to_owned()),
                    budget_policy_hash: "sha256:budget".to_owned(),
                    provider_background_handle_ref: None,
                },
                objective: "inspect kernel".to_owned(),
                prompt_hash: "sha256:prompt".to_owned(),
                invocation_mode: sigil_kernel::AgentInvocationMode::Foreground,
                invocation_source: sigil_kernel::AgentInvocationSource::Chat,
                display_name: Some("kernel explorer".to_owned()),
                created_at_ms: Some(42),
            },
        )),
        SessionLogEntry::Control(ControlEntry::AgentThreadResultRecorded(
            sigil_kernel::AgentThreadResultRecordedEntry {
                result: sigil_kernel::AgentThreadResult {
                    thread_id: thread_id.clone(),
                    session_ref,
                    status: sigil_kernel::AgentThreadTerminalStatus::Completed,
                    summary: "done".to_owned(),
                    summary_truncated: false,
                    original_summary_chars: None,
                    artifacts: Vec::new(),
                    changed_paths: Vec::new(),
                    risks: Vec::new(),
                    followups: Vec::new(),
                    usage: None,
                    output_hash: "sha256:done".to_owned(),
                    final_answer_ref: None,
                },
            },
        )),
    ]);
    app.active_agent_view = super::super::AgentView::Child {
        child_task_id: "agent_chat_terminal".to_owned(),
        child_session_ref: sigil_kernel::SessionRef::new_relative(
            "children/agent_chat_terminal.jsonl",
        )?,
    };
    app.runtime.is_busy = false;

    assert!(app.live_activity_summary().is_none());

    app.runtime.is_busy = true;
    app.runtime.run_phase = RunPhase::Streaming;
    let summary = app
        .live_activity_summary()
        .expect("parent activity should still be visible");
    assert_eq!(summary.label, "streaming");
    assert_eq!(summary.detail, "writing the reply");
    Ok(())
}

#[test]
fn terminal_legacy_child_view_does_not_render_working_progress() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    sync_child_agent_for_transcript_tests(&mut app)?;
    let mut entries = app.session_browser.current_entries.clone();
    entries.push(SessionLogEntry::Control(ControlEntry::TaskChildSession(
        sigil_kernel::TaskChildSessionEntry {
            task_id: sigil_kernel::TaskId::new("task_1")?,
            plan_version: 1,
            step_id: sigil_kernel::TaskStepId::new("step_1")?,
            child_task_id: sigil_kernel::TaskId::new("child_1")?,
            child_session_ref: sigil_kernel::SessionRef::new_relative(
                "children/task_1/step_1-child_1.jsonl",
            )?,
            role: sigil_kernel::AgentRole::SubagentRead,
            status: sigil_kernel::TaskChildSessionStatus::Completed,
            summary_hash: None,
        },
    )));
    entries.push(SessionLogEntry::Control(ControlEntry::AgentThreadClosed(
        sigil_kernel::AgentThreadClosedEntry {
            thread_id: sigil_kernel::AgentThreadId::new("legacy_task_1_v1_step_1_child_1")?,
            reason: Some("hidden from sidebar".to_owned()),
        },
    )));
    app.sync_current_session_state(entries);
    app.active_agent_view = super::super::AgentView::Child {
        child_task_id: "child_1".to_owned(),
        child_session_ref: sigil_kernel::SessionRef::new_relative(
            "children/task_1/step_1-child_1.jsonl",
        )?,
    };

    assert!(app.live_activity_summary().is_none());
    Ok(())
}

#[test]
fn child_agent_view_live_activity_falls_back_to_legacy_child_entry() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    sync_child_agent_for_transcript_tests(&mut app)?;
    let mut entries = app.session_browser.current_entries.clone();
    entries.push(SessionLogEntry::Control(ControlEntry::AgentThreadClosed(
        sigil_kernel::AgentThreadClosedEntry {
            thread_id: sigil_kernel::AgentThreadId::new("legacy_task_1_v1_step_1_child_1")?,
            reason: Some("hidden from sidebar".to_owned()),
        },
    )));
    app.sync_current_session_state(entries);
    app.active_agent_view = super::super::AgentView::Child {
        child_task_id: "child_1".to_owned(),
        child_session_ref: sigil_kernel::SessionRef::new_relative(
            "children/task_1/step_1-child_1.jsonl",
        )?,
    };
    app.runtime.is_busy = true;
    app.runtime.run_phase = RunPhase::Tool("wait_agent".to_owned());

    let summary = app
        .live_activity_summary()
        .expect("legacy child summary should survive closed thread filtering");

    assert!(matches!(app.live_panel_phase(), RunPhase::Agent(profile) if profile == "agent"));
    assert_eq!(summary.label, "agent");
    assert!(summary.detail.contains("child_1"));
    assert!(summary.detail.contains("started"));
    assert!(summary.detail.contains("subagent_read"));
    assert!(!summary.detail.contains("wait_agent"));
    Ok(())
}

#[test]
fn duplicate_phase_markers_do_not_append_duplicate_events() {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());

    app.push_phase_marker("thinking|deepseek-v4-flash");
    app.push_phase_marker("thinking|deepseek-v4-flash");
    app.push_phase_marker("tool|bash");

    let phase_events = app
        .events
        .iter()
        .filter(|event| event.label == "phase")
        .map(|event| event.detail.as_str())
        .collect::<Vec<_>>();
    assert_eq!(
        phase_events,
        vec!["thinking|deepseek-v4-flash", "tool|bash"]
    );
}

#[test]
fn transcript_lines_on_empty_timeline_still_renders_placeholder_lines() {
    let app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());

    let lines = app.transcript_lines(3);
    assert!(!lines.is_empty());
    assert!(lines.iter().any(|line| {
        line.spans
            .iter()
            .any(|span| !span.content.as_ref().trim().is_empty())
    }));
}

#[test]
fn push_assistant_message_once_deduplicates_and_ignores_empty() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());

    // Empty content should not push.
    app.push_assistant_message_once(String::new());
    assert!(
        !app.timeline
            .iter()
            .any(|entry| entry.role == TimelineRole::Assistant)
    );

    // First non-empty push creates an entry.
    app.push_assistant_message_once("hello".to_owned());
    assert_eq!(
        app.timeline
            .iter()
            .filter(|entry| entry.role == TimelineRole::Assistant)
            .count(),
        1
    );

    // Duplicate content since last user message should be suppressed.
    app.push_assistant_message_once("hello".to_owned());
    assert_eq!(
        app.timeline
            .iter()
            .filter(|entry| entry.role == TimelineRole::Assistant)
            .count(),
        1
    );

    // Different content pushes a new entry.
    app.push_assistant_message_once("world".to_owned());
    assert_eq!(
        app.timeline
            .iter()
            .filter(|entry| entry.role == TimelineRole::Assistant)
            .count(),
        2
    );

    // After a user message interjection, duplicate of previous assistant is allowed.
    app.push_timeline(TimelineRole::User, "interjection".to_owned());
    app.push_assistant_message_once("hello".to_owned());
    assert_eq!(
        app.timeline
            .iter()
            .filter(|entry| entry.role == TimelineRole::Assistant)
            .count(),
        3
    );

    Ok(())
}

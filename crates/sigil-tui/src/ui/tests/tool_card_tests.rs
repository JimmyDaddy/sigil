use ratatui::text::Line;
use serde_json::json;

use crate::timeline::TimelineRole;

use super::*;

fn rendered_plain_lines(lines: &[Line<'static>]) -> Vec<String> {
    lines
        .iter()
        .map(|line| {
            line.spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect::<Vec<_>>()
                .join("")
        })
        .collect()
}

fn tool_entry(value: serde_json::Value) -> TimelineEntry {
    TimelineEntry {
        role: TimelineRole::Tool,
        text: value.to_string(),
    }
}

#[test]
fn tool_activity_view_marks_read_file_as_inspection() {
    let entry = tool_entry(json!({
        "tool_name": "read_file",
        "status": "ok",
        "call_id": "call-read",
        "summary": "12 lines · 240 B",
        "metadata": { "details": { "call": { "summary": "path=docs/guide.md" } } }
    }));

    let activity = tool_activity_view(&entry, 0).expect("tool activity view");

    assert_eq!(activity.key, "call:call-read");
    assert!(activity.is_inspection);
    assert!(!activity.defaults_expanded);
    assert!(activity.title.contains("Read"));
}

#[test]
fn render_tool_entry_lines_show_selected_hidden_bash_output_summary() {
    let entry = tool_entry(json!({
        "tool_name": "bash",
        "status": "ok",
        "call_id": "call-bash",
        "summary": "3 lines · 48 B",
        "preview_kind": "text",
        "preview_lines": ["first", "second", "third"],
        "hidden_lines": 2,
        "metadata": { "details": { "call": { "summary": "command=git status" } } }
    }));

    let lines = render_tool_entry_lines(
        &entry,
        &TimelineRenderOptions {
            selected_tool_activity_key: Some("call:call-bash".to_owned()),
            max_content_width: 64,
            ..TimelineRenderOptions::default()
        },
        0,
    );
    let plain = rendered_plain_lines(&lines).join("\n");

    assert!(plain.contains("Ran"));
    assert!(plain.contains("output hidden"));
    assert!(plain.contains("5 lines available"));
    assert!(plain.contains("●"));
}

#[test]
fn render_tool_entry_lines_expand_markdown_read_file_preview() {
    let entry = tool_entry(json!({
        "tool_name": "read_file",
        "status": "ok",
        "call_id": "call-md",
        "summary": "2 lines · 42 B",
        "preview_kind": "markdown",
        "preview_lines": ["# Guide", "- first item"],
        "hidden_lines": 0,
        "metadata": { "details": { "call": { "summary": "path=docs/guide.md" } } }
    }));

    let lines = render_tool_entry_lines(
        &entry,
        &TimelineRenderOptions {
            expand_tool_previews: true,
            max_content_width: 48,
            ..TimelineRenderOptions::default()
        },
        0,
    );
    let plain = rendered_plain_lines(&lines).join("\n");

    assert!(plain.contains("Read"));
    assert!(plain.contains("document excerpt"));
    assert!(plain.contains("Guide"));
    assert!(plain.contains("• first item"));
}

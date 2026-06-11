use ratatui::text::Line;
use serde_json::json;

use crate::timeline::{TimelineEntry, TimelineRole};

use super::*;

fn plain_line_text(line: &Line<'static>) -> String {
    line.spans
        .iter()
        .map(|span| span.content.as_ref())
        .collect::<String>()
}

fn plain_lines_text(lines: &[Line<'static>]) -> String {
    lines
        .iter()
        .map(plain_line_text)
        .collect::<Vec<_>>()
        .join("\n")
}

fn rendered_plain_lines(lines: &[Line<'static>]) -> Vec<String> {
    lines.iter().map(plain_line_text).collect()
}

fn tool_entry(value: serde_json::Value) -> TimelineEntry {
    TimelineEntry {
        role: TimelineRole::Tool,
        text: value.to_string(),
    }
}

fn base_summary(tool_name: &str) -> ToolCardRender {
    ToolCardRender {
        call_id: None,
        tool_name: tool_name.to_owned(),
        is_error: false,
        error_kind: None,
        summary: None,
        metadata: ToolCardMetadata::default(),
        preview_kind: ToolPreviewKind::Text,
        preview_lines: Vec::new(),
        hidden_lines: 0,
        preview_value: None,
        diff: None,
    }
}

#[test]
fn hidden_preview_labels_cover_diff_bash_and_path_variants() {
    let diff_summary = ToolCardRender {
        preview_lines: vec!["line".to_owned()],
        hidden_lines: 2,
        diff: Some(ToolCardDiff {
            summary: "1 file".to_owned(),
            truncated: false,
            original_line_count: 4,
            rendered_line_count: 2,
            files: vec![ToolCardDiffFile {
                path: "src/lib.rs".to_owned(),
                lines: vec!["+ fn main() {}".to_owned()],
                truncated: false,
                original_line_count: 1,
                rendered_line_count: 1,
            }],
        }),
        ..base_summary("edit_file")
    };
    let bash_summary = ToolCardRender {
        preview_lines: vec!["out".to_owned()],
        hidden_lines: 3,
        ..base_summary("bash")
    };
    let glob_summary = ToolCardRender {
        preview_lines: vec!["src/lib.rs".to_owned()],
        hidden_lines: 1,
        ..base_summary("glob")
    };

    assert!(
        plain_line_text(&render_tool_hidden_preview_line(
            &diff_summary,
            accent_rose(),
            false
        ))
        .contains("diff hidden")
    );
    assert!(
        plain_line_text(&render_tool_hidden_preview_line(
            &bash_summary,
            accent_rose(),
            true
        ))
        .contains("output hidden")
    );
    assert!(
        plain_line_text(&render_tool_hidden_preview_line(
            &glob_summary,
            accent_rose(),
            false
        ))
        .contains("paths hidden")
    );
}

#[test]
fn bash_preview_uses_stdout_stderr_and_empty_output_branches() {
    let stderr_summary = ToolCardRender {
        is_error: true,
        error_kind: Some("failed".to_owned()),
        summary: Some("command failed".to_owned()),
        metadata: ToolCardMetadata {
            exit_code: Some(1),
            stdout_bytes: Some(0),
            stderr_bytes: Some(12),
            ..ToolCardMetadata::default()
        },
        ..base_summary("bash")
    };
    let stdout_summary = ToolCardRender {
        is_error: true,
        metadata: ToolCardMetadata {
            exit_code: Some(1),
            stdout_bytes: Some(8),
            stderr_bytes: Some(0),
            ..ToolCardMetadata::default()
        },
        ..base_summary("bash")
    };
    let stderr_lines = render_bash_preview(&stderr_summary, accent_rose());
    let stdout_lines = render_bash_preview(&stdout_summary, accent_rose());

    assert!(plain_lines_text(&stderr_lines).contains("stderr"));
    assert!(plain_lines_text(&stderr_lines).contains("(no output)"));
    assert!(plain_lines_text(&stdout_lines).contains("stdout"));
}

#[test]
fn file_change_preview_renders_delete_labels_and_diff_truncation() {
    let summary = ToolCardRender {
        summary: Some("removed old file".to_owned()),
        metadata: ToolCardMetadata {
            action: Some("delete".to_owned()),
            changed_files: vec!["src/old.rs".to_owned()],
            ..ToolCardMetadata::default()
        },
        preview_lines: vec!["deleted successfully".to_owned()],
        diff: Some(ToolCardDiff {
            summary: "1 file changed".to_owned(),
            truncated: true,
            original_line_count: 8,
            rendered_line_count: 4,
            files: vec![ToolCardDiffFile {
                path: "src/old.rs".to_owned(),
                lines: vec![
                    "--- a/src/old.rs".to_owned(),
                    "+++ /dev/null".to_owned(),
                    "@@ -1,2 +0,0 @@".to_owned(),
                    "-fn old() {}".to_owned(),
                ],
                truncated: false,
                original_line_count: 4,
                rendered_line_count: 4,
            }],
        }),
        ..base_summary("delete_file")
    };
    let lines = render_file_change_preview(&summary, accent_rose()).expect("preview");
    let plain = plain_lines_text(&lines);

    assert!(plain.contains("1 deleted"));
    assert!(plain.contains("deleted"));
    assert!(plain.contains("src/old.rs"));
    assert!(plain.contains("delete summary"));
    assert!(plain.contains("diff truncated"));
}

#[test]
fn tool_activity_view_and_titles_cover_search_and_mcp_cases() {
    let search_entry = TimelineEntry {
        role: crate::app::TimelineRole::Tool,
        text: json!({
            "tool_name": "bash",
            "status": "ok",
            "call_id": "call-search",
            "metadata": { "details": { "call": { "summary": "command=rg needle src" } } }
        })
        .to_string(),
    };
    let mcp_entry = TimelineEntry {
        role: crate::app::TimelineRole::Tool,
        text: json!({
            "tool_name": "mcp__github__search_code",
            "status": "ok",
            "metadata": {
                "details": {
                    "call": { "summary": "query=needle" },
                    "mcp": { "server": "github", "tool": "search_code", "trust_class": "trusted" }
                }
            }
        })
        .to_string(),
    };

    let search = tool_activity_view(&search_entry, 0).expect("activity view");
    let mcp = tool_activity_view(&mcp_entry, 0).expect("activity view");

    assert!(search.is_inspection);
    assert_eq!(search.key, "call:call-search");
    assert!(search.title.contains("Searched needle in src"));
    assert!(!mcp.is_inspection);
    assert!(mcp.title.contains("Called search_code on github"));
}

#[test]
fn tool_title_spans_truncate_narrow_headers() {
    let spans = tool_title_spans(
        &ToolCardTitle::new(
            "Called",
            "very-long-tool-name",
            Some("with many arguments".to_owned()),
        ),
        10,
    );
    let text = spans
        .iter()
        .map(|span| span.content.as_ref())
        .collect::<String>();

    assert!(text.ends_with("..."));
    assert!(text.chars().count() <= 13);
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

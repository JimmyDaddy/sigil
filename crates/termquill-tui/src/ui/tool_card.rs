use ratatui::{
    style::{Color, Modifier, Style},
    text::{Line, Span},
};
use serde_json::Value;

use crate::app::TimelineEntry;

use super::{
    TimelineRenderOptions,
    markdown::{
        MarkdownRenderOptions, render_code_line_spans_with_bg, render_markdown_timeline_lines,
    },
    primitives::{section_badge, timeline_badge, timeline_content_line, timeline_section_line},
    text::truncate_inline_text,
    theme::{
        accent_blue, accent_gold, accent_lime, accent_rose, accent_teal, badge_bg, dim, ink, muted,
    },
};

pub(crate) fn render_tool_entry_lines(
    entry: &TimelineEntry,
    options: &TimelineRenderOptions,
    entry_index: usize,
) -> Vec<Line<'static>> {
    let summary = parse_tool_summary(&entry.text);
    let accent = accent_rose();
    let selected = options.selected_tool_entry == Some(entry_index);
    let expanded =
        options.expand_tool_previews || options.expanded_tool_entries.contains(&entry_index);
    let mut lines = vec![tool_card_header_line(&summary, selected, expanded)];
    let mut status_line = vec![Span::styled(
        summary.status.clone(),
        tool_status_style(summary.is_error),
    )];
    if let Some(call_id) = &summary.call_id {
        status_line.push(Span::raw(" "));
        status_line.push(Span::styled(
            format!("call {}", truncate_inline_text(call_id, 28)),
            Style::default().fg(dim()),
        ));
    }
    if let Some(ref summary_line) = summary.summary {
        status_line.push(Span::raw(" "));
        status_line.push(Span::styled(
            summary_line.clone(),
            Style::default().fg(ink()),
        ));
    }
    lines.push(timeline_content_line(accent, status_line));
    if let Some(ref metadata_line) = summary.metadata_line {
        lines.push(timeline_section_line(
            accent,
            "meta",
            accent,
            vec![Span::styled(
                metadata_line.clone(),
                Style::default().fg(muted()),
            )],
        ));
    }
    if !summary.preview_lines.is_empty() || summary.preview_value.is_some() {
        if expanded {
            lines.extend(render_tool_preview_body(
                &summary,
                accent,
                options.max_content_width,
            ));
        } else {
            let available_lines = summary.preview_lines.len() + summary.hidden_lines;
            lines.push(timeline_content_line(
                accent,
                vec![Span::styled(
                    format!(
                        "{} hidden · {} lines available",
                        summary.preview_kind.description(),
                        available_lines
                    ),
                    Style::default()
                        .fg(if selected { accent_blue() } else { dim() })
                        .add_modifier(Modifier::BOLD),
                )],
            ));
        }
    }
    lines
}

fn tool_card_header_line(
    summary: &ToolCardRender,
    selected: bool,
    expanded: bool,
) -> Line<'static> {
    let accent = accent_rose();
    let mut spans = vec![
        Span::styled(
            "▎",
            Style::default().fg(accent).add_modifier(Modifier::BOLD),
        ),
        Span::raw(" "),
        timeline_badge("tool", accent),
        Span::raw(" "),
        Span::styled(
            summary.tool_name.clone(),
            Style::default().fg(ink()).add_modifier(Modifier::BOLD),
        ),
    ];
    if selected {
        spans.push(Span::raw(" "));
        spans.push(section_badge("focus", accent_blue()));
    }
    if expanded {
        spans.push(Span::raw(" "));
        spans.push(section_badge("open", accent_lime()));
    }
    spans.push(Span::raw(" "));
    spans.push(Span::styled(
        summary.preview_kind.label().to_owned(),
        Style::default().fg(dim()).add_modifier(Modifier::BOLD),
    ));
    Line::from(spans)
}

fn render_tool_preview_body(
    summary: &ToolCardRender,
    accent: Color,
    max_content_width: usize,
) -> Vec<Line<'static>> {
    if (tool_name_matches(&summary.tool_name, "ls")
        || tool_name_matches(&summary.tool_name, "glob"))
        && let Some(lines) = render_path_list_preview(summary, accent)
    {
        return lines;
    }
    if tool_name_matches(&summary.tool_name, "grep")
        && let Some(lines) = render_grep_preview(summary, accent)
    {
        return lines;
    }
    if tool_name_matches(&summary.tool_name, "bash") {
        return render_bash_preview(summary, accent);
    }
    if (tool_name_matches(&summary.tool_name, "write_file")
        || tool_name_matches(&summary.tool_name, "edit_file"))
        && let Some(lines) = render_file_change_preview(summary, accent)
    {
        return lines;
    }
    if tool_name_matches(&summary.tool_name, "read_file") {
        return render_read_file_preview(summary, accent, max_content_width);
    }
    render_generic_tool_preview(summary, accent, max_content_width)
}

fn render_read_file_preview(
    summary: &ToolCardRender,
    accent: Color,
    max_content_width: usize,
) -> Vec<Line<'static>> {
    let mut lines = vec![timeline_section_line(
        accent,
        if summary.preview_kind == ToolPreviewKind::Markdown {
            "doc"
        } else {
            "file"
        },
        accent_blue(),
        vec![Span::styled(
            if summary.preview_kind == ToolPreviewKind::Markdown {
                "document excerpt"
            } else {
                "file excerpt"
            },
            Style::default().fg(dim()),
        )],
    )];
    match summary.preview_kind {
        ToolPreviewKind::Markdown => {
            lines.extend(render_markdown_timeline_lines(
                accent,
                Style::default().fg(ink()),
                &summary.preview_lines.join("\n"),
                MarkdownRenderOptions::tool_preview(max_content_width),
            ));
        }
        ToolPreviewKind::Json | ToolPreviewKind::Text => {
            lines.extend(render_code_preview_lines(
                accent,
                &summary.preview_lines,
                Color::Rgb(28, 33, 41),
            ));
        }
    }
    lines.extend(render_tool_hidden_tail(accent, summary.hidden_lines));
    lines
}

fn render_path_list_preview(summary: &ToolCardRender, accent: Color) -> Option<Vec<Line<'static>>> {
    let entries = summary
        .preview_value
        .as_ref()
        .and_then(json_string_list)
        .or_else(|| Some(infer_string_list_preview(&summary.preview_lines)))
        .filter(|entries| !entries.is_empty())?;

    let mut lines = vec![timeline_section_line(
        accent,
        if tool_name_matches(&summary.tool_name, "glob") {
            "matches"
        } else {
            "files"
        },
        accent_blue(),
        vec![Span::styled(
            format!("{} paths", entries.len() + summary.hidden_lines),
            Style::default().fg(dim()),
        )],
    )];
    for path in entries {
        lines.push(timeline_content_line(
            accent,
            vec![
                Span::styled(
                    "• ",
                    Style::default()
                        .fg(accent_gold())
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(path, Style::default().fg(ink())),
            ],
        ));
    }
    lines.extend(render_tool_hidden_tail(accent, summary.hidden_lines));
    Some(lines)
}

fn render_grep_preview(summary: &ToolCardRender, accent: Color) -> Option<Vec<Line<'static>>> {
    let matches = summary.preview_value.as_ref().and_then(json_grep_matches)?;
    if matches.is_empty() {
        return None;
    }

    let mut grouped = Vec::<(String, Vec<(u64, String)>)>::new();
    for (path, line, text) in matches {
        if let Some((_, rows)) = grouped.iter_mut().find(|(existing, _)| existing == &path) {
            rows.push((line, text));
        } else {
            grouped.push((path, vec![(line, text)]));
        }
    }

    let mut lines = vec![timeline_section_line(
        accent,
        "matches",
        accent_blue(),
        vec![Span::styled(
            format!("{} files", grouped.len()),
            Style::default().fg(dim()),
        )],
    )];
    for (path, rows) in grouped {
        lines.push(timeline_content_line(
            accent,
            vec![
                section_badge("file", accent_teal()),
                Span::raw(" "),
                Span::styled(
                    path,
                    Style::default().fg(ink()).add_modifier(Modifier::BOLD),
                ),
                Span::raw(" "),
                Span::styled(format!("{} hits", rows.len()), Style::default().fg(dim())),
            ],
        ));
        for (line_number, text) in rows {
            lines.push(timeline_content_line(
                accent,
                vec![
                    Span::styled(
                        format!("L{line_number:<4}"),
                        Style::default()
                            .fg(accent_gold())
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::raw(" "),
                    Span::styled(truncate_inline_text(&text, 140), Style::default().fg(ink())),
                ],
            ));
        }
    }
    lines.extend(render_tool_hidden_tail(accent, summary.hidden_lines));
    Some(lines)
}

fn render_bash_preview(summary: &ToolCardRender, accent: Color) -> Vec<Line<'static>> {
    let subtitle = match summary.metadata.exit_code {
        Some(code) if code != 0 => format!("exit {code} · terminal tail"),
        Some(code) => format!("exit {code} · terminal tail"),
        None => "terminal tail".to_owned(),
    };
    let mut lines = vec![timeline_section_line(
        accent,
        "tail",
        accent_gold(),
        vec![Span::styled(subtitle, Style::default().fg(dim()))],
    )];
    if summary.preview_lines.is_empty() {
        lines.push(timeline_content_line(
            accent,
            vec![Span::styled(
                "(no output)".to_owned(),
                Style::default().fg(dim()),
            )],
        ));
    } else {
        lines.extend(render_code_preview_lines(
            accent,
            &summary.preview_lines,
            Color::Rgb(33, 24, 28),
        ));
    }
    lines.extend(render_tool_hidden_tail(accent, summary.hidden_lines));
    lines
}

fn render_file_change_preview(
    summary: &ToolCardRender,
    accent: Color,
) -> Option<Vec<Line<'static>>> {
    if summary.metadata.changed_files.is_empty() {
        return None;
    }
    let mut lines = vec![timeline_section_line(
        accent,
        "files",
        accent_blue(),
        vec![Span::styled(
            format!("{} changed", summary.metadata.changed_files.len()),
            Style::default().fg(dim()),
        )],
    )];
    for path in &summary.metadata.changed_files {
        lines.push(timeline_content_line(
            accent,
            vec![
                Span::styled(
                    "• ",
                    Style::default()
                        .fg(accent_lime())
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(path.clone(), Style::default().fg(ink())),
            ],
        ));
    }
    if !summary.preview_lines.is_empty() {
        lines.push(timeline_section_line(
            accent,
            "result",
            accent_gold(),
            vec![Span::styled("write summary", Style::default().fg(dim()))],
        ));
        lines.extend(render_code_preview_lines(
            accent,
            &summary.preview_lines,
            Color::Rgb(28, 33, 41),
        ));
    }
    Some(lines)
}

fn render_generic_tool_preview(
    summary: &ToolCardRender,
    accent: Color,
    max_content_width: usize,
) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    if let Some(value) = &summary.preview_value {
        lines.push(timeline_section_line(
            accent,
            "tree",
            accent_blue(),
            vec![Span::styled(
                "structured payload",
                Style::default().fg(dim()),
            )],
        ));
        for line in render_json_tree_preview(value) {
            lines.push(timeline_content_line(
                accent,
                render_code_line_spans_with_bg(
                    &line,
                    accent_blue(),
                    Style::default().fg(ink()),
                    Color::Rgb(28, 33, 41),
                ),
            ));
        }
    } else if summary.preview_kind == ToolPreviewKind::Markdown {
        lines.push(timeline_section_line(
            accent,
            "md",
            accent_blue(),
            vec![Span::styled(
                "formatted preview",
                Style::default().fg(dim()),
            )],
        ));
        lines.extend(render_markdown_timeline_lines(
            accent,
            Style::default().fg(ink()),
            &summary.preview_lines.join("\n"),
            MarkdownRenderOptions::tool_preview(max_content_width),
        ));
    } else {
        lines.push(timeline_section_line(
            accent,
            summary.preview_kind.label(),
            accent_blue(),
            vec![Span::styled(
                summary.preview_kind.description(),
                Style::default().fg(dim()),
            )],
        ));
        lines.extend(render_code_preview_lines(
            accent,
            &summary.preview_lines,
            Color::Rgb(38, 28, 34),
        ));
    }
    lines.extend(render_tool_hidden_tail(accent, summary.hidden_lines));
    lines
}

fn render_code_preview_lines(accent: Color, lines: &[String], bg: Color) -> Vec<Line<'static>> {
    lines
        .iter()
        .map(|line| {
            timeline_content_line(
                accent,
                render_code_line_spans_with_bg(line, accent_blue(), Style::default().fg(ink()), bg),
            )
        })
        .collect()
}

fn render_tool_hidden_tail(accent: Color, hidden_lines: usize) -> Vec<Line<'static>> {
    if hidden_lines == 0 {
        return Vec::new();
    }
    vec![timeline_content_line(
        accent,
        vec![Span::styled(
            format!("… {} more lines hidden", hidden_lines),
            Style::default().fg(dim()).add_modifier(Modifier::BOLD),
        )],
    )]
}

fn render_json_tree_preview(value: &Value) -> Vec<String> {
    let mut lines = Vec::new();
    push_json_tree_lines(value, "", None, &mut lines);
    if lines.is_empty() {
        lines.push("(empty)".to_owned());
    }
    lines
}

fn push_json_tree_lines(value: &Value, prefix: &str, key: Option<&str>, lines: &mut Vec<String>) {
    match value {
        Value::Object(object) => {
            if let Some(key) = key {
                lines.push(format!("{prefix}{key}: {{}}"));
            } else if prefix.is_empty() {
                lines.push("{object}".to_owned());
            }
            let len = object.len();
            for (index, (child_key, child_value)) in object.iter().enumerate() {
                let branch = if index + 1 == len {
                    "└─ "
                } else {
                    "├─ "
                };
                let next_prefix = if index + 1 == len {
                    format!("{prefix}   ")
                } else {
                    format!("{prefix}│  ")
                };
                if json_tree_is_leaf(child_value) {
                    lines.push(format!(
                        "{prefix}{branch}{child_key}: {}",
                        json_tree_leaf_text(child_value)
                    ));
                } else {
                    lines.push(format!(
                        "{prefix}{branch}{child_key}: {}",
                        json_tree_container_label(child_value)
                    ));
                    push_json_tree_lines(child_value, &next_prefix, None, lines);
                }
            }
        }
        Value::Array(items) => {
            if let Some(key) = key {
                lines.push(format!("{prefix}{key}: [{}]", items.len()));
            } else if prefix.is_empty() {
                lines.push(format!("[array] {}", items.len()));
            }
            for (index, item) in items.iter().enumerate() {
                let branch = if index + 1 == items.len() {
                    "└─ "
                } else {
                    "├─ "
                };
                let next_prefix = if index + 1 == items.len() {
                    format!("{prefix}   ")
                } else {
                    format!("{prefix}│  ")
                };
                if json_tree_is_leaf(item) {
                    lines.push(format!(
                        "{prefix}{branch}[{index}] {}",
                        json_tree_leaf_text(item)
                    ));
                } else {
                    lines.push(format!(
                        "{prefix}{branch}[{index}] {}",
                        json_tree_container_label(item)
                    ));
                    push_json_tree_lines(item, &next_prefix, None, lines);
                }
            }
        }
        _ => {
            let leaf = json_tree_leaf_text(value);
            if let Some(key) = key {
                lines.push(format!("{prefix}{key}: {leaf}"));
            } else {
                lines.push(format!("{prefix}{leaf}"));
            }
        }
    }
}

fn json_tree_is_leaf(value: &Value) -> bool {
    !matches!(value, Value::Object(_) | Value::Array(_))
}

fn json_tree_leaf_text(value: &Value) -> String {
    match value {
        Value::Null => "null".to_owned(),
        Value::Bool(boolean) => boolean.to_string(),
        Value::Number(number) => number.to_string(),
        Value::String(text) => format!("\"{}\"", truncate_inline_text(text, 100)),
        Value::Array(items) => format!("[{}]", items.len()),
        Value::Object(object) => format!("{{{}}}", object.len()),
    }
}

fn json_tree_container_label(value: &Value) -> String {
    match value {
        Value::Array(items) => format!("[{} items]", items.len()),
        Value::Object(object) => format!("{{{} keys}}", object.len()),
        _ => json_tree_leaf_text(value),
    }
}

fn json_string_list(value: &Value) -> Option<Vec<String>> {
    let entries = value
        .as_array()?
        .iter()
        .filter_map(Value::as_str)
        .map(str::to_owned)
        .collect::<Vec<_>>();
    Some(entries)
}

fn infer_string_list_preview(lines: &[String]) -> Vec<String> {
    lines
        .iter()
        .map(|line| {
            line.trim()
                .trim_end_matches(',')
                .trim_matches('"')
                .to_owned()
        })
        .filter(|line| !line.is_empty() && line != "[" && line != "]")
        .collect()
}

fn json_grep_matches(value: &Value) -> Option<Vec<(String, u64, String)>> {
    let array = value.as_array()?;
    let mut matches = Vec::new();
    for entry in array {
        let object = entry.as_object()?;
        let path = object.get("path")?.as_str()?.to_owned();
        let line = object.get("line")?.as_u64()?;
        let text = object.get("text")?.as_str()?.to_owned();
        matches.push((path, line, text));
    }
    Some(matches)
}

fn tool_name_matches(tool_name: &str, expected: &str) -> bool {
    tool_name == expected || tool_name.ends_with(&format!("_{expected}"))
}

struct ToolCardRender {
    tool_name: String,
    call_id: Option<String>,
    status: String,
    is_error: bool,
    summary: Option<String>,
    metadata: ToolCardMetadata,
    metadata_line: Option<String>,
    preview_kind: ToolPreviewKind,
    preview_lines: Vec<String>,
    hidden_lines: usize,
    preview_value: Option<Value>,
}

#[derive(Default)]
struct ToolCardMetadata {
    exit_code: Option<i64>,
    bytes: Option<u64>,
    truncated: bool,
    changed_files: Vec<String>,
}

#[derive(Clone, Copy, Default, PartialEq, Eq)]
enum ToolPreviewKind {
    Markdown,
    Json,
    #[default]
    Text,
}

impl ToolPreviewKind {
    fn label(self) -> &'static str {
        match self {
            Self::Markdown => "md",
            Self::Json => "json",
            Self::Text => "text",
        }
    }

    fn description(self) -> &'static str {
        match self {
            Self::Markdown => "formatted preview",
            Self::Json => "structured preview",
            Self::Text => "captured output",
        }
    }

    fn from_value(value: &str) -> Self {
        match value {
            "markdown" => Self::Markdown,
            "json" => Self::Json,
            _ => Self::Text,
        }
    }
}

fn parse_tool_summary(text: &str) -> ToolCardRender {
    let fallback = ToolCardRender {
        tool_name: "result".to_owned(),
        call_id: None,
        status: " OK ".to_owned(),
        is_error: false,
        summary: None,
        metadata: ToolCardMetadata::default(),
        metadata_line: None,
        preview_kind: ToolPreviewKind::Text,
        preview_lines: text.lines().take(8).map(str::to_owned).collect(),
        hidden_lines: text.lines().count().saturating_sub(8),
        preview_value: None,
    };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(text) else {
        return fallback;
    };
    let Some(object) = value.as_object() else {
        return fallback;
    };
    let Some(tool_name) = object
        .get("tool_name")
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned)
    else {
        return fallback;
    };
    let call_id = object
        .get("call_id")
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned);
    let status = object
        .get("status")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("ok")
        .to_uppercase();
    let is_error = status == "ERROR";
    let metadata = object
        .get("metadata")
        .map(parse_tool_metadata)
        .unwrap_or_default();
    let summary = object
        .get("summary")
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned)
        .or_else(|| call_id.as_ref().map(|call_id| format!("call {call_id}")));
    let metadata_line = object
        .get("metadata_line")
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned)
        .or_else(|| render_tool_metadata(&metadata).filter(|line| !line.is_empty()));
    let preview_kind = object
        .get("preview_kind")
        .and_then(serde_json::Value::as_str)
        .map(ToolPreviewKind::from_value)
        .or_else(|| object.get("content").map(legacy_tool_preview_kind))
        .unwrap_or_default();
    let preview_value = object.get("preview_value").cloned().or_else(|| {
        object
            .get("content")
            .cloned()
            .filter(|value| matches!(value, Value::Array(_) | Value::Object(_)))
    });
    let (preview_lines, hidden_lines) = object
        .get("preview_lines")
        .and_then(serde_json::Value::as_array)
        .map(|lines| {
            let preview = lines
                .iter()
                .filter_map(serde_json::Value::as_str)
                .map(str::to_owned)
                .collect::<Vec<_>>();
            let hidden = object
                .get("hidden_lines")
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(0) as usize;
            (preview, hidden)
        })
        .unwrap_or_else(|| legacy_tool_preview(object.get("content"), preview_kind));

    ToolCardRender {
        tool_name,
        call_id,
        status: format!(" {status} "),
        is_error,
        summary,
        metadata,
        metadata_line,
        preview_kind,
        preview_lines,
        hidden_lines,
        preview_value,
    }
}

fn legacy_tool_preview_kind(value: &serde_json::Value) -> ToolPreviewKind {
    match value {
        serde_json::Value::Array(_) | serde_json::Value::Object(_) => ToolPreviewKind::Json,
        serde_json::Value::String(content)
            if content.trim_start().starts_with('#')
                || content.contains("```")
                || content.contains("\n- ")
                || content.contains("\n|") =>
        {
            ToolPreviewKind::Markdown
        }
        _ => ToolPreviewKind::Text,
    }
}

fn legacy_tool_preview(
    value: Option<&serde_json::Value>,
    preview_kind: ToolPreviewKind,
) -> (Vec<String>, usize) {
    let Some(value) = value else {
        return (Vec::new(), 0);
    };
    let source = match value {
        serde_json::Value::Array(_) | serde_json::Value::Object(_) => {
            serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string())
        }
        serde_json::Value::String(text) => text.clone(),
        _ => value.to_string(),
    };
    let limit = match preview_kind {
        ToolPreviewKind::Markdown => 18,
        ToolPreviewKind::Json => 12,
        ToolPreviewKind::Text => 12,
    };
    let lines = source.lines().map(str::to_owned).collect::<Vec<_>>();
    let hidden_lines = lines.len().saturating_sub(limit);
    let preview_lines = lines.into_iter().take(limit).collect::<Vec<_>>();
    (preview_lines, hidden_lines)
}

fn parse_tool_metadata(value: &Value) -> ToolCardMetadata {
    let Some(object) = value.as_object() else {
        return ToolCardMetadata::default();
    };
    ToolCardMetadata {
        exit_code: object.get("exit_code").and_then(Value::as_i64),
        bytes: object.get("bytes").and_then(Value::as_u64),
        truncated: object
            .get("truncated")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        changed_files: object
            .get("changed_files")
            .and_then(Value::as_array)
            .map(|files| {
                files
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_owned)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default(),
    }
}

fn render_tool_metadata(metadata: &ToolCardMetadata) -> Option<String> {
    let mut parts = Vec::new();
    if let Some(exit_code) = metadata.exit_code {
        parts.push(format!("exit={exit_code}"));
    }
    if let Some(bytes) = metadata.bytes {
        parts.push(format!("bytes={bytes}"));
    }
    if metadata.truncated {
        parts.push("truncated=yes".to_owned());
    }
    if !metadata.changed_files.is_empty() {
        let preview = metadata
            .changed_files
            .iter()
            .take(2)
            .cloned()
            .collect::<Vec<_>>();
        if preview.is_empty() {
            parts.push(format!("files={}", metadata.changed_files.len()));
        } else {
            let mut summary = format!(
                "files={} {}",
                metadata.changed_files.len(),
                preview.join(", ")
            );
            if metadata.changed_files.len() > preview.len() {
                summary.push_str(" ...");
            }
            parts.push(summary);
        }
    }
    (!parts.is_empty()).then_some(parts.join("  "))
}

fn tool_status_style(is_error: bool) -> Style {
    if is_error {
        Style::default()
            .fg(accent_rose())
            .bg(badge_bg())
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default()
            .fg(accent_lime())
            .bg(badge_bg())
            .add_modifier(Modifier::BOLD)
    }
}

use ratatui::{
    style::{Color, Modifier, Style},
    text::{Line, Span},
};
use serde_json::Value;

use crate::app::TimelineEntry;

use super::{
    TimelineRenderOptions,
    diff::{
        DiffLineKind, NumberedDiffLine, diff_line_kind, diff_line_number_gutter, diff_line_style,
        number_unified_diff_lines,
    },
    markdown::{
        MarkdownRenderOptions, render_code_line_spans_with_bg, render_markdown_timeline_lines,
    },
    primitives::{section_badge, timeline_badge, timeline_content_line, timeline_section_line},
    text::truncate_inline_text,
    theme::{accent_blue, accent_gold, accent_lime, accent_rose, accent_teal, badge_bg, dim, ink},
};

pub(crate) fn render_tool_entry_lines(
    entry: &TimelineEntry,
    options: &TimelineRenderOptions,
    entry_index: usize,
) -> Vec<Line<'static>> {
    let summary = parse_tool_summary(&entry.text);
    let accent = accent_rose();
    let selected = options.selected_tool_entry == Some(entry_index);
    let default_expanded = summary.diff.is_some();
    let expanded = options.expand_tool_previews
        || options.expanded_tool_entries.contains(&entry_index)
        || (default_expanded && !options.collapsed_tool_entries.contains(&entry_index));
    let mut lines = vec![tool_card_header_line(
        &summary,
        selected,
        expanded,
        options.max_content_width,
    )];
    let mut status_line = vec![Span::styled(
        summary.status.clone(),
        tool_status_style(summary.is_error),
    )];
    if let Some(ref summary_line) = summary.summary {
        status_line.push(Span::raw(" "));
        status_line.push(Span::styled(
            summary_line.clone(),
            Style::default().fg(ink()),
        ));
    }
    lines.push(timeline_content_line(accent, status_line));
    if !summary.preview_lines.is_empty()
        || summary.preview_value.is_some()
        || summary.diff.is_some()
    {
        if expanded {
            lines.extend(render_tool_preview_body(
                &summary,
                accent,
                options.max_content_width,
            ));
        } else {
            let available_lines = tool_available_preview_lines(&summary);
            lines.push(timeline_content_line(
                accent,
                vec![Span::styled(
                    format!(
                        "{} hidden · {} lines available",
                        tool_hidden_preview_label(&summary),
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

fn tool_hidden_preview_label(summary: &ToolCardRender) -> &'static str {
    if summary.diff.is_some() {
        return "diff";
    }
    if tool_name_matches(&summary.tool_name, "read_file") {
        return "file preview";
    }
    if tool_name_matches(&summary.tool_name, "bash") {
        return "output";
    }
    if tool_name_matches(&summary.tool_name, "grep") {
        return "matches";
    }
    if tool_name_matches(&summary.tool_name, "ls") || tool_name_matches(&summary.tool_name, "glob")
    {
        return "paths";
    }
    "result preview"
}

fn tool_available_preview_lines(summary: &ToolCardRender) -> usize {
    if let Some(diff) = &summary.diff {
        return diff.rendered_line_count.max(
            diff.files
                .iter()
                .map(|file| file.lines.len())
                .sum::<usize>(),
        );
    }
    summary.preview_lines.len() + summary.hidden_lines
}

fn tool_card_header_line(
    summary: &ToolCardRender,
    selected: bool,
    expanded: bool,
    max_content_width: usize,
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
    if let Some(call_summary) = &summary.metadata.call_summary {
        spans.push(Span::raw(" "));
        spans.push(Span::styled(
            truncate_inline_text(
                call_summary,
                tool_call_summary_width(summary, max_content_width),
            ),
            Style::default()
                .fg(accent_gold())
                .add_modifier(Modifier::BOLD),
        ));
    }
    if selected {
        spans.push(Span::raw(" "));
        spans.push(section_badge("focus", accent_blue()));
    }
    if expanded {
        spans.push(Span::raw(" "));
        spans.push(section_badge("open", accent_lime()));
    }
    Line::from(spans)
}

fn tool_call_summary_width(summary: &ToolCardRender, max_content_width: usize) -> usize {
    max_content_width
        .saturating_sub(summary.tool_name.chars().count() + 12)
        .clamp(32, 160)
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
        || tool_name_matches(&summary.tool_name, "edit_file")
        || tool_name_matches(&summary.tool_name, "delete_file"))
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
    if summary.metadata.changed_files.is_empty() && summary.diff.is_none() {
        return None;
    }
    let mut lines = Vec::new();
    if !summary.metadata.changed_files.is_empty() {
        lines.push(timeline_section_line(
            accent,
            "files",
            accent_blue(),
            vec![Span::styled(
                format!(
                    "{} {}",
                    summary.metadata.changed_files.len(),
                    file_change_count_label(summary)
                ),
                Style::default().fg(dim()),
            )],
        ));
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
    }
    if let Some(diff) = &summary.diff {
        lines.extend(render_tool_diff_preview(summary, diff, accent));
    }
    if !summary.preview_lines.is_empty() {
        lines.push(timeline_section_line(
            accent,
            "result",
            accent_gold(),
            vec![Span::styled(
                file_change_result_label(summary),
                Style::default().fg(dim()),
            )],
        ));
        lines.extend(render_code_preview_lines(
            accent,
            &summary.preview_lines,
            Color::Rgb(28, 33, 41),
        ));
    }
    Some(lines)
}

fn file_change_count_label(summary: &ToolCardRender) -> &'static str {
    if summary.metadata.action.as_deref() == Some("delete")
        || tool_name_matches(&summary.tool_name, "delete_file")
    {
        "deleted"
    } else {
        "changed"
    }
}

fn file_change_result_label(summary: &ToolCardRender) -> &'static str {
    if summary.metadata.action.as_deref() == Some("delete")
        || tool_name_matches(&summary.tool_name, "delete_file")
    {
        "delete summary"
    } else if tool_name_matches(&summary.tool_name, "edit_file") {
        "edit summary"
    } else if tool_name_matches(&summary.tool_name, "write_file") {
        "write summary"
    } else {
        "file summary"
    }
}

fn render_tool_diff_preview(
    summary: &ToolCardRender,
    diff: &ToolCardDiff,
    accent: Color,
) -> Vec<Line<'static>> {
    let mut lines = vec![timeline_section_line(
        accent,
        "diff",
        accent_gold(),
        vec![Span::styled(
            diff.summary.clone(),
            Style::default().fg(dim()),
        )],
    )];
    for file in &diff.files {
        lines.push(timeline_content_line(
            accent,
            vec![
                timeline_badge(tool_diff_file_label(summary, file), accent_blue()),
                Span::raw(" "),
                Span::styled(file.path.clone(), Style::default().fg(ink())),
            ],
        ));
        for line in number_unified_diff_lines(file.lines.iter().map(String::as_str)) {
            lines.push(render_tool_diff_line(accent, line));
        }
        if file.truncated {
            lines.push(timeline_content_line(
                accent,
                vec![Span::styled(
                    format!(
                        "... diff truncated · showing {}/{} lines",
                        file.rendered_line_count, file.original_line_count
                    ),
                    Style::default()
                        .fg(accent_gold())
                        .add_modifier(Modifier::BOLD),
                )],
            ));
        }
    }
    if diff.truncated && diff.files.iter().all(|file| !file.truncated) {
        lines.push(timeline_content_line(
            accent,
            vec![Span::styled(
                format!(
                    "... diff truncated · showing {}/{} lines",
                    diff.rendered_line_count, diff.original_line_count
                ),
                Style::default()
                    .fg(accent_gold())
                    .add_modifier(Modifier::BOLD),
            )],
        ));
    }
    lines
}

fn tool_diff_file_label(summary: &ToolCardRender, file: &ToolCardDiffFile) -> &'static str {
    if summary.metadata.action.as_deref() == Some("delete")
        || tool_name_matches(&summary.tool_name, "delete_file")
    {
        return "deleted";
    }
    let (added, removed) = file_diff_line_stats(file);
    match (added > 0, removed > 0) {
        (true, false) => "created",
        (false, true) => "deleted",
        (true, true) => "modified",
        (false, false) => "changed",
    }
}

fn file_diff_line_stats(file: &ToolCardDiffFile) -> (usize, usize) {
    file.lines.iter().fold((0, 0), |(added, removed), line| {
        match diff_line_kind(line) {
            DiffLineKind::Added => (added + 1, removed),
            DiffLineKind::Removed => (added, removed + 1),
            _ => (added, removed),
        }
    })
}

fn render_tool_diff_line(accent: Color, line: NumberedDiffLine<'_>) -> Line<'static> {
    let (marker_color, body_style) = diff_line_style(line.kind);
    timeline_content_line(
        accent,
        vec![
            Span::styled("│ ", Style::default().fg(marker_color)),
            Span::styled(
                diff_line_number_gutter(line.old_line, line.new_line),
                Style::default().fg(dim()),
            ),
            Span::styled(
                if line.text.is_empty() {
                    " ".to_owned()
                } else {
                    line.text.to_owned()
                },
                body_style,
            ),
        ],
    )
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
    status: String,
    is_error: bool,
    summary: Option<String>,
    metadata: ToolCardMetadata,
    preview_kind: ToolPreviewKind,
    preview_lines: Vec<String>,
    hidden_lines: usize,
    preview_value: Option<Value>,
    diff: Option<ToolCardDiff>,
}

#[derive(Default)]
struct ToolCardMetadata {
    exit_code: Option<i64>,
    changed_files: Vec<String>,
    call_summary: Option<String>,
    action: Option<String>,
}

struct ToolCardDiff {
    summary: String,
    truncated: bool,
    original_line_count: usize,
    rendered_line_count: usize,
    files: Vec<ToolCardDiffFile>,
}

struct ToolCardDiffFile {
    path: String,
    lines: Vec<String>,
    truncated: bool,
    original_line_count: usize,
    rendered_line_count: usize,
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
        status: " OK ".to_owned(),
        is_error: false,
        summary: None,
        metadata: ToolCardMetadata::default(),
        preview_kind: ToolPreviewKind::Text,
        preview_lines: text.lines().take(8).map(str::to_owned).collect(),
        hidden_lines: text.lines().count().saturating_sub(8),
        preview_value: None,
        diff: None,
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
        .map(str::to_owned);
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
    let diff = object.get("diff").and_then(parse_tool_diff);

    ToolCardRender {
        tool_name,
        status: format!(" {status} "),
        is_error,
        summary,
        metadata,
        preview_kind,
        preview_lines,
        hidden_lines,
        preview_value,
        diff,
    }
}

fn parse_tool_diff(value: &Value) -> Option<ToolCardDiff> {
    let object = value.as_object()?;
    let files = object
        .get("files")
        .and_then(Value::as_array)?
        .iter()
        .filter_map(parse_tool_diff_file)
        .collect::<Vec<_>>();
    if files.is_empty() {
        return None;
    }
    Some(ToolCardDiff {
        summary: object
            .get("summary")
            .and_then(Value::as_str)
            .unwrap_or("file diff")
            .to_owned(),
        truncated: object
            .get("truncated")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        original_line_count: object
            .get("original_line_count")
            .and_then(Value::as_u64)
            .unwrap_or_else(|| {
                files
                    .iter()
                    .map(|file| file.original_line_count as u64)
                    .sum()
            }) as usize,
        rendered_line_count: object
            .get("rendered_line_count")
            .and_then(Value::as_u64)
            .unwrap_or_else(|| files.iter().map(|file| file.lines.len() as u64).sum())
            as usize,
        files,
    })
}

fn parse_tool_diff_file(value: &Value) -> Option<ToolCardDiffFile> {
    let object = value.as_object()?;
    let lines = object
        .get("lines")
        .and_then(Value::as_array)?
        .iter()
        .filter_map(Value::as_str)
        .map(str::to_owned)
        .collect::<Vec<_>>();
    Some(ToolCardDiffFile {
        path: object
            .get("path")
            .and_then(Value::as_str)
            .unwrap_or("unknown")
            .to_owned(),
        truncated: object
            .get("truncated")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        original_line_count: object
            .get("original_line_count")
            .and_then(Value::as_u64)
            .unwrap_or(lines.len() as u64) as usize,
        rendered_line_count: object
            .get("rendered_line_count")
            .and_then(Value::as_u64)
            .unwrap_or(lines.len() as u64) as usize,
        lines,
    })
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
        call_summary: object
            .get("details")
            .and_then(|details| details.get("call"))
            .and_then(|call| call.get("summary"))
            .and_then(Value::as_str)
            .map(str::to_owned),
        action: object
            .get("details")
            .and_then(|details| details.get("action"))
            .and_then(Value::as_str)
            .map(str::to_owned),
    }
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

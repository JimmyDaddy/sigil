use ratatui::{
    style::{Color, Modifier, Style},
    text::{Line, Span},
};
use serde_json::Value;

use crate::app::TimelineEntry;

use super::{
    TimelineRenderOptions,
    diff::{
        DiffLineKind, NumberedDiffLine, diff_line_kind, diff_line_number_text,
        diff_line_number_width, diff_line_style, number_unified_diff_lines,
    },
    markdown::{
        MarkdownRenderOptions, render_code_line_spans_with_bg, render_markdown_timeline_lines,
    },
    primitives::{section_badge, timeline_badge, timeline_content_line, timeline_section_line},
    text::truncate_inline_text,
    theme::{accent_blue, accent_gold, accent_lime, accent_rose, accent_teal, badge_bg, dim, ink},
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ToolActivityView {
    pub(crate) key: String,
    pub(crate) title: String,
    pub(crate) is_inspection: bool,
    pub(crate) defaults_expanded: bool,
}

pub(crate) fn tool_activity_view(
    entry: &TimelineEntry,
    _entry_index: usize,
) -> Option<ToolActivityView> {
    let summary = parse_tool_summary(&entry.text);
    Some(build_tool_activity_view(&summary, &entry.text))
}

pub(crate) fn render_tool_entry_lines(
    entry: &TimelineEntry,
    options: &TimelineRenderOptions,
    _entry_index: usize,
) -> Vec<Line<'static>> {
    let summary = parse_tool_summary(&entry.text);
    let display = build_tool_card_display(&summary);
    let activity = build_tool_activity_view(&summary, &entry.text);
    let accent = accent_rose();
    let selected = options
        .selected_tool_activity_key
        .as_deref()
        .is_some_and(|selected| selected == activity.key.as_str());
    let default_expanded = activity.defaults_expanded;
    let expanded = options.expand_tool_previews
        || options.expanded_tool_activity_keys.contains(&activity.key)
        || (default_expanded && !options.collapsed_tool_activity_keys.contains(&activity.key));
    let mut lines = vec![tool_card_header_line(
        &display,
        selected,
        expanded,
        options.max_content_width,
    )];
    if let Some(summary_line) = display.summary.clone() {
        lines.push(timeline_content_line(
            accent,
            vec![Span::styled(
                summary_line,
                Style::default().fg(dim()).add_modifier(Modifier::ITALIC),
            )],
        ));
    }
    if tool_has_preview(&summary) {
        if expanded {
            lines.extend(render_tool_preview_body(
                &summary,
                accent,
                options.max_content_width,
            ));
        } else {
            lines.push(render_tool_hidden_preview_line(&summary, accent, selected));
        }
    }
    lines
}

fn tool_has_preview(summary: &ToolCardRender) -> bool {
    !summary.preview_lines.is_empty() || summary.preview_value.is_some() || summary.diff.is_some()
}

fn render_tool_hidden_preview_line(
    summary: &ToolCardRender,
    accent: Color,
    selected: bool,
) -> Line<'static> {
    let available_lines = tool_available_preview_lines(summary);
    timeline_content_line(
        accent,
        vec![Span::styled(
            format!(
                "{} hidden · {} lines available",
                tool_hidden_preview_label(summary),
                available_lines
            ),
            Style::default()
                .fg(if selected { accent_blue() } else { dim() })
                .add_modifier(Modifier::BOLD),
        )],
    )
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
    display: &ToolCardDisplay,
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
    ];
    spans.extend(tool_title_spans(
        &display.title,
        tool_title_width(display, max_content_width),
    ));
    spans.push(Span::raw("  "));
    spans.push(Span::styled(
        format!(" {} ", display.status.label),
        tool_status_style(display.status.is_error),
    ));
    if let Some(detail) = &display.status.detail {
        spans.push(Span::raw(" "));
        spans.push(Span::styled(
            detail.clone(),
            Style::default().fg(if display.status.is_error {
                accent_rose()
            } else {
                dim()
            }),
        ));
    }
    if selected {
        spans.push(Span::raw(" "));
        spans.push(Span::styled(
            "●",
            Style::default()
                .fg(accent_blue())
                .add_modifier(Modifier::BOLD),
        ));
    }
    if expanded {
        spans.push(Span::raw(" "));
        spans.push(Span::styled("▾", Style::default().fg(accent_lime())));
    }
    Line::from(spans)
}

fn tool_title_width(display: &ToolCardDisplay, max_content_width: usize) -> usize {
    if max_content_width == 0 {
        return 160;
    }
    max_content_width
        .saturating_sub(display.status.label.chars().count() + 10)
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
    let section = bash_preview_section_label(summary);
    let subtitle = match (&summary.summary, summary.metadata.exit_code) {
        (Some(summary), Some(code)) => format!("exit {code} · {summary}"),
        (Some(summary), None) => summary.clone(),
        (None, Some(code)) => format!("exit {code}"),
        (None, None) => "terminal tail".to_owned(),
    };
    let mut lines = vec![timeline_section_line(
        accent,
        section,
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

fn bash_preview_section_label(summary: &ToolCardRender) -> &'static str {
    if summary.is_error {
        if summary.metadata.stderr_bytes.unwrap_or(0) > 0 {
            return "stderr";
        }
        if summary.metadata.stdout_bytes.unwrap_or(0) > 0 {
            return "stdout";
        }
    }
    "output"
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
                Span::raw(" "),
                Span::styled(diff_hunk_summary(file), Style::default().fg(dim())),
            ],
        ));
        let numbered_lines = number_unified_diff_lines(file.lines.iter().map(String::as_str));
        let line_number_width = diff_line_number_width(&numbered_lines);
        for line in numbered_lines {
            if matches!(line.kind, DiffLineKind::Hunk) {
                continue;
            }
            lines.push(render_tool_diff_line(accent, line, line_number_width));
        }
        if file.truncated {
            let hidden = file
                .original_line_count
                .saturating_sub(file.rendered_line_count);
            lines.push(timeline_content_line(
                accent,
                vec![Span::styled(
                    format!("diff truncated · {hidden} lines hidden"),
                    Style::default()
                        .fg(accent_gold())
                        .add_modifier(Modifier::BOLD),
                )],
            ));
        }
    }
    if diff.truncated && diff.files.iter().all(|file| !file.truncated) {
        let hidden = diff
            .original_line_count
            .saturating_sub(diff.rendered_line_count);
        lines.push(timeline_content_line(
            accent,
            vec![Span::styled(
                format!("diff truncated · {hidden} lines hidden"),
                Style::default()
                    .fg(accent_gold())
                    .add_modifier(Modifier::BOLD),
            )],
        ));
    }
    lines
}

fn diff_hunk_summary(file: &ToolCardDiffFile) -> String {
    let count = file
        .lines
        .iter()
        .filter(|line| matches!(diff_line_kind(line), DiffLineKind::Hunk))
        .count();
    match count {
        0 => "0 hunks".to_owned(),
        1 => "1 hunk".to_owned(),
        count => format!("{count} hunks"),
    }
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

fn render_tool_diff_line(
    accent: Color,
    line: NumberedDiffLine<'_>,
    line_number_width: usize,
) -> Line<'static> {
    let (marker_color, body_style) = diff_line_style(line.kind);
    timeline_content_line(
        accent,
        vec![
            Span::styled("│", Style::default().fg(marker_color)),
            Span::styled(
                diff_line_number_text(line.old_line, line_number_width),
                tool_diff_old_line_number_style(line),
            ),
            Span::styled(" ", Style::default().fg(dim())),
            Span::styled(
                diff_line_number_text(line.new_line, line_number_width),
                tool_diff_new_line_number_style(line),
            ),
            Span::styled("│ ", Style::default().fg(dim())),
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

fn tool_diff_old_line_number_style(line: NumberedDiffLine<'_>) -> Style {
    if line.old_line.is_none() {
        return Style::default().fg(dim());
    }
    let style = Style::default().fg(accent_rose());
    if matches!(line.kind, DiffLineKind::Removed) {
        style.add_modifier(Modifier::BOLD)
    } else {
        style
    }
}

fn tool_diff_new_line_number_style(line: NumberedDiffLine<'_>) -> Style {
    if line.new_line.is_none() {
        return Style::default().fg(dim());
    }
    let style = Style::default().fg(accent_lime());
    if matches!(line.kind, DiffLineKind::Added) {
        style.add_modifier(Modifier::BOLD)
    } else {
        style
    }
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
    call_id: Option<String>,
    tool_name: String,
    is_error: bool,
    error_kind: Option<String>,
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
    stdout_bytes: Option<u64>,
    stderr_bytes: Option<u64>,
    changed_files: Vec<String>,
    call_summary: Option<String>,
    action: Option<String>,
}

struct ToolCardDisplay {
    title: ToolCardTitle,
    status: ToolCardDisplayStatus,
    summary: Option<String>,
}

struct ToolCardTitle {
    action: String,
    subject: String,
    args: Option<String>,
}

impl ToolCardTitle {
    fn new(action: impl Into<String>, subject: impl Into<String>, args: Option<String>) -> Self {
        Self {
            action: action.into(),
            subject: subject.into(),
            args,
        }
    }

    fn plain(&self) -> String {
        match &self.args {
            Some(args) if !args.is_empty() => {
                format!("{} {} {}", self.action, self.subject, args)
            }
            _ => format!("{} {}", self.action, self.subject),
        }
    }
}

struct ToolCardDisplayStatus {
    label: &'static str,
    detail: Option<String>,
    is_error: bool,
}

fn build_tool_card_display(summary: &ToolCardRender) -> ToolCardDisplay {
    ToolCardDisplay {
        title: tool_action_title(summary),
        status: tool_display_status(summary),
        summary: tool_display_summary(summary),
    }
}

fn build_tool_activity_view(summary: &ToolCardRender, source: &str) -> ToolActivityView {
    let display = build_tool_card_display(summary);
    ToolActivityView {
        key: tool_activity_key(summary, source),
        title: display.title.plain(),
        is_inspection: tool_activity_is_inspection_summary(summary),
        defaults_expanded: summary.diff.is_some(),
    }
}

fn tool_display_status(summary: &ToolCardRender) -> ToolCardDisplayStatus {
    let label = if summary.is_error {
        match summary.error_kind.as_deref() {
            Some("approval_denied") | Some("permission_denied") => "DENIED",
            Some("interrupted") => "INTERRUPTED",
            _ => "ERROR",
        }
    } else {
        "OK"
    };
    let detail = if tool_name_matches(&summary.tool_name, "bash") {
        summary
            .metadata
            .exit_code
            .map(|code| format!("exit {code}"))
    } else {
        None
    };
    ToolCardDisplayStatus {
        label,
        detail,
        is_error: summary.is_error,
    }
}

fn tool_display_summary(summary: &ToolCardRender) -> Option<String> {
    if tool_name_matches(&summary.tool_name, "bash")
        && !summary.is_error
        && summary.preview_lines.is_empty()
        && summary.preview_value.is_none()
        && summary.hidden_lines == 0
    {
        return Some("(no output)".to_owned());
    }
    if let Some(diff) = &summary.diff {
        return Some(format!("diff {}", diff.summary));
    }
    summary.summary.clone()
}

fn tool_action_title(summary: &ToolCardRender) -> ToolCardTitle {
    if tool_name_matches(&summary.tool_name, "bash") {
        let command = call_argument(summary, "command")
            .or_else(|| summary.metadata.call_summary.clone())
            .unwrap_or_else(|| summary.tool_name.clone());
        if !summary.is_error
            && let Some(search) = classify_simple_shell_search(&command)
        {
            return ToolCardTitle::new(
                "Searched",
                search.pattern,
                search.location.map(|location| format!("in {location}")),
            );
        }
        return shell_command_title("Ran", &command);
    }
    if tool_name_matches(&summary.tool_name, "read_file") {
        return ToolCardTitle::new("Read", primary_path(summary), None);
    }
    if tool_name_matches(&summary.tool_name, "write_file") {
        return ToolCardTitle::new(write_file_action(summary), primary_path(summary), None);
    }
    if tool_name_matches(&summary.tool_name, "edit_file") {
        return ToolCardTitle::new("Edited", primary_path(summary), None);
    }
    if tool_name_matches(&summary.tool_name, "delete_file") {
        return ToolCardTitle::new("Deleted", primary_path(summary), None);
    }
    if tool_name_matches(&summary.tool_name, "grep") {
        let pattern = call_argument(summary, "pattern").unwrap_or_else(|| "pattern".to_owned());
        let path = call_argument(summary, "path").unwrap_or_else(|| "workspace".to_owned());
        return ToolCardTitle::new("Searched", pattern, Some(format!("in {path}")));
    }
    if tool_name_matches(&summary.tool_name, "glob") {
        return ToolCardTitle::new(
            "Searched",
            call_argument(summary, "pattern").unwrap_or_else(|| summary.tool_name.clone()),
            None,
        );
    }
    if tool_name_matches(&summary.tool_name, "ls") {
        return ToolCardTitle::new("Listed", primary_path(summary), None);
    }
    match &summary.metadata.call_summary {
        Some(call_summary) => ToolCardTitle::new(
            "Called",
            summary.tool_name.clone(),
            Some(sanitize_call_summary(call_summary)),
        ),
        None => ToolCardTitle::new("Called", summary.tool_name.clone(), None),
    }
}

fn tool_title_spans(title: &ToolCardTitle, max_chars: usize) -> Vec<Span<'static>> {
    let action_style = Style::default()
        .fg(accent_gold())
        .add_modifier(Modifier::BOLD);
    let subject_style = Style::default()
        .fg(accent_blue())
        .add_modifier(Modifier::BOLD);
    let args_style = Style::default().fg(ink());
    let segments = title_segments(title, action_style, subject_style, args_style);
    let plain_len = title.plain().chars().count();
    if plain_len <= max_chars {
        return segments
            .into_iter()
            .map(|(segment, style)| Span::styled(segment, style))
            .collect();
    }

    let mut remaining = max_chars.saturating_sub(3).max(1);
    let mut spans = Vec::new();
    for (segment, style) in segments {
        let segment_len = segment.chars().count();
        if segment_len <= remaining {
            spans.push(Span::styled(segment, style));
            remaining -= segment_len;
            if remaining == 0 {
                spans.push(Span::styled("...", style));
                break;
            }
            continue;
        }
        let truncated = segment.chars().take(remaining).collect::<String>();
        spans.push(Span::styled(format!("{truncated}..."), style));
        break;
    }
    if spans.is_empty() {
        spans.push(Span::styled("...", args_style));
    }
    spans
}

fn title_segments(
    title: &ToolCardTitle,
    action_style: Style,
    subject_style: Style,
    args_style: Style,
) -> Vec<(String, Style)> {
    let mut segments = vec![
        (title.action.clone(), action_style),
        (" ".to_owned(), Style::default()),
        (title.subject.clone(), subject_style),
    ];
    if let Some(args) = &title.args
        && !args.is_empty()
    {
        segments.push((" ".to_owned(), Style::default()));
        segments.push((args.clone(), args_style));
    }
    segments
}

fn shell_command_title(action: &'static str, command: &str) -> ToolCardTitle {
    let command = command.trim();
    let mut parts = command.splitn(2, char::is_whitespace);
    let subject = parts
        .next()
        .filter(|part| !part.is_empty())
        .unwrap_or(command);
    let args = parts
        .next()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned);
    ToolCardTitle::new(action, subject, args)
}

struct ShellSearch {
    pattern: String,
    location: Option<String>,
}

fn classify_simple_shell_search(command: &str) -> Option<ShellSearch> {
    if command_contains_shell_control(command) {
        return None;
    }
    let tokens = simple_shell_tokens(command)?;
    let (program, args) = tokens.split_first()?;
    if program.contains('=') {
        return None;
    }
    let program = program.rsplit('/').next().unwrap_or(program.as_str());
    match program {
        "rg" => classify_pattern_search_args(args, &["-e", "--regexp", "-g", "--glob"]),
        "grep" => classify_pattern_search_args(args, &["-e", "--regexp", "-f", "--file"]),
        "fd" => classify_pattern_search_args(args, &["-e", "--extension", "-t", "--type"]),
        "find" => classify_find_search_args(args),
        _ => None,
    }
}

fn command_contains_shell_control(command: &str) -> bool {
    command
        .chars()
        .any(|character| matches!(character, '|' | '>' | '<' | ';' | '&' | '`' | '$'))
}

fn simple_shell_tokens(command: &str) -> Option<Vec<String>> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut quote = None;
    for character in command.trim().chars() {
        match quote {
            Some(active) if character == active => quote = None,
            Some(_) => current.push(character),
            None if character == '\'' || character == '"' => quote = Some(character),
            None if character.is_whitespace() => {
                if !current.is_empty() {
                    tokens.push(std::mem::take(&mut current));
                }
            }
            None => current.push(character),
        }
    }
    if quote.is_some() {
        return None;
    }
    if !current.is_empty() {
        tokens.push(current);
    }
    (!tokens.is_empty()).then_some(tokens)
}

fn classify_pattern_search_args(
    args: &[String],
    options_with_values: &[&str],
) -> Option<ShellSearch> {
    let mut positional = Vec::new();
    let mut index = 0;
    while index < args.len() {
        let token = &args[index];
        if token == "--" {
            positional.extend(args[index + 1..].iter().cloned());
            break;
        }
        if options_with_values.iter().any(|option| token == option) {
            index += 2;
            continue;
        }
        if token.starts_with("--") && token.contains('=') {
            index += 1;
            continue;
        }
        if token.starts_with('-') {
            index += 1;
            continue;
        }
        positional.push(token.clone());
        index += 1;
    }
    let pattern = positional.first()?.clone();
    Some(ShellSearch {
        pattern,
        location: positional.get(1).cloned(),
    })
}

fn classify_find_search_args(args: &[String]) -> Option<ShellSearch> {
    let location = args
        .iter()
        .find(|token| !token.starts_with('-') && token.as_str() != ".")
        .cloned()
        .or_else(|| args.first().cloned());
    let pattern = args
        .windows(2)
        .find_map(|window| {
            matches!(
                window[0].as_str(),
                "-name" | "-iname" | "-path" | "-ipath" | "-regex"
            )
            .then(|| window[1].clone())
        })
        .or_else(|| {
            args.iter()
                .find(|token| token.contains('*') || token.contains('?'))
                .cloned()
        })?;
    Some(ShellSearch { pattern, location })
}

fn tool_activity_is_inspection_summary(summary: &ToolCardRender) -> bool {
    if tool_name_matches(&summary.tool_name, "read_file")
        || tool_name_matches(&summary.tool_name, "grep")
        || tool_name_matches(&summary.tool_name, "glob")
        || tool_name_matches(&summary.tool_name, "ls")
    {
        return true;
    }
    tool_name_matches(&summary.tool_name, "bash")
        && !summary.is_error
        && call_argument(summary, "command")
            .or_else(|| summary.metadata.call_summary.clone())
            .and_then(|command| classify_simple_shell_search(&command))
            .is_some()
}

fn tool_activity_key(summary: &ToolCardRender, source: &str) -> String {
    summary
        .call_id
        .as_ref()
        .map(|call_id| format!("call:{call_id}"))
        .unwrap_or_else(|| format!("hash:{:016x}", stable_tool_activity_hash(source)))
}

fn stable_tool_activity_hash(source: &str) -> u64 {
    let mut hash = 0xcbf29ce484222325u64;
    for byte in source.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

fn write_file_action(summary: &ToolCardRender) -> &'static str {
    if summary
        .diff
        .as_ref()
        .is_some_and(|diff| diff.files.iter().all(diff_file_is_create))
    {
        "Created"
    } else {
        "Wrote"
    }
}

fn diff_file_is_create(file: &ToolCardDiffFile) -> bool {
    let (added, removed) = file_diff_line_stats(file);
    added > 0 && removed == 0
}

fn primary_path(summary: &ToolCardRender) -> String {
    call_argument(summary, "path")
        .or_else(|| summary.metadata.changed_files.first().cloned())
        .or_else(|| {
            summary
                .diff
                .as_ref()?
                .files
                .first()
                .map(|file| file.path.clone())
        })
        .unwrap_or_else(|| "workspace".to_owned())
}

fn call_argument(summary: &ToolCardRender, key: &str) -> Option<String> {
    let call_summary = summary.metadata.call_summary.as_deref()?;
    call_summary_argument(call_summary, key)
}

fn call_summary_argument(call_summary: &str, key: &str) -> Option<String> {
    let prefix = format!("{key}=");
    let start = call_summary.find(&prefix)? + prefix.len();
    if key == "command" {
        return Some(call_summary[start..].trim().to_owned());
    }
    let tail = &call_summary[start..];
    let end = tail
        .find(|character: char| character.is_whitespace())
        .unwrap_or(tail.len());
    Some(tail[..end].trim().to_owned()).filter(|value| !value.is_empty())
}

fn sanitize_call_summary(call_summary: &str) -> String {
    truncate_inline_text(
        &call_summary
            .split_whitespace()
            .filter(|part| !part.starts_with("call_") && !part.starts_with("id="))
            .collect::<Vec<_>>()
            .join(" "),
        120,
    )
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
        call_id: None,
        tool_name: "result".to_owned(),
        is_error: false,
        error_kind: None,
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
    let error_kind = object
        .get("error_kind")
        .and_then(Value::as_str)
        .or_else(|| {
            object
                .get("error")
                .and_then(|error| error.get("kind"))
                .and_then(Value::as_str)
        })
        .map(str::to_owned);
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
        call_id: object
            .get("call_id")
            .and_then(Value::as_str)
            .map(str::to_owned),
        tool_name,
        is_error,
        error_kind,
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
        stdout_bytes: object.get("stdout_bytes").and_then(Value::as_u64),
        stderr_bytes: object.get("stderr_bytes").and_then(Value::as_u64),
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

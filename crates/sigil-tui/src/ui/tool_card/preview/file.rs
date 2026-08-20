use super::*;

#[cfg(test)]
pub(in crate::ui::tool_card) fn render_read_file_preview(
    summary: &ToolCardRender,
    accent: Color,
    max_content_width: usize,
) -> Vec<Line<'static>> {
    let palette = crate::ui::theme::default_palette();
    render_read_file_preview_with_palette(
        summary,
        accent,
        max_content_width,
        SyntaxThemeId::default(),
        &palette,
    )
}

pub(in crate::ui::tool_card) fn render_read_file_preview_with_palette(
    summary: &ToolCardRender,
    accent: Color,
    max_content_width: usize,
    syntax_theme: SyntaxThemeId,
    palette: &ThemePalette,
) -> Vec<Line<'static>> {
    let preview_lines = summary
        .preview_lines
        .iter()
        .filter(|line| !read_file_truncation_notice(line))
        .cloned()
        .collect::<Vec<_>>();
    let mut lines = Vec::new();
    match summary.preview_kind {
        ToolPreviewKind::Markdown => {
            lines.extend(render_markdown_timeline_lines_with_palette(
                accent,
                Style::default().fg(palette.text_primary),
                &preview_lines.join("\n"),
                MarkdownRenderOptions::tool_preview(max_content_width)
                    .with_syntax_theme(syntax_theme),
                palette,
            ));
        }
        ToolPreviewKind::Json | ToolPreviewKind::Code | ToolPreviewKind::Text => {
            lines.extend(render_numbered_file_preview_lines(
                summary,
                &preview_lines,
                accent,
                syntax_theme,
                palette,
            ));
        }
    }
    lines.extend(render_tool_hidden_tail(
        accent,
        summary.hidden_lines,
        palette,
    ));
    lines
}

fn read_file_truncation_notice(line: &str) -> bool {
    line.starts_with("[sigil: output truncated")
}

fn render_numbered_file_preview_lines(
    summary: &ToolCardRender,
    preview_lines: &[String],
    accent: Color,
    syntax_theme: SyntaxThemeId,
    palette: &ThemePalette,
) -> Vec<Line<'static>> {
    let start_line = summary.metadata.read_offset.unwrap_or(0).saturating_add(1);
    let end_line = start_line
        .saturating_add(preview_lines.len().saturating_sub(1) as u64)
        .max(start_line);
    let line_number_width = end_line.to_string().len();
    let highlighted = (summary.preview_kind == ToolPreviewKind::Code)
        .then_some(summary.preview_language.as_deref())
        .flatten()
        .and_then(|language| {
            highlight_code_to_spans_with_theme(&preview_lines.join("\n"), language, syntax_theme)
        });

    preview_lines
        .iter()
        .enumerate()
        .map(|(index, line)| {
            let line_number = start_line.saturating_add(index as u64);
            let mut spans = vec![
                Span::styled(
                    format!("{line_number:>line_number_width$}"),
                    Style::default()
                        .fg(palette.text_muted)
                        .bg(palette.markdown_code_bg),
                ),
                Span::styled(
                    " │ ",
                    Style::default()
                        .fg(palette.accent_info)
                        .bg(palette.markdown_code_bg),
                ),
            ];
            if let Some(highlighted) = highlighted.as_ref().and_then(|rows| rows.get(index)) {
                spans.extend(highlighted.iter().cloned().map(|mut span| {
                    span.style = span.style.bg(palette.markdown_code_bg);
                    span
                }));
            } else {
                spans.push(Span::styled(
                    if line.is_empty() {
                        " ".to_owned()
                    } else {
                        line.clone()
                    },
                    Style::default()
                        .fg(palette.markdown_code_fg)
                        .bg(palette.markdown_code_bg),
                ));
            }
            timeline_content_line(accent, spans)
        })
        .collect()
}

#[cfg(test)]
pub(in crate::ui::tool_card) fn render_path_list_preview(
    summary: &ToolCardRender,
    accent: Color,
) -> Option<Vec<Line<'static>>> {
    let palette = crate::ui::theme::default_palette();
    render_path_list_preview_with_palette(summary, accent, &palette)
}

pub(in crate::ui::tool_card) fn render_path_list_preview_with_palette(
    summary: &ToolCardRender,
    accent: Color,
    palette: &ThemePalette,
) -> Option<Vec<Line<'static>>> {
    let entries = if let Some(value) = summary.preview_value.as_ref() {
        json_string_list(value)?
    } else {
        let entries = infer_string_list_preview(&summary.preview_lines);
        if entries.is_empty() {
            return None;
        }
        entries
    };

    let mut lines = vec![timeline_section_line_with_palette(
        accent,
        if tool_name_matches(&summary.tool_name, "glob") {
            "matches"
        } else {
            "files"
        },
        palette.accent_info,
        vec![Span::styled(
            format!("{} paths", entries.len() + summary.hidden_lines),
            Style::default().fg(palette.text_muted),
        )],
        palette,
    )];
    if entries.is_empty() {
        lines.push(timeline_content_line(
            accent,
            vec![Span::styled(
                if tool_name_matches(&summary.tool_name, "glob") {
                    "no matches"
                } else {
                    "no files"
                },
                Style::default().fg(palette.text_muted),
            )],
        ));
        return Some(lines);
    }
    for path in entries {
        lines.push(timeline_content_line(
            accent,
            vec![
                Span::styled(
                    "• ",
                    Style::default()
                        .fg(palette.accent_warning)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(path, Style::default().fg(palette.text_primary)),
            ],
        ));
    }
    lines.extend(render_tool_hidden_tail(
        accent,
        summary.hidden_lines,
        palette,
    ));
    Some(lines)
}
#[cfg(test)]
pub(in crate::ui::tool_card) fn render_file_change_preview(
    summary: &ToolCardRender,
    accent: Color,
) -> Option<Vec<Line<'static>>> {
    let palette = crate::ui::theme::default_palette();
    render_file_change_preview_with_palette(summary, accent, &palette)
}

pub(in crate::ui::tool_card) fn render_file_change_preview_with_palette(
    summary: &ToolCardRender,
    accent: Color,
    palette: &ThemePalette,
) -> Option<Vec<Line<'static>>> {
    if summary.metadata.changed_files.is_empty() && summary.diff.is_none() {
        return None;
    }
    let mut lines = Vec::new();
    if !summary.metadata.changed_files.is_empty() {
        lines.push(timeline_section_line_with_palette(
            accent,
            "files",
            palette.accent_info,
            vec![Span::styled(
                format!(
                    "{} {}",
                    summary.metadata.changed_files.len(),
                    file_change_count_label(summary)
                ),
                Style::default().fg(palette.text_muted),
            )],
            palette,
        ));
        for path in &summary.metadata.changed_files {
            lines.push(timeline_content_line(
                accent,
                vec![
                    Span::styled(
                        "• ",
                        Style::default()
                            .fg(palette.accent_success)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(path.clone(), Style::default().fg(palette.text_primary)),
                ],
            ));
        }
    }
    if let Some(diff) = &summary.diff {
        lines.extend(render_tool_diff_preview_with_palette(
            summary, diff, accent, palette,
        ));
    }
    if !summary.preview_lines.is_empty() {
        lines.push(timeline_section_line_with_palette(
            accent,
            "result",
            palette.accent_warning,
            vec![Span::styled(
                file_change_result_label(summary),
                Style::default().fg(palette.text_muted),
            )],
            palette,
        ));
        lines.extend(render_code_preview_lines_with_palette(
            accent,
            &summary.preview_lines,
            palette.markdown_code_bg,
            palette,
        ));
    }
    Some(lines)
}
pub(in crate::ui::tool_card) fn file_change_tool(summary: &ToolCardRender) -> bool {
    tool_name_matches(&summary.tool_name, "write_file")
        || tool_name_matches(&summary.tool_name, "edit_file")
        || tool_name_matches(&summary.tool_name, "delete_file")
        || tool_name_matches(&summary.tool_name, "code_action")
        || tool_name_matches(&summary.tool_name, "code_rename")
}
pub(in crate::ui::tool_card) fn file_change_count_label(summary: &ToolCardRender) -> &'static str {
    if summary.metadata.action.as_deref() == Some("delete")
        || tool_name_matches(&summary.tool_name, "delete_file")
    {
        "deleted"
    } else {
        "changed"
    }
}

pub(in crate::ui::tool_card) fn file_change_result_label(summary: &ToolCardRender) -> &'static str {
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

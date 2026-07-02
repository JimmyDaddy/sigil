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
    let mut lines = vec![timeline_section_line_with_palette(
        accent,
        if summary.preview_kind == ToolPreviewKind::Markdown {
            "doc"
        } else {
            "file"
        },
        palette.accent_info,
        vec![Span::styled(
            if summary.preview_kind == ToolPreviewKind::Markdown {
                "document excerpt"
            } else {
                "file excerpt"
            },
            Style::default().fg(palette.text_muted),
        )],
        palette,
    )];
    match summary.preview_kind {
        ToolPreviewKind::Markdown => {
            lines.extend(render_markdown_timeline_lines_with_palette(
                accent,
                Style::default().fg(palette.text_primary),
                &summary.preview_lines.join("\n"),
                MarkdownRenderOptions::tool_preview(max_content_width)
                    .with_syntax_theme(syntax_theme),
                palette,
            ));
        }
        ToolPreviewKind::Json | ToolPreviewKind::Text => {
            lines.extend(render_code_preview_lines_with_palette(
                accent,
                &summary.preview_lines,
                palette.markdown_code_bg,
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
    let entries = summary
        .preview_value
        .as_ref()
        .and_then(json_string_list)
        .or_else(|| Some(infer_string_list_preview(&summary.preview_lines)))
        .filter(|entries| !entries.is_empty())?;

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

use super::*;

#[cfg(test)]
pub(in crate::ui::tool_card) fn render_generic_tool_preview(
    summary: &ToolCardRender,
    accent: Color,
    max_content_width: usize,
) -> Vec<Line<'static>> {
    let palette = crate::ui::theme::default_palette();
    render_generic_tool_preview_with_palette(
        summary,
        accent,
        max_content_width,
        SyntaxThemeId::default(),
        &palette,
    )
}

pub(in crate::ui::tool_card) fn render_generic_tool_preview_with_palette(
    summary: &ToolCardRender,
    accent: Color,
    max_content_width: usize,
    syntax_theme: SyntaxThemeId,
    palette: &ThemePalette,
) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    if let Some(value) = &summary.preview_value {
        if let Some(lines) = render_grep_preview_with_palette(summary, accent, palette) {
            return lines;
        }
        if let Some(lines) = render_path_list_preview_with_palette(summary, accent, palette) {
            return lines;
        }
        lines.push(timeline_section_line_with_palette(
            accent,
            "tree",
            palette.accent_info,
            vec![Span::styled(
                "structured payload",
                Style::default().fg(palette.text_muted),
            )],
            palette,
        ));
        for line in render_json_tree_preview(value) {
            lines.push(timeline_content_line(
                accent,
                render_code_line_spans_with_bg(
                    &line,
                    palette.accent_info,
                    Style::default().fg(palette.markdown_code_fg),
                    palette.markdown_code_bg,
                ),
            ));
        }
    } else if summary.preview_kind == ToolPreviewKind::Markdown {
        lines.push(timeline_section_line_with_palette(
            accent,
            "md",
            palette.accent_info,
            vec![Span::styled(
                "formatted preview",
                Style::default().fg(palette.text_muted),
            )],
            palette,
        ));
        lines.extend(render_markdown_timeline_lines_with_palette(
            accent,
            Style::default().fg(palette.text_primary),
            &summary.preview_lines.join("\n"),
            MarkdownRenderOptions::tool_preview(max_content_width).with_syntax_theme(syntax_theme),
            palette,
        ));
    } else {
        lines.push(timeline_section_line_with_palette(
            accent,
            summary.preview_kind.label(),
            palette.accent_info,
            vec![Span::styled(
                summary.preview_kind.description(),
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
    lines.extend(render_tool_hidden_tail(
        accent,
        summary.hidden_lines,
        palette,
    ));
    lines
}

#[allow(dead_code)]
pub(in crate::ui::tool_card) fn render_code_preview_lines(
    accent: Color,
    lines: &[String],
    bg: Color,
) -> Vec<Line<'static>> {
    let palette = crate::ui::theme::default_palette();
    render_code_preview_lines_with_palette(accent, lines, bg, &palette)
}

pub(in crate::ui::tool_card) fn render_code_preview_lines_with_palette(
    accent: Color,
    lines: &[String],
    bg: Color,
    palette: &ThemePalette,
) -> Vec<Line<'static>> {
    lines
        .iter()
        .map(|line| {
            timeline_content_line(
                accent,
                render_code_line_spans_with_bg(
                    line,
                    palette.accent_info,
                    Style::default().fg(palette.markdown_code_fg),
                    bg,
                ),
            )
        })
        .collect()
}

pub(in crate::ui::tool_card) fn render_highlighted_code_preview_lines_with_palette(
    accent: Color,
    lines: &[String],
    language: &str,
    syntax_theme: SyntaxThemeId,
    bg: Color,
) -> Option<Vec<Line<'static>>> {
    let highlighted =
        highlight_code_to_spans_with_theme(&lines.join("\n"), language, syntax_theme)?;
    Some(
        highlighted
            .into_iter()
            .map(|spans| {
                let spans = spans
                    .into_iter()
                    .map(|mut span| {
                        span.style = span.style.bg(bg);
                        span
                    })
                    .collect::<Vec<_>>();
                timeline_content_line(accent, spans)
            })
            .collect(),
    )
}

pub(in crate::ui::tool_card) fn render_tool_hidden_tail(
    accent: Color,
    hidden_lines: usize,
    palette: &ThemePalette,
) -> Vec<Line<'static>> {
    if hidden_lines == 0 {
        return Vec::new();
    }
    vec![timeline_content_line(
        accent,
        vec![Span::styled(
            format!("… {} more lines hidden", hidden_lines),
            Style::default()
                .fg(palette.text_muted)
                .add_modifier(Modifier::BOLD),
        )],
    )]
}

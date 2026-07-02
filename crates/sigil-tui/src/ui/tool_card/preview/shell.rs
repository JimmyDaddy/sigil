use super::*;

#[cfg(test)]
pub(in crate::ui::tool_card) fn render_bash_preview(
    summary: &ToolCardRender,
    accent: Color,
) -> Vec<Line<'static>> {
    let palette = crate::ui::theme::default_palette();
    render_bash_preview_with_palette(summary, accent, &palette)
}

pub(in crate::ui::tool_card) fn render_bash_preview_with_palette(
    summary: &ToolCardRender,
    accent: Color,
    palette: &ThemePalette,
) -> Vec<Line<'static>> {
    let section = bash_preview_section_label(summary);
    let subtitle = match (&summary.summary, summary.metadata.exit_code) {
        (Some(summary), Some(code)) => format!("exit {code} · {summary}"),
        (Some(summary), None) => summary.clone(),
        (None, Some(code)) => format!("exit {code}"),
        (None, None) => "terminal tail".to_owned(),
    };
    let mut lines = vec![timeline_section_line_with_palette(
        accent,
        section,
        palette.accent_warning,
        vec![Span::styled(
            subtitle,
            Style::default().fg(palette.text_muted),
        )],
        palette,
    )];
    if summary.preview_lines.is_empty() {
        lines.push(timeline_content_line(
            accent,
            vec![Span::styled(
                "(no output)".to_owned(),
                Style::default().fg(palette.text_muted),
            )],
        ));
    } else {
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
pub(in crate::ui::tool_card) fn bash_preview_section_label(
    summary: &ToolCardRender,
) -> &'static str {
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

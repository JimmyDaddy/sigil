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
    if bash_running_without_output_preview(summary) {
        return Vec::new();
    }
    if summary.preview_lines.is_empty() {
        return vec![timeline_content_line(
            accent,
            vec![Span::styled(
                "— No output",
                Style::default().fg(palette.text_muted),
            )],
        )];
    }
    let mut lines = render_terminal_output_lines(summary, accent, palette);
    lines.extend(render_tool_hidden_tail(
        accent,
        summary.hidden_lines,
        palette,
    ));
    lines
}

pub(in crate::ui::tool_card) fn bash_running_without_output_preview(
    summary: &ToolCardRender,
) -> bool {
    status_kind_from_label(&summary.status) == StatusKind::Running
        && summary.metadata.returned_bytes.unwrap_or(0) == 0
        && summary.metadata.returned_lines.unwrap_or(0) == 0
}

pub(in crate::ui::tool_card) fn bash_running_without_observed_output(
    summary: &ToolCardRender,
) -> bool {
    bash_running_without_output_preview(summary) && summary.metadata.bytes.unwrap_or(0) == 0
}

pub(in crate::ui::tool_card) fn render_bash_command_section_with_palette(
    summary: &ToolCardRender,
    accent: Color,
    max_content_width: usize,
    palette: &ThemePalette,
) -> Vec<Line<'static>> {
    let Some(command) =
        call_argument(summary, "command").filter(|command| !command.trim().is_empty())
    else {
        return Vec::new();
    };
    let command_width = if max_content_width == 0 {
        120
    } else {
        max_content_width.saturating_sub(4).max(1)
    };
    let command_lines = wrap_display_width(&command, command_width);
    let mut lines = command_lines
        .into_iter()
        .enumerate()
        .map(|(index, line)| {
            let marker = if index == 0 { "› " } else { "  " };
            timeline_content_line(
                accent,
                vec![
                    Span::styled(
                        marker,
                        Style::default()
                            .fg(palette.accent_info)
                            .bg(palette.markdown_code_bg)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(
                        line,
                        Style::default()
                            .fg(palette.markdown_code_fg)
                            .bg(palette.markdown_code_bg),
                    ),
                ],
            )
        })
        .collect::<Vec<_>>();
    if let Some(policy) = summary
        .metadata
        .execution_network_policy
        .as_deref()
        .filter(|policy| *policy != "unknown")
    {
        lines.push(timeline_content_line(
            accent,
            vec![Span::styled(
                format!("network: {}", execution_network_display_label(policy)),
                Style::default().fg(palette.text_muted),
            )],
        ));
    }
    lines
}

pub(in crate::ui::tool_card) fn render_terminal_output_lines(
    summary: &ToolCardRender,
    accent: Color,
    palette: &ThemePalette,
) -> Vec<Line<'static>> {
    let stderr = summary.is_error && summary.metadata.stderr_bytes.unwrap_or(0) > 0;
    let (marker, marker_color) = if stderr {
        ("! ", palette.accent_danger)
    } else {
        ("│ ", palette.text_muted)
    };
    summary
        .preview_lines
        .iter()
        .map(|line| {
            timeline_content_line(
                accent,
                vec![
                    Span::styled(
                        marker,
                        Style::default()
                            .fg(marker_color)
                            .bg(palette.markdown_code_bg)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(
                        line.clone(),
                        Style::default()
                            .fg(palette.markdown_code_fg)
                            .bg(palette.markdown_code_bg),
                    ),
                ],
            )
        })
        .collect()
}

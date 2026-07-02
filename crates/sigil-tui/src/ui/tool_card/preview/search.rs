use super::*;

#[cfg(test)]
pub(in crate::ui::tool_card) fn render_grep_preview(
    summary: &ToolCardRender,
    accent: Color,
) -> Option<Vec<Line<'static>>> {
    let palette = crate::ui::theme::default_palette();
    render_grep_preview_with_palette(summary, accent, &palette)
}

pub(in crate::ui::tool_card) fn render_grep_preview_with_palette(
    summary: &ToolCardRender,
    accent: Color,
    palette: &ThemePalette,
) -> Option<Vec<Line<'static>>> {
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

    let mut lines = vec![timeline_section_line_with_palette(
        accent,
        "matches",
        palette.accent_info,
        vec![Span::styled(
            format!("{} files", grouped.len()),
            Style::default().fg(palette.text_muted),
        )],
        palette,
    )];
    for (path, rows) in grouped {
        lines.push(timeline_content_line(
            accent,
            vec![
                section_badge_with_palette("file", palette.accent_secondary, palette),
                Span::raw(" "),
                Span::styled(
                    path,
                    Style::default()
                        .fg(palette.text_primary)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::raw(" "),
                Span::styled(
                    format!("{} hits", rows.len()),
                    Style::default().fg(palette.text_muted),
                ),
            ],
        ));
        for (line_number, text) in rows {
            lines.push(timeline_content_line(
                accent,
                vec![
                    Span::styled(
                        format!("L{line_number:<4}"),
                        Style::default()
                            .fg(palette.accent_warning)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::raw(" "),
                    Span::styled(
                        truncate_inline_text(&text, 140),
                        Style::default().fg(palette.text_primary),
                    ),
                ],
            ));
        }
    }
    lines.extend(render_tool_hidden_tail(
        accent,
        summary.hidden_lines,
        palette,
    ));
    Some(lines)
}

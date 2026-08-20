use super::*;

#[cfg(test)]
pub(in crate::ui::tool_card) fn render_grep_preview(
    summary: &ToolCardRender,
    accent: Color,
) -> Option<Vec<Line<'static>>> {
    let palette = crate::ui::theme::default_palette();
    render_grep_preview_with_palette(summary, accent, 96, &palette)
}

pub(in crate::ui::tool_card) fn render_grep_preview_with_palette(
    summary: &ToolCardRender,
    accent: Color,
    max_content_width: usize,
    palette: &ThemePalette,
) -> Option<Vec<Line<'static>>> {
    let matches = summary.preview_value.as_ref().and_then(json_grep_matches)?;
    if matches.is_empty() {
        return Some(vec![timeline_content_line(
            accent,
            vec![Span::styled(
                "— No matches in workspace",
                Style::default().fg(palette.text_muted),
            )],
        )]);
    }

    let mut grouped = Vec::<(String, Vec<(u64, String)>)>::new();
    for (path, line, text) in matches {
        if let Some((_, rows)) = grouped.iter_mut().find(|(existing, _)| existing == &path) {
            rows.push((line, text));
        } else {
            grouped.push((path, vec![(line, text)]));
        }
    }

    let content_width = if max_content_width == 0 {
        120
    } else {
        max_content_width.saturating_sub(2).max(1)
    };
    let pattern = call_argument(summary, "pattern");
    let mut lines = Vec::new();
    for (path, rows) in grouped {
        let hit_label = format!(
            "{} {}",
            rows.len(),
            if rows.len() == 1 { "hit" } else { "hits" }
        );
        let path_width = content_width
            .saturating_sub(terminal_cell_width(&hit_label) + 2)
            .max(1);
        lines.push(timeline_content_line(
            accent,
            vec![
                Span::styled(
                    truncate_middle_display_width(&path, path_width),
                    Style::default()
                        .fg(palette.text_primary)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::raw("  "),
                Span::styled(hit_label, Style::default().fg(palette.text_muted)),
            ],
        ));
        let line_number_width = rows
            .iter()
            .map(|(line, _)| line.to_string().len())
            .max()
            .unwrap_or(1);
        for (line_number, text) in rows {
            let gutter_width = line_number_width.saturating_add(3);
            let text =
                truncate_display_width(&text, content_width.saturating_sub(gutter_width).max(1));
            let mut spans = vec![
                Span::styled(
                    format!("{line_number:>line_number_width$}"),
                    Style::default().fg(palette.accent_warning),
                ),
                Span::styled(" │ ", Style::default().fg(palette.text_muted)),
            ];
            spans.extend(search_match_spans(&text, pattern.as_deref(), palette));
            lines.push(timeline_content_line(accent, spans));
        }
    }
    lines.extend(render_tool_hidden_tail(
        accent,
        summary.hidden_lines,
        palette,
    ));
    Some(lines)
}

fn search_match_spans(
    text: &str,
    pattern: Option<&str>,
    palette: &ThemePalette,
) -> Vec<Span<'static>> {
    let Some(pattern) = pattern.filter(|pattern| !pattern.is_empty()) else {
        return vec![Span::styled(
            text.to_owned(),
            Style::default().fg(palette.text_primary),
        )];
    };
    let Some(start) = text.find(pattern) else {
        return vec![Span::styled(
            text.to_owned(),
            Style::default().fg(palette.text_primary),
        )];
    };
    let end = start + pattern.len();
    vec![
        Span::styled(
            text[..start].to_owned(),
            Style::default().fg(palette.text_primary),
        ),
        Span::styled(
            text[start..end].to_owned(),
            Style::default()
                .fg(palette.accent_info)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            text[end..].to_owned(),
            Style::default().fg(palette.text_primary),
        ),
    ]
}

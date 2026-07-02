use super::*;

#[cfg(test)]
pub(in crate::ui::tool_card) fn render_tool_diff_preview(
    summary: &ToolCardRender,
    diff: &ToolCardDiff,
    accent: Color,
) -> Vec<Line<'static>> {
    let palette = crate::ui::theme::default_palette();
    render_tool_diff_preview_with_palette(summary, diff, accent, &palette)
}

pub(in crate::ui::tool_card) fn render_tool_diff_preview_with_palette(
    summary: &ToolCardRender,
    diff: &ToolCardDiff,
    accent: Color,
    palette: &ThemePalette,
) -> Vec<Line<'static>> {
    let mut lines = vec![timeline_section_line_with_palette(
        accent,
        "diff",
        palette.accent_warning,
        vec![Span::styled(
            diff.summary.clone(),
            Style::default().fg(palette.text_muted),
        )],
        palette,
    )];
    for file in &diff.files {
        lines.push(timeline_content_line(
            accent,
            vec![
                timeline_badge_with_palette(
                    tool_diff_file_label(summary, file),
                    palette.accent_info,
                    palette,
                ),
                Span::raw(" "),
                Span::styled(file.path.clone(), Style::default().fg(palette.text_primary)),
                Span::raw(" "),
                Span::styled(
                    diff_hunk_summary(file),
                    Style::default().fg(palette.text_muted),
                ),
            ],
        ));
        let numbered_lines = number_unified_diff_lines(file.lines.iter().map(String::as_str));
        let line_number_width = diff_line_number_width(&numbered_lines);
        for line in numbered_lines {
            if matches!(line.kind, DiffLineKind::Hunk) {
                continue;
            }
            lines.push(render_tool_diff_line_with_palette(
                accent,
                line,
                line_number_width,
                palette,
            ));
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
                        .fg(palette.accent_warning)
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
                    .fg(palette.accent_warning)
                    .add_modifier(Modifier::BOLD),
            )],
        ));
    }
    lines
}

pub(in crate::ui::tool_card) fn diff_hunk_summary(file: &ToolCardDiffFile) -> String {
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

pub(in crate::ui::tool_card) fn tool_diff_file_label(
    summary: &ToolCardRender,
    file: &ToolCardDiffFile,
) -> &'static str {
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

pub(in crate::ui::tool_card) fn file_diff_line_stats(file: &ToolCardDiffFile) -> (usize, usize) {
    file.lines.iter().fold((0, 0), |(added, removed), line| {
        match diff_line_kind(line) {
            DiffLineKind::Added => (added + 1, removed),
            DiffLineKind::Removed => (added, removed + 1),
            _ => (added, removed),
        }
    })
}

pub(in crate::ui::tool_card) fn render_tool_diff_line_with_palette(
    accent: Color,
    line: NumberedDiffLine<'_>,
    line_number_width: usize,
    palette: &ThemePalette,
) -> Line<'static> {
    let (marker_color, body_style) = diff_line_style_for_palette(line.kind, palette);
    timeline_content_line(
        accent,
        vec![
            Span::styled("│", Style::default().fg(marker_color)),
            Span::styled(
                diff_line_number_text(line.old_line, line_number_width),
                tool_diff_old_line_number_style_with_palette(line, palette),
            ),
            Span::styled(" ", Style::default().fg(palette.text_muted)),
            Span::styled(
                diff_line_number_text(line.new_line, line_number_width),
                tool_diff_new_line_number_style_with_palette(line, palette),
            ),
            Span::styled("│ ", Style::default().fg(palette.text_muted)),
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

pub(in crate::ui::tool_card) fn tool_diff_old_line_number_style_with_palette(
    line: NumberedDiffLine<'_>,
    palette: &ThemePalette,
) -> Style {
    if line.old_line.is_none() {
        return Style::default().fg(palette.text_muted);
    }
    let style = Style::default().fg(palette.diff_removed_fg);
    if matches!(line.kind, DiffLineKind::Removed) {
        style.add_modifier(Modifier::BOLD)
    } else {
        style
    }
}

pub(in crate::ui::tool_card) fn tool_diff_new_line_number_style_with_palette(
    line: NumberedDiffLine<'_>,
    palette: &ThemePalette,
) -> Style {
    if line.new_line.is_none() {
        return Style::default().fg(palette.text_muted);
    }
    let style = Style::default().fg(palette.diff_added_fg);
    if matches!(line.kind, DiffLineKind::Added) {
        style.add_modifier(Modifier::BOLD)
    } else {
        style
    }
}

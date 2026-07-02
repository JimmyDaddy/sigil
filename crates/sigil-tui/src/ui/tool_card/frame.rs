use super::*;

pub(super) fn tool_card_header_line(
    display: &ToolCardDisplay,
    marker_style: Style,
    expanded: bool,
    max_content_width: usize,
    palette: &ThemePalette,
) -> Line<'static> {
    let mut spans = vec![Span::styled("●", marker_style), Span::raw(" ")];
    spans.extend(tool_title_spans_with_palette(
        &display.title,
        tool_title_width(display, max_content_width),
        palette,
    ));
    spans.push(Span::raw("  "));
    let status_indicator = StatusIndicator::animated(display.status.kind);
    spans.push(Span::styled(
        format!(" {} {} ", status_indicator.symbol(), display.status.label),
        tool_status_style(display.status.kind, palette),
    ));
    if let Some(detail) = &display.status.detail {
        spans.push(Span::raw(" "));
        spans.push(Span::styled(
            detail.clone(),
            if display.status.is_error {
                Style::default()
                    .fg(palette.accent_danger)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(palette.text_muted)
            },
        ));
    }
    if expanded {
        spans.push(Span::raw(" "));
        spans.push(Span::styled(
            "▾",
            Style::default().fg(palette.accent_success),
        ));
    }
    Line::from(spans)
}

pub(super) fn tool_card_frame_lines(
    lines: Vec<Line<'static>>,
    selected: bool,
    max_content_width: usize,
    marker_style: Style,
    palette: &ThemePalette,
) -> Vec<Line<'static>> {
    ToolCardFrame {
        selected,
        max_content_width,
        marker_style,
        palette,
    }
    .render(lines)
}

pub(super) struct ToolCardFrame<'a> {
    selected: bool,
    max_content_width: usize,
    marker_style: Style,
    palette: &'a ThemePalette,
}

impl ToolCardFrame<'_> {
    fn render(&self, lines: Vec<Line<'static>>) -> Vec<Line<'static>> {
        let card_width = if self.max_content_width == 0 {
            160
        } else {
            self.max_content_width
        };
        lines
            .into_iter()
            .enumerate()
            .map(|(index, line)| {
                let line = if index == 0 {
                    line
                } else {
                    tool_card_body_frame_line(line, index == 1, self.marker_style, self.palette)
                };
                if self.selected {
                    tool_card_selected_line(line, card_width, self.palette)
                } else {
                    line
                }
            })
            .collect()
    }
}

pub(super) fn tool_card_body_frame_line(
    line: Line<'static>,
    first_body_line: bool,
    marker_style: Style,
    palette: &ThemePalette,
) -> Line<'static> {
    let marker = if first_body_line { "└ " } else { "  " };
    let branch_style = if first_body_line {
        marker_style
    } else {
        Style::default().fg(palette.text_muted)
    };
    let mut spans = vec![Span::styled(marker, branch_style)];
    spans.extend(strip_timeline_content_indent(line.spans));
    Line::from(spans)
}

pub(super) fn tool_card_activity_marker_style(
    status: StatusKind,
    hovered: bool,
    palette: &ThemePalette,
) -> Style {
    if hovered {
        Style::default()
            .fg(palette.accent_warning)
            .add_modifier(Modifier::BOLD)
    } else {
        StatusIndicator::static_kind(status).style_with_palette(palette)
    }
}

pub(super) fn strip_timeline_content_indent(spans: Vec<Span<'static>>) -> Vec<Span<'static>> {
    let mut iter = spans.into_iter();
    let Some(first) = iter.next() else {
        return Vec::new();
    };
    let mut stripped = Vec::new();
    let first_text = first.content.as_ref();
    if first_text == "  " {
        // Drop the generic timeline indent; the tool-card frame supplies it.
    } else if let Some(rest) = first_text.strip_prefix("  ") {
        if !rest.is_empty() {
            stripped.push(Span::styled(rest.to_owned(), first.style));
        }
    } else {
        stripped.push(first);
    }
    stripped.extend(iter);
    stripped
}

pub(super) fn tool_card_selected_line(
    line: Line<'static>,
    card_width: usize,
    palette: &ThemePalette,
) -> Line<'static> {
    let bg = palette.surface_selection;
    let mut spans = line
        .spans
        .into_iter()
        .map(|span| {
            let mut style = span.style;
            style.bg = Some(bg);
            Span::styled(span.content, style)
        })
        .collect::<Vec<_>>();
    let width = spans_display_width(&spans);
    if card_width > width {
        spans.push(Span::styled(
            " ".repeat(card_width - width),
            Style::default().bg(bg),
        ));
    }
    Line::from(spans)
}

pub(super) fn spans_display_width(spans: &[Span<'static>]) -> usize {
    spans
        .iter()
        .map(|span| UnicodeWidthStr::width(span.content.as_ref()))
        .sum()
}

pub(super) fn tool_title_width(display: &ToolCardDisplay, max_content_width: usize) -> usize {
    if max_content_width == 0 {
        return 160;
    }
    max_content_width
        .saturating_sub(display.status.label.chars().count() + 12)
        .clamp(32, 160)
}

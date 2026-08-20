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
    let status_text = if display.status.kind == StatusKind::Success && display.status.label == "OK"
    {
        format!(" {} ", status_indicator.symbol())
    } else {
        format!(" {} {} ", status_indicator.symbol(), display.status.label)
    };
    spans.push(Span::styled(
        status_text,
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
    result_frame: Option<(usize, ToolResultPresentation)>,
    selected: bool,
    max_content_width: usize,
    marker_style: Style,
    palette: &ThemePalette,
) -> Vec<Line<'static>> {
    ToolCardFrame {
        result_frame,
        selected,
        max_content_width,
        marker_style,
        palette,
    }
    .render(lines)
}

pub(super) struct ToolCardFrame<'a> {
    result_frame: Option<(usize, ToolResultPresentation)>,
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
                } else if let Some((result_start_index, presentation)) = self.result_frame
                    && index >= result_start_index
                {
                    tool_card_result_frame_line(
                        line,
                        card_width,
                        presentation,
                        self.selected,
                        self.palette,
                    )
                } else {
                    tool_card_body_frame_line(line, index == 1, self.marker_style, self.palette)
                };
                let result_line = self
                    .result_frame
                    .is_some_and(|(result_start_index, _)| index >= result_start_index);
                if self.selected && !result_line {
                    tool_card_selected_line(line, card_width, self.palette)
                } else {
                    line
                }
            })
            .collect()
    }
}

fn tool_card_result_frame_line(
    line: Line<'static>,
    card_width: usize,
    presentation: ToolResultPresentation,
    selected: bool,
    palette: &ThemePalette,
) -> Line<'static> {
    let rail_color = match presentation {
        ToolResultPresentation::SearchMatches => palette.accent_info,
        ToolResultPresentation::CodeExcerpt => palette.accent_secondary,
        ToolResultPresentation::TerminalOutput => palette.accent_warning,
        ToolResultPresentation::FileTree => palette.accent_success,
        ToolResultPresentation::UnifiedDiff => palette.accent_warning,
        ToolResultPresentation::StructuredData => palette.accent_info,
        ToolResultPresentation::Document => palette.accent_secondary,
        ToolResultPresentation::PlainText => palette.text_muted,
    };
    let rail_background = if selected {
        palette.surface_selection
    } else {
        palette.surface_panel_alt
    };
    let mut spans = vec![Span::styled(
        "│ ",
        Style::default()
            .fg(rail_color)
            .bg(rail_background)
            .add_modifier(Modifier::BOLD),
    )];
    spans.extend(strip_timeline_content_indent(line.spans));
    tool_card_surface_line(spans, card_width, palette.surface_panel_alt)
}

fn tool_card_surface_line(
    spans: Vec<Span<'static>>,
    card_width: usize,
    background: Color,
) -> Line<'static> {
    let mut spans = spans
        .into_iter()
        .map(|span| {
            let mut style = span.style;
            if style.bg.is_none() {
                style.bg = Some(background);
            }
            Span::styled(span.content, style)
        })
        .collect::<Vec<_>>();
    let width = spans_display_width(&spans);
    if card_width > width {
        spans.push(Span::styled(
            " ".repeat(card_width - width),
            Style::default().bg(background),
        ));
    }
    Line::from(spans)
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
        .map(|span| terminal_cell_width(span.content.as_ref()))
        .sum()
}

pub(super) fn tool_title_width(display: &ToolCardDisplay, max_content_width: usize) -> usize {
    if max_content_width == 0 {
        return 160;
    }
    let status_width = if display.status.kind == StatusKind::Success && display.status.label == "OK"
    {
        3
    } else {
        display.status.label.chars().count() + 4
    };
    max_content_width
        .saturating_sub(status_width + 8)
        .clamp(32, 160)
}

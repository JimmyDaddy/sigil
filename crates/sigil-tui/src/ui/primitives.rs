use ratatui::{
    style::{Color, Modifier, Style},
    text::{Line, Span},
};

use super::theme::ThemePalette;

pub(crate) fn section_badge_with_palette(
    label: &str,
    accent: Color,
    palette: &ThemePalette,
) -> Span<'static> {
    Span::styled(
        format!(" {label} "),
        Style::default()
            .fg(accent)
            .bg(palette.surface_badge)
            .add_modifier(Modifier::BOLD),
    )
}

pub(crate) fn timeline_badge_with_palette(
    label: &str,
    color: Color,
    palette: &ThemePalette,
) -> Span<'static> {
    Span::styled(
        format!(" {label} "),
        Style::default()
            .fg(color)
            .bg(palette.surface_badge)
            .add_modifier(Modifier::BOLD),
    )
}

pub(crate) fn timeline_header_line_with_palette(
    label: &str,
    accent: Color,
    subtitle: &str,
    palette: &ThemePalette,
) -> Line<'static> {
    let mut spans = vec![Span::styled(
        label.to_owned(),
        Style::default().fg(accent).add_modifier(Modifier::BOLD),
    )];
    if !subtitle.is_empty() {
        spans.push(Span::raw(" "));
        spans.push(Span::styled(
            subtitle.to_owned(),
            Style::default()
                .fg(palette.text_muted)
                .add_modifier(Modifier::BOLD),
        ));
    }
    Line::from(spans)
}

pub(crate) fn timeline_minor_header_line_with_palette(
    label: &str,
    accent: Color,
    detail: &str,
    palette: &ThemePalette,
) -> Line<'static> {
    let mut spans = vec![Span::styled(label.to_owned(), Style::default().fg(accent))];
    if !detail.is_empty() {
        spans.push(Span::raw(" "));
        spans.push(Span::styled(
            detail.to_owned(),
            Style::default().fg(palette.text_muted),
        ));
    }
    Line::from(spans)
}

pub(crate) fn timeline_content_line(_accent: Color, spans: Vec<Span<'static>>) -> Line<'static> {
    let mut line = vec![Span::raw("  ")];
    line.extend(spans);
    Line::from(line)
}

pub(crate) fn spans_with_background(spans: Vec<Span<'static>>, bg: Color) -> Vec<Span<'static>> {
    spans
        .into_iter()
        .map(|span| {
            let mut style = span.style;
            style.bg = Some(bg);
            Span::styled(span.content, style)
        })
        .collect()
}

#[cfg(all(test, not(sigil_tui_test_slice_app_input_flow)))]
#[path = "tests/primitives_tests.rs"]
mod tests;

pub(crate) fn timeline_section_line_with_palette(
    rail_accent: Color,
    badge_label: &str,
    badge_accent: Color,
    detail_spans: Vec<Span<'static>>,
    palette: &ThemePalette,
) -> Line<'static> {
    let mut spans = vec![section_badge_with_palette(
        badge_label,
        badge_accent,
        palette,
    )];
    if !detail_spans.is_empty() {
        spans.push(Span::raw(" "));
        spans.extend(detail_spans);
    }
    timeline_content_line(rail_accent, spans)
}

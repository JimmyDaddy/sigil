use ratatui::{
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders},
};

use super::theme::{badge_bg, dim, muted};

#[allow(dead_code)]
pub(crate) fn themed_block(
    title: &str,
    subtitle: Option<&str>,
    accent: Color,
    bg: Color,
) -> Block<'static> {
    let mut spans = vec![Span::styled(
        format!(" {title} "),
        Style::default()
            .fg(accent)
            .bg(badge_bg())
            .add_modifier(Modifier::BOLD),
    )];
    if let Some(subtitle) = subtitle {
        spans.push(Span::raw(" "));
        spans.push(Span::styled(
            subtitle.to_owned(),
            Style::default().fg(muted()),
        ));
    }

    Block::default()
        .title(Line::from(spans))
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(accent))
        .style(Style::default().bg(bg))
}

pub(crate) fn section_badge(label: &str, accent: Color) -> Span<'static> {
    Span::styled(
        format!(" {label} "),
        Style::default()
            .fg(accent)
            .bg(badge_bg())
            .add_modifier(Modifier::BOLD),
    )
}

pub(crate) fn timeline_badge(label: &str, color: Color) -> Span<'static> {
    Span::styled(
        format!(" {label} "),
        Style::default()
            .fg(color)
            .bg(badge_bg())
            .add_modifier(Modifier::BOLD),
    )
}

pub(crate) fn timeline_header_line(label: &str, accent: Color, subtitle: &str) -> Line<'static> {
    let mut spans = vec![Span::styled(
        label.to_owned(),
        Style::default().fg(accent).add_modifier(Modifier::BOLD),
    )];
    if !subtitle.is_empty() {
        spans.push(Span::raw(" "));
        spans.push(Span::styled(
            subtitle.to_owned(),
            Style::default().fg(dim()).add_modifier(Modifier::BOLD),
        ));
    }
    Line::from(spans)
}

pub(crate) fn timeline_minor_header_line(
    label: &str,
    accent: Color,
    detail: &str,
) -> Line<'static> {
    let mut spans = vec![Span::styled(label.to_owned(), Style::default().fg(accent))];
    if !detail.is_empty() {
        spans.push(Span::raw(" "));
        spans.push(Span::styled(detail.to_owned(), Style::default().fg(dim())));
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

pub(crate) fn timeline_section_line(
    rail_accent: Color,
    badge_label: &str,
    badge_accent: Color,
    detail_spans: Vec<Span<'static>>,
) -> Line<'static> {
    let mut spans = vec![section_badge(badge_label, badge_accent)];
    if !detail_spans.is_empty() {
        spans.push(Span::raw(" "));
        spans.extend(detail_spans);
    }
    timeline_content_line(rail_accent, spans)
}

use std::time::{SystemTime, UNIX_EPOCH};

use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, Paragraph, Wrap},
};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use crate::view_model::{LivePanelViewModel, LiveProgressViewModel};

use super::{
    geometry::inset_rect,
    theme::{ink, phase_accent, shell_bg},
};

pub(crate) const LIVE_PANEL_BOTTOM_PADDING: u16 = 1;
pub(crate) const LIVE_PROGRESS_ROWS: u16 = 3;

pub(crate) fn render_live_panel(frame: &mut Frame, area: Rect, view_model: &LivePanelViewModel) {
    frame.render_widget(
        Block::default().style(Style::default().bg(shell_bg())),
        area,
    );
    if area.width == 0 || area.height == 0 {
        return;
    }

    let inner = inset_rect(area, 1, 0);
    if inner.width == 0 || inner.height == 0 {
        return;
    }
    let content_frame = Rect::new(
        inner.x,
        inner.y,
        inner.width,
        inner
            .height
            .saturating_sub(LIVE_PANEL_BOTTOM_PADDING)
            .max(1),
    );

    let mut lines = view_model.transcript_lines.clone();
    if let Some(progress) = &view_model.progress {
        if !lines.is_empty() {
            lines.push(Line::raw(String::new()));
        }
        lines.extend(render_live_progress_lines(
            progress,
            phase_accent(&view_model.phase),
        ));
    }
    let visual_lines = wrap_live_panel_lines(lines, content_frame.width as usize);
    let visual_start = visual_lines
        .len()
        .saturating_sub(content_frame.height as usize);
    let visual_lines = visual_lines
        .into_iter()
        .skip(visual_start)
        .collect::<Vec<_>>();
    let content_height = visual_lines.len().max(1) as u16;
    let content_y = content_frame
        .y
        .saturating_add(content_frame.height.saturating_sub(content_height));
    let content_area = Rect::new(
        content_frame.x,
        content_y,
        content_frame.width,
        content_height,
    );
    frame.render_widget(
        Paragraph::new(Text::from(visual_lines))
            .style(Style::default().bg(shell_bg()))
            .wrap(Wrap { trim: false }),
        content_area,
    );
}

fn wrap_live_panel_lines(lines: Vec<Line<'static>>, width: usize) -> Vec<Line<'static>> {
    let width = width.max(1);
    let mut rows = Vec::new();
    for line in lines {
        rows.extend(wrap_live_panel_line(line, width));
    }
    if rows.is_empty() {
        rows.push(Line::raw(String::new()));
    }
    rows
}

fn wrap_live_panel_line(line: Line<'static>, width: usize) -> Vec<Line<'static>> {
    let mut rows = Vec::new();
    let mut current_spans = Vec::new();
    let mut current_width = 0usize;
    let line_style = line.style;
    let line_alignment = line.alignment;

    for span in line.spans {
        let mut segment = String::new();
        for grapheme in span.content.as_ref().graphemes(true) {
            let grapheme_width = UnicodeWidthStr::width(grapheme).max(1);
            if current_width > 0 && current_width + grapheme_width > width {
                push_live_panel_segment(&mut current_spans, &mut segment, span.style);
                rows.push(live_panel_wrapped_line(
                    std::mem::take(&mut current_spans),
                    line_style,
                    line_alignment,
                ));
                current_width = 0;
            }
            segment.push_str(grapheme);
            current_width += grapheme_width;
        }
        push_live_panel_segment(&mut current_spans, &mut segment, span.style);
    }

    rows.push(live_panel_wrapped_line(
        current_spans,
        line_style,
        line_alignment,
    ));
    rows
}

fn push_live_panel_segment(spans: &mut Vec<Span<'static>>, segment: &mut String, style: Style) {
    if segment.is_empty() {
        return;
    }
    spans.push(Span::styled(std::mem::take(segment), style));
}

fn live_panel_wrapped_line(
    spans: Vec<Span<'static>>,
    style: Style,
    alignment: Option<ratatui::layout::Alignment>,
) -> Line<'static> {
    Line {
        spans,
        style,
        alignment,
    }
}

pub(crate) fn render_live_progress_lines(
    progress: &LiveProgressViewModel,
    accent: Color,
) -> Vec<Line<'static>> {
    let spinner = live_spinner_frame();
    vec![
        Line::from(vec![
            Span::styled(
                spinner,
                Style::default().fg(accent).add_modifier(Modifier::BOLD),
            ),
            Span::raw(" "),
            Span::styled(
                format!("{}...", progress.title),
                Style::default().fg(accent).add_modifier(Modifier::BOLD),
            ),
        ]),
        Line::from(vec![
            Span::styled("  ↳ ", Style::default().fg(accent)),
            Span::styled(progress.detail.clone(), Style::default().fg(ink())),
        ]),
    ]
}

pub(crate) fn live_spinner_frame() -> &'static str {
    const FRAMES: &[&str] = &["▰▱▱▱", "▰▰▱▱", "▱▰▰▱", "▱▱▰▰", "▱▱▱▰"];
    let tick = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() / 120)
        .unwrap_or(0);
    FRAMES[(tick as usize) % FRAMES.len()]
}

#[cfg(test)]
#[path = "tests/live_panel_tests.rs"]
mod tests;

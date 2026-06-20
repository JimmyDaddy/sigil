use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, Paragraph, Wrap},
};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use crate::{
    app::AppState,
    view_model::{
        LivePanelViewModel, LiveProgressViewModel, PlanApprovalViewModel, TaskStripRowViewModel,
        TaskStripViewModel,
    },
};

use super::{
    geometry::inset_rect,
    status_indicator::{StatusIndicator, StatusKind},
    text::truncate_display_width,
    theme::{dock_edge, ink, phase_accent, shell_bg, status_band_bg},
};

pub(crate) const LIVE_PANEL_BOTTOM_PADDING: u16 = 1;
pub(crate) const LIVE_PROGRESS_ROWS: u16 = 2;
const LIVE_PLAN_APPROVAL_ROWS: u16 = 2;
const LIVE_TASK_ROW_LIMIT: usize = 4;

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

    let status_height = live_status_rows(view_model).min(content_frame.height.saturating_sub(1));
    let transcript_frame = Rect::new(
        content_frame.x,
        content_frame.y,
        content_frame.width,
        content_frame.height.saturating_sub(status_height).max(1),
    );
    let visual_lines = wrap_live_panel_lines(
        view_model.transcript_lines.clone(),
        transcript_frame.width as usize,
    );
    let visual_start = visual_lines
        .len()
        .saturating_sub(transcript_frame.height as usize);
    let visual_lines = visual_lines
        .into_iter()
        .skip(visual_start)
        .collect::<Vec<_>>();
    let content_height = visual_lines.len().max(1) as u16;
    let content_y = transcript_frame
        .y
        .saturating_add(transcript_frame.height.saturating_sub(content_height));
    let content_area = Rect::new(
        transcript_frame.x,
        content_y,
        transcript_frame.width,
        content_height,
    );
    frame.render_widget(
        Paragraph::new(Text::from(visual_lines))
            .style(Style::default().bg(shell_bg()))
            .wrap(Wrap { trim: false }),
        content_area,
    );

    if status_height > 0 {
        let status_area = Rect::new(
            content_frame.x,
            content_frame
                .y
                .saturating_add(content_frame.height.saturating_sub(status_height)),
            content_frame.width,
            status_height,
        );
        render_live_status_band(frame, status_area, view_model);
    }
}

pub(crate) fn live_status_rows_for_app(app: &AppState) -> u16 {
    let progress_rows = if app.live_activity_summary().is_some() {
        LIVE_PROGRESS_ROWS
    } else {
        0
    };
    let plan_rows = if app.pending_plan_approval().is_some() {
        LIVE_PLAN_APPROVAL_ROWS
    } else {
        0
    };
    let task_rows = app
        .task_strip_view()
        .map(|view| live_task_strip_rows(view.rows.len()))
        .unwrap_or(0);
    live_status_rows_with_separator(
        progress_rows
            .saturating_add(plan_rows)
            .saturating_add(task_rows),
    )
}

pub(crate) fn live_status_rows(view_model: &LivePanelViewModel) -> u16 {
    let progress_rows = if view_model.progress.is_some() {
        LIVE_PROGRESS_ROWS
    } else {
        0
    };
    let plan_rows = if view_model.plan_approval.is_some() {
        LIVE_PLAN_APPROVAL_ROWS
    } else {
        0
    };
    let task_rows = view_model
        .task_strip
        .as_ref()
        .map(|view| live_task_strip_rows(view.rows.len()))
        .unwrap_or(0);
    live_status_rows_with_separator(
        progress_rows
            .saturating_add(plan_rows)
            .saturating_add(task_rows),
    )
}

fn live_status_rows_with_separator(content_rows: u16) -> u16 {
    if content_rows == 0 {
        return 0;
    }
    content_rows.saturating_add(1)
}

fn live_task_strip_rows(row_count: usize) -> u16 {
    if row_count == 0 {
        return 0;
    }
    1 + row_count.min(LIVE_TASK_ROW_LIMIT) as u16
}

fn render_live_status_band(frame: &mut Frame, area: Rect, view_model: &LivePanelViewModel) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    frame.render_widget(
        Block::default().style(Style::default().bg(status_band_bg())),
        area,
    );
    let accent = phase_accent(&view_model.phase);
    render_status_separator(frame, area);
    render_status_left_rail(frame, area, accent);
    let content_area = Rect::new(
        area.x.saturating_add(2),
        area.y.saturating_add(1),
        area.width.saturating_sub(2),
        area.height.saturating_sub(1),
    );
    if content_area.width == 0 || content_area.height == 0 {
        return;
    }

    let mut lines = Vec::new();
    if let Some(progress) = &view_model.progress {
        lines.extend(render_live_progress_lines(progress, accent));
    }
    if let Some(plan_approval) = &view_model.plan_approval {
        lines.extend(render_plan_approval_lines(
            plan_approval,
            area.width as usize,
        ));
    }
    if let Some(task_strip) = &view_model.task_strip {
        lines.extend(render_task_strip_lines(task_strip, area.width as usize));
    }
    let lines = lines
        .into_iter()
        .take(content_area.height as usize)
        .collect::<Vec<_>>();

    frame.render_widget(
        Paragraph::new(Text::from(lines))
            .style(Style::default().bg(status_band_bg()))
            .wrap(Wrap { trim: false }),
        content_area,
    );
}

fn render_status_separator(frame: &mut Frame, area: Rect) {
    if area.width == 0 {
        return;
    }
    let line = Line::from(vec![Span::styled(
        "─".repeat(area.width as usize),
        Style::default().fg(dock_edge()).bg(status_band_bg()),
    )]);
    frame.render_widget(
        Paragraph::new(Text::from(vec![line])).style(Style::default().bg(status_band_bg())),
        Rect::new(area.x, area.y, area.width, 1),
    );
}

fn render_status_left_rail(frame: &mut Frame, area: Rect, accent: Color) {
    if area.width == 0 || area.height <= 1 {
        return;
    }
    let lines = (0..area.height.saturating_sub(1))
        .map(|_| {
            Line::from(vec![Span::styled(
                "▌",
                Style::default().fg(accent).bg(status_band_bg()),
            )])
        })
        .collect::<Vec<_>>();
    frame.render_widget(
        Paragraph::new(Text::from(lines)).style(Style::default().bg(status_band_bg())),
        Rect::new(
            area.x,
            area.y.saturating_add(1),
            1,
            area.height.saturating_sub(1),
        ),
    );
}

fn render_plan_approval_lines(plan: &PlanApprovalViewModel, width: usize) -> Vec<Line<'static>> {
    let title_width = width.saturating_sub(24);
    let scope = truncate_display_width(&plan.scope_summary, title_width);
    vec![
        Line::from(vec![
            Span::styled(
                "Plan",
                Style::default()
                    .fg(super::theme::accent_gold())
                    .bg(status_band_bg())
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(" "),
            Span::styled(
                "ready",
                Style::default()
                    .fg(ink())
                    .bg(status_band_bg())
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!("  ·  {}  ·  {}", plan.hash, scope),
                Style::default()
                    .fg(super::theme::muted())
                    .bg(status_band_bg()),
            ),
        ]),
        Line::from(vec![
            Span::styled("A", Style::default().fg(ink()).bg(status_band_bg())),
            Span::styled(
                " ask",
                Style::default()
                    .fg(super::theme::muted())
                    .bg(status_band_bg()),
            ),
            Span::styled("  W", Style::default().fg(ink()).bg(status_band_bg())),
            Span::styled(
                " workspace edits",
                Style::default()
                    .fg(super::theme::muted())
                    .bg(status_band_bg()),
            ),
            Span::styled("  C", Style::default().fg(ink()).bg(status_band_bg())),
            Span::styled(
                " continue",
                Style::default()
                    .fg(super::theme::muted())
                    .bg(status_band_bg()),
            ),
            Span::styled("  Esc", Style::default().fg(ink()).bg(status_band_bg())),
            Span::styled(
                " discard",
                Style::default()
                    .fg(super::theme::muted())
                    .bg(status_band_bg()),
            ),
        ]),
    ]
}

fn render_task_strip_lines(task_strip: &TaskStripViewModel, width: usize) -> Vec<Line<'static>> {
    if task_strip.rows.is_empty() {
        return Vec::new();
    }
    let mut lines = Vec::with_capacity(1 + task_strip.rows.len().min(LIVE_TASK_ROW_LIMIT));
    lines.push(render_task_strip_header(task_strip, width));
    lines.extend(
        task_strip
            .rows
            .iter()
            .take(LIVE_TASK_ROW_LIMIT)
            .map(|row| render_task_strip_row(row, width)),
    );
    lines
}

fn render_task_strip_header(task_strip: &TaskStripViewModel, width: usize) -> Line<'static> {
    let title = task_strip
        .title
        .strip_prefix("Task ")
        .unwrap_or(&task_strip.title);
    let title = truncate_display_width(title, width.saturating_sub(8));
    let detail_width = width.saturating_sub("Task  ".chars().count() + title.chars().count() + 3);
    Line::from(vec![
        Span::styled(
            "Task",
            Style::default()
                .fg(super::theme::accent_gold())
                .bg(status_band_bg())
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(" "),
        Span::styled(
            title,
            Style::default()
                .fg(ink())
                .bg(status_band_bg())
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            "  ·  ",
            Style::default()
                .fg(super::theme::dim())
                .bg(status_band_bg()),
        ),
        Span::styled(
            truncate_display_width(&task_strip.detail, detail_width),
            Style::default()
                .fg(super::theme::muted())
                .bg(status_band_bg()),
        ),
    ])
}

fn render_task_strip_row(row: &TaskStripRowViewModel, width: usize) -> Line<'static> {
    let status = StatusIndicator::animated(row.kind);
    let row_bg = if row.active {
        super::theme::composer_input_bg()
    } else {
        status_band_bg()
    };
    let label_width = width.saturating_sub(LIVE_TASK_ROW_RESERVED_WIDTH);
    let label = truncate_display_width(&row.label, label_width);
    let label_style = if row.active {
        Style::default()
            .fg(ink())
            .bg(row_bg)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(ink()).bg(row_bg)
    };
    Line::from(vec![
        Span::styled("  ", Style::default().bg(row_bg)),
        Span::styled(status.symbol(), status.style().bg(row_bg)),
        Span::styled(" ", Style::default().bg(row_bg)),
        Span::styled(label, label_style),
    ])
}

const LIVE_TASK_ROW_RESERVED_WIDTH: usize = 2 + 1 + 1;

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
    let indicator = StatusIndicator::animated(StatusKind::Running);
    vec![
        Line::from(vec![
            indicator.span(),
            Span::raw(" "),
            Span::styled(
                format!("{}...", progress.title),
                Style::default()
                    .fg(accent)
                    .bg(status_band_bg())
                    .add_modifier(Modifier::BOLD),
            ),
        ]),
        Line::from(vec![
            Span::styled("  ↳ ", Style::default().fg(accent).bg(status_band_bg())),
            Span::styled(
                progress.detail.clone(),
                Style::default().fg(ink()).bg(status_band_bg()),
            ),
        ]),
    ]
}

#[cfg(all(test, not(sigil_tui_test_slice_app_input_flow)))]
#[path = "tests/live_panel_tests.rs"]
mod tests;

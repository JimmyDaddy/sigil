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
    timeline::ComposerQueueRow,
    view_model::{
        LivePanelViewModel, LiveProgressViewModel, PlanApprovalViewModel,
        QueueActionButtonViewModel, TaskStripRowViewModel, TaskStripViewModel,
    },
};

use super::{
    geometry::inset_rect,
    status_indicator::{StatusIndicator, StatusKind},
    text::{pad_display_width, truncate_display_width},
    theme::Theme,
};

pub(crate) const LIVE_PANEL_BOTTOM_PADDING: u16 = 1;
pub(crate) const LIVE_PROGRESS_ROWS: u16 = 2;
const LIVE_PLAN_APPROVAL_BASE_ROWS: u16 = 2;
const LIVE_PLAN_APPROVAL_STEP_LIMIT: usize = 3;
const LIVE_QUEUE_ROW_LIMIT: usize = 4;
const LIVE_TASK_ROUTE_LIMIT: usize = 3;
const LIVE_TASK_ROW_LIMIT: usize = 4;

#[cfg(test)]
pub(crate) fn render_live_panel(frame: &mut Frame, area: Rect, view_model: &LivePanelViewModel) {
    let theme = Theme::default();
    render_live_panel_with_theme(frame, area, view_model, &theme);
}

pub(crate) fn render_live_panel_with_theme(
    frame: &mut Frame,
    area: Rect,
    view_model: &LivePanelViewModel,
    theme: &Theme,
) {
    let palette = &theme.palette;
    frame.render_widget(
        Block::default().style(Style::default().bg(palette.surface_base)),
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
            .style(Style::default().bg(palette.surface_base))
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
        render_live_status_band(frame, status_area, view_model, theme);
    }
}

pub(crate) fn live_status_rows_for_app(app: &AppState) -> u16 {
    let progress_rows = if app.live_activity_summary().is_some() {
        LIVE_PROGRESS_ROWS
    } else {
        0
    };
    let plan_rows = app
        .pending_plan_approval()
        .map(live_plan_approval_rows)
        .unwrap_or(0);
    let task_rows = app
        .task_strip_view()
        .map(|view| {
            live_task_strip_rows(
                view.rows.len(),
                app.runtime.task_provider_route_diagnostics.routes.len(),
                verification_card_rows(view.verification.as_ref(), app.verification_inspect_open()),
            )
        })
        .unwrap_or(0);
    live_status_rows_with_separator(
        app.queue_strip_rows()
            .saturating_add(progress_rows)
            .saturating_add(plan_rows)
            .saturating_add(task_rows),
    )
}

pub(crate) fn live_status_rows(view_model: &LivePanelViewModel) -> u16 {
    let queue_rows = live_queue_strip_rows(view_model.queue_rows.len());
    let progress_rows = if view_model.progress.is_some() {
        LIVE_PROGRESS_ROWS
    } else {
        0
    };
    let plan_rows = view_model
        .plan_approval
        .as_ref()
        .map(live_plan_approval_view_rows)
        .unwrap_or(0);
    let task_rows = view_model
        .task_strip
        .as_ref()
        .map(|view| {
            live_task_strip_rows(
                view.rows.len(),
                view.route_diagnostics.len(),
                verification_card_view_rows(view.verification.as_ref()),
            )
        })
        .unwrap_or(0);
    live_status_rows_with_separator(
        queue_rows
            .saturating_add(progress_rows)
            .saturating_add(plan_rows)
            .saturating_add(task_rows),
    )
}

pub(crate) fn verification_card_area_for_app(live_area: Rect, app: &AppState) -> Option<Rect> {
    let task_strip = app.task_strip_view()?;
    let verification = task_strip.verification.as_ref()?;
    let verification_rows =
        verification_card_rows(Some(verification), app.verification_inspect_open());
    if verification_rows == 0 {
        return None;
    }
    let inner = inset_rect(live_area, 1, 0);
    let content_frame = Rect::new(
        inner.x,
        inner.y,
        inner.width,
        inner
            .height
            .saturating_sub(LIVE_PANEL_BOTTOM_PADDING)
            .max(1),
    );
    let task_rows = live_task_strip_rows(
        task_strip.rows.len(),
        app.runtime.task_provider_route_diagnostics.routes.len(),
        verification_rows,
    );
    let status_rows = live_status_rows_for_app(app).min(content_frame.height.saturating_sub(1));
    let status_top = content_frame
        .y
        .saturating_add(content_frame.height.saturating_sub(status_rows));
    let card_top = content_frame
        .y
        .saturating_add(content_frame.height.saturating_sub(task_rows))
        .saturating_add(1)
        .max(status_top.saturating_add(1));
    let available = content_frame.bottom().saturating_sub(card_top);
    Some(Rect::new(
        inner.x,
        card_top,
        inner.width,
        verification_rows.min(available),
    ))
}

fn live_status_rows_with_separator(content_rows: u16) -> u16 {
    if content_rows == 0 {
        return 0;
    }
    content_rows.saturating_add(1)
}

fn live_task_strip_rows(row_count: usize, route_count: usize, verification_rows: u16) -> u16 {
    if row_count == 0 {
        return 0;
    }
    1 + route_count.min(LIVE_TASK_ROUTE_LIMIT) as u16
        + verification_rows
        + row_count.min(LIVE_TASK_ROW_LIMIT) as u16
}

fn verification_card_rows(
    card: Option<&crate::app::task_sidebar::VerificationCardView>,
    inspect_open: bool,
) -> u16 {
    let Some(card) = card else {
        return 0;
    };
    3 + u16::from(card.why.is_some())
        + if inspect_open {
            card.inspect_lines.len() as u16
        } else {
            0
        }
}

fn verification_card_view_rows(card: Option<&crate::view_model::VerificationCardViewModel>) -> u16 {
    let Some(card) = card else {
        return 0;
    };
    3 + u16::from(card.why.is_some())
        + if card.inspect_open {
            card.inspect_lines.len() as u16
        } else {
            0
        }
}

fn live_queue_strip_rows(row_count: usize) -> u16 {
    if row_count == 0 {
        return 0;
    }
    2 + row_count.min(LIVE_QUEUE_ROW_LIMIT) as u16
}

fn live_plan_approval_rows(plan: &crate::app::PendingPlanApproval) -> u16 {
    let detail_rows =
        usize::from(!plan.target_paths.is_empty()) + usize::from(!plan.suggested_checks.is_empty());
    let overflow_rows = usize::from(plan.steps.len() > LIVE_PLAN_APPROVAL_STEP_LIMIT);
    LIVE_PLAN_APPROVAL_BASE_ROWS
        + plan.steps.len().min(LIVE_PLAN_APPROVAL_STEP_LIMIT) as u16
        + u16::try_from(overflow_rows).unwrap_or(0)
        + u16::try_from(detail_rows).unwrap_or(0)
}

fn live_plan_approval_view_rows(plan: &PlanApprovalViewModel) -> u16 {
    let detail_rows =
        usize::from(!plan.target_paths.is_empty()) + usize::from(!plan.suggested_checks.is_empty());
    let overflow_rows = usize::from(plan.steps.len() > LIVE_PLAN_APPROVAL_STEP_LIMIT);
    LIVE_PLAN_APPROVAL_BASE_ROWS
        + plan.steps.len().min(LIVE_PLAN_APPROVAL_STEP_LIMIT) as u16
        + u16::try_from(overflow_rows).unwrap_or(0)
        + u16::try_from(detail_rows).unwrap_or(0)
}

fn render_live_status_band(
    frame: &mut Frame,
    area: Rect,
    view_model: &LivePanelViewModel,
    theme: &Theme,
) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let palette = &theme.palette;
    let band_bg = palette.surface_panel_alt;
    frame.render_widget(Block::default().style(Style::default().bg(band_bg)), area);
    let accent = palette.phase_accent(&view_model.phase);
    render_status_separator(frame, area, band_bg, palette.border_subtle);
    render_status_left_rail(frame, area, accent, band_bg);
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
    if !view_model.queue_rows.is_empty() {
        lines.extend(render_queue_strip_lines(
            view_model,
            area.width as usize,
            theme,
        ));
    }
    if let Some(progress) = &view_model.progress {
        lines.extend(render_live_progress_lines_with_theme(
            progress, accent, theme,
        ));
    }
    if let Some(plan_approval) = &view_model.plan_approval {
        lines.extend(render_plan_approval_lines(
            plan_approval,
            content_area.width as usize,
            theme,
        ));
    }
    if let Some(task_strip) = &view_model.task_strip {
        lines.extend(render_task_strip_lines(
            task_strip,
            area.width as usize,
            theme,
        ));
    }
    let lines = lines
        .into_iter()
        .take(content_area.height as usize)
        .map(|line| status_band_line(line, content_area.width as usize, band_bg))
        .collect::<Vec<_>>();

    frame.render_widget(
        Paragraph::new(Text::from(lines))
            .style(Style::default().bg(band_bg))
            .wrap(Wrap { trim: false }),
        content_area,
    );
}

fn status_band_line(line: Line<'static>, width: usize, bg: Color) -> Line<'static> {
    let mut spans = line
        .spans
        .into_iter()
        .map(|span| {
            let mut style = span.style;
            if style.bg.is_none() {
                style.bg = Some(bg);
            }
            Span::styled(span.content, style)
        })
        .collect::<Vec<_>>();
    let line_width = line_display_width(&spans);
    if width > line_width {
        spans.push(Span::styled(
            " ".repeat(width - line_width),
            Style::default().bg(bg),
        ));
    }
    Line {
        spans,
        style: line.style.patch(Style::default().bg(bg)),
        alignment: line.alignment,
    }
}

fn line_display_width(spans: &[Span<'static>]) -> usize {
    spans
        .iter()
        .map(|span| UnicodeWidthStr::width(span.content.as_ref()))
        .sum()
}

fn render_status_separator(frame: &mut Frame, area: Rect, bg: Color, edge: Color) {
    if area.width == 0 {
        return;
    }
    let line = Line::from(vec![Span::styled(
        "─".repeat(area.width as usize),
        Style::default().fg(edge).bg(bg),
    )]);
    frame.render_widget(
        Paragraph::new(Text::from(vec![line])).style(Style::default().bg(bg)),
        Rect::new(area.x, area.y, area.width, 1),
    );
}

fn render_status_left_rail(frame: &mut Frame, area: Rect, accent: Color, bg: Color) {
    if area.width == 0 || area.height <= 1 {
        return;
    }
    let lines = (0..area.height.saturating_sub(1))
        .map(|_| Line::from(vec![Span::styled("▌", Style::default().fg(accent).bg(bg))]))
        .collect::<Vec<_>>();
    frame.render_widget(
        Paragraph::new(Text::from(lines)).style(Style::default().bg(bg)),
        Rect::new(
            area.x,
            area.y.saturating_add(1),
            1,
            area.height.saturating_sub(1),
        ),
    );
}

fn render_queue_strip_lines(
    view_model: &LivePanelViewModel,
    width: usize,
    theme: &Theme,
) -> Vec<Line<'static>> {
    if view_model.queue_rows.is_empty() {
        return Vec::new();
    }
    let mut lines = Vec::with_capacity(2 + view_model.queue_rows.len().min(LIVE_QUEUE_ROW_LIMIT));
    lines.push(render_queue_header(view_model, width, theme));
    for row in view_model.queue_rows.iter().take(LIVE_QUEUE_ROW_LIMIT) {
        lines.push(render_queue_row(
            row,
            width,
            view_model.queue_panel_focused,
            theme,
        ));
    }
    lines.push(render_queue_actions(
        &view_model.queue_action_buttons,
        width,
        view_model.queue_panel_focused,
        theme,
    ));
    lines
}

fn render_queue_header(
    view_model: &LivePanelViewModel,
    width: usize,
    theme: &Theme,
) -> Line<'static> {
    let palette = &theme.palette;
    let bg = palette.surface_panel_alt;
    let title = if view_model.queue_paused {
        "Follow-ups paused"
    } else {
        "Follow-ups"
    };
    let detail = if view_model.queue_panel_focused {
        "↑↓ item · ←/→ action · Enter selected · Tab/Esc input"
    } else {
        "Tab focus · /queue advanced"
    };
    Line::from(vec![
        Span::styled(
            title,
            Style::default()
                .fg(if view_model.queue_paused {
                    palette.status_warning
                } else {
                    palette.accent_info
                })
                .bg(bg)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            truncate_display_width(&format!("  {detail}"), width.saturating_sub(title.len())),
            Style::default().fg(palette.text_secondary).bg(bg),
        ),
    ])
}

fn render_queue_row(
    row: &ComposerQueueRow,
    width: usize,
    panel_focused: bool,
    theme: &Theme,
) -> Line<'static> {
    const QUEUE_LABEL_WIDTH: usize = 28;

    let palette = &theme.palette;
    let bg = palette.surface_panel_alt;
    let selected = panel_focused && row.selected;
    let row_bg = if selected { palette.selection_bg } else { bg };
    let fg = if selected {
        palette.selection_fg
    } else {
        palette.text_primary
    };
    let marker = if selected { "▸" } else { " " };
    let status = StatusIndicator::animated(row.status);
    let label = truncate_display_width(&row.label, QUEUE_LABEL_WIDTH);
    let label = format!("{label:<QUEUE_LABEL_WIDTH$}");
    let reserved = 2 + QUEUE_LABEL_WIDTH + 3;
    let detail = truncate_display_width(&row.detail, width.saturating_sub(reserved));
    if selected {
        let content = truncate_display_width(
            &format!("{marker} {label} {} {detail}", status.symbol()),
            width,
        );
        return Line::from(vec![Span::styled(
            pad_display_width(&content, width),
            Style::default().fg(fg).bg(row_bg),
        )]);
    }
    Line::from(vec![
        Span::styled(marker, Style::default().fg(palette.accent_info).bg(bg)),
        Span::styled(" ", Style::default().bg(bg)),
        Span::styled(label, Style::default().fg(fg).bg(row_bg)),
        Span::styled(" ", Style::default().bg(bg)),
        status.span_with_palette(palette),
        Span::styled(" ", Style::default().bg(bg)),
        Span::styled(detail, Style::default().fg(palette.text_secondary).bg(bg)),
    ])
}

fn render_queue_actions(
    buttons: &[QueueActionButtonViewModel],
    width: usize,
    panel_focused: bool,
    theme: &Theme,
) -> Line<'static> {
    let palette = &theme.palette;
    let bg = palette.surface_panel_alt;
    let mut spans = vec![Span::styled(
        "Actions ",
        Style::default()
            .fg(palette.text_muted)
            .bg(bg)
            .add_modifier(Modifier::BOLD),
    )];
    for button in buttons {
        spans.push(Span::styled(" ", Style::default().bg(bg)));
        let style = if panel_focused && button.selected {
            Style::default()
                .fg(palette.selection_fg)
                .bg(palette.selection_bg)
                .add_modifier(Modifier::BOLD)
        } else if button.destructive {
            Style::default().fg(palette.status_error).bg(bg)
        } else {
            Style::default().fg(palette.text_primary).bg(bg)
        };
        spans.push(Span::styled(format!(" {} ", button.label), style));
    }
    let selected_detail = buttons
        .iter()
        .find(|button| button.selected)
        .map(|button| button.detail.as_str())
        .unwrap_or("");
    if !selected_detail.is_empty() {
        spans.push(Span::styled(
            "  ·  ",
            Style::default().fg(palette.text_muted).bg(bg),
        ));
        spans.push(Span::styled(
            truncate_display_width(selected_detail, width.saturating_sub(32)),
            Style::default().fg(palette.text_secondary).bg(bg),
        ));
    }
    Line::from(spans)
}

fn render_plan_approval_lines(
    plan: &PlanApprovalViewModel,
    width: usize,
    theme: &Theme,
) -> Vec<Line<'static>> {
    let palette = &theme.palette;
    let bg = palette.surface_panel_alt;
    let path_label = if plan.target_path_count == 1 {
        "path"
    } else {
        "paths"
    };
    let check_label = if plan.suggested_check_count == 1 {
        "check"
    } else {
        "checks"
    };
    let counts = format!(
        "structured plan · {} {} · {} {}",
        plan.target_path_count, path_label, plan.suggested_check_count, check_label
    );
    let reserved_width =
        UnicodeWidthStr::width("Plan ready") + UnicodeWidthStr::width(counts.as_str()) + 10;
    let summary_width = width.saturating_sub(reserved_width);
    let summary = truncate_display_width(&plan.summary, summary_width);
    let mut lines = vec![Line::from(vec![
        Span::styled(
            "Plan",
            Style::default()
                .fg(palette.accent_warning)
                .bg(bg)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(" "),
        Span::styled(
            "ready",
            Style::default()
                .fg(palette.text_primary)
                .bg(bg)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!("  ·  {counts}  ·  {summary}"),
            Style::default().fg(palette.text_secondary).bg(bg),
        ),
    ])];
    for (index, step) in plan
        .steps
        .iter()
        .take(LIVE_PLAN_APPROVAL_STEP_LIMIT)
        .enumerate()
    {
        let prefix = format!("{}. ", index + 1);
        let available = width.saturating_sub(UnicodeWidthStr::width(prefix.as_str()));
        lines.push(Line::from(vec![
            Span::styled(prefix, Style::default().fg(palette.text_secondary).bg(bg)),
            Span::styled(
                truncate_display_width(step, available),
                Style::default().fg(palette.text_primary).bg(bg),
            ),
        ]));
    }
    if plan.steps.len() > LIVE_PLAN_APPROVAL_STEP_LIMIT {
        lines.push(Line::from(vec![Span::styled(
            format!(
                "... {} more steps",
                plan.steps.len() - LIVE_PLAN_APPROVAL_STEP_LIMIT
            ),
            Style::default().fg(palette.text_muted).bg(bg),
        )]));
    }
    let path_summary = if plan.target_paths.is_empty() {
        None
    } else {
        Some(format!(
            "Paths: {}",
            plan.target_paths
                .iter()
                .take(3)
                .cloned()
                .collect::<Vec<_>>()
                .join(", ")
        ))
    };
    let check_summary = if plan.suggested_checks.is_empty() {
        None
    } else {
        Some(format!(
            "Checks: {}",
            plan.suggested_checks
                .iter()
                .take(2)
                .cloned()
                .collect::<Vec<_>>()
                .join("; ")
        ))
    };
    for detail in [path_summary, check_summary].into_iter().flatten() {
        lines.push(Line::from(vec![Span::styled(
            truncate_display_width(&detail, width),
            Style::default().fg(palette.text_secondary).bg(bg),
        )]));
    }
    lines.push(Line::from(vec![
        Span::styled("Enter", Style::default().fg(palette.text_primary).bg(bg)),
        Span::styled(
            " create and run task",
            Style::default().fg(palette.text_secondary).bg(bg),
        ),
        Span::styled("  Esc", Style::default().fg(palette.text_primary).bg(bg)),
        Span::styled(
            " discard",
            Style::default().fg(palette.text_secondary).bg(bg),
        ),
    ]));
    lines
}

fn render_task_strip_lines(
    task_strip: &TaskStripViewModel,
    width: usize,
    theme: &Theme,
) -> Vec<Line<'static>> {
    if task_strip.rows.is_empty() {
        return Vec::new();
    }
    let mut lines = Vec::with_capacity(
        1 + task_strip.route_diagnostics.len() + task_strip.rows.len().min(LIVE_TASK_ROW_LIMIT),
    );
    lines.push(render_task_strip_header(task_strip, width, theme));
    lines.extend(
        task_strip
            .route_diagnostics
            .iter()
            .take(LIVE_TASK_ROUTE_LIMIT)
            .map(|diagnostic| {
                Line::from(vec![
                    Span::styled(
                        "  Route ",
                        Style::default()
                            .fg(theme.palette.accent_info)
                            .bg(theme.palette.surface_panel_alt)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(
                        truncate_display_width(diagnostic, width.saturating_sub(8)),
                        Style::default()
                            .fg(theme.palette.text_secondary)
                            .bg(theme.palette.surface_panel_alt),
                    ),
                ])
            }),
    );
    if let Some(verification) = &task_strip.verification {
        lines.extend(render_verification_card_lines(verification, width, theme));
    }
    lines.extend(
        task_strip
            .rows
            .iter()
            .take(LIVE_TASK_ROW_LIMIT)
            .map(|row| render_task_strip_row(row, width, theme)),
    );
    lines
}

fn render_verification_card_lines(
    card: &crate::view_model::VerificationCardViewModel,
    width: usize,
    theme: &Theme,
) -> Vec<Line<'static>> {
    let palette = &theme.palette;
    let bg = if card.focused {
        palette.surface_input
    } else {
        palette.surface_panel_alt
    };
    let mut lines = vec![Line::from(vec![
        Span::styled(
            "Verification",
            Style::default()
                .fg(palette.accent_warning)
                .bg(bg)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled("  ·  ", Style::default().fg(palette.text_muted).bg(bg)),
        Span::styled(
            truncate_display_width(&card.status, width.saturating_sub(16)),
            Style::default().fg(palette.text_primary).bg(bg),
        ),
    ])];
    lines.push(Line::from(Span::styled(
        truncate_display_width(
            &format!(
                "  Recommended  {}",
                card.recommended.as_deref().unwrap_or("none")
            ),
            width,
        ),
        Style::default().fg(palette.text_primary).bg(bg),
    )));
    if let Some(why) = &card.why {
        lines.push(Line::from(Span::styled(
            truncate_display_width(&format!("  Why         {why}"), width),
            Style::default().fg(palette.text_secondary).bg(bg),
        )));
    }
    if let Some(action) = card.action_label {
        lines.push(Line::from(Span::styled(
            truncate_display_width(&format!("  Enter {action}  ·  I inspect"), width),
            Style::default()
                .fg(if card.focused {
                    palette.accent_primary
                } else {
                    palette.text_muted
                })
                .bg(bg),
        )));
    } else {
        lines.push(Line::from(Span::styled(
            "  I inspect",
            Style::default()
                .fg(if card.focused {
                    palette.accent_primary
                } else {
                    palette.text_muted
                })
                .bg(bg),
        )));
    }
    if card.inspect_open {
        lines.extend(card.inspect_lines.iter().map(|line| {
            Line::from(Span::styled(
                truncate_display_width(&format!("  {line}"), width),
                Style::default().fg(palette.text_secondary).bg(bg),
            ))
        }));
    }
    lines
}

fn render_task_strip_header(
    task_strip: &TaskStripViewModel,
    width: usize,
    theme: &Theme,
) -> Line<'static> {
    let palette = &theme.palette;
    let bg = palette.surface_panel_alt;
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
                .fg(palette.accent_warning)
                .bg(bg)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(" "),
        Span::styled(
            title,
            Style::default()
                .fg(palette.text_primary)
                .bg(bg)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled("  ·  ", Style::default().fg(palette.text_muted).bg(bg)),
        Span::styled(
            truncate_display_width(&task_strip.detail, detail_width),
            Style::default().fg(palette.text_secondary).bg(bg),
        ),
    ])
}

fn render_task_strip_row(
    row: &TaskStripRowViewModel,
    width: usize,
    theme: &Theme,
) -> Line<'static> {
    let palette = &theme.palette;
    let status = StatusIndicator::animated(row.kind);
    let row_bg = if row.active {
        palette.surface_input
    } else {
        palette.surface_panel_alt
    };
    let label_width = width.saturating_sub(LIVE_TASK_ROW_RESERVED_WIDTH);
    let label = truncate_display_width(&row.label, label_width);
    let label_style = if row.active {
        Style::default()
            .fg(palette.text_primary)
            .bg(row_bg)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(palette.text_primary).bg(row_bg)
    };
    Line::from(vec![
        Span::styled("  ", Style::default().bg(row_bg)),
        Span::styled(
            status.symbol(),
            status.style_with_palette(palette).bg(row_bg),
        ),
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

#[cfg(test)]
pub(crate) fn render_live_progress_lines(
    progress: &LiveProgressViewModel,
    accent: Color,
) -> Vec<Line<'static>> {
    let theme = Theme::default();
    render_live_progress_lines_with_theme(progress, accent, &theme)
}

fn render_live_progress_lines_with_theme(
    progress: &LiveProgressViewModel,
    accent: Color,
    theme: &Theme,
) -> Vec<Line<'static>> {
    let palette = &theme.palette;
    let bg = palette.surface_panel_alt;
    let indicator = StatusIndicator::animated(StatusKind::Running);
    vec![
        Line::from(vec![
            indicator.span_with_palette(palette),
            Span::raw(" "),
            Span::styled(
                format!("{}...", progress.title),
                Style::default()
                    .fg(accent)
                    .bg(bg)
                    .add_modifier(Modifier::BOLD),
            ),
        ]),
        Line::from(vec![
            Span::styled("  ↳ ", Style::default().fg(accent).bg(bg)),
            Span::styled(
                progress.detail.clone(),
                Style::default().fg(palette.text_primary).bg(bg),
            ),
        ]),
    ]
}

#[cfg(all(test, not(sigil_tui_test_slice_app_input_flow)))]
#[path = "tests/live_panel_tests.rs"]
mod tests;

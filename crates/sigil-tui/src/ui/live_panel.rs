use crate::surface::VirtualTextWindow;
use crate::{
    app::{AppState, task_sidebar::TASK_STRIP_COLLAPSED_ROW_LIMIT},
    timeline::ComposerQueueRow,
    view_model::{
        LivePanelViewModel, LiveProgressViewModel, PlanApprovalViewModel,
        QueueActionButtonViewModel, TaskStripRowViewModel, TaskStripViewModel,
    },
};
use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, Paragraph, Wrap},
};
use unicode_segmentation::UnicodeSegmentation;

use super::{
    geometry::inset_rect,
    status_indicator::{StatusIndicator, StatusKind},
    text::{
        pad_display_width, sanitize_terminal_text, terminal_cell_width, terminal_grapheme_width,
        wrap_terminal_lines,
    },
    theme::Theme,
};

pub(crate) const LIVE_PANEL_BOTTOM_PADDING: u16 = 1;
pub(crate) const LIVE_PROGRESS_ROWS: u16 = 2;
const LIVE_PLAN_APPROVAL_BASE_ROWS: u16 = 2;
const LIVE_PLAN_APPROVAL_ROW_LIMIT: usize = 7;
const LIVE_PLAN_APPROVAL_STEP_LIMIT: usize = 3;
const LIVE_QUEUE_ROW_LIMIT: usize = 4;
const LIVE_TASK_ROUTE_LIMIT: usize = 3;
const LIVE_TASK_COMPLETION_LIMIT: usize = 5;
const LIVE_STATUS_CONTENT_INSET: usize = 2;

#[cfg(test)]
pub(crate) fn render_live_panel(frame: &mut Frame, area: Rect, view_model: &LivePanelViewModel) {
    let theme = Theme::default();
    render_live_panel_with_theme(frame, area, view_model, &theme);
}

#[cfg(test)]
pub(crate) fn render_live_panel_with_theme(
    frame: &mut Frame,
    area: Rect,
    view_model: &LivePanelViewModel,
    theme: &Theme,
) {
    render_live_panel_with_lines(
        frame,
        area,
        view_model,
        view_model.transcript_lines.clone(),
        theme,
    );
}

pub(crate) fn render_live_panel_surface(
    frame: &mut Frame,
    area: Rect,
    view_model: &LivePanelViewModel,
    transcript: &VirtualTextWindow,
    theme: &Theme,
) {
    debug_assert!(transcript.is_bounded_by(transcript.resident_len()));
    render_live_panel_with_lines(frame, area, view_model, transcript.items.clone(), theme);
}

fn render_live_panel_with_lines(
    frame: &mut Frame,
    area: Rect,
    view_model: &LivePanelViewModel,
    transcript_lines: Vec<Line<'static>>,
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

    let status_height = live_status_rows(view_model, content_frame.width as usize)
        .min(content_frame.height.saturating_sub(1));
    let transcript_frame = Rect::new(
        content_frame.x,
        content_frame.y,
        content_frame.width,
        content_frame.height.saturating_sub(status_height).max(1),
    );
    let visual_lines = wrap_live_panel_lines(transcript_lines, transcript_frame.width as usize);
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

pub(crate) fn live_status_rows_for_app(app: &AppState, width: usize) -> u16 {
    live_status_rows_with_separator(live_status_row_counts_for_app(app, width).total())
}

fn live_status_row_counts_for_app(app: &AppState, width: usize) -> LiveStatusRowAllocation {
    let content_width = live_status_content_width(width);
    let progress = if app.live_activity_summary().is_some() {
        usize::from(LIVE_PROGRESS_ROWS)
    } else {
        0
    };
    let plan = app
        .pending_plan_approval()
        .map(|plan| usize::from(live_plan_approval_rows(plan, content_width)))
        .unwrap_or(0);
    let task_strip = app.task_strip_view();
    let verification = task_strip
        .as_ref()
        .map(|view| {
            usize::from(verification_card_rows(
                view.verification.as_ref(),
                app.verification_inspect_open(),
            ))
        })
        .unwrap_or(0);
    let task = task_strip
        .as_ref()
        .map(|view| {
            usize::from(live_task_strip_rows(
                view.rows.len(),
                app.runtime.task_provider_route_diagnostics.routes.len(),
                crate::app::task_sidebar::task_completion_progress_live_lines(
                    &app.runtime.task_completion_progress,
                )
                .len(),
                app.task_strip_expanded(&view.task_id),
            ))
        })
        .unwrap_or(0);
    LiveStatusRowAllocation {
        queue: usize::from(app.queue_strip_rows()),
        progress,
        plan,
        verification,
        task,
    }
}

pub(crate) fn live_status_row_allocation_for_app(
    app: &AppState,
    width: usize,
    capacity: usize,
) -> LiveStatusRowAllocation {
    let preferred_action = if app.pending_plan_approval().is_some() {
        Some(StatusBandSectionKind::Plan)
    } else if app.is_composer_queue_panel_focused() {
        Some(StatusBandSectionKind::Queue)
    } else if app.verification_card_focused() {
        Some(StatusBandSectionKind::Verification)
    } else {
        None
    };
    allocate_status_band_rows(
        live_status_row_counts_for_app(app, width),
        capacity,
        preferred_action,
    )
}

pub(crate) fn live_status_rows(view_model: &LivePanelViewModel, width: usize) -> u16 {
    let content_width = live_status_content_width(width);
    let queue_rows = live_queue_strip_rows(view_model.queue_rows.len());
    let progress_rows = if view_model.progress.is_some() {
        LIVE_PROGRESS_ROWS
    } else {
        0
    };
    let plan_rows = view_model
        .plan_approval
        .as_ref()
        .map(|plan| live_plan_approval_view_rows(plan, content_width))
        .unwrap_or(0);
    let verification_rows = view_model
        .task_strip
        .as_ref()
        .map(|view| verification_card_view_rows(view.verification.as_ref()))
        .unwrap_or(0);
    let task_rows = view_model
        .task_strip
        .as_ref()
        .map(|view| {
            live_task_strip_rows(
                view.rows.len(),
                view.route_diagnostics.len(),
                view.completion_progress.len(),
                view.expanded,
            )
        })
        .unwrap_or(0);
    live_status_rows_with_separator(
        usize::from(queue_rows)
            .saturating_add(usize::from(progress_rows))
            .saturating_add(usize::from(plan_rows))
            .saturating_add(usize::from(verification_rows))
            .saturating_add(usize::from(task_rows)),
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
    let status_rows = live_status_rows_for_app(app, content_frame.width as usize)
        .min(content_frame.height.saturating_sub(1));
    let status_content_rows = status_rows.saturating_sub(1) as usize;
    let allocation =
        live_status_row_allocation_for_app(app, content_frame.width as usize, status_content_rows);
    let visible_card_rows = allocation
        .verification_rows()
        .min(usize::from(verification_rows));
    if visible_card_rows == 0 {
        return None;
    }
    let status_top = content_frame
        .y
        .saturating_add(content_frame.height.saturating_sub(status_rows));
    let content_top = status_top.saturating_add(1);
    let card_top = content_top
        .saturating_add(u16::try_from(allocation.rows_before_verification()).unwrap_or(u16::MAX));
    Some(Rect::new(
        content_frame.x.saturating_add(2),
        card_top,
        content_frame.width.saturating_sub(2),
        u16::try_from(visible_card_rows).unwrap_or(u16::MAX),
    ))
}

pub(crate) fn task_strip_toggle_area_for_app(live_area: Rect, app: &AppState) -> Option<Rect> {
    let task_strip = app.task_strip_view()?;
    if task_strip.rows.len() <= TASK_STRIP_COLLAPSED_ROW_LIMIT {
        return None;
    }
    let expanded = app.task_strip_expanded(&task_strip.task_id);
    let task_rows = live_task_strip_rows(
        task_strip.rows.len(),
        app.runtime.task_provider_route_diagnostics.routes.len(),
        crate::app::task_sidebar::task_completion_progress_live_lines(
            &app.runtime.task_completion_progress,
        )
        .len(),
        expanded,
    );
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
    let status_rows = live_status_rows_for_app(app, content_frame.width as usize)
        .min(content_frame.height.saturating_sub(1));
    let status_content_rows = status_rows.saturating_sub(1) as usize;
    let allocation =
        live_status_row_allocation_for_app(app, content_frame.width as usize, status_content_rows);
    if allocation.task_rows() < usize::from(task_rows) {
        return None;
    }
    let status_top = content_frame
        .y
        .saturating_add(content_frame.height.saturating_sub(status_rows));
    let toggle_top = status_top
        .saturating_add(1)
        .saturating_add(u16::try_from(allocation.rows_before_task()).unwrap_or(u16::MAX))
        .saturating_add(task_rows.saturating_sub(1));
    Some(Rect::new(
        content_frame.x.saturating_add(2),
        toggle_top,
        content_frame.width.saturating_sub(2),
        1,
    ))
}

fn live_status_rows_with_separator(content_rows: usize) -> u16 {
    if content_rows == 0 {
        return 0;
    }
    u16::try_from(content_rows.saturating_add(1)).unwrap_or(u16::MAX)
}

fn live_status_content_width(width: usize) -> usize {
    width.saturating_sub(LIVE_STATUS_CONTENT_INSET).max(1)
}

fn live_task_strip_rows(
    row_count: usize,
    route_count: usize,
    completion_count: usize,
    expanded: bool,
) -> u16 {
    if row_count == 0 {
        return 0;
    }
    let visible_rows = if expanded {
        row_count
    } else {
        row_count.min(TASK_STRIP_COLLAPSED_ROW_LIMIT)
    };
    let toggle_rows = usize::from(row_count > TASK_STRIP_COLLAPSED_ROW_LIMIT);
    1u16.saturating_add(route_count.min(LIVE_TASK_ROUTE_LIMIT) as u16)
        .saturating_add(completion_count.min(LIVE_TASK_COMPLETION_LIMIT) as u16)
        .saturating_add(visible_rows as u16)
        .saturating_add(toggle_rows as u16)
}

fn verification_card_rows(
    card: Option<&crate::app::task_sidebar::VerificationCardView>,
    inspect_open: bool,
) -> u16 {
    let Some(card) = card else {
        return 0;
    };
    3u16.saturating_add(u16::from(card.why.is_some()))
        .saturating_add(if inspect_open {
            u16::try_from(card.inspect_lines.len()).unwrap_or(u16::MAX)
        } else {
            0
        })
}

fn verification_card_view_rows(card: Option<&crate::view_model::VerificationCardViewModel>) -> u16 {
    let Some(card) = card else {
        return 0;
    };
    3u16.saturating_add(u16::from(card.why.is_some()))
        .saturating_add(if card.inspect_open {
            u16::try_from(card.inspect_lines.len()).unwrap_or(u16::MAX)
        } else {
            0
        })
}

fn live_queue_strip_rows(row_count: usize) -> u16 {
    if row_count == 0 {
        return 0;
    }
    2 + row_count.min(LIVE_QUEUE_ROW_LIMIT) as u16
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct QueueItemWindow {
    pub(crate) show_header: bool,
    pub(crate) item_indices: Vec<usize>,
}

pub(crate) fn queue_item_window(
    item_count: usize,
    selected_index: usize,
    row_budget: usize,
) -> QueueItemWindow {
    if item_count == 0 || row_budget <= 1 {
        return QueueItemWindow {
            show_header: false,
            item_indices: Vec::new(),
        };
    }

    let show_header = row_budget >= 3;
    let item_capacity = row_budget
        .saturating_sub(1 + usize::from(show_header))
        .min(item_count);
    let selected_index = selected_index.min(item_count - 1);
    let start = selected_index
        .saturating_sub(item_capacity / 2)
        .min(item_count.saturating_sub(item_capacity));
    QueueItemWindow {
        show_header,
        item_indices: (start..start.saturating_add(item_capacity)).collect(),
    }
}

fn live_plan_approval_rows(plan: &crate::app::PendingPlanApproval, _width: usize) -> u16 {
    let overflow_rows = usize::from(plan.steps.len() > LIVE_PLAN_APPROVAL_STEP_LIMIT);
    let stale_rows = usize::from(plan.stale);
    u16::try_from(
        LIVE_PLAN_APPROVAL_BASE_ROWS as usize
            + plan.steps.len().min(LIVE_PLAN_APPROVAL_STEP_LIMIT)
            + overflow_rows
            + stale_rows,
    )
    .unwrap_or(u16::MAX)
    .min(LIVE_PLAN_APPROVAL_ROW_LIMIT as u16)
}

fn live_plan_approval_view_rows(plan: &PlanApprovalViewModel, _width: usize) -> u16 {
    let overflow_rows = usize::from(plan.steps.len() > LIVE_PLAN_APPROVAL_STEP_LIMIT);
    let stale_rows = usize::from(plan.stale);
    u16::try_from(
        LIVE_PLAN_APPROVAL_BASE_ROWS as usize
            + plan.steps.len().min(LIVE_PLAN_APPROVAL_STEP_LIMIT)
            + overflow_rows
            + stale_rows,
    )
    .unwrap_or(u16::MAX)
    .min(LIVE_PLAN_APPROVAL_ROW_LIMIT as u16)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StatusBandSectionKind {
    Queue,
    Progress,
    Plan,
    Task,
    Verification,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct LiveStatusRowAllocation {
    queue: usize,
    progress: usize,
    plan: usize,
    task: usize,
    verification: usize,
}

impl LiveStatusRowAllocation {
    fn get(self, kind: StatusBandSectionKind) -> usize {
        match kind {
            StatusBandSectionKind::Queue => self.queue,
            StatusBandSectionKind::Progress => self.progress,
            StatusBandSectionKind::Plan => self.plan,
            StatusBandSectionKind::Task => self.task,
            StatusBandSectionKind::Verification => self.verification,
        }
    }

    fn set(&mut self, kind: StatusBandSectionKind, rows: usize) {
        match kind {
            StatusBandSectionKind::Queue => self.queue = rows,
            StatusBandSectionKind::Progress => self.progress = rows,
            StatusBandSectionKind::Plan => self.plan = rows,
            StatusBandSectionKind::Task => self.task = rows,
            StatusBandSectionKind::Verification => self.verification = rows,
        }
    }

    fn total(self) -> usize {
        self.queue
            .saturating_add(self.progress)
            .saturating_add(self.plan)
            .saturating_add(self.verification)
            .saturating_add(self.task)
    }

    pub(crate) fn queue_rows(self) -> usize {
        self.queue
    }

    pub(crate) fn rows_before_task(self) -> usize {
        self.queue
            .saturating_add(self.progress)
            .saturating_add(self.plan)
    }

    pub(crate) fn task_rows(self) -> usize {
        self.task
    }

    pub(crate) fn verification_rows(self) -> usize {
        self.verification
    }

    pub(crate) fn rows_before_verification(self) -> usize {
        self.rows_before_task().saturating_add(self.task)
    }
}

fn minimum_status_rows(_kind: StatusBandSectionKind, row_count: usize) -> usize {
    usize::from(row_count > 0)
}

fn allocate_status_band_rows(
    row_counts: LiveStatusRowAllocation,
    capacity: usize,
    preferred_action: Option<StatusBandSectionKind>,
) -> LiveStatusRowAllocation {
    let priority = status_band_priority(preferred_action);
    let mut allocation = LiveStatusRowAllocation::default();
    let mut remaining = capacity;
    for kind in priority.iter().copied() {
        let reserved = minimum_status_rows(kind, row_counts.get(kind)).min(remaining);
        allocation.set(kind, reserved);
        remaining -= reserved;
    }
    for kind in [
        StatusBandSectionKind::Plan,
        StatusBandSectionKind::Queue,
        StatusBandSectionKind::Verification,
    ] {
        let additional = row_counts
            .get(kind)
            .min(2)
            .saturating_sub(allocation.get(kind))
            .min(remaining);
        allocation.set(kind, allocation.get(kind).saturating_add(additional));
        remaining -= additional;
    }
    for kind in priority {
        let additional = row_counts
            .get(kind)
            .saturating_sub(allocation.get(kind))
            .min(remaining);
        allocation.set(kind, allocation.get(kind).saturating_add(additional));
        remaining -= additional;
    }
    allocation
}

fn status_band_priority(
    preferred_action: Option<StatusBandSectionKind>,
) -> Vec<StatusBandSectionKind> {
    let mut priority = vec![StatusBandSectionKind::Plan];
    if let Some(preferred) = preferred_action
        && preferred != StatusBandSectionKind::Plan
    {
        priority.push(preferred);
    }
    for kind in [
        StatusBandSectionKind::Queue,
        StatusBandSectionKind::Verification,
        StatusBandSectionKind::Progress,
        StatusBandSectionKind::Task,
    ] {
        if !priority.contains(&kind) {
            priority.push(kind);
        }
    }
    priority
}

struct StatusBandSection {
    kind: StatusBandSectionKind,
    lines: Vec<Line<'static>>,
    selected_queue_item: Option<usize>,
    compact_action_line: Option<Line<'static>>,
}

impl StatusBandSection {
    fn into_fitted_lines(mut self, budget: usize, theme: &Theme) -> Vec<Line<'static>> {
        if budget >= self.lines.len() {
            return self.lines;
        }
        if budget == 0 {
            return Vec::new();
        }

        match self.kind {
            StatusBandSectionKind::Queue => {
                let Some(action) = self.lines.pop() else {
                    return Vec::new();
                };
                if budget == 1 {
                    return vec![self.compact_action_line.take().unwrap_or(action)];
                }
                if self.lines.is_empty() {
                    return vec![action];
                }
                let header = self.lines.remove(0);
                let item_lines = self.lines;
                let window = queue_item_window(
                    item_lines.len(),
                    self.selected_queue_item.unwrap_or(0),
                    budget,
                );
                let mut fitted = Vec::with_capacity(budget);
                if window.show_header {
                    fitted.push(header);
                }
                fitted.extend(
                    window
                        .item_indices
                        .into_iter()
                        .filter_map(|index| item_lines.get(index).cloned()),
                );
                fitted.push(action);
                fitted
            }
            StatusBandSectionKind::Plan | StatusBandSectionKind::Verification => {
                let Some(action) = self.lines.pop() else {
                    return Vec::new();
                };
                let action = if self.kind == StatusBandSectionKind::Verification {
                    self.compact_action_line.take().unwrap_or(action)
                } else {
                    action
                };
                if budget == 1 {
                    return if self.kind == StatusBandSectionKind::Plan {
                        vec![render_hidden_plan_action_line(action, theme)]
                    } else {
                        vec![action]
                    };
                }
                if self.kind == StatusBandSectionKind::Plan {
                    self.lines.truncate(budget.saturating_sub(2));
                    self.lines.push(render_plan_overflow_line(theme));
                } else {
                    self.lines.truncate(budget.saturating_sub(1));
                }
                self.lines.push(action);
                self.lines
            }
            StatusBandSectionKind::Progress | StatusBandSectionKind::Task => {
                self.lines.truncate(budget);
                self.lines
            }
        }
    }
}

fn fit_status_band_sections(
    sections: Vec<StatusBandSection>,
    capacity: usize,
    preferred_action: Option<StatusBandSectionKind>,
    theme: &Theme,
) -> Vec<Line<'static>> {
    let mut row_counts = LiveStatusRowAllocation::default();
    for section in &sections {
        row_counts.set(section.kind, section.lines.len());
    }
    // Decision rows are reserved before optional detail rows so a short viewport never clips the
    // plan or queue actions merely because an earlier surface has verbose content.
    let allocation = allocate_status_band_rows(row_counts, capacity, preferred_action);
    sections
        .into_iter()
        .flat_map(|section| {
            let budget = allocation.get(section.kind);
            section.into_fitted_lines(budget, theme)
        })
        .collect()
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

    let content_width = content_area.width as usize;
    let mut sections = Vec::new();
    if !view_model.queue_rows.is_empty() {
        let selected_queue_item = selected_queue_item_index(view_model);
        let selected_item_preview = selected_queue_item_preview(view_model);
        sections.push(StatusBandSection {
            kind: StatusBandSectionKind::Queue,
            lines: render_queue_strip_lines(view_model, content_width, theme),
            selected_queue_item,
            compact_action_line: Some(render_queue_actions(
                &view_model.queue_action_buttons,
                selected_item_preview.as_deref(),
                selected_queue_item,
                content_width,
                view_model.queue_panel_focused,
                1,
                theme,
            )),
        });
    }
    if let Some(progress) = &view_model.progress {
        sections.push(StatusBandSection {
            kind: StatusBandSectionKind::Progress,
            lines: render_live_progress_lines_with_theme(progress, accent, content_width, theme),
            selected_queue_item: None,
            compact_action_line: None,
        });
    }
    if let Some(plan_approval) = &view_model.plan_approval {
        sections.push(StatusBandSection {
            kind: StatusBandSectionKind::Plan,
            lines: render_plan_approval_lines(plan_approval, content_width, theme),
            selected_queue_item: None,
            compact_action_line: None,
        });
    }
    if let Some(task_strip) = &view_model.task_strip {
        let lines = render_task_strip_lines(task_strip, content_width, theme);
        if !lines.is_empty() {
            sections.push(StatusBandSection {
                kind: StatusBandSectionKind::Task,
                lines,
                selected_queue_item: None,
                compact_action_line: None,
            });
        }
        if let Some(verification) = &task_strip.verification {
            sections.push(StatusBandSection {
                kind: StatusBandSectionKind::Verification,
                lines: render_verification_card_lines(verification, content_width, theme),
                selected_queue_item: None,
                compact_action_line: Some(render_compact_verification_action(
                    verification,
                    content_width,
                    theme,
                )),
            });
        }
    }
    let preferred_action = if view_model.plan_approval.is_some() {
        Some(StatusBandSectionKind::Plan)
    } else if view_model.queue_panel_focused {
        Some(StatusBandSectionKind::Queue)
    } else if view_model
        .task_strip
        .as_ref()
        .and_then(|task| task.verification.as_ref())
        .is_some_and(|verification| verification.focused)
    {
        Some(StatusBandSectionKind::Verification)
    } else {
        None
    };
    let lines = fit_status_band_sections(
        sections,
        content_area.height as usize,
        preferred_action,
        theme,
    )
    .into_iter()
    .map(|line| status_band_line(line, content_width, band_bg))
    .collect::<Vec<_>>();

    frame.render_widget(
        Paragraph::new(Text::from(lines)).style(Style::default().bg(band_bg)),
        content_area,
    );
}

fn status_band_line(line: Line<'static>, width: usize, bg: Color) -> Line<'static> {
    let mut remaining = width;
    let mut spans = Vec::new();
    for span in line.spans {
        if remaining == 0 {
            break;
        }
        let mut style = span.style;
        if style.bg.is_none() {
            style.bg = Some(bg);
        }
        let content = truncate_status_text(span.content.as_ref(), remaining);
        let content_width = terminal_cell_width(&content);
        if !content.is_empty() {
            spans.push(Span::styled(content, style));
        }
        remaining = remaining.saturating_sub(content_width);
    }
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

fn truncate_status_text(text: &str, max_width: usize) -> String {
    if max_width == 0 {
        return String::new();
    }
    let sanitized = sanitize_terminal_text(text);
    if terminal_cell_width(&sanitized) <= max_width {
        return sanitized;
    }

    let ellipsis = "...";
    let ellipsis_width = terminal_cell_width(ellipsis);
    if max_width <= ellipsis_width {
        return ".".repeat(max_width);
    }
    let budget = max_width - ellipsis_width;
    let mut out = String::new();
    let mut used_width = 0usize;
    for grapheme in sanitized.graphemes(true) {
        let grapheme_width = terminal_grapheme_width(grapheme).unwrap_or(0);
        if used_width + grapheme_width > budget {
            break;
        }
        out.push_str(grapheme);
        used_width += grapheme_width;
    }
    out.push_str(ellipsis);
    out
}

fn line_display_width(spans: &[Span<'static>]) -> usize {
    spans
        .iter()
        .map(|span| terminal_cell_width(span.content.as_ref()))
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
    let selected_item_preview = selected_queue_item_preview(view_model);
    lines.push(render_queue_actions(
        &view_model.queue_action_buttons,
        selected_item_preview.as_deref(),
        selected_queue_item_index(view_model),
        width,
        view_model.queue_panel_focused,
        usize::MAX,
        theme,
    ));
    lines
}

fn selected_queue_item_index(view_model: &LivePanelViewModel) -> Option<usize> {
    view_model
        .queue_rows
        .iter()
        .take(LIVE_QUEUE_ROW_LIMIT)
        .position(|row| row.selected)
}

fn selected_queue_item_preview(view_model: &LivePanelViewModel) -> Option<String> {
    view_model
        .queue_rows
        .iter()
        .take(LIVE_QUEUE_ROW_LIMIT)
        .enumerate()
        .find(|(_, row)| row.selected)
        .map(|(index, row)| format!("#{} {}", index + 1, row.label))
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
    let title_width = terminal_cell_width(title);
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
            truncate_status_text(&format!("  {detail}"), width.saturating_sub(title_width)),
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
    const QUEUE_ROW_CHROME_WIDTH: usize = 5;

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
    let label_width = QUEUE_LABEL_WIDTH.min(width.saturating_sub(QUEUE_ROW_CHROME_WIDTH));
    let label = truncate_status_text(&row.label, label_width);
    let label = pad_display_width(&label, label_width);
    let reserved = label_width.saturating_add(QUEUE_ROW_CHROME_WIDTH);
    let detail = truncate_status_text(&row.detail, width.saturating_sub(reserved));
    if selected {
        let content = truncate_status_text(
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
    selected_item_preview: Option<&str>,
    selected_item_index: Option<usize>,
    width: usize,
    panel_focused: bool,
    queue_row_budget: usize,
    theme: &Theme,
) -> Line<'static> {
    let palette = &theme.palette;
    let bg = palette.surface_panel_alt;
    let primary = Style::default().fg(palette.text_muted).bg(bg);
    let labels = buttons
        .iter()
        .map(|button| button.label.as_str())
        .collect::<Vec<_>>();
    let selected_index = buttons
        .iter()
        .position(|button| button.selected)
        .unwrap_or(0);
    let layout = queue_action_button_layout_for_budget(
        &labels,
        selected_index,
        selected_item_index,
        width,
        queue_row_budget,
    );
    let omit_prefix = layout.first().is_some_and(|placement| placement.x == 0);
    let mut spans = Vec::new();
    let mut cursor = 0;
    if !omit_prefix {
        spans.push(Span::styled(
            "Actions ",
            primary.add_modifier(Modifier::BOLD),
        ));
        cursor = terminal_cell_width("Actions ").min(width);
    }
    for (placement_index, placement) in layout.iter().enumerate() {
        if placement_index == 0 && placement.button_index > 0 && placement.x > cursor {
            let gap = placement.x.saturating_sub(cursor);
            spans.push(Span::styled(
                pad_display_width("…", gap),
                Style::default().fg(palette.text_muted).bg(bg),
            ));
        } else if placement.x > cursor {
            spans.push(Span::styled(
                " ".repeat(placement.x - cursor),
                Style::default().bg(bg),
            ));
        }
        let button = &buttons[placement.button_index];
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
        let display_label = if queue_row_budget == 1 {
            selected_item_index
                .map(|index| format!("#{} {}", index + 1, button.label))
                .unwrap_or_else(|| button.label.clone())
        } else {
            button.label.clone()
        };
        let padded_button = format!(" {display_label} ");
        let button_text = if terminal_cell_width(&padded_button) <= placement.width {
            padded_button
        } else {
            truncate_status_text(&display_label, placement.width)
        };
        spans.push(Span::styled(
            pad_display_width(&button_text, placement.width),
            style,
        ));
        cursor = placement.x.saturating_add(placement.width);
    }
    if layout
        .last()
        .is_some_and(|placement| placement.button_index + 1 < buttons.len())
        && cursor < width
    {
        let suffix = truncate_status_text(" …", width.saturating_sub(cursor));
        cursor = cursor.saturating_add(terminal_cell_width(&suffix));
        spans.push(Span::styled(suffix, primary));
    }
    let selected_detail = selected_item_preview.unwrap_or_else(|| {
        buttons
            .iter()
            .find(|button| button.selected)
            .map(|button| button.detail.as_str())
            .unwrap_or("")
    });
    if !selected_detail.is_empty() && width.saturating_sub(cursor) >= 5 {
        let separator = " · ";
        spans.push(Span::styled(
            separator,
            Style::default().fg(palette.text_muted).bg(bg),
        ));
        cursor = cursor.saturating_add(terminal_cell_width(separator));
        spans.push(Span::styled(
            truncate_status_text(selected_detail, width.saturating_sub(cursor)),
            Style::default().fg(palette.text_secondary).bg(bg),
        ));
    }
    status_band_line(Line::from(spans), width, bg)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct QueueActionButtonPlacement {
    pub(crate) button_index: usize,
    pub(crate) x: usize,
    pub(crate) width: usize,
}

pub(crate) fn queue_action_button_layout(
    labels: &[&str],
    selected_index: usize,
    width: usize,
) -> Vec<QueueActionButtonPlacement> {
    if labels.is_empty() || width == 0 {
        return Vec::new();
    }
    let selected_index = selected_index.min(labels.len() - 1);
    let prefix_width = terminal_cell_width("Actions ").min(width);
    let button_widths = labels
        .iter()
        .map(|label| terminal_cell_width(label).saturating_add(2))
        .collect::<Vec<_>>();
    let mut best: Option<(usize, usize, usize)> = None;
    for start in 0..=selected_index {
        for end in selected_index..labels.len() {
            let hidden_chrome = usize::from(start > 0).saturating_mul(2)
                + usize::from(end + 1 < labels.len()).saturating_mul(2);
            let buttons_width = button_widths[start..=end]
                .iter()
                .fold(0usize, |total, button_width| {
                    total.saturating_add(1).saturating_add(*button_width)
                });
            let cost = prefix_width
                .saturating_add(hidden_chrome)
                .saturating_add(buttons_width);
            if cost > width {
                continue;
            }
            let count = end - start + 1;
            let imbalance = selected_index
                .saturating_sub(start)
                .abs_diff(end.saturating_sub(selected_index));
            if best.is_none_or(|(best_start, best_end, best_imbalance)| {
                count > best_end - best_start + 1
                    || (count == best_end - best_start + 1 && imbalance < best_imbalance)
            }) {
                best = Some((start, end, imbalance));
            }
        }
    }

    let Some((start, end, _)) = best else {
        return vec![QueueActionButtonPlacement {
            button_index: selected_index,
            x: 0,
            width: button_widths[selected_index].min(width),
        }];
    };

    let mut cursor = prefix_width.saturating_add(usize::from(start > 0).saturating_mul(2));
    (start..=end)
        .map(|button_index| {
            cursor = cursor.saturating_add(1);
            let placement = QueueActionButtonPlacement {
                button_index,
                x: cursor,
                width: button_widths[button_index],
            };
            cursor = cursor.saturating_add(button_widths[button_index]);
            placement
        })
        .collect()
}

pub(crate) fn queue_action_button_layout_for_budget(
    labels: &[&str],
    selected_index: usize,
    selected_item_index: Option<usize>,
    width: usize,
    queue_row_budget: usize,
) -> Vec<QueueActionButtonPlacement> {
    if queue_row_budget != 1 {
        return queue_action_button_layout(labels, selected_index, width);
    }
    if labels.is_empty() || width == 0 {
        return Vec::new();
    }
    let selected_index = selected_index.min(labels.len() - 1);
    let display_label = selected_item_index
        .map(|index| format!("#{} {}", index + 1, labels[selected_index]))
        .unwrap_or_else(|| labels[selected_index].to_owned());
    vec![QueueActionButtonPlacement {
        button_index: selected_index,
        x: 0,
        width: terminal_cell_width(&display_label)
            .saturating_add(2)
            .min(width),
    }]
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
    let step_label = if plan.steps.len() == 1 {
        "step"
    } else {
        "steps"
    };
    let counts = format!(
        "{} {} · {} {} · {} {}",
        plan.steps.len(),
        step_label,
        plan.target_path_count,
        path_label,
        plan.suggested_check_count,
        check_label
    );
    let header_detail = format!("  ·  {counts}  ·  {}", plan.summary);
    let header_detail = truncate_status_text(
        &header_detail,
        width.saturating_sub(terminal_cell_width("Plan ready")),
    );
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
            header_detail,
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
        let available = width.saturating_sub(terminal_cell_width(&prefix));
        lines.push(Line::from(vec![
            Span::styled(prefix, Style::default().fg(palette.text_secondary).bg(bg)),
            Span::styled(
                truncate_status_text(step, available),
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
    if plan.stale {
        let reason = plan.stale_reason.as_deref().unwrap_or("plan may be stale");
        lines.push(Line::from(vec![
            Span::styled(
                "Stale",
                Style::default()
                    .fg(palette.status_error)
                    .bg(bg)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!("  {reason}"),
                Style::default().fg(palette.text_secondary).bg(bg),
            ),
        ]));
        lines.push(render_plan_action_line(width, theme));
        return bound_plan_approval_lines(lines, theme);
    }
    lines.push(render_plan_action_line(width, theme));
    bound_plan_approval_lines(lines, theme)
}

fn render_plan_action_line(width: usize, theme: &Theme) -> Line<'static> {
    let palette = &theme.palette;
    let bg = palette.surface_panel_alt;
    let primary = Style::default().fg(palette.text_primary).bg(bg);
    let secondary = Style::default().fg(palette.text_secondary).bg(bg);
    let full_text = "Enter review & actions";
    if width < terminal_cell_width(full_text) {
        let keys = ["Enter"].as_slice();
        let mut spans = Vec::with_capacity(keys.len().saturating_mul(2));
        for (index, key) in keys.iter().enumerate() {
            if index > 0 {
                spans.push(Span::styled(" ", secondary));
            }
            spans.push(Span::styled((*key).to_owned(), primary));
        }
        return status_band_line(Line::from(spans), width, bg);
    }

    Line::from(vec![
        Span::styled("Enter", primary),
        Span::styled(" review & choose an action", secondary),
    ])
}

fn render_plan_overflow_line(theme: &Theme) -> Line<'static> {
    Line::from(Span::styled(
        "... more plan detail; Enter opens complete review",
        Style::default()
            .fg(theme.palette.text_muted)
            .bg(theme.palette.surface_panel_alt),
    ))
}

fn render_hidden_plan_action_line(action: Line<'static>, theme: &Theme) -> Line<'static> {
    let bg = theme.palette.surface_panel_alt;
    let supports_review = action
        .spans
        .iter()
        .any(|span| span.content.contains("Enter"));
    let mut spans = Vec::new();
    if supports_review {
        spans.push(Span::styled(
            "Enter",
            Style::default()
                .fg(theme.palette.text_primary)
                .bg(bg)
                .add_modifier(Modifier::BOLD),
        ));
        spans.push(Span::styled(
            " review plan… ",
            Style::default()
                .fg(theme.palette.accent_warning)
                .bg(bg)
                .add_modifier(Modifier::BOLD),
        ));
        spans.push(Span::styled(
            "· full detail",
            Style::default().fg(theme.palette.text_secondary).bg(bg),
        ));
    } else {
        spans.push(Span::styled(
            "Enter review",
            Style::default()
                .fg(theme.palette.accent_warning)
                .bg(bg)
                .add_modifier(Modifier::BOLD),
        ));
        spans.push(Span::styled(
            " · full detail",
            Style::default().fg(theme.palette.text_secondary).bg(bg),
        ));
    }
    Line::from(spans)
}

fn bound_plan_approval_lines(mut lines: Vec<Line<'static>>, theme: &Theme) -> Vec<Line<'static>> {
    if lines.len() <= LIVE_PLAN_APPROVAL_ROW_LIMIT {
        return lines;
    }
    let Some(action) = lines.pop() else {
        return Vec::new();
    };
    lines.truncate(LIVE_PLAN_APPROVAL_ROW_LIMIT.saturating_sub(2));
    lines.push(render_plan_overflow_line(theme));
    lines.push(action);
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
        1 + task_strip.route_diagnostics.len()
            + task_strip.completion_progress.len()
            + if task_strip.expanded {
                task_strip.rows.len()
            } else {
                task_strip.rows.len().min(TASK_STRIP_COLLAPSED_ROW_LIMIT)
            }
            + usize::from(task_strip.rows.len() > TASK_STRIP_COLLAPSED_ROW_LIMIT),
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
                        truncate_status_text(diagnostic, width.saturating_sub(8)),
                        Style::default()
                            .fg(theme.palette.text_secondary)
                            .bg(theme.palette.surface_panel_alt),
                    ),
                ])
            }),
    );
    lines.extend(
        task_strip
            .completion_progress
            .iter()
            .take(LIVE_TASK_COMPLETION_LIMIT)
            .enumerate()
            .map(|(index, progress)| {
                let label = if index == 0 { "  Batch " } else { "    ↳ " };
                Line::from(vec![
                    Span::styled(
                        label,
                        Style::default()
                            .fg(theme.palette.accent_warning)
                            .bg(theme.palette.surface_panel_alt)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(
                        truncate_status_text(progress, width.saturating_sub(8)),
                        Style::default()
                            .fg(theme.palette.text_secondary)
                            .bg(theme.palette.surface_panel_alt),
                    ),
                ])
            }),
    );
    let visible_range = task_strip_visible_row_range(task_strip);
    lines.extend(
        task_strip.rows[visible_range]
            .iter()
            .map(|row| render_task_strip_row(row, width, theme)),
    );
    if task_strip.rows.len() > TASK_STRIP_COLLAPSED_ROW_LIMIT {
        lines.push(render_task_strip_toggle_line(task_strip, theme));
    }
    lines
}

fn task_strip_visible_row_range(task_strip: &TaskStripViewModel) -> std::ops::Range<usize> {
    let row_count = task_strip.rows.len();
    if task_strip.expanded || row_count <= TASK_STRIP_COLLAPSED_ROW_LIMIT {
        return 0..row_count;
    }
    let focus_index = task_strip
        .rows
        .iter()
        .position(|row| row.active)
        .unwrap_or(0);
    let start = focus_index
        .saturating_sub(TASK_STRIP_COLLAPSED_ROW_LIMIT / 2)
        .min(row_count.saturating_sub(TASK_STRIP_COLLAPSED_ROW_LIMIT));
    start..start.saturating_add(TASK_STRIP_COLLAPSED_ROW_LIMIT)
}

fn render_task_strip_toggle_line(task_strip: &TaskStripViewModel, theme: &Theme) -> Line<'static> {
    let label = if task_strip.expanded {
        "  ▾ Show less · click/Ctrl-T collapse".to_owned()
    } else {
        let hidden = task_strip
            .rows
            .len()
            .saturating_sub(TASK_STRIP_COLLAPSED_ROW_LIMIT);
        format!("  ▸ +{hidden} more tasks · click/Ctrl-T expand")
    };
    Line::from(Span::styled(
        label,
        Style::default()
            .fg(theme.palette.accent_info)
            .bg(theme.palette.surface_panel_alt)
            .add_modifier(Modifier::BOLD),
    ))
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
            card.title.clone(),
            Style::default()
                .fg(palette.accent_warning)
                .bg(bg)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled("  ·  ", Style::default().fg(palette.text_muted).bg(bg)),
        Span::styled(
            truncate_status_text(&card.status, width.saturating_sub(16)),
            Style::default().fg(palette.text_primary).bg(bg),
        ),
    ])];
    lines.push(Line::from(Span::styled(
        truncate_status_text(
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
            truncate_status_text(&format!("  Why         {why}"), width),
            Style::default().fg(palette.text_secondary).bg(bg),
        )));
    }
    if card.inspect_open {
        lines.extend(card.inspect_lines.iter().map(|line| {
            Line::from(Span::styled(
                truncate_status_text(&format!("  {line}"), width),
                Style::default().fg(palette.text_secondary).bg(bg),
            ))
        }));
    }
    let action = card
        .action_label
        .map(|action| format!("  Enter {action}  ·  I inspect"))
        .unwrap_or_else(|| "  I inspect".to_owned());
    lines.push(Line::from(Span::styled(
        truncate_status_text(&action, width),
        Style::default()
            .fg(if card.focused {
                palette.accent_primary
            } else {
                palette.text_muted
            })
            .bg(bg),
    )));
    lines
}

fn render_compact_verification_action(
    card: &crate::view_model::VerificationCardViewModel,
    width: usize,
    theme: &Theme,
) -> Line<'static> {
    let palette = &theme.palette;
    let bg = if card.focused {
        palette.surface_input
    } else {
        palette.surface_panel_alt
    };
    let action = card
        .action_label
        .map(|label| {
            if label == "run check" {
                "Enter run".to_owned()
            } else {
                format!("Enter {label}")
            }
        })
        .unwrap_or_else(|| "I inspect".to_owned());
    let target = card.recommended.as_deref().unwrap_or("none");
    let full = format!("{action} · {target}");
    let text = if terminal_cell_width(&full) <= width {
        full
    } else if card.action_label == Some("run check") {
        let tight = format!("{action} {target}");
        if terminal_cell_width(&tight) <= width {
            tight
        } else {
            let enter_target = format!("Enter {target}");
            if terminal_cell_width(&enter_target) <= width {
                enter_target
            } else {
                compact_hidden_verification_action(&action, width)
            }
        }
    } else {
        compact_hidden_verification_action(&action, width)
    };
    status_band_line(
        Line::from(Span::styled(
            text,
            Style::default()
                .fg(if card.focused {
                    palette.accent_primary
                } else {
                    palette.text_muted
                })
                .bg(bg)
                .add_modifier(Modifier::BOLD),
        )),
        width,
        bg,
    )
}

fn compact_hidden_verification_action(action: &str, width: usize) -> String {
    let hidden = format!("{action} · tgt hidden");
    if terminal_cell_width(&hidden) <= width {
        hidden
    } else {
        truncate_status_text(&format!("{action} · tgt…"), width)
    }
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
    let fixed_width = terminal_cell_width("Task ") + terminal_cell_width("  ·  ");
    let title = truncate_status_text(title, width.saturating_sub(fixed_width));
    let detail_width = width.saturating_sub(fixed_width + terminal_cell_width(&title));
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
            truncate_status_text(&task_strip.detail, detail_width),
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
    let label = truncate_status_text(&row.label, label_width);
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
    wrap_terminal_lines(lines, width)
}

#[cfg(test)]
pub(crate) fn render_live_progress_lines(
    progress: &LiveProgressViewModel,
    accent: Color,
    width: usize,
) -> Vec<Line<'static>> {
    let theme = Theme::default();
    render_live_progress_lines_with_theme(progress, accent, width, &theme)
}

fn render_live_progress_lines_with_theme(
    progress: &LiveProgressViewModel,
    accent: Color,
    width: usize,
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
                truncate_status_text(&format!("{}...", progress.title), width.saturating_sub(2)),
                Style::default()
                    .fg(accent)
                    .bg(bg)
                    .add_modifier(Modifier::BOLD),
            ),
        ]),
        Line::from(vec![
            Span::styled("  ↳ ", Style::default().fg(accent).bg(bg)),
            Span::styled(
                truncate_status_text(&progress.detail, width.saturating_sub(4)),
                Style::default().fg(palette.text_primary).bg(bg),
            ),
        ]),
    ]
}

#[cfg(all(test, not(sigil_tui_test_slice_app_input_flow)))]
#[path = "tests/live_panel_tests.rs"]
mod tests;

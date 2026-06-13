use ratatui::layout::{Constraint, Direction, Layout, Rect};

use crate::{
    app::{AppState, ApprovalAction, ApprovalModalView},
    mouse::HitTarget,
};

use super::{
    geometry::{centered_rect, inset_rect, sidebar_width_for_terminal},
    live_panel::{LIVE_PANEL_BOTTOM_PADDING, LIVE_PROGRESS_ROWS},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LayoutMode {
    Main,
    Setup,
    Config,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LayoutSnapshot {
    pub screen: Rect,
    pub mode: LayoutMode,
    pub live_panel: Rect,
    pub composer: Rect,
    pub footer: Rect,
    pub info_rail: Rect,
    pub live_text_rows: Vec<LiveTextRowHitArea>,
    pub tool_cards: Vec<ToolCardHitArea>,
    pub slash_overlay: Option<SlashOverlayHitAreas>,
    pub approval_modal: Option<Rect>,
    pub approval_modal_hit_areas: Option<ApprovalModalHitAreas>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ToolCardHitArea {
    pub entry_index: usize,
    pub area: Rect,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LiveTextRowHitArea {
    pub line_index: usize,
    pub area: Rect,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SlashOverlayHitAreas {
    pub overlay: Rect,
    pub content: Rect,
    pub window_start: usize,
    pub window_end: usize,
    pub title_rows: u16,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApprovalModalHitAreas {
    pub modal: Rect,
    pub diff_area: Rect,
    pub hunk_previous: Rect,
    pub hunk_next: Rect,
    pub diff_view_toggle: Rect,
    pub metadata_toggle: Rect,
    pub file_rows: Vec<ApprovalFileRowHitArea>,
    pub allow_action: Rect,
    pub deny_action: Rect,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ApprovalFileRowHitArea {
    pub index: usize,
    pub area: Rect,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct ShellLayout {
    pub live_panel: Rect,
    pub composer: Rect,
    pub footer: Rect,
    pub info_rail: Rect,
}

impl LayoutSnapshot {
    pub fn from_app(screen: Rect, app: &AppState) -> Self {
        if app.is_setup_mode() {
            return Self::single(screen, LayoutMode::Setup);
        }
        if app.is_config_mode() {
            return Self::single(screen, LayoutMode::Config);
        }

        let shell = shell_layout(screen, app.footer_strip_height());
        Self {
            screen,
            mode: LayoutMode::Main,
            live_panel: shell.live_panel,
            composer: shell.composer,
            footer: shell.footer,
            info_rail: shell.info_rail,
            live_text_rows: live_text_row_hit_areas(shell.live_panel, app),
            tool_cards: tool_card_hit_areas(shell.live_panel, app),
            slash_overlay: slash_overlay_hit_areas(shell.live_panel, shell.composer, app),
            approval_modal: app
                .approval_modal_view()
                .map(|view| approval_modal_area(screen, &view)),
            approval_modal_hit_areas: app
                .approval_modal_view()
                .and_then(|view| approval_modal_hit_areas(screen, &view)),
        }
    }

    fn single(screen: Rect, mode: LayoutMode) -> Self {
        Self {
            screen,
            mode,
            live_panel: Rect::default(),
            composer: Rect::default(),
            footer: Rect::default(),
            info_rail: Rect::default(),
            live_text_rows: Vec::new(),
            tool_cards: Vec::new(),
            slash_overlay: None,
            approval_modal: None,
            approval_modal_hit_areas: None,
        }
    }

    pub fn hit_target(&self, column: u16, row: u16) -> HitTarget {
        if let Some(areas) = &self.approval_modal_hit_areas {
            if contains(areas.hunk_previous, column, row) {
                return HitTarget::ApprovalHunkPrevious;
            }
            if contains(areas.hunk_next, column, row) {
                return HitTarget::ApprovalHunkNext;
            }
            if contains(areas.diff_view_toggle, column, row) {
                return HitTarget::ApprovalDiffViewToggle;
            }
            if contains(areas.metadata_toggle, column, row) {
                return HitTarget::ApprovalMetadataToggle;
            }
            for file_row in &areas.file_rows {
                if contains(file_row.area, column, row) {
                    return HitTarget::ApprovalFileRow {
                        index: file_row.index,
                    };
                }
            }
            if contains(areas.allow_action, column, row) {
                return HitTarget::ApprovalAction { approved: true };
            }
            if contains(areas.deny_action, column, row) {
                return HitTarget::ApprovalAction { approved: false };
            }
            if contains(areas.diff_area, column, row) {
                return HitTarget::ApprovalDiffArea;
            }
        }

        if let Some(area) = self.approval_modal
            && contains(area, column, row)
        {
            return HitTarget::ApprovalModal;
        }

        if let Some(slash_overlay) = self.slash_overlay {
            if let Some(index) = slash_overlay.candidate_at(column, row) {
                return HitTarget::SlashCandidate { index };
            }
            if contains(slash_overlay.overlay, column, row) {
                return HitTarget::SlashOverlay;
            }
        }

        if self.mode != LayoutMode::Main {
            return HitTarget::Background;
        }
        if contains(self.composer, column, row) {
            return HitTarget::Composer;
        }
        for tool_card in &self.tool_cards {
            if contains(tool_card.area, column, row) {
                return HitTarget::ToolCard {
                    entry_index: tool_card.entry_index,
                };
            }
        }
        if contains(self.live_panel, column, row) {
            return HitTarget::LivePanel;
        }
        if contains(self.info_rail, column, row) {
            return HitTarget::InfoRail;
        }
        HitTarget::Background
    }

    pub fn live_text_line_at(&self, column: u16, row: u16) -> Option<usize> {
        self.live_text_rows
            .iter()
            .find_map(|hit| contains(hit.area, column, row).then_some(hit.line_index))
    }
}

impl SlashOverlayHitAreas {
    pub fn candidate_at(self, column: u16, row: u16) -> Option<usize> {
        if !contains(self.content, column, row) {
            return None;
        }
        let row_offset = row.saturating_sub(self.content.y);
        if row_offset < self.title_rows {
            return None;
        }

        let candidate_offset = row_offset.saturating_sub(self.title_rows) as usize;
        let index = self.window_start.saturating_add(candidate_offset);
        (index < self.window_end).then_some(index)
    }
}

fn tool_card_hit_areas(live_area: Rect, app: &AppState) -> Vec<ToolCardHitArea> {
    let Some(rows) = visible_timeline_rows(live_area, app) else {
        return Vec::new();
    };

    app.tool_activity_entry_indices()
        .into_iter()
        .filter_map(|entry_index| {
            let range = app.timeline_entry_render_range(entry_index)?;
            let start = range.start.max(rows.visible_start);
            let end = range.end.min(rows.visible_end);
            (start < end).then(|| ToolCardHitArea {
                entry_index,
                area: Rect::new(
                    rows.content_frame.x,
                    rows.content_y
                        .saturating_add((start - rows.visible_start) as u16),
                    rows.content_frame.width,
                    (end - start) as u16,
                ),
            })
        })
        .collect()
}

fn live_text_row_hit_areas(live_area: Rect, app: &AppState) -> Vec<LiveTextRowHitArea> {
    let Some(rows) = visible_timeline_rows(live_area, app) else {
        return Vec::new();
    };

    (rows.visible_start..rows.visible_end)
        .map(|line_index| LiveTextRowHitArea {
            line_index,
            area: Rect::new(
                rows.content_frame.x,
                rows.content_y
                    .saturating_add((line_index - rows.visible_start) as u16),
                rows.content_frame.width,
                1,
            ),
        })
        .collect()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct VisibleTimelineRows {
    content_frame: Rect,
    content_y: u16,
    visible_start: usize,
    visible_end: usize,
}

fn visible_timeline_rows(live_area: Rect, app: &AppState) -> Option<VisibleTimelineRows> {
    if live_area.width == 0 || live_area.height == 0 {
        return None;
    }
    let inner = inset_rect(live_area, 1, 0);
    if inner.width == 0 || inner.height == 0 {
        return None;
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
    let progress_rows = if app.live_activity_summary().is_some() {
        LIVE_PROGRESS_ROWS as usize
    } else {
        0
    };
    let transcript_rows = inner
        .height
        .saturating_sub(if progress_rows > 0 {
            LIVE_PROGRESS_ROWS
        } else {
            0
        })
        .max(1) as usize;
    let requested_timeline_range = app.visible_timeline_render_range(transcript_rows);

    let requested_timeline_rows = requested_timeline_range.end - requested_timeline_range.start;
    let total_rows = requested_timeline_rows.saturating_add(progress_rows);
    let content_capacity = content_frame.height as usize;
    let dropped_rows = total_rows.saturating_sub(content_capacity);
    let dropped_timeline_rows = dropped_rows.min(requested_timeline_rows);
    let visible_timeline_start = requested_timeline_range
        .start
        .saturating_add(dropped_timeline_rows);
    let visible_timeline_end = requested_timeline_range.end;
    if visible_timeline_start >= visible_timeline_end {
        return None;
    }

    let rendered_rows = total_rows.min(content_capacity);
    let content_y = content_frame
        .y
        .saturating_add(content_frame.height.saturating_sub(rendered_rows as u16));

    Some(VisibleTimelineRows {
        content_frame,
        content_y,
        visible_start: visible_timeline_start,
        visible_end: visible_timeline_end,
    })
}

pub(super) fn approval_modal_area(screen: Rect, view: &ApprovalModalView) -> Rect {
    let diff_width = view
        .diff_lines
        .iter()
        .map(|line| line.text.chars().count().saturating_add(12))
        .max()
        .unwrap_or(0);
    let inner_width = [
        72usize,
        diff_width.saturating_add(12),
        view.preview_title.chars().count().saturating_add(10),
    ]
    .into_iter()
    .max()
    .unwrap_or(72)
    .min(screen.width.saturating_sub(8).max(36) as usize);

    centered_rect(
        inner_width as u16 + 2,
        screen.height.saturating_sub(4).min(30),
        screen,
    )
}

fn approval_modal_hit_areas(
    screen: Rect,
    view: &ApprovalModalView,
) -> Option<ApprovalModalHitAreas> {
    let modal = approval_modal_area(screen, view);
    let inner = inset_rect(modal, 1, 1);
    if inner.width == 0 || inner.height == 0 {
        return None;
    }

    let footer_height = 4u16.min(inner.height);
    let header_height = approval_header_line_count(view)
        .saturating_add(2)
        .min(inner.height.saturating_sub(footer_height));
    let body_y = inner.y.saturating_add(header_height);
    let footer_y = inner
        .y
        .saturating_add(inner.height.saturating_sub(footer_height));
    let body_area = Rect::new(
        inner.x,
        body_y,
        inner.width,
        footer_y.saturating_sub(body_y),
    );
    let footer_area = Rect::new(inner.x, footer_y, inner.width, footer_height);
    let footer_inner = inset_rect(footer_area, 1, 1);
    let (allow_action, deny_action) = approval_action_hit_areas(footer_inner, view.selected_action);
    let diff_area = approval_diff_area(body_area, view);
    let diff_inner = inset_rect(diff_area, 1, 1);
    let diff_status = Rect::new(diff_inner.x, diff_inner.y, diff_inner.width, 1);
    let diff_controls = approval_diff_control_hit_areas(diff_status, view);

    Some(ApprovalModalHitAreas {
        modal,
        diff_area,
        hunk_previous: diff_controls.hunk_previous,
        hunk_next: diff_controls.hunk_next,
        diff_view_toggle: diff_controls.diff_view_toggle,
        metadata_toggle: diff_controls.metadata_toggle,
        file_rows: approval_file_row_hit_areas(body_area, view),
        allow_action,
        deny_action,
    })
}

fn approval_header_line_count(view: &ApprovalModalView) -> u16 {
    let summary_lines = if view.metadata_collapsed || view.preview_summary.trim().is_empty() {
        1
    } else {
        view.preview_summary.lines().take(2).count().max(1) as u16
    };
    3u16.saturating_add(summary_lines)
}

fn approval_file_row_hit_areas(
    body_area: Rect,
    view: &ApprovalModalView,
) -> Vec<ApprovalFileRowHitArea> {
    if view.file_rows.is_empty() || body_area.width == 0 || body_area.height == 0 {
        return Vec::new();
    }

    let file_width = if body_area.width >= 92 { 28 } else { 22 }
        .min(body_area.width.saturating_sub(18))
        .max(16)
        .min(body_area.width);
    let file_inner = inset_rect(
        Rect::new(body_area.x, body_area.y, file_width, body_area.height),
        1,
        1,
    );
    if file_inner.width == 0 || file_inner.height == 0 {
        return Vec::new();
    }

    view.file_rows
        .iter()
        .enumerate()
        .take(file_inner.height as usize)
        .map(|(index, _)| ApprovalFileRowHitArea {
            index,
            area: Rect::new(
                file_inner.x,
                file_inner.y.saturating_add(index as u16),
                file_inner.width,
                1,
            ),
        })
        .collect()
}

fn approval_diff_area(body_area: Rect, view: &ApprovalModalView) -> Rect {
    if body_area.width == 0 || body_area.height == 0 {
        return Rect::default();
    }
    if view.file_rows.is_empty() {
        return body_area;
    }

    let file_width = if body_area.width >= 92 { 28 } else { 22 }
        .min(body_area.width.saturating_sub(18))
        .max(16)
        .min(body_area.width);
    let diff_x = body_area.x.saturating_add(file_width);
    Rect::new(
        diff_x,
        body_area.y,
        body_area
            .x
            .saturating_add(body_area.width)
            .saturating_sub(diff_x),
        body_area.height,
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct ApprovalDiffControlHitAreas {
    pub hunk_previous: Rect,
    pub hunk_next: Rect,
    pub diff_view_toggle: Rect,
    pub metadata_toggle: Rect,
}

pub(super) fn approval_diff_control_hit_areas(
    status_area: Rect,
    view: &ApprovalModalView,
) -> ApprovalDiffControlHitAreas {
    if status_area.width == 0 || status_area.height == 0 {
        return ApprovalDiffControlHitAreas {
            hunk_previous: Rect::default(),
            hunk_next: Rect::default(),
            diff_view_toggle: Rect::default(),
            metadata_toggle: Rect::default(),
        };
    }

    let end = status_area.x.saturating_add(status_area.width);
    let mut cursor = status_area.x;
    let hunk_previous = approval_status_badge_rect(status_area.y, &mut cursor, end, "Prev");
    let hunk_next = approval_status_badge_rect(status_area.y, &mut cursor, end, "Next");
    let diff_view_toggle = approval_status_badge_rect(
        status_area.y,
        &mut cursor,
        end,
        &approval_diff_view_control_label(view.diff_mode_label),
    );
    let metadata_toggle = approval_status_badge_rect(
        status_area.y,
        &mut cursor,
        end,
        approval_metadata_control_label(view.metadata_collapsed),
    );

    ApprovalDiffControlHitAreas {
        hunk_previous,
        hunk_next,
        diff_view_toggle,
        metadata_toggle,
    }
}

pub(super) fn approval_diff_view_control_label(diff_mode_label: &str) -> String {
    format!("View {diff_mode_label}")
}

pub(super) fn approval_metadata_control_label(metadata_collapsed: bool) -> &'static str {
    if metadata_collapsed {
        "Meta hidden"
    } else {
        "Meta"
    }
}

fn approval_status_badge_rect(y: u16, cursor: &mut u16, end: u16, label: &str) -> Rect {
    if *cursor >= end {
        return Rect::default();
    }
    let width = label.chars().count().saturating_add(2) as u16;
    let rect = Rect::new(*cursor, y, width.min(end.saturating_sub(*cursor)), 1);
    *cursor = (*cursor).saturating_add(width).saturating_add(1);
    rect
}

fn approval_action_hit_areas(footer_inner: Rect, selected_action: ApprovalAction) -> (Rect, Rect) {
    if footer_inner.width == 0 || footer_inner.height == 0 {
        return (Rect::default(), Rect::default());
    }

    let allow_width =
        approval_action_badge_width("Allow", selected_action == ApprovalAction::Allow);
    let deny_width = approval_action_badge_width("Deny", selected_action == ApprovalAction::Deny);
    let allow = Rect::new(
        footer_inner.x,
        footer_inner.y,
        allow_width.min(footer_inner.width),
        1,
    );
    let deny_x = allow.x.saturating_add(allow.width).saturating_add(1);
    let deny = Rect::new(
        deny_x,
        footer_inner.y,
        deny_width.min(
            footer_inner
                .x
                .saturating_add(footer_inner.width)
                .saturating_sub(deny_x),
        ),
        1,
    );
    (allow, deny)
}

fn approval_action_badge_width(label: &str, selected: bool) -> u16 {
    if selected {
        format!("▶ {label} ").chars().count() as u16
    } else {
        label.chars().count().saturating_add(2) as u16
    }
}

pub(super) fn shell_layout(screen: Rect, footer_height: u16) -> ShellLayout {
    let sidebar_width = sidebar_width_for_terminal(screen.width as usize) as u16;
    let shell = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Min(10), Constraint::Length(sidebar_width)])
        .split(screen);

    let main = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(1),
            Constraint::Length(footer_height),
            Constraint::Length(1),
        ])
        .split(shell[0]);

    ShellLayout {
        live_panel: main[0],
        composer: main[1],
        footer: main[2],
        info_rail: shell[1],
    }
}

pub(super) fn slash_selector_overlay_rect(
    live_area: Rect,
    composer_area: Rect,
    visible_rows: usize,
) -> Option<Rect> {
    let height = visible_rows.min(live_area.height as usize) as u16;
    if height == 0 {
        return None;
    }

    let x = composer_area.x.saturating_add(1);
    let right = live_area.x.saturating_add(live_area.width);
    let width = composer_area
        .width
        .saturating_sub(2)
        .min(right.saturating_sub(x));
    if width == 0 {
        return None;
    }

    let mut y = composer_area.y.saturating_sub(height);
    if y < live_area.y {
        y = live_area.y;
    }

    Some(Rect::new(x, y, width, height))
}

fn slash_overlay_hit_areas(
    live_area: Rect,
    composer_area: Rect,
    app: &AppState,
) -> Option<SlashOverlayHitAreas> {
    if !app.has_slash_selector() || live_area.width == 0 || live_area.height == 0 {
        return None;
    }

    let visible_rows = app.slash_selector_visible_rows() as usize;
    let overlay = slash_selector_overlay_rect(live_area, composer_area, visible_rows)?;
    let content = Rect::new(
        overlay.x.saturating_add(2),
        overlay.y,
        overlay.width.saturating_sub(4),
        overlay.height,
    );
    if content.width == 0 || content.height == 0 {
        return None;
    }

    let title_rows = u16::from(app.slash_selector_title().is_some());
    let row_capacity = (content.height as usize).saturating_sub(title_rows as usize);
    let selector_rows = app.slash_selector_rows();
    let selected_index = app.slash_selector_selected_index().unwrap_or(0);
    let (window_start, window_end) = if row_capacity == 0 || selector_rows.is_empty() {
        (0, 0)
    } else {
        super::geometry::selector_window_range(selector_rows.len(), selected_index, row_capacity)
    };

    Some(SlashOverlayHitAreas {
        overlay,
        content,
        window_start,
        window_end,
        title_rows,
    })
}

fn contains(area: Rect, column: u16, row: u16) -> bool {
    column >= area.x
        && column < area.x.saturating_add(area.width)
        && row >= area.y
        && row < area.y.saturating_add(area.height)
}

#[cfg(test)]
#[path = "tests/layout_snapshot_tests.rs"]
mod tests;

use ratatui::layout::{Constraint, Direction, Layout, Rect};

use crate::{
    app::{AppState, ApprovalModalView},
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
    pub tool_cards: Vec<ToolCardHitArea>,
    pub slash_overlay: Option<SlashOverlayHitAreas>,
    pub approval_modal: Option<Rect>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ToolCardHitArea {
    pub entry_index: usize,
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
            tool_cards: tool_card_hit_areas(shell.live_panel, app),
            slash_overlay: slash_overlay_hit_areas(shell.live_panel, shell.composer, app),
            approval_modal: app
                .approval_modal_view()
                .map(|view| approval_modal_area(screen, &view)),
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
            tool_cards: Vec::new(),
            slash_overlay: None,
            approval_modal: None,
        }
    }

    pub fn hit_target(&self, column: u16, row: u16) -> HitTarget {
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
    if live_area.width == 0 || live_area.height == 0 {
        return Vec::new();
    }
    let inner = inset_rect(live_area, 1, 0);
    if inner.width == 0 || inner.height == 0 {
        return Vec::new();
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
    if requested_timeline_range.is_empty() {
        return Vec::new();
    }

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
        return Vec::new();
    }

    let rendered_rows = total_rows.min(content_capacity);
    let content_y = content_frame
        .y
        .saturating_add(content_frame.height.saturating_sub(rendered_rows as u16));

    app.tool_activity_entry_indices()
        .into_iter()
        .filter_map(|entry_index| {
            let range = app.timeline_entry_render_range(entry_index)?;
            let start = range.start.max(visible_timeline_start);
            let end = range.end.min(visible_timeline_end);
            (start < end).then(|| ToolCardHitArea {
                entry_index,
                area: Rect::new(
                    content_frame.x,
                    content_y.saturating_add((start - visible_timeline_start) as u16),
                    content_frame.width,
                    (end - start) as u16,
                ),
            })
        })
        .collect()
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

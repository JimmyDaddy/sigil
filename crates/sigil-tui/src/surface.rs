//! Owned input to the TUI renderer.
//!
//! `AppState` is an orchestration state machine.  It must not cross the renderer boundary: a
//! frame is rendered from one immutable, owned snapshot so that layout, hit testing and pixels
//! cannot observe different application states during a draw.

use ratatui::{buffer::Buffer, layout::Rect, text::Line, widgets::Widget};

pub(crate) use crate::app::CheckpointRestoreModalView;
pub(crate) use crate::app::EgressDisclosureCard;
pub(crate) use crate::app::IntentStackModalView;
pub(crate) use crate::app::PaneFocus;
pub(crate) use crate::app::session_lifecycle_flow::SessionModalAction;
pub(crate) use crate::app::{
    PendingPlanApproval, PendingUserInputForm, PlanWorkbenchAction, UserInputDraftValue,
    UserInputFormAction, UserInputFormSource,
};
pub(crate) use crate::approval::ApprovalModalView;

use crate::{
    ui::theme::ThemeSpec,
    ui::{LayoutMode, LayoutSnapshot},
    view_model::{ComposerViewModel, FooterViewModel, InfoRailViewModel, LivePanelViewModel},
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SetupSurface {
    pub trust_gate: bool,
    pub lines: Vec<String>,
    pub field_line_indices: Vec<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ConfigSurface {
    /// A bounded, display-only file label.  The renderer never receives the authority path.
    pub file_label: String,
    pub section_title: Option<String>,
    pub selected_field_label: Option<String>,
    pub detail_lines: Vec<String>,
    pub status_summary: String,
    pub footer_hint: String,
    pub selected_footer_action: Option<String>,
    pub footer_actions: Vec<String>,
    pub save_error: Option<String>,
    pub dirty: bool,
    pub close_guard_armed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SlashSelectorSurface {
    pub visible: bool,
    pub title: Option<String>,
    pub rows: Vec<(String, String)>,
    pub selected_index: Option<usize>,
    pub visible_rows: u16,
    pub empty_message: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ModalSurface {
    pub visible: bool,
    pub checkpoint_restore_visible: bool,
    pub title: Option<String>,
    pub lines: Vec<String>,
    pub input_cursor: Option<(String, usize, usize)>,
    pub config_mode: bool,
    pub session_modal_actions: Vec<(SessionModalAction, String)>,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct StatusSurface {
    /// Bounded display labels, not executable or authority paths.
    pub workspace_label: String,
    pub config_label: String,
    pub session_id: String,
    pub provider_name: String,
    pub model_name: String,
    pub permission_mode: String,
    pub is_busy: bool,
    pub cache_hit_ratio: f64,
    pub memory_enabled: bool,
    pub memory_document_count: usize,
    pub memory_last_status: String,
    pub compaction_status: String,
    pub active_pane: PaneFocus,
    pub last_notice: Option<String>,
}

#[derive(Debug, Clone)]
pub(crate) struct SurfaceModel {
    pub theme: ThemeSpec,
    pub mode: LayoutMode,
    pub layout: LayoutSnapshot,
    pub info_rail: InfoRailViewModel,
    pub composer: ComposerViewModel,
    pub footer: FooterViewModel,
    pub live_panel: LivePanelViewModel,
    pub transcript: VirtualTextWindow,
    pub setup: Option<SetupSurface>,
    pub config: Option<ConfigSurface>,
    pub status: StatusSurface,
    pub egress_disclosure: Option<EgressDisclosureCard>,
    pub pending_user_input: Option<PendingUserInputForm>,
    pub pending_plan_approval: Option<PendingPlanApproval>,
    pub approval: Option<ApprovalModalView>,
    pub approval_scroll_back: u16,
    pub checkpoint_restore: Option<CheckpointRestoreModalView>,
    pub intent_stack: Option<IntentStackModalView>,
    pub modal: ModalSurface,
    pub slash_selector: SlashSelectorSurface,
    pub composer_auxiliary_panel_focused: bool,
    pub user_input_open: bool,
    pub plan_workbench_open: bool,
    pub state: SurfaceState,
}

/// A renderer-facing window over a larger logical text stream.
///
/// The adapter owns paging/scroll decisions and supplies only the resident rows needed by this
/// frame.  `total_items` is deliberately separate from `items.len()` so a renderer cannot
/// accidentally interpret a preloaded full transcript as the paging source.
#[derive(Debug, Clone)]
pub(crate) struct VirtualTextWindow {
    pub generation: u64,
    pub first_item: usize,
    pub total_items: usize,
    pub items: Vec<Line<'static>>,
    pub item_ids: Vec<SurfaceItemId>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct SurfaceItemId {
    pub generation: u64,
    pub ordinal: usize,
}

impl VirtualTextWindow {
    pub(crate) fn new(
        generation: u64,
        first_item: usize,
        total_items: usize,
        items: Vec<Line<'static>>,
    ) -> Self {
        let item_ids = (0..items.len())
            .map(|offset| SurfaceItemId {
                generation,
                ordinal: first_item.saturating_add(offset),
            })
            .collect();
        Self {
            generation,
            first_item,
            total_items: total_items.max(first_item.saturating_add(items.len())),
            items,
            item_ids,
        }
    }

    pub(crate) fn resident_len(&self) -> usize {
        self.items.len()
    }

    pub(crate) fn is_bounded_by(&self, max_resident_items: usize) -> bool {
        self.items.len() <= max_resident_items
            && self.total_items >= self.first_item.saturating_add(self.items.len())
            && self.item_ids.len() == self.items.len()
            && self.item_ids.iter().enumerate().all(|(offset, id)| {
                id.generation == self.generation
                    && id.ordinal == self.first_item.saturating_add(offset)
            })
    }
}

impl SurfaceModel {
    pub(crate) fn is_setup(&self) -> bool {
        matches!(self.mode, LayoutMode::Setup)
    }

    pub(crate) fn is_config(&self) -> bool {
        matches!(self.mode, LayoutMode::Config)
    }

    pub(crate) fn main_overlays_hidden(&self) -> bool {
        self.plan_workbench_open || self.user_input_open
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SurfaceState {
    pub frame_generation: u64,
    pub terminal_epoch: u64,
}

/// Scratch-only rendering context for custom surface widgets.
///
/// A custom widget receives a buffer whose area is limited to its assigned rectangle.  The
/// scissor is applied before dispatch, so a widget cannot paint outside its surface or borrow the
/// terminal's full frame.  The final compositor remains responsible for copying this bounded
/// result into the committed frame.
#[allow(dead_code)]
#[derive(Debug)]
pub(crate) struct RenderContext {
    bounds: Rect,
    scissor: Rect,
    buffer: Buffer,
}

#[allow(dead_code)]
impl RenderContext {
    pub(crate) fn new(bounds: Rect, scissor: Rect) -> Self {
        let scissor = bounds.intersection(scissor);
        Self {
            bounds,
            scissor,
            buffer: Buffer::empty(bounds),
        }
    }

    pub(crate) fn bounds(&self) -> Rect {
        self.bounds
    }

    pub(crate) fn scissor(&self) -> Rect {
        self.scissor
    }

    pub(crate) fn render_widget<W: Widget>(&mut self, area: Rect, widget: W) -> bool {
        let area = self.scissor.intersection(area);
        if area.width == 0 || area.height == 0 {
            return false;
        }
        widget.render(area, &mut self.buffer);
        true
    }

    pub(crate) fn buffer(&self) -> &Buffer {
        &self.buffer
    }

    pub(crate) fn into_buffer(self) -> Buffer {
        self.buffer
    }
}

/// An O(log N) prefix-sum index for variable-height virtual items.
#[allow(dead_code)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct HeightIndex {
    heights: Vec<u32>,
    tree: Vec<u64>,
}

#[allow(dead_code)]
impl HeightIndex {
    pub(crate) fn with_estimate(item_count: usize, estimated_height: u32) -> Self {
        let mut index = Self {
            heights: vec![estimated_height.max(1); item_count],
            tree: vec![0; item_count.saturating_add(1)],
        };
        for item in 0..item_count {
            index.add_tree(item, u64::from(estimated_height.max(1)));
        }
        index
    }

    pub(crate) fn len(&self) -> usize {
        self.heights.len()
    }

    pub(crate) fn total_height(&self) -> u64 {
        self.prefix_height(self.len())
    }

    pub(crate) fn height(&self, item: usize) -> Option<u32> {
        self.heights.get(item).copied()
    }

    pub(crate) fn set_height(&mut self, item: usize, height: u32) -> Option<u32> {
        let previous = *self.heights.get(item)?;
        let height = height.max(1);
        if previous != height {
            self.heights[item] = height;
            if height > previous {
                self.add_tree(item, u64::from(height - previous));
            } else {
                self.subtract_tree(item, u64::from(previous - height));
            }
        }
        Some(previous)
    }

    pub(crate) fn prefix_height(&self, item_count: usize) -> u64 {
        let mut index = item_count.min(self.len());
        let mut total = 0;
        while index > 0 {
            total += self.tree[index];
            index &= index - 1;
        }
        total
    }

    /// Locate the item containing a logical row offset and return its intra-item row.
    pub(crate) fn locate_row(&self, row: u64) -> Option<(usize, u32)> {
        if row >= self.total_height() || self.heights.is_empty() {
            return None;
        }
        let mut low = 0usize;
        let mut high = self.len();
        while low < high {
            let middle = low + (high - low) / 2;
            if self.prefix_height(middle.saturating_add(1)) <= row {
                low = middle.saturating_add(1);
            } else {
                high = middle;
            }
        }
        let item = low.min(self.len().saturating_sub(1));
        Some((item, (row - self.prefix_height(item)) as u32))
    }

    fn add_tree(&mut self, item: usize, value: u64) {
        let mut index = item.saturating_add(1);
        while index < self.tree.len() {
            self.tree[index] = self.tree[index].saturating_add(value);
            index += index & index.wrapping_neg();
        }
    }

    fn subtract_tree(&mut self, item: usize, value: u64) {
        let mut index = item.saturating_add(1);
        while index < self.tree.len() {
            self.tree[index] = self.tree[index].saturating_sub(value);
            index += index & index.wrapping_neg();
        }
    }
}

#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ViewportAnchor {
    pub item_id: SurfaceItemId,
    pub intra_item_row: u32,
}

#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ProjectionPageRequest {
    pub request_id: u64,
    pub generation: u64,
    pub first_item: usize,
    pub item_count: usize,
}

#[cfg(test)]
mod tests {
    use super::{HeightIndex, RenderContext, VirtualTextWindow};
    use ratatui::text::Line;
    use ratatui::{layout::Rect, widgets::Block};

    #[test]
    fn virtual_text_window_keeps_a_bounded_resident_set_for_large_transcripts() {
        let resident = (0..32)
            .map(|index| Line::from(format!("line {index}")))
            .collect::<Vec<_>>();
        let window = VirtualTextWindow::new(7, 99_968, 100_000, resident);
        assert_eq!(window.total_items, 100_000);
        assert_eq!(window.resident_len(), 32);
        assert!(window.is_bounded_by(32));
        assert_eq!(window.item_ids.first().map(|id| id.ordinal), Some(99_968));
        assert_eq!(window.item_ids.last().map(|id| id.ordinal), Some(99_999));
    }

    #[test]
    fn virtual_text_window_ids_change_with_generation_not_position_only() {
        let first = VirtualTextWindow::new(1, 4, 8, vec![Line::from("same")]);
        let second = VirtualTextWindow::new(2, 4, 8, vec![Line::from("same")]);
        assert_ne!(first.item_ids, second.item_ids);
        assert_ne!(first.generation, second.generation);
    }

    #[test]
    fn height_index_updates_variable_rows_and_preserves_anchor_lookup() {
        let mut index = HeightIndex::with_estimate(100_000, 2);
        assert_eq!(index.total_height(), 200_000);
        assert_eq!(index.set_height(50_000, 7), Some(2));
        assert_eq!(index.total_height(), 200_005);
        assert_eq!(index.locate_row(100_000), Some((50_000, 0)));
        assert_eq!(index.locate_row(100_006), Some((50_000, 6)));
        assert_eq!(index.locate_row(100_007), Some((50_001, 0)));
    }

    #[test]
    fn render_context_scissors_custom_widgets_to_assigned_bounds() {
        let mut context = RenderContext::new(Rect::new(4, 3, 5, 2), Rect::new(0, 0, 20, 20));
        assert_eq!(context.bounds(), Rect::new(4, 3, 5, 2));
        assert_eq!(context.scissor(), Rect::new(4, 3, 5, 2));
        assert!(context.render_widget(Rect::new(0, 0, 20, 20), Block::default()));
        assert_eq!(context.buffer().area, Rect::new(4, 3, 5, 2));
        assert!(!context.render_widget(Rect::new(0, 0, 0, 0), Block::default()));
    }
}

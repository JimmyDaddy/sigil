//! Owned input to the TUI renderer.
//!
//! `AppState` is an orchestration state machine.  It must not cross the renderer boundary: a
//! frame is rendered from one immutable, owned snapshot so that layout, hit testing and pixels
//! cannot observe different application states during a draw.

use ratatui::text::Line;

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

pub(crate) type VirtualTextWindow = sigil_tui::VirtualSequence<Line<'static>>;

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

//! Application-to-surface projection.
//!
//! This is the only production module in the TUI that is allowed to know `AppState` for the
//! purpose of building a render snapshot.  The renderer receives the resulting owned DTO.

use std::path::Path;

use crate::{
    app::AppState,
    surface::SurfaceState,
    surface::{
        ConfigSurface, ModalSurface, SetupSurface, SlashSelectorSurface, StatusSurface,
        SurfaceModel, VirtualTextWindow,
    },
    ui,
    view_model::{LivePanelViewModel, UiViewModel},
};

pub(crate) fn build_surface_model(
    screen: ratatui::layout::Rect,
    app: &AppState,
    surface_state: SurfaceState,
) -> SurfaceModel {
    let theme = ui::theme::resolve_for_app(app);
    let view_model = UiViewModel::from_app(app);
    let transcript_rows = ui::live_transcript_rows_for_app(screen, app);
    let live_panel = LivePanelViewModel::from_app(app, transcript_rows);
    let transcript_lines = live_panel
        .transcript_lines
        .iter()
        .map(ui::bidi_reorder_line)
        .collect::<Vec<_>>();
    let transcript = VirtualTextWindow::new(
        app.session_browser.current_entries_revision,
        if app.main_timeline_active() {
            app.visible_timeline_render_range(transcript_rows).start
        } else {
            0
        },
        if app.main_timeline_active() {
            app.timeline_render_line_count()
        } else {
            live_panel.transcript_lines.len()
        },
        transcript_lines,
    );
    let layout = ui::LayoutSnapshot::from_app(screen, app);
    let mode = layout.mode;

    SurfaceModel {
        theme,
        mode,
        layout,
        info_rail: view_model.info_rail,
        composer: view_model.composer,
        footer: view_model.footer,
        live_panel,
        transcript,
        setup: setup_surface(app),
        config: config_surface(app),
        status: status_surface(app),
        egress_disclosure: app.active_egress_disclosure_card(),
        pending_user_input: app.pending_user_input().cloned(),
        pending_plan_approval: app.pending_plan_approval().cloned(),
        approval: app.approval_modal_view(),
        approval_scroll_back: app.approval.scroll_back.min(u16::MAX as usize) as u16,
        checkpoint_restore: app.checkpoint_restore_modal_view(),
        intent_stack: app.intent_stack_modal_view(),
        modal: modal_surface(app),
        slash_selector: slash_selector_surface(app),
        composer_auxiliary_panel_focused: app.is_composer_queue_panel_focused()
            || app.is_composer_agent_panel_focused(),
        user_input_open: app.pending_user_input().is_some_and(|form| form.open),
        plan_workbench_open: app
            .pending_plan_approval()
            .is_some_and(|pending| pending.workbench_open),
        state: surface_state,
    }
}

fn setup_surface(app: &AppState) -> Option<SetupSurface> {
    if !app.is_setup_mode() && !app.is_workspace_trust_gate_mode() {
        return None;
    }
    Some(SetupSurface {
        trust_gate: app.is_workspace_trust_gate_mode(),
        lines: if app.is_workspace_trust_gate_mode() {
            app.workspace_trust_gate_lines()
        } else {
            app.setup_lines()
        },
        field_line_indices: app.setup_field_line_indices(),
    })
}

fn config_surface(app: &AppState) -> Option<ConfigSurface> {
    if !app.is_config_mode() {
        return None;
    }
    Some(ConfigSurface {
        file_label: display_file_label(&app.config_path),
        section_title: app.config_section_title().map(str::to_owned),
        selected_field_label: app.config_selected_field_label().map(str::to_owned),
        detail_lines: app.config_detail_lines(),
        status_summary: app.config_status_summary(),
        footer_hint: app.config_footer_hint(),
        selected_footer_action: app.config_selected_footer_action_label().map(str::to_owned),
        footer_actions: app
            .config_footer_action_labels()
            .into_iter()
            .map(str::to_owned)
            .collect(),
        save_error: app.config_save_error().map(str::to_owned),
        dirty: app.config_is_dirty(),
        close_guard_armed: app.config_close_guard_armed(),
    })
}

fn status_surface(app: &AppState) -> StatusSurface {
    StatusSurface {
        workspace_label: display_file_label(&app.workspace_root),
        config_label: display_file_label(&app.config_path),
        session_id: app.session_id.clone(),
        provider_name: app.runtime.provider_name.clone(),
        model_name: app.runtime.model_name.clone(),
        permission_mode: app.runtime.permission_mode.to_string(),
        is_busy: app.runtime.is_busy,
        cache_hit_ratio: app.cache_hit_ratio(),
        memory_enabled: app.runtime.memory_enabled,
        memory_document_count: app.runtime.memory_document_count,
        memory_last_status: app.runtime.memory_last_status.clone(),
        compaction_status: app.runtime.compaction_status.clone(),
        active_pane: app.active_pane,
        last_notice: app.last_notice().map(str::to_owned),
    }
}

fn display_file_label(path: &Path) -> String {
    path.file_name()
        .and_then(|value| value.to_str())
        .filter(|value| !value.is_empty())
        .unwrap_or("workspace")
        .chars()
        .take(96)
        .collect()
}

fn modal_surface(app: &AppState) -> ModalSurface {
    ModalSurface {
        visible: app.has_modal(),
        checkpoint_restore_visible: app.checkpoint_restore_modal_open(),
        title: app.modal_title().map(str::to_owned),
        lines: app.modal_lines(),
        input_cursor: app.modal_input_cursor(),
        config_mode: app.is_config_mode(),
        session_modal_actions: app.session_modal_action_rows(),
    }
}

fn slash_selector_surface(app: &AppState) -> SlashSelectorSurface {
    SlashSelectorSurface {
        visible: app.has_slash_selector(),
        title: app.slash_selector_title().map(str::to_owned),
        rows: app.slash_selector_rows(),
        selected_index: app.slash_selector_selected_index(),
        visible_rows: app.slash_selector_visible_rows(),
        empty_message: app.slash_selector_empty_message().map(str::to_owned),
    }
}

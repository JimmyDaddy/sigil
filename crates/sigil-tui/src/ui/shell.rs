#[cfg(test)]
use std::path::Path;

use ratatui::{
    Frame,
    layout::{Alignment, Rect},
    style::{Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, Borders, Clear, Paragraph, Wrap},
};

#[cfg(test)]
use crate::app::{AppState, PaneFocus};
use crate::surface::{PaneFocus as SurfacePaneFocus, SurfaceModel};
use crate::view_model::FooterViewModel;
#[cfg(test)]
use crate::view_model::{LivePanelViewModel, UiViewModel};

#[cfg(test)]
use super::{
    approval::render_approval_modal,
    checkpoint_restore::render_checkpoint_restore_modal,
    egress_disclosure::{egress_disclosure_layout, render_active_egress_disclosure_card},
    intent_stack::render_intent_stack_modal,
    layout_snapshot::{live_transcript_rows_for_app, shell_layout},
    live_panel::render_live_panel_with_theme,
    modal::render_modal,
    setup_config::{render_config, render_setup},
    slash_overlay::render_slash_selector_overlay_with_theme,
};
use super::{
    approval::render_approval_modal_surface,
    checkpoint_restore::render_checkpoint_restore_modal_surface,
    composer::{composer_cursor_position, render_agent_panel_with_theme, render_input_with_theme},
    egress_disclosure::render_active_egress_disclosure_card_surface,
    geometry::inset_rect,
    info_rail::render_info_rail_with_theme,
    intent_stack::render_intent_stack_modal_surface,
    live_panel::render_live_panel_surface,
    modal::render_modal_surface,
    plan_workbench::render_plan_workbench,
    setup_config::{render_config_surface, render_setup_surface},
    slash_overlay::render_slash_selector_overlay_surface,
    text::{terminal_cell_width, truncate_display_width},
    theme::{self, styles},
    user_input_form::render_user_input_form,
};

const FOOTER_GIT_MIN_WIDTH: u16 = 12;
const FOOTER_SECTION_GAP: u16 = 1;

#[cfg(test)]
pub fn render(frame: &mut Frame, app: &AppState) {
    let _phase_timing = crate::phase_timing::PhaseTimer::new("render");
    app.begin_egress_disclosure_frame();
    let theme = theme::resolve_for_app(app);
    if app.is_setup_mode() || app.is_workspace_trust_gate_mode() {
        render_setup(frame, app);
        return;
    }
    if app.is_config_mode() {
        render_config(frame, app);
        let (egress_disclosure, _) = egress_disclosure_layout(frame.area(), app);
        if let Some(area) = egress_disclosure {
            let _ = render_active_egress_disclosure_card(frame, area, app, &theme);
        }
        return;
    }

    // Ratatui retains the current buffer while resizing. Styling the base block alone does not
    // replace its symbols, so an explicit reset prevents old wrapped timeline cells from leaking
    // into the reflowed frame.
    frame.render_widget(Clear, frame.area());
    frame.render_widget(
        Block::default().style(Style::default().bg(theme.palette.surface_base)),
        frame.area(),
    );

    let user_input_takeover = app
        .pending_user_input()
        .is_some_and(|form| form.open && frame.area().height <= 11);
    let plan_takeover = app
        .pending_plan_approval()
        .is_some_and(|pending| pending.workbench_open && frame.area().height <= 11)
        && !user_input_takeover;
    let shell = if plan_takeover || user_input_takeover {
        shell_layout(frame.area(), 0, 0, false)
    } else {
        shell_layout(
            frame.area(),
            app.footer_strip_height(),
            app.composer_height(),
            app.info_rail_visible(),
        )
    };

    let view_model = UiViewModel::from_app(app);
    let (egress_disclosure, live_panel) = egress_disclosure_layout(shell.live_panel, app);
    let live_transcript_rows = live_transcript_rows_for_app(frame.area(), app);
    let live_view_model = LivePanelViewModel::from_app(app, live_transcript_rows);

    if let Some(area) = egress_disclosure
        && !app.pending_user_input().is_some_and(|form| form.open)
        && !app
            .pending_plan_approval()
            .is_some_and(|pending| pending.workbench_open)
    {
        let _ = render_active_egress_disclosure_card(frame, area, app, &theme);
    }
    if let Some(form) = app.pending_user_input().filter(|form| form.open) {
        render_user_input_form(frame, shell.live_panel, form, &theme);
    } else if let Some(pending) = app
        .pending_plan_approval()
        .filter(|pending| pending.workbench_open)
    {
        render_plan_workbench(frame, shell.live_panel, pending, &theme);
    } else {
        render_live_panel_with_theme(frame, live_panel, &live_view_model, &theme);
    }
    if !plan_takeover && !user_input_takeover {
        render_input_with_theme(frame, shell.composer, &view_model.composer, &theme);
        render_agent_panel_with_theme(frame, shell.agent_panel, &view_model.composer, &theme);
    }
    render_footer_status(frame, shell.footer, &view_model.footer, &theme);
    if !plan_takeover && !user_input_takeover {
        render_slash_selector_overlay_with_theme(frame, live_panel, shell.composer, app, &theme);
    }
    if shell.info_rail.width > 0 {
        render_info_rail_with_theme(frame, shell.info_rail, &view_model.info_rail, &theme);
    }

    if app.approval.has_actionable_pending() {
        render_approval_modal(frame, app);
    }

    if app.active_pane == PaneFocus::Composer
        && !app.has_modal()
        && !app.pending_user_input().is_some_and(|form| form.open)
        && !app
            .pending_plan_approval()
            .is_some_and(|pending| pending.workbench_open)
        && !app.is_composer_queue_panel_focused()
        && !app.is_composer_agent_panel_focused()
        && let Some(cursor_position) =
            composer_cursor_position(shell.composer, &view_model.composer)
    {
        frame.set_cursor_position(cursor_position);
    }

    if app.checkpoint_restore_modal_open() {
        render_checkpoint_restore_modal(frame, app);
    } else if app.intent_stack_modal_open() {
        render_intent_stack_modal(frame, app);
    } else {
        render_modal(frame, app);
    }
}

/// Render one immutable, owned application surface snapshot.
///
/// The adapter constructs `SurfaceModel` before the draw callback.  No renderer below this
/// function can observe or query the mutable application state, which keeps the committed frame
/// and its interaction geometry from diverging.
pub(crate) fn render_surface(frame: &mut Frame, surface: &SurfaceModel) {
    let _phase_timing = crate::phase_timing::PhaseTimer::new("render");
    let _surface_state = surface.state;
    if surface.is_setup() {
        render_setup_surface(frame, surface);
        return;
    }
    if surface.is_config() {
        render_config_surface(frame, surface);
        return;
    }

    let theme = &surface.theme;
    frame.render_widget(Clear, frame.area());
    frame.render_widget(
        Block::default().style(Style::default().bg(theme.palette.surface_base)),
        frame.area(),
    );

    let layout = &surface.layout;
    let live_area = layout
        .egress_disclosure
        .map(|disclosure| {
            Rect::new(
                layout.live_panel.x,
                layout.live_panel.y.saturating_add(disclosure.height),
                layout.live_panel.width,
                layout.live_panel.height.saturating_sub(disclosure.height),
            )
        })
        .unwrap_or(layout.live_panel);
    if let Some(area) = layout.egress_disclosure
        && !surface.user_input_open
        && !surface.plan_workbench_open
    {
        let _ = render_active_egress_disclosure_card_surface(
            frame,
            area,
            surface.egress_disclosure.as_ref(),
            theme,
        );
    }

    if let Some(form) = surface.pending_user_input.as_ref().filter(|form| form.open) {
        render_user_input_form(frame, live_area, form, theme);
    } else if let Some(pending) = surface
        .pending_plan_approval
        .as_ref()
        .filter(|pending| pending.workbench_open)
    {
        render_plan_workbench(frame, live_area, pending, theme);
    } else {
        render_live_panel_surface(
            frame,
            live_area,
            &surface.live_panel,
            &surface.transcript,
            theme,
        );
    }

    if !surface.main_overlays_hidden() {
        render_input_with_theme(frame, layout.composer, &surface.composer, theme);
        render_agent_panel_with_theme(frame, layout.agent_panel, &surface.composer, theme);
    }
    render_footer_status(frame, layout.footer, &surface.footer, theme);
    if !surface.main_overlays_hidden() {
        render_slash_selector_overlay_surface(
            frame,
            live_area,
            layout.composer,
            &surface.slash_selector,
            theme,
        );
    }
    if layout.info_rail.width > 0 {
        render_info_rail_with_theme(frame, layout.info_rail, &surface.info_rail, theme);
    }

    if surface.approval.is_some() {
        render_approval_modal_surface(
            frame,
            surface.approval.as_ref(),
            surface.approval_scroll_back,
            theme,
        );
    }

    if matches!(surface.status.active_pane, SurfacePaneFocus::Composer)
        && !surface.modal.visible
        && !surface.user_input_open
        && !surface.plan_workbench_open
        && !surface.composer_auxiliary_panel_focused
        && let Some(cursor_position) = composer_cursor_position(layout.composer, &surface.composer)
    {
        frame.set_cursor_position(cursor_position);
    }

    if let Some(view) = surface.checkpoint_restore.as_ref() {
        render_checkpoint_restore_modal_surface(frame, view, theme);
    } else if let Some(view) = surface.intent_stack.as_ref() {
        render_intent_stack_modal_surface(frame, view, theme);
    } else {
        render_modal_surface(frame, &surface.modal, theme);
    }
}

fn render_footer_status(
    frame: &mut Frame,
    area: Rect,
    footer: &FooterViewModel,
    theme: &theme::Theme,
) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let _ = (
        &footer.phase,
        footer.is_busy,
        &footer.run_label,
        &footer.hints,
    );
    frame.render_widget(Block::default().style(styles::body(&theme.palette)), area);
    let inner = inset_rect(area, 2, 0);
    if inner.width == 0 || inner.height == 0 {
        return;
    }
    let context_width = footer_context_width(footer, inner.width);
    let workspace_width = inner
        .width
        .saturating_sub(context_width)
        .saturating_sub(u16::from(context_width > 0));
    if !footer.workspace_git_label.is_empty() && workspace_width >= 12 {
        let git_label = truncate_display_width(
            &footer.workspace_git_label,
            workspace_width.saturating_sub(4) as usize,
        );
        frame.render_widget(
            Paragraph::new(Text::from(vec![Line::from(vec![
                Span::styled(
                    "git ",
                    Style::default()
                        .fg(theme.palette.accent_info)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(git_label, styles::muted(&theme.palette)),
            ])]))
            .style(Style::default().bg(theme.palette.surface_base))
            .wrap(Wrap { trim: false }),
            Rect::new(inner.x, inner.y, workspace_width, inner.height),
        );
    }
    if context_width > 0 {
        let context_area = Rect::new(
            inner.x + inner.width.saturating_sub(context_width),
            inner.y,
            context_width,
            inner.height,
        );
        let context = truncate_display_width(&footer.context_label, context_width as usize);
        frame.render_widget(
            Paragraph::new(Text::from(vec![Line::from(vec![Span::styled(
                context,
                styles::muted(&theme.palette),
            )])]))
            .style(Style::default().bg(theme.palette.surface_base))
            .alignment(Alignment::Right)
            .wrap(Wrap { trim: false }),
            context_area,
        );
    }
}

fn footer_context_width(footer: &FooterViewModel, available_width: u16) -> u16 {
    if footer.context_label.is_empty() || available_width < 24 {
        return 0;
    }
    let preferred = terminal_cell_width(&footer.context_label) as u16;
    let reserved_workspace_width = if footer.workspace_git_label.is_empty() {
        0
    } else {
        FOOTER_GIT_MIN_WIDTH + FOOTER_SECTION_GAP
    };

    preferred.min(available_width.saturating_sub(reserved_workspace_width))
}

#[cfg(test)]
pub(super) fn render_status(frame: &mut Frame, area: Rect, app: &AppState) {
    let current_theme = theme::resolve_for_app(app);
    let palette = &current_theme.palette;
    if app.is_setup_mode() || app.is_workspace_trust_gate_mode() {
        let (mode_title, mode_subtitle, default_notice) = if app.is_workspace_trust_gate_mode() {
            (
                " Workspace trust ",
                " review workspace ",
                "press Enter to trust, Ctrl-C to quit",
            )
        } else {
            (
                " Sigil setup ",
                " quick setup ",
                "trust folder, set auth, save",
            )
        };
        let title = Line::from(vec![
            Span::styled(
                mode_title,
                Style::default()
                    .fg(palette.button_selected_fg)
                    .bg(palette.button_selected_bg)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(mode_subtitle),
        ]);
        let secondary = Line::from(vec![Span::raw(format!(
            "ws={}  cfg={}",
            short_path_label(&app.workspace_root),
            short_path_label(&app.config_path)
        ))]);
        let tertiary = Line::from(vec![Span::styled(
            app.last_notice().unwrap_or(default_notice),
            Style::default().fg(palette.config_warning),
        )]);
        let paragraph = Paragraph::new(Text::from(vec![title, secondary, tertiary]))
            .block(Block::default().title("Status").borders(Borders::ALL))
            .wrap(Wrap { trim: false });
        frame.render_widget(paragraph, area);
        return;
    }
    let title = Line::from(vec![
        Span::styled(
            " Sigil TUI ",
            Style::default()
                .fg(palette.button_selected_fg)
                .bg(palette.button_selected_bg)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(format!(
            " {}/{}  mode={}  {} ",
            app.runtime.provider_name,
            app.runtime.model_name,
            app.runtime.permission_mode,
            if app.runtime.is_busy {
                "running"
            } else {
                "idle"
            }
        )),
    ]);

    let secondary = Line::from(vec![Span::raw(format!(
        "ws={}  sid={}  pane={}  cache={:.0}%  mem={}  compact={}",
        short_path_label(&app.workspace_root),
        short_session_id(&app.session_id),
        short_pane_label(app),
        app.cache_hit_ratio() * 100.0,
        memory_badge(app),
        app.runtime.compaction_status,
    ))]);

    let tertiary = Line::from(vec![Span::styled(
        app.last_notice().unwrap_or("ready"),
        Style::default().fg(palette.config_warning),
    )]);

    let paragraph = Paragraph::new(Text::from(vec![title, secondary, tertiary]))
        .block(Block::default().title("Status").borders(Borders::ALL))
        .wrap(Wrap { trim: false });

    frame.render_widget(paragraph, area);
}

pub(super) fn render_status_surface(
    frame: &mut Frame,
    area: Rect,
    status: &crate::surface::StatusSurface,
    theme: &theme::Theme,
    setup: Option<&crate::surface::SetupSurface>,
) {
    let palette = &theme.palette;
    if let Some(setup) = setup {
        let (mode_title, mode_subtitle, default_notice) = if setup.trust_gate {
            (
                " Workspace trust ",
                " review workspace ",
                "press Enter to trust, Ctrl-C to quit",
            )
        } else {
            (
                " Sigil setup ",
                " quick setup ",
                "trust folder, set auth, save",
            )
        };
        let title = Line::from(vec![
            Span::styled(
                mode_title,
                Style::default()
                    .fg(palette.button_selected_fg)
                    .bg(palette.button_selected_bg)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(mode_subtitle),
        ]);
        let secondary = Line::from(vec![Span::raw(format!(
            "ws={}  cfg={}",
            status.workspace_label, status.config_label
        ))]);
        let tertiary = Line::from(vec![Span::styled(
            status.last_notice.as_deref().unwrap_or(default_notice),
            Style::default().fg(palette.config_warning),
        )]);
        frame.render_widget(
            Paragraph::new(Text::from(vec![title, secondary, tertiary]))
                .block(Block::default().title("Status").borders(Borders::ALL))
                .wrap(Wrap { trim: false }),
            area,
        );
        return;
    }
    let title = Line::from(vec![
        Span::styled(
            " Sigil TUI ",
            Style::default()
                .fg(palette.button_selected_fg)
                .bg(palette.button_selected_bg)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(format!(
            " {}/{}  mode={}  {} ",
            status.provider_name,
            status.model_name,
            status.permission_mode,
            if status.is_busy { "running" } else { "idle" }
        )),
    ]);
    let secondary = Line::from(vec![Span::raw(format!(
        "ws={}  sid={}  pane={}  cache={:.0}%  mem={}  compact={}",
        status.workspace_label,
        short_session_id(&status.session_id),
        short_pane_label_surface(status.active_pane),
        status.cache_hit_ratio * 100.0,
        memory_badge_surface(status),
        status.compaction_status,
    ))]);
    let tertiary = Line::from(vec![Span::styled(
        status.last_notice.as_deref().unwrap_or("ready"),
        Style::default().fg(palette.config_warning),
    )]);
    frame.render_widget(
        Paragraph::new(Text::from(vec![title, secondary, tertiary]))
            .block(Block::default().title("Status").borders(Borders::ALL))
            .wrap(Wrap { trim: false }),
        area,
    );
}

#[cfg(test)]
fn short_path_label(path: &Path) -> String {
    path.file_name()
        .and_then(|value| value.to_str())
        .unwrap_or_else(|| path.to_str().unwrap_or("?"))
        .to_owned()
}

fn short_session_id(session_id: &str) -> String {
    session_id.chars().take(8).collect()
}

#[cfg(test)]
fn short_pane_label(app: &AppState) -> &'static str {
    match app.active_pane {
        PaneFocus::Composer => "composer",
        PaneFocus::Activity => "activity",
    }
}

#[cfg(test)]
fn memory_badge(app: &AppState) -> String {
    if !app.runtime.memory_enabled {
        return "off".to_owned();
    }
    if app.runtime.memory_last_status == "ok" {
        format!("{}/ok", app.runtime.memory_document_count)
    } else {
        format!("{}/err", app.runtime.memory_document_count)
    }
}

fn short_pane_label_surface(pane: SurfacePaneFocus) -> &'static str {
    match pane {
        SurfacePaneFocus::Composer => "composer",
        SurfacePaneFocus::Activity => "activity",
    }
}

fn memory_badge_surface(status: &crate::surface::StatusSurface) -> String {
    if !status.memory_enabled {
        return "off".to_owned();
    }
    if status.memory_last_status == "ok" {
        format!("{}/ok", status.memory_document_count)
    } else {
        format!("{}/err", status.memory_document_count)
    }
}

#[cfg(all(test, not(sigil_tui_test_slice_app_input_flow)))]
#[path = "tests/shell_tests.rs"]
mod tests;

use std::path::Path;

use ratatui::{
    Frame,
    layout::{Alignment, Rect},
    style::{Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, Borders, Paragraph, Wrap},
};
use unicode_width::UnicodeWidthStr;

use crate::app::{AppState, PaneFocus};
use crate::view_model::{FooterViewModel, LivePanelViewModel, UiViewModel};

use super::{
    approval::render_approval_modal,
    composer::{composer_cursor_origin, render_agent_panel_with_theme, render_input_with_theme},
    geometry::inset_rect,
    info_rail::render_info_rail_with_theme,
    layout_snapshot::shell_layout,
    live_panel::{
        LIVE_PANEL_BOTTOM_PADDING, live_status_rows_for_app, render_live_panel_with_theme,
    },
    modal::render_modal,
    setup_config::{render_config, render_setup},
    slash_overlay::render_slash_selector_overlay_with_theme,
    text::truncate_display_width,
    theme::{self, styles},
};

pub fn render(frame: &mut Frame, app: &AppState) {
    let theme = theme::resolve_for_app(app);
    if app.is_setup_mode() || app.is_workspace_trust_gate_mode() {
        render_setup(frame, app);
        return;
    }
    if app.is_config_mode() {
        render_config(frame, app);
        return;
    }

    frame.render_widget(
        Block::default().style(Style::default().bg(theme.palette.surface_base)),
        frame.area(),
    );

    let shell = shell_layout(
        frame.area(),
        app.footer_strip_height(),
        app.composer_height(),
    );

    let view_model = UiViewModel::from_app(app);
    let live_inner = inset_rect(shell.live_panel, 1, 0);
    let live_transcript_rows = live_inner
        .height
        .saturating_sub(LIVE_PANEL_BOTTOM_PADDING)
        .saturating_sub(live_status_rows_for_app(app))
        .max(1) as usize;
    let live_view_model = LivePanelViewModel::from_app(app, live_transcript_rows);

    render_live_panel_with_theme(frame, shell.live_panel, &live_view_model, &theme);
    render_input_with_theme(frame, shell.composer, &view_model.composer, &theme);
    render_agent_panel_with_theme(frame, shell.agent_panel, &view_model.composer, &theme);
    render_footer_status(frame, shell.footer, &view_model.footer, &theme);
    render_slash_selector_overlay_with_theme(frame, shell.live_panel, shell.composer, app, &theme);
    if shell.info_rail.width > 0 {
        render_info_rail_with_theme(frame, shell.info_rail, &view_model.info_rail, &theme);
    }

    if app.pending_approval.is_some() {
        render_approval_modal(frame, app);
    }

    if app.active_pane == PaneFocus::Composer
        && !app.has_modal()
        && !app.is_composer_queue_panel_focused()
        && !app.is_composer_agent_panel_focused()
    {
        let (cursor_col, _) = view_model.composer.cursor_position;
        if let Some((cursor_x, cursor_y)) =
            composer_cursor_origin(shell.composer, &view_model.composer)
        {
            frame.set_cursor_position((cursor_x.saturating_add(cursor_col), cursor_y));
        }
    }

    render_modal(frame, app);
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
    let preferred = UnicodeWidthStr::width(footer.context_label.as_str()) as u16;
    preferred.min(available_width / 2).min(42)
}

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
            " {}/{}  write={}  {} ",
            app.provider_name,
            app.model_name,
            app.permission_default_mode,
            if app.is_busy { "running" } else { "idle" }
        )),
    ]);

    let secondary = Line::from(vec![Span::raw(format!(
        "ws={}  sid={}  pane={}  cache={:.0}%  mem={}  compact={}",
        short_path_label(&app.workspace_root),
        short_session_id(&app.session_id),
        short_pane_label(app),
        app.cache_hit_ratio() * 100.0,
        memory_badge(app),
        app.compaction_status,
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

fn short_path_label(path: &Path) -> String {
    path.file_name()
        .and_then(|value| value.to_str())
        .unwrap_or_else(|| path.to_str().unwrap_or("?"))
        .to_owned()
}

fn short_session_id(session_id: &str) -> String {
    session_id.chars().take(8).collect()
}

fn short_pane_label(app: &AppState) -> &'static str {
    match app.active_pane {
        PaneFocus::Composer => "composer",
        PaneFocus::Activity => "activity",
    }
}

fn memory_badge(app: &AppState) -> String {
    if !app.memory_enabled {
        return "off".to_owned();
    }
    if app.memory_last_status == "ok" {
        format!("{}/ok", app.memory_document_count)
    } else {
        format!("{}/err", app.memory_document_count)
    }
}

#[cfg(all(test, not(sigil_tui_test_slice_app_input_flow)))]
#[path = "tests/shell_tests.rs"]
mod tests;

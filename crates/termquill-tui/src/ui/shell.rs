use std::path::Path;

use ratatui::{
    Frame,
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, Borders, Paragraph, Wrap},
};

use crate::app::{AppState, PaneFocus};
use crate::view_model::{LivePanelViewModel, UiViewModel};

use super::{
    approval::render_approval_modal,
    composer::{composer_cursor_origin, render_input},
    geometry::{inset_rect, sidebar_width_for_terminal},
    info_rail::render_info_rail,
    live_panel::render_live_panel,
    modal::render_modal,
    setup_config::{render_config, render_setup},
    slash_overlay::render_slash_selector_overlay,
    text::truncate_display_width,
    theme::{muted, shell_bg},
};

pub fn render(frame: &mut Frame, app: &AppState) {
    if app.is_setup_mode() {
        render_setup(frame, app);
        return;
    }
    if app.is_config_mode() {
        render_config(frame, app);
        return;
    }

    frame.render_widget(
        Block::default().style(Style::default().bg(shell_bg())),
        frame.area(),
    );

    let sidebar_width = sidebar_width_for_terminal(frame.area().width as usize) as u16;
    let shell = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Min(10), Constraint::Length(sidebar_width)])
        .split(frame.area());

    let footer_height = app.footer_strip_height();
    let main = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(1),
            Constraint::Length(footer_height),
            Constraint::Length(1),
        ])
        .split(shell[0]);

    let view_model = UiViewModel::from_app(app);
    let live_inner = inset_rect(main[0], 1, 0);
    let live_transcript_rows = live_inner
        .height
        .saturating_sub(u16::from(app.live_activity_summary().is_some()))
        .max(1) as usize;
    let live_view_model = LivePanelViewModel::from_app(app, live_transcript_rows);

    render_live_panel(frame, main[0], &live_view_model);
    render_input(frame, main[1], &view_model.composer);
    render_footer_status(frame, main[2], app);
    render_slash_selector_overlay(frame, main[0], main[1], app);
    render_info_rail(frame, shell[1], &view_model.info_rail);

    if app.pending_approval.is_some() {
        render_approval_modal(frame, app);
    }

    if app.active_pane == PaneFocus::Composer && !app.has_modal() {
        let (cursor_col, cursor_row) = view_model.composer.cursor_position;
        if let Some((cursor_x, cursor_y)) = composer_cursor_origin(main[1], &view_model.composer) {
            frame.set_cursor_position((
                cursor_x.saturating_add(cursor_col),
                cursor_y.saturating_add(cursor_row),
            ));
        }
    }

    render_modal(frame, app);
}

fn render_footer_status(frame: &mut Frame, area: Rect, app: &AppState) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    frame.render_widget(
        Block::default().style(Style::default().bg(shell_bg())),
        area,
    );
    let inner = inset_rect(area, 2, 0);
    if inner.width == 0 || inner.height == 0 {
        return;
    }
    let line = truncate_display_width(&app.footer_status_line(), inner.width as usize);
    frame.render_widget(
        Paragraph::new(Text::from(vec![Line::from(vec![Span::styled(
            line,
            Style::default().fg(muted()),
        )])]))
        .style(Style::default().bg(shell_bg()))
        .alignment(Alignment::Right)
        .wrap(Wrap { trim: false }),
        inner,
    );
}

pub(super) fn render_status(frame: &mut Frame, area: Rect, app: &AppState) {
    if app.is_setup_mode() {
        let title = Line::from(vec![
            Span::styled(
                " Termquill setup ",
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(" quick setup "),
        ]);
        let secondary = Line::from(vec![Span::raw(format!(
            "ws={}  cfg={}",
            short_path_label(&app.workspace_root),
            short_path_label(&app.config_path)
        ))]);
        let tertiary = Line::from(vec![Span::styled(
            app.last_notice().unwrap_or("trust folder, set auth, save"),
            Style::default().fg(Color::Yellow),
        )]);
        let paragraph = Paragraph::new(Text::from(vec![title, secondary, tertiary]))
            .block(Block::default().title("Status").borders(Borders::ALL))
            .wrap(Wrap { trim: false });
        frame.render_widget(paragraph, area);
        return;
    }
    if app.is_config_mode() {
        let title = Line::from(vec![
            Span::styled(
                " Termquill config ",
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::Green)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(" config "),
        ]);
        let secondary = Line::from(vec![Span::raw(format!(
            "step={}  field={}  dirty={}  cfg={}",
            app.config_section_title().unwrap_or("summary"),
            app.config_selected_field_label().unwrap_or("<none>"),
            if app.config_is_dirty() { "yes" } else { "no" },
            short_path_label(&app.config_path)
        ))]);
        let tertiary = Line::from(vec![Span::styled(
            app.last_notice()
                .unwrap_or("Tab step  Up/Down field  Down footer  Enter open"),
            Style::default().fg(Color::Yellow),
        )]);
        let paragraph = Paragraph::new(Text::from(vec![title, secondary, tertiary]))
            .block(
                Block::default()
                    .title("Status")
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(Color::Green)),
            )
            .wrap(Wrap { trim: false });
        frame.render_widget(paragraph, area);
        return;
    }

    let title = Line::from(vec![
        Span::styled(
            " Termquill TUI ",
            Style::default()
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(format!(
            " {}/{}  write={}  {} ",
            app.provider_name,
            app.model_name,
            app.permission_write_mode,
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
        Style::default().fg(Color::Yellow),
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

#[cfg(test)]
#[path = "tests/shell_tests.rs"]
mod tests;

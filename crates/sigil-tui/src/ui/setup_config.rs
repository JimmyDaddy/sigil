use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, BorderType, Borders, Paragraph, Wrap},
};

use crate::app::AppState;
use crate::config_panel::{CONFIG_HEADER_NOTICE, ConfigSection};

use super::{
    StatusKind,
    modal::render_modal,
    shell::render_status,
    status_indicator::{
        FocusKind, StatusIndicator, focus_style_with_palette, focus_symbol,
        status_rest_style_with_palette,
    },
    theme::{self, ThemePalette},
};

pub(super) const CONFIG_DETAIL_SPLIT_MIN_WIDTH: u16 = 128;
pub(super) const CONFIG_DETAIL_PANEL_WIDTH: u16 = 42;
const CONFIG_CONTENT_MIN_WIDTH: u16 = 72;
const CONFIG_CONTENT_MAX_WIDTH: u16 = 180;
const CONFIG_SIDE_MARGIN_PERCENT: u16 = 8;
const CONFIG_FORM_LABEL_WIDTH: usize = 16;
const CONFIG_FORM_ACTION_CHIP_WIDTH: usize = 14;
const CONFIG_FOOTER_BUTTON_WIDTH: usize = 14;
pub(super) const CONFIG_FOOTER_COMPACT_WIDTH: u16 = 76;
const CONFIG_SCROLL_MARKER_WIDTH: u16 = 8;
const CONFIG_STATUS_MARKER_WIDTH: usize = 2;

pub(super) fn render_setup(frame: &mut Frame, app: &AppState) {
    let current_theme = theme::resolve_for_app(app);
    let palette = &current_theme.palette;
    let panel_bg = palette.setup_bg;
    let outer = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(4), Constraint::Min(12)])
        .split(frame.area());
    render_status(frame, outer[0], app);

    let body = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage(10),
            Constraint::Min(68),
            Constraint::Percentage(10),
        ])
        .split(outer[1]);

    let (title, lines) = if app.is_workspace_trust_gate_mode() {
        ("Workspace Trust", app.workspace_trust_gate_lines())
    } else {
        ("Setup", app.setup_lines())
    };
    let detail = lines
        .into_iter()
        .map(|line| render_setup_line_with_palette(&line, palette))
        .collect::<Vec<_>>();

    let detail_widget = Paragraph::new(Text::from(detail))
        .block(
            Block::default()
                .title(title)
                .title_style(
                    Style::default()
                        .fg(palette.button_selected_fg)
                        .bg(palette.button_selected_bg)
                        .add_modifier(Modifier::BOLD),
                )
                .border_type(BorderType::Rounded)
                .borders(Borders::ALL)
                .border_style(Style::default().fg(palette.config_warning))
                .style(Style::default().bg(panel_bg)),
        )
        .style(Style::default().bg(panel_bg))
        .wrap(Wrap { trim: false });

    frame.render_widget(detail_widget, body[1]);
    render_modal(frame, app);
}

pub(super) fn render_config(frame: &mut Frame, app: &AppState) {
    let current_theme = theme::resolve_for_app(app);
    let palette = &current_theme.palette;
    let panel_bg = palette.config_bg;
    frame.render_widget(
        Block::default().style(Style::default().bg(panel_bg)),
        frame.area(),
    );
    let outer = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(3), Constraint::Min(1)])
        .split(frame.area());
    let header_area = centered_config_area(outer[0]);
    let content_area = centered_config_area(outer[1]);
    render_config_header(frame, header_area, app, panel_bg, palette);

    let (mut main_lines, context_lines) = split_config_context_lines(app.config_detail_lines());
    let show_context_panel = content_area.width >= CONFIG_DETAIL_SPLIT_MIN_WIDTH;
    if !show_context_panel && !context_lines.is_empty() {
        main_lines.push(String::new());
        main_lines.push("[details]".to_owned());
        main_lines.extend(context_lines.iter().cloned());
    }
    let footer_height = u16::from(content_area.height > 0);
    let footer_gap = u16::from(content_area.height > footer_height + 1);
    let panel_max_height = content_area
        .height
        .saturating_sub(footer_height)
        .saturating_sub(footer_gap);
    let panel_height = if show_context_panel {
        config_panel_height(&main_lines, &context_lines, panel_max_height)
    } else {
        config_panel_height(&main_lines, &[], panel_max_height)
    };
    let panel_area = top_aligned_config_area(content_area, panel_height);

    if show_context_panel {
        let content = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([
                Constraint::Min(72),
                Constraint::Length(2),
                Constraint::Length(CONFIG_DETAIL_PANEL_WIDTH),
            ])
            .split(panel_area);
        render_config_panel(frame, content[0], main_lines, panel_bg, palette);
        render_config_context_panel(frame, content[2], app, context_lines, panel_bg, palette);
    } else {
        render_config_panel(frame, panel_area, main_lines, panel_bg, palette);
    }
    if footer_height > 0 {
        let footer_area = Rect {
            y: panel_area.y + panel_area.height + footer_gap,
            height: footer_height,
            ..content_area
        };
        render_config_footer(frame, footer_area, app, panel_bg, palette);
    }
    render_modal(frame, app);
}

pub(super) fn centered_config_area(area: Rect) -> Rect {
    let min_width = CONFIG_CONTENT_MIN_WIDTH.min(area.width);
    let max_width = CONFIG_CONTENT_MAX_WIDTH.min(area.width);
    let side_margin = area.width.saturating_mul(CONFIG_SIDE_MARGIN_PERCENT) / 100;
    let width_without_margins = area.width.saturating_sub(side_margin.saturating_mul(2));
    let width = width_without_margins.max(min_width).min(max_width);
    let x = area.x + area.width.saturating_sub(width) / 2;
    Rect { x, width, ..area }
}

fn render_config_header(
    frame: &mut Frame,
    area: Rect,
    app: &AppState,
    panel_bg: Color,
    palette: &ThemePalette,
) {
    let content_width = area.width as usize;
    let section = app.config_section_title().unwrap_or("Config");
    let (state, state_kind) = if app.config_is_dirty() {
        ("unsaved", StatusKind::Warning)
    } else {
        ("saved", StatusKind::Success)
    };
    let state_style = if app.config_is_dirty() {
        Style::default()
            .fg(palette.button_selected_fg)
            .bg(palette.config_warning)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default()
            .fg(palette.button_selected_fg)
            .bg(palette.config_primary)
            .add_modifier(Modifier::BOLD)
    };
    let field = app.config_selected_field_label().unwrap_or("summary");
    let file_label = config_file_label(app);
    let notice = app.last_notice().unwrap_or(CONFIG_HEADER_NOTICE);
    let title = " Sigil config ";
    let mut summary_spans = Vec::new();
    let mut remaining = content_width;
    push_config_header_span(
        &mut summary_spans,
        &mut remaining,
        title,
        Style::default()
            .fg(palette.button_selected_fg)
            .bg(palette.button_selected_bg)
            .add_modifier(Modifier::BOLD),
    );
    push_config_header_gap(&mut summary_spans, &mut remaining);
    push_config_header_span(
        &mut summary_spans,
        &mut remaining,
        section,
        Style::default()
            .fg(palette.config_detail)
            .add_modifier(Modifier::BOLD),
    );
    push_config_header_gap(&mut summary_spans, &mut remaining);
    push_config_header_span(
        &mut summary_spans,
        &mut remaining,
        &format!(
            " {} {state} ",
            StatusIndicator::static_kind(state_kind).symbol()
        ),
        state_style,
    );
    push_config_header_pair_with_palette(
        &mut summary_spans,
        &mut remaining,
        "field",
        field,
        palette,
    );

    let notice_min_width = "hint ".chars().count() + 12;
    let file_value_width = content_width
        .saturating_sub("file ".chars().count() + 2 + notice_min_width)
        .min(content_width / 3);
    let file_value = fit_config_value(&file_label, file_value_width);
    let notice_width = content_width.saturating_sub(
        "file ".chars().count() + file_value.chars().count() + "  ".chars().count(),
    );
    let mut file_spans = vec![
        Span::styled("file ", Style::default().fg(palette.text_muted)),
        Span::styled(file_value, Style::default().fg(palette.text_secondary)),
        Span::raw("  "),
    ];
    file_spans.extend(render_config_header_notice_with_palette(
        notice,
        notice_width,
        palette,
    ));
    let lines = vec![Line::from(summary_spans), Line::from(file_spans)];
    let header = Paragraph::new(Text::from(lines))
        .style(Style::default().bg(panel_bg))
        .wrap(Wrap { trim: false });
    frame.render_widget(header, area);
}

fn config_file_label(app: &AppState) -> String {
    app.config_path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("config")
        .to_owned()
}

#[allow(dead_code)]
fn render_config_header_notice(notice: &str, width: usize) -> Vec<Span<'static>> {
    let palette = theme::default_palette();
    render_config_header_notice_with_palette(notice, width, &palette)
}

fn render_config_header_notice_with_palette(
    notice: &str,
    width: usize,
    palette: &ThemePalette,
) -> Vec<Span<'static>> {
    if width == 0 {
        return Vec::new();
    }
    let (marker, value_style) = if notice == CONFIG_HEADER_NOTICE {
        ("hint ", Style::default().fg(palette.text_secondary))
    } else {
        (
            "note ",
            Style::default()
                .fg(palette.config_warning)
                .add_modifier(Modifier::BOLD),
        )
    };
    let marker_width = marker.chars().count();
    let marker_style = Style::default()
        .fg(palette.text_muted)
        .bg(palette.config_tab_bg)
        .add_modifier(Modifier::BOLD);
    if width <= marker_width {
        return vec![Span::styled(fit_config_value(marker, width), marker_style)];
    }
    vec![
        Span::styled(marker, marker_style),
        Span::styled(
            fit_config_value(notice, width.saturating_sub(marker_width)),
            value_style,
        ),
    ]
}

fn push_config_header_gap(spans: &mut Vec<Span<'static>>, remaining: &mut usize) -> bool {
    push_config_header_span(spans, remaining, "  ", Style::default())
}

#[allow(dead_code)]
fn push_config_header_pair(
    spans: &mut Vec<Span<'static>>,
    remaining: &mut usize,
    label: &str,
    value: &str,
) {
    let palette = theme::default_palette();
    push_config_header_pair_with_palette(spans, remaining, label, value, &palette);
}

fn push_config_header_pair_with_palette(
    spans: &mut Vec<Span<'static>>,
    remaining: &mut usize,
    label: &str,
    value: &str,
    palette: &ThemePalette,
) {
    if !push_config_header_gap(spans, remaining) {
        return;
    }
    let label = format!("{label}: ");
    if *remaining <= label.chars().count() {
        push_config_header_span(
            spans,
            remaining,
            &label,
            Style::default().fg(palette.text_muted),
        );
        return;
    }
    push_config_header_span(
        spans,
        remaining,
        &label,
        Style::default().fg(palette.text_muted),
    );
    push_config_header_span(
        spans,
        remaining,
        value,
        Style::default().fg(palette.text_primary),
    );
}

fn push_config_header_span(
    spans: &mut Vec<Span<'static>>,
    remaining: &mut usize,
    text: &str,
    style: Style,
) -> bool {
    if *remaining == 0 {
        return false;
    }
    let text = fit_config_value(text, *remaining);
    let width = text.chars().count();
    if width == 0 {
        return false;
    }
    spans.push(Span::styled(text, style));
    *remaining = remaining.saturating_sub(width);
    true
}

pub(super) fn config_panel_height(
    main_lines: &[String],
    context_lines: &[String],
    max_height: u16,
) -> u16 {
    let content_rows = main_lines.len().max(context_lines.len()).max(8) as u16;
    let minimum = 10.min(max_height);
    content_rows.saturating_add(2).min(max_height).max(minimum)
}

pub(super) fn top_aligned_config_area(area: Rect, height: u16) -> Rect {
    Rect {
        height: height.min(area.height),
        ..area
    }
}

fn render_config_panel(
    frame: &mut Frame,
    area: Rect,
    lines: Vec<String>,
    panel_bg: Color,
    palette: &ThemePalette,
) {
    let details_index = lines.iter().position(|line| line == "[details]");
    let selected_line_indexes = lines
        .iter()
        .enumerate()
        .filter_map(|(index, line)| {
            (line.starts_with("> ") || line.starts_with("selected:")).then_some(index)
        })
        .collect::<Vec<_>>();
    let block = config_block_with_palette("Config", palette.config_primary, panel_bg, palette);
    let content_area = block.inner(area);
    let content_width = content_area.width as usize;
    let detail = lines
        .into_iter()
        .enumerate()
        .map(|(index, line)| {
            if details_index == Some(index) {
                render_config_subsection_line_with_palette(
                    &line,
                    palette.config_detail,
                    content_width,
                    palette,
                )
            } else if details_index.is_some_and(|details_index| index > details_index) {
                render_config_context_line_with_palette(&line, content_width, palette)
            } else {
                render_config_line_with_palette(index, &line, content_width, palette)
            }
        })
        .collect::<Vec<_>>();
    let scroll_offset =
        config_scroll_offset(detail.len(), content_area.height, &selected_line_indexes);
    frame.render_widget(block, area);
    render_config_scroll_markers(
        frame,
        area,
        detail.len(),
        content_area.height,
        scroll_offset,
        palette,
    );
    render_config_selected_row_bgs(
        frame,
        content_area,
        &selected_line_indexes,
        scroll_offset,
        palette,
    );

    let widget =
        Paragraph::new(Text::from(detail)).scroll((scroll_offset.min(u16::MAX as usize) as u16, 0));

    frame.render_widget(widget, content_area);
}

fn render_config_selected_row_bgs(
    frame: &mut Frame,
    content_area: Rect,
    selected_line_indexes: &[usize],
    scroll_offset: usize,
    palette: &ThemePalette,
) {
    for selected_line_index in selected_line_indexes.iter().copied() {
        let Some(visible_index) = selected_line_index.checked_sub(scroll_offset) else {
            continue;
        };
        let row_offset = visible_index.min(u16::MAX as usize) as u16;
        if row_offset >= content_area.height {
            continue;
        }
        let row_area = Rect {
            y: content_area.y + row_offset,
            height: 1,
            ..content_area
        };
        frame.render_widget(
            Block::default().style(Style::default().bg(palette.config_selected_bg)),
            row_area,
        );
    }
}

fn render_config_context_panel(
    frame: &mut Frame,
    area: Rect,
    app: &AppState,
    context_lines: Vec<String>,
    panel_bg: Color,
    palette: &ThemePalette,
) {
    let lines = if context_lines.is_empty() {
        vec![
            app.config_status_summary(),
            String::new(),
            "Move to a field to inspect its key and behavior.".to_owned(),
            app.config_footer_hint(),
        ]
    } else {
        context_lines
    };
    let selected_line_indexes = lines
        .iter()
        .enumerate()
        .filter_map(|(index, line)| line.starts_with("selected:").then_some(index))
        .collect::<Vec<_>>();
    let block = config_block_with_palette("Details", palette.config_detail, panel_bg, palette);
    let content_area = block.inner(area);
    let content_width = content_area.width as usize;
    let detail = lines
        .into_iter()
        .map(|line| render_config_context_line_with_palette(&line, content_width, palette))
        .collect::<Vec<_>>();
    let scroll_offset =
        config_scroll_offset(detail.len(), content_area.height, &selected_line_indexes);
    frame.render_widget(block, area);
    render_config_scroll_markers(
        frame,
        area,
        detail.len(),
        content_area.height,
        scroll_offset,
        palette,
    );
    render_config_selected_row_bgs(
        frame,
        content_area,
        &selected_line_indexes,
        scroll_offset,
        palette,
    );

    let widget =
        Paragraph::new(Text::from(detail)).scroll((scroll_offset.min(u16::MAX as usize) as u16, 0));

    frame.render_widget(widget, content_area);
}

pub(super) fn config_scroll_offset(
    line_count: usize,
    viewport_height: u16,
    focus_indexes: &[usize],
) -> usize {
    let viewport_height = viewport_height as usize;
    if viewport_height == 0 || line_count <= viewport_height {
        return 0;
    }
    let Some(focus_index) = focus_indexes.first().copied() else {
        return 0;
    };
    let max_offset = line_count.saturating_sub(viewport_height);
    let focus_padding = if viewport_height >= 6 { 2 } else { 0 };
    if focus_index < viewport_height.saturating_sub(focus_padding) {
        return 0;
    }
    focus_index.saturating_sub(focus_padding).min(max_offset)
}

fn render_config_scroll_markers(
    frame: &mut Frame,
    area: Rect,
    line_count: usize,
    viewport_height: u16,
    scroll_offset: usize,
    palette: &ThemePalette,
) {
    let viewport_height = viewport_height as usize;
    if area.width <= CONFIG_SCROLL_MARKER_WIDTH + 2 || line_count <= viewport_height {
        return;
    }
    let top_hidden = scroll_offset > 0;
    let bottom_hidden = scroll_offset.saturating_add(viewport_height) < line_count;
    if !top_hidden && !bottom_hidden {
        return;
    }

    let marker_x = area.x + area.width.saturating_sub(CONFIG_SCROLL_MARKER_WIDTH + 1);
    if top_hidden {
        render_config_scroll_marker(frame, marker_x, area.y, " more ^ ", palette);
    }
    if bottom_hidden {
        render_config_scroll_marker(
            frame,
            marker_x,
            area.y + area.height.saturating_sub(1),
            " more v ",
            palette,
        );
    }
}

fn render_config_scroll_marker(
    frame: &mut Frame,
    x: u16,
    y: u16,
    text: &'static str,
    palette: &ThemePalette,
) {
    let marker = Paragraph::new(Line::from(vec![Span::styled(
        text,
        Style::default()
            .fg(palette.button_selected_fg)
            .bg(palette.config_warning)
            .add_modifier(Modifier::BOLD),
    )]));
    frame.render_widget(
        marker,
        Rect {
            x,
            y,
            width: CONFIG_SCROLL_MARKER_WIDTH,
            height: 1,
        },
    );
}

#[allow(dead_code)]
fn config_block(title: &'static str, accent: Color, panel_bg: Color) -> Block<'static> {
    let palette = theme::default_palette();
    config_block_with_palette(title, accent, panel_bg, &palette)
}

fn config_block_with_palette(
    title: &'static str,
    accent: Color,
    panel_bg: Color,
    palette: &ThemePalette,
) -> Block<'static> {
    Block::default()
        .title(title)
        .title_style(
            Style::default()
                .fg(palette.button_selected_fg)
                .bg(accent)
                .add_modifier(Modifier::BOLD),
        )
        .border_type(BorderType::Rounded)
        .borders(Borders::ALL)
        .border_style(Style::default().fg(palette.config_border))
        .style(Style::default().bg(panel_bg))
}

pub(super) fn split_config_context_lines(lines: Vec<String>) -> (Vec<String>, Vec<String>) {
    let Some(details_index) = lines.iter().position(|line| line == "[details]") else {
        return (lines, Vec::new());
    };

    let mut main_lines = lines[..details_index].to_vec();
    while main_lines.last().is_some_and(|line| line.is_empty()) {
        main_lines.pop();
    }
    let context_lines = lines[details_index + 1..].to_vec();
    (main_lines, context_lines)
}

fn render_config_footer(
    frame: &mut Frame,
    area: Rect,
    app: &AppState,
    panel_bg: Color,
    palette: &ThemePalette,
) {
    let selected = app.config_selected_footer_action_label();
    let compact = area.width < CONFIG_FOOTER_COMPACT_WIDTH;
    let action_labels = app.config_footer_action_labels();
    let mut action_spans = Vec::new();
    let mut actions_width = 0usize;
    for (index, label) in action_labels.iter().copied().enumerate() {
        if index > 0 {
            action_spans.push(Span::raw(" "));
            actions_width += 1;
        }
        let is_selected = selected == Some(label);
        action_spans.push(footer_action_span(
            label,
            is_selected,
            footer_action_accent_with_palette(label, palette),
            compact,
            palette,
        ));
        actions_width += footer_action_width(label, is_selected, compact);
    }
    let gap_width = if compact {
        " | ".chars().count()
    } else {
        let preferred_status_width = footer_status_width(&app.config_footer_hint());
        area.width
            .saturating_sub(actions_width as u16)
            .saturating_sub(preferred_status_width as u16)
            .max(2) as usize
    };
    let status_width = (area.width as usize)
        .saturating_sub(actions_width)
        .saturating_sub(gap_width);
    let status_style = if app.config_close_guard_armed() {
        Style::default()
            .fg(palette.config_danger)
            .add_modifier(Modifier::BOLD)
    } else if app.config_is_dirty() {
        Style::default()
            .fg(palette.config_warning)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(palette.text_secondary)
    };
    let gap = if compact {
        " | ".to_owned()
    } else {
        " ".repeat(gap_width)
    };
    let mut spans = action_spans;
    spans.push(Span::raw(gap));
    spans.extend(footer_status_spans_with_palette(
        &app.config_footer_hint(),
        status_width,
        status_style,
        palette,
    ));
    let line = Line::from(spans);
    let footer = Paragraph::new(Text::from(vec![line]))
        .style(Style::default().bg(panel_bg))
        .wrap(Wrap { trim: false });
    frame.render_widget(footer, area);
}

#[allow(dead_code)]
fn footer_action_accent(label: &str) -> Color {
    let palette = theme::default_palette();
    footer_action_accent_with_palette(label, &palette)
}

fn footer_action_accent_with_palette(label: &str, palette: &ThemePalette) -> Color {
    match label {
        "save" => palette.config_primary,
        "save+close" => palette.config_warning,
        "activate" => palette.config_detail,
        "close" => palette.config_danger,
        _ => palette.config_primary,
    }
}

fn footer_action_span(
    label: &'static str,
    selected: bool,
    accent: Color,
    compact: bool,
    palette: &ThemePalette,
) -> Span<'static> {
    let text = footer_action_text(label, selected, compact);
    let style = if selected {
        Style::default()
            .fg(palette.button_selected_fg)
            .bg(accent)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default()
            .fg(accent)
            .bg(palette.config_tab_bg)
            .add_modifier(Modifier::BOLD)
    };
    Span::styled(text, style)
}

pub(super) fn footer_action_width(label: &'static str, selected: bool, compact: bool) -> usize {
    footer_action_text(label, selected, compact).chars().count()
}

fn footer_action_text(label: &'static str, selected: bool, compact: bool) -> String {
    let inner = if selected {
        format!("> {label} <")
    } else {
        format!("[{label}]")
    };
    if compact {
        inner
    } else {
        format!("{inner:^CONFIG_FOOTER_BUTTON_WIDTH$}")
    }
}

fn footer_status_width(value: &str) -> usize {
    CONFIG_STATUS_MARKER_WIDTH + footer_status_value(value).chars().count()
}

#[allow(dead_code)]
fn footer_status_spans(value: &str, width: usize, value_style: Style) -> Vec<Span<'static>> {
    let palette = theme::default_palette();
    footer_status_spans_with_palette(value, width, value_style, &palette)
}

fn footer_status_spans_with_palette(
    value: &str,
    width: usize,
    value_style: Style,
    palette: &ThemePalette,
) -> Vec<Span<'static>> {
    if width == 0 {
        return Vec::new();
    }
    let status_value = footer_status_value(value);
    let kind = config_status_kind_for_value("status", status_value).unwrap_or(StatusKind::Unknown);
    let indicator = StatusIndicator::static_kind(kind);
    if width == 1 {
        return vec![indicator.span_with_palette(palette)];
    }
    let value = fit_config_value(
        status_value,
        width.saturating_sub(CONFIG_STATUS_MARKER_WIDTH),
    );
    vec![
        indicator.span_with_palette(palette),
        Span::raw(" "),
        Span::styled(value, config_status_value_style(kind, value_style, palette)),
    ]
}

fn footer_status_value(value: &str) -> &str {
    value.strip_prefix("status: ").unwrap_or(value)
}

fn config_status_kind_for_value(label: &str, value: &str) -> Option<StatusKind> {
    let normalized_label = label.trim().to_ascii_lowercase();
    let normalized = config_status_key(value)?;
    match normalized.as_str() {
        "yes" | "enabled" | "configured" | "trusted" | "approved" | "ok" | "ready" | "saved"
        | "available" | "valid" | "active" | "loaded" | "completed" => Some(StatusKind::Success),
        "running" | "started" | "starting" | "loading" | "activating" | "checking" => {
            Some(StatusKind::Running)
        }
        "pending" | "none" | "off" | "disabled" | "unavailable" => Some(StatusKind::Unknown),
        "no" => match normalized_label.as_str() {
            "warnings" => Some(StatusKind::Success),
            _ => Some(StatusKind::Unknown),
        },
        "dirty" | "unsaved" | "needs_review" | "warning" | "warnings" | "warn" | "paused"
        | "deferred" | "lazy" | "missing" | "shadowed" => Some(StatusKind::Warning),
        "confirm" | "failed" | "error" | "invalid" | "untrusted" | "denied" | "blocked"
        | "cancelled" | "interrupted" | "timeout" | "timed" => Some(StatusKind::Error),
        "not" | "is" => Some(StatusKind::Unknown),
        _ => None,
    }
}

fn config_status_key(value: &str) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return None;
    }
    if let Some(first) = trimmed.split_whitespace().next()
        && first.chars().all(|character| character.is_ascii_digit())
    {
        if trimmed.contains(" warning") {
            return if first == "0" {
                Some("yes".to_owned())
            } else {
                Some("warning".to_owned())
            };
        }
        return None;
    }
    let first = trimmed
        .split(|character: char| {
            character.is_whitespace()
                || matches!(character, ':' | ',' | ';' | '(' | ')' | '[' | ']')
        })
        .find(|token| !token.is_empty())?;
    Some(first.to_ascii_lowercase().replace('-', "_"))
}

fn config_status_value_style(kind: StatusKind, fallback: Style, palette: &ThemePalette) -> Style {
    match kind {
        StatusKind::Unknown | StatusKind::Pending => fallback,
        _ => status_rest_style_with_palette(kind, palette),
    }
}

#[allow(dead_code)]
fn config_status_value_spans(
    label: &str,
    value: &str,
    width: usize,
    fallback_style: Style,
) -> Vec<Span<'static>> {
    let palette = theme::default_palette();
    config_status_value_spans_with_palette(label, value, width, fallback_style, &palette)
}

fn config_status_value_spans_with_palette(
    label: &str,
    value: &str,
    width: usize,
    fallback_style: Style,
    palette: &ThemePalette,
) -> Vec<Span<'static>> {
    if width == 0 {
        return Vec::new();
    }
    let Some(kind) = config_status_kind_for_value(label, value) else {
        return vec![Span::styled(fit_config_value(value, width), fallback_style)];
    };
    let indicator = StatusIndicator::animated(kind);
    if width == 1 {
        return vec![indicator.span_with_palette(palette)];
    }
    let value = fit_config_value(value, width.saturating_sub(CONFIG_STATUS_MARKER_WIDTH));
    vec![
        indicator.span_with_palette(palette),
        Span::raw(" "),
        Span::styled(
            value,
            config_status_value_style(kind, fallback_style, palette),
        ),
    ]
}

#[allow(dead_code)]
fn render_config_line(index: usize, line: &str, content_width: usize) -> Line<'static> {
    let palette = theme::default_palette();
    render_config_line_with_palette(index, line, content_width, &palette)
}

fn render_config_line_with_palette(
    index: usize,
    line: &str,
    content_width: usize,
    palette: &ThemePalette,
) -> Line<'static> {
    if line.is_empty() {
        return Line::raw(String::new());
    }
    if index == 0 {
        return render_config_title_line_with_palette(line, palette);
    }
    if index == 1 {
        return render_config_step_line_with_palette(
            line,
            palette.config_primary,
            content_width,
            palette,
        );
    }
    if line.starts_with('[') && line.ends_with(']') {
        return render_config_subsection_line_with_palette(
            line,
            palette.config_primary,
            content_width,
            palette,
        );
    }
    if let Some(line) = render_theme_preview_line_with_palette(line, content_width, palette) {
        return line;
    }
    if let Some(line) = render_readonly_line_with_palette(line, content_width, palette) {
        return line;
    }
    if let Some(line) = render_hint_line_with_palette(line, palette) {
        return line;
    }
    if let Some(line) =
        render_form_line_with_palette(line, palette.config_primary, content_width, palette)
    {
        return line;
    }
    if line.starts_with("Type value")
        || line.starts_with("Tab ")
        || line.starts_with("Enter ")
        || line.starts_with("Ctrl-")
    {
        return Line::styled(line.to_owned(), Style::default().fg(palette.config_warning));
    }
    if config_line_is_meta(line) {
        return Line::styled(line.to_owned(), Style::default().fg(palette.text_muted));
    }
    if config_line_looks_like_field(line) {
        return Line::styled(line.to_owned(), Style::default().fg(palette.text_primary));
    }

    Line::styled(line.to_owned(), Style::default().fg(palette.text_secondary))
}

fn render_theme_preview_line_with_palette(
    line: &str,
    content_width: usize,
    palette: &ThemePalette,
) -> Option<Line<'static>> {
    let rest = line.strip_prefix("preview ")?;
    let (kind, samples) = rest.split_once(':')?;
    let mut remaining = content_width;
    let mut spans = Vec::new();
    if !push_theme_preview_span(
        &mut spans,
        &mut remaining,
        "preview ",
        Style::default()
            .fg(palette.text_muted)
            .bg(palette.config_tab_bg)
            .add_modifier(Modifier::BOLD),
    ) {
        return Some(Line::from(spans));
    }
    if !push_theme_preview_span(
        &mut spans,
        &mut remaining,
        kind,
        Style::default()
            .fg(palette.config_detail)
            .add_modifier(Modifier::BOLD),
    ) {
        return Some(Line::from(spans));
    }
    if !push_theme_preview_span(
        &mut spans,
        &mut remaining,
        ": ",
        Style::default().fg(palette.text_muted),
    ) {
        return Some(Line::from(spans));
    }

    match kind {
        "compare" => {
            if let Some((current, draft)) = theme_preview_compare_values(samples) {
                push_theme_preview_sample(
                    &mut spans,
                    &mut remaining,
                    "current",
                    Style::default()
                        .fg(palette.text_muted)
                        .bg(palette.config_tab_bg)
                        .add_modifier(Modifier::BOLD),
                );
                push_theme_preview_sample(
                    &mut spans,
                    &mut remaining,
                    current,
                    Style::default().fg(palette.text_secondary),
                );
                push_theme_preview_sample(
                    &mut spans,
                    &mut remaining,
                    "->",
                    Style::default().fg(palette.text_muted),
                );
                push_theme_preview_sample(
                    &mut spans,
                    &mut remaining,
                    "draft",
                    Style::default()
                        .fg(palette.button_selected_fg)
                        .bg(palette.config_primary)
                        .add_modifier(Modifier::BOLD),
                );
                push_theme_preview_sample(
                    &mut spans,
                    &mut remaining,
                    draft,
                    Style::default()
                        .fg(palette.text_primary)
                        .add_modifier(Modifier::BOLD),
                );
            } else {
                push_theme_preview_sample(
                    &mut spans,
                    &mut remaining,
                    samples.trim(),
                    Style::default().fg(palette.text_secondary),
                );
            }
        }
        "page" => {
            push_theme_preview_sample(
                &mut spans,
                &mut remaining,
                "rail",
                Style::default()
                    .fg(palette.text_primary)
                    .bg(palette.surface_rail)
                    .add_modifier(Modifier::BOLD),
            );
            push_theme_preview_sample(
                &mut spans,
                &mut remaining,
                "timeline",
                Style::default()
                    .fg(palette.text_primary)
                    .bg(palette.surface_base),
            );
            push_theme_preview_sample(
                &mut spans,
                &mut remaining,
                "composer",
                Style::default()
                    .fg(palette.text_primary)
                    .bg(palette.surface_input),
            );
            push_theme_preview_sample(
                &mut spans,
                &mut remaining,
                "tool",
                Style::default()
                    .fg(palette.markdown_code_fg)
                    .bg(palette.markdown_code_bg),
            );
            push_theme_preview_sample(
                &mut spans,
                &mut remaining,
                "modal",
                Style::default()
                    .fg(palette.text_inverse)
                    .bg(palette.approval_selected_bg)
                    .add_modifier(Modifier::BOLD),
            );
        }
        "syntax" => {
            if let Some((configured, resolved)) = samples.trim().split_once(" -> ") {
                push_theme_preview_sample(
                    &mut spans,
                    &mut remaining,
                    configured.trim(),
                    Style::default()
                        .fg(palette.config_warning)
                        .bg(palette.config_tab_bg)
                        .add_modifier(Modifier::BOLD),
                );
                push_theme_preview_sample(
                    &mut spans,
                    &mut remaining,
                    "->",
                    Style::default().fg(palette.text_muted),
                );
                push_theme_preview_sample(
                    &mut spans,
                    &mut remaining,
                    resolved.trim(),
                    Style::default()
                        .fg(palette.markdown_code_fg)
                        .bg(palette.markdown_code_bg)
                        .add_modifier(Modifier::BOLD),
                );
            } else {
                push_theme_preview_sample(
                    &mut spans,
                    &mut remaining,
                    samples.trim(),
                    Style::default()
                        .fg(palette.markdown_code_fg)
                        .bg(palette.markdown_code_bg),
                );
            }
        }
        "shell" => {
            push_theme_preview_sample(
                &mut spans,
                &mut remaining,
                "rail",
                Style::default()
                    .fg(palette.text_primary)
                    .bg(palette.surface_rail),
            );
            push_theme_preview_sample(
                &mut spans,
                &mut remaining,
                "live",
                Style::default()
                    .fg(palette.text_primary)
                    .bg(palette.surface_base),
            );
            push_theme_preview_sample(
                &mut spans,
                &mut remaining,
                "composer",
                Style::default()
                    .fg(palette.text_primary)
                    .bg(palette.surface_panel),
            );
            push_theme_preview_sample(
                &mut spans,
                &mut remaining,
                "footer",
                Style::default()
                    .fg(palette.text_secondary)
                    .bg(palette.surface_panel_alt),
            );
        }
        "composer" => {
            push_theme_preview_sample(
                &mut spans,
                &mut remaining,
                "Build",
                Style::default()
                    .fg(palette.accent_streaming)
                    .bg(palette.surface_panel)
                    .add_modifier(Modifier::BOLD),
            );
            push_theme_preview_sample(
                &mut spans,
                &mut remaining,
                "agent: main",
                Style::default()
                    .fg(palette.accent_info)
                    .bg(palette.surface_agent_panel)
                    .add_modifier(Modifier::BOLD),
            );
            push_theme_preview_sample(
                &mut spans,
                &mut remaining,
                "deepseek-v4-flash",
                Style::default()
                    .fg(palette.text_primary)
                    .bg(palette.surface_input),
            );
        }
        "tool" => {
            push_theme_preview_sample(
                &mut spans,
                &mut remaining,
                "read_file",
                Style::default()
                    .fg(palette.accent_info)
                    .add_modifier(Modifier::BOLD),
            );
            push_theme_preview_status_sample(
                &mut spans,
                &mut remaining,
                StatusKind::Success,
                "ok",
                palette,
            );
            push_theme_preview_sample(
                &mut spans,
                &mut remaining,
                "doc excerpt",
                Style::default()
                    .fg(palette.markdown_code_fg)
                    .bg(palette.markdown_code_bg),
            );
            push_theme_preview_sample(
                &mut spans,
                &mut remaining,
                "2 hidden",
                Style::default().fg(palette.text_muted),
            );
        }
        "modal" => {
            push_theme_preview_sample(
                &mut spans,
                &mut remaining,
                "Review Tool Call",
                Style::default()
                    .fg(palette.text_inverse)
                    .bg(palette.approval_selected_bg)
                    .add_modifier(Modifier::BOLD),
            );
            push_theme_preview_sample(
                &mut spans,
                &mut remaining,
                "allow",
                Style::default()
                    .fg(palette.button_selected_fg)
                    .bg(palette.approval_allow_bg)
                    .add_modifier(Modifier::BOLD),
            );
            push_theme_preview_sample(
                &mut spans,
                &mut remaining,
                "deny",
                Style::default()
                    .fg(palette.button_selected_fg)
                    .bg(palette.approval_deny_bg)
                    .add_modifier(Modifier::BOLD),
            );
            push_theme_preview_sample(
                &mut spans,
                &mut remaining,
                "selected",
                Style::default()
                    .fg(palette.button_selected_fg)
                    .bg(palette.approval_selected_bg)
                    .add_modifier(Modifier::BOLD),
            );
        }
        "token" => {
            let mut parts = samples.split_whitespace();
            let token = parts.next().unwrap_or("token");
            let value = parts.next().unwrap_or("inherited");
            push_theme_preview_sample(
                &mut spans,
                &mut remaining,
                token,
                Style::default()
                    .fg(palette.config_primary)
                    .bg(palette.config_section_bg)
                    .add_modifier(Modifier::BOLD),
            );
            let value_style = if value == "inherited" {
                Style::default().fg(palette.text_muted)
            } else {
                Style::default()
                    .fg(palette.markdown_code_fg)
                    .bg(palette.markdown_code_bg)
                    .add_modifier(Modifier::BOLD)
            };
            push_theme_preview_sample(&mut spans, &mut remaining, value, value_style);
        }
        "text" => {
            push_theme_preview_sample(
                &mut spans,
                &mut remaining,
                "primary",
                Style::default().fg(palette.text_primary),
            );
            push_theme_preview_sample(
                &mut spans,
                &mut remaining,
                "secondary",
                Style::default().fg(palette.text_secondary),
            );
            push_theme_preview_sample(
                &mut spans,
                &mut remaining,
                "muted",
                Style::default().fg(palette.text_muted),
            );
        }
        "selection" => {
            push_theme_preview_sample(
                &mut spans,
                &mut remaining,
                "selected row",
                Style::default()
                    .fg(palette.selection_fg)
                    .bg(palette.selection_bg)
                    .add_modifier(Modifier::BOLD),
            );
            push_theme_preview_sample(
                &mut spans,
                &mut remaining,
                "normal",
                Style::default().fg(palette.text_primary),
            );
        }
        "status" => {
            push_theme_preview_status_sample(
                &mut spans,
                &mut remaining,
                StatusKind::Success,
                "success",
                palette,
            );
            push_theme_preview_status_sample(
                &mut spans,
                &mut remaining,
                StatusKind::Warning,
                "warning",
                palette,
            );
            push_theme_preview_status_sample(
                &mut spans,
                &mut remaining,
                StatusKind::Error,
                "error",
                palette,
            );
            push_theme_preview_status_sample(
                &mut spans,
                &mut remaining,
                StatusKind::Pending,
                "pending",
                palette,
            );
        }
        "diff" => {
            push_theme_preview_sample(
                &mut spans,
                &mut remaining,
                "+added",
                Style::default()
                    .fg(palette.diff_added_fg)
                    .bg(palette.diff_added_bg),
            );
            push_theme_preview_sample(
                &mut spans,
                &mut remaining,
                "-removed",
                Style::default()
                    .fg(palette.diff_removed_fg)
                    .bg(palette.diff_removed_bg),
            );
            push_theme_preview_sample(
                &mut spans,
                &mut remaining,
                "@@ hunk",
                Style::default()
                    .fg(palette.diff_hunk_fg)
                    .bg(palette.diff_current_hunk_bg)
                    .add_modifier(Modifier::BOLD),
            );
        }
        "approval" => {
            push_theme_preview_sample(
                &mut spans,
                &mut remaining,
                "allow",
                Style::default()
                    .fg(palette.button_selected_fg)
                    .bg(palette.approval_allow_bg)
                    .add_modifier(Modifier::BOLD),
            );
            push_theme_preview_sample(
                &mut spans,
                &mut remaining,
                "deny",
                Style::default()
                    .fg(palette.button_selected_fg)
                    .bg(palette.approval_deny_bg)
                    .add_modifier(Modifier::BOLD),
            );
            push_theme_preview_sample(
                &mut spans,
                &mut remaining,
                "selected",
                Style::default()
                    .fg(palette.button_selected_fg)
                    .bg(palette.approval_selected_bg)
                    .add_modifier(Modifier::BOLD),
            );
        }
        "markdown" => {
            push_theme_preview_sample(
                &mut spans,
                &mut remaining,
                "heading",
                Style::default()
                    .fg(palette.markdown_heading)
                    .add_modifier(Modifier::BOLD),
            );
            push_theme_preview_sample(
                &mut spans,
                &mut remaining,
                "link",
                Style::default()
                    .fg(palette.markdown_link)
                    .add_modifier(Modifier::UNDERLINED),
            );
            push_theme_preview_sample(
                &mut spans,
                &mut remaining,
                "code",
                Style::default()
                    .fg(palette.markdown_code_fg)
                    .bg(palette.markdown_code_bg),
            );
        }
        _ => {
            push_theme_preview_sample(
                &mut spans,
                &mut remaining,
                rest,
                Style::default().fg(palette.text_secondary),
            );
        }
    }

    Some(Line::from(spans))
}

fn theme_preview_compare_values(samples: &str) -> Option<(&str, &str)> {
    let rest = samples.trim().strip_prefix("current ")?;
    let (current, draft) = rest.split_once(" -> draft ")?;
    Some((current.trim(), draft.trim()))
}

fn push_theme_preview_status_sample(
    spans: &mut Vec<Span<'static>>,
    remaining: &mut usize,
    kind: StatusKind,
    label: &'static str,
    palette: &ThemePalette,
) {
    let indicator = StatusIndicator::static_kind(kind);
    push_theme_preview_sample(
        spans,
        remaining,
        &format!("{} {label}", indicator.symbol()),
        status_rest_style_with_palette(kind, palette),
    );
}

fn push_theme_preview_sample(
    spans: &mut Vec<Span<'static>>,
    remaining: &mut usize,
    text: &str,
    style: Style,
) -> bool {
    if spans
        .last()
        .is_some_and(|span| !span.content.as_ref().ends_with(' '))
        && !push_theme_preview_span(spans, remaining, " ", Style::default())
    {
        return false;
    }
    push_theme_preview_span(spans, remaining, text, style)
}

fn push_theme_preview_span(
    spans: &mut Vec<Span<'static>>,
    remaining: &mut usize,
    text: &str,
    style: Style,
) -> bool {
    if *remaining == 0 {
        return false;
    }
    let original_width = text.chars().count();
    let text = fit_config_value(text, *remaining);
    let width = text.chars().count();
    spans.push(Span::styled(text, style));
    *remaining = remaining.saturating_sub(width);
    width == original_width && *remaining > 0
}

#[allow(dead_code)]
fn render_config_context_line(line: &str, content_width: usize) -> Line<'static> {
    let palette = theme::default_palette();
    render_config_context_line_with_palette(line, content_width, &palette)
}

fn render_config_context_line_with_palette(
    line: &str,
    content_width: usize,
    palette: &ThemePalette,
) -> Line<'static> {
    if line.is_empty() {
        return Line::raw(String::new());
    }
    if let Some((label, value)) = line.split_once(':') {
        return render_config_context_pair_with_palette(
            label,
            value.trim_start(),
            content_width,
            palette,
        );
    }

    render_config_context_info_line_with_palette(line, content_width, palette)
}

#[allow(dead_code)]
fn render_config_context_pair(label: &str, value: &str, content_width: usize) -> Line<'static> {
    let palette = theme::default_palette();
    render_config_context_pair_with_palette(label, value, content_width, &palette)
}

fn render_config_context_pair_with_palette(
    label: &str,
    value: &str,
    content_width: usize,
    palette: &ThemePalette,
) -> Line<'static> {
    match label {
        "selected" => {
            let marker = format!("{} ", focus_symbol(FocusKind::Selected));
            let value = fit_config_value(
                value,
                available_config_value_width(content_width, marker.chars().count(), 0),
            );
            Line::from(vec![
                Span::styled(
                    marker,
                    focus_style_with_palette(FocusKind::Selected, palette)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    value,
                    Style::default()
                        .fg(palette.text_primary)
                        .bg(palette.config_selected_bg)
                        .add_modifier(Modifier::BOLD),
                ),
            ])
            .style(Style::default().bg(palette.config_selected_bg))
        }
        "controls" | "actions" | "mcp" => {
            render_config_context_commands_with_palette(label, value, content_width, palette)
        }
        "status" => render_config_status_line_with_palette(value, content_width, palette),
        "key" | "advanced" | "override" => {
            render_config_metadata_line_with_palette(label, value, content_width, palette)
        }
        _ => {
            let label_text = format!("{label}: ");
            let value_width =
                available_config_value_width(content_width, label_text.chars().count(), 0);
            let mut spans = vec![Span::styled(
                label_text,
                Style::default()
                    .fg(palette.config_detail)
                    .add_modifier(Modifier::BOLD),
            )];
            spans.extend(config_status_value_spans_with_palette(
                label,
                value,
                value_width,
                Style::default().fg(palette.text_primary),
                palette,
            ));
            Line::from(spans)
        }
    }
}

#[allow(dead_code)]
fn render_config_context_commands(label: &str, value: &str, content_width: usize) -> Line<'static> {
    let palette = theme::default_palette();
    render_config_context_commands_with_palette(label, value, content_width, &palette)
}

fn render_config_context_commands_with_palette(
    label: &str,
    value: &str,
    content_width: usize,
    palette: &ThemePalette,
) -> Line<'static> {
    let marker = config_context_command_marker(label);
    let marker_width = marker.chars().count();
    let marker_style = Style::default()
        .fg(palette.text_muted)
        .bg(palette.config_tab_bg)
        .add_modifier(Modifier::BOLD);
    if content_width <= marker_width {
        return Line::from(vec![Span::styled(
            fit_config_value(marker, content_width),
            marker_style,
        )]);
    }
    let mut remaining = content_width.saturating_sub(marker_width);
    let mut spans = vec![Span::styled(marker, marker_style)];
    for (index, token) in value.split(" · ").enumerate() {
        if remaining == 0 {
            break;
        }
        let separator = if index > 0 { " · " } else { "" };
        let separator_width = separator.chars().count();
        if separator_width >= remaining {
            spans.push(Span::styled(
                fit_config_value("...", remaining),
                Style::default().fg(palette.text_muted),
            ));
            break;
        }
        let token_capacity = remaining.saturating_sub(separator_width);
        if index > 0 {
            spans.push(Span::styled(
                separator,
                Style::default().fg(palette.text_muted),
            ));
        }
        let (rendered_width, truncated) =
            push_config_command_token_with_palette(&mut spans, token, token_capacity, palette);
        if truncated {
            break;
        }
        remaining = remaining.saturating_sub(separator_width + rendered_width);
    }
    Line::from(spans)
}

fn config_context_command_marker(label: &str) -> &'static str {
    match label {
        "controls" => "keys ",
        "actions" => "actions ",
        "mcp" => "mcp ",
        _ => "cmd ",
    }
}

#[allow(dead_code)]
fn push_config_command_token(
    spans: &mut Vec<Span<'static>>,
    token: &str,
    capacity: usize,
) -> (usize, bool) {
    let palette = theme::default_palette();
    push_config_command_token_with_palette(spans, token, capacity, &palette)
}

fn push_config_command_token_with_palette(
    spans: &mut Vec<Span<'static>>,
    token: &str,
    capacity: usize,
    palette: &ThemePalette,
) -> (usize, bool) {
    if capacity == 0 {
        return (0, !token.is_empty());
    }

    let (key, suffix) = token.split_once(' ').unwrap_or((token, ""));
    let original_key_width = key.chars().count();
    let key = fit_config_value(key, capacity);
    let key_width = key.chars().count();
    spans.push(Span::styled(
        key,
        Style::default()
            .fg(palette.config_warning)
            .bg(palette.config_tab_bg)
            .add_modifier(Modifier::BOLD),
    ));
    if key_width < original_key_width || key_width >= capacity || suffix.is_empty() {
        return (key_width, key_width < token.chars().count());
    }

    let suffix_text = format!(" {suffix}");
    let suffix_capacity = capacity.saturating_sub(key_width);
    let rendered_suffix = fit_config_value(&suffix_text, suffix_capacity);
    let suffix_width = rendered_suffix.chars().count();
    spans.push(Span::styled(
        rendered_suffix,
        Style::default().fg(palette.text_secondary),
    ));
    (
        key_width + suffix_width,
        suffix_text.chars().count() > suffix_width,
    )
}

#[allow(dead_code)]
fn render_config_context_info_line(line: &str, content_width: usize) -> Line<'static> {
    let palette = theme::default_palette();
    render_config_context_info_line_with_palette(line, content_width, &palette)
}

fn render_config_context_info_line_with_palette(
    line: &str,
    content_width: usize,
    palette: &ThemePalette,
) -> Line<'static> {
    let text = fit_config_value(
        line,
        available_config_value_width(content_width, "i ".chars().count(), 0),
    );
    Line::from(vec![
        Span::styled(
            "i ",
            Style::default()
                .fg(palette.config_warning)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            text,
            Style::default()
                .fg(palette.text_secondary)
                .add_modifier(Modifier::ITALIC),
        ),
    ])
}

#[allow(dead_code)]
fn render_config_metadata_line(label: &str, value: &str, content_width: usize) -> Line<'static> {
    let palette = theme::default_palette();
    render_config_metadata_line_with_palette(label, value, content_width, &palette)
}

fn render_config_metadata_line_with_palette(
    label: &str,
    value: &str,
    content_width: usize,
    palette: &ThemePalette,
) -> Line<'static> {
    let marker = config_metadata_marker(label);
    let value = fit_config_value(
        value,
        available_config_value_width(content_width, marker.chars().count(), 0),
    );
    Line::from(vec![
        Span::styled(
            marker,
            Style::default()
                .fg(palette.text_muted)
                .bg(palette.config_tab_bg)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(value, Style::default().fg(palette.text_secondary)),
    ])
}

fn config_metadata_marker(label: &str) -> &'static str {
    match label {
        "key" => "key ",
        "advanced" => "advanced ",
        "override" => "source ",
        _ => "meta ",
    }
}

#[allow(dead_code)]
fn render_config_status_line(value: &str, content_width: usize) -> Line<'static> {
    let palette = theme::default_palette();
    render_config_status_line_with_palette(value, content_width, &palette)
}

fn render_config_status_line_with_palette(
    value: &str,
    content_width: usize,
    palette: &ThemePalette,
) -> Line<'static> {
    if content_width == 0 {
        return Line::raw(String::new());
    }
    let kind = if value.contains("confirm close") {
        StatusKind::Error
    } else {
        config_status_kind_for_value("status", value).unwrap_or(StatusKind::Unknown)
    };
    let value_style = if value.contains("confirm close") {
        Style::default()
            .fg(palette.config_danger)
            .add_modifier(Modifier::BOLD)
    } else if value.contains("unsaved") {
        Style::default()
            .fg(palette.config_warning)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(palette.config_primary)
    };
    let indicator = StatusIndicator::static_kind(kind);
    let value = fit_config_value(
        value,
        content_width.saturating_sub(CONFIG_STATUS_MARKER_WIDTH),
    );
    Line::from(vec![
        indicator.span_with_palette(palette),
        Span::raw(" "),
        Span::styled(value, value_style),
    ])
}

#[allow(dead_code)]
fn render_readonly_line(line: &str, content_width: usize) -> Option<Line<'static>> {
    let palette = theme::default_palette();
    render_readonly_line_with_palette(line, content_width, &palette)
}

fn render_readonly_line_with_palette(
    line: &str,
    content_width: usize,
    palette: &ThemePalette,
) -> Option<Line<'static>> {
    let rest = line.strip_prefix("- ")?;
    let (label, value) = rest.split_once(':')?;
    let label_width = config_form_label_width(content_width, label);
    let padded_label = format!("{label:<label_width$}");
    let value = value.trim_start();
    let marker_width = CONFIG_STATUS_MARKER_WIDTH;
    let value_width =
        available_config_value_width(content_width, marker_width + label_width + 2, 0);
    let marker_kind = config_status_kind_for_value(label, value).unwrap_or(StatusKind::Unknown);
    let indicator = StatusIndicator::animated(marker_kind);

    let mut spans = vec![
        indicator.span_with_palette(palette),
        Span::raw(" "),
        Span::styled(padded_label, Style::default().fg(palette.text_secondary)),
        Span::styled(": ", Style::default().fg(palette.text_muted)),
    ];
    spans.extend(config_status_value_spans_with_palette(
        label,
        value,
        value_width,
        Style::default().fg(palette.text_secondary),
        palette,
    ));

    Some(Line::from(spans))
}

#[allow(dead_code)]
fn render_hint_line(line: &str) -> Option<Line<'static>> {
    let palette = theme::default_palette();
    render_hint_line_with_palette(line, &palette)
}

fn render_hint_line_with_palette(line: &str, palette: &ThemePalette) -> Option<Line<'static>> {
    let text = line.strip_prefix("i ")?;

    Some(Line::from(vec![
        Span::styled(
            "i ",
            Style::default()
                .fg(palette.config_warning)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            text.to_owned(),
            Style::default()
                .fg(palette.text_secondary)
                .add_modifier(Modifier::ITALIC),
        ),
    ]))
}

#[allow(dead_code)]
fn render_setup_line(line: &str) -> Line<'static> {
    let palette = theme::default_palette();
    render_setup_line_with_palette(line, &palette)
}

fn render_setup_line_with_palette(line: &str, palette: &ThemePalette) -> Line<'static> {
    if line.is_empty() {
        return Line::raw(String::new());
    }
    if line.starts_with('[') && line.ends_with(']') {
        return render_subsection_line_with_palette(line, palette.config_warning, palette);
    }
    if let Some(line) =
        render_form_line_with_palette(line, palette.config_warning, usize::MAX, palette)
    {
        return line;
    }
    if line.starts_with("Enter ")
        || line.starts_with("Type custom")
        || line.starts_with("Ctrl-")
        || line.starts_with("auth=")
    {
        return Line::styled(line.to_owned(), Style::default().fg(palette.config_warning));
    }
    if line.starts_with("defaults:") {
        return Line::styled(line.to_owned(), Style::default().fg(palette.text_muted));
    }
    if line == "Quick setup" || line == "Workspace trust" {
        return Line::styled(
            line.to_owned(),
            Style::default()
                .fg(palette.text_primary)
                .add_modifier(Modifier::BOLD),
        );
    }
    Line::styled(line.to_owned(), Style::default().fg(palette.text_secondary))
}

#[allow(dead_code)]
fn render_form_line(line: &str, accent: Color, content_width: usize) -> Option<Line<'static>> {
    let palette = theme::default_palette();
    render_form_line_with_palette(line, accent, content_width, &palette)
}

fn render_form_line_with_palette(
    line: &str,
    accent: Color,
    content_width: usize,
    palette: &ThemePalette,
) -> Option<Line<'static>> {
    let content_width = content_width.min(CONFIG_CONTENT_MAX_WIDTH as usize);
    let row_bg = selected_row_bg_with_palette(accent, palette);
    let (selected, rest) = if let Some(rest) = line.strip_prefix("> ") {
        (true, rest)
    } else if let Some(rest) = line.strip_prefix("  ") {
        (false, rest)
    } else {
        return None;
    };

    if let Some(label) = rest
        .strip_prefix('[')
        .and_then(|value| value.strip_suffix(']'))
    {
        let marker_style = if selected {
            Style::default()
                .fg(palette.button_selected_fg)
                .bg(accent)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(palette.text_muted)
        };
        let value_style = if selected {
            Style::default()
                .fg(palette.button_selected_fg)
                .bg(accent)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(palette.text_primary)
        };
        let spans = vec![
            Span::styled(if selected { "> " } else { "  " }, marker_style),
            Span::styled(format!("[{label}]"), value_style),
        ];
        return Some(finish_form_line(spans, selected, row_bg));
    }

    let (label, value_and_action) = rest.split_once(':')?;
    let (value, action) = if let Some((value, action)) = value_and_action.rsplit_once("  [") {
        if action.ends_with(']') {
            (
                value.trim_start(),
                Some(action.trim_end_matches(']').to_owned()),
            )
        } else {
            (value_and_action.trim_start(), None)
        }
    } else {
        (value_and_action.trim_start(), None)
    };
    let label_width = config_form_label_width(content_width, label);
    let padded_label = format!("{label:<label_width$}");

    let marker_style = if selected {
        Style::default()
            .fg(palette.button_selected_fg)
            .bg(accent)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(palette.text_muted)
    };
    let label_style = if selected {
        Style::default()
            .fg(accent)
            .bg(row_bg)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(palette.config_detail)
    };
    let value_style = if selected {
        Style::default()
            .fg(palette.text_primary)
            .bg(row_bg)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(palette.text_primary)
    };
    let colon_style = if selected {
        Style::default().fg(palette.text_muted).bg(row_bg)
    } else {
        Style::default().fg(palette.text_muted)
    };
    let action_display = action.as_deref().map(config_action_display_label);
    let action_width = action_display.map_or(0, |action| 2 + config_action_chip_width(action));
    let value_width =
        available_config_value_width(content_width, 2 + label_width + 2, action_width);
    let mut value_spans =
        config_status_value_spans_with_palette(label, value, value_width, value_style, palette);
    if selected {
        apply_span_background(&mut value_spans, row_bg);
    }
    let value_len = spans_width(&value_spans);

    let mut spans = vec![
        Span::styled(if selected { "> " } else { "  " }, marker_style),
        Span::styled(padded_label, label_style),
        Span::styled(": ", colon_style),
    ];
    spans.extend(value_spans);
    if let Some(action) = action_display {
        let action_gap = value_width.saturating_sub(value_len).saturating_add(2);
        spans.push(if selected {
            Span::styled(" ".repeat(action_gap), Style::default().bg(row_bg))
        } else {
            Span::raw(" ".repeat(action_gap))
        });
        spans.push(Span::styled(
            format!("[{action}]"),
            if selected {
                Style::default()
                    .fg(palette.button_selected_fg)
                    .bg(accent)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(palette.config_warning)
            },
        ));
    }
    Some(finish_form_line(spans, selected, row_bg))
}

fn config_action_display_label(action: &str) -> &str {
    action.strip_prefix("Enter ").unwrap_or(action)
}

fn config_form_label_width(content_width: usize, label: &str) -> usize {
    let label_len = label.chars().count();
    if content_width >= 48 {
        CONFIG_FORM_LABEL_WIDTH.max(label_len)
    } else {
        label_len
    }
}

fn config_action_chip_width(action: &str) -> usize {
    CONFIG_FORM_ACTION_CHIP_WIDTH.max(action.chars().count() + 2)
}

fn available_config_value_width(
    content_width: usize,
    leading_width: usize,
    trailing_width: usize,
) -> usize {
    content_width
        .saturating_sub(leading_width)
        .saturating_sub(trailing_width)
}

fn fit_config_value(value: &str, max_chars: usize) -> String {
    let value_len = value.chars().count();
    if value_len <= max_chars {
        return value.to_owned();
    }
    if max_chars == 0 {
        return String::new();
    }
    if max_chars <= 3 {
        return value.chars().take(max_chars).collect();
    }

    let visible = max_chars - 3;
    let head_len = visible.div_ceil(2);
    let tail_len = visible - head_len;
    let head = value.chars().take(head_len).collect::<String>();
    let tail = value
        .chars()
        .rev()
        .take(tail_len)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect::<String>();
    format!("{head}...{tail}")
}

fn finish_form_line(spans: Vec<Span<'static>>, selected: bool, row_bg: Color) -> Line<'static> {
    let line = Line::from(spans);
    if selected {
        line.style(Style::default().bg(row_bg))
    } else {
        line
    }
}

fn apply_span_background(spans: &mut [Span<'static>], background: Color) {
    for span in spans {
        span.style = span.style.bg(background);
    }
}

fn spans_width(spans: &[Span<'_>]) -> usize {
    spans.iter().map(|span| span.content.chars().count()).sum()
}

#[allow(dead_code)]
fn render_config_title_line(line: &str) -> Line<'static> {
    let palette = theme::default_palette();
    render_config_title_line_with_palette(line, &palette)
}

fn render_config_title_line_with_palette(line: &str, palette: &ThemePalette) -> Line<'static> {
    let Some((title, rest)) = line.split_once(' ') else {
        return Line::styled(
            line.to_owned(),
            Style::default()
                .fg(palette.text_primary)
                .add_modifier(Modifier::BOLD),
        );
    };
    let (position, summary) = rest.split_once(" · ").unwrap_or((rest, ""));
    let mut spans = vec![
        Span::styled(
            title.to_owned(),
            Style::default()
                .fg(palette.text_primary)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(" "),
        Span::styled(
            position.to_owned(),
            Style::default().fg(palette.config_warning),
        ),
    ];
    if !summary.is_empty() {
        spans.push(Span::raw("  "));
        spans.push(Span::styled(
            summary.to_owned(),
            Style::default().fg(palette.text_secondary),
        ));
    }
    Line::from(spans)
}

#[allow(dead_code)]
fn render_config_step_line(line: &str, accent: Color, content_width: usize) -> Line<'static> {
    let palette = theme::default_palette();
    render_config_step_line_with_palette(line, accent, content_width, &palette)
}

fn render_config_step_line_with_palette(
    line: &str,
    accent: Color,
    content_width: usize,
    palette: &ThemePalette,
) -> Line<'static> {
    let Some(selected_section) = selected_config_step_section(line) else {
        return render_config_step_words_with_palette(line, accent, palette);
    };
    let sections = config_step_sections(line);
    let Some(selected_index) = sections
        .iter()
        .position(|section| *section == selected_section)
    else {
        return render_config_step_words_with_palette(line, accent, palette);
    };
    let (start, end) = config_step_window(&sections, selected_index, content_width);
    let mut spans = Vec::new();
    if start > 0 {
        push_config_step_item(
            &mut spans,
            Span::styled("...", Style::default().fg(palette.text_muted)),
        );
    }
    for index in start..end {
        let section = sections[index];
        let label = section.title().to_ascii_lowercase();
        let selected = index == selected_index;
        let span = if selected {
            Span::styled(
                format!(" {label} "),
                Style::default()
                    .fg(palette.button_selected_fg)
                    .bg(accent)
                    .add_modifier(Modifier::BOLD),
            )
        } else {
            Span::styled(label, Style::default().fg(palette.text_secondary))
        };
        push_config_step_item(&mut spans, span);
    }
    if end < sections.len() {
        push_config_step_item(
            &mut spans,
            Span::styled("...", Style::default().fg(palette.text_muted)),
        );
    }
    Line::from(spans)
}

fn selected_config_step_section(line: &str) -> Option<ConfigSection> {
    let selected = line.split_whitespace().find_map(|token| {
        token
            .strip_prefix('[')
            .and_then(|value| value.strip_suffix(']'))
    })?;
    ConfigSection::FLOW
        .iter()
        .copied()
        .find(|section| section.step_token().eq_ignore_ascii_case(selected))
}

fn config_step_sections(line: &str) -> Vec<ConfigSection> {
    line.split_whitespace()
        .filter_map(|token| {
            let token = token
                .strip_prefix('[')
                .and_then(|value| value.strip_suffix(']'))
                .unwrap_or(token);
            ConfigSection::FLOW
                .iter()
                .copied()
                .find(|section| section.step_token().eq_ignore_ascii_case(token))
        })
        .collect()
}

fn config_step_window(
    sections: &[ConfigSection],
    selected_index: usize,
    max_width: usize,
) -> (usize, usize) {
    let mut start = selected_index;
    let mut end = selected_index.saturating_add(1).min(sections.len());
    let mut prefer_left = selected_index >= sections.len() / 2;
    loop {
        let mut changed = false;
        for try_left in [prefer_left, !prefer_left] {
            if try_left {
                if start == 0 {
                    continue;
                }
                let candidate_start = start - 1;
                if config_step_window_width(sections, candidate_start, end, selected_index)
                    <= max_width
                {
                    start = candidate_start;
                    changed = true;
                    break;
                }
            } else {
                if end >= sections.len() {
                    continue;
                }
                let candidate_end = end + 1;
                if config_step_window_width(sections, start, candidate_end, selected_index)
                    <= max_width
                {
                    end = candidate_end;
                    changed = true;
                    break;
                }
            }
        }
        if !changed {
            break;
        }
        prefer_left = !prefer_left;
    }
    (start, end)
}

fn config_step_window_width(
    sections: &[ConfigSection],
    start: usize,
    end: usize,
    selected_index: usize,
) -> usize {
    let mut item_count = 0usize;
    let mut width = 0usize;
    if start > 0 {
        width += "...".chars().count();
        item_count += 1;
    }
    for index in start..end {
        width += config_step_section_width(sections, index, selected_index);
        item_count += 1;
    }
    if end < sections.len() {
        width += "...".chars().count();
        item_count += 1;
    }
    width + item_count.saturating_sub(1) * 2
}

fn config_step_section_width(
    sections: &[ConfigSection],
    index: usize,
    selected_index: usize,
) -> usize {
    let label_width = sections[index].title().chars().count();
    if index == selected_index {
        label_width + 2
    } else {
        label_width
    }
}

fn push_config_step_item(spans: &mut Vec<Span<'static>>, span: Span<'static>) {
    if !spans.is_empty() {
        spans.push(Span::raw("  "));
    }
    spans.push(span);
}

#[allow(dead_code)]
fn render_config_step_words(line: &str, accent: Color) -> Line<'static> {
    let palette = theme::default_palette();
    render_config_step_words_with_palette(line, accent, &palette)
}

fn render_config_step_words_with_palette(
    line: &str,
    accent: Color,
    palette: &ThemePalette,
) -> Line<'static> {
    let mut spans = Vec::new();
    for (index, token) in line.split_whitespace().enumerate() {
        if index > 0 {
            spans.push(Span::raw("  "));
        }
        let (text, style) = if token.starts_with('[') && token.ends_with(']') {
            (
                format!(" {} ", token.trim_start_matches('[').trim_end_matches(']')),
                Style::default()
                    .fg(palette.button_selected_fg)
                    .bg(accent)
                    .add_modifier(Modifier::BOLD),
            )
        } else {
            (
                token.to_owned(),
                Style::default().fg(palette.text_secondary),
            )
        };
        spans.push(Span::styled(text, style));
    }
    Line::from(spans)
}

#[allow(dead_code)]
fn render_subsection_line(line: &str, accent: Color) -> Line<'static> {
    let palette = theme::default_palette();
    render_subsection_line_with_palette(line, accent, &palette)
}

fn render_subsection_line_with_palette(
    line: &str,
    accent: Color,
    palette: &ThemePalette,
) -> Line<'static> {
    let text = line.trim_start_matches('[').trim_end_matches(']');
    Line::from(vec![
        Span::styled(
            format!(" {text} "),
            Style::default()
                .fg(accent)
                .bg(palette.config_section_bg)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(" "),
    ])
}

#[allow(dead_code)]
fn render_config_subsection_line(line: &str, accent: Color, content_width: usize) -> Line<'static> {
    let palette = theme::default_palette();
    render_config_subsection_line_with_palette(line, accent, content_width, &palette)
}

fn render_config_subsection_line_with_palette(
    line: &str,
    accent: Color,
    content_width: usize,
    palette: &ThemePalette,
) -> Line<'static> {
    let text = line.trim_start_matches('[').trim_end_matches(']');
    let leading = "  ";
    let chip = format!(" {text} ");
    let separator_width = content_width.saturating_sub(
        leading
            .chars()
            .count()
            .saturating_add(chip.chars().count())
            .saturating_add(1),
    );
    let separator = (separator_width > 0).then(|| format!(" {}", "─".repeat(separator_width)));
    Line::from(vec![
        Span::raw(leading),
        Span::styled(
            chip,
            Style::default()
                .fg(accent)
                .bg(palette.config_section_bg)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            separator.unwrap_or_default(),
            Style::default().fg(palette.text_muted),
        ),
    ])
}

#[allow(dead_code)]
fn selected_row_bg(accent: Color) -> Color {
    let palette = theme::default_palette();
    selected_row_bg_with_palette(accent, &palette)
}

fn selected_row_bg_with_palette(accent: Color, palette: &ThemePalette) -> Color {
    if accent == palette.config_primary {
        palette.config_selected_bg
    } else {
        palette.surface_selection
    }
}

fn config_line_is_meta(line: &str) -> bool {
    [
        "cfg:",
        "ws:",
        "servers:",
        "selected:",
        "overrides:",
        "docs:",
        "status:",
        "auth:",
        "api_key:",
        "root docs:",
        "args_csv:",
        "advanced:",
        "key:",
        "override:",
        "env:",
        "All unmatched",
        "No MCP servers",
        "MCP:",
    ]
    .iter()
    .any(|prefix| line.starts_with(prefix))
}

fn config_line_looks_like_field(line: &str) -> bool {
    matches!(line.chars().next(), Some(' ' | '>' | '*'))
}

#[cfg(all(test, not(sigil_tui_test_slice_app_input_flow)))]
#[path = "tests/setup_config_tests.rs"]
mod tests;

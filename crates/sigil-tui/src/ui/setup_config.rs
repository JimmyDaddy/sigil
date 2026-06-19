use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, BorderType, Borders, Paragraph, Wrap},
};

use crate::app::AppState;
use crate::config_panel::CONFIG_HEADER_NOTICE;

use super::{
    StatusKind,
    modal::render_modal,
    shell::render_status,
    status_indicator::{FocusKind, StatusIndicator, focus_style, focus_symbol, status_rest_style},
    theme,
};

pub(super) const CONFIG_DETAIL_SPLIT_MIN_WIDTH: u16 = 128;
pub(super) const CONFIG_DETAIL_PANEL_WIDTH: u16 = 52;
const CONFIG_CONTENT_MIN_WIDTH: u16 = 72;
const CONFIG_CONTENT_MAX_WIDTH: u16 = 152;
const CONFIG_SIDE_MARGIN_PERCENT: u16 = 8;
const CONFIG_FORM_LABEL_WIDTH: usize = 16;
const CONFIG_FORM_ACTION_CHIP_WIDTH: usize = 14;
const CONFIG_FOOTER_BUTTON_WIDTH: usize = 14;
pub(super) const CONFIG_FOOTER_COMPACT_WIDTH: u16 = 76;
const CONFIG_SCROLL_MARKER_WIDTH: u16 = 8;
const CONFIG_STATUS_MARKER_WIDTH: usize = 2;

pub(super) fn render_setup(frame: &mut Frame, app: &AppState) {
    let panel_bg = Color::Rgb(24, 22, 13);
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

    let detail = app
        .setup_lines()
        .into_iter()
        .map(|line| render_setup_line(&line))
        .collect::<Vec<_>>();

    let detail_widget = Paragraph::new(Text::from(detail))
        .block(
            Block::default()
                .title("Setup")
                .title_style(
                    Style::default()
                        .fg(Color::Black)
                        .bg(Color::Yellow)
                        .add_modifier(Modifier::BOLD),
                )
                .border_type(BorderType::Rounded)
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Yellow))
                .style(Style::default().bg(panel_bg)),
        )
        .style(Style::default().bg(panel_bg))
        .wrap(Wrap { trim: false });

    frame.render_widget(detail_widget, body[1]);
    render_modal(frame, app);
}

pub(super) fn render_config(frame: &mut Frame, app: &AppState) {
    let panel_bg = theme::config_panel_bg();
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
    render_config_header(frame, header_area, app, panel_bg);

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
        render_config_panel(frame, content[0], main_lines, panel_bg);
        render_config_context_panel(frame, content[2], app, context_lines, panel_bg);
    } else {
        render_config_panel(frame, panel_area, main_lines, panel_bg);
    }
    if footer_height > 0 {
        let footer_area = Rect {
            y: panel_area.y + panel_area.height + footer_gap,
            height: footer_height,
            ..content_area
        };
        render_config_footer(frame, footer_area, app, panel_bg);
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

fn render_config_header(frame: &mut Frame, area: Rect, app: &AppState, panel_bg: Color) {
    let content_width = area.width as usize;
    let section = app.config_section_title().unwrap_or("Config");
    let (state, state_kind) = if app.config_is_dirty() {
        ("unsaved", StatusKind::Warning)
    } else {
        ("saved", StatusKind::Success)
    };
    let state_style = if app.config_is_dirty() {
        Style::default()
            .fg(Color::Black)
            .bg(theme::config_warning())
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default()
            .fg(Color::Black)
            .bg(theme::config_primary())
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
            .fg(Color::Black)
            .bg(theme::config_primary())
            .add_modifier(Modifier::BOLD),
    );
    push_config_header_gap(&mut summary_spans, &mut remaining);
    push_config_header_span(
        &mut summary_spans,
        &mut remaining,
        section,
        Style::default()
            .fg(theme::config_detail())
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
    push_config_header_pair(&mut summary_spans, &mut remaining, "field", field);

    let notice_min_width = "hint ".chars().count() + 12;
    let file_value_width = content_width
        .saturating_sub("file ".chars().count() + 2 + notice_min_width)
        .min(content_width / 3);
    let file_value = fit_config_value(&file_label, file_value_width);
    let notice_width = content_width.saturating_sub(
        "file ".chars().count() + file_value.chars().count() + "  ".chars().count(),
    );
    let mut file_spans = vec![
        Span::styled("file ", Style::default().fg(theme::dim())),
        Span::styled(file_value, Style::default().fg(theme::muted())),
        Span::raw("  "),
    ];
    file_spans.extend(render_config_header_notice(notice, notice_width));
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

fn render_config_header_notice(notice: &str, width: usize) -> Vec<Span<'static>> {
    if width == 0 {
        return Vec::new();
    }
    let (marker, value_style) = if notice == CONFIG_HEADER_NOTICE {
        ("hint ", Style::default().fg(theme::muted()))
    } else {
        (
            "note ",
            Style::default()
                .fg(theme::config_warning())
                .add_modifier(Modifier::BOLD),
        )
    };
    let marker_width = marker.chars().count();
    let marker_style = Style::default()
        .fg(theme::dim())
        .bg(theme::config_tab_bg())
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

fn push_config_header_pair(
    spans: &mut Vec<Span<'static>>,
    remaining: &mut usize,
    label: &str,
    value: &str,
) {
    if !push_config_header_gap(spans, remaining) {
        return;
    }
    let label = format!("{label}: ");
    if *remaining <= label.chars().count() {
        push_config_header_span(spans, remaining, &label, Style::default().fg(theme::dim()));
        return;
    }
    push_config_header_span(spans, remaining, &label, Style::default().fg(theme::dim()));
    push_config_header_span(spans, remaining, value, Style::default().fg(theme::ink()));
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

fn render_config_panel(frame: &mut Frame, area: Rect, lines: Vec<String>, panel_bg: Color) {
    let details_index = lines.iter().position(|line| line == "[details]");
    let selected_line_indexes = lines
        .iter()
        .enumerate()
        .filter_map(|(index, line)| {
            (line.starts_with("> ") || line.starts_with("selected:")).then_some(index)
        })
        .collect::<Vec<_>>();
    let block = config_block("Config", theme::config_primary(), panel_bg);
    let content_area = block.inner(area);
    let content_width = content_area.width as usize;
    let detail = lines
        .into_iter()
        .enumerate()
        .map(|(index, line)| {
            if details_index == Some(index) {
                render_config_subsection_line(&line, theme::config_detail(), content_width)
            } else if details_index.is_some_and(|details_index| index > details_index) {
                render_config_context_line(&line, content_width)
            } else {
                render_config_line(index, &line, content_width)
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
    );
    render_config_selected_row_bgs(frame, content_area, &selected_line_indexes, scroll_offset);

    let widget =
        Paragraph::new(Text::from(detail)).scroll((scroll_offset.min(u16::MAX as usize) as u16, 0));

    frame.render_widget(widget, content_area);
}

fn render_config_selected_row_bgs(
    frame: &mut Frame,
    content_area: Rect,
    selected_line_indexes: &[usize],
    scroll_offset: usize,
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
            Block::default().style(Style::default().bg(theme::config_selected_bg())),
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
    let block = config_block("Details", theme::config_detail(), panel_bg);
    let content_area = block.inner(area);
    let content_width = content_area.width as usize;
    let detail = lines
        .into_iter()
        .map(|line| render_config_context_line(&line, content_width))
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
    );
    render_config_selected_row_bgs(frame, content_area, &selected_line_indexes, scroll_offset);

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
        render_config_scroll_marker(frame, marker_x, area.y, " more ^ ");
    }
    if bottom_hidden {
        render_config_scroll_marker(
            frame,
            marker_x,
            area.y + area.height.saturating_sub(1),
            " more v ",
        );
    }
}

fn render_config_scroll_marker(frame: &mut Frame, x: u16, y: u16, text: &'static str) {
    let marker = Paragraph::new(Line::from(vec![Span::styled(
        text,
        Style::default()
            .fg(Color::Black)
            .bg(theme::config_warning())
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

fn config_block(title: &'static str, accent: Color, panel_bg: Color) -> Block<'static> {
    Block::default()
        .title(title)
        .title_style(
            Style::default()
                .fg(Color::Black)
                .bg(accent)
                .add_modifier(Modifier::BOLD),
        )
        .border_type(BorderType::Rounded)
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme::config_border()))
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

fn render_config_footer(frame: &mut Frame, area: Rect, app: &AppState, panel_bg: Color) {
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
            footer_action_accent(label),
            compact,
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
            .fg(theme::config_danger())
            .add_modifier(Modifier::BOLD)
    } else if app.config_is_dirty() {
        Style::default()
            .fg(theme::config_warning())
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(theme::muted())
    };
    let gap = if compact {
        " | ".to_owned()
    } else {
        " ".repeat(gap_width)
    };
    let mut spans = action_spans;
    spans.push(Span::raw(gap));
    spans.extend(footer_status_spans(
        &app.config_footer_hint(),
        status_width,
        status_style,
    ));
    let line = Line::from(spans);
    let footer = Paragraph::new(Text::from(vec![line]))
        .style(Style::default().bg(panel_bg))
        .wrap(Wrap { trim: false });
    frame.render_widget(footer, area);
}

fn footer_action_accent(label: &str) -> Color {
    match label {
        "save" => theme::config_primary(),
        "save+close" => theme::config_warning(),
        "activate" => theme::config_detail(),
        "close" => theme::config_danger(),
        _ => theme::config_primary(),
    }
}

fn footer_action_span(
    label: &'static str,
    selected: bool,
    accent: Color,
    compact: bool,
) -> Span<'static> {
    let text = footer_action_text(label, selected, compact);
    let style = if selected {
        Style::default()
            .fg(Color::Black)
            .bg(accent)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default()
            .fg(accent)
            .bg(theme::config_tab_bg())
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

fn footer_status_spans(value: &str, width: usize, value_style: Style) -> Vec<Span<'static>> {
    if width == 0 {
        return Vec::new();
    }
    let status_value = footer_status_value(value);
    let kind = config_status_kind_for_value("status", status_value).unwrap_or(StatusKind::Unknown);
    let indicator = StatusIndicator::static_kind(kind);
    if width == 1 {
        return vec![indicator.span()];
    }
    let value = fit_config_value(
        status_value,
        width.saturating_sub(CONFIG_STATUS_MARKER_WIDTH),
    );
    vec![
        indicator.span(),
        Span::raw(" "),
        Span::styled(value, config_status_value_style(kind, value_style)),
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

fn config_status_value_style(kind: StatusKind, fallback: Style) -> Style {
    match kind {
        StatusKind::Unknown | StatusKind::Pending => fallback,
        _ => status_rest_style(kind),
    }
}

fn config_status_value_spans(
    label: &str,
    value: &str,
    width: usize,
    fallback_style: Style,
) -> Vec<Span<'static>> {
    if width == 0 {
        return Vec::new();
    }
    let Some(kind) = config_status_kind_for_value(label, value) else {
        return vec![Span::styled(fit_config_value(value, width), fallback_style)];
    };
    let indicator = StatusIndicator::animated(kind);
    if width == 1 {
        return vec![indicator.span()];
    }
    let value = fit_config_value(value, width.saturating_sub(CONFIG_STATUS_MARKER_WIDTH));
    vec![
        indicator.span(),
        Span::raw(" "),
        Span::styled(value, config_status_value_style(kind, fallback_style)),
    ]
}

fn render_config_line(index: usize, line: &str, content_width: usize) -> Line<'static> {
    if line.is_empty() {
        return Line::raw(String::new());
    }
    if index == 0 {
        return render_config_title_line(line);
    }
    if index == 1 {
        return render_config_step_line(line, theme::config_primary());
    }
    if line.starts_with('[') && line.ends_with(']') {
        return render_config_subsection_line(line, theme::config_primary(), content_width);
    }
    if let Some(line) = render_readonly_line(line, content_width) {
        return line;
    }
    if let Some(line) = render_hint_line(line) {
        return line;
    }
    if let Some(line) = render_form_line(line, theme::config_primary(), content_width) {
        return line;
    }
    if line.starts_with("Type value")
        || line.starts_with("Tab ")
        || line.starts_with("Enter ")
        || line.starts_with("Ctrl-")
    {
        return Line::styled(
            line.to_owned(),
            Style::default().fg(theme::config_warning()),
        );
    }
    if config_line_is_meta(line) {
        return Line::styled(line.to_owned(), Style::default().fg(theme::dim()));
    }
    if config_line_looks_like_field(line) {
        return Line::styled(line.to_owned(), Style::default().fg(theme::ink()));
    }

    Line::styled(line.to_owned(), Style::default().fg(theme::muted()))
}

fn render_config_context_line(line: &str, content_width: usize) -> Line<'static> {
    if line.is_empty() {
        return Line::raw(String::new());
    }
    if let Some((label, value)) = line.split_once(':') {
        return render_config_context_pair(label, value.trim_start(), content_width);
    }

    render_config_context_info_line(line, content_width)
}

fn render_config_context_pair(label: &str, value: &str, content_width: usize) -> Line<'static> {
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
                    focus_style(FocusKind::Selected).add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    value,
                    Style::default()
                        .fg(theme::ink())
                        .bg(theme::config_selected_bg())
                        .add_modifier(Modifier::BOLD),
                ),
            ])
            .style(Style::default().bg(theme::config_selected_bg()))
        }
        "controls" | "actions" | "mcp" => {
            render_config_context_commands(label, value, content_width)
        }
        "status" => render_config_status_line(value, content_width),
        "key" | "advanced" | "override" => render_config_metadata_line(label, value, content_width),
        _ => {
            let label_text = format!("{label}: ");
            let value_width =
                available_config_value_width(content_width, label_text.chars().count(), 0);
            let mut spans = vec![Span::styled(
                label_text,
                Style::default()
                    .fg(theme::config_detail())
                    .add_modifier(Modifier::BOLD),
            )];
            spans.extend(config_status_value_spans(
                label,
                value,
                value_width,
                Style::default().fg(theme::ink()),
            ));
            Line::from(spans)
        }
    }
}

fn render_config_context_commands(label: &str, value: &str, content_width: usize) -> Line<'static> {
    let marker = config_context_command_marker(label);
    let marker_width = marker.chars().count();
    let marker_style = Style::default()
        .fg(theme::dim())
        .bg(theme::config_tab_bg())
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
                Style::default().fg(theme::dim()),
            ));
            break;
        }
        let token_capacity = remaining.saturating_sub(separator_width);
        if index > 0 {
            spans.push(Span::styled(separator, Style::default().fg(theme::dim())));
        }
        let (rendered_width, truncated) =
            push_config_command_token(&mut spans, token, token_capacity);
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

fn push_config_command_token(
    spans: &mut Vec<Span<'static>>,
    token: &str,
    capacity: usize,
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
            .fg(theme::config_warning())
            .bg(theme::config_tab_bg())
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
        Style::default().fg(theme::muted()),
    ));
    (
        key_width + suffix_width,
        suffix_text.chars().count() > suffix_width,
    )
}

fn render_config_context_info_line(line: &str, content_width: usize) -> Line<'static> {
    let text = fit_config_value(
        line,
        available_config_value_width(content_width, "i ".chars().count(), 0),
    );
    Line::from(vec![
        Span::styled(
            "i ",
            Style::default()
                .fg(theme::config_warning())
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            text,
            Style::default()
                .fg(theme::muted())
                .add_modifier(Modifier::ITALIC),
        ),
    ])
}

fn render_config_metadata_line(label: &str, value: &str, content_width: usize) -> Line<'static> {
    let marker = config_metadata_marker(label);
    let value = fit_config_value(
        value,
        available_config_value_width(content_width, marker.chars().count(), 0),
    );
    Line::from(vec![
        Span::styled(
            marker,
            Style::default()
                .fg(theme::dim())
                .bg(theme::config_tab_bg())
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(value, Style::default().fg(theme::muted())),
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

fn render_config_status_line(value: &str, content_width: usize) -> Line<'static> {
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
            .fg(theme::config_danger())
            .add_modifier(Modifier::BOLD)
    } else if value.contains("unsaved") {
        Style::default()
            .fg(theme::config_warning())
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(theme::config_primary())
    };
    let indicator = StatusIndicator::static_kind(kind);
    let value = fit_config_value(
        value,
        content_width.saturating_sub(CONFIG_STATUS_MARKER_WIDTH),
    );
    Line::from(vec![
        indicator.span(),
        Span::raw(" "),
        Span::styled(value, value_style),
    ])
}

fn render_readonly_line(line: &str, content_width: usize) -> Option<Line<'static>> {
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
        indicator.span(),
        Span::raw(" "),
        Span::styled(padded_label, Style::default().fg(theme::muted())),
        Span::styled(": ", Style::default().fg(theme::dim())),
    ];
    spans.extend(config_status_value_spans(
        label,
        value,
        value_width,
        Style::default().fg(theme::muted()),
    ));

    Some(Line::from(spans))
}

fn render_hint_line(line: &str) -> Option<Line<'static>> {
    let text = line.strip_prefix("i ")?;

    Some(Line::from(vec![
        Span::styled(
            "i ",
            Style::default()
                .fg(theme::config_warning())
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            text.to_owned(),
            Style::default()
                .fg(theme::muted())
                .add_modifier(Modifier::ITALIC),
        ),
    ]))
}

fn render_setup_line(line: &str) -> Line<'static> {
    if line.is_empty() {
        return Line::raw(String::new());
    }
    if line.starts_with('[') && line.ends_with(']') {
        return render_subsection_line(line, Color::Yellow);
    }
    if let Some(line) = render_form_line(line, Color::Yellow, usize::MAX) {
        return line;
    }
    if line.starts_with("Enter ")
        || line.starts_with("Type custom")
        || line.starts_with("Ctrl-")
        || line.starts_with("auth=")
    {
        return Line::styled(line.to_owned(), Style::default().fg(Color::Yellow));
    }
    if line.starts_with("defaults:") {
        return Line::styled(line.to_owned(), Style::default().fg(Color::DarkGray));
    }
    if line == "Quick setup" {
        return Line::styled(
            line.to_owned(),
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        );
    }
    Line::styled(line.to_owned(), Style::default().fg(Color::Gray))
}

fn render_form_line(line: &str, accent: Color, content_width: usize) -> Option<Line<'static>> {
    let content_width = content_width.min(CONFIG_CONTENT_MAX_WIDTH as usize);
    let row_bg = selected_row_bg(accent);
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
                .fg(Color::Black)
                .bg(accent)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(theme::dim())
        };
        let value_style = if selected {
            Style::default()
                .fg(Color::Black)
                .bg(accent)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(theme::ink())
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
            .fg(Color::Black)
            .bg(accent)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(theme::dim())
    };
    let label_style = if selected {
        Style::default()
            .fg(accent)
            .bg(row_bg)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(theme::config_detail())
    };
    let value_style = if selected {
        Style::default()
            .fg(theme::ink())
            .bg(row_bg)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(theme::ink())
    };
    let colon_style = if selected {
        Style::default().fg(theme::dim()).bg(row_bg)
    } else {
        Style::default().fg(theme::dim())
    };
    let action_display = action.as_deref().map(config_action_display_label);
    let action_width = action_display.map_or(0, |action| 2 + config_action_chip_width(action));
    let value_width =
        available_config_value_width(content_width, 2 + label_width + 2, action_width);
    let mut value_spans = config_status_value_spans(label, value, value_width, value_style);
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
                    .fg(Color::Black)
                    .bg(accent)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(theme::config_warning())
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

fn render_config_title_line(line: &str) -> Line<'static> {
    let Some((title, rest)) = line.split_once(' ') else {
        return Line::styled(
            line.to_owned(),
            Style::default()
                .fg(theme::ink())
                .add_modifier(Modifier::BOLD),
        );
    };
    let (position, summary) = rest.split_once(" · ").unwrap_or((rest, ""));
    let mut spans = vec![
        Span::styled(
            title.to_owned(),
            Style::default()
                .fg(theme::ink())
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(" "),
        Span::styled(
            position.to_owned(),
            Style::default().fg(theme::config_warning()),
        ),
    ];
    if !summary.is_empty() {
        spans.push(Span::raw("  "));
        spans.push(Span::styled(
            summary.to_owned(),
            Style::default().fg(theme::muted()),
        ));
    }
    Line::from(spans)
}

fn render_config_step_line(line: &str, accent: Color) -> Line<'static> {
    let mut spans = Vec::new();
    for (index, token) in line.split_whitespace().enumerate() {
        if index > 0 {
            spans.push(Span::raw("  "));
        }
        let (text, style) = if token.starts_with('[') && token.ends_with(']') {
            (
                format!(" {} ", token.trim_start_matches('[').trim_end_matches(']')),
                Style::default()
                    .fg(Color::Black)
                    .bg(accent)
                    .add_modifier(Modifier::BOLD),
            )
        } else {
            (token.to_owned(), Style::default().fg(theme::muted()))
        };
        spans.push(Span::styled(text, style));
    }
    Line::from(spans)
}

fn render_subsection_line(line: &str, accent: Color) -> Line<'static> {
    let text = line.trim_start_matches('[').trim_end_matches(']');
    Line::from(vec![
        Span::styled(
            format!(" {text} "),
            Style::default()
                .fg(accent)
                .bg(theme::config_section_bg())
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(" "),
    ])
}

fn render_config_subsection_line(line: &str, accent: Color, content_width: usize) -> Line<'static> {
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
                .bg(theme::config_section_bg())
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            separator.unwrap_or_default(),
            Style::default().fg(theme::dim()),
        ),
    ])
}

fn selected_row_bg(accent: Color) -> Color {
    match accent {
        Color::Yellow => Color::Rgb(51, 43, 14),
        Color::Green => Color::Rgb(14, 36, 22),
        Color::Cyan => Color::Rgb(14, 32, 36),
        _ if accent == theme::config_primary() => theme::config_selected_bg(),
        _ => Color::Rgb(28, 32, 30),
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
        "Ctrl-N",
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

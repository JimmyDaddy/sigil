use std::time::{SystemTime, UNIX_EPOCH};
use std::{collections::BTreeSet, env, path::Path};

use ratatui::{
    Frame,
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, BorderType, Borders, Clear, Paragraph, Wrap},
};
use serde_json::Value;
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use crate::app::{AppState, PaneFocus, RunPhase, TimelineEntry, TimelineRole};

mod approval;
use approval::render_approval_modal;

fn shell_bg() -> Color {
    Color::Rgb(7, 8, 10)
}

fn rail_bg() -> Color {
    Color::Rgb(26, 28, 34)
}

fn composer_bg() -> Color {
    Color::Rgb(24, 26, 31)
}

fn composer_input_bg() -> Color {
    Color::Rgb(18, 20, 25)
}

fn selector_bg() -> Color {
    Color::Rgb(19, 21, 27)
}

fn selector_shadow_bg() -> Color {
    Color::Rgb(10, 11, 15)
}

fn selector_accent() -> Color {
    Color::Rgb(242, 171, 122)
}

fn user_message_bg() -> Color {
    Color::Rgb(20, 22, 27)
}

fn ink() -> Color {
    Color::Rgb(236, 240, 246)
}

fn muted() -> Color {
    Color::Rgb(149, 158, 173)
}

fn dim() -> Color {
    Color::Rgb(99, 109, 126)
}

fn accent_teal() -> Color {
    Color::Rgb(126, 180, 226)
}

fn accent_blue() -> Color {
    Color::Rgb(148, 178, 244)
}

fn accent_gold() -> Color {
    Color::Rgb(196, 176, 128)
}

fn accent_lime() -> Color {
    Color::Rgb(145, 182, 170)
}

fn accent_rose() -> Color {
    Color::Rgb(198, 142, 150)
}

fn badge_bg() -> Color {
    Color::Rgb(30, 35, 43)
}

#[allow(dead_code)]
fn themed_block(title: &str, subtitle: Option<&str>, accent: Color, bg: Color) -> Block<'static> {
    let mut spans = vec![Span::styled(
        format!(" {title} "),
        Style::default()
            .fg(accent)
            .bg(badge_bg())
            .add_modifier(Modifier::BOLD),
    )];
    if let Some(subtitle) = subtitle {
        spans.push(Span::raw(" "));
        spans.push(Span::styled(
            subtitle.to_owned(),
            Style::default().fg(muted()),
        ));
    }

    Block::default()
        .title(Line::from(spans))
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(accent))
        .style(Style::default().bg(bg))
}

fn section_badge(label: &str, accent: Color) -> Span<'static> {
    Span::styled(
        format!(" {label} "),
        Style::default()
            .fg(accent)
            .bg(badge_bg())
            .add_modifier(Modifier::BOLD),
    )
}

fn phase_accent(phase: &RunPhase) -> Color {
    match phase {
        RunPhase::Idle => accent_teal(),
        RunPhase::Thinking => accent_gold(),
        RunPhase::Tool(_) => accent_rose(),
        RunPhase::Streaming => accent_blue(),
    }
}

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

    render_live_panel(frame, main[0], app);
    render_input(frame, main[1], app);
    render_footer_status(frame, main[2], app);
    render_slash_selector_overlay(frame, main[0], main[1], app);
    render_info_rail(frame, shell[1], app);

    if app.pending_approval.is_some() {
        render_approval_modal(frame, app);
    }

    if app.active_pane == PaneFocus::Composer {
        let (cursor_col, cursor_row) = app.input_cursor_visual_position();
        if let Some((cursor_x, cursor_y)) = composer_cursor_origin(main[1], app) {
            frame.set_cursor_position((
                cursor_x.saturating_add(cursor_col),
                cursor_y.saturating_add(cursor_row),
            ));
        }
    }
}

fn render_live_panel(frame: &mut Frame, area: Rect, app: &AppState) {
    frame.render_widget(
        Block::default().style(Style::default().bg(shell_bg())),
        area,
    );
    if area.width == 0 || area.height == 0 {
        return;
    }

    let inner = inset_rect(area, 2, 0);
    if inner.width == 0 || inner.height == 0 {
        return;
    }

    let activity = render_live_activity_line(app, phase_accent(&app.run_phase()));
    let transcript_rows = inner
        .height
        .saturating_sub(u16::from(activity.is_some()))
        .max(1) as usize;
    let mut lines = app.transcript_lines(transcript_rows);
    if let Some(activity_line) = activity {
        lines.push(activity_line);
    }
    while lines.len() > inner.height as usize {
        let _ = lines.remove(0);
    }
    let content_height = lines.len() as u16;
    let content_y = inner
        .y
        .saturating_add(inner.height.saturating_sub(content_height));
    let content_area = Rect::new(inner.x, content_y, inner.width, content_height);
    frame.render_widget(
        Paragraph::new(Text::from(lines))
            .style(Style::default().bg(shell_bg()))
            .wrap(Wrap { trim: false }),
        content_area,
    );
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

fn render_status(frame: &mut Frame, area: Rect, app: &AppState) {
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

fn render_live_activity_line(app: &AppState, accent: Color) -> Option<Line<'static>> {
    let summary = app.live_activity_summary()?;
    let spinner = live_spinner_frame();
    Some(Line::from(vec![
        Span::styled(
            format!("{spinner} {}", summary.label),
            Style::default().fg(accent).add_modifier(Modifier::BOLD),
        ),
        Span::raw("  "),
        Span::styled(summary.detail, Style::default().fg(ink())),
    ]))
}

fn live_spinner_frame() -> &'static str {
    const FRAMES: &[&str] = &["◴", "◷", "◶", "◵"];
    let tick = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() / 120)
        .unwrap_or(0);
    FRAMES[(tick as usize) % FRAMES.len()]
}

#[derive(Clone, Default)]
pub(crate) struct TimelineRenderOptions {
    pub expand_tool_previews: bool,
    pub expand_thinking_blocks: bool,
    pub selected_tool_entry: Option<usize>,
    pub expanded_tool_entries: BTreeSet<usize>,
    pub max_content_width: usize,
}

#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn render_timeline_entry_lines(entry: &TimelineEntry) -> Vec<Line<'static>> {
    let options = TimelineRenderOptions::default();
    render_timeline_entry_lines_with_options(entry, &options, 0)
}

pub(crate) fn render_timeline_entry_lines_with_options(
    entry: &TimelineEntry,
    options: &TimelineRenderOptions,
    entry_index: usize,
) -> Vec<Line<'static>> {
    let lines = if entry.role == TimelineRole::User {
        render_user_entry_lines(entry, options.max_content_width)
    } else if entry.role == TimelineRole::Assistant {
        render_assistant_entry_lines(entry, options.max_content_width)
    } else if entry.role == TimelineRole::Phase {
        render_phase_entry_lines(entry)
    } else if entry.role == TimelineRole::Thinking {
        render_thinking_entry_lines(
            entry,
            options.expand_thinking_blocks,
            options.max_content_width,
        )
    } else if entry.role == TimelineRole::Tool {
        render_tool_entry_lines(entry, options, entry_index)
    } else if entry.role == TimelineRole::Notice {
        render_notice_entry_lines(entry)
    } else {
        let mut lines = vec![timeline_header_line("system", Color::Cyan, "")];
        let mut markdown_state = MarkdownRenderState::default();
        if !entry.text.is_empty() {
            for chunk in entry.text.split('\n') {
                let content = render_timeline_content_spans(
                    entry.role,
                    chunk,
                    Style::default().fg(muted()),
                    &mut markdown_state,
                );
                lines.push(timeline_content_line(Color::Cyan, content));
            }
        }
        lines
    };
    append_entry_gap(lines)
}

fn append_entry_gap(mut lines: Vec<Line<'static>>) -> Vec<Line<'static>> {
    if !lines.is_empty() {
        lines.push(Line::raw(String::new()));
    }
    lines
}

fn render_info_rail(frame: &mut Frame, area: Rect, app: &AppState) {
    frame.render_widget(Block::default().style(Style::default().bg(rail_bg())), area);
    let inner = inset_rect(area, 3, 1);
    if inner.width == 0 || inner.height == 0 {
        return;
    }
    let mut lines = vec![Line::from(vec![
        Span::styled(
            "info",
            Style::default()
                .fg(accent_blue())
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw("  "),
        Span::styled(
            truncate_display_width(&app.session_display_title(), inner.width as usize),
            Style::default().fg(ink()).add_modifier(Modifier::BOLD),
        ),
    ])];
    lines.push(Line::from(vec![Span::styled(
        truncate_display_width(
            &display_path_label(&app.workspace_root),
            inner.width as usize,
        ),
        Style::default().fg(dim()),
    )]));
    lines.push(Line::raw(String::new()));

    push_info_section(
        &mut lines,
        "session",
        accent_blue(),
        app.session_sidebar_lines()
            .into_iter()
            .chain(std::iter::once(if app.memory_enabled {
                format!(
                    "memory: {} docs · {}",
                    app.memory_document_count, app.memory_last_status
                )
            } else {
                "memory: off".to_owned()
            })),
        inner.width as usize,
    );
    push_info_section(
        &mut lines,
        "permissions",
        accent_gold(),
        app.permission_card_lines(),
        inner.width as usize,
    );
    push_info_section(
        &mut lines,
        "agents",
        accent_lime(),
        app.agent_sidebar_rows().into_iter().map(|row| {
            format!(
                "{} {}: {}",
                if row.selected { ">" } else { "-" },
                row.label,
                row.detail
            )
        }),
        inner.width as usize,
    );
    push_info_section(
        &mut lines,
        "usage",
        accent_teal(),
        app.usage_sidebar_lines().iter().cloned(),
        inner.width as usize,
    );
    push_info_section(
        &mut lines,
        "controls",
        accent_rose(),
        [
            "/ or 、: command palette".to_owned(),
            "Shift-Tab: write mode".to_owned(),
            format!("Ctrl-C: {}", if app.is_busy { "cancel" } else { "quit" }),
            "Ctrl-T: thinking".to_owned(),
        ],
        inner.width as usize,
    );

    frame.render_widget(
        Paragraph::new(Text::from(lines))
            .style(Style::default().bg(rail_bg()))
            .wrap(Wrap { trim: false }),
        inner,
    );
}

fn push_info_section<I>(
    lines: &mut Vec<Line<'static>>,
    title: &str,
    accent: Color,
    values: I,
    width: usize,
) where
    I: IntoIterator<Item = String>,
{
    lines.push(Line::from(vec![section_badge(title, accent)]));
    for value in values {
        lines.push(render_info_line(&value, width));
    }
    lines.push(Line::raw(String::new()));
}

fn render_info_line(value: &str, width: usize) -> Line<'static> {
    let clipped = truncate_display_width(value, width.saturating_sub(2).max(1));
    if let Some((label, rest)) = clipped.split_once(": ") {
        return Line::from(vec![
            Span::raw("  "),
            Span::styled(
                format!("{label}:"),
                Style::default().fg(dim()).add_modifier(Modifier::BOLD),
            ),
            Span::raw(" "),
            Span::styled(rest.to_owned(), Style::default().fg(ink())),
        ]);
    }

    if let Some((marker, rest)) = clipped.split_once(' ')
        && matches!(marker, ">" | "-")
    {
        return Line::from(vec![
            Span::raw("  "),
            Span::styled(
                format!("{marker} "),
                if marker == ">" {
                    Style::default()
                        .fg(accent_blue())
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(dim())
                },
            ),
            Span::styled(rest.to_owned(), Style::default().fg(ink())),
        ]);
    }

    Line::from(vec![
        Span::raw("  "),
        Span::styled(clipped, Style::default().fg(ink())),
    ])
}

fn sidebar_width_for_terminal(total_width: usize) -> usize {
    let min = if total_width < 72 { 16 } else { 24 };
    let max = if total_width < 72 { 24 } else { 42 };
    ((total_width * 30) / 100).clamp(min, max)
}

fn timeline_badge(label: &str, color: Color) -> Span<'static> {
    Span::styled(
        format!(" {label} "),
        Style::default()
            .fg(color)
            .bg(badge_bg())
            .add_modifier(Modifier::BOLD),
    )
}

fn timeline_header_line(label: &str, accent: Color, subtitle: &str) -> Line<'static> {
    let mut spans = vec![Span::styled(
        label.to_owned(),
        Style::default().fg(accent).add_modifier(Modifier::BOLD),
    )];
    if !subtitle.is_empty() {
        spans.push(Span::raw(" "));
        spans.push(Span::styled(
            subtitle.to_owned(),
            Style::default().fg(dim()).add_modifier(Modifier::BOLD),
        ));
    }
    Line::from(spans)
}

fn timeline_minor_header_line(label: &str, accent: Color, detail: &str) -> Line<'static> {
    let mut spans = vec![Span::styled(label.to_owned(), Style::default().fg(accent))];
    if !detail.is_empty() {
        spans.push(Span::raw(" "));
        spans.push(Span::styled(detail.to_owned(), Style::default().fg(dim())));
    }
    Line::from(spans)
}

fn timeline_content_line(_accent: Color, spans: Vec<Span<'static>>) -> Line<'static> {
    let mut line = vec![Span::raw("  ")];
    line.extend(spans);
    Line::from(line)
}

fn spans_with_background(spans: Vec<Span<'static>>, bg: Color) -> Vec<Span<'static>> {
    spans
        .into_iter()
        .map(|span| {
            let mut style = span.style;
            style.bg = Some(bg);
            Span::styled(span.content, style)
        })
        .collect()
}

fn timeline_section_line(
    rail_accent: Color,
    badge_label: &str,
    badge_accent: Color,
    detail_spans: Vec<Span<'static>>,
) -> Line<'static> {
    let mut spans = vec![section_badge(badge_label, badge_accent)];
    if !detail_spans.is_empty() {
        spans.push(Span::raw(" "));
        spans.extend(detail_spans);
    }
    timeline_content_line(rail_accent, spans)
}

fn render_user_entry_lines(entry: &TimelineEntry, max_content_width: usize) -> Vec<Line<'static>> {
    let accent = selector_accent();
    let bubble_bg = user_message_bg();
    let mut lines = Vec::new();
    if entry.text.trim().is_empty() {
        return lines;
    }
    let content_width = max_content_width.saturating_sub(8).max(18);
    lines.push(user_bubble_padding_line(accent, bubble_bg, content_width));
    for line in entry.text.lines() {
        if line.trim().is_empty() {
            lines.push(Line::raw(String::new()));
            continue;
        }
        for row in wrap_display_width(line, content_width) {
            lines.push(user_bubble_content_line(
                &row,
                accent,
                bubble_bg,
                content_width,
            ));
        }
    }
    lines.push(user_bubble_padding_line(accent, bubble_bg, content_width));
    lines
}

fn user_bubble_padding_line(
    accent: Color,
    bubble_bg: Color,
    content_width: usize,
) -> Line<'static> {
    let mut spans = vec![Span::styled(
        "▌  ",
        Style::default()
            .fg(accent)
            .bg(bubble_bg)
            .add_modifier(Modifier::BOLD),
    )];
    spans.push(Span::styled(
        " ".repeat(content_width),
        Style::default().bg(bubble_bg),
    ));
    spans.push(Span::styled("  ", Style::default().bg(bubble_bg)));
    Line::from(spans)
}

fn user_bubble_content_line(
    row: &str,
    accent: Color,
    bubble_bg: Color,
    content_width: usize,
) -> Line<'static> {
    let padded = pad_display_width(row, content_width);
    let mut spans = vec![Span::styled(
        "▌  ",
        Style::default()
            .fg(accent)
            .bg(bubble_bg)
            .add_modifier(Modifier::BOLD),
    )];
    spans.extend(spans_with_background(
        render_inline_markdown_spans(
            &padded,
            Style::default()
                .fg(Color::Rgb(230, 236, 244))
                .add_modifier(Modifier::BOLD),
        ),
        bubble_bg,
    ));
    spans.push(Span::styled("  ", Style::default().bg(bubble_bg)));
    Line::from(spans)
}

fn render_assistant_entry_lines(
    entry: &TimelineEntry,
    max_content_width: usize,
) -> Vec<Line<'static>> {
    let accent = accent_blue();
    if entry.text.trim().is_empty() {
        return Vec::new();
    }
    render_markdown_timeline_lines(
        accent,
        Style::default().fg(ink()),
        &entry.text,
        max_content_width,
    )
}

fn render_thinking_entry_lines(
    entry: &TimelineEntry,
    expanded: bool,
    max_content_width: usize,
) -> Vec<Line<'static>> {
    let accent = Color::Rgb(158, 148, 120);
    let body_style = Style::default()
        .fg(Color::Rgb(170, 166, 152))
        .add_modifier(Modifier::ITALIC);
    let mut lines = vec![Line::from(vec![
        Span::styled(
            "thought",
            Style::default()
                .fg(accent)
                .add_modifier(Modifier::ITALIC | Modifier::BOLD),
        ),
        Span::raw("  "),
        Span::styled(
            if expanded {
                format!(
                    "{} lines · Ctrl-T collapse",
                    thinking_line_count(&entry.text)
                )
            } else {
                format!(
                    "{} · {} lines · Ctrl-T expand",
                    summarize_thinking_text(&entry.text, 64),
                    thinking_line_count(&entry.text)
                )
            },
            Style::default().fg(dim()).add_modifier(Modifier::ITALIC),
        ),
    ])];
    if entry.text.trim().is_empty() {
        return lines;
    }
    if !expanded {
        return lines;
    }
    lines.extend(render_markdown_timeline_lines(
        accent,
        body_style,
        &entry.text,
        max_content_width,
    ));
    lines
}

fn render_phase_entry_lines(entry: &TimelineEntry) -> Vec<Line<'static>> {
    let (kind, detail) = entry
        .text
        .split_once('|')
        .map(|(kind, detail)| (kind, Some(detail)))
        .unwrap_or((entry.text.as_str(), None));
    let (label, accent, summary) = match kind {
        "thinking" => (
            "thinking",
            accent_gold(),
            detail
                .map(|model| format!("reasoning with {model}"))
                .unwrap_or_else(|| "reasoning".to_owned()),
        ),
        "tool" => (
            "tool",
            accent_rose(),
            detail
                .map(|tool| format!("running {tool}"))
                .unwrap_or_else(|| "running tool".to_owned()),
        ),
        "streaming" => ("streaming", accent_blue(), "writing the reply".to_owned()),
        _ => ("phase", muted(), entry.text.clone()),
    };

    vec![
        timeline_minor_header_line(label, accent, "live"),
        timeline_content_line(
            accent,
            vec![Span::styled(summary, Style::default().fg(dim()))],
        ),
    ]
}

fn thinking_line_count(text: &str) -> usize {
    text.lines()
        .filter(|line| !line.trim().is_empty())
        .count()
        .max(1)
}

fn summarize_thinking_text(text: &str, max_chars: usize) -> String {
    let first = text
        .lines()
        .find_map(|line| {
            let trimmed = line.trim();
            (!trimmed.is_empty()).then_some(trimmed)
        })
        .unwrap_or("thinking hidden");
    if first.chars().count() <= max_chars {
        return first.to_owned();
    }
    let truncated = first.chars().take(max_chars).collect::<String>();
    format!("{truncated}...")
}

fn render_notice_entry_lines(entry: &TimelineEntry) -> Vec<Line<'static>> {
    let accent = notice_accent(&entry.text);
    let mut lines = vec![timeline_header_line(
        "notice",
        accent,
        notice_tone_label(accent),
    )];
    for line in entry.text.lines() {
        if line.trim().is_empty() {
            continue;
        }
        lines.push(timeline_content_line(
            accent,
            render_notice_body_spans(line, accent),
        ));
    }
    lines
}

fn render_tool_entry_lines(
    entry: &TimelineEntry,
    options: &TimelineRenderOptions,
    entry_index: usize,
) -> Vec<Line<'static>> {
    let summary = parse_tool_summary(&entry.text);
    let accent = accent_rose();
    let selected = options.selected_tool_entry == Some(entry_index);
    let expanded =
        options.expand_tool_previews || options.expanded_tool_entries.contains(&entry_index);
    let mut lines = vec![tool_card_header_line(&summary, selected, expanded)];
    let mut status_line = vec![Span::styled(
        summary.status.clone(),
        tool_status_style(summary.is_error),
    )];
    if let Some(call_id) = &summary.call_id {
        status_line.push(Span::raw(" "));
        status_line.push(Span::styled(
            format!("call {}", truncate_inline_text(call_id, 28)),
            Style::default().fg(dim()),
        ));
    }
    if let Some(ref summary_line) = summary.summary {
        status_line.push(Span::raw(" "));
        status_line.push(Span::styled(
            summary_line.clone(),
            Style::default().fg(ink()),
        ));
    }
    lines.push(timeline_content_line(accent, status_line));
    if let Some(ref metadata_line) = summary.metadata_line {
        lines.push(timeline_section_line(
            accent,
            "meta",
            accent,
            vec![Span::styled(
                metadata_line.clone(),
                Style::default().fg(muted()),
            )],
        ));
    }
    if !summary.preview_lines.is_empty() || summary.preview_value.is_some() {
        if expanded {
            lines.extend(render_tool_preview_body(
                &summary,
                accent,
                options.max_content_width,
            ));
        } else {
            let available_lines = summary.preview_lines.len() + summary.hidden_lines;
            lines.push(timeline_content_line(
                accent,
                vec![Span::styled(
                    format!(
                        "{} hidden · {} lines available · /tool open",
                        summary.preview_kind.description(),
                        available_lines
                    ),
                    Style::default()
                        .fg(if selected { accent_blue() } else { dim() })
                        .add_modifier(Modifier::BOLD),
                )],
            ));
        }
    }
    lines
}

fn tool_card_header_line(
    summary: &ToolCardRender,
    selected: bool,
    expanded: bool,
) -> Line<'static> {
    let accent = accent_rose();
    let mut spans = vec![
        Span::styled(
            "▎",
            Style::default().fg(accent).add_modifier(Modifier::BOLD),
        ),
        Span::raw(" "),
        timeline_badge("tool", accent),
        Span::raw(" "),
        Span::styled(
            summary.tool_name.clone(),
            Style::default().fg(ink()).add_modifier(Modifier::BOLD),
        ),
    ];
    if selected {
        spans.push(Span::raw(" "));
        spans.push(section_badge("focus", accent_blue()));
    }
    if expanded {
        spans.push(Span::raw(" "));
        spans.push(section_badge("open", accent_lime()));
    }
    spans.push(Span::raw(" "));
    spans.push(Span::styled(
        summary.preview_kind.label().to_owned(),
        Style::default().fg(dim()).add_modifier(Modifier::BOLD),
    ));
    Line::from(spans)
}

fn render_tool_preview_body(
    summary: &ToolCardRender,
    accent: Color,
    max_content_width: usize,
) -> Vec<Line<'static>> {
    if (tool_name_matches(&summary.tool_name, "ls")
        || tool_name_matches(&summary.tool_name, "glob"))
        && let Some(lines) = render_path_list_preview(summary, accent)
    {
        return lines;
    }
    if tool_name_matches(&summary.tool_name, "grep")
        && let Some(lines) = render_grep_preview(summary, accent)
    {
        return lines;
    }
    if tool_name_matches(&summary.tool_name, "bash") {
        return render_bash_preview(summary, accent);
    }
    if (tool_name_matches(&summary.tool_name, "write_file")
        || tool_name_matches(&summary.tool_name, "edit_file"))
        && let Some(lines) = render_file_change_preview(summary, accent)
    {
        return lines;
    }
    if tool_name_matches(&summary.tool_name, "read_file") {
        return render_read_file_preview(summary, accent, max_content_width);
    }
    render_generic_tool_preview(summary, accent, max_content_width)
}

fn render_read_file_preview(
    summary: &ToolCardRender,
    accent: Color,
    max_content_width: usize,
) -> Vec<Line<'static>> {
    let mut lines = vec![timeline_section_line(
        accent,
        if summary.preview_kind == ToolPreviewKind::Markdown {
            "doc"
        } else {
            "file"
        },
        accent_blue(),
        vec![Span::styled(
            if summary.preview_kind == ToolPreviewKind::Markdown {
                "document excerpt"
            } else {
                "file excerpt"
            },
            Style::default().fg(dim()),
        )],
    )];
    match summary.preview_kind {
        ToolPreviewKind::Markdown => {
            lines.extend(render_markdown_timeline_lines(
                accent,
                Style::default().fg(ink()),
                &summary.preview_lines.join("\n"),
                max_content_width,
            ));
        }
        ToolPreviewKind::Json | ToolPreviewKind::Text => {
            lines.extend(render_code_preview_lines(
                accent,
                &summary.preview_lines,
                Color::Rgb(28, 33, 41),
            ));
        }
    }
    lines.extend(render_tool_hidden_tail(accent, summary.hidden_lines));
    lines
}

fn render_path_list_preview(summary: &ToolCardRender, accent: Color) -> Option<Vec<Line<'static>>> {
    let entries = summary
        .preview_value
        .as_ref()
        .and_then(json_string_list)
        .or_else(|| Some(infer_string_list_preview(&summary.preview_lines)))
        .filter(|entries| !entries.is_empty())?;

    let mut lines = vec![timeline_section_line(
        accent,
        if tool_name_matches(&summary.tool_name, "glob") {
            "matches"
        } else {
            "files"
        },
        accent_blue(),
        vec![Span::styled(
            format!("{} paths", entries.len() + summary.hidden_lines),
            Style::default().fg(dim()),
        )],
    )];
    for path in entries {
        lines.push(timeline_content_line(
            accent,
            vec![
                Span::styled(
                    "• ",
                    Style::default()
                        .fg(accent_gold())
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(path, Style::default().fg(ink())),
            ],
        ));
    }
    lines.extend(render_tool_hidden_tail(accent, summary.hidden_lines));
    Some(lines)
}

fn render_grep_preview(summary: &ToolCardRender, accent: Color) -> Option<Vec<Line<'static>>> {
    let matches = summary.preview_value.as_ref().and_then(json_grep_matches)?;
    if matches.is_empty() {
        return None;
    }

    let mut grouped = Vec::<(String, Vec<(u64, String)>)>::new();
    for (path, line, text) in matches {
        if let Some((_, rows)) = grouped.iter_mut().find(|(existing, _)| existing == &path) {
            rows.push((line, text));
        } else {
            grouped.push((path, vec![(line, text)]));
        }
    }

    let mut lines = vec![timeline_section_line(
        accent,
        "matches",
        accent_blue(),
        vec![Span::styled(
            format!("{} files", grouped.len()),
            Style::default().fg(dim()),
        )],
    )];
    for (path, rows) in grouped {
        lines.push(timeline_content_line(
            accent,
            vec![
                section_badge("file", accent_teal()),
                Span::raw(" "),
                Span::styled(
                    path,
                    Style::default().fg(ink()).add_modifier(Modifier::BOLD),
                ),
                Span::raw(" "),
                Span::styled(format!("{} hits", rows.len()), Style::default().fg(dim())),
            ],
        ));
        for (line_number, text) in rows {
            lines.push(timeline_content_line(
                accent,
                vec![
                    Span::styled(
                        format!("L{line_number:<4}"),
                        Style::default()
                            .fg(accent_gold())
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::raw(" "),
                    Span::styled(truncate_inline_text(&text, 140), Style::default().fg(ink())),
                ],
            ));
        }
    }
    lines.extend(render_tool_hidden_tail(accent, summary.hidden_lines));
    Some(lines)
}

fn render_bash_preview(summary: &ToolCardRender, accent: Color) -> Vec<Line<'static>> {
    let subtitle = match summary.metadata.exit_code {
        Some(code) if code != 0 => format!("exit {code} · terminal tail"),
        Some(code) => format!("exit {code} · terminal tail"),
        None => "terminal tail".to_owned(),
    };
    let mut lines = vec![timeline_section_line(
        accent,
        "tail",
        accent_gold(),
        vec![Span::styled(subtitle, Style::default().fg(dim()))],
    )];
    if summary.preview_lines.is_empty() {
        lines.push(timeline_content_line(
            accent,
            vec![Span::styled(
                "(no output)".to_owned(),
                Style::default().fg(dim()),
            )],
        ));
    } else {
        lines.extend(render_code_preview_lines(
            accent,
            &summary.preview_lines,
            Color::Rgb(33, 24, 28),
        ));
    }
    lines.extend(render_tool_hidden_tail(accent, summary.hidden_lines));
    lines
}

fn render_file_change_preview(
    summary: &ToolCardRender,
    accent: Color,
) -> Option<Vec<Line<'static>>> {
    if summary.metadata.changed_files.is_empty() {
        return None;
    }
    let mut lines = vec![timeline_section_line(
        accent,
        "files",
        accent_blue(),
        vec![Span::styled(
            format!("{} changed", summary.metadata.changed_files.len()),
            Style::default().fg(dim()),
        )],
    )];
    for path in &summary.metadata.changed_files {
        lines.push(timeline_content_line(
            accent,
            vec![
                Span::styled(
                    "• ",
                    Style::default()
                        .fg(accent_lime())
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(path.clone(), Style::default().fg(ink())),
            ],
        ));
    }
    if !summary.preview_lines.is_empty() {
        lines.push(timeline_section_line(
            accent,
            "result",
            accent_gold(),
            vec![Span::styled("write summary", Style::default().fg(dim()))],
        ));
        lines.extend(render_code_preview_lines(
            accent,
            &summary.preview_lines,
            Color::Rgb(28, 33, 41),
        ));
    }
    Some(lines)
}

fn render_generic_tool_preview(
    summary: &ToolCardRender,
    accent: Color,
    max_content_width: usize,
) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    if let Some(value) = &summary.preview_value {
        lines.push(timeline_section_line(
            accent,
            "tree",
            accent_blue(),
            vec![Span::styled(
                "structured payload",
                Style::default().fg(dim()),
            )],
        ));
        for line in render_json_tree_preview(value) {
            lines.push(timeline_content_line(
                accent,
                render_code_line_spans_with_bg(
                    &line,
                    accent_blue(),
                    Style::default().fg(ink()),
                    Color::Rgb(28, 33, 41),
                ),
            ));
        }
    } else if summary.preview_kind == ToolPreviewKind::Markdown {
        lines.push(timeline_section_line(
            accent,
            "md",
            accent_blue(),
            vec![Span::styled(
                "formatted preview",
                Style::default().fg(dim()),
            )],
        ));
        lines.extend(render_markdown_timeline_lines(
            accent,
            Style::default().fg(ink()),
            &summary.preview_lines.join("\n"),
            max_content_width,
        ));
    } else {
        lines.push(timeline_section_line(
            accent,
            summary.preview_kind.label(),
            accent_blue(),
            vec![Span::styled(
                summary.preview_kind.description(),
                Style::default().fg(dim()),
            )],
        ));
        lines.extend(render_code_preview_lines(
            accent,
            &summary.preview_lines,
            Color::Rgb(38, 28, 34),
        ));
    }
    lines.extend(render_tool_hidden_tail(accent, summary.hidden_lines));
    lines
}

fn render_code_preview_lines(accent: Color, lines: &[String], bg: Color) -> Vec<Line<'static>> {
    lines
        .iter()
        .map(|line| {
            timeline_content_line(
                accent,
                render_code_line_spans_with_bg(line, accent_blue(), Style::default().fg(ink()), bg),
            )
        })
        .collect()
}

fn render_tool_hidden_tail(accent: Color, hidden_lines: usize) -> Vec<Line<'static>> {
    if hidden_lines == 0 {
        return Vec::new();
    }
    vec![timeline_content_line(
        accent,
        vec![Span::styled(
            format!("… {} more lines hidden", hidden_lines),
            Style::default().fg(dim()).add_modifier(Modifier::BOLD),
        )],
    )]
}

fn render_json_tree_preview(value: &Value) -> Vec<String> {
    let mut lines = Vec::new();
    push_json_tree_lines(value, "", None, &mut lines);
    if lines.is_empty() {
        lines.push("(empty)".to_owned());
    }
    lines
}

fn push_json_tree_lines(value: &Value, prefix: &str, key: Option<&str>, lines: &mut Vec<String>) {
    match value {
        Value::Object(object) => {
            if let Some(key) = key {
                lines.push(format!("{prefix}{key}: {{}}"));
            } else if prefix.is_empty() {
                lines.push("{object}".to_owned());
            }
            let len = object.len();
            for (index, (child_key, child_value)) in object.iter().enumerate() {
                let branch = if index + 1 == len {
                    "└─ "
                } else {
                    "├─ "
                };
                let next_prefix = if index + 1 == len {
                    format!("{prefix}   ")
                } else {
                    format!("{prefix}│  ")
                };
                if json_tree_is_leaf(child_value) {
                    lines.push(format!(
                        "{prefix}{branch}{child_key}: {}",
                        json_tree_leaf_text(child_value)
                    ));
                } else {
                    lines.push(format!(
                        "{prefix}{branch}{child_key}: {}",
                        json_tree_container_label(child_value)
                    ));
                    push_json_tree_lines(child_value, &next_prefix, None, lines);
                }
            }
        }
        Value::Array(items) => {
            if let Some(key) = key {
                lines.push(format!("{prefix}{key}: [{}]", items.len()));
            } else if prefix.is_empty() {
                lines.push(format!("[array] {}", items.len()));
            }
            for (index, item) in items.iter().enumerate() {
                let branch = if index + 1 == items.len() {
                    "└─ "
                } else {
                    "├─ "
                };
                let next_prefix = if index + 1 == items.len() {
                    format!("{prefix}   ")
                } else {
                    format!("{prefix}│  ")
                };
                if json_tree_is_leaf(item) {
                    lines.push(format!(
                        "{prefix}{branch}[{index}] {}",
                        json_tree_leaf_text(item)
                    ));
                } else {
                    lines.push(format!(
                        "{prefix}{branch}[{index}] {}",
                        json_tree_container_label(item)
                    ));
                    push_json_tree_lines(item, &next_prefix, None, lines);
                }
            }
        }
        _ => {
            let leaf = json_tree_leaf_text(value);
            if let Some(key) = key {
                lines.push(format!("{prefix}{key}: {leaf}"));
            } else {
                lines.push(format!("{prefix}{leaf}"));
            }
        }
    }
}

fn json_tree_is_leaf(value: &Value) -> bool {
    !matches!(value, Value::Object(_) | Value::Array(_))
}

fn json_tree_leaf_text(value: &Value) -> String {
    match value {
        Value::Null => "null".to_owned(),
        Value::Bool(boolean) => boolean.to_string(),
        Value::Number(number) => number.to_string(),
        Value::String(text) => format!("\"{}\"", truncate_inline_text(text, 100)),
        Value::Array(items) => format!("[{}]", items.len()),
        Value::Object(object) => format!("{{{}}}", object.len()),
    }
}

fn json_tree_container_label(value: &Value) -> String {
    match value {
        Value::Array(items) => format!("[{} items]", items.len()),
        Value::Object(object) => format!("{{{} keys}}", object.len()),
        _ => json_tree_leaf_text(value),
    }
}

fn json_string_list(value: &Value) -> Option<Vec<String>> {
    let entries = value
        .as_array()?
        .iter()
        .filter_map(Value::as_str)
        .map(str::to_owned)
        .collect::<Vec<_>>();
    Some(entries)
}

fn infer_string_list_preview(lines: &[String]) -> Vec<String> {
    lines
        .iter()
        .map(|line| {
            line.trim()
                .trim_end_matches(',')
                .trim_matches('"')
                .to_owned()
        })
        .filter(|line| !line.is_empty() && line != "[" && line != "]")
        .collect()
}

fn json_grep_matches(value: &Value) -> Option<Vec<(String, u64, String)>> {
    let array = value.as_array()?;
    let mut matches = Vec::new();
    for entry in array {
        let object = entry.as_object()?;
        let path = object.get("path")?.as_str()?.to_owned();
        let line = object.get("line")?.as_u64()?;
        let text = object.get("text")?.as_str()?.to_owned();
        matches.push((path, line, text));
    }
    Some(matches)
}

fn tool_name_matches(tool_name: &str, expected: &str) -> bool {
    tool_name == expected || tool_name.ends_with(&format!("_{expected}"))
}

fn truncate_inline_text(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_owned();
    }
    let truncated = text.chars().take(max_chars).collect::<String>();
    format!("{truncated}...")
}

fn truncate_display_width(text: &str, max_width: usize) -> String {
    let max_width = max_width.max(1);
    if UnicodeWidthStr::width(text) <= max_width {
        return text.to_owned();
    }
    let ellipsis = "...";
    let ellipsis_width = UnicodeWidthStr::width(ellipsis);
    let budget = max_width.saturating_sub(ellipsis_width).max(1);
    let mut out = String::new();
    let mut used_width = 0usize;
    for grapheme in text.graphemes(true) {
        let grapheme_width = UnicodeWidthStr::width(grapheme).max(1);
        if !out.is_empty() && used_width + grapheme_width > budget {
            break;
        }
        out.push_str(grapheme);
        used_width += grapheme_width;
    }
    format!("{out}{ellipsis}")
}

fn notice_tone_label(accent: Color) -> &'static str {
    if accent == accent_rose() {
        "error"
    } else if accent == accent_lime() {
        "ok"
    } else {
        "info"
    }
}

#[derive(Default)]
struct MarkdownRenderState {
    in_fenced_code: bool,
}

fn render_markdown_timeline_lines(
    accent: Color,
    body_style: Style,
    text: &str,
    max_content_width: usize,
) -> Vec<Line<'static>> {
    let source_lines = text.lines().collect::<Vec<_>>();
    let mut rendered = Vec::new();
    let max_content_width = max_content_width.max(20);
    let mut index = 0usize;
    while index < source_lines.len() {
        let line = source_lines[index];
        if line.trim().is_empty() {
            if rendered
                .last()
                .map(|line: &Line<'static>| !line.spans.is_empty())
                .unwrap_or(false)
            {
                rendered.push(Line::raw(String::new()));
            }
            index += 1;
            continue;
        }
        if let Some((level, content)) = markdown_heading(line) {
            rendered.extend(render_markdown_heading_block(
                level,
                content,
                body_style,
                max_content_width,
            ));
            index += 1;
            continue;
        }
        if let Some(language) = fenced_code_language(line) {
            let label = if language.is_empty() {
                "plain"
            } else {
                language
            };
            index += 1;
            let mut block_lines = Vec::new();
            while index < source_lines.len() {
                if fenced_code_language(source_lines[index]).is_some() {
                    index += 1;
                    break;
                }
                block_lines.push(source_lines[index]);
                index += 1;
            }
            rendered.push(timeline_section_line(
                accent,
                "code",
                accent_blue(),
                vec![Span::styled(label.to_owned(), Style::default().fg(dim()))],
            ));
            if block_lines.is_empty() {
                rendered.push(timeline_content_line(
                    accent,
                    render_code_line_spans_with_bg(
                        "",
                        accent_blue(),
                        Style::default().fg(ink()),
                        Color::Rgb(28, 33, 41),
                    ),
                ));
            } else {
                for block_line in block_lines {
                    rendered.push(timeline_content_line(
                        accent,
                        render_code_line_spans_with_bg(
                            block_line,
                            accent_blue(),
                            Style::default().fg(ink()),
                            Color::Rgb(28, 33, 41),
                        ),
                    ));
                }
            }
            continue;
        }
        if markdown_table_line(line) {
            let start = index;
            while index < source_lines.len() && markdown_table_line(source_lines[index]) {
                index += 1;
            }
            rendered.extend(render_markdown_table_block(
                accent,
                body_style,
                &source_lines[start..index],
                max_content_width,
            ));
            continue;
        }
        if markdown_quote(line).is_some() {
            let start = index;
            while index < source_lines.len() && markdown_quote(source_lines[index]).is_some() {
                index += 1;
            }
            rendered.push(timeline_section_line(
                accent,
                "quote",
                accent_teal(),
                vec![Span::styled("quoted context", Style::default().fg(dim()))],
            ));
            for quote_line in &source_lines[start..index] {
                let content = markdown_quote(quote_line).unwrap_or_else(|| quote_line.trim());
                let mut spans = vec![Span::styled(
                    "▌ ",
                    Style::default()
                        .fg(accent_teal())
                        .add_modifier(Modifier::BOLD),
                )];
                spans.extend(render_inline_markdown_spans(
                    content,
                    body_style.fg(muted()),
                ));
                rendered.push(timeline_content_line(accent, spans));
            }
            continue;
        }

        let mut markdown_state = MarkdownRenderState::default();
        rendered.push(timeline_content_line(
            accent,
            render_markdown_spans(line, body_style, &mut markdown_state),
        ));
        index += 1;
    }
    rendered
}

fn render_markdown_heading_block(
    level: usize,
    content: &str,
    base_style: Style,
    max_content_width: usize,
) -> Vec<Line<'static>> {
    let accent = match level {
        1 => accent_gold(),
        2 => accent_blue(),
        3 => accent_lime(),
        _ => accent_teal(),
    };
    let title_spans =
        render_inline_markdown_spans(content, base_style.fg(accent).add_modifier(Modifier::BOLD));
    let mut lines = vec![Line::from(title_spans)];
    if level <= 2 {
        let underline_width = UnicodeWidthStr::width(content).clamp(8, max_content_width.max(8));
        lines.push(Line::from(vec![Span::styled(
            "─".repeat(underline_width),
            Style::default().fg(dim()),
        )]));
    }
    lines
}

fn render_markdown_table_block(
    accent: Color,
    body_style: Style,
    rows: &[&str],
    max_content_width: usize,
) -> Vec<Line<'static>> {
    if rows.is_empty() {
        return Vec::new();
    }

    let parsed_rows = rows
        .iter()
        .map(|line| {
            markdown_table_cells(line)
                .into_iter()
                .map(|cell| markdown_plain_text(&cell))
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
    let header = parsed_rows.first().cloned().unwrap_or_default();
    let has_divider = parsed_rows
        .get(1)
        .is_some_and(|row| row.iter().all(|cell| markdown_table_divider_cell(cell)));
    let body_rows = if has_divider {
        parsed_rows.iter().skip(2).cloned().collect::<Vec<_>>()
    } else {
        parsed_rows.iter().skip(1).cloned().collect::<Vec<_>>()
    };
    let column_count = parsed_rows.iter().map(Vec::len).max().unwrap_or(0);
    if column_count == 0 {
        return Vec::new();
    }

    let natural_widths = (0..column_count)
        .map(|column| {
            parsed_rows
                .iter()
                .filter_map(|row| row.get(column))
                .filter(|cell| !markdown_table_divider_cell(cell))
                .map(|cell| UnicodeWidthStr::width(cell.as_str()))
                .max()
                .unwrap_or(3)
                .max(3)
        })
        .collect::<Vec<_>>();
    let widths = clamp_table_widths(&natural_widths, max_content_width.max(24));

    let summary = format!(
        "{} cols · {} rows",
        column_count,
        body_rows.len().saturating_add(1)
    );
    let mut lines = vec![timeline_section_line(
        accent,
        "table",
        accent_teal(),
        vec![Span::styled(summary, Style::default().fg(dim()))],
    )];

    lines.push(timeline_content_line(
        accent,
        vec![Span::styled(
            markdown_table_border(&widths, '┌', '┬', '┐', '─'),
            Style::default().fg(dim()),
        )],
    ));
    for header_line in markdown_table_row_lines(&header, &widths) {
        lines.push(timeline_content_line(
            accent,
            vec![Span::styled(
                header_line,
                body_style.fg(accent_blue()).add_modifier(Modifier::BOLD),
            )],
        ));
    }
    lines.push(timeline_content_line(
        accent,
        vec![Span::styled(
            markdown_table_border(&widths, '├', '┼', '┤', if has_divider { '═' } else { '─' }),
            Style::default().fg(dim()),
        )],
    ));
    for row in body_rows {
        for row_line in markdown_table_row_lines(&row, &widths) {
            lines.push(timeline_content_line(
                accent,
                vec![Span::styled(row_line, body_style)],
            ));
        }
    }
    lines.push(timeline_content_line(
        accent,
        vec![Span::styled(
            markdown_table_border(&widths, '└', '┴', '┘', '─'),
            Style::default().fg(dim()),
        )],
    ));
    lines
}

fn markdown_table_cells(line: &str) -> Vec<String> {
    line.trim()
        .trim_matches('|')
        .split('|')
        .map(|cell| cell.trim().to_owned())
        .collect()
}

fn markdown_table_divider_cell(cell: &str) -> bool {
    !cell.is_empty()
        && cell
            .chars()
            .all(|character| matches!(character, '-' | ':' | ' '))
}

fn markdown_table_border(
    widths: &[usize],
    left: char,
    join: char,
    right: char,
    fill: char,
) -> String {
    let mut out = String::new();
    out.push(left);
    for (index, width) in widths.iter().enumerate() {
        out.push_str(&fill.to_string().repeat(width + 2));
        if index + 1 < widths.len() {
            out.push(join);
        }
    }
    out.push(right);
    out
}

fn markdown_table_row(cells: &[String], widths: &[usize]) -> String {
    let mut out = String::new();
    out.push('│');
    for (index, width) in widths.iter().enumerate() {
        let cell = cells.get(index).map(String::as_str).unwrap_or("");
        out.push(' ');
        out.push_str(cell);
        let cell_width = UnicodeWidthStr::width(cell);
        if *width > cell_width {
            out.push_str(&" ".repeat(*width - cell_width));
        }
        out.push(' ');
        out.push('│');
    }
    out
}

fn markdown_table_row_lines(cells: &[String], widths: &[usize]) -> Vec<String> {
    let wrapped_cells = widths
        .iter()
        .enumerate()
        .map(|(index, width)| {
            let cell = cells.get(index).map(String::as_str).unwrap_or("");
            wrap_display_width(cell, *width)
        })
        .collect::<Vec<_>>();
    let row_height = wrapped_cells.iter().map(Vec::len).max().unwrap_or(1).max(1);
    let mut lines = Vec::with_capacity(row_height);
    for line_index in 0..row_height {
        let row = widths
            .iter()
            .enumerate()
            .map(|(column, width)| {
                let text = wrapped_cells[column]
                    .get(line_index)
                    .cloned()
                    .unwrap_or_default();
                pad_display_width(&text, *width)
            })
            .collect::<Vec<_>>();
        lines.push(markdown_table_row(&row, widths));
    }
    lines
}

fn clamp_table_widths(widths: &[usize], max_content_width: usize) -> Vec<usize> {
    if widths.is_empty() {
        return Vec::new();
    }
    let mut clamped = widths.to_vec();
    let min_widths = widths
        .iter()
        .map(|width| (*width).min(12).clamp(4, 12))
        .collect::<Vec<_>>();
    while markdown_table_total_width(&clamped) > max_content_width {
        let Some((index, _)) = clamped
            .iter()
            .enumerate()
            .filter(|(index, width)| **width > min_widths[*index])
            .max_by_key(|(_, width)| **width)
        else {
            break;
        };
        clamped[index] = clamped[index].saturating_sub(1);
    }
    clamped
}

fn markdown_table_total_width(widths: &[usize]) -> usize {
    if widths.is_empty() {
        return 0;
    }
    widths.iter().sum::<usize>() + widths.len() * 3 + 1
}

fn wrap_display_width(text: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    if text.is_empty() {
        return vec![String::new()];
    }
    let mut rows = Vec::new();
    let mut current = String::new();
    let mut current_width = 0usize;
    for grapheme in text.graphemes(true) {
        let grapheme_width = UnicodeWidthStr::width(grapheme).max(1);
        if !current.is_empty() && current_width + grapheme_width > width {
            rows.push(std::mem::take(&mut current));
            current_width = 0;
        }
        current.push_str(grapheme);
        current_width += grapheme_width;
    }
    if current.is_empty() {
        rows.push(String::new());
    } else {
        rows.push(current);
    }
    rows
}

fn wrap_composer_input(text: &str, width: usize) -> Vec<String> {
    let mut rows = Vec::new();
    for line in text.split('\n') {
        rows.extend(wrap_display_width(line, width));
    }
    if rows.is_empty() {
        rows.push(String::new());
    }
    rows
}

fn selector_window_range(total: usize, selected: usize, visible: usize) -> (usize, usize) {
    if total <= visible || visible == 0 {
        return (0, total);
    }

    let half = visible / 2;
    let max_start = total.saturating_sub(visible);
    let start = selected.saturating_sub(half).min(max_start);
    (start, start + visible)
}

fn pad_display_width(text: &str, width: usize) -> String {
    let mut out = text.to_owned();
    let display_width = UnicodeWidthStr::width(text);
    if width > display_width {
        out.push_str(&" ".repeat(width - display_width));
    }
    out
}

fn render_timeline_content_spans(
    role: TimelineRole,
    line: &str,
    base_style: Style,
    state: &mut MarkdownRenderState,
) -> Vec<Span<'static>> {
    match role {
        TimelineRole::Assistant => render_markdown_spans(line, base_style, state),
        TimelineRole::Thinking => render_markdown_spans(line, base_style, state),
        TimelineRole::Tool => {
            render_code_line_spans(line, accent_rose(), Style::default().fg(ink()))
        }
        TimelineRole::System | TimelineRole::Phase | TimelineRole::Notice => {
            render_inline_markdown_spans(line, base_style.add_modifier(Modifier::BOLD))
        }
        TimelineRole::User => vec![Span::styled(line.to_owned(), base_style)],
    }
}

fn render_markdown_spans(
    line: &str,
    base_style: Style,
    state: &mut MarkdownRenderState,
) -> Vec<Span<'static>> {
    if state.in_fenced_code || line_looks_like_code(line) {
        return render_code_line_spans(line, accent_blue(), Style::default().fg(ink()));
    }
    if let Some((level, content)) = markdown_heading(line) {
        let accent = match level {
            1 => accent_gold(),
            2 => accent_blue(),
            3 => accent_lime(),
            _ => accent_teal(),
        };
        return render_inline_markdown_spans(
            content,
            base_style.fg(accent).add_modifier(Modifier::BOLD),
        );
    }
    if markdown_rule(line) {
        return vec![Span::styled(
            "────────────────────────────────",
            Style::default().fg(dim()),
        )];
    }
    if let Some((checked, content)) = markdown_task_item(line) {
        let marker = if checked { "[x]" } else { "[ ]" };
        let mut spans = vec![Span::styled(
            format!("{marker} "),
            Style::default()
                .fg(if checked {
                    accent_lime()
                } else {
                    accent_gold()
                })
                .add_modifier(Modifier::BOLD),
        )];
        spans.extend(render_inline_markdown_spans(content, base_style));
        return spans;
    }
    if let Some(content) = markdown_bullet_item(line) {
        let mut spans = vec![Span::styled(
            "• ",
            Style::default()
                .fg(accent_gold())
                .add_modifier(Modifier::BOLD),
        )];
        spans.extend(render_inline_markdown_spans(content, base_style));
        return spans;
    }
    if let Some((number, content)) = markdown_ordered_item(line) {
        let mut spans = vec![Span::styled(
            format!("{number}. "),
            Style::default()
                .fg(accent_gold())
                .add_modifier(Modifier::BOLD),
        )];
        spans.extend(render_inline_markdown_spans(content, base_style));
        return spans;
    }
    if let Some(content) = markdown_quote(line) {
        let mut spans = vec![Span::styled("│ ", Style::default().fg(accent_teal()))];
        spans.extend(render_inline_markdown_spans(
            content,
            base_style.fg(muted()),
        ));
        return spans;
    }
    if markdown_table_line(line) {
        return render_table_spans(line, base_style);
    }
    render_inline_markdown_spans(line, base_style)
}

fn render_inline_markdown_spans(text: &str, base_style: Style) -> Vec<Span<'static>> {
    let mut spans = Vec::new();
    let mut rest = text;
    while !rest.is_empty() {
        let next_marker = next_inline_marker(rest);

        let Some(marker_index) = next_marker else {
            spans.push(Span::styled(rest.to_owned(), base_style));
            break;
        };

        if marker_index > 0 {
            spans.push(Span::styled(rest[..marker_index].to_owned(), base_style));
            rest = &rest[marker_index..];
            continue;
        }

        if let Some((label, url, consumed)) = markdown_link(rest) {
            spans.push(Span::styled(
                label.to_owned(),
                base_style
                    .fg(accent_blue())
                    .add_modifier(Modifier::UNDERLINED | Modifier::BOLD),
            ));
            spans.push(Span::styled(
                format!(" <{url}>"),
                Style::default().fg(dim()),
            ));
            rest = &rest[consumed..];
            continue;
        }

        if let Some(after) = rest.strip_prefix("**") {
            if let Some(end) = after.find("**") {
                spans.push(Span::styled(
                    after[..end].to_owned(),
                    base_style.add_modifier(Modifier::BOLD),
                ));
                rest = &after[end + 2..];
            } else {
                spans.push(Span::styled("**".to_owned(), base_style));
                rest = after;
            }
            continue;
        }

        if let Some((content, consumed)) = markdown_emphasis(rest) {
            spans.push(Span::styled(
                content.to_owned(),
                base_style.add_modifier(Modifier::ITALIC),
            ));
            rest = &rest[consumed..];
            continue;
        }

        if let Some(after) = rest.strip_prefix('`') {
            if let Some(end) = after.find('`') {
                spans.push(Span::styled(
                    after[..end].to_owned(),
                    Style::default()
                        .fg(accent_blue())
                        .bg(Color::Rgb(35, 40, 48))
                        .add_modifier(Modifier::BOLD),
                ));
                rest = &after[end + 1..];
            } else {
                spans.push(Span::styled("`".to_owned(), base_style));
                rest = after;
            }
            continue;
        }

        spans.push(Span::styled(rest.to_owned(), base_style));
        break;
    }
    spans
}

fn markdown_plain_text(text: &str) -> String {
    let mut plain = String::new();
    let mut rest = text;
    while !rest.is_empty() {
        let next_marker = next_inline_marker(rest);
        let Some(marker_index) = next_marker else {
            plain.push_str(rest);
            break;
        };
        if marker_index > 0 {
            plain.push_str(&rest[..marker_index]);
            rest = &rest[marker_index..];
            continue;
        }
        if let Some((label, _, consumed)) = markdown_link(rest) {
            plain.push_str(label);
            rest = &rest[consumed..];
            continue;
        }
        if let Some(after) = rest.strip_prefix("**")
            && let Some(end) = after.find("**")
        {
            plain.push_str(&after[..end]);
            rest = &after[end + 2..];
            continue;
        }
        if let Some(after) = rest.strip_prefix('`')
            && let Some(end) = after.find('`')
        {
            plain.push_str(&after[..end]);
            rest = &after[end + 1..];
            continue;
        }
        if let Some((content, consumed)) = markdown_emphasis(rest) {
            plain.push_str(content);
            rest = &rest[consumed..];
            continue;
        }
        if let Some(character) = rest.chars().next() {
            plain.push(character);
            rest = &rest[character.len_utf8()..];
        } else {
            break;
        }
    }
    plain
}

fn render_code_line_spans(line: &str, accent: Color, base_style: Style) -> Vec<Span<'static>> {
    render_code_line_spans_with_bg(line, accent, base_style, Color::Rgb(28, 33, 41))
}

fn render_code_line_spans_with_bg(
    line: &str,
    accent: Color,
    base_style: Style,
    bg: Color,
) -> Vec<Span<'static>> {
    vec![
        Span::styled(
            "│ ",
            Style::default().fg(accent).add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            if line.is_empty() {
                " ".to_owned()
            } else {
                line.to_owned()
            },
            base_style.bg(bg),
        ),
    ]
}

struct ToolCardRender {
    tool_name: String,
    call_id: Option<String>,
    status: String,
    is_error: bool,
    summary: Option<String>,
    metadata: ToolCardMetadata,
    metadata_line: Option<String>,
    preview_kind: ToolPreviewKind,
    preview_lines: Vec<String>,
    hidden_lines: usize,
    preview_value: Option<Value>,
}

#[derive(Default)]
struct ToolCardMetadata {
    exit_code: Option<i64>,
    bytes: Option<u64>,
    truncated: bool,
    changed_files: Vec<String>,
}

#[derive(Clone, Copy, Default, PartialEq, Eq)]
enum ToolPreviewKind {
    Markdown,
    Json,
    #[default]
    Text,
}

impl ToolPreviewKind {
    fn label(self) -> &'static str {
        match self {
            Self::Markdown => "md",
            Self::Json => "json",
            Self::Text => "text",
        }
    }

    fn description(self) -> &'static str {
        match self {
            Self::Markdown => "formatted preview",
            Self::Json => "structured preview",
            Self::Text => "captured output",
        }
    }

    fn from_value(value: &str) -> Self {
        match value {
            "markdown" => Self::Markdown,
            "json" => Self::Json,
            _ => Self::Text,
        }
    }
}

fn parse_tool_summary(text: &str) -> ToolCardRender {
    let fallback = ToolCardRender {
        tool_name: "result".to_owned(),
        call_id: None,
        status: " OK ".to_owned(),
        is_error: false,
        summary: None,
        metadata: ToolCardMetadata::default(),
        metadata_line: None,
        preview_kind: ToolPreviewKind::Text,
        preview_lines: text.lines().take(8).map(str::to_owned).collect(),
        hidden_lines: text.lines().count().saturating_sub(8),
        preview_value: None,
    };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(text) else {
        return fallback;
    };
    let Some(object) = value.as_object() else {
        return fallback;
    };
    let Some(tool_name) = object
        .get("tool_name")
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned)
    else {
        return fallback;
    };
    let call_id = object
        .get("call_id")
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned);
    let status = object
        .get("status")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("ok")
        .to_uppercase();
    let is_error = status == "ERROR";
    let metadata = object
        .get("metadata")
        .map(parse_tool_metadata)
        .unwrap_or_default();
    let summary = object
        .get("summary")
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned)
        .or_else(|| call_id.as_ref().map(|call_id| format!("call {call_id}")));
    let metadata_line = object
        .get("metadata_line")
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned)
        .or_else(|| render_tool_metadata(&metadata).filter(|line| !line.is_empty()));
    let preview_kind = object
        .get("preview_kind")
        .and_then(serde_json::Value::as_str)
        .map(ToolPreviewKind::from_value)
        .or_else(|| object.get("content").map(legacy_tool_preview_kind))
        .unwrap_or_default();
    let preview_value = object.get("preview_value").cloned().or_else(|| {
        object
            .get("content")
            .cloned()
            .filter(|value| matches!(value, Value::Array(_) | Value::Object(_)))
    });
    let (preview_lines, hidden_lines) = object
        .get("preview_lines")
        .and_then(serde_json::Value::as_array)
        .map(|lines| {
            let preview = lines
                .iter()
                .filter_map(serde_json::Value::as_str)
                .map(str::to_owned)
                .collect::<Vec<_>>();
            let hidden = object
                .get("hidden_lines")
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(0) as usize;
            (preview, hidden)
        })
        .unwrap_or_else(|| legacy_tool_preview(object.get("content"), preview_kind));

    ToolCardRender {
        tool_name,
        call_id,
        status: format!(" {status} "),
        is_error,
        summary,
        metadata,
        metadata_line,
        preview_kind,
        preview_lines,
        hidden_lines,
        preview_value,
    }
}

fn legacy_tool_preview_kind(value: &serde_json::Value) -> ToolPreviewKind {
    match value {
        serde_json::Value::Array(_) | serde_json::Value::Object(_) => ToolPreviewKind::Json,
        serde_json::Value::String(content)
            if content.trim_start().starts_with('#')
                || content.contains("```")
                || content.contains("\n- ")
                || content.contains("\n|") =>
        {
            ToolPreviewKind::Markdown
        }
        _ => ToolPreviewKind::Text,
    }
}

fn legacy_tool_preview(
    value: Option<&serde_json::Value>,
    preview_kind: ToolPreviewKind,
) -> (Vec<String>, usize) {
    let Some(value) = value else {
        return (Vec::new(), 0);
    };
    let source = match value {
        serde_json::Value::Array(_) | serde_json::Value::Object(_) => {
            serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string())
        }
        serde_json::Value::String(text) => text.clone(),
        _ => value.to_string(),
    };
    let limit = match preview_kind {
        ToolPreviewKind::Markdown => 18,
        ToolPreviewKind::Json => 12,
        ToolPreviewKind::Text => 12,
    };
    let lines = source.lines().map(str::to_owned).collect::<Vec<_>>();
    let hidden_lines = lines.len().saturating_sub(limit);
    let preview_lines = lines.into_iter().take(limit).collect::<Vec<_>>();
    (preview_lines, hidden_lines)
}

fn parse_tool_metadata(value: &Value) -> ToolCardMetadata {
    let Some(object) = value.as_object() else {
        return ToolCardMetadata::default();
    };
    ToolCardMetadata {
        exit_code: object.get("exit_code").and_then(Value::as_i64),
        bytes: object.get("bytes").and_then(Value::as_u64),
        truncated: object
            .get("truncated")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        changed_files: object
            .get("changed_files")
            .and_then(Value::as_array)
            .map(|files| {
                files
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_owned)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default(),
    }
}

fn render_tool_metadata(metadata: &ToolCardMetadata) -> Option<String> {
    let mut parts = Vec::new();
    if let Some(exit_code) = metadata.exit_code {
        parts.push(format!("exit={exit_code}"));
    }
    if let Some(bytes) = metadata.bytes {
        parts.push(format!("bytes={bytes}"));
    }
    if metadata.truncated {
        parts.push("truncated=yes".to_owned());
    }
    if !metadata.changed_files.is_empty() {
        let preview = metadata
            .changed_files
            .iter()
            .take(2)
            .cloned()
            .collect::<Vec<_>>();
        if preview.is_empty() {
            parts.push(format!("files={}", metadata.changed_files.len()));
        } else {
            let mut summary = format!(
                "files={} {}",
                metadata.changed_files.len(),
                preview.join(", ")
            );
            if metadata.changed_files.len() > preview.len() {
                summary.push_str(" ...");
            }
            parts.push(summary);
        }
    }
    (!parts.is_empty()).then_some(parts.join("  "))
}

fn tool_status_style(is_error: bool) -> Style {
    if is_error {
        Style::default()
            .fg(accent_rose())
            .bg(badge_bg())
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default()
            .fg(accent_lime())
            .bg(badge_bg())
            .add_modifier(Modifier::BOLD)
    }
}

fn notice_accent(text: &str) -> Color {
    let lower = text.to_ascii_lowercase();
    if lower.contains("failed")
        || lower.contains("error")
        || lower.contains("deny")
        || lower.contains("missing")
    {
        accent_rose()
    } else if lower.contains("approved")
        || lower.contains("restored")
        || lower.contains("ready")
        || lower.contains("saved")
    {
        accent_lime()
    } else {
        accent_gold()
    }
}

fn render_notice_body_spans(line: &str, accent: Color) -> Vec<Span<'static>> {
    if let Some((label, value)) = line.split_once(':') {
        let mut spans = vec![];
        spans.push(Span::styled(
            format!("{label}:"),
            Style::default().fg(accent).add_modifier(Modifier::BOLD),
        ));
        spans.push(Span::raw(" "));
        spans.extend(render_inline_markdown_spans(
            value.trim_start(),
            Style::default().fg(ink()),
        ));
        return spans;
    }
    render_inline_markdown_spans(line, Style::default().fg(ink()))
}

fn fenced_code_language(line: &str) -> Option<&str> {
    let trimmed = line.trim_start();
    trimmed.strip_prefix("```").map(str::trim)
}

fn markdown_heading(line: &str) -> Option<(usize, &str)> {
    let trimmed = line.trim_start();
    let level = trimmed
        .chars()
        .take_while(|character| *character == '#')
        .count();
    if !(1..=6).contains(&level) {
        return None;
    }
    let remainder = trimmed[level..].trim_start();
    if remainder.is_empty() {
        None
    } else {
        Some((level, remainder))
    }
}

fn markdown_rule(line: &str) -> bool {
    let trimmed = line.trim();
    trimmed.len() >= 3
        && trimmed
            .chars()
            .all(|character| matches!(character, '-' | '*' | '_' | ' '))
}

fn markdown_task_item(line: &str) -> Option<(bool, &str)> {
    let trimmed = line.trim_start();
    let content = trimmed
        .strip_prefix("- [ ] ")
        .or_else(|| trimmed.strip_prefix("* [ ] "))
        .map(|content| (false, content));
    content.or_else(|| {
        trimmed
            .strip_prefix("- [x] ")
            .or_else(|| trimmed.strip_prefix("* [x] "))
            .map(|content| (true, content))
    })
}

fn markdown_bullet_item(line: &str) -> Option<&str> {
    let trimmed = line.trim_start();
    trimmed
        .strip_prefix("- ")
        .or_else(|| trimmed.strip_prefix("* "))
}

fn markdown_ordered_item(line: &str) -> Option<(&str, &str)> {
    let trimmed = line.trim_start();
    let digits = trimmed
        .chars()
        .take_while(|character| character.is_ascii_digit())
        .count();
    if digits == 0 {
        return None;
    }
    let number = &trimmed[..digits];
    let rest = trimmed[digits..].strip_prefix(". ")?;
    Some((number, rest))
}

fn markdown_quote(line: &str) -> Option<&str> {
    line.trim_start().strip_prefix("> ")
}

fn markdown_table_line(line: &str) -> bool {
    let trimmed = line.trim();
    trimmed.starts_with('|') && trimmed.ends_with('|') && trimmed.matches('|').count() >= 2
}

fn markdown_link(text: &str) -> Option<(&str, &str, usize)> {
    let after_label = text.strip_prefix('[')?;
    let label_end = after_label.find("](")?;
    let label = &after_label[..label_end];
    let after_url = &after_label[label_end + 2..];
    let url_end = after_url.find(')')?;
    let url = &after_url[..url_end];
    let consumed = 1 + label_end + 2 + url_end + 1;
    Some((label, url, consumed))
}

fn markdown_emphasis(text: &str) -> Option<(&str, usize)> {
    for marker in ['*', '_'] {
        let Some(after) = text.strip_prefix(marker) else {
            continue;
        };
        if after.starts_with(marker) {
            continue;
        }
        let end = after.find(marker)?;
        let content = &after[..end];
        if content.is_empty() {
            continue;
        }
        return Some((content, 1 + end + 1));
    }
    None
}

fn next_inline_marker(text: &str) -> Option<usize> {
    let markers = [
        text.find("**"),
        text.find('`'),
        text.find('['),
        text.find('*'),
        text.find('_'),
    ];
    markers.into_iter().flatten().min()
}

fn render_table_spans(line: &str, base_style: Style) -> Vec<Span<'static>> {
    let cells = line
        .trim()
        .trim_matches('|')
        .split('|')
        .map(str::trim)
        .collect::<Vec<_>>();
    if cells.is_empty() {
        return vec![Span::styled(line.to_owned(), base_style)];
    }
    if cells.iter().all(|cell| {
        !cell.is_empty()
            && cell
                .chars()
                .all(|character| matches!(character, '-' | ':' | ' '))
    }) {
        let width = cells
            .iter()
            .map(|cell| cell.len().max(3) + 2)
            .sum::<usize>()
            .saturating_add(cells.len().saturating_sub(1) * 3)
            .max(12);
        return vec![Span::styled("┄".repeat(width), Style::default().fg(dim()))];
    }
    let mut spans = vec![Span::styled("│ ", Style::default().fg(Color::DarkGray))];
    for (index, cell) in cells.iter().enumerate() {
        if index > 0 {
            spans.push(Span::styled(" │ ", Style::default().fg(Color::DarkGray)));
        }
        if cell.is_empty() {
            spans.push(Span::styled(" ", base_style));
        } else {
            spans.extend(render_inline_markdown_spans(cell, base_style));
        }
    }
    spans.push(Span::styled(" │", Style::default().fg(Color::DarkGray)));
    spans
}

fn line_looks_like_code(line: &str) -> bool {
    let trimmed = line.trim_start();
    line.starts_with("    ")
        || line.starts_with('\t')
        || line.contains('│')
        || line.contains('└')
        || line.contains('├')
        || line.contains('┌')
        || line.contains('─')
        || trimmed.starts_with('{')
        || trimmed.starts_with('}')
        || trimmed.starts_with('[')
        || trimmed.starts_with(']')
}

fn render_input(frame: &mut Frame, area: Rect, app: &AppState) {
    let phase = app.run_phase();
    let accent = phase_accent(&phase);
    frame.render_widget(
        Block::default().style(Style::default().bg(composer_bg())),
        area,
    );
    render_composer_gutter(frame, area, accent);

    let inner = inset_rect(area, 3, 1);
    if inner.width == 0 || inner.height == 0 {
        return;
    }

    let input_height = app
        .composer_input_rows()
        .min(inner.height.saturating_sub(1).max(1));
    let layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Min(input_height)])
        .split(inner);
    let header_area = layout[0];
    let input_area = layout[1];

    let spinner = if app.is_busy {
        live_spinner_frame()
    } else {
        "•"
    };
    let header = Line::from(vec![
        Span::styled(
            "Composer",
            Style::default().fg(accent).add_modifier(Modifier::BOLD),
        ),
        Span::raw("  "),
        Span::styled(app.model_name.clone(), Style::default().fg(ink())),
        Span::raw("  ·  "),
        Span::styled(
            app.reasoning_effort_label(),
            Style::default().fg(accent_gold()),
        ),
        Span::raw("  ·  "),
        Span::styled(
            format!("{spinner} {}", app.run_phase_label()),
            Style::default().fg(muted()),
        ),
    ]);
    frame.render_widget(
        Paragraph::new(Text::from(vec![header]))
            .style(Style::default().bg(composer_bg()))
            .wrap(Wrap { trim: false }),
        header_area,
    );

    let input_bg = composer_input_bg();
    frame.render_widget(
        Block::default().style(Style::default().bg(input_bg)),
        input_area,
    );
    let input_inner = inset_rect(input_area, 1, 0);
    if input_inner.width > 0 && input_inner.height > 0 {
        let input_width = input_inner.width as usize;
        let cursor_row = app.input_cursor_visual_position().1 as usize;
        let visible_rows = input_inner.height as usize;
        let row_offset = cursor_row.saturating_sub(visible_rows.saturating_sub(1));
        let wrapped_rows = wrap_composer_input(&app.input, input_width);
        let mut lines = wrapped_rows
            .into_iter()
            .skip(row_offset)
            .take(visible_rows)
            .map(|row| {
                Line::from(vec![Span::styled(
                    pad_display_width(&row, input_width),
                    Style::default().fg(ink()).bg(input_bg),
                )])
            })
            .collect::<Vec<_>>();
        while lines.len() < visible_rows {
            lines.push(Line::from(vec![Span::styled(
                " ".repeat(input_width),
                Style::default().bg(input_bg),
            )]));
        }
        frame.render_widget(
            Paragraph::new(Text::from(lines))
                .style(Style::default().bg(input_bg))
                .wrap(Wrap { trim: false }),
            input_inner,
        );
    }
}

fn composer_cursor_origin(area: Rect, app: &AppState) -> Option<(u16, u16)> {
    let inner = inset_rect(area, 3, 1);
    if inner.width == 0 || inner.height == 0 {
        return None;
    }
    let input_height = app
        .composer_input_rows()
        .min(inner.height.saturating_sub(1).max(1));
    let input_area_y = inner
        .y
        .saturating_add(inner.height.saturating_sub(input_height));
    let input_inner_x = inner.x.saturating_add(1);
    let cursor_row = app.input_cursor_visual_position().1;
    let row_offset = cursor_row.saturating_sub(input_height.saturating_sub(1));
    Some((input_inner_x, input_area_y.saturating_sub(row_offset)))
}

fn render_composer_gutter(frame: &mut Frame, area: Rect, accent: Color) {
    let gutter = Rect::new(area.x.saturating_add(1), area.y, 1, area.height);
    if gutter.width == 0 || gutter.height == 0 {
        return;
    }
    let lines = (0..gutter.height)
        .map(|_| {
            Line::from(vec![Span::styled(
                "▌",
                Style::default().fg(accent).bg(composer_bg()),
            )])
        })
        .collect::<Vec<_>>();
    frame.render_widget(
        Paragraph::new(Text::from(lines))
            .style(Style::default().bg(composer_bg()))
            .wrap(Wrap { trim: false }),
        gutter,
    );
}

fn render_slash_selector_overlay(
    frame: &mut Frame,
    live_area: Rect,
    composer_area: Rect,
    app: &AppState,
) {
    if !app.has_slash_selector() || live_area.width == 0 || live_area.height == 0 {
        return;
    }

    let selector_rows = app.slash_selector_rows();
    let visible_rows = app.slash_selector_visible_rows() as usize;
    if visible_rows == 0 {
        return;
    }

    let Some(overlay) = slash_selector_overlay_rect(live_area, composer_area, visible_rows) else {
        return;
    };
    frame.render_widget(Clear, overlay);
    let shadow = shadow_rect(overlay, frame.area());
    frame.render_widget(
        Block::default().style(Style::default().bg(selector_shadow_bg())),
        shadow,
    );
    frame.render_widget(
        Block::default().style(Style::default().bg(selector_bg())),
        overlay,
    );

    let accent = accent_blue();
    let gutter = Rect::new(overlay.x, overlay.y, 1, overlay.height);
    frame.render_widget(
        Paragraph::new(Text::from(
            (0..gutter.height)
                .map(|_| {
                    Line::from(vec![Span::styled(
                        "▌",
                        Style::default().fg(accent).bg(selector_bg()),
                    )])
                })
                .collect::<Vec<_>>(),
        ))
        .style(Style::default().bg(selector_bg()))
        .wrap(Wrap { trim: false }),
        gutter,
    );

    let content = Rect::new(
        overlay.x.saturating_add(2),
        overlay.y,
        overlay.width.saturating_sub(4),
        overlay.height,
    );
    if content.width == 0 || content.height == 0 {
        return;
    }

    let lines = if selector_rows.is_empty() {
        vec![Line::styled(
            app.slash_selector_empty_message()
                .unwrap_or("no slash match"),
            Style::default().fg(accent_rose()),
        )]
    } else {
        let selected_index = app.slash_selector_selected_index().unwrap_or(0);
        let (window_start, window_end) =
            selector_window_range(selector_rows.len(), selected_index, visible_rows);
        selector_rows
            .into_iter()
            .enumerate()
            .skip(window_start)
            .take(window_end.saturating_sub(window_start))
            .map(|(index, (command, description))| {
                let selected = index == selected_index;
                let marker = if selected { "› " } else { "  " };
                let style = if selected {
                    Style::default()
                        .fg(Color::Black)
                        .bg(Color::Rgb(247, 186, 144))
                } else {
                    Style::default().fg(accent_blue()).bg(selector_bg())
                };
                Line::from(vec![
                    Span::styled(marker, style.add_modifier(Modifier::BOLD)),
                    Span::styled(format!("{command:<12}"), style.add_modifier(Modifier::BOLD)),
                    Span::styled(
                        description,
                        if selected {
                            style
                        } else {
                            Style::default().fg(muted()).bg(selector_bg())
                        },
                    ),
                ])
            })
            .collect::<Vec<_>>()
    };

    frame.render_widget(
        Paragraph::new(Text::from(lines))
            .style(Style::default().bg(selector_bg()))
            .wrap(Wrap { trim: false }),
        content,
    );
}

fn slash_selector_overlay_rect(
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

fn render_setup(frame: &mut Frame, app: &AppState) {
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

fn render_config(frame: &mut Frame, app: &AppState) {
    let panel_bg = Color::Rgb(14, 18, 16);
    let outer = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(4),
            Constraint::Min(10),
            Constraint::Length(3),
        ])
        .split(frame.area());
    render_status(frame, outer[0], app);

    let body = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage(8),
            Constraint::Min(72),
            Constraint::Percentage(8),
        ])
        .split(outer[1]);
    let detail = app
        .config_detail_lines()
        .into_iter()
        .enumerate()
        .map(|(index, line)| render_config_line(index, &line))
        .collect::<Vec<_>>();

    let detail_widget = Paragraph::new(Text::from(detail))
        .block(
            Block::default()
                .title("Config")
                .title_style(
                    Style::default()
                        .fg(Color::Black)
                        .bg(Color::Green)
                        .add_modifier(Modifier::BOLD),
                )
                .border_type(BorderType::Rounded)
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Green))
                .style(Style::default().bg(panel_bg)),
        )
        .style(Style::default().bg(panel_bg))
        .wrap(Wrap { trim: false });

    frame.render_widget(detail_widget, body[1]);
    render_config_footer(frame, outer[2], app, panel_bg);
    render_modal(frame, app);
}

fn render_config_footer(frame: &mut Frame, area: Rect, app: &AppState, panel_bg: Color) {
    let dirty_style = if app.config_is_dirty() {
        Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::Gray)
    };
    let selected = app.config_selected_footer_action_label();
    let line = Line::from(vec![
        footer_action_span("save", selected == Some("save"), Color::Green, panel_bg),
        Span::raw(" "),
        footer_action_span(
            "save+close",
            selected == Some("save+close"),
            Color::Yellow,
            panel_bg,
        ),
        Span::raw(" "),
        footer_action_span("close", selected == Some("close"), Color::Red, panel_bg),
        Span::raw("  "),
        Span::styled(app.config_footer_hint(), dirty_style),
    ]);
    let footer = Paragraph::new(Text::from(vec![line]))
        .block(
            Block::default()
                .title("Actions")
                .title_style(
                    Style::default()
                        .fg(Color::Black)
                        .bg(Color::Green)
                        .add_modifier(Modifier::BOLD),
                )
                .border_type(BorderType::Rounded)
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Green))
                .style(Style::default().bg(panel_bg)),
        )
        .style(Style::default().bg(panel_bg))
        .wrap(Wrap { trim: false });
    frame.render_widget(footer, area);
}

fn footer_action_span(
    label: &'static str,
    selected: bool,
    accent: Color,
    panel_bg: Color,
) -> Span<'static> {
    let text = if selected {
        format!("> {label} <")
    } else {
        format!("[{label}]")
    };
    let style = if selected {
        Style::default()
            .fg(Color::Black)
            .bg(accent)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default()
            .fg(accent)
            .bg(panel_bg)
            .add_modifier(Modifier::BOLD)
    };
    Span::styled(text, style)
}

fn render_config_line(index: usize, line: &str) -> Line<'static> {
    if line.is_empty() {
        return Line::raw(String::new());
    }
    if index == 0 {
        return Line::styled(
            line.to_owned(),
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        );
    }
    if index == 1 {
        return render_config_step_line(line, Color::Green);
    }
    if line.starts_with('[') && line.ends_with(']') {
        return render_subsection_line(line, Color::Green);
    }
    if let Some(line) = render_form_line(line, Color::Green) {
        return line;
    }
    if line.starts_with("Type value")
        || line.starts_with("Tab ")
        || line.starts_with("Enter ")
        || line.starts_with("Ctrl-")
    {
        return Line::styled(line.to_owned(), Style::default().fg(Color::Yellow));
    }
    if config_line_is_meta(line) {
        return Line::styled(line.to_owned(), Style::default().fg(Color::DarkGray));
    }
    if config_line_looks_like_field(line) {
        return Line::styled(line.to_owned(), Style::default().fg(Color::White));
    }

    Line::styled(line.to_owned(), Style::default().fg(Color::Gray))
}

fn render_setup_line(line: &str) -> Line<'static> {
    if line.is_empty() {
        return Line::raw(String::new());
    }
    if line.starts_with('[') && line.ends_with(']') {
        return render_subsection_line(line, Color::Yellow);
    }
    if let Some(line) = render_form_line(line, Color::Yellow) {
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

fn render_form_line(line: &str, accent: Color) -> Option<Line<'static>> {
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
                .bg(row_bg)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::DarkGray)
        };
        let value_style = if selected {
            Style::default()
                .fg(Color::Black)
                .bg(accent)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::White)
        };
        return Some(Line::from(vec![
            Span::styled(if selected { "> " } else { "  " }, marker_style),
            Span::styled(format!("[{label}]"), value_style),
        ]));
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

    let marker_style = if selected {
        Style::default()
            .fg(accent)
            .bg(row_bg)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::DarkGray)
    };
    let label_style = if selected {
        Style::default()
            .fg(accent)
            .bg(row_bg)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::Cyan)
    };
    let value_style = if selected {
        Style::default()
            .fg(Color::White)
            .bg(row_bg)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::White)
    };
    let colon_style = if selected {
        Style::default().fg(Color::DarkGray).bg(row_bg)
    } else {
        Style::default().fg(Color::DarkGray)
    };

    let mut spans = vec![
        Span::styled(if selected { "> " } else { "  " }, marker_style),
        Span::styled(label.to_owned(), label_style),
        Span::styled(": ", colon_style),
        Span::styled(value.to_owned(), value_style),
    ];
    if let Some(action) = action {
        spans.push(Span::raw("  "));
        spans.push(Span::styled(
            format!("[{action}]"),
            if selected {
                Style::default()
                    .fg(Color::Black)
                    .bg(accent)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::Yellow)
            },
        ));
    }
    Some(Line::from(spans))
}

fn render_config_step_line(line: &str, accent: Color) -> Line<'static> {
    let mut spans = Vec::new();
    for token in line.split_whitespace() {
        let (text, style) = if token.starts_with('[') && token.ends_with(']') {
            (
                token
                    .trim_start_matches('[')
                    .trim_end_matches(']')
                    .to_owned(),
                Style::default()
                    .fg(Color::Black)
                    .bg(accent)
                    .add_modifier(Modifier::BOLD),
            )
        } else {
            (
                token.to_owned(),
                Style::default().fg(Color::Gray).bg(Color::Rgb(28, 32, 30)),
            )
        };
        spans.push(Span::styled(format!(" {text} "), style));
        spans.push(Span::raw(" "));
    }
    Line::from(spans)
}

fn render_subsection_line(line: &str, accent: Color) -> Line<'static> {
    let text = line.trim_start_matches('[').trim_end_matches(']');
    Line::from(vec![
        Span::styled(
            format!(" {text} "),
            Style::default()
                .fg(Color::Black)
                .bg(accent)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(" "),
    ])
}

fn selected_row_bg(accent: Color) -> Color {
    match accent {
        Color::Yellow => Color::Rgb(51, 43, 14),
        Color::Green => Color::Rgb(14, 36, 22),
        Color::Cyan => Color::Rgb(14, 32, 36),
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
        "env:",
    ]
    .iter()
    .any(|prefix| line.starts_with(prefix))
}

fn config_line_looks_like_field(line: &str) -> bool {
    matches!(line.chars().next(), Some(' ' | '>' | '*'))
}

fn render_modal(frame: &mut Frame, app: &AppState) {
    if !app.has_modal() {
        return;
    }

    let visual = modal_visual(app);
    let raw_lines = app.modal_lines();
    let title = app.modal_title().unwrap_or("Modal");
    let max_inner_width = frame.area().width.saturating_sub(8).max(24) as usize;
    let desired_inner_width = raw_lines
        .iter()
        .map(|line| line.chars().count())
        .chain(std::iter::once(title.chars().count()))
        .max()
        .unwrap_or(24)
        .saturating_add(2)
        .clamp(24, max_inner_width);
    let body_height = raw_lines
        .iter()
        .map(|line| wrapped_line_rows(line, desired_inner_width))
        .sum::<usize>()
        .max(4) as u16
        + 2;
    let area = centered_rect(
        desired_inner_width as u16 + 2,
        body_height.min(frame.area().height.saturating_sub(2)),
        frame.area(),
    );
    let lines = raw_lines
        .iter()
        .cloned()
        .map(|line| render_modal_line(line, visual.accent))
        .collect::<Vec<_>>();

    let backdrop = halo_rect(area, frame.area(), 4, 1);
    if backdrop.width > 0 && backdrop.height > 0 {
        frame.render_widget(Clear, backdrop);
        frame.render_widget(
            Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(Style::default().fg(visual.backdrop_border))
                .style(Style::default().bg(visual.backdrop_bg)),
            backdrop,
        );
    }
    let shadow = shadow_rect(area, frame.area());
    if shadow.width > 0 && shadow.height > 0 {
        frame.render_widget(
            Block::default().style(Style::default().bg(visual.shadow_bg)),
            shadow,
        );
    }
    frame.render_widget(Clear, area);
    let widget = Paragraph::new(Text::from(lines))
        .style(Style::default().bg(visual.modal_bg))
        .block(
            Block::default()
                .title(title)
                .title_style(
                    Style::default()
                        .fg(Color::Black)
                        .bg(visual.accent)
                        .add_modifier(Modifier::BOLD),
                )
                .border_type(BorderType::Rounded)
                .borders(Borders::ALL)
                .border_style(Style::default().fg(visual.accent))
                .style(Style::default().bg(visual.modal_bg)),
        )
        .wrap(Wrap { trim: false });
    frame.render_widget(widget, area);

    if let Some((label, offset, line_index)) = app.modal_input_cursor() {
        let rows_before = raw_lines
            .iter()
            .take(line_index)
            .map(|line| wrapped_line_rows(line, desired_inner_width))
            .sum::<usize>() as u16;
        let max_offset = desired_inner_width.saturating_sub(label.chars().count() + 2);
        let cursor_x = area
            .x
            .saturating_add(1 + format!("{label}: ").len() as u16 + offset.min(max_offset) as u16);
        let cursor_y = area.y.saturating_add(1 + rows_before);
        frame.set_cursor_position((cursor_x, cursor_y));
    }
}

fn render_modal_line(line: String, accent: Color) -> Line<'static> {
    if line.is_empty() {
        return Line::raw(String::new());
    }
    if line.starts_with("> ") {
        return Line::styled(
            line,
            Style::default()
                .fg(Color::Black)
                .bg(accent)
                .add_modifier(Modifier::BOLD),
        );
    }
    if line.starts_with("Up/Down ") || line.starts_with("Enter apply") {
        return Line::styled(line, Style::default().fg(accent));
    }
    if let Some((label, value)) = line.split_once(':') {
        return Line::from(vec![
            Span::styled(label.to_owned(), Style::default().fg(accent)),
            Span::styled(": ", Style::default().fg(Color::DarkGray)),
            Span::styled(
                value.trim_start().to_owned(),
                Style::default().fg(Color::White),
            ),
        ]);
    }
    Line::styled(line, Style::default().fg(Color::White))
}

fn centered_rect(width: u16, height: u16, area: Rect) -> Rect {
    let popup_width = width.min(area.width.saturating_sub(2)).max(24);
    let popup_height = height.min(area.height.saturating_sub(2)).max(6);
    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Fill(1),
            Constraint::Length(popup_height),
            Constraint::Fill(1),
        ])
        .split(area);
    let horizontal = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Fill(1),
            Constraint::Length(popup_width),
            Constraint::Fill(1),
        ])
        .split(vertical[1]);
    horizontal[1]
}

fn shadow_rect(area: Rect, bounds: Rect) -> Rect {
    let x = area.x.saturating_add(1);
    let y = area.y.saturating_add(1);
    let right = bounds.x.saturating_add(bounds.width);
    let bottom = bounds.y.saturating_add(bounds.height);
    Rect {
        x,
        y,
        width: area.width.min(right.saturating_sub(x)),
        height: area.height.min(bottom.saturating_sub(y)),
    }
}

fn inset_rect(area: Rect, x_pad: u16, y_pad: u16) -> Rect {
    let width = area.width.saturating_sub(x_pad.saturating_mul(2));
    let height = area.height.saturating_sub(y_pad.saturating_mul(2));
    Rect {
        x: area.x.saturating_add(x_pad),
        y: area.y.saturating_add(y_pad),
        width,
        height,
    }
}

fn halo_rect(area: Rect, bounds: Rect, x_pad: u16, y_pad: u16) -> Rect {
    let x = area.x.saturating_sub(x_pad);
    let y = area.y.saturating_sub(y_pad);
    let right = bounds.x.saturating_add(bounds.width);
    let bottom = bounds.y.saturating_add(bounds.height);
    let expanded_right = area.x.saturating_add(area.width).saturating_add(x_pad);
    let expanded_bottom = area.y.saturating_add(area.height).saturating_add(y_pad);
    Rect {
        x,
        y,
        width: expanded_right.min(right).saturating_sub(x),
        height: expanded_bottom.min(bottom).saturating_sub(y),
    }
}

struct ModalVisual {
    accent: Color,
    modal_bg: Color,
    backdrop_bg: Color,
    backdrop_border: Color,
    shadow_bg: Color,
}

fn modal_visual(app: &AppState) -> ModalVisual {
    match app.modal_title() {
        Some("API Key") => ModalVisual {
            accent: Color::Yellow,
            modal_bg: Color::Rgb(28, 26, 18),
            backdrop_bg: Color::Rgb(17, 16, 12),
            backdrop_border: Color::Rgb(90, 82, 30),
            shadow_bg: Color::Rgb(8, 8, 6),
        },
        Some("Model") | Some("FIM Model") | Some("Model ID") => ModalVisual {
            accent: Color::Cyan,
            modal_bg: Color::Rgb(18, 24, 30),
            backdrop_bg: Color::Rgb(12, 18, 22),
            backdrop_border: Color::Rgb(38, 84, 92),
            shadow_bg: Color::Rgb(6, 10, 12),
        },
        _ => ModalVisual {
            accent: Color::Green,
            modal_bg: Color::Rgb(19, 26, 22),
            backdrop_bg: Color::Rgb(12, 18, 15),
            backdrop_border: Color::Rgb(42, 88, 58),
            shadow_bg: Color::Rgb(6, 10, 8),
        },
    }
}

fn wrapped_line_rows(line: &str, width: usize) -> usize {
    if width == 0 {
        return 1;
    }
    let display_width = UnicodeWidthStr::width(line);
    if display_width == 0 {
        return 1;
    }
    display_width.div_ceil(width)
}

fn approval_block_title(app: &AppState) -> &'static str {
    let _ = app;
    " Review Tool Call "
}

fn short_path_label(path: &Path) -> String {
    path.file_name()
        .and_then(|value| value.to_str())
        .unwrap_or_else(|| path.to_str().unwrap_or("?"))
        .to_owned()
}

fn display_path_label(path: &Path) -> String {
    let display = path.to_string_lossy().into_owned();
    if let Ok(home) = env::var("HOME")
        && let Some(suffix) = display.strip_prefix(&home)
    {
        return format!("~{suffix}");
    }
    display
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
mod tests {
    use std::{collections::BTreeMap, path::Path};

    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use ratatui::layout::Rect;
    use termquill_kernel::{
        AgentConfig, CompactionConfig, MemoryConfig, PermissionConfig, RootConfig, SessionConfig,
        WorkspaceConfig,
    };
    use unicode_width::UnicodeWidthStr;

    use crate::app::{AppState, TimelineEntry, TimelineRole};

    use super::{
        TimelineRenderOptions, halo_rect, phase_accent, render_live_activity_line,
        render_timeline_entry_lines, render_timeline_entry_lines_with_options, shadow_rect,
        slash_selector_overlay_rect, user_message_bg, wrapped_line_rows,
    };

    fn test_config() -> RootConfig {
        RootConfig {
            workspace: WorkspaceConfig {
                root: ".".to_owned(),
            },
            session: SessionConfig {
                log_dir: ".termquill/sessions".to_owned(),
            },
            agent: AgentConfig {
                provider: "deepseek".to_owned(),
                model: "deepseek-v4-flash".to_owned(),
                max_turns: 8,
                tool_timeout_secs: 30,
            },
            permission: PermissionConfig::default(),
            memory: MemoryConfig { enabled: true },
            compaction: CompactionConfig::default(),
            providers: BTreeMap::new(),
            mcp_servers: Vec::new(),
        }
    }

    #[test]
    fn wrapped_line_rows_counts_visual_rows() {
        assert_eq!(wrapped_line_rows("", 10), 1);
        assert_eq!(wrapped_line_rows("short", 10), 1);
        assert_eq!(wrapped_line_rows("1234567890", 10), 1);
        assert_eq!(wrapped_line_rows("12345678901", 10), 2);
        assert_eq!(wrapped_line_rows("你好", 2), 2);
    }

    #[test]
    fn render_live_activity_line_shows_current_phase() -> anyhow::Result<()> {
        let mut app = AppState::from_root_config(Path::new("/tmp/termquill.toml"), &test_config());
        app.set_terminal_size(120, 30);
        app.handle_key_event(KeyEvent::new(KeyCode::Char('你'), KeyModifiers::NONE))?;
        app.handle_key_event(KeyEvent::new(KeyCode::Char('好'), KeyModifiers::NONE))?;
        let _ = app.submit_input()?;

        let line = render_live_activity_line(&app, phase_accent(&app.run_phase()))
            .expect("busy run should expose live activity");
        let plain = line
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect::<String>();

        assert!(plain.contains("thinking"));
        assert!(plain.contains("reasoning with"));
        Ok(())
    }

    #[test]
    fn render_timeline_entry_lines_preserves_multiline_blocks() {
        let entry = TimelineEntry {
            role: TimelineRole::Tool,
            text: "first line\nsecond line\nthird line".to_owned(),
        };

        let lines = render_timeline_entry_lines_with_options(
            &entry,
            &TimelineRenderOptions {
                expand_tool_previews: true,
                ..TimelineRenderOptions::default()
            },
            0,
        );

        assert!(lines.iter().any(|line| {
            line.spans
                .iter()
                .any(|span| span.content.as_ref().contains("text"))
        }));
        assert!(lines.iter().any(|line| {
            line.spans
                .iter()
                .any(|span| span.content.as_ref().contains("first line"))
        }));
    }

    #[test]
    fn render_timeline_entry_lines_separates_tool_header_and_json_body() {
        let entry = TimelineEntry {
            role: TimelineRole::Tool,
            text: r#"{"tool_name":"ls","status":"ok","call_id":"call_123","metadata":{"exit_code":0}}"#
                .to_owned(),
        };

        let lines = render_timeline_entry_lines(&entry);

        assert!(lines.iter().any(|line| {
            line.spans
                .iter()
                .any(|span| span.content.as_ref().contains("call_123"))
        }));
        assert!(lines.iter().any(|line| {
            line.spans
                .iter()
                .any(|span| span.content.as_ref().contains("meta"))
        }));
    }

    #[test]
    fn render_timeline_entry_lines_styles_basic_markdown() {
        let entry = TimelineEntry {
            role: TimelineRole::Assistant,
            text: "## Title\n- **bold** and `code`".to_owned(),
        };

        let lines = render_timeline_entry_lines(&entry);

        assert!(lines.iter().any(|line| {
            line.spans
                .iter()
                .any(|span| span.content.as_ref().contains("Title"))
        }));
        assert!(lines.iter().any(|line| {
            line.spans
                .iter()
                .any(|span| span.content.as_ref().contains("─"))
        }));
        assert!(lines.iter().any(|line| {
            line.spans
                .iter()
                .any(|span| span.content.as_ref().contains("bold"))
        }));
        assert!(lines.iter().any(|line| {
            line.spans
                .iter()
                .any(|span| span.content.as_ref().contains("code"))
        }));
    }

    #[test]
    fn render_timeline_entry_lines_compacts_assistant_blank_lines() {
        let entry = TimelineEntry {
            role: TimelineRole::Assistant,
            text: "hello\n\n## Title\n\n- item".to_owned(),
        };

        let lines = render_timeline_entry_lines(&entry);

        assert!(lines.iter().any(|line| {
            line.spans
                .iter()
                .any(|span| span.content.as_ref().contains("hello"))
        }));
        assert!(lines.iter().any(|line| {
            line.spans
                .iter()
                .any(|span| span.content.as_ref().contains("Title"))
        }));
        assert!(lines.iter().any(|line| {
            line.spans
                .iter()
                .any(|span| span.content.as_ref().contains("─"))
        }));
        assert!(lines.iter().any(|line| {
            line.spans
                .iter()
                .any(|span| span.content.as_ref().contains("item"))
        }));
    }

    #[test]
    fn render_timeline_entry_lines_preserves_cjk_adjacency() {
        let entry = TimelineEntry {
            role: TimelineRole::Assistant,
            text: "你好！很高兴再次见到你。".to_owned(),
        };

        let lines = render_timeline_entry_lines(&entry);
        let rendered = lines
            .iter()
            .flat_map(|line| line.spans.iter())
            .map(|span| span.content.as_ref())
            .collect::<String>();

        assert!(rendered.contains("你好！很高兴再次见到你。"));
        assert!(!rendered.contains("你 好"));
    }

    #[test]
    fn render_timeline_entry_lines_show_phase_block() {
        let entry = TimelineEntry {
            role: TimelineRole::Phase,
            text: "thinking|deepseek-v4-flash".to_owned(),
        };

        let lines = render_timeline_entry_lines(&entry);

        assert!(
            lines[0]
                .spans
                .iter()
                .any(|span| span.content.as_ref().contains("thinking"))
        );
        assert!(lines[1].spans.iter().any(|span| {
            span.content
                .as_ref()
                .contains("reasoning with deepseek-v4-flash")
        }));
    }

    #[test]
    fn render_timeline_entry_lines_show_thinking_trace_block() {
        let entry = TimelineEntry {
            role: TimelineRole::Thinking,
            text: "step 1\nstep 2".to_owned(),
        };

        let lines = render_timeline_entry_lines(&entry);

        assert!(
            lines[0]
                .spans
                .iter()
                .any(|span| span.content.as_ref().contains("thought"))
        );
        assert!(
            lines[0]
                .spans
                .iter()
                .any(|span| span.content.as_ref().contains("Ctrl-T expand"))
        );
        assert!(
            lines[0]
                .spans
                .iter()
                .any(|span| span.content.as_ref().contains("step 1"))
        );

        let expanded = render_timeline_entry_lines_with_options(
            &entry,
            &TimelineRenderOptions {
                expand_thinking_blocks: true,
                ..TimelineRenderOptions::default()
            },
            0,
        );
        assert!(
            expanded[0]
                .spans
                .iter()
                .any(|span| span.content.as_ref().contains("Ctrl-T collapse"))
        );
        assert!(expanded.iter().any(|line| {
            line.spans
                .iter()
                .any(|span| span.content.as_ref().contains("step 1"))
        }));
        assert!(expanded.iter().any(|line| {
            line.spans
                .iter()
                .any(|span| span.content.as_ref().contains("step 2"))
        }));
    }

    #[test]
    fn render_timeline_entry_lines_make_user_and_assistant_distinct() {
        let user = TimelineEntry {
            role: TimelineRole::User,
            text: "hello".to_owned(),
        };
        let assistant = TimelineEntry {
            role: TimelineRole::Assistant,
            text: "hello back".to_owned(),
        };

        let user_lines = render_timeline_entry_lines(&user);
        let assistant_lines = render_timeline_entry_lines(&assistant);

        assert!(
            user_lines[0]
                .spans
                .iter()
                .any(|span| span.style.bg == Some(user_message_bg()))
        );
        assert!(
            !assistant_lines
                .iter()
                .flat_map(|line| line.spans.iter())
                .any(|span| span.style.bg == Some(user_message_bg()))
        );
    }

    #[test]
    fn render_timeline_entry_lines_make_headings_primary_and_flush_left() {
        let entry = TimelineEntry {
            role: TimelineRole::Assistant,
            text: "## 关键架构决策\n正文内容".to_owned(),
        };

        let lines = render_timeline_entry_lines(&entry);
        let heading_plain = lines[0]
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect::<String>();
        let body_plain = lines[2]
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect::<String>();

        assert!(heading_plain.starts_with("关键架构决策"));
        assert!(!heading_plain.starts_with("▏"));
        assert_eq!(body_plain.trim_start(), "正文内容");
    }

    #[test]
    fn render_timeline_entry_lines_supports_task_lists_and_links() {
        let entry = TimelineEntry {
            role: TimelineRole::Assistant,
            text: "- [x] shipped [README](https://example.com)".to_owned(),
        };

        let lines = render_timeline_entry_lines(&entry);

        assert!(lines.iter().any(|line| {
            line.spans
                .iter()
                .any(|span| span.content.as_ref().contains("[x]"))
        }));
        assert!(lines.iter().any(|line| {
            line.spans
                .iter()
                .any(|span| span.content.as_ref().contains("README"))
        }));
        assert!(lines.iter().any(|line| {
            line.spans
                .iter()
                .any(|span| span.content.as_ref().contains("example.com"))
        }));
    }

    #[test]
    fn render_timeline_entry_lines_groups_paragraphs_and_code_blocks() {
        let entry = TimelineEntry {
            role: TimelineRole::Assistant,
            text: "first line\nsecond line\n\n```rust\nfn main() {}\n```".to_owned(),
        };

        let lines = render_timeline_entry_lines(&entry);

        assert!(lines.iter().any(|line| {
            line.spans
                .iter()
                .any(|span| span.content.as_ref().contains("first line"))
        }));
        assert!(lines.iter().any(|line| {
            line.spans
                .iter()
                .any(|span| span.content.as_ref().contains("second line"))
        }));
        assert!(lines.iter().any(|line| {
            line.spans
                .iter()
                .any(|span| span.content.as_ref().contains("rust"))
        }));
        assert!(lines.iter().any(|line| {
            line.spans
                .iter()
                .any(|span| span.content.as_ref().contains("fn main() {}"))
        }));
    }

    #[test]
    fn render_timeline_entry_lines_formats_markdown_tables_as_grid() {
        let entry = TimelineEntry {
            role: TimelineRole::Assistant,
            text: "| file | role |\n| --- | --- |\n| Cargo.toml | root |\n| src/lib.rs | core |"
                .to_owned(),
        };

        let lines = render_timeline_entry_lines(&entry);

        assert!(lines.iter().any(|line| {
            line.spans
                .iter()
                .any(|span| span.content.as_ref().contains("table"))
        }));
        assert!(lines.iter().any(|line| {
            line.spans
                .iter()
                .any(|span| span.content.as_ref().contains("┌"))
        }));
        assert!(lines.iter().any(|line| {
            line.spans
                .iter()
                .any(|span| span.content.as_ref().contains("Cargo.toml"))
        }));
    }

    #[test]
    fn render_timeline_entry_lines_wrap_markdown_tables_to_panel_width() {
        let entry = TimelineEntry {
            role: TimelineRole::Assistant,
            text: "| Phase | 内容 | 状态 |\n| --- | --- | --- |\n| **Phase 3** | planner/executor + compaction + memory + subagent + workspace confinement | 部分完成 |".to_owned(),
        };

        let lines = render_timeline_entry_lines_with_options(
            &entry,
            &TimelineRenderOptions {
                max_content_width: 48,
                ..TimelineRenderOptions::default()
            },
            0,
        );

        assert!(lines.iter().any(|line| {
            line.spans
                .iter()
                .any(|span| span.content.as_ref().contains("planner/executor"))
        }));
        assert!(lines.iter().all(|line| {
            let plain = line
                .spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect::<String>();
            UnicodeWidthStr::width(plain.as_str()) <= 50
        }));
        assert!(lines.iter().all(|line| {
            let plain = line
                .spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect::<String>();
            !plain.contains("**Phase 3**")
        }));
    }

    #[test]
    fn render_timeline_entry_lines_formats_tool_cards() {
        let entry = TimelineEntry {
            role: TimelineRole::Tool,
            text: r#"{
  "call_id": "call-1",
  "tool_name": "ls",
  "status": "ok",
  "preview_kind": "json",
  "summary": "first 2/4 lines · 64 B",
  "preview_lines": ["[", "  \".git\",", "  \"Cargo.toml\"", "]"],
  "preview_value": [".git", "Cargo.toml"],
  "hidden_lines": 0,
  "metadata_line": "bytes=64",
  "metadata": {"bytes": 64}
}"#
            .to_owned(),
        };

        let lines = render_timeline_entry_lines_with_options(
            &entry,
            &TimelineRenderOptions {
                expand_tool_previews: true,
                ..TimelineRenderOptions::default()
            },
            0,
        );

        assert!(
            lines[0]
                .spans
                .iter()
                .any(|span| span.content.as_ref().contains("ls"))
        );
        assert!(
            lines[1]
                .spans
                .iter()
                .any(|span| span.content.as_ref().contains("OK"))
        );
        assert!(
            lines[2]
                .spans
                .iter()
                .any(|span| span.content.as_ref().contains("bytes=64"))
        );
        assert!(lines.iter().any(|line| {
            line.spans
                .iter()
                .any(|span| span.content.as_ref().contains("files"))
        }));
        assert!(lines.iter().any(|line| {
            line.spans
                .iter()
                .any(|span| span.content.as_ref().contains("Cargo.toml"))
        }));
    }

    #[test]
    fn render_timeline_entry_lines_formats_grep_cards_by_file() {
        let entry = TimelineEntry {
            role: TimelineRole::Tool,
            text: r#"{
  "tool_name": "grep",
  "status": "ok",
  "preview_kind": "json",
  "summary": "first 2/2 lines · 91 B",
  "preview_lines": ["[]"],
  "preview_value": [
    {"path": "src/lib.rs", "line": 12, "text": "fn helper()"},
    {"path": "src/lib.rs", "line": 29, "text": "helper();"}
  ],
  "hidden_lines": 0
}"#
            .to_owned(),
        };

        let lines = render_timeline_entry_lines_with_options(
            &entry,
            &TimelineRenderOptions {
                expand_tool_previews: true,
                ..TimelineRenderOptions::default()
            },
            0,
        );

        assert!(lines.iter().any(|line| {
            line.spans
                .iter()
                .any(|span| span.content.as_ref().contains("matches"))
        }));
        assert!(lines.iter().any(|line| {
            line.spans
                .iter()
                .any(|span| span.content.as_ref().contains("src/lib.rs"))
        }));
        assert!(lines.iter().any(|line| {
            line.spans
                .iter()
                .any(|span| span.content.as_ref().contains("L12"))
        }));
    }

    #[test]
    fn render_timeline_entry_lines_renders_generic_json_tree_preview() {
        let entry = TimelineEntry {
            role: TimelineRole::Tool,
            text: r#"{
  "tool_name": "custom_tool",
  "status": "ok",
  "preview_kind": "json",
  "summary": "first 3/3 lines · 44 B",
  "preview_lines": ["{}"],
  "preview_value": {"root": {"leaf": "value"}},
  "hidden_lines": 0
}"#
            .to_owned(),
        };

        let lines = render_timeline_entry_lines_with_options(
            &entry,
            &TimelineRenderOptions {
                expand_tool_previews: true,
                ..TimelineRenderOptions::default()
            },
            0,
        );

        assert!(lines.iter().any(|line| {
            line.spans
                .iter()
                .any(|span| span.content.as_ref().contains("tree"))
        }));
        assert!(lines.iter().any(|line| {
            line.spans
                .iter()
                .any(|span| span.content.as_ref().contains("root"))
        }));
        assert!(lines.iter().any(|line| {
            line.spans
                .iter()
                .any(|span| span.content.as_ref().contains("leaf"))
        }));
    }

    #[test]
    fn render_timeline_entry_lines_hide_tool_preview_by_default() {
        let entry = TimelineEntry {
            role: TimelineRole::Tool,
            text: r##"{
  "tool_name": "read_file",
  "status": "ok",
  "preview_kind": "markdown",
  "summary": "first 2/2 lines · 18 B",
  "preview_lines": ["# Title", "- item"],
  "hidden_lines": 0
}"##
            .to_owned(),
        };

        let lines = render_timeline_entry_lines(&entry);

        assert!(lines.iter().any(|line| {
            line.spans
                .iter()
                .any(|span| span.content.as_ref().contains("preview hidden"))
        }));
        assert!(!lines.iter().any(|line| {
            line.spans
                .iter()
                .any(|span| span.content.as_ref().contains("# Title"))
        }));
    }

    #[test]
    fn shadow_rect_offsets_and_stays_within_bounds() {
        let area = Rect::new(10, 4, 20, 6);
        let bounds = Rect::new(0, 0, 40, 20);
        assert_eq!(shadow_rect(area, bounds), Rect::new(11, 5, 20, 6));

        let clipped = shadow_rect(Rect::new(30, 18, 10, 4), bounds);
        assert_eq!(clipped, Rect::new(31, 19, 9, 1));
    }

    #[test]
    fn halo_rect_expands_and_clips_to_bounds() {
        let area = Rect::new(10, 4, 20, 6);
        let bounds = Rect::new(0, 0, 40, 20);
        assert_eq!(halo_rect(area, bounds, 4, 1), Rect::new(6, 3, 28, 8));

        let clipped = halo_rect(Rect::new(1, 1, 10, 4), bounds, 4, 2);
        assert_eq!(clipped, Rect::new(0, 0, 15, 7));
    }

    #[test]
    fn slash_selector_overlay_rect_tracks_composer_width() {
        let live = Rect::new(0, 0, 120, 24);
        let composer = Rect::new(0, 20, 120, 4);

        assert_eq!(
            slash_selector_overlay_rect(live, composer, 6),
            Some(Rect::new(1, 14, 118, 6))
        );
    }
}

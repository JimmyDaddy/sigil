use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout},
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, BorderType, Borders, Clear, Paragraph, Wrap},
};

use crate::app::{
    AppState, ApprovalDiffLine, ApprovalDiffLineKind, ApprovalFileRow, ApprovalModalView,
};

use super::{
    diff::{DiffLineKind, diff_line_number_gutter, diff_line_style, number_unified_diff_lines},
    geometry::{centered_rect, halo_rect, shadow_rect},
    markdown::{MarkdownRenderOptions, render_inline_markdown_spans_with_options},
};

pub(super) fn render_approval_modal(frame: &mut Frame, app: &AppState) {
    let Some(view) = app.approval_modal_view() else {
        return;
    };
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
    .min(frame.area().width.saturating_sub(8).max(36) as usize);
    let area = centered_rect(
        inner_width as u16 + 2,
        frame.area().height.saturating_sub(4).min(30),
        frame.area(),
    );
    let backdrop = halo_rect(area, frame.area(), 5, 2);
    if backdrop.width > 0 && backdrop.height > 0 {
        frame.render_widget(Clear, backdrop);
        frame.render_widget(
            Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(Style::default().fg(Color::Rgb(52, 102, 84)))
                .style(Style::default().bg(Color::Rgb(10, 16, 14))),
            backdrop,
        );
    }
    let shadow = shadow_rect(area, frame.area());
    if shadow.width > 0 && shadow.height > 0 {
        frame.render_widget(
            Block::default().style(Style::default().bg(Color::Rgb(5, 8, 7))),
            shadow,
        );
    }
    frame.render_widget(Clear, area);
    let block = Block::default()
        .title(approval_block_title(app))
        .title_style(
            Style::default()
                .fg(Color::Black)
                .bg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        )
        .border_type(BorderType::Rounded)
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Rgb(94, 174, 127)))
        .style(Style::default().bg(Color::Rgb(18, 23, 26)));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if inner.width == 0 || inner.height == 0 {
        return;
    }

    let header_lines = approval_header_lines(&view, inner_width);
    let footer_lines = approval_footer_lines(&view);
    let layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length((header_lines.len() as u16).saturating_add(2)),
            Constraint::Min(8),
            Constraint::Length((footer_lines.len() as u16).saturating_add(2)),
        ])
        .split(inner);

    frame.render_widget(
        Paragraph::new(Text::from(header_lines))
            .block(
                Block::default()
                    .title("Summary")
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(Color::Rgb(66, 110, 140))),
            )
            .wrap(Wrap { trim: false }),
        layout[0],
    );

    let body_chunks = if !view.file_rows.is_empty() {
        let file_width = if layout[1].width >= 92 { 28 } else { 22 }
            .min(layout[1].width.saturating_sub(18))
            .max(16);
        Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Length(file_width), Constraint::Min(12)])
            .split(layout[1])
    } else {
        Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Min(1)])
            .split(layout[1])
    };

    if !view.file_rows.is_empty() {
        let file_lines = view
            .file_rows
            .iter()
            .enumerate()
            .map(|(index, row)| render_approval_file_row(index, row))
            .collect::<Vec<_>>();
        let selected_file_index = view
            .file_rows
            .iter()
            .position(|row| row.selected)
            .map(|index| index + 1)
            .unwrap_or(0);
        frame.render_widget(
            Paragraph::new(Text::from(file_lines))
                .block(
                    Block::default()
                        .title(format!(
                            "Files {selected_file_index}/{}",
                            view.file_rows.len()
                        ))
                        .borders(Borders::ALL)
                        .border_style(Style::default().fg(Color::Rgb(75, 90, 108))),
                )
                .wrap(Wrap { trim: false }),
            body_chunks[0],
        );
    }

    let diff_area = *body_chunks.last().unwrap_or(&layout[1]);
    let diff_block = Block::default()
        .title("Diff")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Rgb(75, 90, 108)));
    let diff_inner = diff_block.inner(diff_area);
    frame.render_widget(diff_block, diff_area);
    if diff_inner.width > 0 && diff_inner.height > 0 {
        let diff_sections = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(1), Constraint::Min(1)])
            .split(diff_inner);
        frame.render_widget(
            Paragraph::new(approval_diff_status_line(&view)),
            diff_sections[0],
        );
        let numbered =
            number_unified_diff_lines(view.diff_lines.iter().map(|line| line.text.as_str()));
        let diff_lines = view
            .diff_lines
            .iter()
            .cloned()
            .zip(numbered)
            .map(|(line, numbered)| {
                render_approval_diff_line(line, numbered.old_line, numbered.new_line)
            })
            .collect::<Vec<_>>();
        frame.render_widget(
            Paragraph::new(Text::from(diff_lines))
                .scroll((app.approval_scroll_back as u16, 0))
                .wrap(Wrap { trim: false }),
            diff_sections[1],
        );
    }

    frame.render_widget(
        Paragraph::new(Text::from(footer_lines))
            .block(
                Block::default()
                    .title("Actions")
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(Color::Rgb(75, 90, 108))),
            )
            .wrap(Wrap { trim: false }),
        layout[2],
    );
}

fn approval_block_title(_app: &AppState) -> &'static str {
    " Review Tool Call "
}

fn approval_header_lines(view: &ApprovalModalView, max_content_width: usize) -> Vec<Line<'static>> {
    let mut lines = vec![
        Line::from(vec![
            approval_badge(
                &view.access_label,
                if view.access_label.contains("write")
                    || view.access_label.contains("execute")
                    || view.access_label.contains("network")
                {
                    Color::Yellow
                } else {
                    Color::Green
                },
            ),
            Span::raw(" "),
            Span::styled(
                view.tool_name.clone(),
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw("  "),
            Span::styled("call", Style::default().fg(Color::DarkGray)),
            Span::raw(" "),
            Span::styled(view.call_id.clone(), Style::default().fg(Color::Cyan)),
        ]),
        Line::from(vec![Span::styled(
            view.preview_title.clone(),
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        )]),
    ];

    if view.metadata_collapsed {
        lines.push(Line::from(vec![
            approval_badge("meta hidden", Color::DarkGray),
            Span::raw(" "),
            Span::styled("press M to expand", Style::default().fg(Color::DarkGray)),
        ]));
    } else if view.preview_summary.trim().is_empty() {
        lines.push(Line::styled(
            "No preview summary provided.",
            Style::default().fg(Color::DarkGray),
        ));
    } else {
        let markdown_options = MarkdownRenderOptions::modal(max_content_width);
        lines.extend(view.preview_summary.lines().take(2).map(|line| {
            Line::from(render_inline_markdown_spans_with_options(
                line,
                Style::default().fg(Color::Gray),
                markdown_options,
            ))
        }));
    }

    let change_count = view.changed_files.len().max(view.file_rows.len());
    lines.push(Line::from(vec![
        Span::styled("files", Style::default().fg(Color::DarkGray)),
        Span::raw(" "),
        Span::styled(change_count.to_string(), Style::default().fg(Color::White)),
        Span::raw("  "),
        Span::styled("hunks", Style::default().fg(Color::DarkGray)),
        Span::raw(" "),
        Span::styled(
            view.hunk_total.to_string(),
            Style::default().fg(Color::White),
        ),
        Span::raw("  "),
        Span::styled("mode", Style::default().fg(Color::DarkGray)),
        Span::raw(" "),
        Span::styled(view.diff_mode_label, Style::default().fg(Color::Cyan)),
    ]));
    lines
}

fn approval_footer_lines(view: &ApprovalModalView) -> Vec<Line<'static>> {
    let file_hint = if view.file_rows.len() > 1 {
        "  ,/. file"
    } else {
        ""
    };
    vec![
        Line::from(vec![
            approval_badge("Y allow", Color::Green),
            Span::raw(" "),
            approval_badge("N deny", Color::Red),
            Span::raw(" "),
            approval_badge("M meta", Color::Blue),
            Span::raw(" "),
            approval_badge("V view", Color::Cyan),
        ]),
        Line::styled(
            format!("[,] hunk{file_hint}  Up/Down scroll"),
            Style::default().fg(Color::DarkGray),
        ),
    ]
}

fn render_approval_file_row(index: usize, row: &ApprovalFileRow) -> Line<'static> {
    let marker = if row.selected { "> " } else { "  " };
    let style = if row.selected {
        Style::default()
            .fg(Color::Black)
            .bg(Color::Rgb(108, 202, 180))
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::White)
    };
    Line::from(vec![
        Span::styled(format!("{marker}{} ", index + 1), style),
        Span::styled(row.path.clone(), style),
    ])
}

fn approval_diff_status_line(view: &ApprovalModalView) -> Line<'static> {
    let hunk_label = if view.hunk_total == 0 {
        "hunk 0/0".to_owned()
    } else {
        format!("hunk {}/{}", view.active_hunk_index, view.hunk_total)
    };
    Line::from(vec![
        Span::styled("path", Style::default().fg(Color::DarkGray)),
        Span::raw(" "),
        Span::styled(view.diff_label.clone(), Style::default().fg(Color::White)),
        Span::raw("  "),
        Span::styled("mode", Style::default().fg(Color::DarkGray)),
        Span::raw(" "),
        Span::styled(view.diff_mode_label, Style::default().fg(Color::Cyan)),
        Span::raw("  "),
        Span::styled(hunk_label, Style::default().fg(Color::Yellow)),
    ])
}

fn render_approval_diff_line(
    line: ApprovalDiffLine,
    old_line: Option<usize>,
    new_line: Option<usize>,
) -> Line<'static> {
    let (accent, body_style) = diff_line_style(approval_diff_line_kind(line.kind));
    let marker = if line.active_hunk { ">" } else { "│" };
    let marker_style = if line.active_hunk {
        Style::default()
            .fg(Color::Black)
            .bg(Color::Yellow)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(accent)
    };
    let body_style = if line.active_hunk {
        body_style.bg(Color::Rgb(58, 52, 18))
    } else {
        body_style
    };
    Line::from(vec![
        Span::styled(format!("{marker} "), marker_style),
        Span::styled(
            diff_line_number_gutter(old_line, new_line),
            Style::default().fg(Color::DarkGray),
        ),
        Span::styled(
            if line.text.is_empty() {
                " ".to_owned()
            } else {
                line.text
            },
            body_style,
        ),
    ])
}

fn approval_diff_line_kind(kind: ApprovalDiffLineKind) -> DiffLineKind {
    match kind {
        ApprovalDiffLineKind::Header => DiffLineKind::Header,
        ApprovalDiffLineKind::Hunk => DiffLineKind::Hunk,
        ApprovalDiffLineKind::Added => DiffLineKind::Added,
        ApprovalDiffLineKind::Removed => DiffLineKind::Removed,
        ApprovalDiffLineKind::Context => DiffLineKind::Context,
    }
}

fn approval_badge(label: &str, color: Color) -> Span<'static> {
    Span::styled(
        format!(" {label} "),
        Style::default()
            .fg(Color::Black)
            .bg(color)
            .add_modifier(Modifier::BOLD),
    )
}

use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, BorderType, Borders, Paragraph, Wrap},
};

use crate::app::AppState;

use super::{modal::render_modal, shell::render_status};

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

use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, Borders, Paragraph},
};

use crate::app::{
    PendingUserInputForm, UserInputDraftValue, UserInputFormAction, UserInputFormSource,
};

use super::{
    text::wrap_terminal_lines,
    theme::{Theme, styles},
};

pub(super) fn render_user_input_form(
    frame: &mut Frame,
    area: Rect,
    form: &PendingUserInputForm,
    theme: &Theme,
) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(2),
            Constraint::Min(1),
            Constraint::Length(2),
        ])
        .split(area);
    let title = match &form.source {
        UserInputFormSource::DurableAgent if form.queue_length > 1 => format!(
            "Input required {} of {} · Ctrl-N/P switch",
            form.queue_position, form.queue_length
        ),
        UserInputFormSource::DurableAgent => "Input required".to_owned(),
        UserInputFormSource::Mcp { .. } => "MCP input required".to_owned(),
    };
    let header = Text::from(vec![
        Line::from(Span::styled(
            title,
            Style::default()
                .fg(theme.palette.accent_warning)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(Span::styled(
            if form.recovery_command.is_some() {
                "An accepted answer is durable · Enter resumes the exact continuation · Esc close"
            } else if !form.focus_actions
                && form
                    .view
                    .questions
                    .get(form.focused_question)
                    .is_some_and(|question| {
                        matches!(
                            question.field,
                            sigil_kernel::UserInputFieldKindV1::Text {
                                multiline: true,
                                ..
                            }
                        )
                    })
            {
                "Tab fields/actions · Pg scroll · Enter newline · Ctrl-Enter actions · Esc close"
            } else {
                "Tab fields/actions · ↑↓ choose · Pg scroll · Space toggle · Enter actions · Esc close"
            },
            styles::muted(&theme.palette),
        )),
    ]);
    frame.render_widget(
        Paragraph::new(header).style(styles::body(&theme.palette)),
        rows[0],
    );
    render_fields(frame, rows[1], form, theme);
    render_actions(frame, rows[2], form, theme);
}

fn render_fields(frame: &mut Frame, area: Rect, form: &PendingUserInputForm, theme: &Theme) {
    let block = Block::default()
        .borders(Borders::TOP | Borders::BOTTOM)
        .border_style(Style::default().fg(theme.palette.border_subtle))
        .style(styles::body(&theme.palette));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if inner.width == 0 || inner.height == 0 {
        return;
    }
    let mut lines = vec![
        Line::raw(form.view.prompt.clone()),
        Line::raw(String::new()),
    ];
    if form.recovery_command.is_some() {
        lines.push(Line::from(Span::styled(
            "The answer was accepted before the previous owner stopped. Resume reuses the exact durable command and does not ask the provider to repeat the question.",
            styles::muted(&theme.palette),
        )));
        if let Some(receipt) = form
            .request
            .as_ref()
            .and_then(|request| request.answer_receipt.as_ref())
            && !receipt.answered_question_ids.is_empty()
        {
            lines.push(Line::raw(format!(
                "Answered fields: {}",
                receipt.answered_question_ids.join(", ")
            )));
        }
    } else {
        for (index, (question, draft)) in form.view.questions.iter().zip(&form.drafts).enumerate() {
            let focused = !form.focus_actions && index == form.focused_question;
            lines.push(Line::from(vec![
                Span::styled(
                    if focused { "▶ " } else { "  " },
                    Style::default().fg(theme.palette.accent_primary),
                ),
                Span::styled(
                    format!("{}: ", question.header),
                    Style::default().add_modifier(Modifier::BOLD),
                ),
                Span::raw(question.question.clone()),
            ]));
            if let Some(description) = &question.description {
                lines.push(Line::from(Span::styled(
                    format!("    {description}"),
                    styles::muted(&theme.palette),
                )));
            }
            lines.extend(answer_lines(question, draft, focused, theme));
            lines.push(Line::raw(String::new()));
        }
    }
    lines = wrap_terminal_lines(lines, inner.width as usize);
    let max_scroll = lines.len().saturating_sub(inner.height as usize);
    form.scroll_extent.set(max_scroll);
    let scroll = form.scroll.min(max_scroll);
    frame.render_widget(
        Paragraph::new(Text::from(
            lines
                .into_iter()
                .skip(scroll)
                .take(inner.height as usize)
                .collect::<Vec<_>>(),
        ))
        .style(styles::body(&theme.palette)),
        inner,
    );
}

fn answer_lines(
    question: &sigil_kernel::UserInputQuestionV1,
    draft: &UserInputDraftValue,
    focused: bool,
    theme: &Theme,
) -> Vec<Line<'static>> {
    let marker = if focused { "    › " } else { "      " };
    match (&question.field, draft) {
        (_, UserInputDraftValue::Text(value))
        | (_, UserInputDraftValue::Number(value))
        | (_, UserInputDraftValue::Integer(value)) => vec![Line::from(vec![
            Span::styled(marker, Style::default().fg(theme.palette.accent_primary)),
            Span::styled(
                if value.is_empty() {
                    "type an answer".to_owned()
                } else {
                    value.clone()
                },
                if value.is_empty() {
                    styles::muted(&theme.palette)
                } else {
                    styles::body(&theme.palette)
                },
            ),
        ])],
        (_, UserInputDraftValue::Boolean(value)) => vec![Line::raw(format!(
            "{marker}{}",
            match value {
                Some(true) => "Yes",
                Some(false) => "No",
                None => "Not answered",
            }
        ))],
        (
            sigil_kernel::UserInputFieldKindV1::SingleSelect {
                options,
                allow_other,
            },
            UserInputDraftValue::SingleSelect { selected, other },
        ) => {
            let mut values = options
                .iter()
                .enumerate()
                .map(|(index, option)| {
                    Line::raw(format!(
                        "    {} {}",
                        if Some(index) == *selected {
                            "●"
                        } else {
                            "○"
                        },
                        option.label
                    ))
                })
                .collect::<Vec<_>>();
            if *allow_other {
                values.push(Line::raw(format!(
                    "    {} Other{}",
                    if *selected == Some(options.len()) {
                        "●"
                    } else {
                        "○"
                    },
                    if other.is_empty() {
                        String::new()
                    } else {
                        format!(": {other}")
                    }
                )));
            }
            values
        }
        (
            sigil_kernel::UserInputFieldKindV1::MultiSelect { options, .. },
            UserInputDraftValue::MultiSelect { cursor, selected },
        ) => options
            .iter()
            .enumerate()
            .map(|(index, option)| {
                Line::raw(format!(
                    "    {} {} {}",
                    if index == *cursor { "›" } else { " " },
                    if selected.contains(&option.id) {
                        "[x]"
                    } else {
                        "[ ]"
                    },
                    option.label
                ))
            })
            .collect(),
        _ => Vec::new(),
    }
}

fn render_actions(frame: &mut Frame, area: Rect, form: &PendingUserInputForm, theme: &Theme) {
    let spans = UserInputFormAction::ORDER
        .iter()
        .filter(|action| action_available(form, **action))
        .flat_map(|action| {
            let selected = form.focus_actions && *action == form.selected_action;
            let style = if selected {
                Style::default()
                    .fg(theme.palette.button_selected_fg)
                    .bg(theme.palette.button_selected_bg)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(theme.palette.button_inactive_fg)
            };
            [Span::raw("  "), Span::styled(action.label(), style)]
        })
        .collect::<Vec<_>>();
    frame.render_widget(
        Paragraph::new(Line::from(spans)).style(styles::body(&theme.palette)),
        area,
    );
}

fn action_available(form: &PendingUserInputForm, action: UserInputFormAction) -> bool {
    if action == UserInputFormAction::Resume {
        return form.recovery_command.is_some();
    }
    if form.recovery_command.is_some() {
        return false;
    }
    let expected = match action {
        UserInputFormAction::Resume => unreachable!("resume handled above"),
        UserInputFormAction::Submit => sigil_kernel::UserInputActionV1::Submit,
        UserInputFormAction::Decline => sigil_kernel::UserInputActionV1::Decline,
        UserInputFormAction::CancelRun => sigil_kernel::UserInputActionV1::CancelRun,
    };
    form.view.allowed_actions.contains(&expected)
}

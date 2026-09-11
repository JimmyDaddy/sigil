use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, BorderType, Borders, Paragraph},
};

use crate::surface::{
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
    if form.is_plan_revision_editor() {
        render_plan_revision_form(frame, area, form, theme);
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
    let title = if is_plan_revision(form) {
        "Plan revision · current plan stays active".to_owned()
    } else {
        match &form.source {
            UserInputFormSource::DurableAgent if form.queue_length > 1 => format!(
                "Input required {} of {} · Ctrl-N/P switch",
                form.queue_position, form.queue_length
            ),
            UserInputFormSource::DurableAgent => "Input required".to_owned(),
            UserInputFormSource::Mcp { .. } => "MCP input required".to_owned(),
        }
    };
    let header = Text::from(vec![
        Line::from(Span::styled(
            title,
            Style::default()
                .fg(theme.palette.accent_warning)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(Span::styled(
            if is_plan_revision(form) {
                "Describe the change · Enter newline · Ctrl-Enter actions · Esc close"
            } else if form.recovery_command.is_some() {
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
                "Tab fields/actions · ↑↓ choose · Pg scroll · Space toggle · Enter next field · Ctrl-Enter actions · Esc close"
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

fn render_plan_revision_form(
    frame: &mut Frame,
    area: Rect,
    form: &PendingUserInputForm,
    theme: &Theme,
) {
    let card = if area.width >= 72 && area.height >= 14 {
        plan_revision_card_area(area)
    } else {
        area
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(theme.palette.border_focus))
        .style(styles::body(&theme.palette).bg(theme.palette.modal_bg))
        .title(Line::from(Span::styled(
            " PLAN REVISION ",
            Style::default()
                .fg(theme.palette.accent_info)
                .add_modifier(Modifier::BOLD),
        )));
    let inner = block.inner(card);
    frame.render_widget(block, card);
    if inner.width == 0 || inner.height == 0 {
        return;
    }

    let header_height = if inner.height >= 8 {
        2
    } else {
        1.min(inner.height.saturating_sub(5))
    };
    let footer_height =
        if inner.height >= 8 { 3 } else { 2 }.min(inner.height.saturating_sub(header_height + 3));
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(header_height),
            Constraint::Min(1),
            Constraint::Length(footer_height),
        ])
        .split(inner);
    render_plan_revision_header(frame, rows[0], form, theme);
    render_plan_revision_editor(frame, rows[1], form, theme);
    render_plan_revision_actions(frame, rows[2], form, theme);
}

fn plan_revision_card_area(area: Rect) -> Rect {
    let width = area.width.min(110);
    let height = area.height.min(20);
    Rect {
        x: area.x + area.width.saturating_sub(width) / 2,
        y: area.y + area.height.saturating_sub(height) / 2,
        width,
        height,
    }
}

fn render_plan_revision_header(
    frame: &mut Frame,
    area: Rect,
    form: &PendingUserInputForm,
    theme: &Theme,
) {
    let lines = vec![
        Line::from(Span::styled(
            form.view.prompt.clone(),
            Style::default().add_modifier(Modifier::BOLD),
        )),
        Line::from(Span::styled(
            "Your current plan stays available.",
            styles::muted(&theme.palette),
        )),
    ];
    frame.render_widget(
        Paragraph::new(Text::from(lines))
            .style(styles::body(&theme.palette).bg(theme.palette.modal_bg)),
        area,
    );
}

fn render_plan_revision_editor(
    frame: &mut Frame,
    area: Rect,
    form: &PendingUserInputForm,
    theme: &Theme,
) {
    let focused = form.open && !form.plan_revision_editor.submitting;
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(if focused {
            theme.palette.border_focus
        } else {
            theme.palette.border_subtle
        }))
        .style(styles::body(&theme.palette).bg(theme.palette.surface_input))
        .title(Line::from(vec![
            Span::styled(
                " REVISION REQUEST ",
                Style::default().add_modifier(Modifier::BOLD),
            ),
            Span::styled("required", styles::muted(&theme.palette)),
        ]));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if inner.width == 0 || inner.height == 0 {
        return;
    }

    form.plan_revision_editor
        .viewport
        .set((inner.width as usize, inner.height as usize));
    let value = form
        .drafts
        .first()
        .and_then(|draft| match draft {
            UserInputDraftValue::Text(value) => Some(value.as_str()),
            _ => None,
        })
        .unwrap_or("");
    let layout = form
        .plan_revision_editor
        .layout(value, inner.width as usize);
    let max_scroll = layout.rows.len().saturating_sub(inner.height as usize);
    form.scroll_extent.set(max_scroll);
    let mut scroll = form.scroll.min(max_scroll);
    let (cursor_row, cursor_column) = layout.cursor_position;
    if cursor_row < scroll {
        scroll = cursor_row;
    } else if cursor_row >= scroll + inner.height as usize {
        scroll = cursor_row + 1 - inner.height as usize;
    }
    let lines = if value.is_empty() {
        vec![Line::from(Span::styled(
            "Describe the changes you want…",
            styles::muted(&theme.palette),
        ))]
    } else {
        layout.rows.into_iter().map(Line::raw).collect()
    };
    frame.render_widget(
        Paragraph::new(Text::from(
            lines
                .into_iter()
                .skip(scroll)
                .take(inner.height as usize)
                .collect::<Vec<_>>(),
        ))
        .style(styles::body(&theme.palette).bg(theme.palette.surface_input)),
        inner,
    );
    if focused {
        frame.set_cursor_position((
            inner.x + cursor_column.min(inner.width.saturating_sub(1) as usize) as u16,
            inner.y + cursor_row.saturating_sub(scroll) as u16,
        ));
    }
}

fn render_plan_revision_actions(
    frame: &mut Frame,
    area: Rect,
    form: &PendingUserInputForm,
    theme: &Theme,
) {
    let submitting = form.plan_revision_editor.submitting;
    let action_style = if submitting {
        styles::muted(&theme.palette)
    } else {
        Style::default()
            .fg(theme.palette.accent_primary)
            .add_modifier(Modifier::BOLD)
    };
    let mut lines = vec![Line::from(vec![
        Span::styled(
            if submitting {
                "Submitting revision…"
            } else {
                "Enter  Submit revision"
            },
            action_style,
        ),
        Span::styled("    Esc  Back", styles::muted(&theme.palette)),
    ])];
    if let Some(error) = form.plan_revision_editor.error.as_ref() {
        lines.push(Line::from(Span::styled(
            crate::ui::sanitize_terminal_text(error),
            Style::default().fg(theme.palette.accent_danger),
        )));
    }
    lines.push(Line::from(Span::styled(
        if submitting {
            "Waiting for confirmation"
        } else {
            "Shift+Enter / Ctrl+J  New line"
        },
        styles::muted(&theme.palette),
    )));
    frame.render_widget(
        Paragraph::new(Text::from(lines))
            .style(styles::body(&theme.palette).bg(theme.palette.modal_bg)),
        area,
    );
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
            "Your answer is saved. Resume reuses the accepted answer and checks that the request can continue.",
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
            [
                Span::raw("  "),
                Span::styled(action_label(form, *action), style),
            ]
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

fn is_plan_revision(form: &PendingUserInputForm) -> bool {
    form.request.as_ref().is_some_and(|request| {
        matches!(
            &request.source,
            sigil_kernel::UserInputSourceV1::PlanRevision { .. }
        )
    })
}

fn action_label(form: &PendingUserInputForm, action: UserInputFormAction) -> &'static str {
    if is_plan_revision(form) {
        return match action {
            UserInputFormAction::Submit => "Prepare revised plan",
            UserInputFormAction::Decline => "Keep current plan",
            UserInputFormAction::Resume => "Resume revision",
            UserInputFormAction::CancelRun => "Cancel plan run",
        };
    }
    action.label()
}

#[cfg(test)]
#[path = "tests/user_input_form_tests.rs"]
mod tests;

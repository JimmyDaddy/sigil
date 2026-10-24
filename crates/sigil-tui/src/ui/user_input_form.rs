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
    let action_lines = regular_action_lines(form, theme, area.width as usize, area.height <= 12);
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Min(1),
            Constraint::Length(action_lines.len().min(u16::MAX as usize) as u16),
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
    let header = Line::from(Span::styled(
        title,
        Style::default()
            .fg(theme.palette.accent_warning)
            .add_modifier(Modifier::BOLD),
    ));
    frame.render_widget(
        Paragraph::new(header).style(styles::body(&theme.palette)),
        rows[0],
    );
    render_fields(frame, rows[1], form, theme);
    frame.render_widget(
        Paragraph::new(Text::from(action_lines)).style(styles::body(&theme.palette)),
        rows[2],
    );
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
    let mut lines = if form.view.prompt_origin
        == Some(sigil_kernel::UserInputPromptOriginV1::HostQuestionGroup)
    {
        Vec::new()
    } else {
        wrap_terminal_lines(
            vec![
                Line::raw(form.view.prompt.clone()),
                Line::raw(String::new()),
            ],
            inner.width as usize,
        )
    };
    let mut question_offsets = Vec::with_capacity(form.view.questions.len());
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
            let question_start = lines.len();
            let focused = !form.focus_actions && index == form.focused_question;
            let mut question_lines = vec![Line::from(vec![
                Span::styled(
                    if focused { "▶ " } else { "  " },
                    Style::default().fg(theme.palette.accent_primary),
                ),
                Span::styled(
                    format!("{}. ", index + 1),
                    Style::default().add_modifier(Modifier::BOLD),
                ),
                Span::raw(question.question.clone()),
            ])];
            if let Some(description) = &question.description {
                question_lines.push(Line::from(Span::styled(
                    format!("    {description}"),
                    styles::muted(&theme.palette),
                )));
            }
            lines.extend(wrap_terminal_lines(question_lines, inner.width as usize));
            let mut focused_row = question_start;
            let focused_answer = match draft {
                UserInputDraftValue::SingleSelect { selected, .. } => *selected,
                UserInputDraftValue::MultiSelect { cursor, .. } => Some(*cursor),
                UserInputDraftValue::Text(_) => Some(0),
            };
            for (answer_index, answer) in answer_lines(question, draft, focused, theme)
                .into_iter()
                .enumerate()
            {
                let answer_rows = wrap_terminal_lines(vec![answer], inner.width as usize);
                if focused_answer == Some(answer_index) {
                    focused_row = lines
                        .len()
                        .saturating_add(answer_rows.len().saturating_sub(1));
                }
                lines.extend(answer_rows);
            }
            question_offsets.push(
                question_start.max(
                    focused_row
                        .saturating_add(1)
                        .saturating_sub(inner.height as usize),
                ),
            );
            lines.push(Line::raw(String::new()));
        }
    }
    *form.question_scroll_offsets.borrow_mut() = question_offsets;
    lines = wrap_terminal_lines(lines, inner.width as usize);
    let max_scroll = lines.len().saturating_sub(inner.height as usize);
    form.scroll_extent.set(max_scroll);
    let scroll = if form.follow_focused_field {
        if form.focus_actions {
            max_scroll
        } else {
            form.question_scroll_offsets
                .borrow()
                .get(form.focused_question)
                .copied()
                .unwrap_or(0)
                .min(max_scroll)
        }
    } else {
        form.scroll.min(max_scroll)
    };
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
    match draft {
        UserInputDraftValue::Text(value) => vec![Line::from(vec![
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
        UserInputDraftValue::SingleSelect {
            selected, other, ..
        } => {
            let mut values = question
                .options
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
            values.push(Line::raw(format!(
                "    {}{} Other{}",
                if focused && *selected == Some(question.options.len()) {
                    "› "
                } else {
                    ""
                },
                if *selected == Some(question.options.len()) {
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
            values
        }
        UserInputDraftValue::MultiSelect {
            cursor,
            selected,
            other,
        } => question
            .options
            .iter()
            .enumerate()
            .map(|(index, option)| {
                Line::raw(format!(
                    "    {} {} {}",
                    if focused && index == *cursor {
                        "›"
                    } else {
                        " "
                    },
                    if selected.contains(&option.id) {
                        "[x]"
                    } else {
                        "[ ]"
                    },
                    option.label
                ))
            })
            .chain(std::iter::once(Line::raw(format!(
                "    {} Other{}",
                if focused && *cursor == question.options.len() {
                    "›"
                } else {
                    " "
                },
                if other.is_empty() {
                    String::new()
                } else {
                    format!(": {other}")
                }
            ))))
            .collect(),
    }
}

fn regular_action_lines(
    form: &PendingUserInputForm,
    theme: &Theme,
    width: usize,
    compact: bool,
) -> Vec<Line<'static>> {
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
    let mut lines = vec![Line::from(spans)];
    if let Some(error) = &form.validation_error {
        lines.push(Line::from(Span::styled(
            error.message.clone(),
            Style::default().fg(if error.question_index.is_some() {
                theme.palette.accent_danger
            } else {
                theme.palette.accent_warning
            }),
        )));
    }
    let hints = if form.recovery_command.is_some() {
        vec!["Enter Resume · Esc Close".to_owned()]
    } else if form.focus_actions {
        vec!["↑ Answers · ←→ / Tab Actions · Enter Confirm · Esc Close".to_owned()]
    } else {
        let next = if form.focused_question + 1 < form.view.questions.len() {
            "Next question"
        } else {
            "Actions"
        };
        if form.other_answer_focused() {
            if compact {
                vec![
                    "Type Other · Space inserts space · ↑↓ Options · Tab Next · Esc Close"
                        .to_owned(),
                ]
            } else {
                vec![
                    format!("Type Other · Space inserts space · Enter {next} · Esc Close"),
                    "↑↓ Options · Tab Next · Shift-Tab Previous".to_owned(),
                ]
            }
        } else if form
            .view
            .questions
            .get(form.focused_question)
            .is_some_and(|question| question.options.is_empty())
        {
            vec![if compact {
                format!("Enter {next} · Tab Next · Esc Close")
            } else {
                format!("Enter {next} · Tab Next · Shift-Tab Previous · Esc Close")
            }]
        } else {
            let choose = if matches!(
                form.drafts.get(form.focused_question),
                Some(UserInputDraftValue::SingleSelect {
                    selected_by_user: true,
                    ..
                })
            ) {
                "↑↓ Questions · Space Change"
            } else if matches!(
                form.drafts.get(form.focused_question),
                Some(UserInputDraftValue::MultiSelect { .. })
            ) {
                "↑↓ Choose · Space Toggle"
            } else {
                "↑↓ Choose · Space Select"
            };
            if compact {
                vec![format!("{choose} · Tab Next · Esc Close")]
            } else {
                vec![
                    format!("{choose} · Enter {next} · Esc Close"),
                    "Type / paste Other · Tab Next · Shift-Tab Previous".to_owned(),
                ]
            }
        }
    };
    lines.extend(
        hints
            .into_iter()
            .map(|hint| Line::from(Span::styled(hint, styles::muted(&theme.palette)))),
    );
    if !compact && !form.focus_actions && form.scroll_extent.get() > 0 {
        lines.push(Line::from(Span::styled(
            "PgUp/PgDn Scroll · Ctrl-Enter Actions",
            styles::muted(&theme.palette),
        )));
    }
    wrap_terminal_lines(lines, width)
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

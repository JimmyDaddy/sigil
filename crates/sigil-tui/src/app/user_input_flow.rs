use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::{AppAction, AppState, PendingUserInputForm, UserInputDraftValue, UserInputFormAction};

impl AppState {
    pub(in crate::app) fn handle_user_input_form_key_event(
        &mut self,
        key: KeyEvent,
    ) -> Option<Option<AppAction>> {
        let open = self
            .composer
            .pending_user_input
            .as_ref()
            .is_some_and(|form| form.open);
        self.composer.pending_user_input.as_ref()?;
        if !open {
            if key.code == KeyCode::BackTab && key.modifiers == KeyModifiers::SHIFT {
                if let Some(form) = self.composer.pending_user_input.as_mut() {
                    form.open = true;
                }
                self.last_notice = Some("input form reopened".to_owned());
                return Some(None);
            }
            return None;
        }
        if key.code == KeyCode::Esc && key.modifiers.is_empty() {
            if let Some(form) = self.composer.pending_user_input.as_mut() {
                form.open = false;
            }
            self.last_notice = Some("input form closed; Shift-Tab reopens it".to_owned());
            return Some(None);
        }
        let focus_actions = self
            .composer
            .pending_user_input
            .as_ref()
            .is_some_and(|form| form.focus_actions);
        if focus_actions {
            return self.handle_user_input_action_key(key);
        }
        self.handle_user_input_field_key(key)
    }

    fn handle_user_input_action_key(&mut self, key: KeyEvent) -> Option<Option<AppAction>> {
        match key.code {
            KeyCode::Tab | KeyCode::Right if key.modifiers.is_empty() => {
                self.select_adjacent_user_input_action(1);
                Some(None)
            }
            KeyCode::BackTab | KeyCode::Left
                if key.modifiers.is_empty() || key.modifiers == KeyModifiers::SHIFT =>
            {
                self.select_adjacent_user_input_action(-1);
                Some(None)
            }
            KeyCode::Up if key.modifiers.is_empty() => {
                if let Some(form) = self.composer.pending_user_input.as_mut() {
                    form.focus_actions = false;
                }
                Some(None)
            }
            KeyCode::Enter if key.modifiers.is_empty() => Some(self.submit_user_input_action()),
            _ => Some(None),
        }
    }

    fn handle_user_input_field_key(&mut self, key: KeyEvent) -> Option<Option<AppAction>> {
        if self
            .composer
            .pending_user_input
            .as_ref()
            .is_some_and(|form| form.recovery_command.is_some())
        {
            if let Some(form) = self.composer.pending_user_input.as_mut() {
                form.focus_actions = true;
            }
            return Some(None);
        }
        match key.code {
            KeyCode::Tab if key.modifiers.is_empty() => {
                if let Some(form) = self.composer.pending_user_input.as_mut() {
                    if form.focused_question + 1 < form.request.questions.len() {
                        form.focused_question += 1;
                    } else {
                        form.focus_actions = true;
                    }
                }
                self.reveal_focused_user_input_region();
                Some(None)
            }
            KeyCode::BackTab if key.modifiers == KeyModifiers::SHIFT => {
                if let Some(form) = self.composer.pending_user_input.as_mut() {
                    form.focused_question = form.focused_question.saturating_sub(1);
                }
                self.reveal_focused_user_input_region();
                Some(None)
            }
            KeyCode::Enter
                if key.modifiers.is_empty() && self.focused_user_input_is_multiline() =>
            {
                self.edit_user_input_text(Some('\n'));
                Some(None)
            }
            KeyCode::Enter
                if key.modifiers.is_empty() || key.modifiers == KeyModifiers::CONTROL =>
            {
                if let Some(form) = self.composer.pending_user_input.as_mut() {
                    form.focus_actions = true;
                }
                self.reveal_focused_user_input_region();
                Some(None)
            }
            KeyCode::Up if key.modifiers.is_empty() => {
                self.move_user_input_selection(-1);
                self.reveal_focused_user_input_region();
                Some(None)
            }
            KeyCode::Down if key.modifiers.is_empty() => {
                self.move_user_input_selection(1);
                self.reveal_focused_user_input_region();
                Some(None)
            }
            KeyCode::PageUp if key.modifiers.is_empty() => {
                self.update_user_input_scroll(|scroll| scroll.saturating_sub(8));
                Some(None)
            }
            KeyCode::PageDown if key.modifiers.is_empty() => {
                self.update_user_input_scroll(|scroll| scroll.saturating_add(8));
                Some(None)
            }
            KeyCode::Home if key.modifiers.is_empty() => {
                self.update_user_input_scroll(|_| 0);
                Some(None)
            }
            KeyCode::End if key.modifiers.is_empty() => {
                let max_scroll = self
                    .composer
                    .pending_user_input
                    .as_ref()
                    .map_or(0, |form| form.scroll_extent.get());
                self.update_user_input_scroll(|_| max_scroll);
                Some(None)
            }
            KeyCode::Left | KeyCode::Right if key.modifiers.is_empty() => {
                self.toggle_user_input_selection();
                Some(None)
            }
            KeyCode::Char(' ') if key.modifiers.is_empty() => {
                if !self.toggle_user_input_selection() {
                    self.edit_user_input_text(Some(' '));
                }
                Some(None)
            }
            KeyCode::Backspace if key.modifiers.is_empty() => {
                self.edit_user_input_text(None);
                Some(None)
            }
            KeyCode::Char(character)
                if key.modifiers.is_empty() || key.modifiers == KeyModifiers::SHIFT =>
            {
                self.edit_user_input_text(Some(character));
                Some(None)
            }
            _ => Some(None),
        }
    }

    fn focused_user_input_is_multiline(&self) -> bool {
        self.composer
            .pending_user_input
            .as_ref()
            .and_then(|form| form.request.questions.get(form.focused_question))
            .is_some_and(|question| {
                matches!(
                    question.field,
                    sigil_kernel::UserInputFieldKindV1::Text {
                        multiline: true,
                        ..
                    }
                )
            })
    }

    fn update_user_input_scroll(&mut self, update: impl FnOnce(usize) -> usize) {
        if let Some(form) = self.composer.pending_user_input.as_mut() {
            let max_scroll = form.scroll_extent.get();
            let current = form.scroll.min(max_scroll);
            form.scroll = update(current).min(max_scroll);
        }
    }

    fn reveal_focused_user_input_region(&mut self) {
        let Some(form) = self.composer.pending_user_input.as_mut() else {
            return;
        };
        let extent = form.scroll_extent.get();
        if form.focus_actions {
            form.scroll = extent;
            return;
        }
        let denominator = form.request.questions.len().saturating_sub(1);
        form.scroll = if denominator == 0 {
            form.scroll.min(extent)
        } else {
            extent.saturating_mul(form.focused_question) / denominator
        };
    }

    fn move_user_input_selection(&mut self, direction: isize) {
        let Some(form) = self.composer.pending_user_input.as_mut() else {
            return;
        };
        let Some(question) = form.request.questions.get(form.focused_question) else {
            return;
        };
        match (&question.field, form.drafts.get_mut(form.focused_question)) {
            (
                sigil_kernel::UserInputFieldKindV1::SingleSelect {
                    options,
                    allow_other,
                },
                Some(UserInputDraftValue::SingleSelect { selected, .. }),
            ) => {
                let len = options.len() + usize::from(*allow_other);
                if len > 0 {
                    let current =
                        selected.map_or(if direction < 0 { 0 } else { -1 }, |value| value as isize);
                    *selected = Some(((current + direction).rem_euclid(len as isize)) as usize);
                }
            }
            (
                sigil_kernel::UserInputFieldKindV1::MultiSelect { options, .. },
                Some(UserInputDraftValue::MultiSelect { cursor, .. }),
            ) => {
                if !options.is_empty() {
                    *cursor = ((*cursor as isize + direction).rem_euclid(options.len() as isize))
                        as usize;
                }
            }
            _ => {
                let len = form.request.questions.len();
                if len > 0 {
                    form.focused_question = ((form.focused_question as isize + direction)
                        .rem_euclid(len as isize))
                        as usize;
                }
            }
        }
    }

    fn toggle_user_input_selection(&mut self) -> bool {
        let Some(form) = self.composer.pending_user_input.as_mut() else {
            return false;
        };
        let Some(question) = form.request.questions.get(form.focused_question) else {
            return false;
        };
        match (&question.field, form.drafts.get_mut(form.focused_question)) {
            (_, Some(UserInputDraftValue::Boolean(value))) => {
                *value = match value {
                    None => Some(true),
                    Some(true) => Some(false),
                    Some(false) => None,
                };
                true
            }
            (
                sigil_kernel::UserInputFieldKindV1::MultiSelect {
                    options,
                    max_selected,
                },
                Some(UserInputDraftValue::MultiSelect { cursor, selected }),
            ) => {
                let Some(option) = options.get(*cursor) else {
                    return true;
                };
                if let Some(index) = selected.iter().position(|value| value == &option.id) {
                    selected.remove(index);
                } else if selected.len() < *max_selected as usize {
                    selected.push(option.id.clone());
                }
                true
            }
            _ => false,
        }
    }

    fn edit_user_input_text(&mut self, character: Option<char>) {
        let Some(form) = self.composer.pending_user_input.as_mut() else {
            return;
        };
        let Some(draft) = form.drafts.get_mut(form.focused_question) else {
            return;
        };
        let target = match draft {
            UserInputDraftValue::Text(value)
            | UserInputDraftValue::Number(value)
            | UserInputDraftValue::Integer(value) => Some(value),
            UserInputDraftValue::SingleSelect { other, .. } => Some(other),
            _ => None,
        };
        let Some(target) = target else {
            return;
        };
        match character {
            Some(character) => target.push(character),
            None => {
                target.pop();
            }
        }
    }

    fn select_adjacent_user_input_action(&mut self, direction: isize) {
        let Some(form) = self.composer.pending_user_input.as_mut() else {
            return;
        };
        let available = UserInputFormAction::ORDER
            .into_iter()
            .filter(|action| user_input_action_available(form, *action))
            .collect::<Vec<_>>();
        if available.is_empty() {
            return;
        }
        let index = available
            .iter()
            .position(|candidate| *candidate == form.selected_action)
            .unwrap_or(0) as isize;
        let next = (index + direction).rem_euclid(available.len() as isize) as usize;
        form.selected_action = available[next];
    }

    fn submit_user_input_action(&mut self) -> Option<AppAction> {
        let form = self.composer.pending_user_input.as_ref()?;
        let (command_id, decision) = match form.selected_action {
            UserInputFormAction::Resume => {
                let command = form.recovery_command.as_ref()?;
                (
                    Some(command.command_id.as_str().to_owned()),
                    command.decision.clone(),
                )
            }
            UserInputFormAction::Submit => {
                let mut answers = Vec::with_capacity(form.request.questions.len());
                for (question, draft) in form.request.questions.iter().zip(&form.drafts) {
                    match user_input_answer(question, draft) {
                        Ok(Some(answer)) => answers.push(answer),
                        Ok(None) => {}
                        Err(error) => {
                            self.last_notice = Some(error);
                            return None;
                        }
                    }
                }
                (
                    None,
                    sigil_kernel::UserInputDecisionV1::Submitted { answers },
                )
            }
            UserInputFormAction::Decline => (None, sigil_kernel::UserInputDecisionV1::Declined),
            UserInputFormAction::CancelRun => {
                (None, sigil_kernel::UserInputDecisionV1::RunCancelled)
            }
        };
        self.last_notice = Some(if command_id.is_some() {
            "resuming accepted user input".to_owned()
        } else {
            "submitting user input decision".to_owned()
        });
        Some(AppAction::SubmitUserInputDecision {
            command_id,
            request_id: form.request.identity.request_id.as_str().to_owned(),
            generation: form.request.identity.generation,
            expected_request_hash: form.request.request_hash.clone(),
            decision,
        })
    }

    pub(crate) fn set_pending_user_input(
        &mut self,
        request: sigil_kernel::PublicUserInputRequestV1,
    ) {
        self.set_pending_user_input_with_recovery(request, None);
    }

    pub(crate) fn set_pending_user_input_recovery(
        &mut self,
        request: sigil_kernel::PublicUserInputRequestV1,
        command: sigil_kernel::UserInputDecisionCommandV1,
    ) {
        self.set_pending_user_input_with_recovery(request, Some(command));
    }

    fn set_pending_user_input_with_recovery(
        &mut self,
        request: sigil_kernel::PublicUserInputRequestV1,
        recovery_command: Option<sigil_kernel::UserInputDecisionCommandV1>,
    ) {
        let drafts = request
            .questions
            .iter()
            .map(|question| match &question.field {
                sigil_kernel::UserInputFieldKindV1::Text { .. } => {
                    UserInputDraftValue::Text(String::new())
                }
                sigil_kernel::UserInputFieldKindV1::Number => {
                    UserInputDraftValue::Number(String::new())
                }
                sigil_kernel::UserInputFieldKindV1::Integer => {
                    UserInputDraftValue::Integer(String::new())
                }
                sigil_kernel::UserInputFieldKindV1::Boolean => UserInputDraftValue::Boolean(None),
                sigil_kernel::UserInputFieldKindV1::SingleSelect { .. } => {
                    UserInputDraftValue::SingleSelect {
                        selected: None,
                        other: String::new(),
                    }
                }
                sigil_kernel::UserInputFieldKindV1::MultiSelect { .. } => {
                    UserInputDraftValue::MultiSelect {
                        cursor: 0,
                        selected: Vec::new(),
                    }
                }
            })
            .collect();
        let selected_action = if recovery_command.is_some() {
            UserInputFormAction::Resume
        } else {
            UserInputFormAction::ORDER
                .into_iter()
                .find(|action| {
                    user_input_action_available_parts(&request, recovery_command.as_ref(), *action)
                })
                .unwrap_or(UserInputFormAction::Submit)
        };
        self.composer.pending_user_input = Some(PendingUserInputForm {
            request,
            recovery_command,
            open: true,
            focused_question: 0,
            focus_actions: selected_action == UserInputFormAction::Resume,
            selected_action,
            drafts,
            scroll: 0,
            scroll_extent: Default::default(),
        });
    }

    pub(crate) fn pending_user_input(&self) -> Option<&PendingUserInputForm> {
        self.composer.pending_user_input.as_ref()
    }

    pub(in crate::app) fn clear_pending_user_input(&mut self) {
        self.composer.pending_user_input = None;
    }
}

fn user_input_action_available(form: &PendingUserInputForm, action: UserInputFormAction) -> bool {
    user_input_action_available_parts(&form.request, form.recovery_command.as_ref(), action)
}

fn user_input_action_available_parts(
    request: &sigil_kernel::PublicUserInputRequestV1,
    recovery_command: Option<&sigil_kernel::UserInputDecisionCommandV1>,
    action: UserInputFormAction,
) -> bool {
    if action == UserInputFormAction::Resume {
        return recovery_command.is_some();
    }
    if recovery_command.is_some() {
        return false;
    }
    let expected = match action {
        UserInputFormAction::Resume => unreachable!("resume handled above"),
        UserInputFormAction::Submit => sigil_kernel::UserInputActionV1::Submit,
        UserInputFormAction::Decline => sigil_kernel::UserInputActionV1::Decline,
        UserInputFormAction::CancelRun => sigil_kernel::UserInputActionV1::CancelRun,
    };
    request.allowed_actions.contains(&expected)
}

fn user_input_answer(
    question: &sigil_kernel::UserInputQuestionV1,
    draft: &UserInputDraftValue,
) -> Result<Option<sigil_kernel::UserInputAnswerV1>, String> {
    let value = match (&question.field, draft) {
        (
            sigil_kernel::UserInputFieldKindV1::Text { max_chars, .. },
            UserInputDraftValue::Text(value),
        ) => {
            if value.is_empty() && !question.required {
                return Ok(None);
            }
            if value.is_empty() {
                return Err(format!("{} requires an answer", question.header));
            }
            if value.chars().count() > *max_chars as usize {
                return Err(format!("{} exceeds its character limit", question.header));
            }
            sigil_kernel::UserInputAnswerValueV1::Text {
                value: value.clone(),
            }
        }
        (_, UserInputDraftValue::Number(value)) => {
            if value.is_empty() && !question.required {
                return Ok(None);
            }
            let parsed = value
                .parse::<f64>()
                .map_err(|_| format!("{} must be a number", question.header))?;
            if !parsed.is_finite() {
                return Err(format!("{} must be a finite number", question.header));
            }
            sigil_kernel::UserInputAnswerValueV1::Number {
                value: value.clone(),
            }
        }
        (_, UserInputDraftValue::Integer(value)) => {
            if value.is_empty() && !question.required {
                return Ok(None);
            }
            sigil_kernel::UserInputAnswerValueV1::Integer {
                value: value
                    .parse()
                    .map_err(|_| format!("{} must be an integer", question.header))?,
            }
        }
        (_, UserInputDraftValue::Boolean(value)) => {
            let Some(value) = value else {
                if question.required {
                    return Err(format!("{} requires an answer", question.header));
                }
                return Ok(None);
            };
            sigil_kernel::UserInputAnswerValueV1::Boolean { value: *value }
        }
        (
            sigil_kernel::UserInputFieldKindV1::SingleSelect {
                options,
                allow_other,
            },
            UserInputDraftValue::SingleSelect { selected, other },
        ) => {
            let Some(selected) = selected else {
                if question.required {
                    return Err(format!("{} requires an answer", question.header));
                }
                return Ok(None);
            };
            if let Some(option) = options.get(*selected) {
                sigil_kernel::UserInputAnswerValueV1::SingleSelect {
                    option_id: Some(option.id.clone()),
                    other: None,
                }
            } else if *allow_other {
                if other.is_empty() {
                    return Err(format!("{} requires an Other value", question.header));
                }
                sigil_kernel::UserInputAnswerValueV1::SingleSelect {
                    option_id: None,
                    other: Some(other.clone()),
                }
            } else {
                return Err(format!("{} requires a valid selection", question.header));
            }
        }
        (_, UserInputDraftValue::MultiSelect { selected, .. }) => {
            if selected.is_empty() && !question.required {
                return Ok(None);
            }
            if selected.is_empty() {
                return Err(format!(
                    "{} requires at least one selection",
                    question.header
                ));
            }
            sigil_kernel::UserInputAnswerValueV1::MultiSelect {
                option_ids: selected.clone(),
            }
        }
        _ => return Err(format!("{} has an invalid answer type", question.header)),
    };
    Ok(Some(sigil_kernel::UserInputAnswerV1 {
        question_id: question.id.clone(),
        value,
    }))
}

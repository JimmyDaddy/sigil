use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::{
    AppAction, AppState, PendingMcpElicitation, PendingUserInputForm, UserInputDraftValue,
    UserInputFormAction, UserInputFormSource, UserInputFormViewModel,
};

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
        if key.modifiers == KeyModifiers::CONTROL
            && matches!(key.code, KeyCode::Char('n') | KeyCode::Char('p'))
            && self.composer.pending_user_input_queue.len() > 1
        {
            self.cycle_pending_user_input(if key.code == KeyCode::Char('n') {
                1
            } else {
                -1
            });
            return Some(None);
        }
        if !open {
            if key.code == KeyCode::BackTab && key.modifiers == KeyModifiers::SHIFT {
                if let Some(form) = self.composer.pending_user_input.as_mut() {
                    form.open = true;
                    if form.is_plan_revision_editor() {
                        form.focus_actions = false;
                        self.active_pane = super::PaneFocus::Composer;
                    }
                }
                self.last_notice = Some("input form reopened".to_owned());
                return Some(None);
            }
            return None;
        }
        if key.code == KeyCode::Esc && key.modifiers.is_empty() {
            let revision = self
                .composer
                .pending_user_input
                .as_ref()
                .is_some_and(PendingUserInputForm::is_plan_revision_editor);
            if let Some(form) = self.composer.pending_user_input.as_mut() {
                form.open = false;
            }
            self.last_notice = Some(if revision {
                "revision draft kept; Shift-Tab returns to editing".to_owned()
            } else {
                "input form closed; Shift-Tab reopens it".to_owned()
            });
            return Some(None);
        }
        if self
            .composer
            .pending_user_input
            .as_ref()
            .is_some_and(PendingUserInputForm::is_plan_revision_editor)
        {
            return Some(self.handle_plan_revision_editor_key(key));
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

    fn handle_plan_revision_editor_key(&mut self, key: KeyEvent) -> Option<AppAction> {
        let form = self.composer.pending_user_input.as_mut()?;
        if form.plan_revision_editor.submitting {
            return None;
        }
        form.focus_actions = false;
        form.focused_question = 0;
        if key.code == KeyCode::Enter && key.modifiers.is_empty() {
            form.selected_action = UserInputFormAction::Submit;
            return self.submit_user_input_action();
        }
        if let Some(UserInputDraftValue::Text(value)) = form.drafts.first_mut() {
            form.plan_revision_editor.handle_key(value, key);
        }
        None
    }

    pub(super) fn handle_user_input_form_paste_text(&mut self, text: &str) -> bool {
        let Some(form) = self
            .composer
            .pending_user_input
            .as_mut()
            .filter(|form| form.open && form.is_plan_revision_editor())
        else {
            return false;
        };
        if let Some(UserInputDraftValue::Text(value)) = form.drafts.first_mut() {
            form.plan_revision_editor.insert(value, text);
        }
        true
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
                    if form.focused_question + 1 < form.view.questions.len() {
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
            KeyCode::Enter if key.modifiers.is_empty() => {
                if let Some(form) = self.composer.pending_user_input.as_mut() {
                    if form.focused_question + 1 < form.view.questions.len() {
                        form.focused_question += 1;
                    } else {
                        form.focus_actions = true;
                    }
                }
                self.reveal_focused_user_input_region();
                Some(None)
            }
            KeyCode::Enter if key.modifiers == KeyModifiers::CONTROL => {
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
        // The provider-neutral question contract no longer carries a field-level
        // multiline switch. Ordinary text questions are single-line form fields;
        // the dedicated plan-revision editor owns multiline editing explicitly.
        false
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
        let denominator = form.view.questions.len().saturating_sub(1);
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
        let Some(question) = form.view.questions.get(form.focused_question) else {
            return;
        };
        let selection_was_confirmed = matches!(
            form.drafts.get(form.focused_question),
            Some(UserInputDraftValue::SingleSelect {
                selected_by_user: true,
                ..
            })
        );
        if !question.options.is_empty() && !question.multiple && selection_was_confirmed {
            let len = form.view.questions.len();
            if len > 0 {
                form.focused_question = ((form.focused_question as isize + direction)
                    .rem_euclid(len as isize)) as usize;
            }
            return;
        }
        match (
            question.options.is_empty(),
            question.multiple,
            form.drafts.get_mut(form.focused_question),
        ) {
            (false, false, Some(UserInputDraftValue::SingleSelect { selected, .. })) => {
                let len = question.options.len() + 1;
                if len > 0 {
                    let current =
                        selected.map_or(if direction < 0 { 0 } else { -1 }, |value| value as isize);
                    *selected = Some(((current + direction).rem_euclid(len as isize)) as usize);
                }
            }
            (false, true, Some(UserInputDraftValue::MultiSelect { cursor, .. })) => {
                if !question.options.is_empty() {
                    *cursor = ((*cursor as isize + direction)
                        .rem_euclid(question.options.len() as isize))
                        as usize;
                }
            }
            _ => {
                let len = form.view.questions.len();
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
        let Some(question) = form.view.questions.get(form.focused_question) else {
            return false;
        };
        match (
            question.options.is_empty(),
            question.multiple,
            form.drafts.get_mut(form.focused_question),
        ) {
            (
                false,
                false,
                Some(UserInputDraftValue::SingleSelect {
                    selected,
                    selected_by_user,
                    ..
                }),
            ) => {
                let next = selected
                    .map(|current| (current + 1) % question.options.len())
                    .unwrap_or(0);
                *selected = Some(next);
                *selected_by_user = true;
                true
            }
            (
                false,
                true,
                Some(UserInputDraftValue::MultiSelect {
                    cursor, selected, ..
                }),
            ) => {
                let Some(option) = question.options.get(*cursor) else {
                    return true;
                };
                if let Some(index) = selected.iter().position(|value| value == &option.id) {
                    selected.remove(index);
                } else {
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
            UserInputDraftValue::Text(value) => Some(value),
            UserInputDraftValue::SingleSelect { other, .. } => Some(other),
            UserInputDraftValue::MultiSelect { other, .. } => Some(other),
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
        let revision = form.is_plan_revision_editor();
        if revision && form.plan_revision_editor.submitting {
            return None;
        }
        if !user_input_action_available(form, form.selected_action) {
            return None;
        }
        let (command_id, decision) = match form.selected_action {
            UserInputFormAction::Resume => {
                let command = form.recovery_command.as_ref()?;
                (
                    Some(command.command_id.as_str().to_owned()),
                    command.decision.clone(),
                )
            }
            UserInputFormAction::Submit => {
                let mut answers = Vec::with_capacity(form.view.questions.len());
                for (question, draft) in form.view.questions.iter().zip(&form.drafts) {
                    match user_input_answer(question, draft) {
                        Ok(Some(answer)) => answers.push(answer),
                        Ok(None) => {}
                        Err(error) => {
                            let message = match error {
                                UserInputAnswerError::Required => {
                                    if revision {
                                        "Revision request requires an answer".to_owned()
                                    } else {
                                        format!("{} requires an answer", question.question)
                                    }
                                }
                                UserInputAnswerError::InvalidType => {
                                    format!("{} has an invalid answer type", question.question)
                                }
                            };
                            self.last_notice = Some(message.clone());
                            if revision
                                && let Some(form) = self.composer.pending_user_input.as_mut()
                            {
                                form.plan_revision_editor.error = Some(message);
                            }
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
        if matches!(form.source, UserInputFormSource::Mcp { .. }) {
            return self.finish_mcp_user_input(decision);
        }
        let request = form
            .request
            .as_ref()
            .expect("durable input form must retain its authoritative request");
        let action = if let Some(original_command_id) = command_id.as_ref() {
            AppAction::ResumeCommittedUserInput {
                original_command_id: original_command_id.clone(),
                request_id: request.identity.request_id.as_str().to_owned(),
                generation: request.identity.generation,
                expected_request_hash: request.request_hash.clone(),
            }
        } else {
            AppAction::SubmitUserInputDecision {
                command_id: None,
                request_id: request.identity.request_id.as_str().to_owned(),
                generation: request.identity.generation,
                expected_request_hash: request.request_hash.clone(),
                decision,
            }
        };
        if revision && let Some(form) = self.composer.pending_user_input.as_mut() {
            form.plan_revision_editor.submitting = true;
            form.plan_revision_editor.error = None;
        }
        self.last_notice = Some(if revision {
            "submitting revision request".to_owned()
        } else if command_id.is_some() {
            "resuming accepted user input".to_owned()
        } else {
            "submitting user input decision".to_owned()
        });
        Some(action)
    }

    pub(crate) fn fail_pending_user_input_submission(
        &mut self,
        request_id: &str,
        generation: u32,
        expected_request_hash: &str,
        message: String,
    ) -> bool {
        let mut matched = false;
        for form in self
            .composer
            .pending_user_input
            .iter_mut()
            .chain(self.composer.pending_user_input_queue.iter_mut())
        {
            if form.is_plan_revision_editor()
                && form.plan_revision_editor.submitting
                && form.request.as_ref().is_some_and(|request| {
                    request.identity.request_id.as_str() == request_id
                        && request.identity.generation == generation
                        && request.request_hash == expected_request_hash
                })
            {
                form.plan_revision_editor.submitting = false;
                form.plan_revision_editor.error = Some(message.clone());
                form.open = true;
                form.focus_actions = false;
                matched = true;
            }
        }
        matched
    }

    pub(super) fn restore_pending_user_input_presentation(
        &mut self,
        previous: Option<PendingUserInputForm>,
    ) {
        let Some(previous) = previous.filter(PendingUserInputForm::is_plan_revision_editor) else {
            return;
        };
        for next in self
            .composer
            .pending_user_input
            .iter_mut()
            .chain(self.composer.pending_user_input_queue.iter_mut())
        {
            preserve_plan_revision_draft(next, &previous);
        }
    }

    pub(crate) fn set_pending_user_input(
        &mut self,
        request: sigil_kernel::PublicUserInputRequestV1,
    ) {
        self.set_pending_user_input_with_recovery(request, None);
    }

    #[cfg(test)]
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
        if self.user_input_attention_is_submitted(&request.identity, &request.request_hash) {
            return;
        }
        let view = UserInputFormViewModel::from(&request);
        let drafts = empty_user_input_drafts(&view.questions);
        let selected_action = if recovery_command.is_some() {
            UserInputFormAction::Resume
        } else {
            UserInputFormAction::ORDER
                .into_iter()
                .find(|action| {
                    user_input_action_available_parts(&view, recovery_command.as_ref(), *action)
                })
                .unwrap_or(UserInputFormAction::Submit)
        };
        let mut form = PendingUserInputForm {
            view,
            request: Some(request),
            source: UserInputFormSource::DurableAgent,
            recovery_command,
            queue_position: 1,
            queue_length: 1,
            open: true,
            focused_question: 0,
            focus_actions: selected_action == UserInputFormAction::Resume,
            selected_action,
            drafts,
            plan_revision_editor: Default::default(),
            scroll: 0,
            scroll_extent: Default::default(),
        };
        if let Some(previous) = self
            .composer
            .pending_user_input
            .iter()
            .chain(self.composer.pending_user_input_queue.iter())
            .find(|previous| same_plan_revision_request(&form, previous))
        {
            preserve_plan_revision_draft(&mut form, previous);
        }
        if form.open && form.is_plan_revision_editor() {
            self.active_pane = super::PaneFocus::Composer;
        }
        self.upsert_pending_user_input(form);
    }

    pub(in crate::app) fn set_pending_mcp_user_input(
        &mut self,
        view: UserInputFormViewModel,
        drafts: Vec<UserInputDraftValue>,
        server_name: String,
        response_tx: crate::runner::McpElicitationResponseTx,
    ) {
        self.pending_mcp_elicitation = Some(PendingMcpElicitation {
            response_tx: Some(response_tx),
        });
        self.composer.pending_user_input = Some(PendingUserInputForm {
            view,
            request: None,
            source: UserInputFormSource::Mcp { server_name },
            recovery_command: None,
            queue_position: 1,
            queue_length: 1,
            open: true,
            focused_question: 0,
            focus_actions: false,
            selected_action: UserInputFormAction::Submit,
            drafts,
            plan_revision_editor: Default::default(),
            scroll: 0,
            scroll_extent: Default::default(),
        });
    }

    fn finish_mcp_user_input(
        &mut self,
        decision: sigil_kernel::UserInputDecisionV1,
    ) -> Option<AppAction> {
        let server_name = self
            .composer
            .pending_user_input
            .as_ref()
            .and_then(|form| match &form.source {
                UserInputFormSource::Mcp { server_name } => Some(server_name.clone()),
                UserInputFormSource::DurableAgent => None,
            })?;
        let response = match decision {
            sigil_kernel::UserInputDecisionV1::Submitted { answers } => {
                let content = mcp_content_from_answers(answers).ok()?;
                sigil_runtime::McpElicitationResponse::accept(content)
            }
            sigil_kernel::UserInputDecisionV1::Declined => {
                sigil_runtime::McpElicitationResponse::decline()
            }
            sigil_kernel::UserInputDecisionV1::RunCancelled => {
                sigil_runtime::McpElicitationResponse::cancel()
            }
        };
        if let Some(mut pending) = self.pending_mcp_elicitation.take() {
            pending.send(response.clone());
        }
        self.composer.pending_user_input = self
            .composer
            .pending_user_input_queue
            .get(self.composer.pending_user_input_queue_index)
            .cloned();
        self.active_pane = super::PaneFocus::Composer;
        let notice = match response.action {
            sigil_runtime::McpElicitationAction::Accept => {
                format!("submitted MCP input to {server_name}")
            }
            sigil_runtime::McpElicitationAction::Decline => {
                format!("declined MCP input request from {server_name}")
            }
            sigil_runtime::McpElicitationAction::Cancel => {
                format!("cancelled MCP input request from {server_name}")
            }
        };
        self.last_notice = Some(notice.clone());
        self.push_event("mcp:elicitation", notice);
        None
    }

    pub(crate) fn pending_user_input(&self) -> Option<&PendingUserInputForm> {
        self.composer.pending_user_input.as_ref()
    }

    #[cfg(test)]
    pub(in crate::app) fn clear_pending_user_input(&mut self) {
        self.composer.pending_user_input = None;
        self.composer.pending_user_input_queue.clear();
        self.composer.pending_user_input_queue_index = 0;
        self.pending_mcp_elicitation = None;
    }

    pub(in crate::app) fn set_pending_user_inputs(
        &mut self,
        requests: Vec<sigil_kernel::PublicUserInputRequestV1>,
        recovery_command: Option<sigil_kernel::UserInputDecisionCommandV1>,
    ) {
        let previous_active = self.composer.pending_user_input.take();
        let previous_queue = std::mem::take(&mut self.composer.pending_user_input_queue);
        let previous_index = self.composer.pending_user_input_queue_index;
        self.composer.pending_user_input_queue_index = 0;
        for request in requests {
            let recovery = recovery_command
                .as_ref()
                .filter(|command| {
                    command.identity == request.identity
                        && command.request_hash == request.request_hash
                })
                .cloned()
                .or_else(|| {
                    previous_active
                        .iter()
                        .chain(previous_queue.iter())
                        .find(|previous| {
                            previous.request.as_ref().is_some_and(|old| {
                                old.identity == request.identity
                                    && old.request_hash == request.request_hash
                                    && old.source == request.source
                            })
                        })
                        .and_then(|previous| previous.recovery_command.clone())
                });
            self.set_pending_user_input_with_recovery(request, recovery);
        }
        for next in &mut self.composer.pending_user_input_queue {
            if let Some(previous) = previous_active
                .iter()
                .chain(previous_queue.iter())
                .find(|previous| same_user_input_presentation(next, previous))
            {
                preserve_user_input_presentation(next, previous);
            }
        }
        self.select_pending_user_input_after_reconcile(previous_active, previous_index);
    }

    pub(super) fn user_input_attention_is_submitted(
        &self,
        identity: &sigil_kernel::UserInputIdentityV1,
        request_hash: &str,
    ) -> bool {
        self.session_auxiliary
            .submitted_user_inputs
            .contains(&(identity.clone(), request_hash.to_owned()))
    }

    pub(super) fn dismiss_submitted_user_input(
        &mut self,
        request: &sigil_kernel::PublicUserInputRequestV1,
    ) {
        self.request_session_auxiliary_refresh();
        self.session_auxiliary
            .submitted_user_inputs
            .insert((request.identity.clone(), request.request_hash.clone()));
        self.retain_pending_user_inputs(|form| {
            form.request.as_ref().is_none_or(|pending| {
                pending.identity != request.identity || pending.request_hash != request.request_hash
            })
        });
    }

    pub(super) fn allow_recovered_user_input_attention(
        &mut self,
        command: &sigil_kernel::UserInputDecisionCommandV1,
    ) {
        self.session_auxiliary
            .submitted_user_inputs
            .remove(&(command.identity.clone(), command.request_hash.clone()));
    }

    pub(super) fn public_user_input_attention_requests(
        &self,
    ) -> anyhow::Result<Vec<sigil_kernel::PublicUserInputRequestV1>> {
        let mut requests = sigil_runtime::conversation_display::public_user_inputs_from_entries(
            &self.session_browser.current_entries,
        )?;
        let routes = sigil_kernel::AgentUserInputRouteProjectionV1::from_session_entries(
            &self.session_browser.current_entries,
        )?;
        // Only an explicit worker recovery may expose a Registered route again. Preserve
        // that exact presentation during later projection refreshes without inventing one.
        for route in routes
            .unresolved()
            .filter(|route| route.status == sigil_kernel::AgentRouteStatus::Registered)
        {
            if self
                .composer
                .pending_user_input
                .iter()
                .chain(self.composer.pending_user_input_queue.iter())
                .any(|form| {
                    form.recovery_command.as_ref().is_some_and(|command| {
                        command.identity == route.request.identity
                            && command.request_hash == route.request.request_hash
                    })
                })
                && !requests.iter().any(|request| {
                    request.identity == route.request.identity
                        && request.request_hash == route.request.request_hash
                })
            {
                requests.push(route.request.clone());
            }
        }
        Ok(requests)
    }

    /// Reconciles attention, not run ownership. Pending questions remain visible while busy;
    /// exact inputs handed back to execution stay hidden until the worker offers recovery.
    pub(super) fn reconcile_pending_user_input_attention(&mut self) {
        let Ok(requests) = self.public_user_input_attention_requests() else {
            return;
        };
        let mut pending = requests
            .into_iter()
            .map(|request| (request.identity, request.request_hash))
            .collect::<std::collections::BTreeSet<_>>();
        let mut unresolved = pending.clone();
        if let Ok(routes) = sigil_kernel::AgentUserInputRouteProjectionV1::from_session_entries(
            &self.session_browser.current_entries,
        ) {
            unresolved.extend(routes.unresolved().map(|route| {
                (
                    route.request.identity.clone(),
                    route.request.request_hash.clone(),
                )
            }));
        }
        // Retain suppression through Registered, but never retain completed input history.
        self.session_auxiliary
            .submitted_user_inputs
            .retain(|key| unresolved.contains(key));
        pending.retain(|key| !self.session_auxiliary.submitted_user_inputs.contains(key));
        self.retain_pending_user_inputs(|form| {
            form.request.as_ref().is_none_or(|request| {
                pending.contains(&(request.identity.clone(), request.request_hash.clone()))
            })
        });
    }

    pub(super) fn user_input_has_advanced_past_attention(
        &self,
        identity: &sigil_kernel::UserInputIdentityV1,
        request_hash: &str,
    ) -> bool {
        let entries = &self.session_browser.current_entries;
        if sigil_kernel::UserInputProjectionV1::from_session_entries(entries)
            .ok()
            .and_then(|projection| projection.request(identity).cloned())
            .is_some_and(|request| {
                request.requested.request_hash == request_hash
                    && request.status == sigil_kernel::UserInputStatusV1::Resolved
            })
        {
            return true;
        }
        sigil_kernel::AgentUserInputRouteProjectionV1::from_session_entries(entries)
            .ok()
            .and_then(|projection| {
                projection
                    .route_for_request(identity, request_hash)
                    .cloned()
            })
            .is_some_and(|route| route.status != sigil_kernel::AgentRouteStatus::Requested)
    }

    pub(super) fn user_input_attention_is_resolved(
        &self,
        identity: &sigil_kernel::UserInputIdentityV1,
        request_hash: &str,
    ) -> bool {
        let entries = &self.session_browser.current_entries;
        let local = sigil_kernel::UserInputProjectionV1::from_session_entries(entries)
            .ok()
            .and_then(|projection| projection.request(identity).cloned());
        if local.is_some_and(|request| {
            request.requested.request_hash == request_hash
                && request.status == sigil_kernel::UserInputStatusV1::Resolved
        }) {
            return true;
        }
        sigil_kernel::AgentUserInputRouteProjectionV1::from_session_entries(entries)
            .ok()
            .and_then(|projection| {
                projection
                    .route_for_request(identity, request_hash)
                    .cloned()
            })
            .is_some_and(|route| {
                !matches!(
                    route.status,
                    sigil_kernel::AgentRouteStatus::Requested
                        | sigil_kernel::AgentRouteStatus::Registered
                )
            })
    }

    fn retain_pending_user_inputs(&mut self, keep: impl FnMut(&PendingUserInputForm) -> bool) {
        let previous_active = self.composer.pending_user_input.take();
        let previous_index = self.composer.pending_user_input_queue_index;
        for form in &mut self.composer.pending_user_input_queue {
            if let Some(active) = previous_active
                .as_ref()
                .filter(|active| same_user_input_presentation(form, active))
            {
                *form = active.clone();
            }
        }
        self.composer.pending_user_input_queue.retain(keep);
        self.select_pending_user_input_after_reconcile(previous_active, previous_index);
    }

    fn select_pending_user_input_after_reconcile(
        &mut self,
        previous_active: Option<PendingUserInputForm>,
        previous_index: usize,
    ) {
        let queue = &mut self.composer.pending_user_input_queue;
        let len = queue.len();
        for (index, form) in queue.iter_mut().enumerate() {
            form.queue_position = index + 1;
            form.queue_length = len;
        }
        let selected = previous_active
            .as_ref()
            .and_then(|previous| {
                queue
                    .iter()
                    .position(|next| same_user_input_presentation(next, previous))
            })
            .unwrap_or_else(|| previous_index.min(len.saturating_sub(1)));
        self.composer.pending_user_input_queue_index = selected;
        self.composer.pending_user_input = previous_active
            .filter(|form| matches!(form.source, UserInputFormSource::Mcp { .. }))
            .or_else(|| queue.get(selected).cloned());
    }

    fn upsert_pending_user_input(&mut self, mut form: PendingUserInputForm) {
        let Some(request) = form.request.as_ref() else {
            self.composer.pending_user_input = Some(form);
            return;
        };
        let identity = request.identity.clone();
        let hash = request.request_hash.clone();
        let mut mcp_active = None;
        if let Some(active) = self.composer.pending_user_input.take() {
            if matches!(&active.source, UserInputFormSource::Mcp { .. }) {
                mcp_active = Some(active);
            } else if let Some(index) =
                self.composer
                    .pending_user_input_queue
                    .iter()
                    .position(|queued| {
                        queued.request.as_ref().is_some_and(|queued_request| {
                            queued_request.identity
                                == active
                                    .request
                                    .as_ref()
                                    .expect("durable form request")
                                    .identity
                                && queued_request.request_hash
                                    == active
                                        .request
                                        .as_ref()
                                        .expect("durable form request")
                                        .request_hash
                        })
                    })
            {
                self.composer.pending_user_input_queue[index] = active;
            }
        }
        if let Some(index) = self
            .composer
            .pending_user_input_queue
            .iter()
            .position(|queued| {
                queued.request.as_ref().is_some_and(|queued_request| {
                    queued_request.identity == identity && queued_request.request_hash == hash
                })
            })
        {
            self.composer.pending_user_input_queue[index] = form;
        } else {
            self.composer.pending_user_input_queue.push(form);
            self.composer
                .pending_user_input_queue
                .sort_by(|left, right| {
                    let left = left.request.as_ref().expect("durable queue request");
                    let right = right.request.as_ref().expect("durable queue request");
                    left.requested_at_unix_ms
                        .cmp(&right.requested_at_unix_ms)
                        .then_with(|| left.identity.cmp(&right.identity))
                });
        }
        let len = self.composer.pending_user_input_queue.len();
        for (index, queued) in self
            .composer
            .pending_user_input_queue
            .iter_mut()
            .enumerate()
        {
            queued.queue_position = index + 1;
            queued.queue_length = len;
        }
        self.composer.pending_user_input_queue_index = self
            .composer
            .pending_user_input_queue_index
            .min(len.saturating_sub(1));
        form = self.composer.pending_user_input_queue[self.composer.pending_user_input_queue_index]
            .clone();
        self.composer.pending_user_input = mcp_active.or(Some(form));
    }

    fn cycle_pending_user_input(&mut self, delta: isize) {
        let len = self.composer.pending_user_input_queue.len();
        if len < 2 {
            return;
        }
        if let Some(active) = self.composer.pending_user_input.take() {
            let index = self.composer.pending_user_input_queue_index.min(len - 1);
            self.composer.pending_user_input_queue[index] = active;
        }
        let current = self.composer.pending_user_input_queue_index.min(len - 1);
        self.composer.pending_user_input_queue_index = if delta > 0 {
            (current + 1) % len
        } else {
            (current + len - 1) % len
        };
        self.composer.pending_user_input = Some(
            self.composer.pending_user_input_queue[self.composer.pending_user_input_queue_index]
                .clone(),
        );
        self.last_notice = Some(format!(
            "input request {} of {}",
            self.composer.pending_user_input_queue_index + 1,
            len
        ));
    }
}

fn same_user_input_presentation(
    next: &PendingUserInputForm,
    previous: &PendingUserInputForm,
) -> bool {
    next.view == previous.view
        && next.recovery_command == previous.recovery_command
        && next
            .request
            .as_ref()
            .zip(previous.request.as_ref())
            .is_some_and(|(next, previous)| {
                next.identity == previous.identity
                    && next.request_hash == previous.request_hash
                    && next.source == previous.source
                    && next.status == previous.status
            })
}

fn preserve_user_input_presentation(
    next: &mut PendingUserInputForm,
    previous: &PendingUserInputForm,
) {
    next.open = previous.open;
    next.focused_question = previous.focused_question;
    next.focus_actions = previous.focus_actions;
    next.selected_action = previous.selected_action;
    next.drafts = previous.drafts.clone();
    next.plan_revision_editor = previous.plan_revision_editor.clone();
    next.scroll = previous.scroll;
    next.scroll_extent = previous.scroll_extent.clone();
}

fn same_plan_revision_request(
    next: &PendingUserInputForm,
    previous: &PendingUserInputForm,
) -> bool {
    next.is_plan_revision_editor()
        && previous.is_plan_revision_editor()
        && next.view == previous.view
        && next
            .request
            .as_ref()
            .zip(previous.request.as_ref())
            .is_some_and(|(next, previous)| {
                next.identity == previous.identity
                    && next.request_hash == previous.request_hash
                    && next.source == previous.source
                    && next.status == sigil_kernel::UserInputStatusV1::Requested
                    && previous.status == sigil_kernel::UserInputStatusV1::Requested
            })
}

fn preserve_plan_revision_draft(next: &mut PendingUserInputForm, previous: &PendingUserInputForm) {
    if !same_plan_revision_request(next, previous) {
        return;
    }
    next.open = previous.open;
    next.focused_question = 0;
    next.focus_actions = false;
    next.selected_action = UserInputFormAction::Submit;
    next.drafts = previous.drafts.clone();
    next.plan_revision_editor = previous.plan_revision_editor.clone();
    next.scroll = previous.scroll;
    next.scroll_extent = previous.scroll_extent.clone();
}

fn mcp_content_from_answers(
    answers: Vec<sigil_kernel::UserInputAnswerV1>,
) -> Result<serde_json::Value, String> {
    let mut content = serde_json::Map::new();
    for answer in answers {
        let value = match answer.value {
            sigil_kernel::UserInputAnswerValueV1::Text { value } => {
                serde_json::Value::String(value)
            }
            sigil_kernel::UserInputAnswerValueV1::SingleSelect { option_id, other } => {
                serde_json::Value::String(option_id.or(other).ok_or_else(|| {
                    "MCP single-select answer omitted its selected value".to_owned()
                })?)
            }
            sigil_kernel::UserInputAnswerValueV1::MultiSelect { option_ids, other } => {
                let mut values = option_ids
                    .into_iter()
                    .map(serde_json::Value::String)
                    .collect::<Vec<_>>();
                if let Some(other) = other {
                    values.push(serde_json::Value::String(other));
                }
                serde_json::Value::Array(values)
            }
        };
        content.insert(answer.question_id, value);
    }
    Ok(serde_json::Value::Object(content))
}

fn user_input_action_available(form: &PendingUserInputForm, action: UserInputFormAction) -> bool {
    user_input_action_available_parts(&form.view, form.recovery_command.as_ref(), action)
}

fn user_input_action_available_parts(
    view: &UserInputFormViewModel,
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
    view.allowed_actions.contains(&expected)
}

fn empty_user_input_drafts(
    questions: &[sigil_kernel::UserInputQuestionV1],
) -> Vec<UserInputDraftValue> {
    questions
        .iter()
        .map(|question| {
            if question.options.is_empty() {
                UserInputDraftValue::Text(String::new())
            } else if question.multiple {
                UserInputDraftValue::MultiSelect {
                    cursor: 0,
                    selected: Vec::new(),
                    other: String::new(),
                }
            } else {
                UserInputDraftValue::SingleSelect {
                    selected: None,
                    other: String::new(),
                    selected_by_user: false,
                }
            }
        })
        .collect()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum UserInputAnswerError {
    Required,
    InvalidType,
}

fn user_input_answer(
    question: &sigil_kernel::UserInputQuestionV1,
    draft: &UserInputDraftValue,
) -> Result<Option<sigil_kernel::UserInputAnswerV1>, UserInputAnswerError> {
    let value = if question.options.is_empty() {
        let UserInputDraftValue::Text(value) = draft else {
            return Err(UserInputAnswerError::InvalidType);
        };
        if value.is_empty() {
            if !question.required {
                return Ok(None);
            }
            return Err(UserInputAnswerError::Required);
        }
        sigil_kernel::UserInputAnswerValueV1::Text {
            value: value.clone(),
        }
    } else if question.multiple {
        let UserInputDraftValue::MultiSelect {
            selected, other, ..
        } = draft
        else {
            return Err(UserInputAnswerError::InvalidType);
        };
        if selected.is_empty() && other.is_empty() {
            if !question.required {
                return Ok(None);
            }
            return Err(UserInputAnswerError::Required);
        }
        sigil_kernel::UserInputAnswerValueV1::MultiSelect {
            option_ids: selected.clone(),
            other: (!other.is_empty()).then_some(other.clone()),
        }
    } else {
        let UserInputDraftValue::SingleSelect {
            selected, other, ..
        } = draft
        else {
            return Err(UserInputAnswerError::InvalidType);
        };
        match selected.and_then(|index| question.options.get(index)) {
            Some(option) => sigil_kernel::UserInputAnswerValueV1::SingleSelect {
                option_id: Some(option.id.clone()),
                other: None,
            },
            None if !other.is_empty() => sigil_kernel::UserInputAnswerValueV1::SingleSelect {
                option_id: None,
                other: Some(other.clone()),
            },
            None if !question.required => return Ok(None),
            None => return Err(UserInputAnswerError::Required),
        }
    };
    Ok(Some(sigil_kernel::UserInputAnswerV1 {
        question_id: question.id.clone(),
        value,
    }))
}

#[cfg(test)]
#[path = "tests/user_input_flow_tests.rs"]
mod tests;

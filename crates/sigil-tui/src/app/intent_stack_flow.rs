use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use sigil_kernel::{
    IntentApplicationState, IntentArtifactAvailability, IntentArtifactOwnership,
    IntentAuthorityState, IntentDropRequestV1, IntentOperationExecutionV1, IntentOperationKind,
    IntentOperationPreviewV1, IntentOperationResolution, IntentVersionRef,
    PublicIntentStackStateV1, PublicIntentStackV1, PublicIntentV1,
};

use super::{AppAction, AppState, ModalState, PaneFocus, TimelineRole};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) enum IntentStackModalOperation {
    #[default]
    Loading,
    Idle,
    PreviewingDrop,
    ConfirmingDrop,
    ApplyingDrop,
}

#[derive(Debug, Default)]
pub(super) struct IntentStackModalState {
    pub(super) request_id: u64,
    pub(super) operation: IntentStackModalOperation,
    pub(super) stack_state: Option<PublicIntentStackStateV1>,
    pub(super) selected: usize,
    pub(super) detail_scroll: u16,
    pub(super) expected_intent_ref: Option<IntentVersionRef>,
    pub(super) drop_preview: Option<IntentOperationPreviewV1>,
    pub(super) error: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum IntentStackModalPhase {
    Loading,
    Ready,
    ReadOnly,
    NotCreated,
    PreviewingDrop,
    ConfirmingDrop,
    ApplyingDrop,
    Unavailable,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct IntentStackRowView {
    pub(crate) title: String,
    pub(crate) status: &'static str,
    pub(crate) detail: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct IntentStackModalView {
    pub(crate) phase: IntentStackModalPhase,
    pub(crate) phase_detail: String,
    pub(crate) summary_lines: Vec<String>,
    pub(crate) rows: Vec<IntentStackRowView>,
    pub(crate) selected: usize,
    pub(crate) detail_lines: Vec<String>,
    pub(crate) detail_scroll: u16,
    pub(crate) primary_action_label: String,
    pub(crate) primary_action_enabled: bool,
    pub(crate) can_refresh: bool,
    pub(crate) error: Option<String>,
}

impl AppState {
    pub(super) fn open_intent_stack_modal(&mut self) -> Option<AppAction> {
        if self.approval.has_actionable_pending() {
            self.last_notice =
                Some("finish the pending approval before opening Intent Stack".to_owned());
            return None;
        }
        self.blur_verification_card();
        self.blur_composer_aux_panels();
        let request_id = self.next_background_request_id();
        self.modal_state = Some(ModalState::IntentStack(Box::new(IntentStackModalState {
            request_id,
            operation: IntentStackModalOperation::Loading,
            ..IntentStackModalState::default()
        })));
        self.active_pane = PaneFocus::Activity;
        self.last_notice = Some("loading Intent Stack from durable history".to_owned());
        self.push_event("intent:stack", format!("load requested {request_id}"));
        Some(AppAction::LoadIntentStack { request_id })
    }

    pub(crate) fn intent_stack_modal_open(&self) -> bool {
        matches!(self.modal_state, Some(ModalState::IntentStack(_)))
    }

    pub(crate) fn intent_drop_applying(&self) -> bool {
        matches!(
            self.modal_state.as_ref(),
            Some(ModalState::IntentStack(state))
                if state.operation == IntentStackModalOperation::ApplyingDrop
        )
    }

    pub(super) fn handle_intent_stack_modal_key_event(
        &mut self,
        key: KeyEvent,
    ) -> Option<AppAction> {
        if key.modifiers != KeyModifiers::NONE {
            return None;
        }
        match key.code {
            KeyCode::Esc => {
                if self.intent_drop_applying() {
                    self.last_notice =
                        Some("Intent drop is applying and cannot be dismissed".to_owned());
                    return None;
                }
                self.modal_state = None;
                self.active_pane = PaneFocus::Composer;
                self.last_notice = Some("closed Intent Stack".to_owned());
                None
            }
            KeyCode::Char('r' | 'R') => self.refresh_intent_stack_modal(),
            KeyCode::Up => {
                self.select_adjacent_intent(false);
                None
            }
            KeyCode::Down => {
                self.select_adjacent_intent(true);
                None
            }
            KeyCode::PageUp => {
                self.scroll_intent_stack_detail(true, 8);
                None
            }
            KeyCode::PageDown => {
                self.scroll_intent_stack_detail(false, 8);
                None
            }
            KeyCode::Home => {
                if let Some(state) = self.intent_stack_modal_state_mut() {
                    state.detail_scroll = 0;
                }
                None
            }
            KeyCode::End => {
                if let Some(state) = self.intent_stack_modal_state_mut() {
                    state.detail_scroll = u16::MAX;
                }
                None
            }
            KeyCode::Char('d' | 'D') => self.handle_intent_stack_primary_action(),
            KeyCode::Enter
                if matches!(
                    self.intent_stack_modal_state().map(|state| state.operation),
                    Some(IntentStackModalOperation::ConfirmingDrop)
                ) =>
            {
                self.handle_intent_stack_primary_action()
            }
            _ => None,
        }
    }

    pub(super) fn handle_intent_stack_primary_action(&mut self) -> Option<AppAction> {
        let operation = self.intent_stack_modal_state()?.operation;
        match operation {
            IntentStackModalOperation::Idle => self.request_selected_intent_drop_preview(),
            IntentStackModalOperation::ConfirmingDrop => self.confirm_selected_intent_drop(),
            IntentStackModalOperation::Loading
            | IntentStackModalOperation::PreviewingDrop
            | IntentStackModalOperation::ApplyingDrop => {
                self.last_notice = Some("Intent Stack operation already in progress".to_owned());
                None
            }
        }
    }

    pub(super) fn select_intent_stack_row(&mut self, index: usize) -> bool {
        let row_count = self
            .intent_stack_modal_state()
            .and_then(|state| state.stack_state.as_ref())
            .and_then(available_stack)
            .map_or(0, |stack| stack.intents.len());
        if index >= row_count {
            return false;
        }
        let Some(state) = self.intent_stack_modal_state_mut() else {
            return false;
        };
        if state.operation == IntentStackModalOperation::ApplyingDrop {
            return false;
        }
        state.selected = index;
        state.detail_scroll = 0;
        state.expected_intent_ref = None;
        state.drop_preview = None;
        if matches!(
            state.operation,
            IntentStackModalOperation::PreviewingDrop | IntentStackModalOperation::ConfirmingDrop
        ) {
            state.operation = IntentStackModalOperation::Idle;
        }
        true
    }

    pub(super) fn scroll_intent_stack_detail(&mut self, up: bool, amount: u16) {
        let Some(state) = self.intent_stack_modal_state_mut() else {
            return;
        };
        state.detail_scroll = if up {
            state.detail_scroll.saturating_sub(amount)
        } else {
            state.detail_scroll.saturating_add(amount)
        };
    }

    pub(super) fn apply_intent_stack_loaded(
        &mut self,
        request_id: u64,
        stack_state: PublicIntentStackStateV1,
    ) -> bool {
        let Some(state) = self.intent_stack_modal_state_mut() else {
            return false;
        };
        if state.request_id != request_id || state.operation != IntentStackModalOperation::Loading {
            return false;
        }
        state.selected = available_stack(&stack_state).map_or(0, |stack| {
            state.selected.min(stack.intents.len().saturating_sub(1))
        });
        state.stack_state = Some(stack_state);
        state.operation = IntentStackModalOperation::Idle;
        state.detail_scroll = 0;
        state.error = None;
        self.last_notice = Some("Intent Stack loaded".to_owned());
        true
    }

    pub(super) fn apply_intent_drop_preview(
        &mut self,
        request_id: u64,
        preview: IntentOperationPreviewV1,
    ) -> bool {
        let Some(state) = self.intent_stack_modal_state_mut() else {
            return false;
        };
        let response_matches = state.request_id == request_id
            && state.operation == IntentStackModalOperation::PreviewingDrop
            && state
                .expected_intent_ref
                .as_ref()
                .is_some_and(|intent_ref| preview.target_intents.first() == Some(intent_ref))
            && state
                .stack_state
                .as_ref()
                .and_then(available_stack)
                .is_some_and(|stack| stack.stack_version == preview.stack_version);
        if !response_matches {
            return false;
        }
        let file_count = preview.file_effects.len();
        let conflict_count = preview.conflicts.len();
        state.drop_preview = Some(preview);
        state.operation = IntentStackModalOperation::ConfirmingDrop;
        state.error = None;
        state.detail_scroll = 0;
        self.last_notice = Some(if conflict_count == 0 {
            format!(
                "exact Intent drop preview ready for {file_count} file(s); press Enter to confirm"
            )
        } else {
            format!("Intent drop is blocked by {conflict_count} conflict(s)")
        });
        true
    }

    pub(super) fn apply_intent_drop_completed(
        &mut self,
        request_id: u64,
        execution: &IntentOperationExecutionV1,
        stack_state: PublicIntentStackStateV1,
    ) -> bool {
        let Some(state) = self.intent_stack_modal_state_mut() else {
            return false;
        };
        let response_matches = state.request_id == request_id
            && state.operation == IntentStackModalOperation::ApplyingDrop
            && state.drop_preview.as_ref().is_some_and(|preview| {
                preview.operation_id == execution.preview.operation_id
                    && preview.preview_digest == execution.preview.preview_digest
            });
        if !response_matches {
            return false;
        }
        let resolution = execution.resolution;
        state.stack_state = Some(stack_state);
        state.operation = IntentStackModalOperation::Idle;
        state.expected_intent_ref = None;
        state.drop_preview = None;
        state.detail_scroll = 0;
        state.error = None;
        self.last_notice = Some(match resolution {
            IntentOperationResolution::Committed => {
                "Intent dropped; affected verification is now stale".to_owned()
            }
            IntentOperationResolution::Conflicted => {
                "Intent drop was blocked; review the recorded conflicts".to_owned()
            }
            _ => format!(
                "Intent drop resolved as {}",
                operation_resolution_label(resolution)
            ),
        });
        if resolution == IntentOperationResolution::Committed {
            self.push_timeline(
                TimelineRole::Notice,
                "Intent dropped using the exact reviewed preview. Verification for affected evidence is stale.",
            );
        }
        true
    }

    pub(super) fn apply_intent_stack_operation_failed(
        &mut self,
        request_id: u64,
        error: &str,
    ) -> bool {
        let Some(state) = self.intent_stack_modal_state_mut() else {
            return false;
        };
        if state.request_id != request_id
            || matches!(
                state.operation,
                IntentStackModalOperation::Idle | IntentStackModalOperation::ConfirmingDrop
            )
        {
            return false;
        }
        state.operation = IntentStackModalOperation::Idle;
        state.expected_intent_ref = None;
        state.drop_preview = None;
        state.error = Some(error.to_owned());
        self.last_notice = Some(format!("Intent Stack operation failed: {error}"));
        true
    }

    pub(crate) fn intent_stack_modal_view(&self) -> Option<IntentStackModalView> {
        let state = self.intent_stack_modal_state()?;
        let phase = intent_stack_phase(state);
        let mut summary_lines = Vec::new();
        let mut rows = Vec::new();
        let mut detail_lines = Vec::new();
        if let Some(stack_state) = &state.stack_state {
            match stack_state {
                PublicIntentStackStateV1::Available { stack, .. } => {
                    summary_lines.push(format!(
                        "Version {} · {} intent(s) · authority {}",
                        stack.stack_version.get(),
                        stack.intents.len(),
                        authority_state_label(stack.authority_state)
                    ));
                    summary_lines.push(format!(
                        "{} verified criteria · {} conflict(s)",
                        stack
                            .intents
                            .iter()
                            .map(|intent| intent.system_verified_criterion_count)
                            .sum::<u32>(),
                        stack.conflicts.len()
                    ));
                    rows = stack.intents.iter().map(intent_row_view).collect();
                    if let Some(intent) = stack.intents.get(state.selected) {
                        detail_lines = intent_detail_lines(stack, intent);
                    }
                }
                PublicIntentStackStateV1::NotCreated { safe_message, .. } => {
                    summary_lines.push("No Intent Stack has been created yet.".to_owned());
                    detail_lines.push(safe_message.clone());
                }
            }
        }
        if let Some(preview) = &state.drop_preview {
            detail_lines = drop_preview_lines(preview);
        }
        if detail_lines.is_empty() && state.operation == IntentStackModalOperation::Loading {
            detail_lines
                .push("Replaying accepted plan, lineage, artifacts, and operations…".to_owned());
        }
        let (primary_action_label, primary_action_enabled) =
            intent_stack_primary_action(self.runtime.is_busy, state);
        Some(IntentStackModalView {
            phase,
            phase_detail: intent_stack_phase_detail(phase).to_owned(),
            summary_lines,
            rows,
            selected: state.selected,
            detail_lines,
            detail_scroll: state.detail_scroll,
            primary_action_label,
            primary_action_enabled,
            can_refresh: state.operation != IntentStackModalOperation::ApplyingDrop,
            error: state.error.clone(),
        })
    }

    fn refresh_intent_stack_modal(&mut self) -> Option<AppAction> {
        if self.intent_drop_applying() {
            self.last_notice = Some("Intent drop is already applying".to_owned());
            return None;
        }
        let request_id = self.next_background_request_id();
        let state = self.intent_stack_modal_state_mut()?;
        state.request_id = request_id;
        state.operation = IntentStackModalOperation::Loading;
        state.stack_state = None;
        state.expected_intent_ref = None;
        state.drop_preview = None;
        state.error = None;
        state.detail_scroll = 0;
        self.last_notice = Some("refreshing Intent Stack from durable history".to_owned());
        Some(AppAction::LoadIntentStack { request_id })
    }

    fn request_selected_intent_drop_preview(&mut self) -> Option<AppAction> {
        if self.runtime.is_busy {
            self.last_notice = Some("wait for the active run before dropping an intent".to_owned());
            return None;
        }
        let intent_ref = {
            let state = self.intent_stack_modal_state()?;
            let stack = state.stack_state.as_ref().and_then(available_stack)?;
            let intent = stack.intents.get(state.selected)?;
            if stack.authority_state != IntentAuthorityState::Active
                || !intent
                    .available_actions
                    .contains(&IntentOperationKind::Drop)
            {
                self.last_notice = Some("the selected intent has no exact Drop action".to_owned());
                return None;
            }
            intent.intent_ref.clone()
        };
        let request_id = self.next_background_request_id();
        let state = self.intent_stack_modal_state_mut()?;
        state.request_id = request_id;
        state.operation = IntentStackModalOperation::PreviewingDrop;
        state.expected_intent_ref = Some(intent_ref.clone());
        state.drop_preview = None;
        state.error = None;
        state.detail_scroll = 0;
        self.last_notice = Some("building exact Intent drop preview".to_owned());
        Some(AppAction::PreviewIntentDrop {
            request_id,
            intent_ref,
        })
    }

    fn confirm_selected_intent_drop(&mut self) -> Option<AppAction> {
        if self.runtime.is_busy {
            self.last_notice = Some("wait for the active run before dropping an intent".to_owned());
            return None;
        }
        let preview = self
            .intent_stack_modal_state()?
            .drop_preview
            .as_ref()?
            .clone();
        if !preview.conflicts.is_empty() {
            self.last_notice =
                Some("Intent drop is blocked; resolve the listed conflicts first".to_owned());
            return None;
        }
        let request = IntentDropRequestV1 {
            operation_id: preview.operation_id.clone(),
            stack_version: preview.stack_version,
            preview_digest: preview.preview_digest.clone(),
        };
        let request_id = self.next_background_request_id();
        let state = self.intent_stack_modal_state_mut()?;
        state.request_id = request_id;
        state.operation = IntentStackModalOperation::ApplyingDrop;
        state.error = None;
        self.last_notice = Some("applying confirmed exact Intent drop".to_owned());
        Some(AppAction::ExecuteIntentDrop {
            request_id,
            request,
        })
    }

    fn select_adjacent_intent(&mut self, forward: bool) {
        let Some(state) = self.intent_stack_modal_state() else {
            return;
        };
        let row_count = state
            .stack_state
            .as_ref()
            .and_then(available_stack)
            .map_or(0, |stack| stack.intents.len());
        if row_count == 0 {
            return;
        }
        let next = if forward {
            (state.selected + 1) % row_count
        } else if state.selected == 0 {
            row_count - 1
        } else {
            state.selected - 1
        };
        let _ = self.select_intent_stack_row(next);
    }

    fn intent_stack_modal_state(&self) -> Option<&IntentStackModalState> {
        let Some(ModalState::IntentStack(state)) = self.modal_state.as_ref() else {
            return None;
        };
        Some(state.as_ref())
    }

    fn intent_stack_modal_state_mut(&mut self) -> Option<&mut IntentStackModalState> {
        let Some(ModalState::IntentStack(state)) = self.modal_state.as_mut() else {
            return None;
        };
        Some(state.as_mut())
    }
}

fn available_stack(state: &PublicIntentStackStateV1) -> Option<&PublicIntentStackV1> {
    let PublicIntentStackStateV1::Available { stack, .. } = state else {
        return None;
    };
    Some(stack)
}

fn intent_stack_phase(state: &IntentStackModalState) -> IntentStackModalPhase {
    match state.operation {
        IntentStackModalOperation::Loading => IntentStackModalPhase::Loading,
        IntentStackModalOperation::PreviewingDrop => IntentStackModalPhase::PreviewingDrop,
        IntentStackModalOperation::ConfirmingDrop => IntentStackModalPhase::ConfirmingDrop,
        IntentStackModalOperation::ApplyingDrop => IntentStackModalPhase::ApplyingDrop,
        IntentStackModalOperation::Idle => match state.stack_state.as_ref() {
            Some(PublicIntentStackStateV1::NotCreated { .. }) => IntentStackModalPhase::NotCreated,
            Some(PublicIntentStackStateV1::Available { stack, .. })
                if stack.authority_state != IntentAuthorityState::Active =>
            {
                IntentStackModalPhase::ReadOnly
            }
            Some(PublicIntentStackStateV1::Available { .. }) => IntentStackModalPhase::Ready,
            None if state.error.is_some() => IntentStackModalPhase::Unavailable,
            None => IntentStackModalPhase::Loading,
        },
    }
}

fn intent_stack_phase_detail(phase: IntentStackModalPhase) -> &'static str {
    match phase {
        IntentStackModalPhase::Loading => "Rebuilding durable Intent Stack state",
        IntentStackModalPhase::Ready => "Review intent ownership and verification before acting",
        IntentStackModalPhase::ReadOnly => {
            "Provenance is visible, but this workspace/session has no mutation authority"
        }
        IntentStackModalPhase::NotCreated => "No Intent Stack has been created in this session",
        IntentStackModalPhase::PreviewingDrop => "Building an exact read-only Drop preview",
        IntentStackModalPhase::ConfirmingDrop => {
            "Review every file and conflict before confirming Drop"
        }
        IntentStackModalPhase::ApplyingDrop => {
            "Applying the confirmed preview under the workspace mutation lease"
        }
        IntentStackModalPhase::Unavailable => "Intent Stack could not be loaded",
    }
}

fn intent_row_view(intent: &PublicIntentV1) -> IntentStackRowView {
    IntentStackRowView {
        title: intent.title.clone(),
        status: application_state_label(intent.application_state),
        detail: format!(
            "{} artifact(s) · {} verified · {} dependencies",
            intent.artifacts.len(),
            intent.system_verified_criterion_count,
            intent.depends_on.len()
        ),
    }
}

fn intent_detail_lines(stack: &PublicIntentStackV1, intent: &PublicIntentV1) -> Vec<String> {
    let mut lines = vec![
        intent.statement.clone(),
        format!(
            "State: {} · definition {:?}",
            application_state_label(intent.application_state),
            intent.definition_state
        ),
        format!(
            "Artifacts: {} exclusive · {} shared · {} unowned · {} drifted · {} unavailable",
            intent.exclusive_artifact_count,
            intent.shared_artifact_count,
            intent.unowned_artifact_count,
            intent.drifted_artifact_count,
            intent.unavailable_artifact_count
        ),
        format!(
            "Evidence: {} system verified · {} advisory",
            intent.system_verified_criterion_count, intent.advisory_criterion_count
        ),
    ];
    if intent.depends_on.is_empty() {
        lines.push("Depends on: none".to_owned());
    } else {
        lines.push(format!(
            "Depends on: {}",
            intent
                .depends_on
                .iter()
                .map(|dependency| dependency.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    lines.push(String::new());
    lines.push("Acceptance criteria".to_owned());
    lines.extend(
        intent
            .acceptance_criteria
            .iter()
            .map(|criterion| format!("• {}", criterion.statement)),
    );
    lines.push(String::new());
    lines.push("Retained artifacts".to_owned());
    if intent.artifacts.is_empty() {
        lines.push("No materialized file artifacts.".to_owned());
    } else {
        lines.extend(intent.artifacts.iter().map(|artifact| {
            let subject = artifact
                .normalized_relative_path
                .as_deref()
                .unwrap_or("non-file artifact");
            format!(
                "• {subject} · {} · {}",
                artifact_ownership_label(artifact.ownership),
                artifact_availability_label(artifact.availability)
            )
        }));
    }
    let conflicts = stack
        .conflicts
        .iter()
        .filter(|conflict| {
            conflict
                .intent_ref
                .as_ref()
                .is_none_or(|intent_ref| intent_ref == &intent.intent_ref)
        })
        .collect::<Vec<_>>();
    if !conflicts.is_empty() {
        lines.push(String::new());
        lines.push("Blocked / manual resolution required".to_owned());
        lines.extend(
            conflicts
                .iter()
                .map(|conflict| format!("• {:?}: {}", conflict.code, conflict.safe_reason)),
        );
    }
    lines
}

fn drop_preview_lines(preview: &IntentOperationPreviewV1) -> Vec<String> {
    let mut lines = vec![
        format!(
            "Exact Drop preview · {} file(s) · {} retained intent(s)",
            preview.file_effects.len(),
            preview.retained_intents.len()
        ),
        "Only the file effects listed below are authorized. Shell, network, and remote effects are not undone."
            .to_owned(),
        String::new(),
    ];
    if preview.file_effects.is_empty() {
        lines.push("No reversible file effects are available.".to_owned());
    } else {
        lines.extend(
            preview
                .file_effects
                .iter()
                .map(|file| format!("• {:?} {}", file.action, file.normalized_relative_path)),
        );
    }
    if !preview.verification_impacts.is_empty() {
        lines.push(String::new());
        lines.push(format!(
            "Verification: {} receipt(s) become stale or require rerun",
            preview.verification_impacts.len()
        ));
    }
    if !preview.conflicts.is_empty() {
        lines.push(String::new());
        lines.push("Blocked / manual resolution required".to_owned());
        lines.extend(
            preview
                .conflicts
                .iter()
                .map(|conflict| format!("• {:?}: {}", conflict.code, conflict.safe_reason)),
        );
    }
    lines
}

fn intent_stack_primary_action(
    runtime_busy: bool,
    state: &IntentStackModalState,
) -> (String, bool) {
    match state.operation {
        IntentStackModalOperation::Loading => ("Loading…".to_owned(), false),
        IntentStackModalOperation::PreviewingDrop => ("Building preview…".to_owned(), false),
        IntentStackModalOperation::ApplyingDrop => ("Applying Drop…".to_owned(), false),
        IntentStackModalOperation::ConfirmingDrop => {
            let enabled = !runtime_busy
                && state
                    .drop_preview
                    .as_ref()
                    .is_some_and(|preview| preview.conflicts.is_empty());
            ("Confirm exact Drop".to_owned(), enabled)
        }
        IntentStackModalOperation::Idle => {
            let enabled = !runtime_busy
                && state
                    .stack_state
                    .as_ref()
                    .and_then(available_stack)
                    .and_then(|stack| {
                        stack
                            .intents
                            .get(state.selected)
                            .map(|intent| (stack, intent))
                    })
                    .is_some_and(|(stack, intent)| {
                        stack.authority_state == IntentAuthorityState::Active
                            && intent
                                .available_actions
                                .contains(&IntentOperationKind::Drop)
                    });
            ("Preview Drop".to_owned(), enabled)
        }
    }
}

fn application_state_label(state: IntentApplicationState) -> &'static str {
    match state {
        IntentApplicationState::Unapplied => "unapplied",
        IntentApplicationState::Applied => "applied",
        IntentApplicationState::Dropped => "dropped",
        IntentApplicationState::NeedsReview => "needs review",
        IntentApplicationState::NeedsRebuild => "needs rebuild",
        IntentApplicationState::ReadOnly => "read only",
        IntentApplicationState::OutOfScope => "out of scope",
    }
}

fn authority_state_label(state: IntentAuthorityState) -> &'static str {
    match state {
        IntentAuthorityState::Active => "active",
        IntentAuthorityState::ReadOnlyProvenance => "read-only provenance",
        IntentAuthorityState::OutOfScope => "out of scope",
    }
}

fn artifact_ownership_label(ownership: IntentArtifactOwnership) -> &'static str {
    match ownership {
        IntentArtifactOwnership::Exclusive => "exclusive",
        IntentArtifactOwnership::Shared => "shared",
        IntentArtifactOwnership::Unowned => "unowned",
        IntentArtifactOwnership::Drifted => "drifted",
    }
}

fn artifact_availability_label(availability: IntentArtifactAvailability) -> &'static str {
    match availability {
        IntentArtifactAvailability::Available => "retained",
        IntentArtifactAvailability::Deleted => "deleted",
        IntentArtifactAvailability::Expired => "expired",
        IntentArtifactAvailability::Corrupted => "corrupted",
    }
}

fn operation_resolution_label(resolution: IntentOperationResolution) -> &'static str {
    match resolution {
        IntentOperationResolution::Committed => "committed",
        IntentOperationResolution::Rejected => "rejected",
        IntentOperationResolution::Cancelled => "cancelled",
        IntentOperationResolution::Conflicted => "conflicted",
        IntentOperationResolution::PartiallyApplied => "partially applied",
        IntentOperationResolution::Interrupted => "interrupted",
    }
}

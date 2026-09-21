use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
#[cfg(test)]
use sigil_kernel::PlanDraftCreatedEntry;
use sigil_kernel::{PlanApprovalPermission, PlanTaskStartMode};

use super::{AppAction, AppState, PendingPlanApproval, PlanActionFeedback, PlanWorkbenchAction};

impl AppState {
    pub(in crate::app) fn handle_pending_plan_approval_key_event(
        &mut self,
        key: KeyEvent,
    ) -> Option<Option<AppAction>> {
        self.clear_completed_plan_attention();
        let workbench_open = self
            .composer
            .pending_plan_approval
            .as_ref()
            .is_some_and(|pending| pending.workbench_open);
        self.composer.pending_plan_approval.as_ref()?;
        if workbench_open {
            return self.handle_plan_workbench_key_event(key);
        }
        match key.code {
            KeyCode::Enter if self.composer.input.trim().is_empty() && key.modifiers.is_empty() => {
                self.open_plan_workbench();
                Some(None)
            }
            KeyCode::BackTab if key.modifiers == KeyModifiers::SHIFT => {
                self.open_plan_workbench();
                Some(None)
            }
            _ => None,
        }
    }

    /// Drops a plan-review surface that was rehydrated one frontier behind its durable Task.
    ///
    /// A completed plan is no longer an input owner. This can happen after a crash/resume when
    /// the application projection restores the last plan before the TaskCreatedFromPlan event is
    /// reflected in the UI state. Keep the decision in the durable projection as the authority,
    /// clear only the stale presentation, and return to Build so the next user prompt is a normal
    /// conversation turn rather than another plan request.
    fn clear_completed_plan_attention(&mut self) {
        let Some(plan_id) = self
            .composer
            .pending_plan_approval
            .as_ref()
            .and_then(|pending| pending.plan_id.clone())
        else {
            return;
        };
        let Ok(plan_id) = sigil_kernel::PlanId::new(plan_id) else {
            return;
        };
        let plans = sigil_kernel::PlanArtifactProjection::from_entries(
            &self.session_browser.current_entries,
        );
        let completed = plans.latest_decision(&plan_id).is_some_and(|decision| {
            decision.decision == sigil_kernel::PlanDecision::Accepted
                && plans.task_created_for_plan(&plan_id)
        });
        if completed {
            self.clear_pending_plan_approval();
            self.composer.mode = super::ComposerMode::Build;
        }
    }

    fn handle_plan_workbench_key_event(&mut self, key: KeyEvent) -> Option<Option<AppAction>> {
        let current_action = self
            .composer
            .pending_plan_approval
            .as_ref()?
            .selected_action;
        match key.code {
            KeyCode::Esc if key.modifiers.is_empty() => {
                if let Some(pending) = self.composer.pending_plan_approval.as_mut() {
                    pending.workbench_open = false;
                }
                self.last_notice = Some("plan review closed; Shift-Tab reopens it".to_owned());
                Some(None)
            }
            KeyCode::Up if key.modifiers.is_empty() => {
                self.update_plan_workbench_scroll(|scroll| scroll.saturating_sub(1));
                Some(None)
            }
            KeyCode::Down if key.modifiers.is_empty() => {
                self.update_plan_workbench_scroll(|scroll| scroll.saturating_add(1));
                Some(None)
            }
            KeyCode::PageUp if key.modifiers.is_empty() => {
                self.update_plan_workbench_scroll(|scroll| scroll.saturating_sub(8));
                Some(None)
            }
            KeyCode::PageDown if key.modifiers.is_empty() => {
                self.update_plan_workbench_scroll(|scroll| scroll.saturating_add(8));
                Some(None)
            }
            KeyCode::Home if key.modifiers.is_empty() => {
                self.update_plan_workbench_scroll(|_| 0);
                Some(None)
            }
            KeyCode::End if key.modifiers.is_empty() => {
                self.update_plan_workbench_scroll(|_| usize::MAX);
                Some(None)
            }
            KeyCode::Tab | KeyCode::Right if key.modifiers.is_empty() => {
                self.select_adjacent_plan_action(current_action, 1);
                Some(None)
            }
            KeyCode::BackTab | KeyCode::Left
                if key.modifiers.is_empty() || key.modifiers == KeyModifiers::SHIFT =>
            {
                self.select_adjacent_plan_action(current_action, -1);
                Some(None)
            }
            KeyCode::Char('r') if key.modifiers.is_empty() => {
                Some(self.execute_plan_workbench_action(PlanWorkbenchAction::Run))
            }
            KeyCode::Char('s') if key.modifiers.is_empty() => {
                Some(self.execute_plan_workbench_action(PlanWorkbenchAction::Save))
            }
            KeyCode::Char('v') if key.modifiers.is_empty() => {
                Some(self.execute_plan_workbench_action(PlanWorkbenchAction::Revise))
            }
            KeyCode::Char('x') if key.modifiers.is_empty() => {
                Some(self.execute_plan_workbench_action(PlanWorkbenchAction::Reject))
            }
            KeyCode::Char('a') if key.modifiers.is_empty() => {
                Some(self.execute_plan_workbench_action(PlanWorkbenchAction::AdoptCandidate))
            }
            KeyCode::Char('t') if key.modifiers.is_empty() => {
                Some(self.execute_plan_workbench_action(PlanWorkbenchAction::RetryReview))
            }
            KeyCode::Enter if key.modifiers.is_empty() => {
                Some(self.execute_plan_workbench_action(current_action))
            }
            _ => Some(None),
        }
    }

    fn execute_plan_workbench_action(&mut self, action: PlanWorkbenchAction) -> Option<AppAction> {
        let pending = self.composer.pending_plan_approval.as_ref()?;
        if pending.action_pending() {
            return None;
        }
        if self.runtime.is_busy {
            let message = "wait for the current operation before changing this plan".to_owned();
            self.set_pending_plan_action_failure(action, message);
            return None;
        }
        match action {
            PlanWorkbenchAction::Run => {
                self.create_task_from_pending_plan(PlanTaskStartMode::CreateAndRun, None)
            }
            PlanWorkbenchAction::Save => self.save_pending_plan(),
            PlanWorkbenchAction::Revise => self.revise_pending_plan(),
            PlanWorkbenchAction::Reject => self.reject_pending_plan(),
            PlanWorkbenchAction::AdoptCandidate => self.adopt_pending_candidate(),
            PlanWorkbenchAction::RetryReview => self.retry_pending_plan_review(),
        }
    }

    fn open_plan_workbench(&mut self) {
        if let Some(pending) = self.composer.pending_plan_approval.as_mut() {
            pending.workbench_open = true;
            self.last_notice = Some("reviewing complete plan".to_owned());
        }
    }

    fn update_plan_workbench_scroll(&mut self, update: impl FnOnce(usize) -> usize) {
        if let Some(pending) = self.composer.pending_plan_approval.as_mut() {
            let max_scroll = pending.workbench_scroll_extent.get();
            let current = pending.workbench_scroll.min(max_scroll);
            pending.workbench_scroll = update(current).min(max_scroll);
        }
    }

    fn select_adjacent_plan_action(&mut self, current: PlanWorkbenchAction, direction: isize) {
        let Some(pending) = self.composer.pending_plan_approval.as_ref() else {
            return;
        };
        let available = PlanWorkbenchAction::ORDER
            .iter()
            .copied()
            .filter(|candidate| pending.action_enabled(*candidate))
            .collect::<Vec<_>>();
        if available.is_empty() {
            return;
        }
        let index = available
            .iter()
            .position(|candidate| *candidate == current)
            .unwrap_or(0) as isize;
        let next = (index + direction).rem_euclid(available.len() as isize);
        if let Some(pending) = self.composer.pending_plan_approval.as_mut() {
            pending.selected_action = available[next as usize];
        }
    }

    fn save_pending_plan(&mut self) -> Option<AppAction> {
        let pending = self.composer.pending_plan_approval.as_ref()?;
        if !pending.action_allowed(PlanWorkbenchAction::Save) {
            self.last_notice = Some("save is unavailable in the current plan state".to_owned());
            return None;
        }
        let plan_id = pending.plan_id.clone()?;
        let expected_plan_hash = pending.plan_hash.clone();
        self.begin_pending_plan_action(PlanWorkbenchAction::Save);
        self.last_notice = Some("saving plan for later".to_owned());
        self.push_event("plan", "save");
        Some(AppAction::SavePlan {
            plan_id,
            expected_plan_hash,
        })
    }

    fn revise_pending_plan(&mut self) -> Option<AppAction> {
        let pending = self.composer.pending_plan_approval.as_ref()?;
        if !pending.action_allowed(PlanWorkbenchAction::Revise) {
            self.last_notice = Some("revision is unavailable in the current plan state".to_owned());
            return None;
        }
        let plan_id = pending.plan_id.clone()?;
        let expected_plan_hash = pending.plan_hash.clone();
        self.begin_pending_plan_action(PlanWorkbenchAction::Revise);
        self.last_notice = Some("revising plan".to_owned());
        self.push_event("plan", "revise");
        Some(AppAction::RevisePlan {
            plan_id,
            expected_plan_hash,
        })
    }

    fn reject_pending_plan(&mut self) -> Option<AppAction> {
        let pending = self.composer.pending_plan_approval.as_ref()?;
        if !pending.action_allowed(PlanWorkbenchAction::Reject) {
            self.last_notice = Some("reject is unavailable in the current plan state".to_owned());
            return None;
        }
        let Some(plan_id) = pending.plan_id.clone() else {
            self.clear_pending_plan_approval();
            self.last_notice = Some("plan dismissed".to_owned());
            self.push_event("plan", "dismissed");
            return None;
        };
        let expected_plan_hash = pending.plan_hash.clone();
        self.begin_pending_plan_action(PlanWorkbenchAction::Reject);
        self.last_notice = Some("rejecting plan".to_owned());
        self.push_event("plan", "reject");
        Some(AppAction::RejectPlan {
            plan_id,
            expected_plan_hash,
        })
    }

    /// A Plan action must stay bound to the exact durable draft displayed in the workbench.
    fn pending_plan_matches_durable_draft(&self, pending: &PendingPlanApproval) -> bool {
        let Some(plan_id) = pending
            .plan_id
            .as_ref()
            .and_then(|plan_id| sigil_kernel::PlanId::new(plan_id.clone()).ok())
        else {
            return false;
        };
        let plans = sigil_kernel::PlanArtifactProjection::from_entries(
            &self.session_browser.current_entries,
        );
        plans
            .plans
            .get(&plan_id)
            .is_some_and(|draft| draft.plan_hash == pending.plan_hash)
    }

    fn create_task_from_pending_plan(
        &mut self,
        start_mode: PlanTaskStartMode,
        permission_grant: Option<PlanApprovalPermission>,
    ) -> Option<AppAction> {
        let pending = self.composer.pending_plan_approval.as_ref()?;
        if !pending.action_allowed(PlanWorkbenchAction::Run) {
            self.last_notice = Some("run is unavailable in the current plan state".to_owned());
            return None;
        }
        if !self.pending_plan_matches_durable_draft(pending) {
            self.last_notice = Some("plan content changed; refresh the review".to_owned());
            return None;
        }
        let Some(plan_id) = pending.plan_id.clone() else {
            self.last_notice = Some("plan is not durable yet".to_owned());
            return None;
        };
        let expected_plan_hash = pending.plan_hash.clone();
        self.begin_pending_plan_action(PlanWorkbenchAction::Run);
        self.last_notice = Some(match start_mode {
            PlanTaskStartMode::CreatePaused if permission_grant.is_some() => {
                "creating task with scoped edits".to_owned()
            }
            PlanTaskStartMode::CreatePaused => "creating task from plan".to_owned(),
            PlanTaskStartMode::CreateAndRun => "creating and running task from plan".to_owned(),
        });
        self.push_event("plan", "create_task");
        Some(AppAction::CreateTaskFromPlan {
            plan_id,
            expected_plan_hash,
            start_mode,
            permission_grant,
        })
    }

    pub(crate) fn pending_plan_approval(&self) -> Option<&PendingPlanApproval> {
        self.composer.pending_plan_approval.as_ref()
    }

    #[cfg(test)]
    pub(crate) fn set_pending_plan_approval_from_draft(
        &mut self,
        draft: &PlanDraftCreatedEntry,
        current_workspace_snapshot_id: Option<&str>,
    ) {
        let entries = [sigil_kernel::SessionLogEntry::Control(
            sigil_kernel::ControlEntry::PlanDraftCreated(draft.clone()),
        )];
        let Ok(detail) = sigil_kernel::plan_review_detail_from_entries(
            &entries,
            &draft.plan_id,
            &draft.plan_hash,
        ) else {
            self.composer.pending_plan_approval = None;
            return;
        };
        self.set_pending_plan_approval_from_detail(&detail, current_workspace_snapshot_id);
        if let Some(pending) = self.composer.pending_plan_approval.as_mut() {
            pending.status = Some(sigil_kernel::PublicPlanReviewStatus::DraftReady);
            pending.allowed_actions = vec![
                sigil_kernel::PublicPlanAction::Run,
                sigil_kernel::PublicPlanAction::Save,
                sigil_kernel::PublicPlanAction::Revise,
                sigil_kernel::PublicPlanAction::Reject,
            ];
        }
    }

    pub(crate) fn set_pending_plan_approval_from_detail(
        &mut self,
        detail: &sigil_kernel::PlanReviewDetailV1,
        current_workspace_snapshot_id: Option<&str>,
    ) {
        let previous = self.composer.pending_plan_approval.take();
        if detail.steps.is_empty()
            && detail
                .legacy_markdown
                .as_deref()
                .is_none_or(|text| text.trim().is_empty())
        {
            self.composer.pending_plan_approval = None;
            return;
        }
        let plan_preview = detail
            .legacy_markdown
            .as_deref()
            .unwrap_or(&detail.summary)
            .trim();
        if plan_preview.is_empty() {
            self.composer.pending_plan_approval = None;
            return;
        }
        let steps = detail
            .steps
            .iter()
            .map(|step| step.title.clone())
            .collect::<Vec<_>>();
        let stale_reason = sigil_runtime::plan_review_coordinator::plan_handoff_stale_reason(
            detail.workspace_snapshot_id.as_deref(),
            current_workspace_snapshot_id,
        );
        self.composer.pending_plan_approval = Some(PendingPlanApproval {
            plan_id: Some(detail.plan_id.as_str().to_owned()),
            plan_hash: detail.plan_hash.clone(),
            summary: detail.summary.clone(),
            steps,
            target_path_count: detail.target_paths.len(),
            suggested_check_count: detail.suggested_checks.len(),
            workspace_snapshot_id: detail.workspace_snapshot_id.clone(),
            stale: stale_reason.is_some(),
            stale_reason,
            last_run_failure: None,
            // Action authority comes only from the canonical public projection. A detail payload
            // is immutable display data and must never grant actions by itself.
            allowed_actions: Vec::new(),
            status: None,
            revision: None,
            saved_for_later: false,
            action_feedback: None,
            detail: detail.clone(),
            workbench_open: false,
            workbench_scroll: 0,
            workbench_scroll_extent: Default::default(),
            selected_action: PlanWorkbenchAction::Run,
        });
        self.restore_pending_plan_presentation(previous);
    }

    pub(crate) fn set_pending_plan_candidate(
        &mut self,
        review: &sigil_kernel::PublicPlanReview,
        current_workspace_snapshot_id: Option<&str>,
    ) {
        let Some(candidate) = review.candidate.as_ref() else {
            return;
        };
        let Ok(plan_id) = sigil_kernel::PlanId::new(review.plan_id.clone()) else {
            return;
        };
        let lineage = self.pending_plan_lineage(&plan_id);
        let summary = candidate
            .content
            .lines()
            .find(|line| !line.trim().is_empty())
            .map(|line| line.trim().to_owned())
            .unwrap_or_else(|| "Complete Plan candidate".to_owned());
        let detail = sigil_kernel::PlanReviewDetailV1 {
            plan_id,
            plan_hash: candidate.content_hash.clone(),
            workspace_snapshot_id: None,
            source: match review.source {
                sigil_kernel::PublicPlanReviewSource::ExplicitPlanCommand => {
                    sigil_kernel::PlanReviewSource::ExplicitPlanCommand
                }
                sigil_kernel::PublicPlanReviewSource::AutomaticConversationRoute => {
                    sigil_kernel::PlanReviewSource::AutomaticConversationRoute
                }
            },
            summary,
            steps: Vec::new(),
            target_paths: Vec::new(),
            suggested_checks: Vec::new(),
            risk: None,
            notes: vec![
                "Complete Plan candidate preserved from read-only review; adopt it to create a durable draft.".to_owned(),
            ],
            lineage,
            legacy_markdown: Some(candidate.content.clone()),
        };
        self.set_pending_plan_approval_from_detail(&detail, current_workspace_snapshot_id);
        self.apply_pending_plan_public_review(review);
        if let Some(pending) = self.composer.pending_plan_approval.as_mut() {
            pending.selected_action = if pending.action_allowed(PlanWorkbenchAction::AdoptCandidate)
            {
                PlanWorkbenchAction::AdoptCandidate
            } else if pending.action_allowed(PlanWorkbenchAction::RetryReview) {
                PlanWorkbenchAction::RetryReview
            } else {
                pending.selected_action
            };
        }
    }

    /// Rehydrates a terminal review that has no readable draft or preserved candidate. The
    /// workbench still exposes the exact durable Retry action so a user can continue the review
    /// without inventing plan text in the presentation layer.
    pub(crate) fn set_pending_plan_retry(
        &mut self,
        review: &sigil_kernel::PublicPlanReview,
        _current_workspace_snapshot_id: Option<&str>,
    ) {
        if !review
            .allowed_actions
            .contains(&sigil_kernel::PublicPlanAction::RetryReview)
        {
            return;
        }
        let Ok(plan_id) = sigil_kernel::PlanId::new(review.plan_id.clone()) else {
            return;
        };
        let previous = self.composer.pending_plan_approval.take();
        let lineage = self.pending_plan_lineage(&plan_id);
        let source = match review.source {
            sigil_kernel::PublicPlanReviewSource::ExplicitPlanCommand => {
                sigil_kernel::PlanReviewSource::ExplicitPlanCommand
            }
            sigil_kernel::PublicPlanReviewSource::AutomaticConversationRoute => {
                sigil_kernel::PlanReviewSource::AutomaticConversationRoute
            }
        };
        let detail = sigil_kernel::PlanReviewDetailV1 {
            plan_id,
            plan_hash: review.plan_hash.clone().unwrap_or_default(),
            workspace_snapshot_id: None,
            source,
            summary: review
                .summary
                .clone()
                .unwrap_or_else(|| "Plan review can be retried".to_owned()),
            steps: Vec::new(),
            target_paths: Vec::new(),
            suggested_checks: Vec::new(),
            risk: review.risk.clone(),
            notes: vec![
                "The previous Plan review ended before producing a draft; retry it to continue."
                    .to_owned(),
            ],
            lineage,
            legacy_markdown: None,
        };
        self.composer.pending_plan_approval = Some(PendingPlanApproval {
            plan_id: Some(review.plan_id.clone()),
            plan_hash: review.plan_hash.clone().unwrap_or_default(),
            summary: detail.summary.clone(),
            steps: Vec::new(),
            target_path_count: 0,
            suggested_check_count: 0,
            workspace_snapshot_id: None,
            stale: review.stale,
            stale_reason: None,
            last_run_failure: None,
            allowed_actions: review.allowed_actions.clone(),
            status: Some(review.status),
            revision: review.revision.clone(),
            saved_for_later: false,
            action_feedback: None,
            detail,
            workbench_open: false,
            workbench_scroll: 0,
            workbench_scroll_extent: Default::default(),
            selected_action: PlanWorkbenchAction::RetryReview,
        });
        self.restore_pending_plan_presentation(previous);
    }

    fn adopt_pending_candidate(&mut self) -> Option<AppAction> {
        let pending = self.composer.pending_plan_approval.as_ref()?;
        if !pending.action_allowed(PlanWorkbenchAction::AdoptCandidate) {
            self.last_notice =
                Some("candidate adoption is unavailable in the current plan state".to_owned());
            return None;
        }
        let plan_id = pending.plan_id.clone()?;
        let expected_candidate_hash = pending.plan_hash.clone();
        self.begin_pending_plan_action(PlanWorkbenchAction::AdoptCandidate);
        self.last_notice = Some("adopting preserved Plan candidate".to_owned());
        self.push_event("plan", "adopt_candidate");
        Some(AppAction::AdoptPlanCandidate {
            plan_id,
            expected_candidate_hash,
        })
    }

    fn retry_pending_plan_review(&mut self) -> Option<AppAction> {
        let pending = self.composer.pending_plan_approval.as_ref()?;
        if !pending.action_allowed(PlanWorkbenchAction::RetryReview) {
            self.last_notice =
                Some("plan review retry is unavailable in the current state".to_owned());
            return None;
        }
        let plan_id = pending.plan_id.clone()?;
        let expected_candidate_hash =
            (!pending.plan_hash.trim().is_empty()).then(|| pending.plan_hash.clone());
        self.begin_pending_plan_action(PlanWorkbenchAction::RetryReview);
        self.last_notice = Some("retrying preserved Plan review".to_owned());
        self.push_event("plan", "retry_review");
        Some(AppAction::RetryPlanReview {
            plan_id,
            expected_candidate_hash,
        })
    }

    pub(crate) fn apply_pending_plan_public_review(
        &mut self,
        review: &sigil_kernel::PublicPlanReview,
    ) {
        let Some(pending) = self.composer.pending_plan_approval.as_mut() else {
            return;
        };
        if pending.plan_id.as_deref() != Some(review.plan_id.as_str()) {
            return;
        }
        pending.allowed_actions = review.allowed_actions.clone();
        pending.status = Some(review.status);
        let previous_revision = pending.revision.clone();
        pending.revision = review.revision.clone();
        pending.saved_for_later = self
            .session_browser
            .current_entries
            .iter()
            .rev()
            .find_map(|entry| match entry {
                sigil_kernel::SessionLogEntry::Control(
                    sigil_kernel::ControlEntry::PlanDecisionRecorded(decision),
                ) if decision.plan_id.as_str() == review.plan_id => Some(decision),
                _ => None,
            })
            .is_some_and(|decision| {
                decision.plan_hash == pending.plan_hash
                    && decision.decision == sigil_kernel::PlanDecision::SavedOnly
            });
        if previous_revision != pending.revision {
            pending.action_feedback = None;
        }
        if pending.status == Some(sigil_kernel::PublicPlanReviewStatus::DraftReady)
            && matches!(
                pending.action_feedback,
                Some(PlanActionFeedback::Pending(
                    PlanWorkbenchAction::AdoptCandidate
                ))
            )
        {
            pending.action_feedback = Some(PlanActionFeedback::Succeeded {
                action: PlanWorkbenchAction::AdoptCandidate,
                message: "Candidate adopted. Review the plan before choosing Run.".to_owned(),
            });
        }
        if !pending.action_allowed(pending.selected_action)
            && let Some(action) = PlanWorkbenchAction::ORDER
                .iter()
                .copied()
                .find(|action| pending.action_allowed(*action))
        {
            pending.selected_action = action;
        }
    }

    pub(in crate::app) fn clear_pending_plan_approval(&mut self) {
        self.composer.pending_plan_approval = None;
    }

    fn begin_pending_plan_action(&mut self, action: PlanWorkbenchAction) {
        if let Some(pending) = self.composer.pending_plan_approval.as_mut() {
            pending.action_feedback = Some(PlanActionFeedback::Pending(action));
        }
    }

    fn set_pending_plan_action_failure(&mut self, action: PlanWorkbenchAction, message: String) {
        self.last_notice = Some(message.clone());
        if let Some(pending) = self.composer.pending_plan_approval.as_mut() {
            pending.action_feedback = Some(PlanActionFeedback::Failed { action, message });
        }
    }

    /// Applies a local operation failure only to the exact Plan operation still awaiting it.
    pub(crate) fn fail_pending_plan_action(
        &mut self,
        action: sigil_kernel::PublicPlanAction,
        plan_id: &str,
        expected_plan_hash: &str,
        message: String,
    ) -> bool {
        let action = PlanWorkbenchAction::from_public(action);
        let matches = self
            .composer
            .pending_plan_approval
            .as_ref()
            .is_some_and(|pending| {
                pending.plan_id.as_deref() == Some(plan_id)
                    && pending.plan_hash == expected_plan_hash
                    && pending.action_feedback == Some(PlanActionFeedback::Pending(action))
            });
        if !matches {
            return false;
        }
        self.set_pending_plan_action_failure(action, message);
        true
    }

    /// Returns a rejected launcher command to its exact workbench without changing run state.
    pub(crate) fn fail_plan_action(&mut self, action: &AppAction, message: String) -> bool {
        let (action, plan_id, plan_hash) = match action {
            AppAction::SavePlan {
                plan_id,
                expected_plan_hash,
            } => (
                sigil_kernel::PublicPlanAction::Save,
                plan_id,
                expected_plan_hash.as_str(),
            ),
            AppAction::RevisePlan {
                plan_id,
                expected_plan_hash,
            } => (
                sigil_kernel::PublicPlanAction::Revise,
                plan_id,
                expected_plan_hash.as_str(),
            ),
            AppAction::RejectPlan {
                plan_id,
                expected_plan_hash,
            } => (
                sigil_kernel::PublicPlanAction::Reject,
                plan_id,
                expected_plan_hash.as_str(),
            ),
            AppAction::CreateTaskFromPlan {
                plan_id,
                expected_plan_hash,
                ..
            } => (
                sigil_kernel::PublicPlanAction::Run,
                plan_id,
                expected_plan_hash.as_str(),
            ),
            AppAction::AdoptPlanCandidate {
                plan_id,
                expected_candidate_hash,
            } => (
                sigil_kernel::PublicPlanAction::AdoptCandidate,
                plan_id,
                expected_candidate_hash.as_str(),
            ),
            AppAction::RetryPlanReview {
                plan_id,
                expected_candidate_hash,
            } => (
                sigil_kernel::PublicPlanAction::RetryReview,
                plan_id,
                expected_candidate_hash.as_deref().unwrap_or_default(),
            ),
            _ => return false,
        };
        self.fail_pending_plan_action(action, plan_id, plan_hash, message)
    }

    pub(crate) fn complete_pending_plan_save(&mut self, plan_id: &str, plan_hash: &str) {
        let Some(pending) = self
            .composer
            .pending_plan_approval
            .as_mut()
            .filter(|pending| {
                pending.plan_id.as_deref() == Some(plan_id)
                    && pending.plan_hash == plan_hash
                    && pending.saved_for_later
            })
        else {
            return;
        };
        pending.action_feedback = Some(PlanActionFeedback::Succeeded {
            action: PlanWorkbenchAction::Save,
            message: "Saved for later. The plan remains available; choose Run when you are ready."
                .to_owned(),
        });
    }

    fn pending_plan_lineage(&self, plan_id: &sigil_kernel::PlanId) -> sigil_kernel::PlanLineageV1 {
        let projection =
            sigil_kernel::PlanReviewProjection::from_entries(&self.session_browser.current_entries);
        let attempt = projection.attempt_for_plan(plan_id);
        sigil_kernel::PlanLineageV1 {
            source: sigil_kernel::PlanSourceRef::default(),
            plan_review_id: attempt.map(|attempt| attempt.plan_review_id.clone()),
            attempt_id: attempt.map(|attempt| attempt.attempt_id.clone()),
            created_at_ms: attempt.map_or(0, |attempt| attempt.recorded_at_ms),
        }
    }

    /// Carries only presentation across an authoritative refresh of the same Plan lineage.
    pub(super) fn restore_pending_plan_presentation(
        &mut self,
        previous: Option<PendingPlanApproval>,
    ) {
        let (Some(previous), Some(pending)) =
            (previous, self.composer.pending_plan_approval.as_mut())
        else {
            return;
        };
        let same_plan =
            previous.plan_id == pending.plan_id && previous.plan_hash == pending.plan_hash;
        let same_review = previous
            .detail
            .lineage
            .plan_review_id
            .as_ref()
            .is_some_and(|review| pending.detail.lineage.plan_review_id.as_ref() == Some(review));
        if !same_plan && !same_review {
            return;
        }
        pending.workbench_open = previous.workbench_open;
        if same_plan {
            pending.workbench_scroll = previous.workbench_scroll;
            pending.workbench_scroll_extent = previous.workbench_scroll_extent;
            if pending.status.is_none() {
                pending.status = previous.status;
                pending.revision = previous.revision.clone();
            }
            if pending.revision == previous.revision && pending.action_feedback.is_none() {
                pending.action_feedback = previous.action_feedback;
            }
            if pending.status == Some(sigil_kernel::PublicPlanReviewStatus::DraftReady)
                && pending.action_feedback
                    == Some(PlanActionFeedback::Pending(
                        PlanWorkbenchAction::AdoptCandidate,
                    ))
            {
                pending.action_feedback = Some(PlanActionFeedback::Succeeded {
                    action: PlanWorkbenchAction::AdoptCandidate,
                    message: "The adopted plan is ready for review. It has not been started."
                        .to_owned(),
                });
            }
        } else if pending.workbench_open
            && pending.status == Some(sigil_kernel::PublicPlanReviewStatus::DraftReady)
        {
            pending.action_feedback = Some(PlanActionFeedback::Succeeded {
                action: PlanWorkbenchAction::Revise,
                message: "The revised plan is ready for review. It has not been started."
                    .to_owned(),
            });
        }
    }
}

impl PendingPlanApproval {
    pub(crate) fn action_allowed(&self, action: PlanWorkbenchAction) -> bool {
        self.allowed_actions.contains(&action.public_action())
    }

    pub(crate) fn action_pending(&self) -> bool {
        matches!(self.action_feedback, Some(PlanActionFeedback::Pending(_)))
    }

    pub(crate) fn action_enabled(&self, action: PlanWorkbenchAction) -> bool {
        // Workspace observations are advisory. Durable Plan state owns action availability.
        self.action_allowed(action) && !self.action_pending()
    }

    pub(crate) fn status_label(&self) -> &'static str {
        if self.saved_for_later {
            return "saved for later";
        }
        if let Some(revision) = self.revision.as_ref() {
            return match revision.status {
                sigil_kernel::PublicPlanRevisionStatusV1::AwaitingGuidance => {
                    "revision awaiting guidance"
                }
                sigil_kernel::PublicPlanRevisionStatusV1::Queued => "revision queued",
                sigil_kernel::PublicPlanRevisionStatusV1::Researching => "revision researching",
                sigil_kernel::PublicPlanRevisionStatusV1::WaitingForInput => {
                    "revision waiting for input"
                }
                sigil_kernel::PublicPlanRevisionStatusV1::Failed => "revision failed",
                sigil_kernel::PublicPlanRevisionStatusV1::Cancelled => "revision cancelled",
                sigil_kernel::PublicPlanRevisionStatusV1::Succeeded => "revision succeeded",
            };
        }
        match self.status {
            Some(sigil_kernel::PublicPlanReviewStatus::DraftReady) => "ready",
            Some(sigil_kernel::PublicPlanReviewStatus::Started) => "researching",
            Some(sigil_kernel::PublicPlanReviewStatus::WaitingForInput) => "waiting for input",
            Some(sigil_kernel::PublicPlanReviewStatus::CompileFailed) => "needs changes",
            Some(sigil_kernel::PublicPlanReviewStatus::Paused) => "paused",
            Some(sigil_kernel::PublicPlanReviewStatus::Blocked) => "blocked",
            Some(sigil_kernel::PublicPlanReviewStatus::Failed) => "failed",
            Some(sigil_kernel::PublicPlanReviewStatus::Interrupted) => "interrupted",
            Some(sigil_kernel::PublicPlanReviewStatus::Cancelled) => "cancelled",
            Some(sigil_kernel::PublicPlanReviewStatus::CompletedWithoutDraft) => {
                "finished without a draft"
            }
            None => "review",
        }
    }

    pub(crate) fn revision_detail(&self) -> Option<&'static str> {
        self.revision
            .as_ref()
            .map(|revision| match revision.status {
                sigil_kernel::PublicPlanRevisionStatusV1::AwaitingGuidance => {
                    "Original plan · read-only while revision guidance is open."
                }
                sigil_kernel::PublicPlanRevisionStatusV1::Queued => {
                    "Original plan · read-only while the revision waits to start."
                }
                sigil_kernel::PublicPlanRevisionStatusV1::Researching
                | sigil_kernel::PublicPlanRevisionStatusV1::WaitingForInput => {
                    "Original plan · read-only while the revised plan is prepared."
                }
                sigil_kernel::PublicPlanRevisionStatusV1::Failed => {
                    "Revision failed. The original plan remains available for review and later use."
                }
                sigil_kernel::PublicPlanRevisionStatusV1::Cancelled => {
                    "Revision cancelled. The original plan remains available."
                }
                sigil_kernel::PublicPlanRevisionStatusV1::Succeeded => {
                    "Revised plan · review it before choosing Run. It has not been started."
                }
            })
    }

    pub(crate) fn structure_summary(&self) -> Option<String> {
        let counts = [
            (self.steps.len(), "step", "steps"),
            (self.target_path_count, "path", "paths"),
            (self.suggested_check_count, "check", "checks"),
        ]
        .into_iter()
        .filter(|(count, _, _)| *count > 0)
        .map(|(count, singular, plural)| {
            format!("{count} {}", if count == 1 { singular } else { plural })
        })
        .collect::<Vec<_>>();
        (!counts.is_empty()).then(|| counts.join(" · "))
    }

    pub(crate) fn workbench_key_hint(&self) -> String {
        let actions = PlanWorkbenchAction::ORDER
            .into_iter()
            .filter(|action| self.action_enabled(*action))
            .map(|action| format!("{} {}", action.shortcut(), action.label().to_lowercase()))
            .collect::<Vec<_>>();
        if actions.is_empty() {
            return "↑↓/Pg scroll · Esc close".to_owned();
        }
        format!(
            "↑↓/Pg scroll · Tab action · {} · Enter confirm · Esc close",
            actions.join(" · ")
        )
    }
}

impl PlanWorkbenchAction {
    pub(crate) fn pending_label(self) -> &'static str {
        match self {
            Self::Run => "Starting the plan",
            Self::Save => "Saving for later",
            Self::Revise => "Opening revision guidance",
            Self::Reject => "Rejecting the plan",
            Self::AdoptCandidate => "Adopting the candidate",
            Self::RetryReview => "Preparing the review retry",
        }
    }

    fn public_action(self) -> sigil_kernel::PublicPlanAction {
        match self {
            PlanWorkbenchAction::Run => sigil_kernel::PublicPlanAction::Run,
            PlanWorkbenchAction::Save => sigil_kernel::PublicPlanAction::Save,
            PlanWorkbenchAction::Revise => sigil_kernel::PublicPlanAction::Revise,
            PlanWorkbenchAction::Reject => sigil_kernel::PublicPlanAction::Reject,
            PlanWorkbenchAction::AdoptCandidate => sigil_kernel::PublicPlanAction::AdoptCandidate,
            PlanWorkbenchAction::RetryReview => sigil_kernel::PublicPlanAction::RetryReview,
        }
    }

    fn from_public(action: sigil_kernel::PublicPlanAction) -> Self {
        match action {
            sigil_kernel::PublicPlanAction::Run => Self::Run,
            sigil_kernel::PublicPlanAction::Save => Self::Save,
            sigil_kernel::PublicPlanAction::Revise => Self::Revise,
            sigil_kernel::PublicPlanAction::Reject => Self::Reject,
            sigil_kernel::PublicPlanAction::AdoptCandidate => Self::AdoptCandidate,
            sigil_kernel::PublicPlanAction::RetryReview => Self::RetryReview,
        }
    }
}

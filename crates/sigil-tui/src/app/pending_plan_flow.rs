use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
#[cfg(test)]
use sigil_kernel::PlanDraftCreatedEntry;
use sigil_kernel::{PlanApprovalPermission, PlanTaskStartMode};

use super::{AppAction, AppState, PendingPlanApproval, PlanWorkbenchAction};

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
        // A durable Task shell alone is not completion while its post-approval
        // materialization is blocked and still needs the Plan retry/revise surface.
        let has_unresolved_materialization_blocker =
            plans.tasks_created.get(&plan_id).is_some_and(|links| {
                links.iter().any(|link| {
                    plans
                        .materialization_blocker_for_task(&link.task_id)
                        .is_some()
                })
            });
        let completed = plans.latest_decision(&plan_id).is_some_and(|decision| {
            decision.decision == sigil_kernel::PlanDecision::Accepted
                && plans.task_created_for_plan(&plan_id)
        }) && !has_unresolved_materialization_blocker;
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
            KeyCode::Enter if key.modifiers.is_empty() => {
                Some(self.execute_plan_workbench_action(current_action))
            }
            _ => Some(None),
        }
    }

    fn execute_plan_workbench_action(&mut self, action: PlanWorkbenchAction) -> Option<AppAction> {
        match action {
            PlanWorkbenchAction::Run => {
                self.create_task_from_pending_plan(PlanTaskStartMode::CreateAndRun, None)
            }
            PlanWorkbenchAction::Save => self.save_pending_plan(),
            PlanWorkbenchAction::Revise => self.revise_pending_plan(),
            PlanWorkbenchAction::Reject => self.reject_pending_plan(),
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
            .filter(|candidate| pending.action_allowed(*candidate))
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
        if pending.stale {
            self.last_notice = Some(
                pending
                    .stale_reason
                    .clone()
                    .unwrap_or_else(|| "plan may be stale; cannot save".to_owned()),
            );
            return None;
        }
        let plan_id = pending.plan_id.clone()?;
        let expected_plan_hash = pending.plan_hash.clone();
        self.last_notice = Some("saving plan".to_owned());
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
        self.last_notice = Some("rejecting plan".to_owned());
        self.push_event("plan", "reject");
        Some(AppAction::RejectPlan {
            plan_id,
            expected_plan_hash,
        })
    }

    /// Returns whether the durable Task attached to this exact approved Plan may retry
    /// materialization despite a workspace-staleness presentation warning.
    ///
    /// This deliberately replays the durable facts instead of trusting
    /// `PendingPlanApproval::retrying_materialization`, which is UI state and can be stale
    /// across a refresh or restart.
    fn has_durable_materialization_retry_admission(&self, pending: &PendingPlanApproval) -> bool {
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
        let draft_matches = plans
            .plans
            .get(&plan_id)
            .is_some_and(|draft| draft.plan_hash == pending.plan_hash);
        let accepted = plans.latest_decision(&plan_id).is_some_and(|decision| {
            decision.decision == sigil_kernel::PlanDecision::Accepted
                && decision.plan_hash == pending.plan_hash
        });
        draft_matches
            && accepted
            && plans.tasks_created.get(&plan_id).is_some_and(|links| {
                links.iter().any(|link| {
                    link.plan_hash == pending.plan_hash
                        && plans
                            .materialization_blocker_for_task(&link.task_id)
                            .is_some_and(|blocked| {
                                blocked.plan_hash == pending.plan_hash
                                    && blocked.blocker.available_actions.contains(
                                        &sigil_kernel::TaskBlockerActionV1::RetryAdmission,
                                    )
                            })
                })
            })
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
        if pending.stale && !self.has_durable_materialization_retry_admission(pending) {
            self.last_notice = Some(
                pending
                    .stale_reason
                    .clone()
                    .unwrap_or_else(|| "plan may be stale; cannot create a task".to_owned()),
            );
            return None;
        }
        let pending = self.composer.pending_plan_approval.take()?;
        let Some(plan_id) = pending.plan_id else {
            self.last_notice = Some("plan is not durable yet".to_owned());
            self.composer.pending_plan_approval = Some(pending);
            return None;
        };
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
            expected_plan_hash: pending.plan_hash,
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
        if detail.steps.is_empty() {
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
            retrying_materialization: false,
            // Action authority comes only from the canonical public projection. A detail payload
            // is immutable display data and must never grant actions by itself.
            allowed_actions: Vec::new(),
            revision: None,
            detail: detail.clone(),
            workbench_open: false,
            workbench_scroll: 0,
            workbench_scroll_extent: Default::default(),
            selected_action: PlanWorkbenchAction::Run,
        });
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
        pending.revision = review.revision.clone();
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
}

impl PendingPlanApproval {
    pub(crate) fn action_allowed(&self, action: PlanWorkbenchAction) -> bool {
        let public = match action {
            PlanWorkbenchAction::Run => sigil_kernel::PublicPlanAction::Run,
            PlanWorkbenchAction::Save => sigil_kernel::PublicPlanAction::Save,
            PlanWorkbenchAction::Revise => sigil_kernel::PublicPlanAction::Revise,
            PlanWorkbenchAction::Reject => sigil_kernel::PublicPlanAction::Reject,
        };
        self.allowed_actions.contains(&public)
    }
}

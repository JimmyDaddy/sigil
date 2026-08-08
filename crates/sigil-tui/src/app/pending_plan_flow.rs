use crossterm::event::{KeyCode, KeyEvent};
use sigil_kernel::{PlanApprovalPermission, PlanDraftCreatedEntry, PlanTaskStartMode};

use super::{AppAction, AppState, PendingPlanApproval};

impl AppState {
    pub(in crate::app) fn handle_pending_plan_approval_key_event(
        &mut self,
        key: KeyEvent,
    ) -> Option<Option<AppAction>> {
        self.composer.pending_plan_approval.as_ref()?;
        match key.code {
            KeyCode::Enter if self.composer.input.trim().is_empty() && key.modifiers.is_empty() => {
                Some(self.create_task_from_pending_plan(PlanTaskStartMode::CreateAndRun, None))
            }
            KeyCode::Esc if key.modifiers.is_empty() => Some(self.reject_pending_plan()),
            KeyCode::Char('s')
                if self.composer.input.trim().is_empty() && key.modifiers.is_empty() =>
            {
                Some(self.save_pending_plan())
            }
            KeyCode::Char('r')
                if self.composer.input.trim().is_empty() && key.modifiers.is_empty() =>
            {
                Some(self.revise_pending_plan())
            }
            _ => None,
        }
    }

    fn save_pending_plan(&mut self) -> Option<AppAction> {
        let pending = self.composer.pending_plan_approval.as_ref()?;
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

    fn create_task_from_pending_plan(
        &mut self,
        start_mode: PlanTaskStartMode,
        permission_grant: Option<PlanApprovalPermission>,
    ) -> Option<AppAction> {
        let pending = self.composer.pending_plan_approval.as_ref()?;
        if pending.stale {
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

    pub(crate) fn set_pending_plan_approval_from_draft(
        &mut self,
        draft: &PlanDraftCreatedEntry,
        current_workspace_snapshot_id: Option<&str>,
    ) {
        if draft.steps.is_empty() {
            self.composer.pending_plan_approval = None;
            return;
        }
        let plan_text = draft
            .inline_text
            .clone()
            .unwrap_or_else(|| draft.summary.clone());
        let plan_text = plan_text.trim();
        if plan_text.is_empty() {
            self.composer.pending_plan_approval = None;
            return;
        }
        let steps = draft
            .steps
            .iter()
            .map(|step| step.title.clone())
            .collect::<Vec<_>>();
        let suggested_checks = draft
            .suggested_checks
            .iter()
            .map(|check| {
                std::iter::once(check.command.command.as_str())
                    .chain(check.command.args.iter().map(String::as_str))
                    .collect::<Vec<_>>()
                    .join(" ")
            })
            .collect::<Vec<_>>();
        let stale_reason = sigil_runtime::plan_review_coordinator::plan_handoff_stale_reason(
            draft.workspace_snapshot_id.as_deref(),
            current_workspace_snapshot_id,
        );
        self.composer.pending_plan_approval = Some(PendingPlanApproval {
            plan_id: Some(draft.plan_id.as_str().to_owned()),
            plan_text: plan_text.to_owned(),
            plan_hash: draft.plan_hash.clone(),
            summary: draft.summary.clone(),
            steps,
            target_paths: draft.target_paths.clone(),
            suggested_checks,
            target_path_count: draft.target_paths.len(),
            suggested_check_count: draft.suggested_checks.len(),
            workspace_snapshot_id: draft.workspace_snapshot_id.clone(),
            stale: stale_reason.is_some(),
            stale_reason,
            rendered_text_row_counts: Default::default(),
        });
    }

    pub(in crate::app) fn clear_pending_plan_approval(&mut self) {
        self.composer.pending_plan_approval = None;
    }
}

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
            _ => None,
        }
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

    pub(crate) fn set_pending_plan_approval_from_draft(&mut self, draft: &PlanDraftCreatedEntry) {
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
        });
    }

    pub(in crate::app) fn clear_pending_plan_approval(&mut self) {
        self.composer.pending_plan_approval = None;
    }
}

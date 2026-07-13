use sigil_kernel::{
    EvidenceScope, ReadinessEvaluatedEntry, RequiredAction, SessionLogEntry, TaskRunProjection,
    VerificationCheckRunEntry, VerificationCheckRunStatus, VerificationStateProjection,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct VerificationRecommendation {
    action: VerificationRecommendationAction,
    reason: VerificationRecommendationReason,
}

impl VerificationRecommendation {
    pub(super) fn check_spec_id(&self) -> &str {
        match &self.action {
            VerificationRecommendationAction::Run { check_spec_id }
            | VerificationRecommendationAction::ReRunNonWriting { check_spec_id }
            | VerificationRecommendationAction::Retry { check_spec_id }
            | VerificationRecommendationAction::Approve { check_spec_id } => check_spec_id,
        }
    }

    pub(super) fn requires_approval(&self) -> bool {
        matches!(
            self.action,
            VerificationRecommendationAction::Approve { .. }
        )
    }

    pub(super) fn action_label(&self) -> String {
        match &self.action {
            VerificationRecommendationAction::Run { check_spec_id } => {
                format!("run check {check_spec_id}")
            }
            VerificationRecommendationAction::ReRunNonWriting { check_spec_id } => {
                format!("rerun non-writing check {check_spec_id}")
            }
            VerificationRecommendationAction::Retry { check_spec_id } => {
                format!("retry check {check_spec_id}")
            }
            VerificationRecommendationAction::Approve { check_spec_id } => {
                format!("review check approval {check_spec_id}")
            }
        }
    }

    pub(super) fn reason_label(&self) -> &'static str {
        match self.reason {
            VerificationRecommendationReason::FreshNonWritingEvidence => {
                "a writing check needs fresh non-writing evidence"
            }
            VerificationRecommendationReason::RequiredByCurrentTask => {
                "this trusted check is required by the current task"
            }
            VerificationRecommendationReason::RetryFailedCheck => {
                "the latest result failed for the current task scope"
            }
            VerificationRecommendationReason::RetryInconclusiveCheck => {
                "the latest result is inconclusive for the current task scope"
            }
            VerificationRecommendationReason::ApprovalRequired => {
                "this check needs one-time approval before it can run"
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum VerificationRecommendationAction {
    Run { check_spec_id: String },
    ReRunNonWriting { check_spec_id: String },
    Retry { check_spec_id: String },
    Approve { check_spec_id: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum VerificationRecommendationReason {
    FreshNonWritingEvidence,
    RequiredByCurrentTask,
    RetryFailedCheck,
    RetryInconclusiveCheck,
    ApprovalRequired,
}

pub(super) fn verification_recommendation(
    entries: &[SessionLogEntry],
    task: &TaskRunProjection,
    scope: &EvidenceScope,
    readiness: &ReadinessEvaluatedEntry,
    verification_projection: &VerificationStateProjection,
) -> Option<VerificationRecommendation> {
    let actions = &readiness.evaluation.required_actions;

    for action in actions {
        let RequiredAction::ReRunNonWritingCheck { check_spec_id } = action else {
            continue;
        };
        if trusted_check_is_actionable(
            entries,
            task,
            scope,
            readiness,
            verification_projection,
            check_spec_id,
        ) {
            return Some(VerificationRecommendation {
                action: VerificationRecommendationAction::ReRunNonWriting {
                    check_spec_id: check_spec_id.clone(),
                },
                reason: VerificationRecommendationReason::FreshNonWritingEvidence,
            });
        }
    }

    for action in actions {
        let RequiredAction::RunCheck { check_spec_id } = action else {
            continue;
        };
        if trusted_check_is_actionable(
            entries,
            task,
            scope,
            readiness,
            verification_projection,
            check_spec_id,
        ) {
            return Some(VerificationRecommendation {
                action: VerificationRecommendationAction::Run {
                    check_spec_id: check_spec_id.clone(),
                },
                reason: VerificationRecommendationReason::RequiredByCurrentTask,
            });
        }
    }

    if let Some(run) = latest_retryable_check_run(entries, task, scope, verification_projection) {
        let reason = match run.status {
            VerificationCheckRunStatus::Failed => {
                VerificationRecommendationReason::RetryFailedCheck
            }
            VerificationCheckRunStatus::Inconclusive => {
                VerificationRecommendationReason::RetryInconclusiveCheck
            }
            _ => return None,
        };
        return Some(VerificationRecommendation {
            action: VerificationRecommendationAction::Retry {
                check_spec_id: run.check_spec_id.clone(),
            },
            reason,
        });
    }

    actions.iter().find_map(|action| {
        let RequiredAction::ApproveCheckExecution { check_spec_id } = action else {
            return None;
        };
        Some(VerificationRecommendation {
            action: VerificationRecommendationAction::Approve {
                check_spec_id: check_spec_id.clone(),
            },
            reason: VerificationRecommendationReason::ApprovalRequired,
        })
    })
}

fn trusted_check_is_actionable(
    entries: &[SessionLogEntry],
    task: &TaskRunProjection,
    scope: &EvidenceScope,
    readiness: &ReadinessEvaluatedEntry,
    verification_projection: &VerificationStateProjection,
    check_spec_id: &str,
) -> bool {
    let Some(trusted_check) =
        trusted_check_for_task(verification_projection, task, scope, check_spec_id)
    else {
        return false;
    };
    latest_check_run_for_check(entries, scope, check_spec_id).is_none_or(|run| {
        if run.check_spec_hash != trusted_check.trusted_check.check_spec.check_spec_hash {
            return true;
        }
        match run.status {
            VerificationCheckRunStatus::Queued | VerificationCheckRunStatus::Running => false,
            VerificationCheckRunStatus::Succeeded => {
                !run.receipt_id.as_deref().is_some_and(|receipt_id| {
                    verification_projection
                        .receipt_link(receipt_id)
                        .is_some_and(|link| {
                            link.scope == *scope
                                && readiness.workspace_snapshot_id.as_deref()
                                    == Some(link.workspace_snapshot_id.as_str())
                        })
                })
            }
            VerificationCheckRunStatus::Failed
            | VerificationCheckRunStatus::Skipped
            | VerificationCheckRunStatus::Inconclusive
            | VerificationCheckRunStatus::Errored => true,
        }
    })
}

fn latest_retryable_check_run<'a>(
    entries: &'a [SessionLogEntry],
    task: &TaskRunProjection,
    scope: &EvidenceScope,
    verification_projection: &VerificationStateProjection,
) -> Option<&'a VerificationCheckRunEntry> {
    entries.iter().rev().find_map(|entry| {
        let SessionLogEntry::Control(sigil_kernel::ControlEntry::VerificationCheckRun(run)) = entry
        else {
            return None;
        };
        let latest_run = verification_projection.check_run(&run.run_id)?;
        let trusted_check =
            trusted_check_for_task(verification_projection, task, scope, &run.check_spec_id)?;
        (run.scope == *scope
            && latest_run == run
            && matches!(
                run.status,
                VerificationCheckRunStatus::Failed | VerificationCheckRunStatus::Inconclusive
            )
            && run.check_spec_hash == trusted_check.trusted_check.check_spec.check_spec_hash)
            .then_some(run)
    })
}

pub(super) fn trusted_check_for_task<'a>(
    verification_projection: &'a VerificationStateProjection,
    task: &TaskRunProjection,
    scope: &EvidenceScope,
    check_spec_id: &str,
) -> Option<&'a sigil_kernel::CheckSpecRecordedEntry> {
    verification_projection
        .check_spec(scope, check_spec_id)
        .or_else(|| {
            verification_projection.check_spec(
                &EvidenceScope::Task(task.task_id.as_str().to_owned()),
                check_spec_id,
            )
        })
}

pub(super) fn latest_check_run_for_check<'a>(
    entries: &'a [SessionLogEntry],
    scope: &EvidenceScope,
    check_spec_id: &str,
) -> Option<&'a VerificationCheckRunEntry> {
    entries.iter().rev().find_map(|entry| {
        let SessionLogEntry::Control(sigil_kernel::ControlEntry::VerificationCheckRun(run)) = entry
        else {
            return None;
        };
        (run.scope == *scope && run.check_spec_id == check_spec_id).then_some(run)
    })
}

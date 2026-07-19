use super::*;
use crate::{
    RequiredAction, TaskRunProjection, TaskStateProjection, TaskStepId, TaskStepSpec,
    TaskStepStatus, TaskVerificationRerunRequest,
};

/// One exact action rendered from append-only task and verification state.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", tag = "kind", content = "request")]
pub enum VerificationProductAction {
    Rerun(TaskVerificationRerunRequest),
    ReviewApproval { check_spec_id: CheckSpecId },
}

/// Why the shared product view selected its current recommendation.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum VerificationRecommendationKind {
    Run,
    RerunNonWriting,
    Retry,
    ReviewApproval,
}

/// Bounded evidence links shown by user-facing verification surfaces.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct VerificationProductEvidence {
    pub check_run_id: Option<VerificationCheckRunId>,
    pub check_spec_id: Option<CheckSpecId>,
    pub check_status: Option<VerificationCheckRunStatus>,
    pub receipt_id: Option<ReceiptId>,
    pub workspace_snapshot_id: Option<WorkspaceSnapshotId>,
    pub changeset_id: Option<ChangesetId>,
    pub changeset_apply_event_id: Option<EventId>,
    pub command_event_id: Option<EventId>,
    pub output_artifact_id: Option<ArtifactId>,
    pub failure_summary: Option<String>,
}

/// Shared product projection used by TUI, HTTP, and desktop adapters.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct VerificationProductView {
    pub task_id: String,
    pub step_id: String,
    pub scope: EvidenceScope,
    pub verdict: VerificationVerdict,
    pub status: String,
    pub recommended_check_spec_id: Option<CheckSpecId>,
    pub recommendation_kind: Option<VerificationRecommendationKind>,
    pub recommendation_reason: Option<String>,
    pub action: Option<VerificationProductAction>,
    pub evidence: VerificationProductEvidence,
}

/// Reconstructs the current task verification card without adapter-owned state reduction.
#[must_use]
pub fn verification_product_view(entries: &[SessionLogEntry]) -> Option<VerificationProductView> {
    let tasks = TaskStateProjection::from_entries(entries);
    let verification = VerificationStateProjection::from_entries(entries);
    let task = tasks.latest_task()?;
    let (_, step, _) = focus_step(task)?;
    let scope = step_scope(task, &step.step_id);
    let readiness = verification.latest_readiness(&scope)?;
    let recommendation = recommendation(entries, task, &scope, readiness, &verification);
    let recommended_check_spec_id = recommendation
        .as_ref()
        .map(|recommendation| recommendation.check_spec_id.clone());
    let recommendation_kind = recommendation
        .as_ref()
        .map(|recommendation| recommendation.kind);
    let action = recommendation.as_ref().and_then(|recommendation| {
        if recommendation.kind == VerificationRecommendationKind::ReviewApproval {
            return Some(VerificationProductAction::ReviewApproval {
                check_spec_id: recommendation.check_spec_id.clone(),
            });
        }
        let trusted = trusted_check(&verification, task, &scope, &recommendation.check_spec_id)?;
        Some(VerificationProductAction::Rerun(
            TaskVerificationRerunRequest {
                task_id: task.task_id.clone(),
                step_id: step.step_id.clone(),
                check_spec_id: trusted.trusted_check.check_spec.check_spec_id.clone(),
                check_spec_hash: trusted.trusted_check.check_spec.check_spec_hash.clone(),
                policy_hash: readiness.policy_hash.clone()?,
                workspace_snapshot_id: readiness.workspace_snapshot_id.clone()?,
            },
        ))
    });
    let run = recommended_check_spec_id
        .as_deref()
        .and_then(|check_spec_id| latest_check_run(entries, &scope, check_spec_id))
        .or_else(|| latest_scope_run(entries, &scope));
    let evidence = evidence_view(run, &verification);
    let status = status_label(task, &scope, readiness, run, &verification);
    Some(VerificationProductView {
        task_id: task.task_id.as_str().to_owned(),
        step_id: step.step_id.as_str().to_owned(),
        scope,
        verdict: readiness.evaluation.verification_verdict,
        status,
        recommended_check_spec_id,
        recommendation_kind,
        recommendation_reason: recommendation.map(|value| value.reason.to_owned()),
        action,
        evidence,
    })
}

struct Recommendation {
    check_spec_id: CheckSpecId,
    kind: VerificationRecommendationKind,
    reason: &'static str,
}

fn recommendation(
    entries: &[SessionLogEntry],
    task: &TaskRunProjection,
    scope: &EvidenceScope,
    readiness: &ReadinessEvaluatedEntry,
    projection: &VerificationStateProjection,
) -> Option<Recommendation> {
    for action in &readiness.evaluation.required_actions {
        let RequiredAction::ReRunNonWritingCheck { check_spec_id } = action else {
            continue;
        };
        if trusted_check_is_actionable(entries, task, scope, readiness, projection, check_spec_id) {
            return Some(Recommendation {
                check_spec_id: check_spec_id.clone(),
                kind: VerificationRecommendationKind::RerunNonWriting,
                reason: "a writing check needs fresh non-writing evidence",
            });
        }
    }
    for action in &readiness.evaluation.required_actions {
        let RequiredAction::RunCheck { check_spec_id } = action else {
            continue;
        };
        if trusted_check_is_actionable(entries, task, scope, readiness, projection, check_spec_id) {
            return Some(Recommendation {
                check_spec_id: check_spec_id.clone(),
                kind: VerificationRecommendationKind::Run,
                reason: "this trusted check is required by the current task",
            });
        }
    }
    if let Some(run) = latest_retryable_run(entries, task, scope, projection) {
        let reason = match run.status {
            VerificationCheckRunStatus::Failed => {
                "the latest result failed for the current task scope"
            }
            VerificationCheckRunStatus::Inconclusive => {
                "the latest result is inconclusive for the current task scope"
            }
            _ => return None,
        };
        return Some(Recommendation {
            check_spec_id: run.check_spec_id.clone(),
            kind: VerificationRecommendationKind::Retry,
            reason,
        });
    }
    readiness
        .evaluation
        .required_actions
        .iter()
        .find_map(|action| {
            let RequiredAction::ApproveCheckExecution { check_spec_id } = action else {
                return None;
            };
            Some(Recommendation {
                check_spec_id: check_spec_id.clone(),
                kind: VerificationRecommendationKind::ReviewApproval,
                reason: "this check needs one-time approval before it can run",
            })
        })
}

fn trusted_check_is_actionable(
    entries: &[SessionLogEntry],
    task: &TaskRunProjection,
    scope: &EvidenceScope,
    readiness: &ReadinessEvaluatedEntry,
    projection: &VerificationStateProjection,
    check_spec_id: &str,
) -> bool {
    let Some(trusted) = trusted_check(projection, task, scope, check_spec_id) else {
        return false;
    };
    latest_check_run(entries, scope, check_spec_id).is_none_or(|run| {
        if run.check_spec_hash != trusted.trusted_check.check_spec.check_spec_hash {
            return true;
        }
        match run.status {
            VerificationCheckRunStatus::Queued | VerificationCheckRunStatus::Running => false,
            VerificationCheckRunStatus::Succeeded => {
                !run.receipt_id.as_deref().is_some_and(|receipt_id| {
                    projection.receipt_link(receipt_id).is_some_and(|link| {
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

fn latest_retryable_run<'a>(
    entries: &'a [SessionLogEntry],
    task: &TaskRunProjection,
    scope: &EvidenceScope,
    projection: &VerificationStateProjection,
) -> Option<&'a VerificationCheckRunEntry> {
    entries.iter().rev().find_map(|entry| {
        let SessionLogEntry::Control(ControlEntry::VerificationCheckRun(run)) = entry else {
            return None;
        };
        let latest = projection.check_run(&run.run_id)?;
        let trusted = trusted_check(projection, task, scope, &run.check_spec_id)?;
        (run.scope == *scope
            && latest == run
            && matches!(
                run.status,
                VerificationCheckRunStatus::Failed | VerificationCheckRunStatus::Inconclusive
            )
            && run.check_spec_hash == trusted.trusted_check.check_spec.check_spec_hash)
            .then_some(run)
    })
}

fn trusted_check<'a>(
    projection: &'a VerificationStateProjection,
    task: &TaskRunProjection,
    scope: &EvidenceScope,
    check_spec_id: &str,
) -> Option<&'a CheckSpecRecordedEntry> {
    projection.check_spec(scope, check_spec_id).or_else(|| {
        projection.check_spec(
            &EvidenceScope::Task(task.task_id.as_str().to_owned()),
            check_spec_id,
        )
    })
}

fn latest_check_run<'a>(
    entries: &'a [SessionLogEntry],
    scope: &EvidenceScope,
    check_spec_id: &str,
) -> Option<&'a VerificationCheckRunEntry> {
    entries.iter().rev().find_map(|entry| {
        let SessionLogEntry::Control(ControlEntry::VerificationCheckRun(run)) = entry else {
            return None;
        };
        (run.scope == *scope && run.check_spec_id == check_spec_id).then_some(run)
    })
}

fn latest_scope_run<'a>(
    entries: &'a [SessionLogEntry],
    scope: &EvidenceScope,
) -> Option<&'a VerificationCheckRunEntry> {
    entries.iter().rev().find_map(|entry| {
        let SessionLogEntry::Control(ControlEntry::VerificationCheckRun(run)) = entry else {
            return None;
        };
        (run.scope == *scope).then_some(run)
    })
}

fn evidence_view(
    run: Option<&VerificationCheckRunEntry>,
    projection: &VerificationStateProjection,
) -> VerificationProductEvidence {
    let Some(run) = run else {
        return VerificationProductEvidence::default();
    };
    let locator = projection.failure_locator(&run.run_id);
    let link = run
        .receipt_id
        .as_deref()
        .and_then(|receipt_id| projection.receipt_link(receipt_id));
    VerificationProductEvidence {
        check_run_id: Some(run.run_id.clone()),
        check_spec_id: Some(run.check_spec_id.clone()),
        check_status: Some(run.status),
        receipt_id: run.receipt_id.clone(),
        workspace_snapshot_id: link.map(|value| value.workspace_snapshot_id.clone()),
        changeset_id: link.and_then(|value| value.changeset_id.clone()),
        changeset_apply_event_id: link.and_then(|value| value.changeset_apply_event_id.clone()),
        command_event_id: locator.and_then(|value| value.command_event_id.clone()),
        output_artifact_id: locator.and_then(|value| value.output_artifact_id.clone()),
        failure_summary: locator.map(|value| value.summary.clone()),
    }
}

fn status_label(
    task: &TaskRunProjection,
    scope: &EvidenceScope,
    readiness: &ReadinessEvaluatedEntry,
    run: Option<&VerificationCheckRunEntry>,
    projection: &VerificationStateProjection,
) -> String {
    let verdict = verdict_label(readiness.evaluation.verification_verdict);
    let Some(run) = run else {
        return verdict.to_owned();
    };
    let Some(trusted) = trusted_check(projection, task, scope, &run.check_spec_id) else {
        return verdict.to_owned();
    };
    if run.check_spec_hash != trusted.trusted_check.check_spec.check_spec_hash {
        return verdict.to_owned();
    }
    if run.status == VerificationCheckRunStatus::Succeeded {
        let current = run.receipt_id.as_deref().is_some_and(|receipt_id| {
            projection.receipt_link(receipt_id).is_some_and(|link| {
                link.scope == *scope
                    && readiness.workspace_snapshot_id.as_deref()
                        == Some(link.workspace_snapshot_id.as_str())
            })
        });
        return if current { "passed" } else { verdict }.to_owned();
    }
    format!("check {}", check_status_label(run.status))
}

fn verdict_label(verdict: VerificationVerdict) -> &'static str {
    match verdict {
        VerificationVerdict::Passed => "passed",
        VerificationVerdict::Failed => "failed",
        VerificationVerdict::Pending => "pending",
        VerificationVerdict::Missing => "missing",
        VerificationVerdict::Stale => "stale",
        VerificationVerdict::Inconclusive => "inconclusive",
        VerificationVerdict::Skipped => "skipped",
        VerificationVerdict::NotApplicable => "not applicable",
        VerificationVerdict::NotEvaluated => "not evaluated",
    }
}

fn check_status_label(status: VerificationCheckRunStatus) -> &'static str {
    match status {
        VerificationCheckRunStatus::Queued => "queued",
        VerificationCheckRunStatus::Running => "running",
        VerificationCheckRunStatus::Succeeded => "passed",
        VerificationCheckRunStatus::Failed => "failed",
        VerificationCheckRunStatus::Skipped => "skipped",
        VerificationCheckRunStatus::Inconclusive => "inconclusive",
        VerificationCheckRunStatus::Errored => "errored",
    }
}

fn focus_step(task: &TaskRunProjection) -> Option<(u32, &TaskStepSpec, TaskStepStatus)> {
    if let Some((plan_version, step_id)) = &task.current_step {
        let plan = task.plans.get(plan_version)?;
        let step = plan.steps.iter().find(|step| step.step_id == *step_id)?;
        return Some((*plan_version, step, step_status(task, *plan_version, step)));
    }
    last_problem_step(task).or_else(|| last_plan_step(task))
}

fn last_plan_step(task: &TaskRunProjection) -> Option<(u32, &TaskStepSpec, TaskStepStatus)> {
    let version = task.latest_plan_version?;
    let plan = task.plans.get(&version)?;
    let step = plan.steps.last()?;
    Some((version, step, step_status(task, version, step)))
}

fn last_problem_step(task: &TaskRunProjection) -> Option<(u32, &TaskStepSpec, TaskStepStatus)> {
    let version = task.latest_plan_version?;
    let plan = task.plans.get(&version)?;
    plan.steps.iter().find_map(|step| {
        let status = step_status(task, version, step);
        matches!(
            status,
            TaskStepStatus::Failed
                | TaskStepStatus::Blocked
                | TaskStepStatus::Interrupted
                | TaskStepStatus::Cancelled
        )
        .then_some((version, step, status))
    })
}

fn step_status(task: &TaskRunProjection, version: u32, step: &TaskStepSpec) -> TaskStepStatus {
    task.steps
        .get(&(version, step.step_id.clone()))
        .map(|value| value.status)
        .unwrap_or(TaskStepStatus::Pending)
}

fn step_scope(task: &TaskRunProjection, step_id: &TaskStepId) -> EvidenceScope {
    EvidenceScope::Step(format!("{}:{}", task.task_id.as_str(), step_id.as_str()))
}

#[cfg(test)]
#[path = "../tests/verification_product_tests.rs"]
mod tests;

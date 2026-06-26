use sigil_kernel::{
    AgentRole, ControlEntry, EvidenceScope, ModelMessage, ReadinessEvaluatedEntry,
    ReadinessEvaluation, RequiredAction, RunStatus, SessionLogEntry, SessionRef, TaskId,
    TaskPlanEntry, TaskPlanStatus, TaskRunEntry, TaskRunStatus, TaskStepEntry, TaskStepId,
    TaskStepSpec, TaskStepStatus, VerificationVerdict, VisibleCompletionState,
};

use super::{
    required_action_label, task_sidebar_lines, task_step_status_label, task_strip_view,
    verification_verdict_label,
};

#[test]
fn verification_labels_cover_all_sidebar_variants() {
    for (status, label) in [
        (TaskStepStatus::Pending, "pending"),
        (TaskStepStatus::Running, "running"),
        (TaskStepStatus::Completed, "completed"),
        (TaskStepStatus::Failed, "failed"),
        (TaskStepStatus::Blocked, "blocked"),
        (TaskStepStatus::Cancelled, "cancelled"),
        (TaskStepStatus::Interrupted, "interrupted"),
    ] {
        assert_eq!(task_step_status_label(status), label);
    }
    for (verdict, label) in [
        (VerificationVerdict::NotEvaluated, "not evaluated"),
        (VerificationVerdict::NotApplicable, "not applicable"),
        (VerificationVerdict::Pending, "pending"),
        (VerificationVerdict::Passed, "passed"),
        (VerificationVerdict::Failed, "failed"),
        (VerificationVerdict::Missing, "missing"),
        (VerificationVerdict::Inconclusive, "inconclusive"),
        (VerificationVerdict::Stale, "stale"),
        (VerificationVerdict::Skipped, "skipped"),
    ] {
        assert_eq!(verification_verdict_label(verdict), label);
    }
    for (action, expected) in [
        (
            RequiredAction::RunCheck {
                check_spec_id: "check-a".to_owned(),
            },
            "run_check check-a",
        ),
        (
            RequiredAction::ApproveCheckExecution {
                check_spec_id: "check-a".to_owned(),
            },
            "approve_check check-a",
        ),
        (RequiredAction::TrustWorkspace, "trust_workspace"),
        (RequiredAction::ResolveUnknownDirty, "resolve_unknown_dirty"),
        (
            RequiredAction::ReRunNonWritingCheck {
                check_spec_id: "check-a".to_owned(),
            },
            "rerun_non_writing_check check-a",
        ),
        (
            RequiredAction::ReviewVerificationFailure {
                receipt_id: "receipt-a".to_owned(),
            },
            "review_verification_failure receipt-a",
        ),
        (
            RequiredAction::ProvideVerificationConfig,
            "provide_verification_config",
        ),
    ] {
        assert_eq!(required_action_label(&action), expected);
    }
}

#[test]
fn task_sidebar_projects_completed_task_with_verification_actions() {
    let completed_entries = task_entries_with_readiness(
        TaskRunStatus::Completed,
        TaskStepStatus::Completed,
        VerificationVerdict::Missing,
    );

    let lines = task_sidebar_lines(&completed_entries);

    assert!(lines.iter().any(|line| line == "status: completed"));
    assert!(
        lines
            .iter()
            .any(|line| line == "last: v1:fix_typo needs check")
    );

    let blocked_entries = task_entries_with_readiness(
        TaskRunStatus::Paused,
        TaskStepStatus::Blocked,
        VerificationVerdict::Missing,
    );
    let lines = task_sidebar_lines(&blocked_entries);
    assert!(lines.iter().any(|line| line == "verification: missing"));
    assert!(
        lines
            .iter()
            .any(|line| line == "action: run_check docs-check")
    );

    let strip = task_strip_view(&blocked_entries).expect("task strip should project");
    assert!(strip.detail.contains("missing"));
    assert_eq!(strip.rows[0].label, "1. needs check · Fix typo");
}

#[test]
fn task_sidebar_keeps_plain_completed_label_without_verification_action() {
    let completed_entries = task_entries_with_custom_readiness(
        TaskRunStatus::Completed,
        TaskStepStatus::Completed,
        VerificationVerdict::Passed,
        VisibleCompletionState::Verified,
        Vec::new(),
    );

    let lines = task_sidebar_lines(&completed_entries);

    assert!(
        lines
            .iter()
            .any(|line| line == "last: v1:fix_typo completed")
    );
    assert!(!lines.iter().any(|line| line.starts_with("verification:")));
    let strip = task_strip_view(&completed_entries).expect("task strip should project");
    assert_eq!(strip.rows[0].label, "1. Fix typo");
}

#[test]
fn task_sidebar_marks_non_user_state_missing_verification_as_needing_check() {
    let entries = task_entries_with_custom_readiness(
        TaskRunStatus::Completed,
        TaskStepStatus::Completed,
        VerificationVerdict::Missing,
        VisibleCompletionState::CompletedUnverified,
        Vec::new(),
    );

    let strip = task_strip_view(&entries).expect("task strip should project");

    assert_eq!(strip.rows[0].label, "1. needs check · Fix typo");
}

fn task_entries_with_readiness(
    run_status: TaskRunStatus,
    step_status: TaskStepStatus,
    verdict: VerificationVerdict,
) -> Vec<SessionLogEntry> {
    task_entries_with_custom_readiness(
        run_status,
        step_status,
        verdict,
        VisibleCompletionState::NeedsUser,
        vec![RequiredAction::RunCheck {
            check_spec_id: "docs-check".to_owned(),
        }],
    )
}

fn task_entries_with_custom_readiness(
    run_status: TaskRunStatus,
    step_status: TaskStepStatus,
    verdict: VerificationVerdict,
    visible_state: VisibleCompletionState,
    required_actions: Vec<RequiredAction>,
) -> Vec<SessionLogEntry> {
    let task_id = TaskId::new("task_1").expect("task id");
    let step_id = TaskStepId::new("fix_typo").expect("step id");
    vec![
        SessionLogEntry::User(ModelMessage::user("/task fix typo")),
        SessionLogEntry::Control(ControlEntry::TaskRun(TaskRunEntry {
            task_id: task_id.clone(),
            parent_session_ref: SessionRef::new_relative("parent.jsonl").expect("session ref"),
            objective: "Fix typo".to_owned(),
            status: run_status,
            reason: None,
        })),
        SessionLogEntry::Control(ControlEntry::TaskPlan(TaskPlanEntry {
            task_id: task_id.clone(),
            plan_version: 1,
            status: TaskPlanStatus::Accepted,
            steps: vec![TaskStepSpec {
                step_id: step_id.clone(),
                title: "Fix typo".to_owned(),
                display_name: None,
                detail: None,
                role: AgentRole::Executor,
            }],
            reason: None,
        })),
        SessionLogEntry::Control(ControlEntry::TaskStep(TaskStepEntry {
            task_id: task_id.clone(),
            plan_version: 1,
            step_id: step_id.clone(),
            role: AgentRole::Executor,
            status: step_status,
            title: Some("Fix typo".to_owned()),
            summary: Some("done".to_owned()),
            reason: None,
        })),
        SessionLogEntry::Control(ControlEntry::ReadinessEvaluated(ReadinessEvaluatedEntry {
            scope: EvidenceScope::Step("task_1:fix_typo".to_owned()),
            evaluation: ReadinessEvaluation {
                run_status: RunStatus::Completed,
                verification_verdict: verdict,
                visible_state,
                reasons: Vec::new(),
                required_actions,
            },
            policy_hash: None,
            workspace_snapshot_id: Some("snapshot-1".to_owned()),
        })),
    ]
}

use sigil_kernel::{
    AgentRole, ChangeSetId, CheckCommand, CheckDiscoverySource, CheckPromotion, CheckSpec,
    CheckSpecRecordedEntry, ControlEntry, EvidenceScope, MergeDecision, MergeReviewId,
    MergeReviewRequested, MergeReviewResolved, ModelMessage, ReadinessEvaluatedEntry,
    ReadinessEvaluation, ReadinessReason, RequiredAction, RunStatus, SessionLogEntry, SessionRef,
    TaskChildSessionDisplayNameEntry, TaskChildSessionEntry, TaskChildSessionStatus, TaskId,
    TaskIsolationMode, TaskPlanEntry, TaskPlanStatus, TaskRunEntry, TaskRunStatus, TaskStepEntry,
    TaskStepId, TaskStepMode, TaskStepSpec, TaskStepStatus, ToolEffect, TrustedCheckSpec,
    VerificationCheckRunEntry, VerificationCheckRunStatus, VerificationFailureLocatorRecorded,
    VerificationReceiptLinkRecorded, VerificationStaleCause, VerificationStaleReason,
    VerificationVerdict, VisibleCompletionState,
};

use super::{
    readiness_reason_summary, required_action_label, task_sidebar_lines, task_step_status_label,
    task_strip_view, verification_stale_reason_compact_label, verification_verdict_label,
};
use crate::app::task_sidebar::VerificationCardAction;

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
        (TaskStepStatus::Superseded, "superseded"),
    ] {
        assert_eq!(task_step_status_label(status), label);
    }
    for (verdict, label) in [
        (VerificationVerdict::NotEvaluated, "not evaluated"),
        (VerificationVerdict::NotApplicable, "not applicable"),
        (VerificationVerdict::Pending, "pending"),
        (VerificationVerdict::Passed, "passed"),
        (VerificationVerdict::Failed, "check failed"),
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
            "run check check-a",
        ),
        (
            RequiredAction::ApproveCheckExecution {
                check_spec_id: "check-a".to_owned(),
            },
            "check approval check-a",
        ),
        (RequiredAction::TrustWorkspace, "workspace trust required"),
        (
            RequiredAction::ResolveUnknownDirty,
            "refresh source or run check",
        ),
        (
            RequiredAction::ReRunNonWritingCheck {
                check_spec_id: "check-a".to_owned(),
            },
            "rerun non-writing check check-a",
        ),
        (
            RequiredAction::ReviewVerificationFailure {
                receipt_id: "receipt-a".to_owned(),
            },
            "review verification failure receipt-a",
        ),
        (
            RequiredAction::ProvideVerificationConfig,
            "verification config required",
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
    assert!(lines.iter().any(|line| line == "run: completed"));
    assert!(lines.iter().any(|line| line == "verification: missing"));
    assert!(
        lines
            .iter()
            .any(|line| line == "action: run check docs-check")
    );

    let strip = task_strip_view(&blocked_entries).expect("task strip should project");
    assert!(strip.detail.contains("missing"));
    assert_eq!(strip.rows[0].label, "1. needs check · Fix typo");
}

#[test]
fn task_sidebar_compacts_multiple_verification_reasons() {
    let entries = task_entries_with_custom_readiness_and_reasons(
        TaskRunStatus::Paused,
        TaskStepStatus::Blocked,
        VerificationVerdict::Stale,
        VisibleCompletionState::NeedsUser,
        vec![RequiredAction::ResolveUnknownDirty],
        vec![
            ReadinessReason::WorkspaceMutationSource {
                event_id: "event-mcp".to_owned(),
                source_label: "MCP server docs".to_owned(),
                recovery_hint: Some("refresh MCP or run check".to_owned()),
            },
            ReadinessReason::VerificationStale(VerificationStaleCause {
                reason: VerificationStaleReason::WorkspaceChanged("event-workspace".to_owned()),
                from_workspace_snapshot_id: Some("snapshot-before".to_owned()),
                to_workspace_snapshot_id: Some("snapshot-after".to_owned()),
            }),
            ReadinessReason::VerificationStale(VerificationStaleCause {
                reason: VerificationStaleReason::PolicyChanged("event-policy".to_owned()),
                from_workspace_snapshot_id: None,
                to_workspace_snapshot_id: None,
            }),
            ReadinessReason::WorkspaceUnknownDirty {
                event_id: Some("event-unknown".to_owned()),
            },
        ],
    );

    let lines = task_sidebar_lines(&entries);

    assert!(lines.iter().any(|line| line == "verification: stale"));
    assert!(lines.iter().any(|line| {
        line.starts_with("verification reason: MCP server docs: refresh MCP")
            && line.contains("+3 more")
    }));
    let strip = task_strip_view(&entries).expect("task strip should project");
    assert!(strip.detail.contains("MCP server docs"));
    assert!(strip.detail.contains("+3 more"));
}

#[test]
fn task_sidebar_surfaces_child_merge_recheck_trace() {
    let mut entries = task_entries_with_custom_readiness_and_reasons(
        TaskRunStatus::Paused,
        TaskStepStatus::Blocked,
        VerificationVerdict::Stale,
        VisibleCompletionState::NeedsUser,
        vec![RequiredAction::RunCheck {
            check_spec_id: "docs-check".to_owned(),
        }],
        vec![ReadinessReason::VerificationStale(VerificationStaleCause {
            reason: VerificationStaleReason::WorkspaceChanged("merge-event".to_owned()),
            from_workspace_snapshot_id: Some("parent-before".to_owned()),
            to_workspace_snapshot_id: Some("parent-after".to_owned()),
        })],
    );
    let child_task_id = TaskId::new("child_1").expect("child task id");
    entries.push(SessionLogEntry::Control(ControlEntry::TaskChildSession(
        TaskChildSessionEntry {
            task_id: TaskId::new("task_1").expect("task id"),
            plan_version: 1,
            step_id: TaskStepId::new("fix_typo").expect("step id"),
            child_task_id: child_task_id.clone(),
            child_session_ref: SessionRef::new_relative("children/task_1/fix_typo-child_1.jsonl")
                .expect("child session ref"),
            role: AgentRole::SubagentRead,
            status: TaskChildSessionStatus::Completed,
            summary_hash: None,
        },
    )));
    entries.push(SessionLogEntry::Control(
        ControlEntry::TaskChildSessionDisplayName(TaskChildSessionDisplayNameEntry {
            task_id: TaskId::new("task_1").expect("task id"),
            plan_version: 1,
            step_id: TaskStepId::new("fix_typo").expect("step id"),
            child_task_id,
            display_name: "Review Agent".to_owned(),
        }),
    ));
    entries.push(SessionLogEntry::Control(
        ControlEntry::ChildVerificationReceiptLinked(
            sigil_kernel::ChildVerificationReceiptLinked {
                parent_session_id: "parent-session".to_owned(),
                child_session_id: "child_1".to_owned(),
                child_receipt_id: "child-receipt".to_owned(),
                child_event_id: "child-event".to_owned(),
                child_workspace_id: "child-workspace".to_owned(),
                child_workspace_snapshot_id: "child-snapshot".to_owned(),
                policy_hash: "policy-hash".to_owned(),
                changeset_id: Some("changeset-1".to_owned()),
                merge_event_id: Some("merge-event".to_owned()),
            },
        ),
    ));

    let lines = task_sidebar_lines(&entries);

    assert!(
        lines
            .iter()
            .any(|line| { line == "merge: Review Agent completed; run parent check" })
    );
    assert!(
        lines
            .iter()
            .any(|line| line == "action: run check docs-check")
    );
    let strip = task_strip_view(&entries).expect("task strip should project");
    assert!(strip.detail.contains("Review Agent completed"));
    assert!(strip.detail.contains("run parent check"));
}

#[test]
fn task_sidebar_surfaces_pending_merge_review_as_single_action() {
    let mut entries =
        task_entries_without_readiness(TaskRunStatus::Paused, TaskStepStatus::Blocked);
    append_merge_review(&mut entries, None);

    let lines = task_sidebar_lines(&entries);

    assert!(
        lines
            .iter()
            .any(|line| line == "merge: changeset changeset-1 ready; review changes")
    );
    assert!(
        lines
            .iter()
            .any(|line| line == "action: review changeset changeset-1")
    );
    assert_eq!(
        lines
            .iter()
            .filter(|line| line.starts_with("action:"))
            .count(),
        1
    );

    let strip = task_strip_view(&entries).expect("task strip should project");
    assert!(strip.detail.contains("review changes"));
}

#[test]
fn task_sidebar_surfaces_accepted_merge_review_as_parent_recheck() {
    let mut entries =
        task_entries_without_readiness(TaskRunStatus::Paused, TaskStepStatus::Blocked);
    append_merge_review(&mut entries, Some(MergeDecision::Accepted));

    let lines = task_sidebar_lines(&entries);

    assert!(
        lines
            .iter()
            .any(|line| line == "merge: changeset changeset-1 accepted; run parent check")
    );
    assert!(lines.iter().any(|line| line == "action: run parent check"));

    let strip = task_strip_view(&entries).expect("task strip should project");
    assert!(strip.detail.contains("run parent check"));
}

#[test]
fn task_sidebar_surfaces_conflict_and_rejected_merge_review_states() {
    let mut conflict_entries =
        task_entries_without_readiness(TaskRunStatus::Paused, TaskStepStatus::Blocked);
    append_merge_review(&mut conflict_entries, Some(MergeDecision::Conflict));

    let conflict_lines = task_sidebar_lines(&conflict_entries);

    assert!(
        conflict_lines
            .iter()
            .any(|line| line == "merge: changeset changeset-1 conflict; resolve conflict")
    );
    assert!(
        conflict_lines
            .iter()
            .any(|line| line == "action: resolve conflict changeset-1")
    );
    assert_eq!(
        conflict_lines
            .iter()
            .filter(|line| line.starts_with("action:"))
            .count(),
        1
    );

    let mut rejected_entries =
        task_entries_without_readiness(TaskRunStatus::Paused, TaskStepStatus::Blocked);
    append_merge_review(&mut rejected_entries, Some(MergeDecision::Rejected));

    let rejected_lines = task_sidebar_lines(&rejected_entries);

    assert!(
        rejected_lines
            .iter()
            .any(|line| line == "merge: changeset changeset-1 rejected; no parent changes")
    );
    assert!(
        rejected_lines
            .iter()
            .all(|line| !line.starts_with("action:"))
    );
}

#[test]
fn task_sidebar_compact_labels_cover_verification_reason_edges() {
    assert_eq!(
        readiness_reason_summary(
            &[
                ReadinessReason::WorkspaceMutationSource {
                    event_id: "event-mcp".to_owned(),
                    source_label: "MCP server docs".to_owned(),
                    recovery_hint: Some("refresh MCP or run check".to_owned()),
                },
                ReadinessReason::WorkspaceUnknownDirty { event_id: None },
                ReadinessReason::CheckMutatedVerificationScope {
                    check_spec_id: "long-check-that-changed-files".to_owned(),
                },
            ],
            7,
        ),
        Some("MCP ser...".to_owned())
    );
    assert_eq!(
        readiness_reason_summary(
            &[ReadinessReason::CheckMutatedVerificationScope {
                check_spec_id: "docs-check".to_owned(),
            }],
            48,
        ),
        Some("check changed files docs-check".to_owned())
    );
    let scope_mismatch = readiness_reason_summary(
        &[ReadinessReason::ReceiptScopeMismatch {
            receipt_id: "receipt-scope-mismatch".to_owned(),
        }],
        48,
    )
    .expect("scope mismatch should summarize");
    assert!(scope_mismatch.starts_with("scope mismatch receipt-scope-mi"));
    assert!(scope_mismatch.ends_with("..."));
    let snapshot_mismatch = readiness_reason_summary(
        &[ReadinessReason::ReceiptSnapshotMismatch {
            receipt_id: "receipt-snapshot-mismatch".to_owned(),
        }],
        48,
    )
    .expect("snapshot mismatch should summarize");
    assert!(snapshot_mismatch.starts_with("snapshot mismatch receipt-snap"));
    assert!(snapshot_mismatch.ends_with("..."));
    assert_eq!(
        readiness_reason_summary(&[ReadinessReason::NoVerificationRequired], 48),
        None
    );

    for (reason, expected) in [
        (
            VerificationStaleReason::CheckSpecChanged("event-check".to_owned()),
            "check spec changed event-check",
        ),
        (
            VerificationStaleReason::EnvironmentChanged("event-env".to_owned()),
            "environment changed event-env",
        ),
        (
            VerificationStaleReason::SandboxChanged("event-sandbox".to_owned()),
            "sandbox changed event-sandbox",
        ),
        (
            VerificationStaleReason::TrustChanged("event-trust".to_owned()),
            "workspace trust changed event-trust",
        ),
        (
            VerificationStaleReason::UnknownDirty("event-dirty".to_owned()),
            "unknown workspace change event-dirty",
        ),
    ] {
        assert_eq!(verification_stale_reason_compact_label(&reason), expected);
    }
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
fn task_sidebar_separates_review_advisory_from_system_verify() {
    let task_id = TaskId::new("task_1").expect("task id");
    let review_step_id = TaskStepId::new("review").expect("step id");
    let verify_step_id = TaskStepId::new("verify").expect("step id");
    let entries = vec![
        SessionLogEntry::Control(ControlEntry::TaskRun(TaskRunEntry {
            task_id: task_id.clone(),
            parent_session_ref: SessionRef::new_relative("parent.jsonl").expect("session ref"),
            objective: "Review then verify".to_owned(),
            status: TaskRunStatus::Completed,
            reason: None,
        })),
        SessionLogEntry::Control(ControlEntry::TaskPlan(TaskPlanEntry {
            task_id: task_id.clone(),
            plan_version: 1,
            status: TaskPlanStatus::Accepted,
            steps: vec![
                TaskStepSpec {
                    step_id: review_step_id.clone(),
                    title: "Review changes".to_owned(),
                    display_name: None,
                    detail: None,
                    role: AgentRole::SubagentRead,
                    depends_on: Vec::new(),
                    mode: Some(TaskStepMode::Review),
                    isolation: Some(TaskIsolationMode::SharedReadOnly),
                },
                TaskStepSpec {
                    step_id: verify_step_id.clone(),
                    title: "Verify changes".to_owned(),
                    display_name: None,
                    detail: None,
                    role: AgentRole::Executor,
                    depends_on: vec![review_step_id],
                    mode: Some(TaskStepMode::Verify),
                    isolation: Some(TaskIsolationMode::SharedReadOnly),
                },
            ],
            reason: None,
        })),
        SessionLogEntry::Control(ControlEntry::TaskStep(TaskStepEntry {
            task_id: task_id.clone(),
            plan_version: 1,
            step_id: TaskStepId::new("review").expect("step id"),
            role: AgentRole::SubagentRead,
            status: TaskStepStatus::Completed,
            title: Some("Review changes".to_owned()),
            summary: Some("advisory review complete".to_owned()),
            reason: None,
        })),
        SessionLogEntry::Control(ControlEntry::TaskStep(TaskStepEntry {
            task_id,
            plan_version: 1,
            step_id: verify_step_id,
            role: AgentRole::Executor,
            status: TaskStepStatus::Completed,
            title: Some("Verify changes".to_owned()),
            summary: Some("agent says verified".to_owned()),
            reason: None,
        })),
    ];

    let lines = task_sidebar_lines(&entries);
    assert!(
        lines
            .iter()
            .any(|line| line == "last: v1:verify needs check")
    );
    assert!(
        lines
            .iter()
            .any(|line| line.contains("reviewed review · Review changes"))
    );

    let strip = task_strip_view(&entries).expect("task strip should project");
    assert_eq!(strip.rows[0].label, "1. reviewed · Review changes");
    assert_eq!(strip.rows[1].label, "2. needs check · Verify changes");
    assert!(strip.rows[1].detail.contains("verify · verify"));
}

#[test]
fn task_sidebar_keeps_non_actionable_missing_verification_completed() {
    let entries = task_entries_with_custom_readiness(
        TaskRunStatus::Completed,
        TaskStepStatus::Completed,
        VerificationVerdict::Missing,
        VisibleCompletionState::CompletedUnverified,
        Vec::new(),
    );

    let strip = task_strip_view(&entries).expect("task strip should project");

    assert_eq!(strip.detail, "completed · v1 · 1/1 done");
    assert_eq!(strip.rows[0].kind, crate::ui::StatusKind::Success);
    assert_eq!(strip.rows[0].label, "1. Fix typo");
    assert_eq!(strip.rows[0].detail, "completed · fix_typo");
}

#[test]
fn task_sidebar_explains_workspace_trust_required_action() {
    let entries = task_entries_with_custom_readiness(
        TaskRunStatus::Paused,
        TaskStepStatus::Blocked,
        VerificationVerdict::Missing,
        VisibleCompletionState::NeedsUser,
        vec![RequiredAction::TrustWorkspace],
    );

    let lines = task_sidebar_lines(&entries);

    assert!(lines.iter().any(|line| line == "workspace trust: required"));
    assert!(
        lines
            .iter()
            .any(|line| line == "action: workspace trust required")
    );
    let strip = task_strip_view(&entries).expect("task strip should project");
    assert!(strip.detail.contains("workspace trust required"));
}

#[test]
fn task_sidebar_explains_check_execution_approval_required_action() {
    let entries = task_entries_with_custom_readiness(
        TaskRunStatus::Paused,
        TaskStepStatus::Blocked,
        VerificationVerdict::Missing,
        VisibleCompletionState::NeedsUser,
        vec![RequiredAction::ApproveCheckExecution {
            check_spec_id: "repo-make-check".to_owned(),
        }],
    );

    let lines = task_sidebar_lines(&entries);

    assert!(
        lines
            .iter()
            .any(|line| line == "check approval: repo-make-check")
    );
    assert!(
        lines
            .iter()
            .any(|line| line == "action: check approval repo-make-check")
    );
    let strip = task_strip_view(&entries).expect("task strip should project");
    assert!(strip.detail.contains("check approval repo-make-check"));
}

#[test]
fn task_sidebar_shows_latest_check_runner_state_for_required_action() {
    let mut entries = task_entries_with_readiness(
        TaskRunStatus::Paused,
        TaskStepStatus::Blocked,
        VerificationVerdict::Missing,
    );
    entries.push(SessionLogEntry::Control(
        ControlEntry::VerificationCheckRun(verification_check_run(
            "run-1",
            VerificationCheckRunStatus::Queued,
            None,
        )),
    ));
    entries.push(SessionLogEntry::Control(
        ControlEntry::VerificationCheckRun(verification_check_run(
            "run-1",
            VerificationCheckRunStatus::Running,
            None,
        )),
    ));

    let lines = task_sidebar_lines(&entries);

    assert!(
        lines
            .iter()
            .any(|line| line == "check: docs-check running timeout=5000 ms")
    );
    assert!(
        !lines
            .iter()
            .any(|line| line == "action: run check docs-check")
    );
    assert!(!lines.iter().any(|line| line.starts_with("recommended:")));

    let strip = task_strip_view(&entries).expect("task strip should project");
    assert!(strip.detail.contains("check running timeout=5000 ms"));
}

#[test]
fn task_sidebar_shows_check_runner_failure_reason() {
    let mut entries = task_entries_with_readiness(
        TaskRunStatus::Paused,
        TaskStepStatus::Blocked,
        VerificationVerdict::Missing,
    );
    entries.push(SessionLogEntry::Control(
        ControlEntry::VerificationCheckRun(verification_check_run(
            "run-1",
            VerificationCheckRunStatus::Errored,
            Some("command timed out"),
        )),
    ));

    let lines = task_sidebar_lines(&entries);

    assert!(
        lines
            .iter()
            .any(|line| line == "check: docs-check errored timeout=5000 ms")
    );
    assert!(
        lines
            .iter()
            .any(|line| line == "check reason: command timed out")
    );
    assert!(
        lines
            .iter()
            .any(|line| line == "action: run check docs-check")
    );
}

#[test]
fn task_sidebar_keeps_retry_action_after_terminal_check_failure() {
    let mut entries = task_entries_with_readiness(
        TaskRunStatus::Paused,
        TaskStepStatus::Blocked,
        VerificationVerdict::Missing,
    );
    entries.push(SessionLogEntry::Control(
        ControlEntry::VerificationCheckRun(verification_check_run(
            "run-1",
            VerificationCheckRunStatus::Failed,
            Some("tests failed"),
        )),
    ));

    let lines = task_sidebar_lines(&entries);

    assert!(
        lines
            .iter()
            .any(|line| line == "check: docs-check failed timeout=5000 ms")
    );
    assert!(
        lines
            .iter()
            .any(|line| line == "action: run check docs-check")
    );
}

#[test]
fn task_sidebar_recommends_current_trusted_required_check() {
    let entries = task_entries_with_readiness(
        TaskRunStatus::Paused,
        TaskStepStatus::Blocked,
        VerificationVerdict::Missing,
    );

    let lines = task_sidebar_lines(&entries);

    assert!(
        lines
            .iter()
            .any(|line| line == "recommended: run check docs-check")
    );
    assert!(lines.iter().any(|line| {
        line == "recommended why: this trusted check is required by the current task"
    }));
}

#[test]
fn task_verification_card_binds_exact_rerun_request_and_failure_evidence() {
    let mut entries = task_entries_with_readiness(
        TaskRunStatus::Paused,
        TaskStepStatus::Blocked,
        VerificationVerdict::Failed,
    );
    for entry in &mut entries {
        if let SessionLogEntry::Control(ControlEntry::ReadinessEvaluated(readiness)) = entry {
            readiness.policy_hash = Some("policy-hash".to_owned());
        }
    }
    let mut run = verification_check_run(
        "run-1",
        VerificationCheckRunStatus::Failed,
        Some("tests failed"),
    );
    run.receipt_id = Some("receipt-1".to_owned());
    entries.push(SessionLogEntry::Control(
        ControlEntry::VerificationCheckRun(run),
    ));
    entries.push(SessionLogEntry::Control(
        ControlEntry::VerificationReceiptLinkRecorded(VerificationReceiptLinkRecorded {
            receipt_id: "receipt-1".to_owned(),
            receipt_event_id: "event-receipt".to_owned(),
            scope: EvidenceScope::Step("task_1:fix_typo".to_owned()),
            workspace_snapshot_id: "snapshot-1".to_owned(),
            changeset_id: None,
            changeset_apply_event_id: None,
        }),
    ));
    entries.push(SessionLogEntry::Control(
        ControlEntry::VerificationFailureLocatorRecorded(VerificationFailureLocatorRecorded {
            check_run_id: "run-1".to_owned(),
            receipt_id: Some("receipt-1".to_owned()),
            command_event_id: Some("event-command".to_owned()),
            output_artifact_id: None,
            summary: "tests failed".to_owned(),
        }),
    ));

    let card = task_strip_view(&entries)
        .expect("task strip")
        .verification
        .expect("verification card");

    assert_eq!(card.status, "check failed");
    assert_eq!(card.recommended.as_deref(), Some("docs-check"));
    let VerificationCardAction::Rerun(request) = card.action.expect("exact rerun action") else {
        panic!("expected exact rerun action");
    };
    assert_eq!(request.task_id.as_str(), "task_1");
    assert_eq!(request.step_id.as_str(), "fix_typo");
    assert_eq!(request.policy_hash, "policy-hash");
    assert_eq!(request.workspace_snapshot_id, "snapshot-1");
    assert!(
        card.inspect_lines
            .iter()
            .any(|line| line == "Failure: tests failed")
    );
    assert!(
        card.inspect_lines
            .iter()
            .any(|line| line == "Command evidence: event-command")
    );
    assert!(
        card.inspect_lines
            .iter()
            .any(|line| line == "Changeset: not linked")
    );
}

#[test]
fn task_verification_card_suppresses_satisfied_action_and_shows_exact_passed_receipt() {
    let mut entries = task_entries_with_readiness(
        TaskRunStatus::Paused,
        TaskStepStatus::Blocked,
        VerificationVerdict::Missing,
    );
    let mut run = verification_check_run("run-1", VerificationCheckRunStatus::Succeeded, None);
    run.receipt_id = Some("receipt-1".to_owned());
    entries.push(SessionLogEntry::Control(
        ControlEntry::VerificationCheckRun(run),
    ));
    entries.push(SessionLogEntry::Control(
        ControlEntry::VerificationReceiptLinkRecorded(VerificationReceiptLinkRecorded {
            receipt_id: "receipt-1".to_owned(),
            receipt_event_id: "event-receipt".to_owned(),
            scope: EvidenceScope::Step("task_1:fix_typo".to_owned()),
            workspace_snapshot_id: "snapshot-1".to_owned(),
            changeset_id: None,
            changeset_apply_event_id: None,
        }),
    ));

    let card = task_strip_view(&entries)
        .expect("task strip")
        .verification
        .expect("verification card");

    assert_eq!(card.status, "passed");
    assert!(card.recommended.is_none());
    assert!(card.action.is_none());
    assert!(
        card.inspect_lines
            .iter()
            .any(|line| line == "Receipt: receipt-1")
    );
    assert!(
        card.inspect_lines
            .iter()
            .any(|line| line == "Snapshot: snapshot-1")
    );
}

#[test]
fn task_verification_card_stays_hidden_when_verification_is_not_applicable() {
    let entries = task_entries_with_custom_readiness(
        TaskRunStatus::Completed,
        TaskStepStatus::Completed,
        VerificationVerdict::NotApplicable,
        VisibleCompletionState::Verified,
        Vec::new(),
    );

    assert!(
        task_strip_view(&entries)
            .expect("task strip")
            .verification
            .is_none()
    );
}

#[test]
fn task_sidebar_does_not_recommend_an_untrusted_run_check() {
    let entries = task_entries_with_custom_readiness(
        TaskRunStatus::Paused,
        TaskStepStatus::Blocked,
        VerificationVerdict::Missing,
        VisibleCompletionState::NeedsUser,
        vec![RequiredAction::RunCheck {
            check_spec_id: "repo-discovered-check".to_owned(),
        }],
    );

    let lines = task_sidebar_lines(&entries);

    assert!(
        !lines
            .iter()
            .any(|line| line == "recommended: run check repo-discovered-check")
    );
}

#[test]
fn task_sidebar_recommends_non_writing_rerun_before_other_actions() {
    let entries = task_entries_with_custom_readiness(
        TaskRunStatus::Paused,
        TaskStepStatus::Blocked,
        VerificationVerdict::Missing,
        VisibleCompletionState::NeedsUser,
        vec![
            RequiredAction::RunCheck {
                check_spec_id: "docs-check".to_owned(),
            },
            RequiredAction::ApproveCheckExecution {
                check_spec_id: "repo-make-check".to_owned(),
            },
            RequiredAction::ReRunNonWritingCheck {
                check_spec_id: "docs-check".to_owned(),
            },
        ],
    );

    let lines = task_sidebar_lines(&entries);

    assert!(
        lines
            .iter()
            .any(|line| line == "recommended: rerun non-writing check docs-check")
    );
    assert!(lines.iter().any(|line| {
        line == "recommended why: a writing check needs fresh non-writing evidence"
    }));
}

#[test]
fn task_sidebar_recommends_retry_for_trusted_failed_check_without_run_action() {
    for (status, expected_reason) in [
        (
            VerificationCheckRunStatus::Failed,
            "recommended why: the latest result failed for the current task scope",
        ),
        (
            VerificationCheckRunStatus::Inconclusive,
            "recommended why: the latest result is inconclusive for the current task scope",
        ),
    ] {
        let mut entries = task_entries_with_custom_readiness(
            TaskRunStatus::Paused,
            TaskStepStatus::Blocked,
            VerificationVerdict::Failed,
            VisibleCompletionState::NeedsUser,
            vec![RequiredAction::ReviewVerificationFailure {
                receipt_id: "receipt-1".to_owned(),
            }],
        );
        entries.push(SessionLogEntry::Control(
            ControlEntry::VerificationCheckRun(verification_check_run(
                "run-1",
                status,
                Some("tests failed"),
            )),
        ));

        let lines = task_sidebar_lines(&entries);

        assert!(
            lines
                .iter()
                .any(|line| line == "recommended: retry check docs-check")
        );
        assert!(lines.iter().any(|line| line == expected_reason));
    }
}

#[test]
fn task_sidebar_does_not_retry_a_superseded_failed_run() {
    let mut entries = task_entries_with_custom_readiness(
        TaskRunStatus::Paused,
        TaskStepStatus::Blocked,
        VerificationVerdict::Failed,
        VisibleCompletionState::NeedsUser,
        vec![RequiredAction::ReviewVerificationFailure {
            receipt_id: "receipt-1".to_owned(),
        }],
    );
    entries.push(SessionLogEntry::Control(
        ControlEntry::VerificationCheckRun(verification_check_run(
            "run-1",
            VerificationCheckRunStatus::Failed,
            Some("tests failed"),
        )),
    ));
    entries.push(SessionLogEntry::Control(
        ControlEntry::VerificationCheckRun(verification_check_run(
            "run-1",
            VerificationCheckRunStatus::Succeeded,
            None,
        )),
    ));

    let lines = task_sidebar_lines(&entries);

    assert!(
        !lines
            .iter()
            .any(|line| line == "recommended: retry check docs-check")
    );
}

#[test]
fn task_sidebar_does_not_retry_a_run_for_a_stale_check_spec_hash() {
    let mut entries = task_entries_with_custom_readiness(
        TaskRunStatus::Paused,
        TaskStepStatus::Blocked,
        VerificationVerdict::Failed,
        VisibleCompletionState::NeedsUser,
        vec![RequiredAction::ReviewVerificationFailure {
            receipt_id: "receipt-1".to_owned(),
        }],
    );
    let mut stale_run = verification_check_run(
        "run-1",
        VerificationCheckRunStatus::Failed,
        Some("tests failed"),
    );
    stale_run.check_spec_hash = "stale-check-spec-hash".to_owned();
    entries.push(SessionLogEntry::Control(
        ControlEntry::VerificationCheckRun(stale_run),
    ));

    let lines = task_sidebar_lines(&entries);

    assert!(
        !lines
            .iter()
            .any(|line| line == "recommended: retry check docs-check")
    );
}

#[test]
fn task_sidebar_recommends_check_approval_without_treating_it_as_execution() {
    let entries = task_entries_with_custom_readiness(
        TaskRunStatus::Paused,
        TaskStepStatus::Blocked,
        VerificationVerdict::Missing,
        VisibleCompletionState::NeedsUser,
        vec![RequiredAction::ApproveCheckExecution {
            check_spec_id: "repo-make-check".to_owned(),
        }],
    );

    let lines = task_sidebar_lines(&entries);

    assert!(
        lines
            .iter()
            .any(|line| line == "recommended: review check approval repo-make-check")
    );
    assert!(lines.iter().any(|line| {
        line == "recommended why: this check needs one-time approval before it can run"
    }));
    assert!(
        !lines
            .iter()
            .any(|line| line == "recommended: run check repo-make-check")
    );
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
    task_entries_with_custom_readiness_and_reasons(
        run_status,
        step_status,
        verdict,
        visible_state,
        required_actions,
        Vec::new(),
    )
}

fn task_entries_with_custom_readiness_and_reasons(
    run_status: TaskRunStatus,
    step_status: TaskStepStatus,
    verdict: VerificationVerdict,
    visible_state: VisibleCompletionState,
    required_actions: Vec<RequiredAction>,
    reasons: Vec<ReadinessReason>,
) -> Vec<SessionLogEntry> {
    let task_id = TaskId::new("task_1").expect("task id");
    let step_id = TaskStepId::new("fix_typo").expect("step id");
    let mut entries = vec![
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
                depends_on: Vec::new(),
                mode: None,
                isolation: None,
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
                reasons,
                required_actions,
            },
            policy_hash: None,
            workspace_snapshot_id: Some("snapshot-1".to_owned()),
        })),
    ];
    entries.push(SessionLogEntry::Control(ControlEntry::CheckSpecRecorded(
        trusted_check_spec_entry("docs-check"),
    )));
    entries
}

fn task_entries_without_readiness(
    run_status: TaskRunStatus,
    step_status: TaskStepStatus,
) -> Vec<SessionLogEntry> {
    let mut entries = task_entries_with_custom_readiness(
        run_status,
        step_status,
        VerificationVerdict::Passed,
        VisibleCompletionState::Verified,
        Vec::new(),
    );
    entries.retain(|entry| {
        !matches!(
            entry,
            SessionLogEntry::Control(ControlEntry::ReadinessEvaluated(_))
        )
    });
    entries
}

fn append_merge_review(entries: &mut Vec<SessionLogEntry>, decision: Option<MergeDecision>) {
    let review_id = MergeReviewId::new("review-1").expect("review id");
    entries.push(SessionLogEntry::Control(
        ControlEntry::MergeReviewRequested(MergeReviewRequested {
            review_id: review_id.clone(),
            changeset_id: ChangeSetId::new("changeset-1").expect("changeset id"),
            parent_workspace_snapshot_id: "parent-snapshot-1".to_owned(),
        }),
    ));
    if let Some(decision) = decision {
        entries.push(SessionLogEntry::Control(ControlEntry::MergeReviewResolved(
            MergeReviewResolved {
                review_id,
                decision,
                reason: None,
            },
        )));
    }
}

fn verification_check_run(
    run_id: &str,
    status: VerificationCheckRunStatus,
    reason: Option<&str>,
) -> VerificationCheckRunEntry {
    VerificationCheckRunEntry {
        run_id: run_id.to_owned(),
        scope: EvidenceScope::Step("task_1:fix_typo".to_owned()),
        check_spec_id: "docs-check".to_owned(),
        check_spec_hash: trusted_check_spec_entry("docs-check")
            .trusted_check
            .check_spec
            .check_spec_hash,
        status,
        receipt_id: None,
        source_event_id: None,
        timeout_ms: Some(5_000),
        reason: reason.map(str::to_owned),
    }
}

fn trusted_check_spec_entry(check_spec_id: &str) -> CheckSpecRecordedEntry {
    let check_spec = CheckSpec::new(
        check_spec_id,
        CheckCommand {
            command: "cargo".to_owned(),
            args: vec!["test".to_owned()],
            cwd: None,
        },
        ToolEffect::ReadOnly,
        "task-verification-scope",
    );
    let trusted_check = TrustedCheckSpec {
        check_spec,
        source: CheckDiscoverySource::UserExplicitConfig,
        workspace_trust_snapshot_id: "trust-1".to_owned(),
        promoted_by: CheckPromotion::ExplicitUserConfig {
            config_event_id: "config-verification".to_owned(),
        },
        approval_event_id: None,
        sandbox_decision_id: None,
    };
    CheckSpecRecordedEntry::new(
        EvidenceScope::Task("task_1".to_owned()),
        trusted_check,
        "config-verification",
    )
}

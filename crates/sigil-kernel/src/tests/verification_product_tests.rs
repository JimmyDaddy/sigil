use anyhow::Result;

use crate::{
    AgentRole, CandidateCheck, CheckCommand, CheckDiscoverySource, CheckPromotion,
    CheckSpecRecordedEntry, ControlEntry, EvidenceScope, ReadinessEvaluatedEntry,
    ReadinessEvaluation, RequiredAction, RunStatus, SessionLogEntry, SessionRef, TaskId,
    TaskIsolationMode, TaskPlanEntry, TaskPlanStatus, TaskRunEntry, TaskRunStatus, TaskStepEntry,
    TaskStepId, TaskStepMode, TaskStepSpec, TaskStepStatus, ToolEffect, VerificationCheckRunEntry,
    VerificationCheckRunStatus, VerificationFailureLocatorRecorded, VerificationProductAction,
    VerificationReceiptLinkRecorded, VerificationVerdict, VisibleCompletionState,
    verification_product_view,
};

const TASK_ID: &str = "task_1";
const STEP_ID: &str = "verify_1";
const CHECK_ID: &str = "cargo-test";
const CHECK_HASH: &str = "scope-hash";
const POLICY_HASH: &str = "policy-hash";
const SNAPSHOT_ID: &str = "snapshot-1";

fn scope() -> EvidenceScope {
    EvidenceScope::Step(format!("{TASK_ID}:{STEP_ID}"))
}

fn task_entries() -> Result<Vec<SessionLogEntry>> {
    let task_id = TaskId::new(TASK_ID)?;
    let step_id = TaskStepId::new(STEP_ID)?;
    Ok(vec![
        SessionLogEntry::Control(ControlEntry::TaskRun(TaskRunEntry {
            task_id: task_id.clone(),
            parent_session_ref: SessionRef::new_relative("parent.jsonl")?,
            objective: "verify the workspace".to_owned(),
            status: TaskRunStatus::Started,
            reason: None,
        })),
        SessionLogEntry::Control(ControlEntry::TaskPlan(TaskPlanEntry {
            task_id: task_id.clone(),
            plan_version: 1,
            status: TaskPlanStatus::Accepted,
            steps: vec![TaskStepSpec {
                step_id: step_id.clone(),
                title: "Verify".to_owned(),
                display_name: None,
                detail: None,
                role: AgentRole::Executor,
                depends_on: Vec::new(),
                mode: Some(TaskStepMode::Verify),
                isolation: Some(TaskIsolationMode::SharedReadOnly),
            }],
            reason: None,
        })),
        SessionLogEntry::Control(ControlEntry::TaskStep(TaskStepEntry {
            task_id,
            plan_version: 1,
            step_id,
            role: AgentRole::Executor,
            status: TaskStepStatus::Running,
            title: Some("Verify".to_owned()),
            summary: None,
            reason: None,
        })),
    ])
}

fn trusted_check_entry() -> CheckSpecRecordedEntry {
    let trusted = CandidateCheck {
        source: CheckDiscoverySource::UserExplicitConfig,
        command: CheckCommand {
            command: "cargo".to_owned(),
            args: vec!["test".to_owned()],
            cwd: None,
        },
        source_event_id: "event-discovery".to_owned(),
        workspace_trust_snapshot_id: "trust-1".to_owned(),
    }
    .promote(
        CHECK_ID,
        CHECK_HASH,
        ToolEffect::ReadOnly,
        CheckPromotion::ExplicitUserConfig {
            config_event_id: "event-config".to_owned(),
        },
    )
    .expect("explicit user check should promote");
    CheckSpecRecordedEntry::new(scope(), trusted, "event-discovery")
}

fn readiness(action: RequiredAction, verdict: VerificationVerdict) -> ReadinessEvaluatedEntry {
    ReadinessEvaluatedEntry {
        scope: scope(),
        evaluation: ReadinessEvaluation {
            run_status: RunStatus::Completed,
            verification_verdict: verdict,
            visible_state: VisibleCompletionState::CompletedUnverified,
            reasons: Vec::new(),
            required_actions: vec![action],
        },
        policy_hash: Some(POLICY_HASH.to_owned()),
        workspace_snapshot_id: Some(SNAPSHOT_ID.to_owned()),
    }
}

#[test]
fn product_view_exposes_one_exact_rerun_binding() -> Result<()> {
    let mut entries = task_entries()?;
    entries.push(SessionLogEntry::Control(ControlEntry::CheckSpecRecorded(
        trusted_check_entry(),
    )));
    entries.push(SessionLogEntry::Control(ControlEntry::ReadinessEvaluated(
        readiness(
            RequiredAction::RunCheck {
                check_spec_id: CHECK_ID.to_owned(),
            },
            VerificationVerdict::Missing,
        ),
    )));

    let view = verification_product_view(&entries).expect("verification card should project");
    let VerificationProductAction::Rerun(request) = view.action.expect("rerun action") else {
        panic!("expected an exact rerun action");
    };
    assert_eq!(request.task_id.as_str(), TASK_ID);
    assert_eq!(request.step_id.as_str(), STEP_ID);
    assert_eq!(request.check_spec_id, CHECK_ID);
    assert_eq!(
        request.check_spec_hash,
        trusted_check_entry()
            .trusted_check
            .check_spec
            .check_spec_hash
    );
    assert_eq!(request.policy_hash, POLICY_HASH);
    assert_eq!(request.workspace_snapshot_id, SNAPSHOT_ID);
    assert_eq!(view.status, "missing");
    assert_eq!(
        view.recommendation_reason.as_deref(),
        Some("this trusted check is required by the current task")
    );
    Ok(())
}

#[test]
fn product_view_binds_failed_run_to_receipt_snapshot_changeset_and_locator() -> Result<()> {
    let mut entries = task_entries()?;
    let check = trusted_check_entry();
    let check_hash = check.trusted_check.check_spec.check_spec_hash.clone();
    entries.extend([
        SessionLogEntry::Control(ControlEntry::CheckSpecRecorded(check)),
        SessionLogEntry::Control(ControlEntry::VerificationCheckRun(
            VerificationCheckRunEntry {
                run_id: "run-1".to_owned(),
                scope: scope(),
                check_spec_id: CHECK_ID.to_owned(),
                check_spec_hash: check_hash,
                status: VerificationCheckRunStatus::Failed,
                receipt_id: Some("receipt-1".to_owned()),
                source_event_id: Some("event-check".to_owned()),
                timeout_ms: Some(60_000),
                reason: Some("tests failed".to_owned()),
            },
        )),
        SessionLogEntry::Control(ControlEntry::VerificationReceiptLinkRecorded(
            VerificationReceiptLinkRecorded {
                receipt_id: "receipt-1".to_owned(),
                receipt_event_id: "event-receipt".to_owned(),
                scope: scope(),
                workspace_snapshot_id: SNAPSHOT_ID.to_owned(),
                changeset_id: Some("changeset-1".to_owned()),
                changeset_apply_event_id: Some("event-apply".to_owned()),
            },
        )),
        SessionLogEntry::Control(ControlEntry::VerificationFailureLocatorRecorded(
            VerificationFailureLocatorRecorded {
                check_run_id: "run-1".to_owned(),
                receipt_id: Some("receipt-1".to_owned()),
                command_event_id: Some("event-command".to_owned()),
                output_artifact_id: Some("artifact-output".to_owned()),
                summary: "2 tests failed".to_owned(),
            },
        )),
        SessionLogEntry::Control(ControlEntry::ReadinessEvaluated(readiness(
            RequiredAction::RunCheck {
                check_spec_id: CHECK_ID.to_owned(),
            },
            VerificationVerdict::Failed,
        ))),
    ]);

    let view = verification_product_view(&entries).expect("verification card should project");
    assert_eq!(view.status, "check failed");
    assert_eq!(view.evidence.check_run_id.as_deref(), Some("run-1"));
    assert_eq!(view.evidence.receipt_id.as_deref(), Some("receipt-1"));
    assert_eq!(
        view.evidence.workspace_snapshot_id.as_deref(),
        Some(SNAPSHOT_ID)
    );
    assert_eq!(view.evidence.changeset_id.as_deref(), Some("changeset-1"));
    assert_eq!(
        view.evidence.changeset_apply_event_id.as_deref(),
        Some("event-apply")
    );
    assert_eq!(
        view.evidence.command_event_id.as_deref(),
        Some("event-command")
    );
    assert_eq!(
        view.evidence.output_artifact_id.as_deref(),
        Some("artifact-output")
    );
    assert_eq!(
        view.evidence.failure_summary.as_deref(),
        Some("2 tests failed")
    );
    Ok(())
}

#[test]
fn product_view_keeps_approval_as_review_only() -> Result<()> {
    let mut entries = task_entries()?;
    entries.push(SessionLogEntry::Control(ControlEntry::ReadinessEvaluated(
        readiness(
            RequiredAction::ApproveCheckExecution {
                check_spec_id: CHECK_ID.to_owned(),
            },
            VerificationVerdict::Pending,
        ),
    )));

    let view = verification_product_view(&entries).expect("verification card should project");
    assert_eq!(view.status, "pending");
    assert_eq!(
        view.action,
        Some(VerificationProductAction::ReviewApproval {
            check_spec_id: CHECK_ID.to_owned(),
        })
    );
    Ok(())
}

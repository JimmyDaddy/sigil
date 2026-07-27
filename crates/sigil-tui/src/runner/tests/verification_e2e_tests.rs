use std::time::{Duration, Instant};

use anyhow::{Result, anyhow};
use sigil_kernel::{
    Agent, AgentRole, CheckCommand, CheckDiscoverySource, CheckPromotion, CheckSpec,
    CheckSpecRecordedEntry, CompletionCriteria, ControlEntry, EvidenceScope, JsonlSessionStore,
    ReadinessEvaluatedEntry, ReadinessEvaluation, RequiredAction, RunEvent, RunStatus, Session,
    SessionLogEntry, SessionRef, TaskId, TaskIsolationMode, TaskPlanEntry, TaskPlanStatus,
    TaskRunEntry, TaskRunStatus, TaskStepEntry, TaskStepId, TaskStepMode, TaskStepSpec,
    TaskStepStatus, TaskVerificationRerunRequest, ToolEffect, ToolRegistry, TrustedCheckSpec,
    VerificationCheckRunStatus, VerificationPolicy, VerificationPolicyChangedEntry,
    VerificationVerdict, VisibleCompletionState, build_workspace_snapshot, stable_workspace_id,
};
use tempfile::tempdir;

use super::{
    super::{WorkerCommand, WorkerMessage},
    common::{PlannedProvider, spawn_test_worker, test_root_config},
};

#[test]
fn exact_verification_rerun_crosses_worker_loop_and_persists_receipt_link() -> Result<()> {
    let temp = tempdir()?;
    let workspace_root = temp.path().join("workspace");
    std::fs::create_dir(&workspace_root)?;
    std::fs::write(workspace_root.join("note.txt"), "verify me\n")?;
    let workspace_root = std::fs::canonicalize(workspace_root)?;
    let session_log_path = temp.path().join(".sigil/sessions/verification.jsonl");
    let store = JsonlSessionStore::new(&session_log_path)?;
    let mut session = Session::new("planned", "planned-model").with_store(store);

    let task_id = TaskId::new("task_1")?;
    let step_id = TaskStepId::new("step_1")?;
    let step = TaskStepSpec {
        step_id: step_id.clone(),
        title: "Verify".to_owned(),
        display_name: None,
        detail: None,
        role: AgentRole::Executor,
        depends_on: Vec::new(),
        intent_refs: Vec::new(),
        mode: Some(TaskStepMode::Verify),
        isolation: Some(TaskIsolationMode::SharedReadOnly),
    };
    session.append_control(ControlEntry::TaskRun(TaskRunEntry {
        task_id: task_id.clone(),
        parent_session_ref: SessionRef::new_relative("parent.jsonl")?,
        objective: "verify the workspace".to_owned(),
        status: TaskRunStatus::Paused,
        reason: Some("waiting for verification".to_owned()),
    }))?;
    session.append_control(ControlEntry::TaskPlan(TaskPlanEntry {
        task_id: task_id.clone(),
        plan_version: 1,
        status: TaskPlanStatus::Accepted,
        steps: vec![step.clone()],
        reason: None,
    }))?;
    session.append_control(ControlEntry::TaskStep(TaskStepEntry {
        task_id: task_id.clone(),
        plan_version: 1,
        step_id: step_id.clone(),
        role: step.role,
        status: TaskStepStatus::Blocked,
        title: Some(step.title),
        summary: None,
        reason: Some("verification missing".to_owned()),
    }))?;
    let step_scope = EvidenceScope::Step("task_1:step_1".to_owned());
    let task_scope = EvidenceScope::Task(task_id.as_str().to_owned());
    let check_spec = CheckSpec::new(
        "rustc-version",
        CheckCommand {
            command: "rustc".to_owned(),
            args: vec!["--version".to_owned()],
            cwd: None,
        },
        ToolEffect::ReadOnly,
        "task_step_default",
    );
    let trusted = TrustedCheckSpec {
        check_spec: check_spec.clone(),
        source: CheckDiscoverySource::UserExplicitConfig,
        workspace_trust_snapshot_id: "user-config".to_owned(),
        promoted_by: CheckPromotion::ExplicitUserConfig {
            config_event_id: "event-config".to_owned(),
        },
        approval_event_id: None,
        sandbox_decision_id: None,
    };
    session.append_control(ControlEntry::CheckSpecRecorded(
        CheckSpecRecordedEntry::new(task_scope.clone(), trusted, "event-config"),
    ))?;

    let mut policy = VerificationPolicy::no_checks_required("task_step_default");
    policy.required_checks = vec![check_spec.clone()];
    policy.completion_criteria = CompletionCriteria::AllRequiredChecks;
    policy.timeout_ms = Some(5_000);
    let policy_entry =
        VerificationPolicyChangedEntry::new(task_scope, policy.clone(), "event-policy")?;
    let policy_hash = policy_entry.policy_hash.clone();
    session.append_control(ControlEntry::VerificationPolicyChanged(policy_entry))?;

    let workspace_id = stable_workspace_id(&workspace_root)?;
    let workspace_snapshot_id =
        build_workspace_snapshot(&workspace_root, workspace_id, &policy.verification_scope, 0)?
            .workspace_snapshot_id
            .ok_or_else(|| anyhow!("test workspace must produce a complete snapshot"))?;
    session.append_control(ControlEntry::ReadinessEvaluated(ReadinessEvaluatedEntry {
        scope: step_scope.clone(),
        evaluation: ReadinessEvaluation {
            run_status: RunStatus::Completed,
            verification_verdict: VerificationVerdict::Missing,
            visible_state: VisibleCompletionState::NeedsUser,
            reasons: Vec::new(),
            required_actions: vec![RequiredAction::RunCheck {
                check_spec_id: check_spec.check_spec_id.clone(),
            }],
        },
        policy_hash: Some(policy_hash.clone()),
        workspace_snapshot_id: Some(workspace_snapshot_id.clone()),
    }))?;
    drop(session);

    let request = TaskVerificationRerunRequest::new(
        task_id,
        1,
        step_id,
        check_spec.check_spec_id.clone(),
        check_spec.check_spec_hash.clone(),
        policy_hash,
        workspace_snapshot_id.clone(),
    );
    let root_config = test_root_config(&workspace_root, "planned", "planned-model");
    let agent = Agent::new(PlannedProvider::new(Vec::new()), ToolRegistry::new());
    let worker = spawn_test_worker(root_config, session_log_path.clone(), agent, workspace_root)?;
    worker.send(WorkerCommand::RerunTaskVerification { request })?;

    let deadline = Instant::now() + Duration::from_secs(10);
    let mut lifecycle = Vec::new();
    let mut receipt_seen = false;
    let mut link_seen = false;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(anyhow!("timed out waiting for verification worker result"));
        }
        match worker.recv_with_timeout(remaining)? {
            WorkerMessage::Event(event) => match *event {
                RunEvent::Control(ControlEntry::VerificationCheckRun(run)) => {
                    lifecycle.push(run.status);
                }
                RunEvent::Control(ControlEntry::VerificationRecorded(_)) => receipt_seen = true,
                RunEvent::Control(ControlEntry::VerificationReceiptLinkRecorded(link)) => {
                    assert_eq!(link.scope, step_scope);
                    assert_eq!(link.workspace_snapshot_id, workspace_snapshot_id);
                    assert!(link.changeset_id.is_none());
                    link_seen = true;
                }
                _ => {}
            },
            WorkerMessage::Notice(message)
                if message == "verification check rustc-version passed" =>
            {
                break;
            }
            WorkerMessage::RunFailed(error) => {
                return Err(anyhow!("verification worker failed: {error}"));
            }
            _ => {}
        }
    }

    assert_eq!(
        lifecycle,
        vec![
            VerificationCheckRunStatus::Queued,
            VerificationCheckRunStatus::Running,
            VerificationCheckRunStatus::Succeeded,
        ]
    );
    assert!(receipt_seen);
    assert!(link_seen);
    let entries = JsonlSessionStore::read_entries(&session_log_path)?;
    assert!(entries.iter().any(|entry| matches!(
        entry,
        SessionLogEntry::Control(ControlEntry::VerificationReceiptLinkRecorded(link))
            if link.scope == step_scope && link.workspace_snapshot_id == workspace_snapshot_id
    )));

    worker.shutdown()?;
    Ok(())
}

#[test]
fn exact_integration_review_reads_and_digest_checks_the_bound_artifact() -> Result<()> {
    let temp = tempdir()?;
    let workspace_root = temp.path().join("workspace");
    std::fs::create_dir(&workspace_root)?;
    std::fs::write(workspace_root.join("note.txt"), "review me\n")?;
    let workspace_root = std::fs::canonicalize(workspace_root)?;
    let session_log_path = temp.path().join(".sigil/sessions/integration-review.jsonl");
    let store = JsonlSessionStore::new(&session_log_path)?;
    let mut session = Session::new("planned", "planned-model").with_store(store);
    let aggregate_diff = b"diff --git a/src/lib.rs b/src/lib.rs\n-old\n+new\n";
    let recorder = session
        .mutation_event_recorder()
        .ok_or_else(|| anyhow!("durable test session must expose mutation artifacts"))?;
    let artifact_ref = recorder.capture_immutable_content_artifact(
        &stable_workspace_id(&workspace_root)?,
        "integration-review-test",
        &workspace_root.join("integration-review.diff"),
        aggregate_diff,
    )?;
    let digest = format!("sha256:{}", sigil_kernel::sha256_hex(aggregate_diff));
    let entries = crate::app::tests::common::integration_review_entries(&artifact_ref, &digest)?;
    let request = sigil_kernel::task_integration_review_product(&entries)
        .expect("fixture integration review")
        .request;
    for entry in entries {
        let SessionLogEntry::Control(control) = entry else {
            return Err(anyhow!("integration review fixture must be control-only"));
        };
        session.append_control(control)?;
    }
    drop(session);

    let root_config = test_root_config(&workspace_root, "planned", "planned-model");
    let agent = Agent::new(PlannedProvider::new(Vec::new()), ToolRegistry::new());
    let worker = spawn_test_worker(root_config, session_log_path, agent, workspace_root)?;
    worker.send(WorkerCommand::ReviewTaskIntegration {
        request: request.clone(),
    })?;

    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(anyhow!("timed out waiting for integration review"));
        }
        match worker.recv_with_timeout(remaining)? {
            WorkerMessage::TaskIntegrationReviewLoaded {
                request: loaded_request,
                aggregate_diff: loaded_diff,
            } => {
                assert_eq!(loaded_request, request);
                assert_eq!(loaded_diff.as_bytes(), aggregate_diff);
                break;
            }
            WorkerMessage::TaskIntegrationReviewFailed { error, .. }
            | WorkerMessage::RunFailed(error) => {
                return Err(anyhow!("integration review worker failed: {error}"));
            }
            _ => {}
        }
    }

    worker.send(WorkerCommand::AcceptTaskIntegration {
        request: request.clone(),
    })?;
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(anyhow!("timed out waiting for integration acceptance"));
        }
        match worker.recv_with_timeout(remaining)? {
            WorkerMessage::TaskIntegrationAcceptanceFailed {
                request: failed_request,
                ..
            } => {
                assert_eq!(failed_request, request);
                break;
            }
            WorkerMessage::RunFailed(error) => {
                return Err(anyhow!("integration acceptance worker failed: {error}"));
            }
            _ => {}
        }
    }
    worker.shutdown()?;
    Ok(())
}

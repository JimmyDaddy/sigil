use super::*;
use sigil_kernel::{
    AgentThreadId, ApprovalMode, ApprovalRequestIdentityV2, AssistantMessageKind, EvidenceReceipt,
    EvidenceScope, ExecutionNetworkReceipt, JsonlSessionStore, LogicalRunId, ModelMessage,
    PermissionRisk, RedactionState, Session, SessionScopeId, TaskId, ToolAccess,
    ToolApprovalDecisionReceiptV2, ToolApprovalEntry, ToolApprovalUserDecision,
    ToolArtifactSensitivity, ToolOperation, ToolResult, ToolResultMeta, ToolSubject,
    ToolSubjectScope, UserInputActionV1, UserInputCommandId, UserInputDecisionAcceptedV1,
    UserInputDecisionV1, UserInputIdentityV1, UserInputPurposeV1, UserInputQuestionV1,
    UserInputRequestId, UserInputRequestV1, UserInputRequestedV1, UserInputSourceV1,
    VerificationBinding, VerificationRecordedEntry, durable_tool_execution_entry,
};
use std::{fs, path::Path};

fn session_at(path: &Path) -> Result<Session> {
    let store = JsonlSessionStore::new(path)?;
    let artifacts = sigil_kernel::ToolArtifactStore::for_session_store(&store);
    let mut session = Session::new("activity-fixture", "activity-model")
        .with_store(store)
        .with_tool_artifact_store_override(artifacts);
    session.ensure_identity_entry()?;
    Ok(session)
}

fn recorded_read(
    session: &mut Session,
    file: &Path,
    id: &str,
    offset: u64,
    truncated: bool,
    publish: bool,
) -> Result<u64> {
    let path = "private-source.txt";
    let call = ToolCall {
        id: id.to_owned(),
        name: "read_file".to_owned(),
        args_json: serde_json::json!({"path": path, "offset": offset, "limit": 4}).to_string(),
    };
    session.append_assistant_message(ModelMessage::assistant_with_kind(
        None,
        vec![call.clone()],
        AssistantMessageKind::ToolPreamble,
    ))?;
    let subjects = vec![ToolSubject::path_with_scope(
        path,
        path,
        Some(file.canonicalize()?),
        ToolSubjectScope::Workspace,
    )];
    session.append_control(ControlEntry::ToolExecution(Box::new(
        durable_tool_execution_entry(&call, &subjects, ToolExecutionStatus::Started, None, None)?,
    )))?;
    let content = fs::read_to_string(file)?
        .lines()
        .skip(offset as usize)
        .take(4)
        .collect::<Vec<_>>()
        .join("\n");
    let result = ToolResult::ok(
        id,
        "read_file",
        content.clone(),
        ToolResultMeta {
            returned_bytes: Some(content.len() as u64),
            returned_lines: Some(content.lines().count() as u64),
            limit_lines: Some(4),
            limit_bytes: Some(8192),
            truncated,
            ..ToolResultMeta::default()
        },
    );
    session.append_control(ControlEntry::ToolExecution(Box::new(
        durable_tool_execution_entry(
            &call,
            &subjects,
            ToolExecutionStatus::Completed,
            Some(2),
            Some(&result),
        )?,
    )))?;
    if publish {
        let store = session
            .tool_artifact_store()
            .expect("durable fixture has artifact storage");
        let (record, _) = ToolResultRecordedV3::capture(
            &result,
            Some(&store),
            ToolArtifactSensitivity::Ordinary,
        )?;
        assert_eq!(
            record
                .artifact
                .descriptor()
                .expect("published read artifact")
                .persisted_bytes,
            content.len() as u64
        );
        session.append(SessionLogEntry::ToolResultV3(record))?;
    }
    Ok(content.len() as u64)
}

#[test]
fn model_eval_activity_repeated_reads_use_persisted_exact_output_and_range() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let log = temp.path().join("session.jsonl");
    let file = temp.path().join("private-source.txt");
    fs::write(&file, "private first\nprivate second\nprivate third")?;
    let mut session = session_at(&log)?;
    let repeated_bytes = recorded_read(&mut session, &file, "first", 0, false, true)?;
    recorded_read(&mut session, &file, "same-range", 0, false, true)?;
    recorded_read(&mut session, &file, "other-range", 1, false, true)?;
    fs::write(&file, "changed first\nchanged second\nprivate third")?;
    recorded_read(&mut session, &file, "changed-output", 0, false, true)?;
    drop(session);
    let before = fs::read(&log)?;
    let records = JsonlSessionStore::read_event_records(&log)?;
    let metrics = observe_activity(&records)?;
    assert_eq!(metrics.reads.completed_read_events, 4);
    assert_eq!(metrics.reads.qualified_reads, 4);
    assert_eq!(metrics.reads.canonical_subject_reads, 4);
    assert_eq!(metrics.reads.managed_logical_subject_reads, 0);
    assert_eq!(metrics.reads.repeated_output_reads, Some(1));
    assert_eq!(
        metrics.reads.repeated_persisted_output_bytes,
        Some(repeated_bytes)
    );
    fs::remove_file(&file)?;
    assert_eq!(
        serde_json::to_value(observe_activity(&records)?)?,
        serde_json::to_value(&metrics)?
    );
    assert_eq!(
        fs::read(&log)?,
        before,
        "projection never rewrites evidence"
    );
    let wire = serde_json::to_string(&metrics)?;
    for private in [
        "private-source",
        "private first",
        "changed first",
        "same-range",
        temp.path().to_str().expect("fixture path is UTF-8"),
    ] {
        assert!(!wire.contains(private));
    }
    Ok(())
}

#[test]
fn model_eval_activity_missing_truncated_and_reused_call_identity_are_excluded() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let log = temp.path().join("session.jsonl");
    let file = temp.path().join("private-source.txt");
    fs::write(&file, "one\ntwo\nthree")?;
    let mut session = session_at(&log)?;
    recorded_read(&mut session, &file, "missing-output", 0, false, false)?;
    recorded_read(&mut session, &file, "truncated-output", 0, true, true)?;
    recorded_read(&mut session, &file, "reused-call", 0, false, true)?;
    // A later provider turn is allowed to reuse an ID. The projection must not cross-join it.
    recorded_read(&mut session, &file, "reused-call", 1, false, true)?;
    drop(session);
    let metrics = observe_activity(&JsonlSessionStore::read_event_records(&log)?)?;
    assert_eq!(metrics.reads.completed_read_events, 4);
    assert_eq!(metrics.reads.qualified_reads, 0);
    assert_eq!(metrics.reads.excluded_completed_read_events, 4);
    assert_eq!(metrics.reads.repeated_output_reads, None);
    assert_eq!(metrics.reads.repeated_persisted_output_bytes, None);
    let summary = super::super::trajectory::ModelEvalTrajectorySummary {
        activity: Some(metrics),
        ..Default::default()
    };
    assert!(summary.redundant_reads.is_none());
    assert!(summary.human_interventions.is_none());
    assert!(summary.ineffective_repair_rounds.is_none());
    Ok(())
}

fn approval(scope: &str, id: &str, action: ToolApprovalAuditAction) -> ToolApprovalEntry {
    let plan_hash = sigil_kernel::stable_event_hash(b"activity-plan");
    let accepted = action == ToolApprovalAuditAction::DecisionAccepted;
    ToolApprovalEntry {
        schema_version: sigil_kernel::TOOL_APPROVAL_AUDIT_SCHEMA_VERSION,
        identity: ApprovalRequestIdentityV2 {
            session_id: scope.to_owned(),
            run_id: "private-run".to_owned(),
            call_id: id.to_owned(),
            approval_request_id: format!("request-{id}"),
            plan_hash: plan_hash.clone(),
            policy_version: "policy-fixture".to_owned(),
            execution_binding_hash: plan_hash.clone(),
            expires_at_ms: u64::MAX,
        },
        plan_hash,
        action,
        call_id: id.to_owned(),
        tool_name: "read_file".to_owned(),
        access: ToolAccess::Read,
        network_effect: None,
        local_policy_decision: ApprovalMode::Ask,
        network_policy_decision: ApprovalMode::Allow,
        source_policy_decision: ApprovalMode::Allow,
        operation: ToolOperation::Read,
        risk: PermissionRisk::Low,
        subjects: Vec::new(),
        subject_zones: Vec::new(),
        policy_decision: ApprovalMode::Ask,
        external_directory_required: false,
        confirmation: None,
        snapshot_required: false,
        command_permission_matches: Vec::new(),
        decision_reasons: Vec::new(),
        user_decision: accepted.then_some(ToolApprovalUserDecision::Approved),
        reason: None,
        preview_hash: None,
        decision_receipt: accepted.then(|| ToolApprovalDecisionReceiptV2 {
            approval_request_id: format!("request-{id}"),
            decision: ToolApprovalUserDecision::Approved,
            accepted_at_ms: 100,
        }),
        terminal_status: None,
    }
}

#[test]
fn model_eval_activity_exact_decisions_do_not_claim_human_provenance() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let log = temp.path().join("session.jsonl");
    let mut session = session_at(&log)?;
    let scope = session.session_scope_id().to_owned();
    session.append_control(ControlEntry::ToolApproval(approval(
        &scope,
        "one",
        ToolApprovalAuditAction::Requested,
    )))?;
    let decision = approval(&scope, "one", ToolApprovalAuditAction::DecisionAccepted);
    session.append_control(ControlEntry::ToolApproval(decision.clone()))?;
    session.append_control(ControlEntry::ToolApproval(decision))?;
    session.append_control(ControlEntry::ToolApproval(approval(
        &scope,
        "missing-request",
        ToolApprovalAuditAction::DecisionAccepted,
    )))?;
    let input = UserInputRequestedV1::new(UserInputRequestV1 {
        schema_version: 1,
        identity: UserInputIdentityV1 {
            session_scope_id: SessionScopeId::new(&scope)?,
            root_logical_run_id: LogicalRunId::new("private-root")?,
            source_thread_id: AgentThreadId::new("private-thread")?,
            request_id: UserInputRequestId::new("private-input")?,
            generation: 1,
            source_binding_hash: sigil_kernel::stable_event_hash(b"input"),
        },
        source: UserInputSourceV1::Agent,
        purpose: UserInputPurposeV1::Clarification,
        prompt: "private question".to_owned(),
        questions: vec![UserInputQuestionV1 {
            id: "choice".to_owned(),
            question: "private question".to_owned(),
            description: None,
            required: true,
            options: Vec::new(),
            multiple: false,
        }],
        allowed_actions: vec![UserInputActionV1::Submit, UserInputActionV1::Decline],
        requested_at_unix_ms: 10,
        continuation: Some(sigil_kernel::UserInputContinuationBindingV1 {
            assistant_message_id: "input-assistant".to_owned(),
            tool_call_id: "input-call".to_owned(),
            provider_name: "activity-fixture".to_owned(),
            model_name: "activity-model".to_owned(),
        }),
    })?;
    session.append_control(ControlEntry::UserInputRequested(Box::new(input.clone())))?;
    let accepted = UserInputDecisionAcceptedV1::new(
        &input,
        UserInputCommandId::new("private-command")?,
        UserInputDecisionV1::Declined,
        20,
    )?;
    session.append_control(ControlEntry::UserInputDecisionAccepted(Box::new(accepted)))?;
    drop(session);
    let metrics = observe_activity(&JsonlSessionStore::read_event_records(&log)?)?;
    assert_eq!(metrics.decisions.tool_decision_events, 3);
    assert_eq!(metrics.decisions.unique_accepted_tool_decisions, 1);
    assert_eq!(metrics.decisions.user_input_decision_events, 1);
    assert_eq!(metrics.decisions.unique_accepted_user_input_decisions, 1);
    assert_eq!(
        metrics.decisions.unmatched_or_conflicting_decision_events,
        1
    );
    assert!(!serde_json::to_string(&metrics)?.contains("private"));
    Ok(())
}

#[test]
fn model_eval_activity_conflicting_tool_request_id_never_attributes_a_decision() -> Result<()> {
    for conflict_after_decision in [false, true] {
        let temp = tempfile::tempdir()?;
        let log = temp.path().join("session.jsonl");
        let mut session = session_at(&log)?;
        let scope = session.session_scope_id().to_owned();
        let first = approval(&scope, "first", ToolApprovalAuditAction::Requested);
        let request_id = first.identity.approval_request_id.clone();
        let mut conflict = approval(&scope, "second", ToolApprovalAuditAction::Requested);
        conflict.identity.approval_request_id = request_id.clone();
        let mut accepted = approval(
            &scope,
            if conflict_after_decision {
                "first"
            } else {
                "second"
            },
            ToolApprovalAuditAction::DecisionAccepted,
        );
        accepted.identity.approval_request_id = request_id.clone();
        accepted
            .decision_receipt
            .as_mut()
            .expect("accepted decision has a receipt")
            .approval_request_id = request_id;
        session.append_control(ControlEntry::ToolApproval(first))?;
        if !conflict_after_decision {
            session.append_control(ControlEntry::ToolApproval(conflict.clone()))?;
        }
        session.append_control(ControlEntry::ToolApproval(accepted))?;
        if conflict_after_decision {
            session.append_control(ControlEntry::ToolApproval(conflict))?;
        }
        drop(session);
        let metrics = observe_activity(&JsonlSessionStore::read_event_records(&log)?)?;
        assert_eq!(metrics.decisions.tool_decision_events, 1);
        assert_eq!(metrics.decisions.unique_accepted_tool_decisions, 0);
        assert_eq!(
            metrics.decisions.unmatched_or_conflicting_decision_events,
            1
        );
    }
    Ok(())
}

fn failed_receipt(
    scope: &str,
    id: &str,
    snapshot: &str,
    check: &str,
    policy: &str,
) -> VerificationReceipt {
    VerificationReceipt {
        receipt: EvidenceReceipt {
            receipt_id: id.to_owned(),
            source_session_id: scope.to_owned(),
            source_event_id: format!("source-{id}"),
            source_event_type: "check_finished".to_owned(),
            scope: EvidenceScope::Task("task-metrics".to_owned()),
            producer_tool_call: None,
            workspace_revision: None,
            workspace_snapshot_id: Some(snapshot.to_owned()),
            policy_hash: Some(policy.to_owned()),
            changeset_id: None,
            status: ReceiptStatus::Failed,
            artifact_refs: Vec::new(),
            redaction_state: RedactionState::None,
            recorded_at_stream_sequence: 1,
        },
        binding: VerificationBinding {
            workspace_id: "workspace".to_owned(),
            workspace_snapshot_id: snapshot.to_owned(),
            verification_scope_hash: "scope".to_owned(),
            check_spec_hash: format!("hash-{check}"),
            environment_fingerprint: "environment".to_owned(),
            sandbox_profile_hash: "sandbox".to_owned(),
            execution_backend: None,
            execution_backend_capabilities: None,
            execution_network: ExecutionNetworkReceipt::unknown("fixture"),
            workspace_trust_snapshot_id: "trust".to_owned(),
            approval_event_id: None,
            sandbox_decision_id: None,
        },
        check_spec_id: check.to_owned(),
        check_status: ReceiptStatus::Failed,
        failure_reason: Some("fixture check failed".to_owned()),
        mutates_verification_scope: false,
    }
}

fn feedback(
    receipt: &VerificationReceipt,
    attempt: &str,
    status: TaskVerificationFeedbackStatusV1,
) -> Result<TaskVerificationFeedbackV1> {
    let mut feedback = TaskVerificationFeedbackV1 {
        feedback_id: String::new(),
        task_id: TaskId::new("task-metrics")?,
        admission_id: "admission-metrics".to_owned(),
        attempt_id: attempt.to_owned(),
        receipt_ids: vec![receipt.receipt.receipt_id.clone()],
        policy_hash: receipt.receipt.policy_hash.clone().expect("fixture policy"),
        workspace_snapshot_id: receipt.binding.workspace_snapshot_id.clone(),
        status,
        reason: None,
    };
    feedback.feedback_id = sigil_kernel::stable_event_uuid("task-verification-feedback-v1", &serde_json::json!({
        "task": feedback.task_id, "admission": feedback.admission_id, "attempt": feedback.attempt_id,
        "receipts": feedback.receipt_ids, "policy": feedback.policy_hash, "snapshot": feedback.workspace_snapshot_id,
    }).to_string());
    feedback.validate()?;
    Ok(feedback)
}

#[test]
fn model_eval_activity_repair_counts_require_new_exact_comparable_failed_receipts() -> Result<()> {
    for (check, policy, attempt, publish, expected) in [
        ("unit", "policy", "attempt", true, Some(1)),
        ("other-check", "policy", "attempt", true, None),
        ("unit", "new-policy", "attempt", true, None),
        ("unit", "policy", "other-attempt", true, None),
        ("unit", "policy", "attempt", false, None),
    ] {
        let temp = tempfile::tempdir()?;
        let log = temp.path().join("session.jsonl");
        let mut session = session_at(&log)?;
        let scope = session.session_scope_id().to_owned();
        let first = failed_receipt(&scope, "before", "before-snapshot", "unit", "policy");
        session.append_control(ControlEntry::VerificationRecorded(
            VerificationRecordedEntry {
                receipt: first.clone(),
            },
        ))?;
        session.append_control(ControlEntry::TaskVerificationFeedbackV1(feedback(
            &first,
            "attempt",
            TaskVerificationFeedbackStatusV1::Pending,
        )?))?;
        session.append_control(ControlEntry::TaskVerificationFeedbackV1(feedback(
            &first,
            "attempt",
            TaskVerificationFeedbackStatusV1::Repair,
        )?))?;
        let second = failed_receipt(&scope, "after", "after-snapshot", check, policy);
        if publish {
            session.append_control(ControlEntry::VerificationRecorded(
                VerificationRecordedEntry {
                    receipt: second.clone(),
                },
            ))?;
        }
        session.append_control(ControlEntry::TaskVerificationFeedbackV1(feedback(
            &second,
            attempt,
            TaskVerificationFeedbackStatusV1::Pending,
        )?))?;
        drop(session);
        let metrics = observe_activity(&JsonlSessionStore::read_event_records(&log)?)?;
        assert_eq!(metrics.repairs.selected_repairs, 1);
        assert_eq!(
            metrics.repairs.failed_rechecks_after_repair, expected,
            "{check}/{policy}/{attempt}/{publish}"
        );
        assert_eq!(
            metrics.repairs.repairs_with_next_feedback,
            usize::from(attempt == "attempt")
        );
        assert_eq!(
            metrics.repairs.repairs_without_next_feedback,
            usize::from(attempt != "attempt")
        );
    }
    Ok(())
}

#[test]
fn model_eval_activity_recheck_excludes_changed_workspace_trust() -> Result<()> {
    for changed_trust in [true, false] {
        let temp = tempfile::tempdir()?;
        let log = temp.path().join("session.jsonl");
        let mut session = session_at(&log)?;
        let scope = session.session_scope_id().to_owned();
        let first = failed_receipt(&scope, "before", "before-snapshot", "unit", "policy");
        session.append_control(ControlEntry::VerificationRecorded(
            VerificationRecordedEntry {
                receipt: first.clone(),
            },
        ))?;
        session.append_control(ControlEntry::TaskVerificationFeedbackV1(feedback(
            &first,
            "attempt",
            TaskVerificationFeedbackStatusV1::Pending,
        )?))?;
        session.append_control(ControlEntry::TaskVerificationFeedbackV1(feedback(
            &first,
            "attempt",
            TaskVerificationFeedbackStatusV1::Repair,
        )?))?;
        let mut second = failed_receipt(&scope, "after", "after-snapshot", "unit", "policy");
        if changed_trust {
            second.binding.workspace_trust_snapshot_id = "changed-trust".to_owned();
        } else {
            // This is diagnostic text; the effective network policy is already in the sandbox hash.
            second.binding.execution_network.reason = Some("changed-reason".to_owned());
        }
        session.append_control(ControlEntry::VerificationRecorded(
            VerificationRecordedEntry {
                receipt: second.clone(),
            },
        ))?;
        session.append_control(ControlEntry::TaskVerificationFeedbackV1(feedback(
            &second,
            "attempt",
            TaskVerificationFeedbackStatusV1::Pending,
        )?))?;
        drop(session);
        let repairs = observe_activity(&JsonlSessionStore::read_event_records(&log)?)?.repairs;
        assert_eq!(repairs.selected_repairs, 1, "changed_trust={changed_trust}");
        assert_eq!(
            repairs.repairs_with_next_feedback, 1,
            "changed_trust={changed_trust}"
        );
        assert_eq!(
            repairs.comparable_recheck_pairs,
            usize::from(!changed_trust),
            "changed_trust={changed_trust}"
        );
        assert_eq!(
            repairs.excluded_recheck_pairs,
            usize::from(changed_trust),
            "changed_trust={changed_trust}"
        );
        assert_eq!(
            repairs.failed_rechecks_after_repair,
            (!changed_trust).then_some(1),
            "changed_trust={changed_trust}"
        );
    }
    Ok(())
}

#[test]
fn model_eval_activity_recheck_requires_receipts_before_their_own_feedback() -> Result<()> {
    for late_receipt in ["before", "after"] {
        let temp = tempfile::tempdir()?;
        let log = temp.path().join("session.jsonl");
        let mut session = session_at(&log)?;
        let scope = session.session_scope_id().to_owned();
        let first = failed_receipt(&scope, "before", "before-snapshot", "unit", "policy");
        let second = failed_receipt(&scope, "after", "after-snapshot", "unit", "policy");
        if late_receipt == "after" {
            session.append_control(ControlEntry::VerificationRecorded(
                VerificationRecordedEntry {
                    receipt: first.clone(),
                },
            ))?;
        }
        session.append_control(ControlEntry::TaskVerificationFeedbackV1(feedback(
            &first,
            "attempt",
            TaskVerificationFeedbackStatusV1::Pending,
        )?))?;
        session.append_control(ControlEntry::TaskVerificationFeedbackV1(feedback(
            &first,
            "attempt",
            TaskVerificationFeedbackStatusV1::Repair,
        )?))?;
        if late_receipt == "before" {
            session.append_control(ControlEntry::VerificationRecorded(
                VerificationRecordedEntry {
                    receipt: first.clone(),
                },
            ))?;
            session.append_control(ControlEntry::VerificationRecorded(
                VerificationRecordedEntry {
                    receipt: second.clone(),
                },
            ))?;
        }
        session.append_control(ControlEntry::TaskVerificationFeedbackV1(feedback(
            &second,
            "attempt",
            TaskVerificationFeedbackStatusV1::Pending,
        )?))?;
        if late_receipt == "after" {
            session.append_control(ControlEntry::VerificationRecorded(
                VerificationRecordedEntry { receipt: second },
            ))?;
        }
        drop(session);
        let repairs = observe_activity(&JsonlSessionStore::read_event_records(&log)?)?.repairs;
        assert_eq!(repairs.selected_repairs, 1, "{late_receipt}");
        assert_eq!(repairs.repairs_with_next_feedback, 1, "{late_receipt}");
        assert_eq!(repairs.comparable_recheck_pairs, 0, "{late_receipt}");
        assert_eq!(repairs.excluded_recheck_pairs, 1, "{late_receipt}");
        assert_eq!(repairs.failed_rechecks_after_repair, None, "{late_receipt}");
    }
    Ok(())
}

#[test]
fn model_eval_activity_overlapping_repairs_do_not_assign_one_recheck_to_one_selection() -> Result<()>
{
    let temp = tempfile::tempdir()?;
    let log = temp.path().join("session.jsonl");
    let mut session = session_at(&log)?;
    let scope = session.session_scope_id().to_owned();
    let first = failed_receipt(&scope, "first", "first-snapshot", "unit", "policy");
    let second = failed_receipt(&scope, "second", "second-snapshot", "unit", "policy");
    for receipt in [&first, &second] {
        session.append_control(ControlEntry::VerificationRecorded(
            VerificationRecordedEntry {
                receipt: (*receipt).clone(),
            },
        ))?;
        session.append_control(ControlEntry::TaskVerificationFeedbackV1(feedback(
            receipt,
            "same-attempt",
            TaskVerificationFeedbackStatusV1::Pending,
        )?))?;
    }
    for receipt in [&first, &second] {
        session.append_control(ControlEntry::TaskVerificationFeedbackV1(feedback(
            receipt,
            "same-attempt",
            TaskVerificationFeedbackStatusV1::Repair,
        )?))?;
    }
    let next = failed_receipt(&scope, "next", "next-snapshot", "unit", "policy");
    session.append_control(ControlEntry::VerificationRecorded(
        VerificationRecordedEntry {
            receipt: next.clone(),
        },
    ))?;
    session.append_control(ControlEntry::TaskVerificationFeedbackV1(feedback(
        &next,
        "same-attempt",
        TaskVerificationFeedbackStatusV1::Pending,
    )?))?;
    drop(session);
    let repairs = observe_activity(&JsonlSessionStore::read_event_records(&log)?)?.repairs;
    assert_eq!(repairs.selected_repairs, 2);
    assert_eq!(repairs.repairs_with_next_feedback, 2);
    assert_eq!(repairs.repairs_without_next_feedback, 0);
    assert_eq!(repairs.comparable_recheck_pairs, 0);
    assert_eq!(repairs.excluded_recheck_pairs, 2);
    assert_eq!(repairs.failed_rechecks_after_repair, None);
    Ok(())
}

#[test]
fn model_eval_activity_no_feedback_does_not_turn_failed_checks_into_failed_repairs() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let log = temp.path().join("session.jsonl");
    let mut session = session_at(&log)?;
    let receipt = failed_receipt(
        session.session_scope_id(),
        "ordinary-check",
        "snapshot",
        "unit",
        "policy",
    );
    session.append_control(ControlEntry::VerificationRecorded(
        VerificationRecordedEntry { receipt },
    ))?;
    drop(session);
    let metrics = observe_activity(&JsonlSessionStore::read_event_records(&log)?)?;
    assert_eq!(metrics.repairs.selected_repairs, 0);
    assert_eq!(metrics.repairs.failed_rechecks_after_repair, None);
    assert_eq!(metrics.reads.repeated_output_reads, None);
    Ok(())
}

#[test]
fn model_eval_activity_foreign_artifact_scope_cannot_fill_read_coverage() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let log = temp.path().join("session.jsonl");
    let file = temp.path().join("private-source.txt");
    fs::write(&file, "actual content")?;
    let other = session_at(&temp.path().join("other.jsonl"))?;
    let foreign = other
        .tool_artifact_store()
        .expect("other durable artifact store");
    let mut session = session_at(&log)?;
    session.attach_tool_artifact_store_override(foreign);
    recorded_read(&mut session, &file, "foreign-artifact", 0, false, true)?;
    drop(session);
    let metrics = observe_activity(&JsonlSessionStore::read_event_records(&log)?)?;
    assert_eq!(metrics.reads.completed_read_events, 1);
    assert_eq!(metrics.reads.excluded_completed_read_events, 1);
    assert_eq!(metrics.reads.repeated_persisted_output_bytes, None);
    Ok(())
}

#[test]
fn model_eval_activity_selected_repair_without_next_feedback_stays_unobserved() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let log = temp.path().join("session.jsonl");
    let mut session = session_at(&log)?;
    let receipt = failed_receipt(
        session.session_scope_id(),
        "last-failure",
        "snapshot",
        "unit",
        "policy",
    );
    session.append_control(ControlEntry::VerificationRecorded(
        VerificationRecordedEntry {
            receipt: receipt.clone(),
        },
    ))?;
    session.append_control(ControlEntry::TaskVerificationFeedbackV1(feedback(
        &receipt,
        "attempt",
        TaskVerificationFeedbackStatusV1::Pending,
    )?))?;
    session.append_control(ControlEntry::TaskVerificationFeedbackV1(feedback(
        &receipt,
        "attempt",
        TaskVerificationFeedbackStatusV1::Repair,
    )?))?;
    drop(session);
    let metrics = observe_activity(&JsonlSessionStore::read_event_records(&log)?)?;
    assert_eq!(metrics.repairs.selected_repairs, 1);
    assert_eq!(metrics.repairs.repairs_without_next_feedback, 1);
    assert_eq!(metrics.repairs.repairs_with_next_feedback, 0);
    assert_eq!(metrics.repairs.failed_rechecks_after_repair, None);
    Ok(())
}

#[test]
fn model_eval_activity_logical_label_without_managed_receipt_is_not_qualified() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let log = temp.path().join("session.jsonl");
    let file = temp.path().join("private-source.txt");
    fs::write(&file, "actual fixture output")?;
    let mut session = session_at(&log)?;
    recorded_read(&mut session, &file, "no-managed-receipt", 0, false, true)?;
    drop(session);
    let mut records = JsonlSessionStore::read_event_records(&log)?;
    for record in &mut records {
        let SessionStreamRecord::Stored(event) = record;
        let Some(entry) = event.payload.get_mut("session_log_entry") else {
            continue;
        };
        let mut typed: SessionLogEntry = serde_json::from_value(entry.clone())?;
        if let SessionLogEntry::Control(ControlEntry::ToolExecution(execution)) = &mut typed {
            execution.subjects[0].canonical_path_sha256 = None;
            if execution.status == ToolExecutionStatus::Started {
                execution.metadata.details["permission_plan_hash"] =
                    serde_json::json!(sigil_kernel::stable_event_hash(b"actual-plan-not-enough"));
            }
        }
        *entry = serde_json::to_value(typed)?;
        event.record_checksum = event.compute_record_checksum()?;
    }
    let metrics = observe_activity(&records)?;
    assert_eq!(metrics.reads.completed_read_events, 1);
    assert_eq!(metrics.reads.qualified_reads, 0);
    assert_eq!(metrics.reads.excluded_completed_read_events, 1);
    assert_eq!(metrics.reads.managed_logical_subject_reads, 0);
    assert_eq!(metrics.reads.repeated_output_reads, None);
    Ok(())
}

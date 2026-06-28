use anyhow::Result;

use crate::{
    CandidateCheck, CheckCommand, CheckDiscoverySource, CheckPromotion, CheckSpec,
    CheckSpecRecordedEntry, ChildVerificationReceiptLinked, CompletionCriteria, ControlEntry,
    DurableEventType, EventClass, EvidenceReceipt, EvidenceScope, FileProjectionStore,
    JsonlSessionStore, ProjectionApplyDecision, ProjectionStore, ReadinessEvaluatedEntry,
    ReadinessEvaluation, ReceiptStatus, RedactionState, RequiredAction, RunStatus,
    SandboxProfileRequirement, SessionListProjectionSnapshot, SessionLogEntry, SessionRef,
    SessionStreamRecord, TaskId, TaskRunEntry, TaskRunStatus, ToolEffect, UsageStats,
    VerificationAutoRunPolicy, VerificationBinding, VerificationCheckRunEntry,
    VerificationCheckRunStatus, VerificationPolicy, VerificationPolicyChangedEntry,
    VerificationRecordedEntry, VerificationScope, VerificationStateProjection,
    VerificationStateProjectionSnapshot, VerificationVerdict, VisibleCompletionState,
    WorkspaceTrust, WorkspaceTrustDecisionEntry, WorkspaceTrustRequirement,
    session_list_projection_from_records,
};

fn workspace_trust_entry(workspace_id: &str, trust_event: &str) -> WorkspaceTrustDecisionEntry {
    WorkspaceTrustDecisionEntry {
        workspace_id: workspace_id.to_owned(),
        workspace_trust_snapshot_id: format!("{workspace_id}-trust"),
        trust: WorkspaceTrust::Trusted,
        decided_by_event_id: Some(trust_event.to_owned()),
        reason: Some("user trusted workspace".to_owned()),
    }
}

fn append_trust_event(
    store: &JsonlSessionStore,
    workspace_id: &str,
    trust_event: &str,
) -> Result<crate::StoredEvent> {
    store.append_session_entry_event(&SessionLogEntry::Control(
        ControlEntry::WorkspaceTrustDecision(workspace_trust_entry(workspace_id, trust_event)),
    ))
}

fn read_records(store: &JsonlSessionStore) -> Result<Vec<SessionStreamRecord>> {
    JsonlSessionStore::read_event_records(store.path())
}

fn sample_check_spec() -> CheckSpec {
    CheckSpec::new(
        "cargo-test",
        CheckCommand {
            command: "cargo".to_owned(),
            args: vec![
                "test".to_owned(),
                "-p".to_owned(),
                "sigil-kernel".to_owned(),
            ],
            cwd: None,
        },
        ToolEffect::ReadOnly,
        "scope-main",
    )
}

fn sample_verification_policy() -> VerificationPolicy {
    VerificationPolicy {
        required_checks: vec![sample_check_spec()],
        completion_criteria: CompletionCriteria::AllRequiredChecks,
        verification_scope: VerificationScope::all_tracked("scope-main"),
        sandbox_profile: SandboxProfileRequirement::None,
        workspace_trust_requirement: WorkspaceTrustRequirement::None,
        allow_unverified_completion: false,
        timeout_ms: Some(60_000),
        auto_run: VerificationAutoRunPolicy::Manual,
    }
}

fn sample_check_spec_entry() -> CheckSpecRecordedEntry {
    let trusted_check = CandidateCheck {
        source: CheckDiscoverySource::Cargo,
        command: CheckCommand {
            command: "cargo".to_owned(),
            args: vec![
                "test".to_owned(),
                "-p".to_owned(),
                "sigil-kernel".to_owned(),
            ],
            cwd: None,
        },
        source_event_id: "event-discovery".to_owned(),
        workspace_trust_snapshot_id: "trust-1".to_owned(),
    }
    .promote(
        "cargo-test",
        "scope-main",
        ToolEffect::ReadOnly,
        CheckPromotion::UserApproved {
            approval_event_id: "event-approval".to_owned(),
        },
    )
    .expect("sample check promotes");
    CheckSpecRecordedEntry::new(
        EvidenceScope::Task("task-1".to_owned()),
        trusted_check,
        "event-discovery",
    )
}

fn sample_policy_entry() -> Result<VerificationPolicyChangedEntry> {
    VerificationPolicyChangedEntry::new(
        EvidenceScope::Task("task-1".to_owned()),
        sample_verification_policy(),
        "event-policy",
    )
}

fn sample_check_run_entry() -> VerificationCheckRunEntry {
    let check = sample_check_spec();
    VerificationCheckRunEntry {
        run_id: "check-run-1".to_owned(),
        scope: EvidenceScope::Task("task-1".to_owned()),
        check_spec_id: check.check_spec_id,
        check_spec_hash: check.check_spec_hash,
        status: VerificationCheckRunStatus::Succeeded,
        receipt_id: Some("receipt-1".to_owned()),
        source_event_id: Some("event-check-finished".to_owned()),
        timeout_ms: Some(60_000),
        reason: None,
    }
}

fn sample_receipt_entry() -> VerificationRecordedEntry {
    let check = sample_check_spec();
    VerificationRecordedEntry {
        receipt: crate::VerificationReceipt {
            receipt: EvidenceReceipt {
                receipt_id: "receipt-1".to_owned(),
                source_session_id: "session-1".to_owned(),
                source_event_id: "event-check-finished".to_owned(),
                source_event_type: DurableEventType::CheckFinished.as_str().to_owned(),
                scope: EvidenceScope::Task("task-1".to_owned()),
                producer_tool_call: Some("tool-call-1".to_owned()),
                workspace_revision: Some(1),
                workspace_snapshot_id: Some("snapshot-1".to_owned()),
                policy_hash: Some("policy-hash".to_owned()),
                changeset_id: None,
                status: ReceiptStatus::Succeeded,
                artifact_refs: Vec::new(),
                redaction_state: RedactionState::None,
                recorded_at_stream_sequence: 2,
            },
            binding: VerificationBinding {
                workspace_id: "workspace-1".to_owned(),
                workspace_snapshot_id: "snapshot-1".to_owned(),
                verification_scope_hash: "scope-main".to_owned(),
                check_spec_hash: check.check_spec_hash,
                environment_fingerprint: "env-1".to_owned(),
                sandbox_profile_hash: "sandbox-local".to_owned(),
                execution_backend: None,
                execution_backend_capabilities: None,
                workspace_trust_snapshot_id: "trust-1".to_owned(),
                approval_event_id: None,
                sandbox_decision_id: None,
            },
            check_spec_id: check.check_spec_id,
            check_status: ReceiptStatus::Succeeded,
            failure_reason: None,
            mutates_verification_scope: false,
        },
    }
}

fn sample_readiness_entry() -> ReadinessEvaluatedEntry {
    ReadinessEvaluatedEntry {
        scope: EvidenceScope::Task("task-1".to_owned()),
        evaluation: ReadinessEvaluation {
            run_status: RunStatus::Completed,
            verification_verdict: VerificationVerdict::Missing,
            visible_state: VisibleCompletionState::CompletedUnverified,
            reasons: Vec::new(),
            required_actions: vec![RequiredAction::RunCheck {
                check_spec_id: "cargo-test".to_owned(),
            }],
        },
        policy_hash: Some("policy-hash".to_owned()),
        workspace_snapshot_id: Some("snapshot-1".to_owned()),
    }
}

fn sample_child_link() -> ChildVerificationReceiptLinked {
    ChildVerificationReceiptLinked {
        parent_session_id: "parent-session".to_owned(),
        child_session_id: "child-session".to_owned(),
        child_receipt_id: "child-receipt".to_owned(),
        child_event_id: "child-event".to_owned(),
        child_workspace_id: "child-workspace".to_owned(),
        child_workspace_snapshot_id: "child-snapshot".to_owned(),
        policy_hash: "policy-hash".to_owned(),
        changeset_id: Some("changeset-1".to_owned()),
        merge_event_id: Some("merge-event".to_owned()),
    }
}

fn sample_usage() -> UsageStats {
    UsageStats {
        prompt_tokens: 11,
        completion_tokens: 7,
        cache_hit_tokens: 3,
        cache_miss_tokens: 8,
        input_cost: 0.0,
        output_cost: 0.0,
        cache_savings: 0.0,
        system_fingerprint: None,
    }
}

fn sample_task_run_entry() -> Result<TaskRunEntry> {
    Ok(TaskRunEntry {
        task_id: TaskId::new("task_1")?,
        parent_session_ref: SessionRef::new_relative("session-parent.jsonl")?,
        objective: "fix selector projection".to_owned(),
        status: TaskRunStatus::Paused,
        reason: Some("needs verification".to_owned()),
    })
}

#[test]
fn verification_projection_snapshot_roundtrips_all_entry_vectors() -> Result<()> {
    let snapshot = VerificationStateProjectionSnapshot {
        check_specs: vec![sample_check_spec_entry()],
        policies: vec![sample_policy_entry()?],
        check_runs: vec![sample_check_run_entry()],
        receipts: vec![sample_receipt_entry()],
        readiness: vec![sample_readiness_entry()],
        child_receipt_links: vec![sample_child_link()],
        workspace_trust: vec![workspace_trust_entry("workspace-1", "trust-event")],
    };

    let projection = VerificationStateProjection::from(snapshot.clone());
    let restored = VerificationStateProjectionSnapshot::from(&projection);

    assert_eq!(restored, snapshot);
    Ok(())
}

#[test]
fn session_list_projection_rebuilds_mixed_stream_metadata() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let session_path = temp.path().join("session.jsonl");
    let legacy_entry = SessionLogEntry::Control(ControlEntry::SessionIdentity {
        provider_name: "deepseek".to_owned(),
        model_name: "deepseek-v4-pro".to_owned(),
    });
    std::fs::write(
        &session_path,
        format!("{}\n", serde_json::to_string(&legacy_entry)?),
    )?;
    let store = JsonlSessionStore::new(&session_path)?;
    store.append(&SessionLogEntry::User(crate::ModelMessage::user(
        "Investigate session projection",
    )))?;
    store.append(&SessionLogEntry::Assistant(crate::ModelMessage::assistant(
        Some("Projection implemented".to_owned()),
        Vec::new(),
    )))?;
    store.append(&SessionLogEntry::Control(ControlEntry::UsageSnapshot(
        sample_usage(),
    )))?;
    store.append(&SessionLogEntry::Control(ControlEntry::TaskRun(
        sample_task_run_entry()?,
    )))?;
    store.append(&SessionLogEntry::Control(ControlEntry::ReadinessEvaluated(
        sample_readiness_entry(),
    )))?;
    let records = read_records(&store)?;

    let projection = session_list_projection_from_records(&records)?;
    let entry = projection
        .latest_session()
        .expect("session projection should contain one entry");

    assert_eq!(entry.provider_name.as_deref(), Some("deepseek"));
    assert_eq!(entry.model_name.as_deref(), Some("deepseek-v4-pro"));
    assert_eq!(
        entry.title.as_deref(),
        Some("Investigate session projection")
    );
    assert_eq!(entry.user_message_count, 1);
    assert_eq!(entry.assistant_message_count, 1);
    assert_eq!(entry.control_entry_count, 4);
    assert_eq!(
        entry.latest_usage.as_ref().map(|usage| usage.prompt_tokens),
        Some(11)
    );
    assert_eq!(
        entry.latest_task.as_ref().map(|task| task.status),
        Some(TaskRunStatus::Paused)
    );
    assert_eq!(
        entry
            .latest_readiness
            .as_ref()
            .map(|readiness| readiness.verification_verdict),
        Some(VerificationVerdict::Missing)
    );
    assert_eq!(entry.last_stream_sequence, records.len() as u64);
    Ok(())
}

#[test]
fn file_projection_store_rebuilds_session_list_projection() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let store = JsonlSessionStore::new(temp.path().join("session.jsonl"))?;
    store.append(&SessionLogEntry::User(crate::ModelMessage::user(
        "List me from projection",
    )))?;
    let records = read_records(&store)?;
    let projection_store = FileProjectionStore::<SessionListProjectionSnapshot>::session_list(
        temp.path().join("session-list.projection.json"),
    );

    let state = projection_store.rebuild_session_list_from_records(&records)?;
    let loaded = projection_store.load()?;

    assert_eq!(state, loaded);
    assert_eq!(
        loaded
            .projection
            .latest_session()
            .and_then(|entry| entry.title.as_deref()),
        Some("List me from projection")
    );
    assert_eq!(
        loaded
            .cursor
            .as_ref()
            .map(|cursor| cursor.last_applied_stream_sequence),
        Some(1)
    );
    Ok(())
}

#[test]
fn file_projection_store_rebuilds_verification_projection_from_jsonl() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let session_path = temp.path().join("session.jsonl");
    let legacy_entry = SessionLogEntry::Control(ControlEntry::WorkspaceTrustDecision(
        workspace_trust_entry("workspace-legacy", "legacy-trust-event"),
    ));
    std::fs::write(
        &session_path,
        format!("{}\n", serde_json::to_string(&legacy_entry)?),
    )?;
    let session_store = JsonlSessionStore::new(&session_path)?;
    append_trust_event(&session_store, "workspace-v2", "v2-trust-event")?;
    let records = read_records(&session_store)?;
    let projection_store = FileProjectionStore::<VerificationStateProjectionSnapshot>::verification(
        temp.path().join("verification.projection.json"),
    );

    let state = projection_store.rebuild_verification_from_records(&records)?;
    let loaded = projection_store.load()?;
    let persisted_projection = VerificationStateProjection::from(loaded.projection.clone());
    let expected_projection = {
        let mut projection = VerificationStateProjection::default();
        projection.apply_control_entry(&ControlEntry::WorkspaceTrustDecision(
            workspace_trust_entry("workspace-legacy", "legacy-trust-event"),
        ));
        projection.apply_control_entry(&ControlEntry::WorkspaceTrustDecision(
            workspace_trust_entry("workspace-v2", "v2-trust-event"),
        ));
        projection
    };

    assert_eq!(state, loaded);
    assert_eq!(persisted_projection, expected_projection);
    assert_eq!(
        loaded
            .cursor
            .as_ref()
            .map(|cursor| cursor.last_applied_stream_sequence),
        Some(2)
    );
    Ok(())
}

#[test]
fn file_projection_store_exposes_trait_and_rebuild_diagnostics() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let session_store = JsonlSessionStore::new(temp.path().join("session.jsonl"))?;
    append_trust_event(&session_store, "workspace-1", "trust-event")?;
    let records = read_records(&session_store)?;
    let repeated = vec![records[0].clone(), records[0].clone()];
    let projection_store = FileProjectionStore::<VerificationStateProjectionSnapshot>::verification(
        temp.path().join("verification.projection.json"),
    );

    let output = projection_store.rebuild_stream_records(
        &repeated,
        super::apply_verification_projection_snapshot_record,
    )?;
    let loaded = projection_store.load_state()?;

    assert_eq!(output.state, loaded);
    assert_eq!(output.report.applied_records, 1);
    assert_eq!(output.report.ignored_records, 1);
    assert_eq!(
        output
            .report
            .cursor
            .as_ref()
            .map(|cursor| cursor.last_applied_stream_sequence),
        Some(1)
    );
    Ok(())
}

#[test]
fn file_projection_store_fails_closed_on_invalid_envelopes() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("verification.projection.json");
    let projection_store =
        FileProjectionStore::<VerificationStateProjectionSnapshot>::verification(&path);
    let snapshot = serde_json::to_value(VerificationStateProjectionSnapshot::default())?;

    std::fs::write(
        &path,
        serde_json::to_vec(&serde_json::json!({
            "schema_version": 999,
            "projection_name": "verification_state",
            "projection_schema_version": crate::session::VERIFICATION_STATE_PROJECTION_SCHEMA_VERSION,
            "state": { "projection": snapshot.clone(), "cursor": null }
        }))?,
    )?;
    let error = projection_store
        .load()
        .expect_err("unsupported store schema should fail closed");
    assert!(
        error
            .to_string()
            .contains("unsupported projection store schema")
    );

    std::fs::write(
        &path,
        serde_json::to_vec(&serde_json::json!({
            "schema_version": crate::FILE_PROJECTION_STORE_SCHEMA_VERSION,
            "projection_name": "other_projection",
            "projection_schema_version": crate::session::VERIFICATION_STATE_PROJECTION_SCHEMA_VERSION,
            "state": { "projection": snapshot.clone(), "cursor": null }
        }))?,
    )?;
    let error = projection_store
        .load()
        .expect_err("wrong projection name should fail closed");
    assert!(error.to_string().contains("projection store contains"));

    std::fs::write(
        &path,
        serde_json::to_vec(&serde_json::json!({
            "schema_version": crate::FILE_PROJECTION_STORE_SCHEMA_VERSION,
            "projection_name": "verification_state",
            "projection_schema_version": 999,
            "state": { "projection": snapshot, "cursor": null }
        }))?,
    )?;
    let error = projection_store
        .load()
        .expect_err("wrong projection schema should fail closed");
    assert!(error.to_string().contains("projection schema 999"));

    std::fs::write(&path, b"{not json")?;
    let error = projection_store
        .load()
        .expect_err("corrupt projection store should fail closed");
    assert!(
        error
            .to_string()
            .contains("failed to parse projection store")
    );
    Ok(())
}

#[test]
fn file_projection_store_ignores_duplicate_current_record_without_rewrite() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let session_store = JsonlSessionStore::new(temp.path().join("session.jsonl"))?;
    append_trust_event(&session_store, "workspace-1", "trust-event")?;
    let records = read_records(&session_store)?;
    let projection_store = FileProjectionStore::<VerificationStateProjectionSnapshot>::verification(
        temp.path().join("verification.projection.json"),
    );

    let first = projection_store.apply_verification_record(&records[0])?;
    let second = projection_store.apply_verification_record(&records[0])?;
    let loaded = projection_store.load()?;

    assert_eq!(first, ProjectionApplyDecision::Apply);
    assert_eq!(second, ProjectionApplyDecision::IgnoreAlreadyApplied);
    assert_eq!(loaded.projection.workspace_trust.len(), 1);
    assert_eq!(
        loaded
            .cursor
            .as_ref()
            .map(|cursor| cursor.last_applied_event_id.as_str()),
        Some(records[0].event_id())
    );
    Ok(())
}

#[test]
fn file_projection_store_fails_closed_on_sequence_gap_without_advancing_cursor() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let session_store = JsonlSessionStore::new(temp.path().join("session.jsonl"))?;
    append_trust_event(&session_store, "workspace-1", "trust-event-1")?;
    let mut records = read_records(&session_store)?;
    let gap = crate::StoredEvent::new(
        DurableEventType::WorkspaceTrustDecision,
        EventClass::Critical,
        "event-gap".to_owned(),
        records[0].session_id().to_owned(),
        3,
        serde_json::json!({
            "session_log_entry": SessionLogEntry::Control(ControlEntry::WorkspaceTrustDecision(
                workspace_trust_entry("workspace-2", "trust-event-2")
            ))
        }),
    )?;
    records.push(SessionStreamRecord::Stored(gap));
    let projection_store = FileProjectionStore::<VerificationStateProjectionSnapshot>::verification(
        temp.path().join("verification.projection.json"),
    );
    projection_store.apply_verification_record(&records[0])?;

    let error = projection_store
        .apply_verification_record(&records[1])
        .expect_err("sequence gap should fail closed");
    let loaded = projection_store.load()?;

    assert!(error.to_string().contains("projection sequence gap"));
    assert_eq!(loaded.projection.workspace_trust.len(), 1);
    assert_eq!(
        loaded
            .cursor
            .as_ref()
            .map(|cursor| cursor.last_applied_stream_sequence),
        Some(1)
    );
    Ok(())
}

#[test]
fn file_projection_store_fails_closed_when_cursor_is_ahead_of_old_record() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let session_store = JsonlSessionStore::new(temp.path().join("session.jsonl"))?;
    append_trust_event(&session_store, "workspace-1", "trust-event-1")?;
    append_trust_event(&session_store, "workspace-2", "trust-event-2")?;
    let records = read_records(&session_store)?;
    let projection_store = FileProjectionStore::<VerificationStateProjectionSnapshot>::verification(
        temp.path().join("verification.projection.json"),
    );
    projection_store.rebuild_verification_from_records(&records)?;

    let error = projection_store
        .apply_verification_record(&records[0])
        .expect_err("cursor ahead of old record should fail closed");
    let loaded = projection_store.load()?;

    assert!(error.to_string().contains("cursor is ahead"));
    assert_eq!(loaded.projection.workspace_trust.len(), 2);
    assert_eq!(
        loaded
            .cursor
            .as_ref()
            .map(|cursor| cursor.last_applied_stream_sequence),
        Some(2)
    );
    Ok(())
}

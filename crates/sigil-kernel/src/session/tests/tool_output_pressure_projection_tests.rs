use anyhow::Result;

use super::*;
use crate::session::ToolResultRecordedV3;
use crate::{
    ControlEntry, DurableEventType, EventClass, EvidenceReceipt, EvidenceScope,
    ExternalProvenanceEntry, ExternalTrust, FileType, JsonlSessionStore, ModelMessage,
    MutationCommitted, MutationPrepared, MutationSubject, MutationSyncClass, ReceiptStatus,
    RedactionState, Session, SessionContextProjection, SnapshotCoverage, ToolApprovalAuditAction,
    ToolApprovalEntry, ToolArtifactSensitivity, ToolCall, ToolExecutionEntry, ToolExecutionStatus,
    ToolResult, ToolResultMeta, VerificationBinding, VerificationReceipt,
    VerificationRecordedEntry,
};

fn session_fixture() -> Result<(tempfile::TempDir, Session)> {
    let temp = tempfile::tempdir()?;
    let store = JsonlSessionStore::new(temp.path().join("session.jsonl"))?;
    Ok((temp, Session::new("test", "model").with_store(store)))
}

fn append_result(session: &mut Session, index: usize, bytes: usize, paired: bool) -> Result<()> {
    let call_id = format!("call-{index}");
    append_named_result(
        session,
        &call_id,
        paired,
        ToolResult::ok(
            call_id.clone(),
            "shell",
            "x".repeat(bytes),
            ToolResultMeta::default(),
        ),
    )
    .map(|_| ())
}

fn append_named_result(
    session: &mut Session,
    call_id: &str,
    paired: bool,
    result: ToolResult,
) -> Result<String> {
    if paired {
        session.append_assistant_message(ModelMessage::assistant(
            None,
            vec![ToolCall {
                id: call_id.to_owned(),
                name: "shell".to_owned(),
                args_json: "{}".to_owned(),
            }],
        ))?;
    }
    let store = session
        .tool_artifact_store()
        .expect("durable artifact store");
    let (recorded, _) =
        ToolResultRecordedV3::capture(&result, Some(&store), ToolArtifactSensitivity::Ordinary)?;
    let message_id = recorded.message_id.clone();
    session.append_tool_result_bundle(recorded, Vec::new())?;
    Ok(message_id)
}

fn records(session: &Session) -> Result<Vec<SessionStreamRecord>> {
    let store = session.durable_store().expect("durable session");
    JsonlSessionStore::read_event_records(store.path())
}

fn requested_approval(call_id: &str) -> ControlEntry {
    ControlEntry::ToolApproval(ToolApprovalEntry::test_fixture(
        ToolApprovalAuditAction::Requested,
        call_id,
        "shell",
    ))
}

fn verification_for(call_id: &str, source_event_id: &str) -> VerificationRecordedEntry {
    VerificationRecordedEntry {
        receipt: VerificationReceipt {
            receipt: EvidenceReceipt {
                receipt_id: format!("verification-{call_id}"),
                source_session_id: "session-test".to_owned(),
                source_event_id: source_event_id.to_owned(),
                source_event_type: DurableEventType::ToolResultRecordedV3.as_str().to_owned(),
                scope: EvidenceScope::Run("run-test".to_owned()),
                producer_tool_call: Some(call_id.to_owned()),
                workspace_revision: Some(1),
                workspace_snapshot_id: Some("snapshot-1".to_owned()),
                policy_hash: Some("policy-1".to_owned()),
                changeset_id: None,
                status: ReceiptStatus::Succeeded,
                artifact_refs: Vec::new(),
                redaction_state: RedactionState::None,
                recorded_at_stream_sequence: 1,
            },
            binding: VerificationBinding {
                workspace_id: "workspace-1".to_owned(),
                workspace_snapshot_id: "snapshot-1".to_owned(),
                verification_scope_hash: "scope-1".to_owned(),
                check_spec_hash: "check-spec-1".to_owned(),
                environment_fingerprint: "environment-1".to_owned(),
                sandbox_profile_hash: "sandbox-1".to_owned(),
                execution_backend: None,
                execution_backend_capabilities: None,
                execution_network: Default::default(),
                workspace_trust_snapshot_id: "trust-1".to_owned(),
                approval_event_id: None,
                sandbox_decision_id: None,
            },
            check_spec_id: "check-1".to_owned(),
            check_status: ReceiptStatus::Succeeded,
            failure_reason: None,
            mutates_verification_scope: false,
        },
    }
}

#[test]
fn full_and_incremental_pressure_reducers_are_equivalent() -> Result<()> {
    let (_temp, mut session) = session_fixture()?;
    session.append_user_message(ModelMessage::user("start"))?;
    for index in 0..12 {
        append_result(&mut session, index, 100_000, true)?;
    }
    session.append_user_message(ModelMessage::user("next"))?;
    let records = records(&session)?;
    let split = records.len() / 2;

    let full = ToolOutputPressureProjectionV1::from_records(&records)?.snapshot();
    let mut incremental = ToolOutputPressureProjectionV1::from_records(&records[..split])?;
    incremental.apply_records(&records[split..])?;

    assert_eq!(incremental.snapshot(), full);
    assert!(full.ageable_count > 0);
    assert!(full.protected_tool_tokens > 0);
    assert!(full.reclaimable_tool_tokens > 0);
    Ok(())
}

#[test]
fn gc_roots_are_projected_directly_from_the_active_body_free_snapshot() -> Result<()> {
    let (_temp, mut session) = session_fixture()?;
    session.append_user_message(ModelMessage::user("start"))?;
    append_result(&mut session, 0, 100_000, true)?;
    append_result(&mut session, 1, 100_000, false)?;
    let snapshot = session
        .active_projection_snapshot()?
        .expect("durable projection")
        .tool_output_pressure();
    let expected_refs = snapshot
        .items
        .iter()
        .filter_map(|item| item.artifact_ref.clone())
        .collect::<BTreeSet<_>>();

    let roots = snapshot.artifact_gc_roots();
    let bindings = snapshot.artifact_source_bindings()?;

    assert_eq!(roots.active_result_refs, expected_refs);
    assert_eq!(roots.context_epoch_refs, expected_refs);
    assert_eq!(
        roots.unresolved_read_refs,
        snapshot
            .items
            .iter()
            .filter(|item| !item.pair_closed)
            .filter_map(|item| item.artifact_ref.clone())
            .collect()
    );
    assert_eq!(bindings.len(), expected_refs.len());
    assert!(bindings.iter().all(|binding| !binding.archived));
    roots.validate()?;
    Ok(())
}

#[test]
fn aged_working_set_retirement_preserves_token_accounting_and_artifact_gc_roots() -> Result<()> {
    let (_temp, mut session) = session_fixture()?;
    session.append_user_message(ModelMessage::user("start"))?;
    append_result(&mut session, 0, 100_000, true)?;
    let template = ToolOutputPressureProjectionV1::from_records(&records(&session)?)?
        .snapshot()
        .items
        .into_iter()
        .next()
        .expect("template pressure item");
    let expected_artifact_ref = template.artifact_ref.clone().expect("published artifact");

    let mut projection = ToolOutputPressureProjectionV1::default();
    for index in 0..=TOOL_OUTPUT_PRESSURE_MAX_RESULTS {
        let mut item = template.clone();
        item.source_event_id = format!("event-{index}");
        item.message_id = format!("message-{index}");
        item.call_id = format!("call-{index}");
        item.retention = if index == 0 {
            ToolOutputRetentionClassV1::Aged
        } else {
            item.artifact_ref = None;
            item.artifact_sha256 = None;
            ToolOutputRetentionClassV1::Ageable
        };
        projection
            .result_ids_by_call
            .insert(item.call_id.clone(), item.source_event_id.clone());
        projection
            .result_ids_by_message
            .insert(item.message_id.clone(), item.source_event_id.clone());
        projection.ordered_ids.push(item.source_event_id.clone());
        projection.items.insert(item.source_event_id.clone(), item);
    }
    let tokens_before = projection
        .items
        .values()
        .map(|item| item.current_model_tokens)
        .sum::<u64>();

    projection.trim_aged_items_to_soft_limit(TOOL_OUTPUT_PRESSURE_MAX_RESULTS)?;
    let snapshot = projection.snapshot();

    assert_eq!(snapshot.items.len(), TOOL_OUTPUT_PRESSURE_MAX_RESULTS);
    assert_eq!(snapshot.archived_aged_count, 1);
    assert_eq!(snapshot.total_tool_tokens, tokens_before);
    let binding = snapshot
        .artifact_source_binding(&expected_artifact_ref)
        .expect("retired artifact source binding");
    assert_eq!(binding.artifact_ref, expected_artifact_ref);
    assert_eq!(binding.source_event_id, "event-0");
    assert_eq!(binding.call_id, "call-0");
    assert_eq!(binding.tool_name, "shell");
    assert!(binding.archived);
    assert_eq!(binding.persisted_bytes, template.persisted_bytes);
    assert_eq!(snapshot.artifact_source_bindings()?, vec![binding]);
    assert!(
        snapshot
            .artifact_gc_roots()
            .active_result_refs
            .contains(&expected_artifact_ref)
    );
    Ok(())
}

#[test]
fn pressure_soft_limit_degrades_without_failure_but_manifest_hard_limit_fails_closed() {
    assert!(validate_pressure_result_capacity(TOOL_OUTPUT_PRESSURE_MAX_RESULTS).is_ok());
    assert!(validate_pressure_result_capacity(TOOL_OUTPUT_PRESSURE_HARD_MAX_RESULTS - 1).is_ok());
    assert!(validate_pressure_result_capacity(TOOL_OUTPUT_PRESSURE_HARD_MAX_RESULTS).is_err());
}

#[test]
fn selector_protects_current_recent_and_unpaired_results() -> Result<()> {
    let (_temp, mut session) = session_fixture()?;
    session.append_user_message(ModelMessage::user("start"))?;
    append_result(&mut session, 999, 100_000, false)?;
    for index in 0..15 {
        append_result(&mut session, index, 100_000, true)?;
    }
    session.append_user_message(ModelMessage::user("next"))?;
    let snapshot = ToolOutputPressureProjectionV1::from_records(&records(&session)?)?.snapshot();

    assert!(snapshot.items.iter().any(|item| {
        item.call_id == "call-999"
            && item.retention == ToolOutputRetentionClassV1::UnpairedProtected
    }));
    let batch = ToolOutputAgingBatchV1::select(&snapshot, ToolOutputAgingReasonV1::CostOnly)?
        .expect("enough old output should satisfy batch economics");
    assert!(batch.reclaimable_tokens >= TOOL_OUTPUT_MIN_BATCH_RECLAIM_TOKENS);
    assert!(batch.source_event_ids.len() <= TOOL_OUTPUT_AGING_MAX_RESULTS);
    assert!(!batch.source_event_ids.iter().any(|event_id| {
        snapshot.items.iter().any(|item| {
            &item.source_event_id == event_id
                && item.retention != ToolOutputRetentionClassV1::Ageable
        })
    }));
    Ok(())
}

#[test]
fn selector_protects_current_and_recent_results_as_distinct_classes() -> Result<()> {
    let (_temp, mut session) = session_fixture()?;
    session.append_user_message(ModelMessage::user("start"))?;
    for index in 0..12 {
        append_result(&mut session, index, 100_000, true)?;
    }
    session.append_user_message(ModelMessage::user("next"))?;
    append_named_result(
        &mut session,
        "call-current",
        true,
        ToolResult::ok(
            "call-current",
            "shell",
            "current".repeat(20_000),
            ToolResultMeta::default(),
        ),
    )?;
    let snapshot = ToolOutputPressureProjectionV1::from_records(&records(&session)?)?.snapshot();

    assert!(snapshot.items.iter().any(|item| {
        item.call_id == "call-current" && item.retention == ToolOutputRetentionClassV1::CurrentTurn
    }));
    assert!(
        snapshot
            .items
            .iter()
            .any(|item| item.retention == ToolOutputRetentionClassV1::RecentProtected)
    );
    let batch = ToolOutputAgingBatchV1::select(&snapshot, ToolOutputAgingReasonV1::FitRequired)?
        .expect("older ordinary results should remain ageable");
    assert!(batch.source_event_ids.iter().all(|event_id| {
        snapshot.items.iter().any(|item| {
            &item.source_event_id == event_id
                && item.retention == ToolOutputRetentionClassV1::Ageable
        })
    }));
    Ok(())
}

#[test]
fn later_control_signals_incrementally_protect_error_approval_mutation_verification_and_provenance()
-> Result<()> {
    let (_temp, mut session) = session_fixture()?;
    session.append_user_message(ModelMessage::user("start"))?;

    append_named_result(
        &mut session,
        "call-error",
        true,
        ToolResult::ok(
            "call-error",
            "shell",
            "error-source".repeat(20_000),
            ToolResultMeta::default(),
        ),
    )?;
    session.append_control(ControlEntry::ToolExecution(Box::new(ToolExecutionEntry {
        call_id: "call-error".to_owned(),
        tool_name: "shell".to_owned(),
        status: ToolExecutionStatus::Failed,
        duration_ms: Some(10),
        subjects: Vec::new(),
        changed_files: Vec::new(),
        metadata: ToolResultMeta::default(),
        error: None,
        model_content_hash: None,
    })))?;

    append_named_result(
        &mut session,
        "call-approval",
        true,
        ToolResult::ok(
            "call-approval",
            "shell",
            "approval".repeat(20_000),
            ToolResultMeta::default(),
        ),
    )?;
    session.append_control(requested_approval("call-approval"))?;

    let prepared = MutationPrepared {
        operation_id: "mutation-operation-1".to_owned(),
        batch_id: None,
        tool_call_id: Some("call-mutation".to_owned()),
        causation_event_id: "mutation-cause-1".to_owned(),
        subject: MutationSubject::File {
            path: "src/lib.rs".into(),
            file_type: FileType::File,
        },
        before_hash: Some("before".to_owned()),
        intended_after_hash: Some("after".to_owned()),
        snapshot_coverage: SnapshotCoverage::NoPriorContent,
        workspace_id: "workspace-1".to_owned(),
        base_workspace_revision: 1,
        sync_class: MutationSyncClass::RecoveryCritical,
    };
    session.append_durable_event(
        DurableEventType::MutationPrepared,
        EventClass::Critical,
        serde_json::to_value(&prepared)?,
    )?;
    append_named_result(
        &mut session,
        "call-mutation",
        true,
        ToolResult::ok(
            "call-mutation",
            "shell",
            "mutation".repeat(20_000),
            ToolResultMeta::default(),
        ),
    )?;
    session.append_durable_event(
        DurableEventType::MutationCommitted,
        EventClass::Critical,
        serde_json::to_value(MutationCommitted {
            operation_id: prepared.operation_id.clone(),
            batch_id: None,
            workspace_id: Some("workspace-1".to_owned()),
            observed_after_hash: Some("after".to_owned()),
            workspace_revision: 2,
            workspace_snapshot_id: "snapshot-2".to_owned(),
            committed_subject: prepared.subject,
        })?,
    )?;

    append_named_result(
        &mut session,
        "call-verification",
        true,
        ToolResult::ok(
            "call-verification",
            "shell",
            "verification".repeat(20_000),
            ToolResultMeta::default(),
        ),
    )?;
    let verification_source = ToolOutputPressureProjectionV1::from_records(&records(&session)?)?
        .snapshot()
        .items
        .into_iter()
        .find(|item| item.call_id == "call-verification")
        .expect("verification result")
        .source_event_id;
    session.append_control(ControlEntry::VerificationRecorded(verification_for(
        "call-verification",
        &verification_source,
    )))?;

    let provenance_message_id = append_named_result(
        &mut session,
        "call-provenance",
        true,
        ToolResult::ok(
            "call-provenance",
            "shell",
            "provenance".repeat(20_000),
            ToolResultMeta::default(),
        ),
    )?;
    session.append_control(ControlEntry::ExternalProvenance(ExternalProvenanceEntry {
        session_scope_id: "session-test".to_owned(),
        message_id: provenance_message_id,
        trust: ExternalTrust::ExternalUntrusted,
        sources: Vec::new(),
        citations: Vec::new(),
    }))?;

    for index in 100..112 {
        append_result(&mut session, index, 100_000, true)?;
    }
    session.append_user_message(ModelMessage::user("next"))?;

    let all_records = records(&session)?;
    let full = ToolOutputPressureProjectionV1::from_records(&all_records)?.snapshot();
    for split in 0..=all_records.len() {
        let mut incremental = ToolOutputPressureProjectionV1::from_records(&all_records[..split])?;
        incremental.apply_records(&all_records[split..])?;
        assert_eq!(incremental.snapshot(), full, "split at record {split}");
    }

    for call_id in [
        "call-error",
        "call-approval",
        "call-mutation",
        "call-verification",
        "call-provenance",
    ] {
        let item = full
            .items
            .iter()
            .find(|item| item.call_id == call_id)
            .expect("high-signal result");
        assert!(item.high_signal, "{call_id} should retain its signal");
        assert_eq!(
            item.retention,
            ToolOutputRetentionClassV1::HighSignalProtected,
            "{call_id} must not become ageable"
        );
    }

    let error = full
        .items
        .iter()
        .find(|item| item.call_id == "call-error")
        .expect("error item");
    assert_eq!(error.facts.status, "error");
    let approval = full
        .items
        .iter()
        .find(|item| item.call_id == "call-approval")
        .expect("approval item");
    assert!(!approval.facts.approval_receipt_refs.is_empty());
    let mutation = full
        .items
        .iter()
        .find(|item| item.call_id == "call-mutation")
        .expect("mutation item");
    assert_eq!(
        mutation.facts.mutation_receipt_refs,
        ["mutation-operation-1"]
    );
    assert_eq!(mutation.facts.changed_files, ["src/lib.rs"]);
    let verification = full
        .items
        .iter()
        .find(|item| item.call_id == "call-verification")
        .expect("verification item");
    assert_eq!(
        verification.facts.verification_receipt_refs,
        ["verification-call-verification"]
    );
    let provenance = full
        .items
        .iter()
        .find(|item| item.call_id == "call-provenance")
        .expect("provenance item");
    assert!(!provenance.facts.external_provenance_refs.is_empty());

    let batch = ToolOutputAgingBatchV1::select(&full, ToolOutputAgingReasonV1::FitRequired)?
        .expect("ordinary filler results remain ageable");
    assert!(batch.source_event_ids.iter().all(|event_id| {
        full.items.iter().any(|item| {
            &item.source_event_id == event_id
                && item.retention == ToolOutputRetentionClassV1::Ageable
        })
    }));
    Ok(())
}

#[test]
fn cost_only_rejects_small_batch_while_fit_required_can_admit_it() -> Result<()> {
    let (_temp, mut session) = session_fixture()?;
    session.append_user_message(ModelMessage::user("start"))?;
    append_result(&mut session, 0, 10_000, true)?;
    append_result(&mut session, 1, 100_000, true)?;
    session.append_user_message(ModelMessage::user("next"))?;
    for index in 2..10 {
        append_result(&mut session, index, 100_000, true)?;
    }
    let snapshot = ToolOutputPressureProjectionV1::from_records(&records(&session)?)?.snapshot();

    assert!(
        ToolOutputAgingBatchV1::select(&snapshot, ToolOutputAgingReasonV1::CostOnly)?.is_none()
    );
    assert!(
        ToolOutputAgingBatchV1::select(&snapshot, ToolOutputAgingReasonV1::FitRequired)?.is_some()
    );
    Ok(())
}

#[test]
fn aging_activation_is_frontier_bound_and_replaces_only_the_next_epoch_view() -> Result<()> {
    let (_temp, mut session) = session_fixture()?;
    session.append_user_message(ModelMessage::user("start"))?;
    for index in 0..20 {
        append_result(&mut session, index, 100_000, true)?;
    }
    session.append_user_message(ModelMessage::user("next"))?;
    let store = session.durable_store().expect("durable session").clone();
    let active = store.active_projection_snapshot()?;
    let before = active.tool_output_pressure();
    let batch = ToolOutputAgingBatchV1::select(&before, ToolOutputAgingReasonV1::CostOnly)?
        .expect("large historical results should be ageable");
    let activation = ToolOutputAgingActivatedV1::prepare(&before, &batch)?;
    let target_epoch = activation.target_epoch_id.clone();
    let selected = activation
        .replacements
        .iter()
        .map(|replacement| replacement.source_event_id.clone())
        .collect::<BTreeSet<_>>();

    let appended = store
        .append_tool_output_aging_activation(active.frontier(), activation)?
        .expect("exact active frontier should publish the next epoch");
    assert_eq!(
        appended.event_kind(),
        Some(DurableEventType::ToolOutputAgingActivated)
    );

    let records = records(&session)?;
    let after = ToolOutputPressureProjectionV1::from_records(&records)?.snapshot();
    assert_eq!(after.active_epoch_id, target_epoch);
    assert!(after.total_tool_tokens < before.total_tool_tokens);
    assert!(
        after
            .items
            .iter()
            .filter(|item| {
                selected.contains(&item.source_event_id)
                    && item.retention == ToolOutputRetentionClassV1::Aged
            })
            .count()
            == selected.len()
    );

    let context =
        SessionContextProjection::from_durable_records(session.entries(), &records, None)?;
    let aged_messages = context
        .model_messages()
        .into_iter()
        .filter(|message| {
            message
                .content
                .as_deref()
                .is_some_and(|content| content.contains("\"preview_kind\":\"aged\""))
        })
        .count();
    assert_eq!(aged_messages, selected.len());
    Ok(())
}

#[test]
fn aging_activation_cas_rejects_a_changed_frontier_without_full_replay() -> Result<()> {
    let (_temp, mut session) = session_fixture()?;
    session.append_user_message(ModelMessage::user("start"))?;
    for index in 0..20 {
        append_result(&mut session, index, 100_000, true)?;
    }
    session.append_user_message(ModelMessage::user("next"))?;
    let store = session.durable_store().expect("durable session").clone();
    let active = store.active_projection_snapshot()?;
    let pressure = active.tool_output_pressure();
    let batch = ToolOutputAgingBatchV1::select(&pressure, ToolOutputAgingReasonV1::CostOnly)?
        .expect("large historical results should be ageable");
    let activation = ToolOutputAgingActivatedV1::prepare(&pressure, &batch)?;

    session.append_user_message(ModelMessage::user("frontier changed"))?;
    assert!(
        store
            .append_tool_output_aging_activation(active.frontier(), activation)?
            .is_none()
    );
    Ok(())
}

#[test]
fn availability_disable_event_denies_retrieval_binding() -> Result<()> {
    // RFC-0062 9.4: after a durable Available -> DisabledPendingDelete transition, the pressure
    // projection's retrieval bindings must report the artifact as expired so no surface admits
    // a read; the initial descriptor availability is never trusted alone.
    let (_temp, mut session) = session_fixture()?;
    session.append_user_message(ModelMessage::user("start"))?;
    append_result(&mut session, 0, 100, true)?;
    let snapshot = session
        .active_projection_snapshot()?
        .expect("durable projection")
        .tool_output_pressure();
    let binding = snapshot
        .artifact_source_bindings()?
        .into_iter()
        .next()
        .expect("one binding");
    assert_eq!(
        binding.artifact_availability,
        crate::ToolArtifactAvailability::Available
    );
    let artifact_ref = binding.artifact_ref.clone();

    session.append_artifact_availability_transition(
        &artifact_ref,
        0,
        crate::ToolArtifactAvailabilityStateV1::Available,
        crate::ToolArtifactAvailabilityStateV1::DisabledPendingDelete,
        crate::ToolArtifactAvailabilityReasonV1::GcDisable,
        1,
    )?;
    let snapshot = session
        .active_projection_snapshot()?
        .expect("durable projection")
        .tool_output_pressure();
    assert_eq!(
        snapshot
            .artifact_source_bindings()?
            .into_iter()
            .next()
            .expect("binding still present")
            .artifact_availability,
        crate::ToolArtifactAvailability::Expired
    );

    // A stale or out-of-order transition must fail closed instead of being applied.
    let error = session
        .append_control(ControlEntry::ToolArtifactAvailabilityChanged(
            crate::ToolArtifactAvailabilityChangedV1 {
                schema_version: super::super::TOOL_ARTIFACT_AVAILABILITY_CHANGED_SCHEMA_VERSION,
                artifact_ref: artifact_ref.clone(),
                expected_generation: 0,
                generation: 1,
                previous: crate::ToolArtifactAvailabilityStateV1::Available,
                next: crate::ToolArtifactAvailabilityStateV1::Missing,
                reason: crate::ToolArtifactAvailabilityReasonV1::ReaderDetectedMissing,
                changed_at_ms: 2,
            },
        ))
        .and_then(|()| {
            session
                .active_projection_snapshot()?
                .expect("durable projection")
                .tool_output_pressure()
                .artifact_source_bindings()
                .map(|_| ())
        });
    assert!(
        error.is_err(),
        "a transition that skips the current state must fail the projection"
    );
    Ok(())
}

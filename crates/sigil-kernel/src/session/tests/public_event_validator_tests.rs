use super::*;
use crate::{
    ConversationRunFinalizedEntryV1, ConversationRunStartedEntryV1, ConversationRunTerminalStatusV1,
};

fn incremental(records: &[SessionStreamRecord]) -> Result<PublicEventOutboxValidatorV1> {
    let mut validator = PublicEventOutboxValidatorV1::default();
    for record in records {
        validator.apply_record(record)?;
    }
    validator.validate_cut()?;
    Ok(validator)
}

fn assert_invalid(records: &[SessionStreamRecord]) {
    assert!(
        PublicEventOutboxProjectionV1::from_records(records).is_err(),
        "canonical replay accepted corrupt fixture"
    );
    assert!(
        incremental(records).is_err(),
        "incremental validator accepted corrupt fixture"
    );
}

fn mutate_record(
    records: &[SessionStreamRecord],
    index: usize,
    change: impl FnOnce(&mut StoredEvent),
) -> Result<Vec<SessionStreamRecord>> {
    let mut changed = records.to_vec();
    let mut event = changed[index].stored_event().clone();
    change(&mut event);
    event.record_checksum = event.compute_record_checksum()?;
    changed[index] = SessionStreamRecord::Stored(event);
    Ok(changed)
}

#[test]
fn incremental_validator_enforces_root_lifecycle_and_terminal_pair_at_fixed_cut() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let store = JsonlSessionStore::new(temp.path().join("session.jsonl"))?;
    let session = Session::new("test", "model").with_store(store.clone());
    let recorder = session.conversation_run_lifecycle_recorder()?;
    recorder.append_started(&ConversationRunStartedEntryV1::new("run-1", 1)?)?;
    let terminal = ConversationRunFinalizedEntryV1::new(
        "run-1",
        ConversationRunTerminalStatusV1::Succeeded,
        Some("final-message".to_owned()),
        Some("done"),
        2,
        &crate::SecretRedactor::default(),
    )?;
    let outbox = crate::conversation_run::test_fixtures::terminal_outbox(
        session.session_scope_id(),
        &terminal,
    )?;
    recorder.append_finalized_with_outbox(&terminal, &outbox)?;
    let records = store.read_event_records_writer()?;
    PublicEventOutboxProjectionV1::from_records(&records)?;
    let mut validator = PublicEventOutboxValidatorV1::default();
    validator.apply_record(&records[0])?;
    validator.validate_cut()?;
    validator.apply_record(&records[1])?;
    assert!(validator.validate_cut().is_err());
    validator.apply_record(&records[2])?;
    validator.validate_cut()?;
    assert_eq!(validator.durable_sequence("run-1"), 1);
    assert_invalid(&records[1..]);
    assert_invalid(&records[..2]);
    let changed = mutate_record(&records, 1, |event| {
        event.payload["run_id"] = serde_json::json!("other-run");
    })?;
    assert_invalid(&changed);
    Ok(())
}

#[test]
fn incremental_validator_preserves_source_dto_count_and_causation_across_batches() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let store = JsonlSessionStore::new(temp.path().join("session.jsonl"))?;
    let mut session = Session::new("test", "model").with_store(store.clone());
    session
        .conversation_run_lifecycle_recorder()?
        .append_started(&ConversationRunStartedEntryV1::new("run-1", 1)?)?;
    session.append_controls_with_public_outbox(
        vec![ControlEntry::TaskPlan(crate::TaskPlanEntry {
            task_id: crate::TaskId::new("task")?,
            plan_version: 1,
            status: crate::TaskPlanStatus::Accepted,
            steps: Vec::new(),
            reason: None,
        })],
        "run-1",
        1,
    )?;
    let records = store.read_event_records_writer()?;
    PublicEventOutboxProjectionV1::from_records(&records)?;
    assert_eq!(incremental(&records)?.durable_sequence("run-1"), 2);
    assert_invalid(&records[..3]);
    assert_invalid(&mutate_record(&records, 2, |event| {
        event.causation_id = Some("wrong-source".to_owned())
    })?);
    let mut public: PublicEventOutboxEntryV1 =
        serde_json::from_value(records[2].stored_event().payload.clone())?;
    public.event.event = PublicRunEventKind::Notice {
        message: "forged".to_owned(),
    };
    public.payload_digest = stable_event_hash(serde_json::to_vec(&public.event)?);
    assert_invalid(&mutate_record(&records, 2, |event| {
        event.payload = serde_json::to_value(public).expect("public encodes")
    })?);
    Ok(())
}

#[test]
fn incremental_validator_keeps_revision_predecessor_and_waiting_pair_checks() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let (mut session, attempt, draft, decision) =
        super::super::plan_review_terminal::tests::fixture(&temp.path().join("terminal.jsonl"))?;
    let event = crate::PublicRunEvent::new(
        session.session_scope_id().to_owned(),
        super::super::plan_review_terminal::plan_review_revision_run_id(&attempt),
        1,
        PublicRunEventKind::RunFinished {
            final_text: "done".to_owned(),
        },
    );
    session.append_plan_review_revision_terminal(attempt, Some(draft), decision, event)?;
    let records = session
        .durable_store()
        .expect("store")
        .read_event_records_writer()?;
    PublicEventOutboxProjectionV1::from_records(&records)?;
    incremental(&records)?;
    assert_invalid(&records[..records.len() - 1]);
    let started_index = records.iter().position(|record| matches!(record.session_log_entry(), Ok(Some(SessionLogEntry::Control(ControlEntry::PlanReviewAttempt(attempt)))) if attempt.status == crate::PlanReviewAttemptStatus::Started)).expect("started");
    let changed = mutate_record(&records, started_index, |event| {
        event.payload["session_log_entry"]["control"]["plan_review_attempt"]["explicit_objective"] =
            serde_json::json!("different");
    })?;
    assert_invalid(&changed);
    let (mut waiting_session, waiting, event) =
        super::super::plan_review_waiting::tests::waiting_fixture(
            &temp.path().join("waiting.jsonl"),
        )?;
    waiting_session.append_plan_review_revision_waiting(waiting, event)?;
    let waiting_records = waiting_session
        .durable_store()
        .expect("store")
        .read_event_records_writer()?;
    PublicEventOutboxProjectionV1::from_records(&waiting_records)?;
    incremental(&waiting_records)?;
    assert_invalid(&waiting_records[..waiting_records.len() - 1]);
    Ok(())
}

#[test]
fn incremental_validator_rejects_duplicate_and_overlapping_root_starts() -> Result<()> {
    for second_run in ["run-1", "run-2"] {
        let records = ["run-1", second_run]
            .into_iter()
            .enumerate()
            .map(|(index, run)| {
                StoredEvent::new(
                    DurableEventType::RunStatusChanged,
                    EventClass::Critical,
                    format!("start-{index}"),
                    "session".to_owned(),
                    index as u64 + 1,
                    serde_json::to_value(
                        ConversationRunLifecycleRecordV1::ConversationRunStartedV1(
                            ConversationRunStartedEntryV1::new(run, index as u64 + 1)?,
                        ),
                    )?,
                )
                .map(SessionStreamRecord::Stored)
            })
            .collect::<Result<Vec<_>>>()?;
        assert_invalid(&records);
    }
    Ok(())
}

#[test]
fn incremental_validator_compares_explicit_assistant_content_with_its_source() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let store = JsonlSessionStore::new(temp.path().join("assistant.jsonl"))?;
    let mut session = Session::new("test", "model").with_store(store.clone());
    session
        .conversation_run_lifecycle_recorder()?
        .append_started(&ConversationRunStartedEntryV1::new("run-1", 1)?)?;
    let message = crate::ModelMessage::assistant(Some("exact source body".to_owned()), Vec::new());
    session.append_session_entries_with_public_outbox(
        vec![SessionLogEntry::Assistant(message.clone())],
        vec![SessionPublicEventProjectionV1::assistant_message(
            0, message,
        )],
        "run-1",
        1,
    )?;
    let records = store.read_event_records_writer()?;
    PublicEventOutboxProjectionV1::from_records(&records)?;
    incremental(&records)?;
    let mut public: PublicEventOutboxEntryV1 =
        serde_json::from_value(records[2].stored_event().payload.clone())?;
    let PublicRunEventKind::AssistantMessage { message } = &mut public.event.event else {
        panic!("assistant event");
    };
    message.content = Some("forged body".to_owned());
    public.payload_digest = stable_event_hash(serde_json::to_vec(&public.event)?);
    assert_invalid(&mutate_record(&records, 2, |event| {
        event.payload = serde_json::to_value(public).expect("public encodes")
    })?);
    Ok(())
}

#[test]
fn revision_candidate_resolution_stays_inside_the_exact_terminal_bundle() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let (mut session, attempt, draft, decision) =
        super::super::plan_review_terminal::tests::fixture(
            &temp.path().join("resolved-candidate.jsonl"),
        )?;
    let candidate = crate::plan_review_candidate_recorded_entry(
        attempt.plan_review_id.clone(),
        attempt.attempt_id.clone(),
        attempt.plan_id.clone(),
        draft.source.clone(),
        Some("candidate-source".to_owned()),
        draft.inline_text.as_deref().expect("plain text draft"),
        crate::PlanReviewCandidateCompletenessV1::Complete,
        3,
    )?;
    session.append_control(ControlEntry::PlanReviewCandidateRecordedV1(Box::new(
        candidate,
    )))?;
    let event = crate::PublicRunEvent::new(
        session.session_scope_id().to_owned(),
        super::super::plan_review_terminal::plan_review_revision_run_id(&attempt),
        1,
        PublicRunEventKind::RunFinished {
            final_text: "done".to_owned(),
        },
    );
    session.append_plan_review_revision_terminal(attempt, Some(draft), decision, event)?;
    let records = session
        .durable_store()
        .expect("store")
        .read_event_records_writer()?;
    PublicEventOutboxProjectionV1::from_records(&records)?;
    incremental(&records)?;
    let resolution_index = records
        .iter()
        .position(|record| {
            matches!(
                record.session_log_entry(),
                Ok(Some(SessionLogEntry::Control(
                    ControlEntry::PlanReviewResolutionRecordedV1(_)
                )))
            )
        })
        .expect("writer resolution");
    let changed = mutate_record(&records, resolution_index, |event| {
        event.payload["session_log_entry"]["control"]["plan_review_resolution_recorded_v1"]["candidate_hash"] =
            serde_json::json!("wrong-candidate");
    })?;
    assert_invalid(&changed);
    Ok(())
}

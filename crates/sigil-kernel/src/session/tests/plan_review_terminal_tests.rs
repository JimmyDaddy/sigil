use super::*;
use crate::{
    ConversationTurnRef, PlanId, PlanReviewAttemptId, PlanReviewId, PlanReviewSource,
    PlanSourceRef, UserInputRequestId,
};

fn fixture(
    path: &Path,
) -> Result<(
    Session,
    PlanReviewAttemptEntry,
    PlanDraftCreatedEntry,
    PlanDecisionRecordedEntry,
)> {
    let store = JsonlSessionStore::new(path)?;
    let mut session = Session::load_from_store("mock", "mock", store)?;
    let turn = ConversationTurnRef::new(session.session_scope_id(), "message-1", "origin-run")?;
    let review_id = PlanReviewId::new("review-1")?;
    let attempt_id = PlanReviewAttemptId::new("revision-1")?;
    let source = PlanSourceRef {
        source_turn: Some(turn.clone()),
        plan_review_id: Some(review_id.clone()),
        ..PlanSourceRef::default()
    };
    let base = crate::plain_text_plan_draft_entry_with_plan_id(
        PlanId::new("base-1")?,
        "Base plan",
        source.clone(),
        1,
        None,
    )?
    .expect("nonempty base");
    let draft = crate::plain_text_plan_draft_entry_with_plan_id(
        PlanId::new("candidate-1")?,
        "Revised plan",
        source,
        3,
        None,
    )?
    .expect("nonempty revision");
    let decision = PlanDecisionRecordedEntry {
        plan_id: base.plan_id.clone(),
        plan_hash: base.plan_hash.clone(),
        decision: PlanDecision::RevisionSucceeded,
        decided_by: PlanDecisionActor::System,
        decided_at_ms: 4,
        reason: None,
    };
    let attempt = PlanReviewAttemptEntry {
        plan_review_id: review_id.clone(),
        attempt_id: attempt_id.clone(),
        plan_id: draft.plan_id.clone(),
        source: PlanReviewSource::ExplicitPlanCommand,
        source_turn: turn,
        explicit_objective: Some("Original plan objective".to_owned()),
        route_decision_id: None,
        child_session_ref: crate::plan_review_child_session_ref(&review_id, &attempt_id),
        finalizer_session_ref: Some(crate::plan_review_finalizer_session_ref(
            &review_id,
            &attempt_id,
            1,
        )),
        revision_request_id: Some(UserInputRequestId::new("request-1")?),
        attempt_ordinal: 1,
        base_plan_id: Some(base.plan_id.clone()),
        base_plan_hash: Some(base.plan_hash.clone()),
        workspace_snapshot_id: None,
        pending_user_input: None,
        status: PlanReviewAttemptStatus::DraftReady,
        terminal_reason: None,
        recorded_at_ms: 4,
    };
    session.append_controls(vec![
        ControlEntry::PlanDraftCreated(base),
        ControlEntry::PlanDecisionRecorded(PlanDecisionRecordedEntry {
            decision: PlanDecision::RevisionRequested,
            decided_by: PlanDecisionActor::User,
            decided_at_ms: 2,
            ..decision.clone()
        }),
        ControlEntry::PlanReviewAttempt(PlanReviewAttemptEntry {
            status: PlanReviewAttemptStatus::Started,
            recorded_at_ms: 2,
            ..attempt.clone()
        }),
    ])?;
    Ok((session, attempt, draft, decision))
}

fn event(
    session: &Session,
    attempt: &PlanReviewAttemptEntry,
    kind: PublicRunEventKind,
) -> PublicRunEvent {
    PublicRunEvent::new(
        session.session_scope_id().to_owned(),
        plan_review_revision_run_id(attempt),
        7,
        kind,
    )
}

#[test]
fn saving_unstarted_revision_requires_no_attempt_and_fences_delayed_start() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let (mut started, attempt, _, decision) = fixture(&temp.path().join("started.jsonl"))?;
    let saved = PlanDecisionRecordedEntry {
        decision: PlanDecision::SavedOnly,
        decided_by: PlanDecisionActor::User,
        decided_at_ms: 5,
        ..decision
    };
    assert!(
        started
            .append_control(ControlEntry::PlanDecisionRecorded(saved.clone()))
            .is_err(),
        "a started revision cannot be discarded through the unstarted Save recovery"
    );

    let store = JsonlSessionStore::new(temp.path().join("unstarted.jsonl"))?;
    let mut unstarted = Session::load_from_store("mock", "mock", store)?;
    let prefix = started
        .entries()
        .iter()
        .filter_map(|entry| match entry {
            SessionLogEntry::Control(control @ ControlEntry::PlanDraftCreated(_))
            | SessionLogEntry::Control(control @ ControlEntry::PlanDecisionRecorded(_)) => {
                Some(control.clone())
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    unstarted.append_controls(prefix)?;
    unstarted.append_control(ControlEntry::PlanDecisionRecorded(saved))?;
    let delayed = PlanReviewAttemptEntry {
        source_turn: ConversationTurnRef::new(
            unstarted.session_scope_id(),
            "message-1",
            "origin-run",
        )?,
        status: PlanReviewAttemptStatus::Started,
        recorded_at_ms: 6,
        ..attempt
    };
    let error = unstarted
        .append_control(ControlEntry::PlanReviewAttempt(delayed))
        .expect_err("a delayed prepared revision cannot start after the user saved its base");
    assert!(error.to_string().contains("exact pending base decision"));
    Ok(())
}

#[test]
fn revision_bundle_recovers_original_success_after_each_torn_append() -> Result<()> {
    for fault in [
        SessionWriterFault::BeforeWrite,
        SessionWriterFault::PartialFirstRecord,
        SessionWriterFault::PartialSecondRecord,
        SessionWriterFault::BeforeSync,
    ] {
        let temp = tempfile::tempdir()?;
        let path = temp.path().join("session.jsonl");
        let (mut session, attempt, draft, decision) = fixture(&path)?;
        let run_id = plan_review_revision_run_id(&attempt);
        let public = event(
            &session,
            &attempt,
            PublicRunEventKind::RunFinished {
                final_text: "Original result".into(),
            },
        );
        session
            .durable_store()
            .expect("store")
            .inject_writer_fault(fault)?;
        assert!(
            session
                .append_plan_review_revision_terminal(
                    attempt.clone(),
                    Some(draft.clone()),
                    decision.clone(),
                    public.clone()
                )
                .is_err(),
            "{fault:?}"
        );
        assert!(
            !session
                .plan_artifact_projection()
                .plans
                .contains_key(&draft.plan_id)
        );
        let recovered = session
            .reconcile_plan_review_revision_terminal(&run_id)?
            .expect("original pair");
        assert_eq!(
            serde_json::to_value(&recovered.event)?,
            serde_json::to_value(&public)?
        );
        assert_eq!(
            session.plan_artifact_projection().plans.get(&draft.plan_id),
            Some(&draft)
        );
        assert_eq!(
            session
                .plan_artifact_projection()
                .latest_decision(&decision.plan_id)
                .expect("base decision")
                .decision,
            PlanDecision::RevisionSucceeded
        );
        let repeated =
            session.append_plan_review_revision_terminal(attempt, Some(draft), decision, public)?;
        assert_eq!(repeated.public_event_id, recovered.public_event_id);
        drop(session);
        let store = JsonlSessionStore::new(&path)?;
        let mut reopened = Session::load_from_store("mock", "mock", store)?;
        assert_eq!(
            reopened
                .reconcile_plan_review_revision_terminal(&run_id)?
                .expect("replayed outbox")
                .payload_digest,
            recovered.payload_digest
        );
        let records = JsonlSessionStore::read_event_records(&path)?;
        assert_eq!(
            records
                .iter()
                .filter(|record| record.stored_event().event_id == recovered.domain_event_id)
                .count(),
            1
        );
        assert_eq!(
            records
                .iter()
                .filter(|record| record.stored_event().event_id == recovered.public_event_id)
                .count(),
            1
        );
    }
    Ok(())
}

#[test]
fn revision_terminal_rejects_cross_identity_status_and_split_history() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let (mut session, attempt, draft, decision) = fixture(&temp.path().join("session.jsonl"))?;
    let good = event(
        &session,
        &attempt,
        PublicRunEventKind::RunFinished {
            final_text: "ready".into(),
        },
    );
    let wrong_status = event(
        &session,
        &attempt,
        PublicRunEventKind::RunFailed {
            error: "wrong".into(),
        },
    );
    assert!(
        session
            .append_plan_review_revision_terminal(
                attempt.clone(),
                Some(draft.clone()),
                decision.clone(),
                wrong_status
            )
            .is_err()
    );
    let mut wrong = attempt.clone();
    wrong.child_session_ref = crate::SessionRef::new_relative("other.jsonl")?;
    assert!(
        session
            .append_plan_review_revision_terminal(
                wrong,
                Some(draft.clone()),
                decision.clone(),
                good.clone()
            )
            .is_err()
    );
    // Only a raw corrupted-history fixture may create the split candidate; the Session API
    // rejects it before reaching the writer.
    session
        .durable_store()
        .expect("store")
        .append(&SessionLogEntry::Control(ControlEntry::PlanDraftCreated(
            draft.clone(),
        )))?;
    assert!(
        session
            .append_plan_review_revision_terminal(attempt, Some(draft), decision, good)
            .is_err()
    );
    Ok(())
}

#[test]
fn revision_outbox_requires_both_original_envelopes_and_decision() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let (mut session, attempt, draft, decision) = fixture(&temp.path().join("session.jsonl"))?;
    let public = event(
        &session,
        &attempt,
        PublicRunEventKind::RunFinished {
            final_text: "ready".into(),
        },
    );
    let outbox =
        session.append_plan_review_revision_terminal(attempt, Some(draft), decision, public)?;
    let records = session
        .durable_store()
        .expect("store")
        .read_event_records_writer()?;
    for id in [&outbox.domain_event_id, &outbox.public_event_id] {
        let incomplete = records
            .iter()
            .filter(|record| &record.stored_event().event_id != id)
            .cloned()
            .collect::<Vec<_>>();
        assert!(PublicEventOutboxProjectionV1::from_records(&incomplete).is_err());
    }
    let without_decision = records
        .iter()
        .filter(|record| {
            !matches!(record.session_log_entry(),
        Ok(Some(SessionLogEntry::Control(ControlEntry::PlanDecisionRecorded(ref decision))))
            if decision.decision == PlanDecision::RevisionSucceeded)
        })
        .cloned()
        .collect::<Vec<_>>();
    assert!(PublicEventOutboxProjectionV1::from_records(&without_decision).is_err());
    Ok(())
}

#[test]
fn nonrevision_attempt_cannot_back_a_terminal_outbox() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let (session, mut attempt, _, _) = fixture(&temp.path().join("source.jsonl"))?;
    let path = temp.path().join("corrupt.jsonl");
    let store = JsonlSessionStore::new(&path)?;
    attempt.revision_request_id = None;
    attempt.base_plan_id = None;
    attempt.base_plan_hash = None;
    attempt.status = PlanReviewAttemptStatus::CompletedWithoutDraft;
    let public = event(
        &session,
        &attempt,
        PublicRunEventKind::RunFinished {
            final_text: "not a revision terminal".into(),
        },
    );
    let outbox = PublicEventOutboxEntryV1 {
        schema_version: PUBLIC_EVENT_OUTBOX_SCHEMA_VERSION,
        domain_event_id: stable_event_uuid("test-domain", "nonrevision"),
        public_event_id: stable_event_uuid("test-public", "nonrevision"),
        run_id: public.run_id.clone(),
        sequence: public.sequence,
        payload_digest: stable_event_hash(serde_json::to_vec(&public)?),
        event: public,
    };
    // Deliberately bypass Session's semantic API to persist a raw historical corruption.
    store.append_crash_safe_events_if(
        vec![
            (
                outbox.domain_event_id.clone(),
                DurableEventType::PlanReviewAttempt,
                EventClass::Critical,
                serde_json::json!({ "session_log_entry": SessionLogEntry::Control(
                    ControlEntry::PlanReviewAttempt(attempt)
                ) }),
            ),
            (
                outbox.public_event_id.clone(),
                DurableEventType::PublicEventOutbox,
                EventClass::Critical,
                serde_json::to_value(outbox)?,
            ),
        ],
        |_| Ok(true),
    )?;
    let records = JsonlSessionStore::read_event_records(&path)?;
    assert_eq!(records.len(), 2);
    let error = PublicEventOutboxProjectionV1::from_records(&records)
        .expect_err("a nonrevision attempt cannot bypass the terminal pair validators");
    assert!(error.to_string().contains("no exact finalized attempt"));
    Ok(())
}

#[test]
fn genuine_unfinished_revision_recovers_interrupted_not_child_success() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("session.jsonl");
    let (session, attempt, draft, _) = fixture(&path)?;
    let child_path = attempt
        .finalizer_session_ref
        .as_ref()
        .expect("finalizer ref")
        .resolve(temp.path());
    let mut child = Session::new("mock", "mock").with_store(JsonlSessionStore::new(child_path)?);
    child.append_control(ControlEntry::PlanDraftCreated(draft.clone()))?;
    drop(child);
    drop(session);
    let mut reopened = Session::load_from_store("mock", "mock", JsonlSessionStore::new(&path)?)?;
    let terminal = reopened
        .reconcile_plan_review_revision_terminal(&plan_review_revision_run_id(&attempt))?
        .expect("interrupted outbox");
    assert!(matches!(
        terminal.event.event,
        PublicRunEventKind::RunInterrupted { .. }
    ));
    assert!(
        !reopened
            .plan_artifact_projection()
            .plans
            .contains_key(&draft.plan_id)
    );
    Ok(())
}

#[test]
fn retry_with_later_decision_time_preserves_original_bundle_bytes() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("session.jsonl");
    let (mut session, attempt, draft, mut decision) = fixture(&path)?;
    let public = event(
        &session,
        &attempt,
        PublicRunEventKind::RunFinished {
            final_text: "ready".into(),
        },
    );
    let first = session.append_plan_review_revision_terminal(
        attempt.clone(),
        Some(draft.clone()),
        decision.clone(),
        public.clone(),
    )?;
    let original = fs::read(&path)?;
    decision.decided_at_ms = 50;
    let retried =
        session.append_plan_review_revision_terminal(attempt, Some(draft), decision, public)?;
    assert_eq!(first.public_event_id, retried.public_event_id);
    assert_eq!(original, fs::read(&path)?);
    Ok(())
}

#[test]
fn unbacked_session_and_stale_adoption_cannot_claim_durable_terminal() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let (mut session, attempt, draft, decision) = fixture(&temp.path().join("session.jsonl"))?;
    let started = PlanReviewProjection::from_entries(session.entries())
        .latest_attempt(&attempt.plan_review_id)
        .expect("started revision")
        .clone();
    let before_start = session
        .entries()
        .iter()
        .filter(|entry| {
            !matches!(
                entry,
                SessionLogEntry::Control(ControlEntry::PlanReviewAttempt(_))
            )
        })
        .cloned()
        .collect::<Vec<_>>();
    let mut reordered = Session::from_entries("mock", "mock", before_start);
    let before = serde_json::to_value(reordered.entries())?;
    let reverse_batch_error = reordered
        .append_controls(vec![
            ControlEntry::PlanDraftCreated(draft.clone()),
            ControlEntry::PlanReviewAttempt(started),
        ])
        .expect_err("candidate-before-Started must not bypass the atomic writer");
    assert!(
        reverse_batch_error
            .to_string()
            .contains("independently committed candidate")
    );
    assert_eq!(serde_json::to_value(reordered.entries())?, before);
    let public = event(
        &session,
        &attempt,
        PublicRunEventKind::RunFinished {
            final_text: "ready".into(),
        },
    );
    let mut unbacked = Session::new("mock", "mock");
    let mut unrelated = public.clone();
    unrelated.session_id = unbacked.session_scope_id().to_owned();
    let mut unbacked_attempt = attempt.clone();
    unbacked_attempt.source_turn.session_scope_id = unbacked.session_scope_id().to_owned();
    assert!(
        unbacked
            .append_plan_review_revision_terminal(
                unbacked_attempt,
                Some(draft.clone()),
                decision.clone(),
                unrelated
            )
            .is_err()
    );
    assert!(
        session
            .append_control(ControlEntry::PlanDraftCreated(draft))
            .is_err(),
        "a revision candidate cannot bypass the atomic writer"
    );
    assert!(
        session
            .append_control(ControlEntry::PlanDecisionRecorded(decision.clone()))
            .is_err(),
        "a revision success decision cannot bypass the atomic writer"
    );
    let mut failed = decision;
    failed.decision = PlanDecision::RevisionFailed;
    assert!(
        session
            .append_control(ControlEntry::PlanDecisionRecorded(failed))
            .is_err(),
        "a revision failure decision cannot bypass the atomic writer"
    );
    assert!(
        session
            .append_control(ControlEntry::PlanReviewAttempt(attempt))
            .is_err(),
        "standalone revision terminal write must be removed"
    );
    let records = session
        .durable_store()
        .expect("store")
        .read_event_records_writer()?;
    session.append_user_message(crate::ModelMessage::user("interleaved"))?;
    assert!(
        session
            .adopt_plan_review_recovery_records(&records)
            .is_err()
    );
    assert!(session.entries().iter().any(|entry| matches!(entry,
        SessionLogEntry::User(message) if message.content.as_deref() == Some("interleaved"))));
    Ok(())
}

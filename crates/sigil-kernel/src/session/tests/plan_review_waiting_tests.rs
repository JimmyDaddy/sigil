use super::*;
use crate::{
    AgentThreadId, ConversationTurnRef, LogicalRunId, PlanDecision, PlanDecisionActor, PlanId,
    PlanReviewAttemptId, PlanReviewId, PlanReviewSource, PlanSourceRef, PublicUserInputRequestV1,
    SessionScopeId, UserInputActionV1, UserInputIdentityV1, UserInputPurposeV1, UserInputRequestId,
    UserInputSourceV1, UserInputStatusV1,
};

fn waiting_fixture(path: &Path) -> Result<(Session, PlanReviewAttemptEntry, PublicRunEvent)> {
    let mut session = Session::load_from_store("test", "model", JsonlSessionStore::new(path)?)?;
    let review = PlanReviewId::new("review")?;
    let attempt_id = PlanReviewAttemptId::new("revision")?;
    let base = crate::plain_text_plan_draft_entry_with_plan_id(
        PlanId::new("base")?,
        "Original plan",
        PlanSourceRef::default(),
        1,
        None,
    )?
    .expect("nonempty plan");
    let mut attempt = PlanReviewAttemptEntry {
        plan_review_id: review.clone(),
        attempt_id: attempt_id.clone(),
        plan_id: PlanId::new("candidate")?,
        source: PlanReviewSource::ExplicitPlanCommand,
        source_turn: ConversationTurnRef::new(
            session.session_scope_id(),
            "source",
            "original-run",
        )?,
        explicit_objective: Some("Original objective".to_owned()),
        route_decision_id: None,
        child_session_ref: crate::plan_review_child_session_ref(&review, &attempt_id),
        finalizer_session_ref: None,
        revision_request_id: Some(UserInputRequestId::new("revision-request")?),
        attempt_ordinal: 1,
        base_plan_id: Some(base.plan_id.clone()),
        base_plan_hash: Some(base.plan_hash.clone()),
        workspace_snapshot_id: None,
        pending_user_input: None,
        status: PlanReviewAttemptStatus::Started,
        terminal_reason: None,
        recorded_at_ms: 2,
    };
    session.append_controls(vec![
        ControlEntry::PlanDraftCreated(base.clone()),
        ControlEntry::PlanDecisionRecorded(crate::PlanDecisionRecordedEntry {
            plan_id: base.plan_id,
            plan_hash: base.plan_hash,
            decision: PlanDecision::RevisionRequested,
            decided_by: PlanDecisionActor::User,
            decided_at_ms: 2,
            reason: None,
        }),
        ControlEntry::PlanReviewAttempt(attempt.clone()),
    ])?;
    let run_id = super::super::plan_review_terminal::plan_review_revision_run_id(&attempt);
    let request_hash = stable_event_hash(b"exact public request");
    attempt.status = PlanReviewAttemptStatus::WaitingForInput;
    attempt.recorded_at_ms = 3;
    attempt.pending_user_input = Some(Box::new(PublicUserInputRequestV1 {
        identity: UserInputIdentityV1 {
            session_scope_id: SessionScopeId::new("child-session")?,
            root_logical_run_id: LogicalRunId::new(&run_id)?,
            source_thread_id: AgentThreadId::new("main")?,
            request_id: UserInputRequestId::new("question")?,
            generation: 1,
            source_binding_hash: stable_event_hash(b"child binding"),
        },
        request_hash: request_hash.clone(),
        source: UserInputSourceV1::PlanReviewResearch {
            plan_review_id: review,
            attempt_id,
        },
        purpose: UserInputPurposeV1::Clarification,
        prompt: "Which scope?".to_owned(),
        questions: vec![crate::UserInputQuestionV1 {
            id: "scope".to_owned(),
            header: "Scope".to_owned(),
            question: "Which scope?".to_owned(),
            description: None,
            required: true,
            field: crate::UserInputFieldKindV1::Text {
                multiline: false,
                max_chars: 256,
            },
        }],
        allowed_actions: vec![UserInputActionV1::Submit, UserInputActionV1::CancelRun],
        requested_at_unix_ms: 3,
        status: UserInputStatusV1::Requested,
        answer_receipt: None,
        resolution: None,
    }));
    let event = PublicRunEvent::new(
        session.session_scope_id().to_owned(),
        run_id,
        1,
        PublicRunEventKind::RunAwaitingUserInput {
            request_id: "question".to_owned(),
            generation: 1,
            request_hash,
        },
    );
    Ok((session, attempt, event))
}

#[test]
fn revision_waiting_recovers_exact_pair_after_torn_append_and_reopen() -> Result<()> {
    for fault in [
        SessionWriterFault::BeforeWrite,
        SessionWriterFault::PartialFirstRecord,
        SessionWriterFault::PartialSecondRecord,
        SessionWriterFault::BeforeSync,
    ] {
        let temp = tempfile::tempdir()?;
        let path = temp.path().join("session.jsonl");
        let (mut session, attempt, event) = waiting_fixture(&path)?;
        session
            .durable_store()
            .expect("store")
            .inject_writer_fault(fault)?;
        assert!(
            session
                .append_plan_review_revision_waiting(attempt.clone(), event.clone())
                .is_err()
        );
        drop(session);
        let mut reopened =
            Session::load_from_store("test", "model", JsonlSessionStore::new(&path)?)?;
        let recovered = reopened
            .reconcile_plan_review_revision_waiting(&event.run_id)?
            .expect("waiting pair");
        assert_eq!(
            serde_json::to_value(&recovered.event)?,
            serde_json::to_value(&event)?
        );
        assert!(
            reopened
                .reconcile_plan_review_revision_terminal(&event.run_id)?
                .is_none()
        );
        let repeated =
            reopened.append_plan_review_revision_waiting(attempt.clone(), event.clone())?;
        assert_eq!(recovered.public_event_id, repeated.public_event_id);
        assert_eq!(reopened.next_plan_review_public_sequence(&event.run_id)?, 2);
        let records = JsonlSessionStore::read_event_records(&path)?;
        assert_eq!(
            records
                .iter()
                .filter(|record| record.stored_event().event_id == recovered.public_event_id)
                .count(),
            1
        );
        assert_eq!(
            records
                .iter()
                .filter(|record| record.stored_event().event_id == recovered.domain_event_id)
                .count(),
            1
        );
        let resumed = PlanReviewAttemptEntry {
            status: PlanReviewAttemptStatus::Started,
            pending_user_input: None,
            recorded_at_ms: 4,
            ..attempt.clone()
        };
        reopened.append_control(ControlEntry::PlanReviewAttempt(resumed.clone()))?;
        assert!(
            reopened
                .reconcile_plan_review_revision_waiting(&event.run_id)?
                .is_none()
        );
        assert_eq!(
            PublicEventOutboxProjectionV1::from_records(&records)?
                .events_in_order()
                .len(),
            1
        );
        assert!(
            reopened
                .append_plan_review_revision_waiting(attempt, event.clone())
                .is_err()
        );
        let terminal = PlanReviewAttemptEntry {
            status: PlanReviewAttemptStatus::Cancelled,
            terminal_reason: Some(crate::PlanReviewTerminalReason::UserCancelled),
            recorded_at_ms: 5,
            ..resumed
        };
        let decision = crate::PlanDecisionRecordedEntry {
            plan_id: terminal.base_plan_id.clone().expect("base"),
            plan_hash: terminal.base_plan_hash.clone().expect("hash"),
            decision: PlanDecision::RevisionFailed,
            decided_by: PlanDecisionActor::System,
            decided_at_ms: 5,
            reason: None,
        };
        let finalized = reopened.append_plan_review_revision_terminal(
            terminal,
            None,
            decision,
            PublicRunEvent::new(
                event.session_id.clone(),
                event.run_id.clone(),
                2,
                PublicRunEventKind::RunCancelled,
            ),
        )?;
        assert_eq!(
            reopened
                .reconcile_plan_review_revision_terminal(&event.run_id)?
                .expect("terminal")
                .public_event_id,
            finalized.public_event_id
        );
        assert!(
            reopened
                .reconcile_plan_review_revision_waiting(&event.run_id)?
                .is_none()
        );
    }
    Ok(())
}

#[test]
fn revision_waiting_rejects_split_and_conflicting_request_or_lineage() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("session.jsonl");
    let (mut session, attempt, event) = waiting_fixture(&path)?;
    assert!(
        session
            .append_control(ControlEntry::PlanReviewAttempt(attempt.clone()))
            .is_err()
    );
    let mut wrong = attempt.clone();
    wrong
        .pending_user_input
        .as_mut()
        .expect("pending")
        .identity
        .generation = 2;
    assert!(
        session
            .append_plan_review_revision_waiting(wrong, event.clone())
            .is_err()
    );
    let mut wrong = attempt.clone();
    wrong.child_session_ref = crate::SessionRef::new_relative("another-child.jsonl")?;
    assert!(
        session
            .append_plan_review_revision_waiting(wrong, event.clone())
            .is_err()
    );
    let mut wrong = event.clone();
    wrong.sequence = 2;
    assert!(
        session
            .append_plan_review_revision_waiting(attempt.clone(), wrong)
            .is_err()
    );
    // Corrupt raw fixture: current public APIs cannot create this half-pair.
    session
        .durable_store()
        .expect("store")
        .append(&SessionLogEntry::Control(ControlEntry::PlanReviewAttempt(
            attempt,
        )))?;
    assert!(
        PublicEventOutboxProjectionV1::from_records(&JsonlSessionStore::read_event_records(&path)?)
            .is_err()
    );
    Ok(())
}

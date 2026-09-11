use super::reject_unstarted_revision_dispatch;
use crate::runner::WorkerMessage;
use sigil_kernel::{
    ControlEntry, JsonlSessionStore, PlanDecision, PlanReviewAttemptStatus, PlanReviewProjection,
    Session, UserInputDecisionCommandV1, UserInputDecisionReceiptV1,
};
use sigil_runtime::{PlanReviewCoordinator, PlanReviewRunRequest};
use std::{path::Path, sync::mpsc};

fn accepted_revision(
    path: &Path,
) -> anyhow::Result<(Session, PlanReviewRunRequest, UserInputDecisionReceiptV1)> {
    let mut session = Session::load_from_store(
        "revision-dispatch-test",
        "planned-model",
        JsonlSessionStore::new(path)?,
    )?;
    let request = PlanReviewCoordinator::prepare_explicit_plan_review(
        &mut session,
        "Preserve the current public contract",
        "revision-dispatch-test",
        None,
        1,
    )?;
    let mut handler = sigil_kernel::NoopEventHandler;
    PlanReviewCoordinator::ensure_attempt_started(&mut session, &request, &mut handler, 2)?;
    let draft = sigil_kernel::plain_text_plan_draft_entry_with_plan_id(
        request.plan_id.clone(),
        "Preserve the current public contract.",
        request.plan_source_ref(),
        3,
        None,
    )?
    .expect("readable base plan");
    let mut ready = PlanReviewProjection::from_entries(session.entries())
        .attempt_for_plan(&draft.plan_id)
        .expect("started review")
        .clone();
    ready.status = PlanReviewAttemptStatus::DraftReady;
    ready.recorded_at_ms = 3;
    session.append_controls(vec![
        ControlEntry::PlanDraftCreated(draft.clone()),
        ControlEntry::PlanReviewAttempt(ready),
    ])?;
    let requested = PlanReviewCoordinator::request_plan_revision_guidance(
        &mut session,
        &draft.plan_id,
        &draft.plan_hash,
        4,
    )?;
    let (receipt, revision) = PlanReviewCoordinator::accept_plan_revision_guidance(
        &mut session,
        UserInputDecisionCommandV1 {
            identity: requested.request.identity,
            request_hash: requested.request_hash,
            command_id: sigil_kernel::UserInputCommandId::new("revision-dispatch-answer")?,
            decision: sigil_kernel::UserInputDecisionV1::Submitted {
                answers: vec![sigil_kernel::UserInputAnswerV1 {
                    question_id: "revision_guidance".to_owned(),
                    value: sigil_kernel::UserInputAnswerValueV1::Text {
                        value: "Keep the same public interface.".to_owned(),
                    },
                }],
            },
        },
        None,
        5,
    )?;
    Ok((session, revision.expect("accepted revision"), receipt))
}

#[test]
fn revision_zero_dispatch_settles_accepted_input_before_showing_blocked() -> anyhow::Result<()> {
    for reason in [
        "cancellation recorder is unavailable",
        "route execution owner rejected dispatch",
    ] {
        let temp = tempfile::tempdir()?;
        let path = temp.path().join("revision.jsonl");
        let (mut session, revision, receipt) = accepted_revision(&path)?;
        let (message_tx, message_rx) = mpsc::channel();
        reject_unstarted_revision_dispatch(
            &mut session,
            &revision,
            &receipt.request,
            reason.to_owned(),
            &message_tx,
        );
        let WorkerMessage::UserInputDecisionApplied {
            request,
            continuation_started,
            entries,
        } = message_rx.recv()?
        else {
            panic!("the exact accepted answer must settle before its blocked presentation");
        };
        assert_eq!(request, receipt.request);
        assert_eq!(request.status, sigil_kernel::UserInputStatusV1::Resolved);
        assert!(!continuation_started);
        assert_eq!(
            serde_json::to_value(&entries)?,
            serde_json::to_value(session.entries())?
        );
        let WorkerMessage::PlanReviewBlocked {
            reason: displayed,
            paused,
            entries: blocked_entries,
        } = message_rx.recv()?
        else {
            panic!("the continuation failure must remain visible");
        };
        assert!(displayed.contains(reason));
        assert!(!paused);
        assert_eq!(
            serde_json::to_value(blocked_entries)?,
            serde_json::to_value(&entries)?
        );
        assert!(message_rx.try_recv().is_err());
        drop(session);
        let restored = Session::load_from_store(
            "revision-dispatch-test",
            "planned-model",
            JsonlSessionStore::new(&path)?,
        )?;
        let input = restored
            .user_input_projection()?
            .request(&receipt.request.identity)
            .expect("accepted input after restart")
            .clone();
        assert_eq!(input.status, sigil_kernel::UserInputStatusV1::Resolved);
        assert_eq!(
            restored
                .plan_artifact_projection()
                .latest_decision(revision.base_plan_id.as_ref().expect("base plan"))
                .expect("plan decision")
                .decision,
            PlanDecision::RevisionFailed
        );
        assert!(
            PlanReviewProjection::from_entries(restored.entries())
                .review(&revision.plan_review_id)
                .expect("review")
                .attempts
                .iter()
                .all(|attempt| attempt.attempt_id != revision.attempt_id)
        );
    }
    Ok(())
}

#[test]
fn revision_zero_dispatch_settles_input_without_replacing_an_existing_attempt() -> anyhow::Result<()>
{
    let temp = tempfile::tempdir()?;
    let (mut session, revision, receipt) = accepted_revision(&temp.path().join("revision.jsonl"))?;
    PlanReviewCoordinator::ensure_revision_attempt_started(&mut session, &revision, 6)?;
    let before = serde_json::to_value(session.entries())?;
    let (message_tx, message_rx) = mpsc::channel();
    reject_unstarted_revision_dispatch(
        &mut session,
        &revision,
        &receipt.request,
        "duplicate dispatch could not acquire its owner".to_owned(),
        &message_tx,
    );
    let WorkerMessage::UserInputDecisionApplied {
        request,
        continuation_started,
        entries,
    } = message_rx.recv()?
    else {
        panic!("accepted input still needs its exact settlement");
    };
    assert_eq!(request, receipt.request);
    assert!(!continuation_started);
    assert_eq!(serde_json::to_value(entries)?, before);
    let WorkerMessage::PlanReviewBlocked {
        reason, entries, ..
    } = message_rx.recv()?
    else {
        panic!("failed dispatch keeps its own failure presentation");
    };
    assert!(reason.contains("durable state is unchanged"));
    assert_eq!(serde_json::to_value(entries)?, before);
    assert_eq!(serde_json::to_value(session.entries())?, before);
    assert_eq!(
        session
            .plan_artifact_projection()
            .latest_decision(revision.base_plan_id.as_ref().expect("base plan"))
            .expect("original decision")
            .decision,
        PlanDecision::RevisionRequested
    );
    assert!(message_rx.try_recv().is_err());
    Ok(())
}

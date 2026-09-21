use anyhow::Result;

use crate::{
    ControlEntry, ConversationTurnRef, JsonlSessionStore, PlanReviewAttemptEntry,
    PlanReviewAttemptStatus, PlanReviewProjection, PlanReviewSource, RunCancellationRequestedEntry,
    RunCancellationTarget, Session, SessionLogEntry, ToolExecutionEntry, ToolExecutionStatus,
    ToolResultMeta, append_run_cancellation_requested, plan_review_attempt_id_for_review,
    plan_review_child_session_ref, plan_review_id_for_source, plan_review_plan_id_for_attempt,
};

#[test]
fn control_loader_preserves_active_owners_while_startup_loader_recovers_them() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("session.jsonl");
    let store = JsonlSessionStore::new(&path)?;
    let mut active =
        Session::load_from_store("persisted-provider", "persisted-model", store.clone())?;
    let source_turn = ConversationTurnRef::new(
        active.session_scope_id(),
        "plan-source",
        "active-parent-run",
    )?;
    let review_id = plan_review_id_for_source(&source_turn);
    let attempt_id = plan_review_attempt_id_for_review(&review_id);
    active.append_control(ControlEntry::PlanReviewAttempt(PlanReviewAttemptEntry {
        plan_review_id: review_id.clone(),
        attempt_id: attempt_id.clone(),
        plan_id: plan_review_plan_id_for_attempt(&review_id, &attempt_id),
        source: PlanReviewSource::ExplicitPlanCommand,
        source_turn,
        explicit_objective: Some("Inspect the current workspace".to_owned()),
        route_decision_id: None,
        child_session_ref: plan_review_child_session_ref(&review_id, &attempt_id),
        revision_request_id: None,
        attempt_ordinal: 1,
        base_plan_id: None,
        base_plan_hash: None,
        workspace_snapshot_id: None,
        pending_user_input: None,
        status: PlanReviewAttemptStatus::Started,
        terminal_reason: None,
        recorded_at_ms: 1,
    }))?;
    active.append_control(ControlEntry::ToolExecution(Box::new(ToolExecutionEntry {
        call_id: "active-tool-call".to_owned(),
        tool_name: "read_file".to_owned(),
        status: ToolExecutionStatus::Started,
        duration_ms: None,
        subjects: Vec::new(),
        changed_files: Vec::new(),
        metadata: ToolResultMeta::default(),
        error: None,
        model_content_hash: None,
    })))?;
    append_run_cancellation_requested(
        &mut active,
        &RunCancellationRequestedEntry {
            request_id: "active-cancellation".to_owned(),
            run_scope_id: "active-parent-run".to_owned(),
            target: RunCancellationTarget::Run,
            reason: "user requested cancellation".to_owned(),
            requested_at_ms: 1,
            quiescence_deadline_ms: 2,
        },
    )?;
    let before = std::fs::read(&path)?;
    let loaded = Session::load_from_store_for_control(store.clone())?;
    assert_eq!(
        std::fs::read(&path)?,
        before,
        "control loading must not manufacture terminals for live owners"
    );
    assert_eq!(loaded.provider_name(), "persisted-provider");
    assert_eq!(loaded.model_name(), "persisted-model");
    assert_eq!(
        serde_json::to_value(loaded.entries())?,
        serde_json::to_value(active.entries())?
    );
    assert_eq!(
        PlanReviewProjection::from_entries(loaded.entries())
            .latest_attempt(&review_id)
            .map(|attempt| attempt.status),
        Some(PlanReviewAttemptStatus::Started)
    );
    drop(loaded);
    drop(active);

    let recovered = Session::load_from_store("fallback", "fallback", store.clone())?;
    assert_eq!(
        PlanReviewProjection::from_entries(recovered.entries())
            .latest_attempt(&review_id)
            .map(|attempt| attempt.status),
        Some(PlanReviewAttemptStatus::Interrupted)
    );
    assert!(recovered.entries().iter().any(|entry| matches!(entry,
        SessionLogEntry::Control(ControlEntry::ToolExecution(execution))
            if execution.call_id == "active-tool-call"
                && execution.status == ToolExecutionStatus::Interrupted)));
    let records = store.read_event_records_writer()?;
    assert_eq!(
        records
            .iter()
            .filter(|record| {
                let payload = &record.stored_event().payload;
                payload.get("record").and_then(serde_json::Value::as_str) == Some("finalized")
                    && payload
                        .get("request_id")
                        .and_then(serde_json::Value::as_str)
                        == Some("active-cancellation")
            })
            .count(),
        1,
        "startup must retain cancellation recovery ownership"
    );
    let recovered_bytes = std::fs::read(&path)?;
    drop(recovered);
    Session::load_from_store("fallback", "fallback", store)?;
    assert_eq!(
        std::fs::read(path)?,
        recovered_bytes,
        "startup recovery must remain idempotent"
    );
    Ok(())
}

#[test]
fn control_loader_rejects_missing_identity_without_initializing_a_session() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("session.jsonl");
    std::fs::write(&path, "")?;
    let store = JsonlSessionStore::new(&path)?;
    assert!(Session::load_from_store_for_control(store).is_err());
    assert_eq!(std::fs::read(path)?, Vec::<u8>::new());
    Ok(())
}

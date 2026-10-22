use super::*;

#[test]
fn child_purpose_reopen_rejects_any_later_conflict_without_writing() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("research.jsonl");
    let parent = Session::new("fixture", "model");
    let source_turn =
        sigil_kernel::ConversationTurnRef::new(parent.session_scope_id(), "source", "run")?;
    let plan_review_id = sigil_kernel::plan_review_id_for_source(&source_turn);
    let attempt_id = sigil_kernel::plan_review_attempt_id_for_review(&plan_review_id);
    let request = PlanReviewRunRequest {
        application_operation: None,
        plan_id: sigil_kernel::plan_review_plan_id_for_attempt(&plan_review_id, &attempt_id),
        child_session_ref: sigil_kernel::plan_review_child_session_ref(
            &plan_review_id,
            &attempt_id,
        ),
        plan_review_id,
        attempt_id,
        source: sigil_kernel::PlanReviewSource::ExplicitPlanCommand,
        source_turn,
        route_decision_id: None,
        revision_request_id: None,
        revision_generation: None,
        attempt_ordinal: 1,
        base_plan_id: None,
        base_plan_hash: None,
        explicit_objective: Some("Review this change".to_owned()),
        objective: "Review this change".to_owned(),
        workspace_snapshot_id: None,
    };
    let mut child = Session::load_from_store(
        "fixture",
        "model",
        sigil_kernel::JsonlSessionStore::new(&path)?,
    )?;
    bind_plan_review_child_purpose(&parent, &request, &mut child)?;
    let bound = std::fs::read(&path)?;
    bind_plan_review_child_purpose(&parent, &request, &mut child)?;
    assert_eq!(std::fs::read(&path)?, bound, "same binding is idempotent");
    child.append_control(ControlEntry::SessionPurposeBoundV1(
        sigil_kernel::SessionPurposeBoundV1 {
            schema_version: 1,
            purpose: sigil_kernel::SessionPurposeV1::Conversation,
        },
    ))?;
    drop(child);
    let conflicting = std::fs::read(&path)?;
    let mut reopened = Session::load_from_store(
        "fixture",
        "model",
        sigil_kernel::JsonlSessionStore::new(&path)?,
    )?;
    let error = bind_plan_review_child_purpose(&parent, &request, &mut reopened)
        .expect_err("a matching first marker cannot hide a later conflict");
    assert!(error.to_string().contains("purpose binding changed"));
    assert_eq!(
        std::fs::read(&path)?,
        conflicting,
        "rejection preserves the complete stream"
    );
    Ok(())
}

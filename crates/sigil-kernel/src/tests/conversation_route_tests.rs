use std::collections::BTreeSet;

use anyhow::Result;

use crate::{
    AutomaticRouteCapability, ConversationRoute, ConversationRouteDecisionId,
    ConversationRouteDecisionProjection, ConversationRouteDecisionRecordedEntry,
    ConversationRouteReason, ConversationTurnRef, PlanReviewAttemptEntry, PlanReviewAttemptId,
    PlanReviewAttemptStatus, PlanReviewId, PlanReviewProjection, PlanReviewSource,
    PlanReviewTerminalReason, PlanSourceRef, Session, SessionLogEntry, SessionRef,
    TaskRoutingPolicy, ToolCall, conversation_route_decision_id_for_source,
    conversation_route_routing_contract_material, plan_review_attempt_id_for_review,
    plan_review_child_session_ref, plan_review_id_for_source, plan_review_plan_id_for_attempt,
    plan_review_reason_codes, reconcile_plan_review_attempts, request_plan_review_tool_spec,
    route_surface_tool_specs, submit_plan_draft_entry,
};

fn source_turn(session: &Session, message_id: &str) -> ConversationTurnRef {
    ConversationTurnRef::new(session.session_scope_id(), message_id, "logical-run-1")
        .expect("source turn is valid")
}

fn decision_entry(
    decision_id: ConversationRouteDecisionId,
    source_turn: &ConversationTurnRef,
    route: ConversationRoute,
) -> ConversationRouteDecisionRecordedEntry {
    ConversationRouteDecisionRecordedEntry {
        decision_id,
        source_turn: source_turn.clone(),
        route,
        reason_codes: vec![ConversationRouteReason::HighImpact],
        configured_policy: TaskRoutingPolicy::Auto,
        effective_capability: AutomaticRouteCapability::ReviewFirst,
        policy_snapshot_hash: "sha256:policy-v1".to_owned(),
        route_contract_fingerprint: "sha256:contract-v1".to_owned(),
        decided_at_ms: 42,
    }
}

fn attempt_entry(
    plan_review_id: &PlanReviewId,
    attempt_id: &PlanReviewAttemptId,
    status: PlanReviewAttemptStatus,
    source_turn: &ConversationTurnRef,
) -> PlanReviewAttemptEntry {
    PlanReviewAttemptEntry {
        plan_review_id: plan_review_id.clone(),
        attempt_id: attempt_id.clone(),
        plan_id: plan_review_plan_id_for_attempt(plan_review_id, attempt_id),
        source: PlanReviewSource::AutomaticConversationRoute,
        source_turn: source_turn.clone(),
        route_decision_id: None,
        child_session_ref: plan_review_child_session_ref(plan_review_id, attempt_id),
        finalizer_session_ref: None,
        revision_request_id: None,
        attempt_ordinal: 1,
        base_plan_id: None,
        base_plan_hash: None,
        workspace_snapshot_id: None,
        pending_user_input: None,
        status,
        terminal_reason: None,
        recorded_at_ms: 42,
    }
}

#[test]
fn route_decision_identity_is_deterministic_and_domain_separated() -> Result<()> {
    let session = Session::new("mock", "mock");
    let turn = source_turn(&session, "msg-1");
    let first = conversation_route_decision_id_for_source(&turn);
    let second = conversation_route_decision_id_for_source(&turn);
    assert_eq!(first, second);
    let other_turn = source_turn(&session, "msg-2");
    assert_ne!(
        first,
        conversation_route_decision_id_for_source(&other_turn)
    );

    let review_id = plan_review_id_for_source(&turn);
    assert_ne!(first.as_str(), review_id.as_str());
    let attempt_id = plan_review_attempt_id_for_review(&review_id);
    let plan_id = plan_review_plan_id_for_attempt(&review_id, &attempt_id);
    assert_ne!(review_id.as_str(), attempt_id.as_str());
    assert_ne!(review_id.as_str(), plan_id.as_str());
    assert_ne!(attempt_id.as_str(), plan_id.as_str());
    assert_eq!(first.as_str().len(), 36);
    Ok(())
}

#[test]
fn route_decision_projection_allows_one_decision_per_source_turn() -> Result<()> {
    let session = Session::new("mock", "mock");
    let turn = source_turn(&session, "msg-1");
    let decision = decision_entry(
        conversation_route_decision_id_for_source(&turn),
        &turn,
        ConversationRoute::PlanReview,
    );
    let entries = vec![SessionLogEntry::Control(
        crate::ControlEntry::ConversationRouteDecisionRecorded(decision.clone()),
    )];
    let projection = ConversationRouteDecisionProjection::from_entries(&entries);
    assert!(!projection.has_conflicts());
    assert_eq!(projection.decision_for_source(&turn), Some(&decision));

    // Identical replay is idempotent.
    let mut duplicate = entries.clone();
    duplicate.push(SessionLogEntry::Control(
        crate::ControlEntry::ConversationRouteDecisionRecorded(decision.clone()),
    ));
    let projection = ConversationRouteDecisionProjection::from_entries(&duplicate);
    assert!(!projection.has_conflicts());
    assert_eq!(
        projection
            .decision(&decision.decision_id)
            .map(|entry| entry.decision_id.clone()),
        Some(decision.decision_id.clone())
    );
    Ok(())
}

#[test]
fn route_decision_projection_conflicts_fail_closed() -> Result<()> {
    let session = Session::new("mock", "mock");
    let turn = source_turn(&session, "msg-1");
    let decision_id = conversation_route_decision_id_for_source(&turn);
    let first = decision_entry(decision_id.clone(), &turn, ConversationRoute::PlanReview);
    let mut second = first.clone();
    second.route = ConversationRoute::Task;
    let entries = vec![
        SessionLogEntry::Control(crate::ControlEntry::ConversationRouteDecisionRecorded(
            first,
        )),
        SessionLogEntry::Control(crate::ControlEntry::ConversationRouteDecisionRecorded(
            second,
        )),
    ];
    let projection = ConversationRouteDecisionProjection::from_entries(&entries);
    assert!(projection.has_conflicts());
    assert!(!projection.conflicts.is_empty());

    // A different decision identity for the same source turn also conflicts.
    let session = Session::new("mock", "mock");
    let turn = source_turn(&session, "msg-1");
    let first = decision_entry(
        conversation_route_decision_id_for_source(&turn),
        &turn,
        ConversationRoute::Chat,
    );
    let mut second = first.clone();
    second.decision_id = ConversationRouteDecisionId::new("route-other")?;
    let entries = vec![
        SessionLogEntry::Control(crate::ControlEntry::ConversationRouteDecisionRecorded(
            first.clone(),
        )),
        SessionLogEntry::Control(crate::ControlEntry::ConversationRouteDecisionRecorded(
            second,
        )),
    ];
    let projection = ConversationRouteDecisionProjection::from_entries(&entries);
    assert!(projection.has_conflicts());
    assert_eq!(first.reason_codes.len(), 1);
    Ok(())
}

#[test]
fn plan_review_projection_validates_attempt_transitions() -> Result<()> {
    let session = Session::new("mock", "mock");
    let turn = source_turn(&session, "msg-1");
    let review_id = plan_review_id_for_source(&turn);
    let attempt_id = plan_review_attempt_id_for_review(&review_id);

    let started = attempt_entry(
        &review_id,
        &attempt_id,
        PlanReviewAttemptStatus::Started,
        &turn,
    );
    let projection = PlanReviewProjection::default();
    projection.validate_append(&started)?;

    let draft_ready = attempt_entry(
        &review_id,
        &attempt_id,
        PlanReviewAttemptStatus::DraftReady,
        &turn,
    );
    let mut entries = vec![SessionLogEntry::Control(
        crate::ControlEntry::PlanReviewAttempt(started),
    )];
    let projection = PlanReviewProjection::from_entries(&entries);
    projection.validate_append(&draft_ready)?;

    // A legal status edge cannot rewrite the attempt's immutable authority binding.
    let mut rebound = draft_ready.clone();
    rebound.child_session_ref = SessionRef::new_relative("different-child.jsonl")?;
    assert!(projection.validate_append(&rebound).is_err());
    let rebound_projection = PlanReviewProjection::from_entries(&[
        entries[0].clone(),
        SessionLogEntry::Control(crate::ControlEntry::PlanReviewAttempt(rebound)),
    ]);
    assert!(rebound_projection.has_conflicts());

    // Terminal after terminal fails.
    entries.push(SessionLogEntry::Control(
        crate::ControlEntry::PlanReviewAttempt(draft_ready),
    ));
    let projection = PlanReviewProjection::from_entries(&entries);
    let cancelled = attempt_entry(
        &review_id,
        &attempt_id,
        PlanReviewAttemptStatus::Cancelled,
        &turn,
    );
    assert!(projection.validate_append(&cancelled).is_err());

    // A first record that is not Started fails.
    let other_turn = source_turn(&session, "msg-2");
    let other_review = plan_review_id_for_source(&other_turn);
    let other_attempt = plan_review_attempt_id_for_review(&other_review);
    let draft_only = attempt_entry(
        &other_review,
        &other_attempt,
        PlanReviewAttemptStatus::DraftReady,
        &turn,
    );
    assert!(
        PlanReviewProjection::default()
            .validate_append(&draft_only)
            .is_err()
    );

    // Revision: a new attempt may start after DraftReady.
    let revision_attempt = crate::plan_review_attempt_id_for_revision(&review_id, &attempt_id);
    let revision_started = attempt_entry(
        &review_id,
        &revision_attempt,
        PlanReviewAttemptStatus::Started,
        &turn,
    );
    projection.validate_append(&revision_started)?;
    Ok(())
}

#[test]
fn plan_review_attempt_conflicting_facts_fail_closed() -> Result<()> {
    let session = Session::new("mock", "mock");
    let turn = source_turn(&session, "msg-1");
    let review_id = plan_review_id_for_source(&turn);
    let attempt_id = plan_review_attempt_id_for_review(&review_id);
    let started = attempt_entry(
        &review_id,
        &attempt_id,
        PlanReviewAttemptStatus::Started,
        &turn,
    );
    let mut different = started.clone();
    different.recorded_at_ms = 99;
    let entries = vec![
        SessionLogEntry::Control(crate::ControlEntry::PlanReviewAttempt(started)),
        SessionLogEntry::Control(crate::ControlEntry::PlanReviewAttempt(different)),
    ];
    let projection = PlanReviewProjection::from_entries(&entries);
    assert!(projection.has_conflicts());
    assert!(!projection.conflicts.is_empty());
    Ok(())
}

#[test]
fn plan_review_reason_codes_are_bounded_and_unique() -> Result<()> {
    let valid = ToolCall {
        id: "call-1".to_owned(),
        name: crate::REQUEST_PLAN_REVIEW_TOOL_NAME.to_owned(),
        args_json: r#"{"reason_codes":["high_impact","scope_uncertain"]}"#.to_owned(),
    };
    let reasons = plan_review_reason_codes(&valid)?;
    assert_eq!(reasons.len(), 2);

    let empty = ToolCall {
        id: "call-2".to_owned(),
        name: crate::REQUEST_PLAN_REVIEW_TOOL_NAME.to_owned(),
        args_json: r#"{"reason_codes":[]}"#.to_owned(),
    };
    assert!(plan_review_reason_codes(&empty).is_err());

    let duplicate = ToolCall {
        id: "call-3".to_owned(),
        name: crate::REQUEST_PLAN_REVIEW_TOOL_NAME.to_owned(),
        args_json: r#"{"reason_codes":["high_impact","high_impact"]}"#.to_owned(),
    };
    assert!(plan_review_reason_codes(&duplicate).is_err());

    let unknown_field = ToolCall {
        id: "call-4".to_owned(),
        name: crate::REQUEST_PLAN_REVIEW_TOOL_NAME.to_owned(),
        args_json: r#"{"reason_codes":["high_impact"],"free_text":"do it"}"#.to_owned(),
    };
    assert!(plan_review_reason_codes(&unknown_field).is_err());

    let wrong_tool = ToolCall {
        id: "call-5".to_owned(),
        name: "request_task_planning".to_owned(),
        args_json: r#"{"reason_codes":["high_impact"]}"#.to_owned(),
    };
    assert!(plan_review_reason_codes(&wrong_tool).is_err());
    Ok(())
}

#[test]
fn route_surface_follows_capability_tier() -> Result<()> {
    let names = |capability| {
        route_surface_tool_specs(capability)
            .into_iter()
            .map(|spec| spec.name)
            .collect::<BTreeSet<_>>()
    };
    assert!(names(AutomaticRouteCapability::Unsupported).is_empty());
    let review_first = names(AutomaticRouteCapability::ReviewFirst);
    assert!(review_first.contains("request_plan_review"));
    assert!(review_first.contains("continue_without_task_planning"));
    assert!(!review_first.contains("request_task_planning"));
    let direct = names(AutomaticRouteCapability::DirectTask);
    assert!(direct.contains("request_plan_review"));
    assert!(direct.contains("request_task_planning"));
    assert!(direct.contains("continue_without_task_planning"));
    Ok(())
}

#[test]
fn routing_contract_material_is_stable_and_capability_independent() {
    let material = conversation_route_routing_contract_material();
    assert!(material.contains("request_plan_review"));
    assert!(material.contains("request_task_planning"));
    assert!(material.contains("continue_without_task_planning"));
    assert_eq!(material, conversation_route_routing_contract_material());
}

#[test]
fn submit_plan_draft_validates_strict_schema() -> Result<()> {
    let session = Session::new("mock", "mock");
    let turn = source_turn(&session, "msg-1");
    let review_id = plan_review_id_for_source(&turn);
    let attempt_id = plan_review_attempt_id_for_review(&review_id);
    let plan_id = plan_review_plan_id_for_attempt(&review_id, &attempt_id);
    let source = PlanSourceRef {
        source_turn: Some(turn.clone()),
        ..PlanSourceRef::default()
    };
    let valid_args = r#"{
        "schema_version": 2,
        "summary": "Refactor the coordinator",
        "steps": [
            {"step_id": "s1", "title": "Update coordinator", "role": "executor", "depends_on": [], "mode": "write", "isolation": "sequential_workspace_write", "target_paths": ["src/coordinator.rs"]}
        ],
        "target_paths": ["src/coordinator.rs"],
        "suggested_checks": [
            "cargo test",
            {"command": "cargo", "args": ["check"], "effect": "workspace_write"}
        ],
        "risk": "medium",
        "notes": ["keep api stable"]
    }"#;
    let entry = submit_plan_draft_entry(valid_args, plan_id.clone(), source.clone(), 42, None)?
        .expect("valid draft materializes");
    assert_eq!(entry.plan_id, plan_id);
    assert_eq!(entry.schema_version, 2);
    assert_eq!(entry.steps.len(), 1);
    assert_eq!(entry.summary, "Refactor the coordinator");
    assert_eq!(entry.target_paths, vec!["src/coordinator.rs"]);
    assert_eq!(entry.suggested_checks.len(), 2);
    assert_eq!(
        entry
            .suggested_checks
            .iter()
            .find(|check| check.check_spec_id == "cargo-check")
            .expect("structured check is retained")
            .effect,
        crate::ToolEffect::WorkspaceWrite
    );

    let wrong_version = r#"{"schema_version": 1, "summary": "x", "steps": [{"title": "s"}], "target_paths": ["a"], "suggested_checks": []}"#;
    assert!(
        submit_plan_draft_entry(wrong_version, plan_id.clone(), source.clone(), 42, None).is_err()
    );

    let empty_steps = r#"{"schema_version": 2, "summary": "x", "steps": [], "target_paths": ["a"], "suggested_checks": []}"#;
    assert!(
        submit_plan_draft_entry(empty_steps, plan_id.clone(), source.clone(), 42, None).is_err()
    );

    let unknown_field = r#"{"schema_version": 2, "summary": "x", "steps": [{"title": "s"}], "target_paths": ["a"], "suggested_checks": [], "mode": "plan"}"#;
    assert!(
        submit_plan_draft_entry(unknown_field, plan_id.clone(), source.clone(), 42, None).is_err()
    );

    let invalid_path = r#"{"schema_version": 2, "summary": "x", "steps": [{"title": "s"}], "target_paths": ["/etc/passwd"], "suggested_checks": []}"#;
    let entry = submit_plan_draft_entry(invalid_path, plan_id.clone(), source.clone(), 42, None)?;
    assert!(entry.is_some());
    Ok(())
}

#[test]
fn submit_plan_draft_accepts_schema_conformant_intents_and_rejects_legacy_shape() -> Result<()> {
    // Regression: the advertised tool schema and the host-side IntentProposalUnitV1 parser drifted
    // (intent_id/description/string criteria vs intent_alias/statement/criterion objects), so a
    // model submitting intents exactly as the schema advertised was always rejected. The schema
    // and the strict parser must accept the same shape.
    let session = Session::new("mock", "mock");
    let turn = source_turn(&session, "msg-1");
    let review_id = plan_review_id_for_source(&turn);
    let attempt_id = plan_review_attempt_id_for_review(&review_id);
    let plan_id = plan_review_plan_id_for_attempt(&review_id, &attempt_id);
    let source = PlanSourceRef {
        source_turn: Some(turn.clone()),
        ..PlanSourceRef::default()
    };
    let conformant = r#"{
        "schema_version": 2,
        "summary": "Refactor the coordinator",
        "steps": [
            {"step_id": "s1", "title": "Update coordinator", "role": "executor", "depends_on": [], "mode": "write", "isolation": "sequential_workspace_write", "target_paths": ["src/coordinator.rs"], "intent_aliases": ["coordinator-refactor"]}
        ],
        "intents": [{
            "intent_alias": "coordinator-refactor",
            "title": "Refactor coordinator",
            "statement": "Extract the coordinator into bounded modules",
            "acceptance_criteria": [
                {"criterion_alias": "c1", "statement": "coordinator compiles", "required": true}
            ],
            "depends_on_aliases": []
        }],
        "target_paths": ["src/coordinator.rs"],
        "suggested_checks": ["cargo test"]
    }"#;
    let entry = submit_plan_draft_entry(conformant, plan_id.clone(), source.clone(), 42, None)?
        .expect("schema-conformant intents must materialize");
    assert_eq!(entry.steps.len(), 1);
    let proposal = entry.intent_proposal.as_ref().expect("intent proposal");
    assert_eq!(proposal.intents.len(), 1);
    assert_eq!(proposal.intents[0].intent_alias, "coordinator-refactor");
    assert_eq!(proposal.intents[0].acceptance_criteria.len(), 1);
    assert_eq!(
        proposal.intents[0].acceptance_criteria[0].criterion_alias,
        "c1"
    );

    // The legacy mismatched shape (what the old schema advertised) must stay fail-closed.
    let legacy = r#"{
        "schema_version": 2,
        "summary": "Refactor the coordinator",
        "steps": [
            {"step_id": "s1", "title": "Update coordinator", "role": "executor", "depends_on": [], "mode": "write", "isolation": "sequential_workspace_write", "target_paths": ["src/coordinator.rs"]}
        ],
        "intents": [{
            "intent_id": "intent-1",
            "title": "Refactor coordinator",
            "description": "Extract the coordinator",
            "acceptance_criteria": ["coordinator compiles"]
        }],
        "target_paths": ["src/coordinator.rs"],
        "suggested_checks": ["cargo test"]
    }"#;
    let error = submit_plan_draft_entry(legacy, plan_id, source, 42, None)
        .expect_err("legacy intent shape must fail closed");
    assert!(
        format!("{error:#}").contains("unknown field `intent_id`"),
        "unexpected error: {error:#}"
    );
    Ok(())
}

#[test]
fn reconcile_closes_started_attempts_and_promotes_durable_drafts() -> Result<()> {
    let mut session = Session::new("mock", "mock");
    let turn = source_turn(&session, "msg-1");
    let review_id = plan_review_id_for_source(&turn);
    let attempt_id = plan_review_attempt_id_for_review(&review_id);
    let plan_id = plan_review_plan_id_for_attempt(&review_id, &attempt_id);
    let started = attempt_entry(
        &review_id,
        &attempt_id,
        PlanReviewAttemptStatus::Started,
        &turn,
    );
    session.append_control(crate::ControlEntry::PlanReviewAttempt(started.clone()))?;

    // Started without draft -> Interrupted.
    reconcile_plan_review_attempts(&mut session, 100)?;
    let projection = PlanReviewProjection::from_entries(session.entries());
    let latest = projection
        .latest_attempt(&review_id)
        .expect("attempt exists");
    assert_eq!(latest.status, PlanReviewAttemptStatus::Interrupted);
    assert_eq!(
        latest.terminal_reason,
        Some(PlanReviewTerminalReason::RunInterrupted)
    );

    // Interrupted is terminal; idempotent second load does not append again.
    let count_before = session.entries().len();
    reconcile_plan_review_attempts(&mut session, 101)?;
    assert_eq!(session.entries().len(), count_before);

    // Started + durable draft -> DraftReady.
    let mut session = Session::new("mock", "mock");
    let started = attempt_entry(
        &review_id,
        &attempt_id,
        PlanReviewAttemptStatus::Started,
        &turn,
    );
    session.append_control(crate::ControlEntry::PlanReviewAttempt(started.clone()))?;
    let draft = crate::plan_draft_created_entry(
        "```sigil-plan-v2\n{\"summary\":\"s\",\"steps\":[{\"title\":\"t\"}],\"target_paths\":[\"a\"],\"suggested_checks\":[\"c\"]}\n```",
        PlanSourceRef::default(),
        42,
        None,
    )?
    .expect("draft materializes");
    let mut bound_draft = draft;
    bound_draft.plan_id = plan_id.clone();
    session.append_control(crate::ControlEntry::PlanDraftCreated(bound_draft))?;
    reconcile_plan_review_attempts(&mut session, 100)?;
    let projection = PlanReviewProjection::from_entries(session.entries());
    let latest = projection
        .latest_attempt(&review_id)
        .expect("attempt exists");
    assert_eq!(latest.status, PlanReviewAttemptStatus::DraftReady);
    Ok(())
}

#[test]
fn reconcile_preserves_waiting_attempts_and_closes_abandoned_finalizers() -> Result<()> {
    let mut waiting_session = Session::new("mock", "mock");
    let waiting_turn = source_turn(&waiting_session, "msg-waiting");
    let waiting_review_id = plan_review_id_for_source(&waiting_turn);
    let waiting_attempt_id = plan_review_attempt_id_for_review(&waiting_review_id);
    let started = attempt_entry(
        &waiting_review_id,
        &waiting_attempt_id,
        PlanReviewAttemptStatus::Started,
        &waiting_turn,
    );
    waiting_session.append_control(crate::ControlEntry::PlanReviewAttempt(started.clone()))?;
    let mut waiting = started;
    waiting.status = PlanReviewAttemptStatus::WaitingForInput;
    waiting.recorded_at_ms = 43;
    waiting.pending_user_input = Some(Box::new(crate::PublicUserInputRequestV1 {
        identity: crate::UserInputIdentityV1 {
            session_scope_id: crate::SessionScopeId::new(waiting_session.session_scope_id())?,
            root_logical_run_id: crate::LogicalRunId::new("run-waiting")?,
            source_thread_id: crate::AgentThreadId::new("main")?,
            request_id: crate::UserInputRequestId::new("request-waiting")?,
            generation: 1,
            source_binding_hash: format!("sha256:{}", "a".repeat(64)),
        },
        request_hash: format!("sha256:{}", "b".repeat(64)),
        source: crate::UserInputSourceV1::PlanReviewResearch {
            plan_review_id: waiting_review_id.clone(),
            attempt_id: waiting_attempt_id.clone(),
        },
        purpose: crate::UserInputPurposeV1::Clarification,
        prompt: "Choose a migration boundary".to_owned(),
        questions: vec![crate::UserInputQuestionV1 {
            id: "boundary".to_owned(),
            header: "Boundary".to_owned(),
            question: "Which boundary should the plan use?".to_owned(),
            description: None,
            required: true,
            field: crate::UserInputFieldKindV1::Text {
                multiline: false,
                max_chars: 256,
            },
        }],
        allowed_actions: vec![crate::UserInputActionV1::Submit],
        requested_at_unix_ms: 43,
        status: crate::UserInputStatusV1::Requested,
        answer_receipt: None,
        resolution: None,
    }));
    waiting_session.append_control(crate::ControlEntry::PlanReviewAttempt(waiting))?;

    let waiting_count = waiting_session.entries().len();
    reconcile_plan_review_attempts(&mut waiting_session, 100)?;
    assert_eq!(waiting_session.entries().len(), waiting_count);
    assert_eq!(
        PlanReviewProjection::from_entries(waiting_session.entries())
            .latest_attempt(&waiting_review_id)
            .expect("waiting attempt")
            .status,
        PlanReviewAttemptStatus::WaitingForInput
    );

    let mut finalizing_session = Session::new("mock", "mock");
    let finalizing_turn = source_turn(&finalizing_session, "msg-finalizing");
    let finalizing_review_id = plan_review_id_for_source(&finalizing_turn);
    let finalizing_attempt_id = plan_review_attempt_id_for_review(&finalizing_review_id);
    let started = attempt_entry(
        &finalizing_review_id,
        &finalizing_attempt_id,
        PlanReviewAttemptStatus::Started,
        &finalizing_turn,
    );
    finalizing_session.append_control(crate::ControlEntry::PlanReviewAttempt(started.clone()))?;
    let mut finalizing = started;
    finalizing.status = PlanReviewAttemptStatus::Finalizing;
    finalizing.recorded_at_ms = 44;
    finalizing_session.append_control(crate::ControlEntry::PlanReviewAttempt(finalizing))?;

    reconcile_plan_review_attempts(&mut finalizing_session, 101)?;
    let finalizing_projection = PlanReviewProjection::from_entries(finalizing_session.entries());
    let recovered = finalizing_projection
        .latest_attempt(&finalizing_review_id)
        .expect("recovered finalizing attempt");
    assert_eq!(recovered.status, PlanReviewAttemptStatus::Interrupted);
    assert_eq!(
        recovered.terminal_reason,
        Some(PlanReviewTerminalReason::RunInterrupted)
    );
    Ok(())
}

#[test]
fn reconcile_recovers_finalizer_child_draft_and_switches_revision_lineage_atomically() -> Result<()>
{
    let temp = tempfile::tempdir()?;
    let parent_path = temp.path().join("sessions/root.jsonl");
    let store = crate::JsonlSessionStore::new(&parent_path)?;
    let mut session = Session::new("mock", "mock").with_store(store);
    let turn = source_turn(&session, "msg-revision-crash");
    let review_id = plan_review_id_for_source(&turn);
    let base_attempt_id = plan_review_attempt_id_for_review(&review_id);
    let base_plan_id = plan_review_plan_id_for_attempt(&review_id, &base_attempt_id);
    let plan_source = PlanSourceRef {
        source_turn: Some(turn.clone()),
        plan_review_id: Some(review_id.clone()),
        ..PlanSourceRef::default()
    };
    let mut base_started = attempt_entry(
        &review_id,
        &base_attempt_id,
        PlanReviewAttemptStatus::Started,
        &turn,
    );
    base_started.finalizer_session_ref = Some(crate::plan_review_finalizer_session_ref(
        &review_id,
        &base_attempt_id,
        1,
    ));
    session.append_control(crate::ControlEntry::PlanReviewAttempt(base_started.clone()))?;
    let mut base_draft = crate::plan_draft_created_entry(
        "```sigil-plan-v2\n{\"schema_version\":2,\"summary\":\"base\",\"steps\":[{\"step_id\":\"s1\",\"title\":\"base\"}],\"target_paths\":[],\"suggested_checks\":[]}\n```",
        plan_source,
        2,
        None,
    )?
    .expect("base draft");
    base_draft.plan_id = base_plan_id.clone();
    session.append_controls(vec![
        crate::ControlEntry::PlanDraftCreated(base_draft.clone()),
        crate::ControlEntry::PlanReviewAttempt(PlanReviewAttemptEntry {
            status: PlanReviewAttemptStatus::DraftReady,
            recorded_at_ms: 3,
            ..base_started
        }),
    ])?;

    let revision_request_id = crate::UserInputRequestId::new("revision-crash-request")?;
    let revision_attempt_id =
        crate::plan_review_attempt_id_for_revision_ordinal(&review_id, &revision_request_id, 1);
    let revised_plan_id = plan_review_plan_id_for_attempt(&review_id, &revision_attempt_id);
    let finalizer_ref =
        crate::plan_review_finalizer_session_ref(&review_id, &revision_attempt_id, 1);
    session.append_controls(vec![
        crate::ControlEntry::PlanDecisionRecorded(crate::PlanDecisionRecordedEntry {
            plan_id: base_plan_id.clone(),
            plan_hash: base_draft.plan_hash.clone(),
            decision: crate::PlanDecision::RevisionRequested,
            decided_by: crate::PlanDecisionActor::User,
            decided_at_ms: 4,
            reason: Some("revise".to_owned()),
        }),
        crate::ControlEntry::PlanReviewAttempt(PlanReviewAttemptEntry {
            plan_review_id: review_id.clone(),
            attempt_id: revision_attempt_id.clone(),
            plan_id: revised_plan_id.clone(),
            source: PlanReviewSource::AutomaticConversationRoute,
            source_turn: turn,
            route_decision_id: None,
            child_session_ref: plan_review_child_session_ref(&review_id, &revision_attempt_id),
            finalizer_session_ref: Some(finalizer_ref.clone()),
            revision_request_id: Some(revision_request_id),
            attempt_ordinal: 1,
            base_plan_id: Some(base_plan_id.clone()),
            base_plan_hash: Some(base_draft.plan_hash.clone()),
            workspace_snapshot_id: None,
            pending_user_input: None,
            status: PlanReviewAttemptStatus::Started,
            terminal_reason: None,
            recorded_at_ms: 5,
        }),
    ])?;

    let parent_dir = parent_path.parent().expect("session parent");
    let child_store = crate::JsonlSessionStore::new(finalizer_ref.resolve(parent_dir))?;
    let mut child = Session::new("mock", "mock").with_store(child_store);
    let mut revised_draft = base_draft.clone();
    revised_draft.plan_id = revised_plan_id.clone();
    revised_draft.plan_hash = format!("sha256:{}", "e".repeat(64));
    revised_draft.summary = "recovered revised draft".to_owned();
    child.append_control(crate::ControlEntry::PlanDraftCreated(revised_draft.clone()))?;
    drop(child);

    reconcile_plan_review_attempts(&mut session, 6)?;
    let review = PlanReviewProjection::from_entries(session.entries());
    assert_eq!(
        review
            .latest_attempt(&review_id)
            .expect("recovered revision")
            .status,
        PlanReviewAttemptStatus::DraftReady
    );
    let plans = session.plan_artifact_projection();
    assert_eq!(plans.plans.get(&revised_plan_id), Some(&revised_draft));
    assert_eq!(
        plans
            .latest_decision(&base_plan_id)
            .expect("recovered lineage decision")
            .decision,
        crate::PlanDecision::RevisionSucceeded
    );
    Ok(())
}

#[test]
fn plan_review_tool_spec_is_typed_and_read_only() {
    let spec = request_plan_review_tool_spec();
    assert_eq!(spec.name, crate::REQUEST_PLAN_REVIEW_TOOL_NAME);
    assert_eq!(spec.access, crate::ToolAccess::Read);
    assert_eq!(spec.category, crate::ToolCategory::Custom);
    let schema = spec.input_schema;
    let reason_codes = schema
        .get("properties")
        .and_then(|properties| properties.get("reason_codes"))
        .expect("reason_codes property exists");
    assert_eq!(reason_codes.get("maxItems"), Some(&serde_json::json!(6)));
    assert_eq!(
        schema.get("additionalProperties"),
        Some(&serde_json::json!(false))
    );
}

#[test]
fn plan_review_child_session_ref_is_relative_and_stable() -> Result<()> {
    let session = Session::new("mock", "mock");
    let turn = source_turn(&session, "msg-1");
    let review_id = plan_review_id_for_source(&turn);
    let attempt_id = plan_review_attempt_id_for_review(&review_id);
    let first = plan_review_child_session_ref(&review_id, &attempt_id);
    let second = plan_review_child_session_ref(&review_id, &attempt_id);
    assert_eq!(first, second);
    let path = first.as_path();
    assert!(path.starts_with("children/plan-reviews/"));
    assert!(path.extension().is_some_and(|ext| ext == "jsonl"));
    SessionRef::new_relative(path).expect("ref stays relative and valid");
    Ok(())
}

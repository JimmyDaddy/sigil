use anyhow::Result;

use crate::session::SessionWriterFault;
use crate::{
    AutomaticRouteCapability, CONTINUE_EXISTING_TASK_TOOL_NAME,
    CONTINUE_WITHOUT_TASK_PLANNING_TOOL_NAME, ControlEntry, ConversationRoute,
    ConversationRouteDecisionRecordedEntry, ConversationTurnRef, JsonlSessionStore, ModelMessage,
    Session, SessionLogEntry, SessionRef, TaskAdmissionReason, TaskAdmissionTrigger,
    TaskContinuationSelectedEntry, TaskHandoffDecision, TaskHandoffId, TaskHandoffProjection,
    TaskHandoffRequestedEntry, TaskHandoffResolvedEntry, TaskId, TaskPlanEntry, TaskPlanStatus,
    TaskRoutingPolicy, TaskRunEntry, TaskRunStatus, ToolCall, continue_existing_task_tool_spec,
    continue_without_task_planning_tool_spec, conversation_route_decision_id_for_source,
    conversation_route_routing_contract_material, keep_pending_plan_tool_spec,
    project_conversation_prompt_for_persistence, run_pending_plan_tool_spec,
    validate_continue_existing_task_call, validate_continue_without_task_planning_call,
};

fn source_turn(message_id: &str) -> Result<ConversationTurnRef> {
    ConversationTurnRef::new("session-1", message_id, "foreground-run-1")
}

#[test]
fn legacy_task_continuation_receipt_keeps_action_unspecified() -> Result<()> {
    let value = serde_json::json!({
        "task_id": "task-legacy-continuation",
        "source_turn": {
            "session_scope_id": "session-1",
            "message_id": "message-1",
            "logical_run_id": "run-1"
        },
        "plan_version": 1,
        "task_status": "paused",
        "plan_status": "accepted",
        "route_contract_fingerprint": "sha256:legacy-route",
        "prompt_hash": "sha256:legacy-prompt",
        "exact_prompt_required": false,
        "guidance": "legacy safe guidance",
        "selected_at_ms": 42
    });

    let receipt: TaskContinuationSelectedEntry = serde_json::from_value(value)?;
    assert_eq!(
        receipt.control,
        crate::TaskContinuationControlKind::LegacyUnspecified
    );
    Ok(())
}

fn request(
    handoff_id: &str,
    source_turn: ConversationTurnRef,
) -> Result<TaskHandoffRequestedEntry> {
    Ok(TaskHandoffRequestedEntry {
        handoff_id: TaskHandoffId::new(handoff_id)?,
        source_turn,
        trigger: TaskAdmissionTrigger::ModelRequested,
        reason_codes: vec![TaskAdmissionReason::CrossLayer],
        recovery_objective: None,
        policy_snapshot_hash: "sha256:policy".to_owned(),
        requested_at_ms: 42,
    })
}

fn resolution(handoff_id: &str, task_id: &str) -> Result<TaskHandoffResolvedEntry> {
    Ok(TaskHandoffResolvedEntry {
        handoff_id: TaskHandoffId::new(handoff_id)?,
        decision: TaskHandoffDecision::Accepted,
        task_id: Some(TaskId::new(task_id)?),
        decided_at_ms: 43,
    })
}

#[test]
fn handoff_identifiers_and_source_turns_validate_shape() {
    assert!(TaskHandoffId::new("").is_err());
    assert!(TaskHandoffId::new("../handoff").is_err());
    assert!(ConversationTurnRef::new("", "message", "run").is_err());
    assert!(ConversationTurnRef::new("session", "message\n", "run").is_err());
    assert_eq!(
        TaskAdmissionReason::ParallelResearch.as_str(),
        "parallel_research"
    );
}

#[test]
fn task_routing_prompt_assigns_semantic_decision_to_the_model() {
    let prompt = conversation_route_routing_contract_material();
    assert!(prompt.contains("Classify the requested outcome by its meaning, not by keywords"));
    assert!(prompt.contains("Call exactly one of the routing tools advertised in this request"));
    assert!(prompt.contains("Call request_task_planning"));
    assert!(prompt.contains("Call continue_without_task_planning"));
    assert!(prompt.contains("Do not produce free text in this routing microturn"));
    assert!(prompt.contains("small single-file edit"));
    assert!(prompt.contains("one narrow read-only query about a single concern"));
    assert!(prompt.contains("Multiple files alone do not require planning"));
    assert!(prompt.contains("one linear call-flow trace"));
    assert!(prompt.contains("independently useful requested outcomes"));
    assert!(prompt.contains("comparative design review across components"));
    assert!(prompt.contains("coordinated implementation across two or more named layers"));
    assert!(prompt.contains("equally in every user language"));
}

#[test]
fn direct_conversation_routing_tool_is_typed_and_bounded() {
    let spec = continue_without_task_planning_tool_spec();
    assert_eq!(spec.name, CONTINUE_WITHOUT_TASK_PLANNING_TOOL_NAME);
    let valid = ToolCall {
        id: "call-direct".to_owned(),
        name: CONTINUE_WITHOUT_TASK_PLANNING_TOOL_NAME.to_owned(),
        args_json: r#"{"reason":"does_not_meet_task_planning_criteria"}"#.to_owned(),
    };
    assert!(validate_continue_without_task_planning_call(&valid).is_ok());

    let mut invalid = valid.clone();
    invalid.args_json = r#"{"reason":"cross_layer"}"#.to_owned();
    assert!(validate_continue_without_task_planning_call(&invalid).is_err());
}

#[test]
fn existing_task_continuation_tool_keeps_task_identity_host_owned() {
    let spec = continue_existing_task_tool_spec();
    assert_eq!(spec.name, CONTINUE_EXISTING_TASK_TOOL_NAME);
    assert!(spec.input_schema["properties"].get("task_id").is_none());
    let valid = ToolCall {
        id: "call-continue-task".to_owned(),
        name: CONTINUE_EXISTING_TASK_TOOL_NAME.to_owned(),
        args_json: r#"{"reason":"continue_current_task","action":"resume_task"}"#.to_owned(),
    };
    assert!(validate_continue_existing_task_call(&valid).is_ok());

    let mut injected_identity = valid;
    injected_identity.args_json =
        r#"{"reason":"continue_current_task","action":"resume_task","task_id":"task-decoy"}"#
            .to_owned();
    assert!(validate_continue_existing_task_call(&injected_identity).is_err());
}

#[test]
fn pending_plan_decision_tools_keep_plan_identity_host_owned() {
    for spec in [run_pending_plan_tool_spec(), keep_pending_plan_tool_spec()] {
        let serialized = serde_json::to_string(&spec.input_schema).expect("serialize schema");
        assert!(!serialized.contains("plan_id"));
        assert!(!serialized.contains("plan_hash"));
        assert_eq!(spec.input_schema["additionalProperties"], false);
    }
}

#[test]
fn handoff_projection_keeps_admission_separate_from_task_runs() -> Result<()> {
    let source = source_turn("message-1")?;
    let request = request("handoff-1", source.clone())?;
    let resolution = resolution("handoff-1", "task-handoff-1")?;
    let entries = vec![
        SessionLogEntry::Control(ControlEntry::TaskHandoffRequested(request.clone())),
        SessionLogEntry::Control(ControlEntry::TaskHandoffResolved(resolution.clone())),
    ];

    let projection = TaskHandoffProjection::from_entries(&entries);
    let state = projection
        .handoff_for_source(&source)
        .expect("source turn should resolve to its handoff");
    assert_eq!(state.request.as_ref(), Some(&request));
    assert_eq!(state.resolution.as_ref(), Some(&resolution));
    assert_eq!(
        projection
            .accepted_tasks
            .get(resolution.task_id.as_ref().expect("accepted task id")),
        Some(&request.handoff_id)
    );
    assert!(!projection.has_conflicts());
    Ok(())
}

#[test]
fn duplicate_handoff_facts_are_idempotent_but_conflicts_fail_closed() -> Result<()> {
    let source = source_turn("message-1")?;
    let first_request = request("handoff-1", source.clone())?;
    let first_resolution = resolution("handoff-1", "task-handoff-1")?;
    let duplicate_entries = vec![
        SessionLogEntry::Control(ControlEntry::TaskHandoffRequested(first_request.clone())),
        SessionLogEntry::Control(ControlEntry::TaskHandoffRequested(first_request.clone())),
        SessionLogEntry::Control(ControlEntry::TaskHandoffResolved(first_resolution.clone())),
        SessionLogEntry::Control(ControlEntry::TaskHandoffResolved(first_resolution)),
    ];
    let duplicate_projection = TaskHandoffProjection::from_entries(&duplicate_entries);
    let duplicate_state = duplicate_projection
        .handoffs
        .get(&first_request.handoff_id)
        .expect("handoff state");
    assert_eq!(duplicate_state.duplicate_requests, 1);
    assert_eq!(duplicate_state.duplicate_resolutions, 1);
    assert!(!duplicate_projection.has_conflicts());

    let conflicting_entries = vec![
        SessionLogEntry::Control(ControlEntry::TaskHandoffRequested(first_request)),
        SessionLogEntry::Control(ControlEntry::TaskHandoffRequested(request(
            "handoff-2",
            ConversationTurnRef::new(
                source.session_scope_id,
                source.message_id,
                "foreground-run-replayed",
            )?,
        )?)),
    ];
    let conflicting_projection = TaskHandoffProjection::from_entries(&conflicting_entries);
    assert!(conflicting_projection.has_conflicts());
    Ok(())
}

#[test]
fn accepted_and_rejected_resolution_shapes_are_strict() -> Result<()> {
    let accepted_without_task = TaskHandoffResolvedEntry {
        handoff_id: TaskHandoffId::new("handoff-1")?,
        decision: TaskHandoffDecision::Accepted,
        task_id: None,
        decided_at_ms: 43,
    };
    let rejected_with_task = TaskHandoffResolvedEntry {
        handoff_id: TaskHandoffId::new("handoff-2")?,
        decision: TaskHandoffDecision::Rejected,
        task_id: Some(TaskId::new("task-2")?),
        decided_at_ms: 44,
    };
    let projection = TaskHandoffProjection::from_entries(&[
        SessionLogEntry::Control(ControlEntry::TaskHandoffResolved(accepted_without_task)),
        SessionLogEntry::Control(ControlEntry::TaskHandoffResolved(rejected_with_task)),
    ]);
    assert_eq!(projection.conflicts.len(), 2);
    Ok(())
}

#[test]
fn continuation_route_and_exact_selection_recover_as_one_crash_safe_bundle() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("continuation-route-bundle.jsonl");
    let store = JsonlSessionStore::new(&path)?;
    crate::session::append_current_test_session_identity(&store)?;
    let mut session = Session::new("test", "model").with_store(store.clone());
    let task_id = TaskId::new("task-continuation-bundle")?;
    session.append_control(ControlEntry::TaskRun(TaskRunEntry {
        task_id: task_id.clone(),
        parent_session_ref: SessionRef::new_relative("parent.jsonl")?,
        objective: "finish the durable task".to_owned(),
        title: None,
        status: TaskRunStatus::Paused,
        reason: Some("waiting for follow-up".to_owned()),
    }))?;
    session.append_control(ControlEntry::TaskPlan(TaskPlanEntry {
        task_id: task_id.clone(),
        plan_version: 1,
        status: TaskPlanStatus::Accepted,
        steps: Vec::new(),
        reason: None,
    }))?;
    let mut user = ModelMessage::user("finish it and include the new requirement");
    user.id = "continuation-source-message".to_owned();
    session.append_user_message(user)?;
    let source_turn = ConversationTurnRef::new(
        session.session_scope_id(),
        "continuation-source-message",
        "continuation-source-run",
    )?;
    let prompt =
        project_conversation_prompt_for_persistence("finish it and include the new requirement");
    let route = ConversationRouteDecisionRecordedEntry {
        decision_id: conversation_route_decision_id_for_source(&source_turn),
        source_turn: source_turn.clone(),
        route: ConversationRoute::Task,
        reason_codes: Vec::new(),
        configured_policy: TaskRoutingPolicy::Auto,
        effective_capability: AutomaticRouteCapability::DirectTask,
        policy_snapshot_hash: "sha256:policy-v1".to_owned(),
        route_contract_fingerprint: "sha256:continuation-contract-v1".to_owned(),
        decided_at_ms: 99,
    };
    let selected = TaskContinuationSelectedEntry {
        task_id: task_id.clone(),
        source_turn,
        plan_version: Some(1),
        task_status: TaskRunStatus::Paused,
        plan_status: Some(TaskPlanStatus::Accepted),
        route_contract_fingerprint: route.route_contract_fingerprint.clone(),
        control: crate::TaskContinuationControlKind::ApplyCurrentRequestAsGuidance,
        prompt_hash: prompt.prompt_hash,
        exact_prompt_required: prompt.exact_prompt_required,
        guidance: prompt.safe_prompt,
        selected_at_ms: 99,
    };

    store.inject_writer_fault(SessionWriterFault::PartialSecondRecord)?;
    assert!(
        session
            .append_controls(vec![
                ControlEntry::ConversationRouteDecisionRecorded(route.clone()),
                ControlEntry::TaskContinuationSelected(selected.clone()),
            ])
            .is_err()
    );

    let recovered = Session::load_from_store("test", "model", JsonlSessionStore::new(&path)?)?;
    let route_index = recovered
        .entries()
        .iter()
        .position(|entry| {
            matches!(
                entry,
                SessionLogEntry::Control(ControlEntry::ConversationRouteDecisionRecorded(value))
                    if value == &route
            )
        })
        .expect("crash recovery restores the exact route decision");
    assert!(matches!(
        recovered.entries().get(route_index + 1),
        Some(SessionLogEntry::Control(ControlEntry::TaskContinuationSelected(value)))
            if value == &selected
    ));
    assert_eq!(
        recovered
            .task_state_projection()
            .current_task()
            .map(|task| &task.task_id),
        Some(&task_id)
    );
    assert!(!path.with_extension("jsonl.append-bundle-intent").exists());
    Ok(())
}

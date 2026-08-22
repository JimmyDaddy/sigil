use anyhow::Result;
use sha2::{Digest, Sha256};
use sigil_kernel::{
    AgentFinalAnswerRef, AgentRole, AgentRunInput, AgentRunPurpose, AssistantMessageKind,
    AutomaticRouteCapability, ContinueDurableTaskAction, ControlEntry, ConversationRoute,
    ConversationRouteDecisionRecordedEntry, ConversationTurnRef, DurableEventType, EventClass,
    ImageAttachment, ImageMimeType, JsonlSessionStore, ModelMessage, PlanId,
    ProviderFailureClassV1, ProviderTurnRecoveryRetryKindV1, ProviderTurnRecoveryScheduledEntry,
    RecoveryBudgetProjectionV1, RunCancellationRequestedEntry, RunCancellationTarget, SecretString,
    Session, SessionLogEntry, SessionRef, TaskAdmissionReason, TaskAdmissionTrigger,
    TaskContinuationControl, TaskContinuationControlKind, TaskContinuationSelectedEntry,
    TaskDirectExecutionAdmittedV1, TaskHandoffDecision, TaskHandoffRequestedEntry,
    TaskHandoffResolvedEntry, TaskId, TaskIsolationMode, TaskParticipantAttemptEntry,
    TaskParticipantAttemptStatus, TaskParticipantPurpose, TaskParticipantResultEntry,
    TaskPlanEntry, TaskPlanStatus, TaskRoutingPolicy, TaskRunCancellationScopeBoundEntry,
    TaskRunEntry, TaskRunStatus, TaskRunTargetSelectedEntry, TaskStepEntry, TaskStepId,
    TaskStepSpec, TaskStepStatus, WriteIsolationMode, WriteLeaseAcquired, WriteLeaseId,
    WriteLeaseScope, conversation_route_decision_id_for_source,
    durable_task_cancellation_requested, project_conversation_prompt_for_persistence,
    task_participant_attempt_id, task_participant_logical_run_id, task_participant_session_ref,
};
use tempfile::tempdir;

use super::{
    ConversationCoordinator, automatic_policy_snapshot_hash, handoff_id_for_source,
    reconcile_committed_planner_attempts, task_id_for_handoff, validate_task_continuation_action,
};

fn parent_ref() -> Result<SessionRef> {
    SessionRef::new_relative("session.jsonl")
}

fn append_source_turn(session: &mut Session, content: &str) -> Result<ConversationTurnRef> {
    let message = ModelMessage::user(content);
    let source = ConversationTurnRef::new(
        session.session_scope_id(),
        message.id.clone(),
        "foreground-run-1",
    )?;
    session.append_user_message(message)?;
    Ok(source)
}

fn append_requested(session: &mut Session, source: &ConversationTurnRef) -> Result<()> {
    session.append_control(ControlEntry::TaskHandoffRequested(
        TaskHandoffRequestedEntry {
            handoff_id: handoff_id_for_source(source)?,
            source_turn: source.clone(),
            trigger: TaskAdmissionTrigger::ModelRequested,
            reason_codes: vec![TaskAdmissionReason::MultiStageChange],
            recovery_objective: None,
            policy_snapshot_hash: automatic_policy_snapshot_hash(),
            requested_at_ms: 42,
        },
    ))
}

fn append_durable_provider_recovery_schedule(
    session: &Session,
    attempt: &TaskParticipantAttemptEntry,
) -> Result<()> {
    let parent_path = session
        .store_path()
        .expect("recovery fixture uses a durable parent session");
    let parent_dir = parent_path
        .parent()
        .expect("durable session path has a parent directory");
    let child_store = JsonlSessionStore::new(attempt.child_session_ref.resolve(parent_dir))?;
    let schedule = ProviderTurnRecoveryScheduledEntry {
        schema_version: 1,
        recovery_id: format!("recovery-{}", attempt.attempt_id.as_str()),
        logical_run_id: task_participant_logical_run_id(&attempt.attempt_id),
        failed_physical_attempt_id: "provider-attempt-fixture".to_owned(),
        next_physical_attempt_ordinal: 2,
        request_envelope_digest: format!("sha256:{}", "a".repeat(64)),
        source_frontier: None,
        failure_class: ProviderFailureClassV1::TransportInterrupted,
        retry_kind: ProviderTurnRecoveryRetryKindV1::Transport,
        not_before_unix_ms: 0,
        retry_after_ms: 0,
        budget_snapshot: RecoveryBudgetProjectionV1 {
            retry_count: 1,
            max_transport_retries: 2,
            partial_output_retry_count: 0,
            max_partial_output_retries: 1,
            cumulative_delay_ms: 0,
            max_cumulative_delay_ms: 120_000,
        },
        recovery_policy_fingerprint: "fixture-recovery-policy-v1".to_owned(),
    };
    child_store.append_event(
        DurableEventType::ProviderTurnRecoveryScheduled,
        EventClass::Critical,
        serde_json::to_value(schedule)?,
    )?;
    Ok(())
}

fn sha256_prefixed(value: &str) -> String {
    format!("sha256:{:x}", Sha256::digest(value.as_bytes()))
}

fn sha256_hex(value: &str) -> String {
    format!("{:x}", Sha256::digest(value.as_bytes()))
}

#[test]
fn coordinator_binds_stable_host_owned_ids_for_direct_auto_input() -> Result<()> {
    let session = Session::new("mock", "model");
    let coordinator = ConversationCoordinator::new(true, TaskRoutingPolicy::Auto)
        .with_route_capability_evidence(crate::RouteCapabilityEvidence {
            provider_supports_routing_tools: true,
            route_qualified: true,
        });
    let input = AgentRunInput::user("implement across crates");
    let message_id = input
        .persisted_user_message_id
        .clone()
        .expect("direct message id");
    let bound = coordinator.bind_conversation_input(
        &session,
        input,
        parent_ref()?,
        "foreground-run-1",
        None,
        42,
    )?;
    let Some(AgentRunPurpose::Conversation(context)) = bound.purpose else {
        panic!("coordinator should bind a conversation purpose");
    };
    assert_eq!(context.source_turn.message_id, message_id);
    assert_eq!(context.routing_policy, TaskRoutingPolicy::Auto);
    let binding = context.task_handoff.expect("automatic handoff binding");
    assert_eq!(
        binding.handoff_id,
        handoff_id_for_source(&context.source_turn)?
    );
    assert_eq!(binding.task_id, task_id_for_handoff(&binding.handoff_id)?);
    assert_eq!(binding.objective, "implement across crates");
    Ok(())
}

#[test]
fn auto_routing_exposes_model_handoff_without_classifying_prompt_text() -> Result<()> {
    let coordinator = ConversationCoordinator::new(true, TaskRoutingPolicy::Auto)
        .with_route_capability_evidence(crate::RouteCapabilityEvidence {
            provider_supports_routing_tools: true,
            route_qualified: true,
        });

    for (index, prompt) in [
        "你好",
        "请并行调用多个子 agent 调查并实现这个跨 crate 任务",
        "Why is the build slow?",
    ]
    .into_iter()
    .enumerate()
    {
        let session = Session::new("mock", "model");
        let bound = coordinator.bind_conversation_input(
            &session,
            AgentRunInput::user(prompt),
            parent_ref()?,
            format!("prompt-agnostic-run-{index}"),
            None,
            42,
        )?;
        let Some(AgentRunPurpose::Conversation(context)) = bound.purpose else {
            panic!("coordinator should bind a conversation purpose");
        };

        assert_eq!(context.routing_policy, TaskRoutingPolicy::Auto);
        assert!(
            context.task_handoff.is_some(),
            "host must expose the typed handoff to the model without classifying prompt text"
        );
    }

    Ok(())
}

#[test]
fn draft_ready_plan_replaces_ordinary_route_surface_with_typed_decisions() -> Result<()> {
    let mut session = Session::new("mock", "model");
    let review = crate::PlanReviewCoordinator::prepare_explicit_plan_review(
        &mut session,
        "implement the approved change",
        "plan-review-run",
        None,
        1,
    )?;
    let draft = sigil_kernel::plan_draft_created_entry_with_plan_id(
        review.plan_id.clone(),
        r#"```sigil-plan-v2
{"summary":"Implement the approved change","steps":[{"step_id":"implement","title":"Implement","role":"executor","depends_on":[],"mode":"write","isolation":"sequential_workspace_write"}]}
```"#,
        review.plan_source_ref(),
        2,
        None,
    )?
    .expect("structured plan draft");
    crate::PlanReviewCoordinator::commit_draft_from_child(
        &mut session,
        &draft,
        &review,
        &sigil_kernel::PlanCompileInputV1 {
            source_attempt_id: "attempt-1".to_owned(),
            source_turn_id: "message-1".to_owned(),
            task_config_contract_hash: sigil_kernel::stable_event_uuid(
                "sigil-plan-task-config-v1",
                "test",
            ),
            planner_schema_hash: sigil_kernel::stable_event_uuid(
                "sigil-plan-planner-schema-v1",
                "v2",
            ),
            task_contract_schema_hash: sigil_kernel::stable_event_uuid(
                "sigil-task-contract-schema-v1",
                "v2",
            ),
            intent_schema_hash: None,
            max_plan_steps: 64,
            workspace_id: None,
            session_scope_id: Some("test-session".to_owned()),
        },
        3,
    )?;

    let coordinator = ConversationCoordinator::new(true, TaskRoutingPolicy::Auto)
        .with_route_capability_evidence(crate::RouteCapabilityEvidence {
            provider_supports_routing_tools: true,
            route_qualified: true,
        });
    let input = AgentRunInput::user("the model decides the semantics of this entire request");
    let bound = coordinator.bind_conversation_input(
        &session,
        input,
        parent_ref()?,
        "pending-plan-route-run",
        None,
        4,
    )?;
    let Some(AgentRunPurpose::Conversation(context)) = bound.purpose else {
        panic!("coordinator should bind a conversation purpose");
    };
    let pending = context
        .plan_review
        .as_ref()
        .and_then(|binding| binding.pending_plan.as_ref())
        .expect("draft-ready plan must be host-bound");
    assert_eq!(pending.plan_id, draft.plan_id);
    assert_eq!(pending.plan_hash, draft.plan_hash);

    let names = coordinator
        .route_tool_specs_for_session(&session, AutomaticRouteCapability::DirectTask)
        .into_iter()
        .map(|spec| spec.name)
        .collect::<Vec<_>>();
    assert_eq!(
        names,
        vec![
            sigil_kernel::RUN_PENDING_PLAN_TOOL_NAME.to_owned(),
            sigil_kernel::KEEP_PENDING_PLAN_TOOL_NAME.to_owned(),
        ]
    );
    assert!(!names.iter().any(|name| {
        name == sigil_kernel::CONTINUE_WITHOUT_TASK_PLANNING_TOOL_NAME
            || name == sigil_kernel::REQUEST_TASK_PLANNING_TOOL_NAME
            || name == sigil_kernel::REQUEST_PLAN_REVIEW_TOOL_NAME
    }));
    Ok(())
}

#[test]
fn coordinator_uses_the_exact_durable_url_and_attachment_projection() -> Result<()> {
    let session = Session::new("mock", "model");
    let coordinator = ConversationCoordinator::new(true, TaskRoutingPolicy::Auto)
        .with_route_capability_evidence(crate::RouteCapabilityEvidence {
            provider_supports_routing_tools: true,
            route_qualified: true,
        });
    let input = AgentRunInput::user("inspect https://example.com/private?q=secret")
        .with_image_attachments(vec![ImageAttachment::from_bytes(
            "image-1",
            ImageMimeType::Png,
            1,
            1,
            vec![1],
        )?]);
    let durable = input
        .durable_user_message_projection()?
        .expect("direct input should project a durable user message");
    let expected_objective = durable.content.expect("durable message content");
    let bound = coordinator.bind_conversation_input(
        &session,
        input,
        parent_ref()?,
        "foreground-run-url-image",
        None,
        42,
    )?;
    let Some(AgentRunPurpose::Conversation(context)) = bound.purpose else {
        panic!("coordinator should bind a conversation purpose");
    };
    assert_eq!(
        context
            .task_handoff
            .expect("automatic handoff binding")
            .objective,
        expected_objective
    );
    assert!(expected_objective.contains("[Image attachment 1:"));
    assert!(!expected_objective.contains("private?q=secret"));
    Ok(())
}

#[test]
fn explicit_task_admission_uses_the_same_idempotent_handoff_protocol() -> Result<()> {
    let temp = tempdir()?;
    let session_path = temp.path().join("explicit-task.jsonl");
    let mut session =
        Session::new("provider", "model").with_store(JsonlSessionStore::new(&session_path)?);
    let coordinator = ConversationCoordinator::new(true, TaskRoutingPolicy::Manual);
    let message = ModelMessage::user("execute a durable task");
    let action = coordinator.admit_explicit_task(
        &mut session,
        message.clone(),
        parent_ref()?,
        "task-command-1",
        17,
    )?;
    let replay = coordinator.admit_explicit_task(
        &mut session,
        message,
        parent_ref()?,
        "task-command-1",
        17,
    )?;

    assert_eq!(action, replay);
    assert_eq!(
        session
            .entries()
            .iter()
            .filter(|entry| matches!(entry, SessionLogEntry::User(_)))
            .count(),
        1
    );
    assert_eq!(
        session
            .entries()
            .iter()
            .filter(|entry| matches!(
                entry,
                SessionLogEntry::Control(ControlEntry::TaskHandoffRequested(_))
            ))
            .count(),
        1
    );
    assert_eq!(
        session
            .entries()
            .iter()
            .filter(|entry| matches!(entry, SessionLogEntry::Control(ControlEntry::TaskRun(_))))
            .count(),
        1
    );
    let durable_entries = JsonlSessionStore::read_entries(&session_path)?;
    let user_index = durable_entries
        .iter()
        .position(|entry| matches!(entry, SessionLogEntry::User(_)))
        .expect("explicit task source should be durable");
    let request_index = durable_entries
        .iter()
        .position(|entry| {
            matches!(
                entry,
                SessionLogEntry::Control(ControlEntry::TaskHandoffRequested(_))
            )
        })
        .expect("explicit task request should be durable");
    assert!(request_index < user_index);
    Ok(())
}

#[test]
fn explicit_requested_anchor_recovers_a_missing_source_user_turn() -> Result<()> {
    let coordinator = ConversationCoordinator::new(true, TaskRoutingPolicy::Manual);
    let mut session = Session::new("provider", "model");
    let source = ConversationTurnRef::new(
        session.session_scope_id(),
        "explicit-source-after-crash",
        "task-command-crashed",
    )?;
    let handoff_id = handoff_id_for_source(&source)?;
    session.append_control(ControlEntry::TaskHandoffRequested(
        TaskHandoffRequestedEntry {
            handoff_id,
            source_turn: source.clone(),
            trigger: TaskAdmissionTrigger::ExplicitTaskCommand,
            reason_codes: Vec::new(),
            recovery_objective: Some("recover explicit objective".to_owned()),
            policy_snapshot_hash: super::explicit_task_policy_snapshot_hash(),
            requested_at_ms: 17,
        },
    ))?;

    let actions = coordinator.reconcile(&mut session, &parent_ref()?, 18)?;
    assert_eq!(actions.len(), 1);
    assert!(session.entries().iter().any(|entry| matches!(
        entry,
        SessionLogEntry::User(message)
            if message.id == source.message_id
                && message.content.as_deref() == Some("recover explicit objective")
    )));
    Ok(())
}

#[test]
fn disabled_or_manual_routing_never_binds_the_internal_handoff() -> Result<()> {
    for coordinator in [
        ConversationCoordinator::new(false, TaskRoutingPolicy::Auto),
        ConversationCoordinator::new(true, TaskRoutingPolicy::Manual),
    ] {
        let session = Session::new("mock", "model");
        let bound = coordinator.bind_conversation_input(
            &session,
            AgentRunInput::user("simple question"),
            parent_ref()?,
            "foreground-run-1",
            None,
            42,
        )?;
        let Some(AgentRunPurpose::Conversation(context)) = bound.purpose else {
            panic!("coordinator should bind a conversation purpose");
        };
        assert_eq!(context.routing_policy, TaskRoutingPolicy::Manual);
        assert!(context.task_handoff.is_none());
    }
    Ok(())
}

#[test]
fn requested_crash_gap_reconciles_resolution_and_task_once() -> Result<()> {
    let coordinator = ConversationCoordinator::new(true, TaskRoutingPolicy::Auto);
    let mut session = Session::new("mock", "model");
    let source = append_source_turn(&mut session, "durable objective")?;
    append_requested(&mut session, &source)?;

    let first = coordinator.reconcile(&mut session, &parent_ref()?, 50)?;
    assert_eq!(first.len(), 1);
    let entry_count = session.entries().len();
    let second = coordinator.reconcile(&mut session, &parent_ref()?, 60)?;
    assert_eq!(second, first);
    assert_eq!(session.entries().len(), entry_count);

    let projection = session.task_handoff_projection();
    let state = projection
        .handoff_for_source(&source)
        .expect("reconciled handoff state");
    let resolution = state.resolution.as_ref().expect("accepted resolution");
    assert_eq!(resolution.decision, TaskHandoffDecision::Accepted);
    let task_id = resolution.task_id.as_ref().expect("accepted task id");
    let task = session
        .task_state_projection()
        .tasks
        .get(task_id)
        .cloned()
        .expect("reconciled task run");
    assert_eq!(task.status, TaskRunStatus::Started);
    assert_eq!(task.objective, "durable objective");
    Ok(())
}

#[test]
fn accepted_crash_gap_reconciles_only_the_missing_task_run() -> Result<()> {
    let coordinator = ConversationCoordinator::new(true, TaskRoutingPolicy::Auto);
    let mut session = Session::new("mock", "model");
    let source = append_source_turn(&mut session, "durable objective")?;
    append_requested(&mut session, &source)?;
    let handoff_id = handoff_id_for_source(&source)?;
    let task_id = task_id_for_handoff(&handoff_id)?;
    session.append_control(ControlEntry::TaskHandoffResolved(
        TaskHandoffResolvedEntry {
            handoff_id,
            decision: TaskHandoffDecision::Accepted,
            task_id: Some(task_id.clone()),
            decided_at_ms: 43,
        },
    ))?;

    let before_resolutions = session
        .entries()
        .iter()
        .filter(|entry| {
            matches!(
                entry,
                SessionLogEntry::Control(ControlEntry::TaskHandoffResolved(_))
            )
        })
        .count();
    let actions = coordinator.reconcile(&mut session, &parent_ref()?, 50)?;
    assert_eq!(actions.len(), 1);
    assert_eq!(actions[0].task_id, task_id);
    assert_eq!(
        session
            .entries()
            .iter()
            .filter(|entry| matches!(
                entry,
                SessionLogEntry::Control(ControlEntry::TaskHandoffResolved(_))
            ))
            .count(),
        before_resolutions
    );
    assert!(session.entries().iter().any(|entry| matches!(
        entry,
        SessionLogEntry::Control(ControlEntry::TaskRun(run)) if run.task_id == task_id
    )));
    Ok(())
}

#[test]
fn durable_running_task_recovery_interrupts_stale_steps_and_requires_explicit_continue()
-> Result<()> {
    let coordinator = ConversationCoordinator::new(true, TaskRoutingPolicy::Auto);
    let temp = tempdir()?;
    let store = JsonlSessionStore::new(temp.path().join("session.jsonl"))?;
    let mut session = Session::load_from_store("mock", "model", store.clone())?;
    let source = append_source_turn(&mut session, "durable objective")?;
    append_requested(&mut session, &source)?;
    let first = coordinator.reconcile(&mut session, &parent_ref()?, 50)?;
    let action = first.first().expect("admission gap should resume");
    let step_id = TaskStepId::new("stale-step")?;
    session.append_control(ControlEntry::TaskPlan(TaskPlanEntry {
        task_id: action.task_id.clone(),
        plan_version: 1,
        status: TaskPlanStatus::Accepted,
        steps: vec![TaskStepSpec {
            step_id: step_id.clone(),
            title: "stale execution".to_owned(),
            display_name: None,
            detail: None,
            role: AgentRole::Executor,
            depends_on: Vec::new(),
            intent_refs: Vec::new(),
            mode: None,
            isolation: None,
        }],
        reason: None,
    }))?;
    session.append_control(ControlEntry::TaskRun(TaskRunEntry {
        task_id: action.task_id.clone(),
        parent_session_ref: parent_ref()?,
        objective: "durable objective".to_owned(),
        title: None,

        status: TaskRunStatus::Running,
        reason: None,
    }))?;
    session.append_control(ControlEntry::TaskStep(TaskStepEntry {
        task_id: action.task_id.clone(),
        plan_version: 1,
        step_id: step_id.clone(),
        role: AgentRole::Executor,
        status: TaskStepStatus::Running,
        title: Some("stale execution".to_owned()),
        summary: None,
        reason: None,
    }))?;
    let attempt_id = task_participant_attempt_id(
        &action.task_id,
        TaskParticipantPurpose::Step,
        Some(1),
        Some(&step_id),
        1,
    )?;
    session.append_control(ControlEntry::TaskParticipantAttempt(
        TaskParticipantAttemptEntry {
            child_session_ref: task_participant_session_ref(&action.task_id, &attempt_id)?,
            attempt_id: attempt_id.clone(),
            task_id: action.task_id.clone(),
            purpose: TaskParticipantPurpose::Step,
            ordinal: 1,
            plan_version: Some(1),
            step_id: Some(step_id.clone()),
            role: AgentRole::Executor,
            status: TaskParticipantAttemptStatus::Started,
            reason: None,
        },
    ))?;

    drop(session);
    let mut session = Session::load_from_store("mock", "model", store)?;
    let resumed = coordinator.reconcile(&mut session, &parent_ref()?, 60)?;
    assert!(resumed.is_empty());
    let projection = session.task_state_projection();
    let task = projection
        .tasks
        .get(&action.task_id)
        .expect("task should remain projected");
    assert_eq!(task.status, TaskRunStatus::Paused);
    assert_eq!(
        task.steps
            .get(&(1, step_id))
            .expect("stale step should remain projected")
            .status,
        TaskStepStatus::Interrupted
    );
    assert_eq!(
        task.participant_attempts
            .get(&attempt_id)
            .expect("stale participant should remain projected")
            .status,
        TaskParticipantAttemptStatus::Interrupted
    );
    Ok(())
}

#[test]
fn recovery_completes_a_started_planner_after_its_parent_plan_is_committed() -> Result<()> {
    let mut session = Session::new("mock", "model");
    let task_id = TaskId::new("planner-commit-recovery")?;
    session.append_control(ControlEntry::TaskRun(TaskRunEntry {
        task_id: task_id.clone(),
        parent_session_ref: parent_ref()?,
        objective: "recover a committed planner result".to_owned(),
        title: None,
        status: TaskRunStatus::Running,
        reason: None,
    }))?;
    let step_id = TaskStepId::new("committed-step")?;
    session.append_control(ControlEntry::TaskPlan(TaskPlanEntry {
        task_id: task_id.clone(),
        plan_version: 1,
        status: TaskPlanStatus::Accepted,
        steps: vec![TaskStepSpec {
            step_id,
            title: "committed step".to_owned(),
            display_name: None,
            detail: None,
            role: AgentRole::Executor,
            depends_on: Vec::new(),
            intent_refs: Vec::new(),
            mode: None,
            isolation: None,
        }],
        reason: None,
    }))?;
    let attempt_id =
        task_participant_attempt_id(&task_id, TaskParticipantPurpose::Planner, None, None, 1)?;
    session.append_control(ControlEntry::TaskParticipantAttempt(
        TaskParticipantAttemptEntry {
            child_session_ref: task_participant_session_ref(&task_id, &attempt_id)?,
            attempt_id: attempt_id.clone(),
            task_id: task_id.clone(),
            purpose: TaskParticipantPurpose::Planner,
            ordinal: 1,
            plan_version: None,
            step_id: None,
            role: AgentRole::Planner,
            status: TaskParticipantAttemptStatus::Started,
            reason: None,
        },
    ))?;

    assert_eq!(
        reconcile_committed_planner_attempts(&mut session, &task_id)?,
        1
    );
    assert_eq!(
        reconcile_committed_planner_attempts(&mut session, &task_id)?,
        0
    );
    assert_eq!(
        session
            .task_state_projection()
            .tasks
            .get(&task_id)
            .and_then(|task| task.participant_attempts.get(&attempt_id))
            .expect("planner attempt should remain projected")
            .status,
        TaskParticipantAttemptStatus::Completed
    );
    Ok(())
}

#[test]
fn resolution_without_request_fails_closed() -> Result<()> {
    let coordinator = ConversationCoordinator::new(true, TaskRoutingPolicy::Auto);
    let mut session = Session::new("mock", "model");
    let handoff_id = sigil_kernel::TaskHandoffId::new("handoff-orphan")?;
    session.append_control(ControlEntry::TaskHandoffResolved(
        TaskHandoffResolvedEntry {
            handoff_id,
            decision: TaskHandoffDecision::Accepted,
            task_id: Some(sigil_kernel::TaskId::new("task-orphan")?),
            decided_at_ms: 43,
        },
    ))?;
    let error = coordinator
        .reconcile(&mut session, &parent_ref()?, 50)
        .expect_err("orphan resolution must fail closed");
    assert!(error.to_string().contains("without a request"));
    Ok(())
}

#[test]
fn durable_task_cancellation_suppresses_crash_prefix_final_repair() -> Result<()> {
    let temp = tempdir()?;
    let store = JsonlSessionStore::new(temp.path().join("session.jsonl"))?;
    let mut session = Session::new("mock", "model").with_store(store);
    let task_id = sigil_kernel::TaskId::new("task-cancelled-prefix")?;
    session.append_control(ControlEntry::TaskRun(TaskRunEntry {
        task_id: task_id.clone(),
        parent_session_ref: parent_ref()?,
        objective: "cancel before final commit".to_owned(),
        title: None,

        status: TaskRunStatus::Running,
        reason: None,
    }))?;
    session.append_control(ControlEntry::TaskRunCancellationScopeBound(
        TaskRunCancellationScopeBoundEntry {
            task_id: task_id.clone(),
            run_scope_id: "task-root-prefix".to_owned(),
        },
    ))?;
    let attempt_id = task_participant_attempt_id(
        &task_id,
        TaskParticipantPurpose::Synthesis,
        Some(1),
        None,
        1,
    )?;
    session.append_control(ControlEntry::TaskParticipantAttempt(
        TaskParticipantAttemptEntry {
            attempt_id: attempt_id.clone(),
            task_id: task_id.clone(),
            purpose: TaskParticipantPurpose::Synthesis,
            ordinal: 1,
            plan_version: Some(1),
            step_id: None,
            role: AgentRole::Planner,
            child_session_ref: task_participant_session_ref(&task_id, &attempt_id)?,
            status: TaskParticipantAttemptStatus::Completed,
            reason: None,
        },
    ))?;
    let summary = "synthesis completed before cancellation won".to_owned();
    session.append_control(ControlEntry::TaskParticipantResult(
        TaskParticipantResultEntry {
            attempt_id,
            task_id: task_id.clone(),
            summary_hash: sha256_prefixed(&summary),
            output_hash: sha256_prefixed("exact synthesis output"),
            summary,
            summary_truncated: false,
            terminal_status: Some(TaskParticipantAttemptStatus::Completed),
            final_answer_ref: None,
            artifact_refs: Vec::new(),
            changed_paths: Vec::new(),
            verification_refs: Vec::new(),
        },
    ))?;
    session
        .run_cancellation_recorder()?
        .append_requested(&RunCancellationRequestedEntry {
            request_id: "cancel-task-prefix".to_owned(),
            run_scope_id: "task-root-prefix".to_owned(),
            target: RunCancellationTarget::Run,
            reason: "user cancelled".to_owned(),
            requested_at_ms: 10,
            quiescence_deadline_ms: 20,
        })?;

    let coordinator = ConversationCoordinator::new(true, TaskRoutingPolicy::Auto);
    let actions = coordinator.reconcile(&mut session, &parent_ref()?, 30)?;

    assert!(actions.is_empty());
    let projection = session.task_state_projection();
    let task = projection
        .tasks
        .get(&task_id)
        .expect("task remains projected");
    assert_eq!(task.status, TaskRunStatus::Interrupted);
    assert!(task.final_answer.is_none());
    assert!(session.entries().iter().all(|entry| {
        !matches!(
            entry,
            SessionLogEntry::Assistant(message)
                if message.assistant_kind == Some(sigil_kernel::AssistantMessageKind::FinalAnswer)
        )
    }));
    session.append_control(ControlEntry::TaskRunCancellationScopeBound(
        TaskRunCancellationScopeBoundEntry {
            task_id: task_id.clone(),
            run_scope_id: "task-root-continued".to_owned(),
        },
    ))?;
    assert!(!durable_task_cancellation_requested(
        &session,
        task_id.as_str()
    )?);
    Ok(())
}

#[test]
fn synthesis_result_only_crash_prefix_completes_without_provider_replay() -> Result<()> {
    let temp = tempdir()?;
    let parent_store_path = temp.path().join("session.jsonl");
    let store = JsonlSessionStore::new(&parent_store_path)?;
    let mut session = Session::load_from_store("mock", "model", store)?;
    let task_id = sigil_kernel::TaskId::new("task-result-only-prefix")?;
    session.append_control(ControlEntry::TaskRun(TaskRunEntry {
        task_id: task_id.clone(),
        parent_session_ref: parent_ref()?,
        objective: "recover result-only synthesis".to_owned(),
        title: None,

        status: TaskRunStatus::Running,
        reason: None,
    }))?;
    let completed_step_id = TaskStepId::new("completed-step")?;
    session.append_control(ControlEntry::TaskPlan(TaskPlanEntry {
        task_id: task_id.clone(),
        plan_version: 1,
        status: TaskPlanStatus::Accepted,
        steps: vec![TaskStepSpec {
            step_id: completed_step_id.clone(),
            title: "completed prerequisite".to_owned(),
            display_name: None,
            detail: None,
            role: AgentRole::Executor,
            depends_on: Vec::new(),
            intent_refs: Vec::new(),
            mode: None,
            isolation: None,
        }],
        reason: None,
    }))?;
    session.append_control(ControlEntry::TaskStep(TaskStepEntry {
        task_id: task_id.clone(),
        plan_version: 1,
        step_id: completed_step_id,
        role: AgentRole::Executor,
        status: TaskStepStatus::Completed,
        title: Some("completed prerequisite".to_owned()),
        summary: Some("done".to_owned()),
        reason: None,
    }))?;
    let attempt_id = task_participant_attempt_id(
        &task_id,
        TaskParticipantPurpose::Synthesis,
        Some(1),
        None,
        1,
    )?;
    let child_session_ref = task_participant_session_ref(&task_id, &attempt_id)?;
    let final_text = "result-only synthesis final";
    let child_message_id = "synthesis-result-only".to_owned();
    let child_store = JsonlSessionStore::new(
        child_session_ref.resolve(parent_store_path.parent().expect("parent store directory")),
    )?;
    let mut child_session = Session::load_from_store("mock", "model", child_store)?;
    let mut child_message = ModelMessage::assistant_with_kind(
        Some(final_text.to_owned()),
        Vec::new(),
        AssistantMessageKind::FinalAnswer,
    );
    child_message.id.clone_from(&child_message_id);
    child_session.append_assistant_message(child_message)?;
    session.append_control(ControlEntry::TaskParticipantAttempt(
        TaskParticipantAttemptEntry {
            attempt_id: attempt_id.clone(),
            task_id: task_id.clone(),
            purpose: TaskParticipantPurpose::Synthesis,
            ordinal: 1,
            plan_version: Some(1),
            step_id: None,
            role: AgentRole::Planner,
            child_session_ref: child_session_ref.clone(),
            status: TaskParticipantAttemptStatus::Started,
            reason: None,
        },
    ))?;
    let summary = final_text.to_owned();
    session.append_control(ControlEntry::TaskParticipantResult(
        TaskParticipantResultEntry {
            attempt_id: attempt_id.clone(),
            task_id: task_id.clone(),
            summary_hash: sha256_prefixed(&summary),
            output_hash: sha256_prefixed(final_text),
            summary,
            summary_truncated: false,
            terminal_status: Some(TaskParticipantAttemptStatus::Completed),
            final_answer_ref: Some(AgentFinalAnswerRef {
                session_ref: child_session_ref,
                message_id: child_message_id,
                content_hash: sha256_hex(final_text),
                char_count: final_text.chars().count(),
            }),
            artifact_refs: Vec::new(),
            changed_paths: Vec::new(),
            verification_refs: Vec::new(),
        },
    ))?;

    let coordinator = ConversationCoordinator::new(true, TaskRoutingPolicy::Auto);
    let actions = coordinator.reconcile(&mut session, &parent_ref()?, 30)?;

    assert!(actions.is_empty());
    let projection = session.task_state_projection();
    let task = projection
        .tasks
        .get(&task_id)
        .expect("task remains projected");
    assert_eq!(task.status, TaskRunStatus::Completed);
    assert_eq!(
        task.participant_attempts
            .get(&attempt_id)
            .expect("synthesis attempt remains projected")
            .status,
        TaskParticipantAttemptStatus::Completed
    );
    assert_eq!(
        session
            .entries()
            .iter()
            .filter(|entry| matches!(
                entry,
                SessionLogEntry::Assistant(message)
                    if message.assistant_kind == Some(AssistantMessageKind::FinalAnswer)
            ))
            .count(),
        1
    );
    Ok(())
}

#[test]
fn step_result_only_crash_prefix_blocks_without_replaying_side_effects() -> Result<()> {
    let coordinator = ConversationCoordinator::new(true, TaskRoutingPolicy::Auto);
    let mut session = Session::new("mock", "model");
    let source = append_source_turn(&mut session, "change the workspace once")?;
    append_requested(&mut session, &source)?;
    let first = coordinator.reconcile(&mut session, &parent_ref()?, 10)?;
    let action = first.first().expect("task should be admitted");
    let task_id = action.task_id.clone();
    let step_id = TaskStepId::new("write-once")?;
    session.append_control(ControlEntry::TaskPlan(TaskPlanEntry {
        task_id: task_id.clone(),
        plan_version: 1,
        status: TaskPlanStatus::Accepted,
        steps: vec![TaskStepSpec {
            step_id: step_id.clone(),
            title: "write once".to_owned(),
            display_name: None,
            detail: None,
            role: AgentRole::Executor,
            depends_on: Vec::new(),
            intent_refs: Vec::new(),
            mode: None,
            isolation: None,
        }],
        reason: None,
    }))?;
    session.append_control(ControlEntry::TaskRun(TaskRunEntry {
        task_id: task_id.clone(),
        parent_session_ref: parent_ref()?,
        objective: "change the workspace once".to_owned(),
        title: None,

        status: TaskRunStatus::Running,
        reason: None,
    }))?;
    session.append_control(ControlEntry::TaskStep(TaskStepEntry {
        task_id: task_id.clone(),
        plan_version: 1,
        step_id: step_id.clone(),
        role: AgentRole::Executor,
        status: TaskStepStatus::Running,
        title: Some("write once".to_owned()),
        summary: None,
        reason: None,
    }))?;
    let attempt_id = task_participant_attempt_id(
        &task_id,
        TaskParticipantPurpose::Step,
        Some(1),
        Some(&step_id),
        1,
    )?;
    session.append_control(ControlEntry::TaskParticipantAttempt(
        TaskParticipantAttemptEntry {
            attempt_id: attempt_id.clone(),
            task_id: task_id.clone(),
            purpose: TaskParticipantPurpose::Step,
            ordinal: 1,
            plan_version: Some(1),
            step_id: Some(step_id.clone()),
            role: AgentRole::Executor,
            child_session_ref: task_participant_session_ref(&task_id, &attempt_id)?,
            status: TaskParticipantAttemptStatus::Started,
            reason: None,
        },
    ))?;
    let lease_id = WriteLeaseId::new("lease-result-only")?;
    session.append_control(ControlEntry::WriteLeaseAcquired(WriteLeaseAcquired {
        lease_id: lease_id.clone(),
        workspace_id: "workspace-result-only".to_owned(),
        owner_agent_id: format!("task:{}:step:{}", task_id.as_str(), step_id.as_str()),
        isolation_mode: WriteIsolationMode::SharedWorkspaceExclusive,
        scope: WriteLeaseScope::Workspace,
    }))?;
    let summary = "workspace mutation already happened".to_owned();
    session.append_control(ControlEntry::TaskParticipantResult(
        TaskParticipantResultEntry {
            attempt_id: attempt_id.clone(),
            task_id: task_id.clone(),
            summary_hash: sha256_prefixed(&summary),
            output_hash: sha256_prefixed("exact step output"),
            summary,
            summary_truncated: false,
            terminal_status: Some(TaskParticipantAttemptStatus::Completed),
            final_answer_ref: None,
            artifact_refs: Vec::new(),
            changed_paths: vec!["src/lib.rs".to_owned()],
            verification_refs: Vec::new(),
        },
    ))?;
    let actions = coordinator.reconcile(&mut session, &parent_ref()?, 20)?;

    assert!(actions.is_empty());
    let projection = session.task_state_projection();
    let task = projection
        .tasks
        .get(&task_id)
        .expect("task remains projected");
    assert_eq!(task.status, TaskRunStatus::Paused);
    assert_eq!(
        task.participant_attempts
            .get(&attempt_id)
            .expect("attempt remains projected")
            .status,
        TaskParticipantAttemptStatus::Completed
    );
    assert_eq!(
        task.steps
            .get(&(1, step_id))
            .expect("step remains projected")
            .status,
        TaskStepStatus::Blocked
    );
    assert!(
        !session
            .write_isolation_projection()
            .leases
            .get(&lease_id)
            .expect("lease remains auditable")
            .is_active()
    );
    let entry_count = session.entries().len();
    assert!(
        coordinator
            .reconcile(&mut session, &parent_ref()?, 30)?
            .is_empty()
    );
    assert_eq!(session.entries().len(), entry_count);
    Ok(())
}

#[test]
fn legacy_step_result_only_prefix_fails_closed() -> Result<()> {
    let mut session = Session::new("mock", "model");
    let task_id = TaskId::new("task-legacy-step-result")?;
    let step_id = TaskStepId::new("legacy-write")?;
    session.append_control(ControlEntry::TaskRun(TaskRunEntry {
        task_id: task_id.clone(),
        parent_session_ref: parent_ref()?,
        objective: "recover legacy write result".to_owned(),
        title: None,

        status: TaskRunStatus::Running,
        reason: None,
    }))?;
    session.append_control(ControlEntry::TaskStep(TaskStepEntry {
        task_id: task_id.clone(),
        plan_version: 1,
        step_id: step_id.clone(),
        role: AgentRole::Executor,
        status: TaskStepStatus::Running,
        title: Some("legacy write".to_owned()),
        summary: None,
        reason: None,
    }))?;
    let attempt_id = task_participant_attempt_id(
        &task_id,
        TaskParticipantPurpose::Step,
        Some(1),
        Some(&step_id),
        1,
    )?;
    session.append_control(ControlEntry::TaskParticipantAttempt(
        TaskParticipantAttemptEntry {
            attempt_id: attempt_id.clone(),
            task_id: task_id.clone(),
            purpose: TaskParticipantPurpose::Step,
            ordinal: 1,
            plan_version: Some(1),
            step_id: Some(step_id.clone()),
            role: AgentRole::Executor,
            child_session_ref: task_participant_session_ref(&task_id, &attempt_id)?,
            status: TaskParticipantAttemptStatus::Started,
            reason: None,
        },
    ))?;
    let summary = "legacy result may already include side effects".to_owned();
    session.append_control(ControlEntry::TaskParticipantResult(
        TaskParticipantResultEntry {
            attempt_id: attempt_id.clone(),
            task_id: task_id.clone(),
            summary_hash: sha256_prefixed(&summary),
            output_hash: sha256_prefixed("legacy exact output"),
            summary,
            summary_truncated: false,
            terminal_status: None,
            final_answer_ref: None,
            artifact_refs: Vec::new(),
            changed_paths: vec!["src/legacy.rs".to_owned()],
            verification_refs: Vec::new(),
        },
    ))?;

    let coordinator = ConversationCoordinator::new(true, TaskRoutingPolicy::Auto);
    assert!(
        coordinator
            .reconcile(&mut session, &parent_ref()?, 20)?
            .is_empty()
    );

    let projection = session.task_state_projection();
    let task = projection
        .tasks
        .get(&task_id)
        .expect("task remains projected");
    assert_eq!(task.status, TaskRunStatus::Paused);
    assert_eq!(
        task.participant_attempts
            .get(&attempt_id)
            .expect("attempt remains projected")
            .status,
        TaskParticipantAttemptStatus::Interrupted
    );
    assert_eq!(
        task.steps
            .get(&(1, step_id))
            .expect("step remains projected")
            .status,
        TaskStepStatus::Blocked
    );
    Ok(())
}

#[test]
fn reconcile_restarts_a_single_started_synthesis_participant_for_durable_recovery() -> Result<()> {
    let coordinator = ConversationCoordinator::new(true, TaskRoutingPolicy::Auto);
    let temp = tempdir()?;
    let store = JsonlSessionStore::new(temp.path().join("session.jsonl"))?;
    let mut session = Session::new("mock", "model").with_store(store);
    let source = append_source_turn(&mut session, "resume final synthesis safely")?;
    append_requested(&mut session, &source)?;
    let admitted = coordinator.reconcile(&mut session, &parent_ref()?, 10)?;
    let task_id = admitted.first().expect("task is admitted").task_id.clone();
    let step_id = TaskStepId::new("completed-step")?;
    let step = TaskStepSpec {
        step_id: step_id.clone(),
        title: "Completed work".to_owned(),
        display_name: None,
        detail: None,
        role: AgentRole::Executor,
        depends_on: Vec::new(),
        intent_refs: Vec::new(),
        mode: None,
        isolation: Some(TaskIsolationMode::SharedReadOnly),
    };
    session.append_control(ControlEntry::TaskRun(TaskRunEntry {
        task_id: task_id.clone(),
        parent_session_ref: parent_ref()?,
        objective: "resume final synthesis safely".to_owned(),
        title: None,
        status: TaskRunStatus::Running,
        reason: None,
    }))?;
    session.append_control(ControlEntry::TaskPlan(TaskPlanEntry {
        task_id: task_id.clone(),
        plan_version: 1,
        status: TaskPlanStatus::Accepted,
        steps: vec![step.clone()],
        reason: None,
    }))?;
    session.append_control(ControlEntry::TaskStep(TaskStepEntry {
        task_id: task_id.clone(),
        plan_version: 1,
        step_id,
        role: step.role,
        status: TaskStepStatus::Completed,
        title: Some(step.title),
        summary: Some("completed before process loss".to_owned()),
        reason: None,
    }))?;
    let attempt_id = task_participant_attempt_id(
        &task_id,
        TaskParticipantPurpose::Synthesis,
        Some(1),
        None,
        1,
    )?;
    session.append_control(ControlEntry::TaskParticipantAttempt(
        TaskParticipantAttemptEntry {
            attempt_id: attempt_id.clone(),
            task_id: task_id.clone(),
            purpose: TaskParticipantPurpose::Synthesis,
            ordinal: 1,
            plan_version: Some(1),
            step_id: None,
            role: AgentRole::Planner,
            child_session_ref: task_participant_session_ref(&task_id, &attempt_id)?,
            status: TaskParticipantAttemptStatus::Started,
            reason: None,
        },
    ))?;
    let synthesis_attempt = session
        .task_state_projection()
        .tasks
        .get(&task_id)
        .and_then(|task| task.participant_attempts.get(&attempt_id))
        .cloned()
        .expect("started synthesis is projected");
    append_durable_provider_recovery_schedule(&session, &synthesis_attempt)?;

    let actions = coordinator.reconcile(&mut session, &parent_ref()?, 20)?;
    assert_eq!(actions.len(), 1);
    assert_eq!(actions[0].task_id, task_id);
    assert_eq!(
        session
            .task_state_projection()
            .tasks
            .get(&actions[0].task_id)
            .expect("task is retained")
            .status,
        TaskRunStatus::Running
    );
    Ok(())
}

#[test]
fn reconcile_restarts_a_single_started_planner_for_durable_recovery() -> Result<()> {
    let coordinator = ConversationCoordinator::new(true, TaskRoutingPolicy::Auto);
    let temp = tempdir()?;
    let store = JsonlSessionStore::new(temp.path().join("session.jsonl"))?;
    let mut session = Session::new("mock", "model").with_store(store);
    let source = append_source_turn(&mut session, "resume planner safely")?;
    append_requested(&mut session, &source)?;
    let admitted = coordinator.reconcile(&mut session, &parent_ref()?, 10)?;
    let task_id = admitted.first().expect("task is admitted").task_id.clone();
    let attempt_id =
        task_participant_attempt_id(&task_id, TaskParticipantPurpose::Planner, None, None, 1)?;
    session.append_control(ControlEntry::TaskParticipantAttempt(
        TaskParticipantAttemptEntry {
            attempt_id: attempt_id.clone(),
            task_id: task_id.clone(),
            purpose: TaskParticipantPurpose::Planner,
            ordinal: 1,
            plan_version: None,
            step_id: None,
            role: AgentRole::Planner,
            child_session_ref: task_participant_session_ref(&task_id, &attempt_id)?,
            status: TaskParticipantAttemptStatus::Started,
            reason: None,
        },
    ))?;
    let planner_attempt = session
        .task_state_projection()
        .tasks
        .get(&task_id)
        .and_then(|task| task.participant_attempts.get(&attempt_id))
        .cloned()
        .expect("started planner is projected");
    append_durable_provider_recovery_schedule(&session, &planner_attempt)?;

    let projected = session.task_state_projection();
    let task = projected
        .tasks
        .get(&task_id)
        .expect("started planner is projected");
    assert!(
        super::single_started_participant_provider_recovery(&session, task),
        "planner recovery projection: {task:#?}"
    );

    let actions = coordinator.reconcile(&mut session, &parent_ref()?, 20)?;
    assert_eq!(actions.len(), 1);
    assert_eq!(actions[0].task_id, task_id);
    assert_eq!(
        session
            .task_state_projection()
            .tasks
            .get(&actions[0].task_id)
            .expect("task is retained")
            .status,
        TaskRunStatus::Started
    );
    Ok(())
}

#[test]
fn handoff_cancellation_interrupts_started_participant_before_resume() -> Result<()> {
    let coordinator = ConversationCoordinator::new(true, TaskRoutingPolicy::Auto);
    let temp = tempdir()?;
    let store = JsonlSessionStore::new(temp.path().join("session.jsonl"))?;
    let mut session = Session::new("mock", "model").with_store(store);
    let source = append_source_turn(&mut session, "cancel this recovered task")?;
    append_requested(&mut session, &source)?;
    let first = coordinator.reconcile(&mut session, &parent_ref()?, 10)?;
    let action = first.first().expect("task should be admitted");
    let task_id = action.task_id.clone();
    session.append_control(ControlEntry::TaskRun(TaskRunEntry {
        task_id: task_id.clone(),
        parent_session_ref: parent_ref()?,
        objective: "cancel this recovered task".to_owned(),
        title: None,

        status: TaskRunStatus::Running,
        reason: None,
    }))?;
    session.append_control(ControlEntry::TaskRunCancellationScopeBound(
        TaskRunCancellationScopeBoundEntry {
            task_id: task_id.clone(),
            run_scope_id: "cancel-handoff-scope".to_owned(),
        },
    ))?;
    let attempt_id =
        task_participant_attempt_id(&task_id, TaskParticipantPurpose::Planner, None, None, 1)?;
    session.append_control(ControlEntry::TaskParticipantAttempt(
        TaskParticipantAttemptEntry {
            attempt_id: attempt_id.clone(),
            task_id: task_id.clone(),
            purpose: TaskParticipantPurpose::Planner,
            ordinal: 1,
            plan_version: None,
            step_id: None,
            role: AgentRole::Planner,
            child_session_ref: task_participant_session_ref(&task_id, &attempt_id)?,
            status: TaskParticipantAttemptStatus::Started,
            reason: None,
        },
    ))?;
    session
        .run_cancellation_recorder()?
        .append_requested(&RunCancellationRequestedEntry {
            request_id: "cancel-handoff".to_owned(),
            run_scope_id: "cancel-handoff-scope".to_owned(),
            target: RunCancellationTarget::Task {
                task_id: task_id.as_str().to_owned(),
            },
            reason: "user cancelled".to_owned(),
            requested_at_ms: 11,
            quiescence_deadline_ms: 21,
        })?;

    let actions = coordinator.reconcile(&mut session, &parent_ref()?, 30)?;

    assert!(actions.is_empty());
    let projection = session.task_state_projection();
    let task = projection
        .tasks
        .get(&task_id)
        .expect("task remains projected");
    assert_eq!(task.status, TaskRunStatus::Interrupted);
    assert_eq!(
        task.participant_attempts
            .get(&attempt_id)
            .expect("attempt remains projected")
            .status,
        TaskParticipantAttemptStatus::Interrupted
    );
    Ok(())
}

#[test]
fn handoff_admission_prefix_with_root_cancel_never_resumes_task() -> Result<()> {
    let temp = tempdir()?;
    let store = JsonlSessionStore::new(temp.path().join("session.jsonl"))?;
    let mut session = Session::new("mock", "model").with_store(store);
    let source = append_source_turn(&mut session, "cancel before task execution")?;
    let handoff_id = handoff_id_for_source(&source)?;
    let task_id = task_id_for_handoff(&handoff_id)?;
    session.append_control(ControlEntry::TaskRunCancellationScopeBound(
        TaskRunCancellationScopeBoundEntry {
            task_id: task_id.clone(),
            run_scope_id: "handoff-admission-scope".to_owned(),
        },
    ))?;
    append_requested(&mut session, &source)?;
    session
        .run_cancellation_recorder()?
        .append_requested(&RunCancellationRequestedEntry {
            request_id: "cancel-handoff-admission".to_owned(),
            run_scope_id: "handoff-admission-scope".to_owned(),
            target: RunCancellationTarget::Run,
            reason: "user cancelled before execution".to_owned(),
            requested_at_ms: 11,
            quiescence_deadline_ms: 21,
        })?;

    assert!(!session.task_state_projection().tasks.contains_key(&task_id));
    let coordinator = ConversationCoordinator::new(true, TaskRoutingPolicy::Auto);
    let actions = coordinator.reconcile(&mut session, &parent_ref()?, 30)?;

    assert!(actions.is_empty());
    let projection = session.task_state_projection();
    let task = projection
        .tasks
        .get(&task_id)
        .expect("task remains projected");
    assert_eq!(task.status, TaskRunStatus::Interrupted);
    let binding_index = session
        .entries()
        .iter()
        .position(|entry| {
            matches!(
                entry,
                SessionLogEntry::Control(ControlEntry::TaskRunCancellationScopeBound(binding))
                    if binding.task_id == task_id
            )
        })
        .expect("scope binding is durable");
    let started_index = session
        .entries()
        .iter()
        .position(|entry| {
            matches!(
                entry,
                SessionLogEntry::Control(ControlEntry::TaskRun(run))
                    if run.task_id == task_id && run.status == TaskRunStatus::Started
            )
        })
        .expect("task start is durable");
    assert!(binding_index < started_index);
    Ok(())
}

#[test]
fn review_first_capability_binds_plan_review_without_direct_task_authority() -> Result<()> {
    let coordinator = ConversationCoordinator::new(true, TaskRoutingPolicy::Auto);
    let mut session = Session::new("review-first", "planned-model");
    let input = AgentRunInput::user("design the migration before touching anything");
    let bound = coordinator.bind_conversation_input(
        &session,
        input,
        SessionRef::new_relative("session.jsonl")?,
        "review-first-run",
        None,
        42,
    )?;
    let AgentRunPurpose::Conversation(context) = bound.purpose.expect("conversation purpose")
    else {
        panic!("expected conversation purpose");
    };
    assert_eq!(
        context.route_capability,
        sigil_kernel::AutomaticRouteCapability::ReviewFirst
    );
    assert!(
        context.task_handoff.is_none(),
        "ReviewFirst never binds direct task handoff"
    );
    let plan_review = context.plan_review.expect("plan review binding");
    assert_eq!(
        plan_review.plan_review_id,
        sigil_kernel::plan_review_id_for_source(&context.source_turn)
    );
    assert_eq!(
        plan_review.plan_id,
        sigil_kernel::plan_review_plan_id_for_attempt(
            &plan_review.plan_review_id,
            &plan_review.attempt_id
        )
    );
    assert_eq!(
        plan_review.objective,
        "design the migration before touching anything"
    );
    assert!(!plan_review.route_contract_fingerprint.is_empty());
    session.append_user_message(ModelMessage::user(
        "design the migration before touching anything",
    ))?;
    Ok(())
}

#[test]
fn direct_task_capability_binds_both_handoff_authorities() -> Result<()> {
    let coordinator = ConversationCoordinator::new(true, TaskRoutingPolicy::Auto)
        .with_route_capability_evidence(crate::RouteCapabilityEvidence {
            provider_supports_routing_tools: true,
            route_qualified: true,
        });
    let mut session = Session::new("direct-task", "planned-model");
    let input = AgentRunInput::user("ship the cross-layer change in reviewed batches");
    let bound = coordinator.bind_conversation_input(
        &session,
        input,
        SessionRef::new_relative("session.jsonl")?,
        "direct-task-run",
        None,
        42,
    )?;
    let AgentRunPurpose::Conversation(context) = bound.purpose.expect("conversation purpose")
    else {
        panic!("expected conversation purpose");
    };
    assert_eq!(
        context.route_capability,
        sigil_kernel::AutomaticRouteCapability::DirectTask
    );
    assert!(
        context.task_handoff.is_some(),
        "qualified route binds direct task handoff"
    );
    assert!(
        context.plan_review.is_some(),
        "qualified route also binds plan review"
    );
    session.append_user_message(ModelMessage::user(
        "ship the cross-layer change in reviewed batches",
    ))?;
    Ok(())
}

#[test]
fn capability_resolution_is_host_owned_and_fail_closed() -> Result<()> {
    let session = Session::new("capability", "planned-model");
    let unsupported_tools = ConversationCoordinator::new(true, TaskRoutingPolicy::Auto)
        .with_route_capability_evidence(crate::RouteCapabilityEvidence {
            provider_supports_routing_tools: false,
            route_qualified: true,
        });
    assert_eq!(
        unsupported_tools.resolve_route_capability(&session),
        sigil_kernel::AutomaticRouteCapability::Unsupported
    );
    let unqualified = ConversationCoordinator::new(true, TaskRoutingPolicy::Auto);
    assert_eq!(
        unqualified.resolve_route_capability(&session),
        sigil_kernel::AutomaticRouteCapability::ReviewFirst
    );
    let manual = ConversationCoordinator::new(true, TaskRoutingPolicy::Manual);
    assert_eq!(
        manual.resolve_route_capability(&session),
        sigil_kernel::AutomaticRouteCapability::Unsupported
    );
    let disabled = ConversationCoordinator::new(false, TaskRoutingPolicy::Auto);
    assert_eq!(
        disabled.resolve_route_capability(&session),
        sigil_kernel::AutomaticRouteCapability::Unsupported
    );
    let qualified = ConversationCoordinator::new(true, TaskRoutingPolicy::Auto)
        .with_route_capability_evidence(crate::RouteCapabilityEvidence {
            provider_supports_routing_tools: true,
            route_qualified: true,
        });
    assert_eq!(
        qualified.resolve_route_capability(&session),
        sigil_kernel::AutomaticRouteCapability::DirectTask
    );
    Ok(())
}

#[test]
fn writable_memory_is_part_of_the_frozen_route_surface_and_fingerprint() -> Result<()> {
    let without_memory = ConversationCoordinator::new(true, TaskRoutingPolicy::Auto);
    let with_memory = ConversationCoordinator::new(true, TaskRoutingPolicy::Auto)
        .with_writable_memory_routing(true);
    let capability = sigil_kernel::AutomaticRouteCapability::ReviewFirst;
    assert_eq!(without_memory.route_tool_specs(capability).len(), 2);
    let with_memory_specs = with_memory.route_tool_specs(capability);
    assert_eq!(with_memory_specs.len(), 4);
    assert_eq!(
        with_memory_specs
            .iter()
            .map(|spec| spec.name.as_str())
            .collect::<Vec<_>>(),
        vec![
            sigil_kernel::REQUEST_PLAN_REVIEW_TOOL_NAME,
            sigil_kernel::CONTINUE_WITHOUT_TASK_PLANNING_TOOL_NAME,
            sigil_kernel::REMEMBER_USER_PREFERENCE_TOOL_NAME,
            sigil_kernel::REMEMBER_PROJECT_FACT_TOOL_NAME,
        ]
    );
    for spec in &with_memory_specs[2..] {
        assert_eq!(spec.access, sigil_kernel::ToolAccess::Write);
        assert_eq!(spec.preview, sigil_kernel::ToolPreviewCapability::Required);
        assert_eq!(spec.network_effect, None);
    }

    let session_without = Session::new("route-fingerprint", "model");
    let bound_without = without_memory.bind_conversation_input(
        &session_without,
        AgentRunInput::user("review this design"),
        parent_ref()?,
        "route-without-memory",
        None,
        42,
    )?;
    let session_with = Session::new("route-fingerprint", "model");
    let bound_with = with_memory.bind_conversation_input(
        &session_with,
        AgentRunInput::user("review this design"),
        parent_ref()?,
        "route-with-memory",
        None,
        42,
    )?;
    let route_binding = |input: AgentRunInput| {
        let AgentRunPurpose::Conversation(context) = input.purpose.expect("conversation purpose")
        else {
            panic!("expected conversation purpose")
        };
        (
            context.writable_memory_routing,
            context
                .plan_review
                .expect("review-first route has a plan review binding")
                .route_contract_fingerprint,
        )
    };
    let (without_memory_bound, without_memory_fingerprint) = route_binding(bound_without);
    let (with_memory_bound, with_memory_fingerprint) = route_binding(bound_with);
    assert!(!without_memory_bound);
    assert!(with_memory_bound);
    assert_ne!(without_memory_fingerprint, with_memory_fingerprint);
    Ok(())
}

fn seed_current_resumable_task(session: &mut Session) -> Result<TaskId> {
    let task_id = TaskId::new("task-current")?;
    let run = |status| {
        ControlEntry::TaskRun(TaskRunEntry {
            task_id: task_id.clone(),
            parent_session_ref: SessionRef::new_relative("session.jsonl")
                .expect("valid parent session ref"),
            objective: "implement the original plan".to_owned(),
            title: None,
            status,
            reason: None,
        })
    };
    session.append_control(run(TaskRunStatus::Started))?;
    session.append_control(ControlEntry::TaskPlan(TaskPlanEntry {
        task_id: task_id.clone(),
        plan_version: 1,
        status: TaskPlanStatus::Accepted,
        steps: vec![TaskStepSpec {
            step_id: TaskStepId::new("step-current")?,
            title: "implement original scope".to_owned(),
            display_name: None,
            detail: None,
            role: AgentRole::Executor,
            depends_on: Vec::new(),
            intent_refs: Vec::new(),
            mode: None,
            isolation: None,
        }],
        reason: Some("accepted v1".to_owned()),
    }))?;
    session.append_control(run(TaskRunStatus::Paused))?;
    Ok(task_id)
}

fn seed_current_resumable_direct_task(session: &mut Session) -> Result<TaskId> {
    let task_id = TaskId::new("task-current-direct")?;
    let objective = "execute the approved objective directly";
    let run = |status| {
        ControlEntry::TaskRun(TaskRunEntry {
            task_id: task_id.clone(),
            parent_session_ref: SessionRef::new_relative("session.jsonl")
                .expect("valid parent session ref"),
            objective: objective.to_owned(),
            title: None,
            status,
            reason: None,
        })
    };
    session.append_control(run(TaskRunStatus::Started))?;
    session.append_control(ControlEntry::TaskDirectExecutionAdmittedV1(
        TaskDirectExecutionAdmittedV1::approved_plan(
            task_id.clone(),
            objective,
            PlanId::new("plan-current-direct")?,
            format!("sha256:{}", "a".repeat(64)),
            41,
        ),
    ))?;
    session.append_control(run(TaskRunStatus::Paused))?;
    Ok(task_id)
}

#[test]
fn coordinator_keeps_latest_resumable_task_as_a_typed_continuation_candidate() -> Result<()> {
    let coordinator = ConversationCoordinator::new(true, TaskRoutingPolicy::Auto)
        .with_route_capability_evidence(crate::RouteCapabilityEvidence {
            provider_supports_routing_tools: true,
            route_qualified: true,
        });
    let mut session = Session::new("continuation-route", "model");
    let task_id = seed_current_resumable_task(&mut session)?;
    let capability = coordinator.resolve_route_capability(&session);
    assert!(
        coordinator
            .route_tool_specs_for_session(&session, capability)
            .iter()
            .any(|spec| spec.name == sigil_kernel::CONTINUE_EXISTING_TASK_TOOL_NAME)
    );

    let bound = coordinator.bind_conversation_input(
        &session,
        AgentRunInput::user("also add the requested compatibility check"),
        parent_ref()?,
        "continue-current-run",
        None,
        42,
    )?;
    let AgentRunPurpose::Conversation(context) = bound.purpose.expect("conversation purpose")
    else {
        panic!("expected conversation purpose")
    };
    let continuation = context
        .task_continuation
        .expect("current resumable Task should be host-bound");
    assert_eq!(continuation.task_id, task_id);
    assert_eq!(continuation.plan_version, Some(1));
    assert_eq!(continuation.task_status, TaskRunStatus::Paused);
    assert_eq!(continuation.plan_status, Some(TaskPlanStatus::Accepted));

    session.append_user_message(ModelMessage::user("unrelated explanation request"))?;
    assert!(
        coordinator
            .route_tool_specs_for_session(&session, capability)
            .iter()
            .any(|spec| spec.name == sigil_kernel::CONTINUE_EXISTING_TASK_TOOL_NAME),
        "clearing execution focus must not make the host-owned resumable Task undiscoverable"
    );
    Ok(())
}

#[test]
fn coordinator_recovers_direct_continuation_after_chat_clears_focus() -> Result<()> {
    let coordinator = ConversationCoordinator::new(true, TaskRoutingPolicy::Auto)
        .with_route_capability_evidence(crate::RouteCapabilityEvidence {
            provider_supports_routing_tools: true,
            route_qualified: true,
        });
    let mut session = Session::new("direct-continuation-route", "model");
    let task_id = seed_current_resumable_direct_task(&mut session)?;
    let capability = coordinator.resolve_route_capability(&session);
    session.append_user_message(ModelMessage::user(
        "an older router admitted this interjection as ordinary Chat",
    ))?;
    assert!(session.task_state_projection().current_task().is_none());
    assert!(
        coordinator
            .route_tool_specs_for_session(&session, capability)
            .iter()
            .any(|spec| spec.name == sigil_kernel::CONTINUE_EXISTING_TASK_TOOL_NAME),
        "first-class direct Task authority must remain resumable after focus loss and without a synthetic TaskPlan"
    );

    let bound = coordinator.bind_conversation_input(
        &session,
        AgentRunInput::user("what is the current task doing?"),
        parent_ref()?,
        "continue-current-direct-run",
        None,
        42,
    )?;
    let AgentRunPurpose::Conversation(context) = bound.purpose.expect("conversation purpose")
    else {
        panic!("expected conversation purpose")
    };
    let continuation = context
        .task_continuation
        .expect("current direct Task should be host-bound");
    assert_eq!(continuation.task_id, task_id);
    assert_eq!(continuation.plan_version, None);
    assert_eq!(continuation.task_status, TaskRunStatus::Paused);
    assert_eq!(continuation.plan_status, None);
    Ok(())
}

fn continuation_action_fixture() -> Result<(Session, ContinueDurableTaskAction)> {
    continuation_action_fixture_with_guidance("also add the requested compatibility check")
}

fn continuation_action_fixture_with_guidance(
    guidance: &str,
) -> Result<(Session, ContinueDurableTaskAction)> {
    continuation_action_fixture_with_control(
        guidance,
        TaskContinuationControlKind::ApplyCurrentRequestAsGuidance,
    )
}

fn continuation_action_fixture_with_control(
    guidance: &str,
    control: TaskContinuationControlKind,
) -> Result<(Session, ContinueDurableTaskAction)> {
    continuation_action_fixture_with_controls(guidance, control, control)
}

fn continuation_action_fixture_with_controls(
    guidance: &str,
    durable_control: TaskContinuationControlKind,
    action_control: TaskContinuationControlKind,
) -> Result<(Session, ContinueDurableTaskAction)> {
    let mut session = Session::new("continuation-validation", "model");
    let task_id = seed_current_resumable_task(&mut session)?;
    let prompt = project_conversation_prompt_for_persistence(guidance);
    let mut source = ModelMessage::user(guidance);
    source.id = "continuation-source-message".to_owned();
    let source_turn = ConversationTurnRef::new(
        session.session_scope_id(),
        source.id.clone(),
        "continuation-source-run",
    )?;
    session.append_user_message(source)?;
    let route_contract_fingerprint = "sha256:continuation-contract".to_owned();
    session.append_control(ControlEntry::ConversationRouteDecisionRecorded(
        ConversationRouteDecisionRecordedEntry {
            decision_id: conversation_route_decision_id_for_source(&source_turn),
            source_turn: source_turn.clone(),
            route: ConversationRoute::Task,
            reason_codes: Vec::new(),
            configured_policy: TaskRoutingPolicy::Auto,
            effective_capability: sigil_kernel::AutomaticRouteCapability::DirectTask,
            policy_snapshot_hash: "sha256:continuation-policy".to_owned(),
            route_contract_fingerprint: route_contract_fingerprint.clone(),
            decided_at_ms: 42,
        },
    ))?;
    let receipt = TaskContinuationSelectedEntry {
        task_id: task_id.clone(),
        source_turn: source_turn.clone(),
        plan_version: Some(1),
        task_status: TaskRunStatus::Paused,
        plan_status: Some(TaskPlanStatus::Accepted),
        route_contract_fingerprint: route_contract_fingerprint.clone(),
        control: durable_control,
        prompt_hash: prompt.prompt_hash,
        exact_prompt_required: prompt.exact_prompt_required,
        guidance: prompt.safe_prompt,
        selected_at_ms: 42,
    };
    session.append_control(ControlEntry::TaskContinuationSelected(receipt.clone()))?;
    let mut action_receipt = receipt.clone();
    action_receipt.control = action_control;
    Ok((
        session,
        ContinueDurableTaskAction {
            task_id,
            source_turn,
            plan_version: Some(1),
            task_status: TaskRunStatus::Paused,
            plan_status: Some(TaskPlanStatus::Accepted),
            route_contract_fingerprint,
            control: match action_control {
                TaskContinuationControlKind::ResumeTask => TaskContinuationControl::ResumeTask,
                TaskContinuationControlKind::ApplyCurrentRequestAsGuidance => {
                    TaskContinuationControl::ApplyTaskGuidance(guidance.to_owned())
                }
                TaskContinuationControlKind::LegacyUnspecified => {
                    TaskContinuationControl::ApplyTaskGuidance(guidance.to_owned())
                }
            },
            guidance: SecretString::new(guidance),
            guidance_receipt: action_receipt,
        },
    ))
}

#[test]
fn continuation_prompt_words_do_not_override_the_typed_model_decision() -> Result<()> {
    for prompt in ["continue", "resume", "继续", "继续执行", "ship the patch"] {
        let (session, action) = continuation_action_fixture_with_control(
            prompt,
            TaskContinuationControlKind::ApplyCurrentRequestAsGuidance,
        )?;
        assert_eq!(
            action.control(),
            TaskContinuationControl::ApplyTaskGuidance(prompt.to_owned())
        );
        validate_task_continuation_action(&session, &action)?;
    }

    let (session, action) = continuation_action_fixture_with_control(
        "arbitrary text that contains no resume keyword",
        TaskContinuationControlKind::ResumeTask,
    )?;
    assert_eq!(action.control(), TaskContinuationControl::ResumeTask);
    validate_task_continuation_action(&session, &action)?;
    Ok(())
}

#[test]
fn typed_resume_receipt_recovers_without_prompt_matching() -> Result<()> {
    let (session, action) = continuation_action_fixture_with_control(
        "the current user text is not interpreted by host code",
        TaskContinuationControlKind::ResumeTask,
    )?;

    validate_task_continuation_action(&session, &action)?;
    assert_eq!(action.control(), TaskContinuationControl::ResumeTask);
    Ok(())
}

#[test]
fn typed_resume_upgrades_a_legacy_receipt_without_prompt_matching() -> Result<()> {
    let (session, action) = continuation_action_fixture_with_controls(
        "arbitrary current text",
        TaskContinuationControlKind::LegacyUnspecified,
        TaskContinuationControlKind::ResumeTask,
    )?;

    validate_task_continuation_action(&session, &action)?;
    assert_eq!(action.control(), TaskContinuationControl::ResumeTask);
    assert_eq!(
        action.guidance_receipt.control,
        TaskContinuationControlKind::ResumeTask
    );
    Ok(())
}

#[test]
fn continuation_dispatch_rejects_stale_plan_and_status_bindings() -> Result<()> {
    let (mut stale_plan_session, stale_plan_action) = continuation_action_fixture()?;
    validate_task_continuation_action(&stale_plan_session, &stale_plan_action)?;
    stale_plan_session.append_control(ControlEntry::TaskPlan(TaskPlanEntry {
        task_id: stale_plan_action.task_id.clone(),
        plan_version: 2,
        status: TaskPlanStatus::Accepted,
        steps: vec![TaskStepSpec {
            step_id: TaskStepId::new("step-v2")?,
            title: "revised scope".to_owned(),
            display_name: None,
            detail: None,
            role: AgentRole::Executor,
            depends_on: Vec::new(),
            intent_refs: Vec::new(),
            mode: None,
            isolation: None,
        }],
        reason: Some("accepted v2 before dispatch".to_owned()),
    }))?;
    assert!(validate_task_continuation_action(&stale_plan_session, &stale_plan_action).is_err());

    let (mut stale_status_session, stale_status_action) = continuation_action_fixture()?;
    stale_status_session.append_control(ControlEntry::TaskRun(TaskRunEntry {
        task_id: stale_status_action.task_id.clone(),
        parent_session_ref: parent_ref()?,
        objective: "implement the original plan".to_owned(),
        title: None,
        status: TaskRunStatus::Failed,
        reason: Some("status changed before dispatch".to_owned()),
    }))?;
    assert!(
        validate_task_continuation_action(&stale_status_session, &stale_status_action).is_err()
    );
    Ok(())
}

#[test]
fn continuation_dispatch_accepts_reused_pending_selection_after_new_user_clears_focus() -> Result<()>
{
    let (mut session, action) = continuation_action_fixture()?;
    session.append_user_message(ModelMessage::user(
        "also add the requested compatibility check",
    ))?;
    assert!(session.task_state_projection().current_task().is_none());

    let resolved = validate_task_continuation_action(&session, &action)?;

    assert_eq!(resolved.task_id, action.task_id);
    assert!(!resolved.needs_planning());
    let run_scope_id = "scope-reused-pending-selection";
    session.append_controls(vec![
        ControlEntry::TaskRunCancellationScopeBound(TaskRunCancellationScopeBoundEntry {
            task_id: action.task_id.clone(),
            run_scope_id: run_scope_id.to_owned(),
        }),
        ControlEntry::TaskRunTargetSelected(TaskRunTargetSelectedEntry::new(
            action.task_id.clone(),
            run_scope_id,
            action.task_status,
            action.plan_version,
            action.plan_status,
        )),
    ])?;
    let coordinator = ConversationCoordinator::new(true, TaskRoutingPolicy::Auto)
        .with_route_capability_evidence(crate::RouteCapabilityEvidence {
            provider_supports_routing_tools: true,
            route_qualified: true,
        });
    assert!(
        coordinator
            .route_tool_specs_for_session(
                &session,
                sigil_kernel::AutomaticRouteCapability::DirectTask,
            )
            .iter()
            .any(|spec| spec.name == sigil_kernel::CONTINUE_EXISTING_TASK_TOOL_NAME),
        "handler-boundary recovery focus must keep exact continuation available after restart"
    );
    Ok(())
}

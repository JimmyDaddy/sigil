use anyhow::Result;
use sigil_kernel::{
    AgentRunInput, AgentRunPurpose, AutomaticRouteCapability, ControlEntry, ConversationTurnRef,
    ImageAttachment, ImageMimeType, JsonlSessionStore, ModelMessage, RunCancellationRequestedEntry,
    RunCancellationTarget, Session, SessionLogEntry, SessionRef, TaskAdmissionTrigger,
    TaskDirectExecutionAttemptV1, TaskExecutionAttemptStatus, TaskHandoffDecision,
    TaskHandoffRequestedEntry, TaskHandoffResolvedEntry, TaskId, TaskRoutingPolicy,
    TaskRunCancellationScopeBoundEntry, TaskRunEntry, TaskRunStatus, ToolAccess, ToolCategory,
    ToolPreviewCapability, ToolSpec,
};
use tempfile::tempdir;

use super::{
    ConversationCoordinator, automatic_policy_snapshot_hash, handoff_id_for_source,
    task_id_for_handoff,
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
            title: None,
            recovery_objective: None,
            policy_snapshot_hash: automatic_policy_snapshot_hash(),
            requested_at_ms: 42,
        },
    ))
}

#[test]
fn coordinator_binds_stable_host_owned_ids_for_direct_auto_input() -> Result<()> {
    let session = Session::new("mock", "model");
    let coordinator = ConversationCoordinator::new(true, TaskRoutingPolicy::Auto)
        .with_route_capability_evidence(crate::RouteCapabilityEvidence {
            provider_supports_routing_tools: true,
            task_executor_available: true,
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
            task_executor_available: true,
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
fn draft_ready_plan_adds_typed_decision_to_ordinary_tool_surface() -> Result<()> {
    let mut session = Session::new("mock", "model");
    let review = crate::PlanReviewCoordinator::prepare_explicit_plan_review(
        &mut session,
        "implement the approved change",
        "plan-review-run",
        None,
        1,
    )?;
    let mut handler = sigil_kernel::NoopEventHandler;
    crate::PlanReviewCoordinator::ensure_attempt_started(&mut session, &review, &mut handler, 2)?;
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
        &mut handler,
        3,
    )?;

    let coordinator = ConversationCoordinator::new(true, TaskRoutingPolicy::Auto)
        .with_route_capability_evidence(crate::RouteCapabilityEvidence {
            provider_supports_routing_tools: true,
            task_executor_available: true,
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

    assert_eq!(
        coordinator
            .conversation_contract_for_session(&session, AutomaticRouteCapability::DirectTask),
        Some(sigil_kernel::conversation_auto_execution_contract_material())
    );
    let names = coordinator
        .conversation_tool_specs_for_session(
            &session,
            AutomaticRouteCapability::DirectTask,
            vec![
                sigil_kernel::request_user_input_tool_spec(),
                ToolSpec {
                    name: "inspect_workspace".to_owned(),
                    description: "Read one bounded workspace summary".to_owned(),
                    input_schema: serde_json::json!({"type":"object","properties":{}}),
                    category: ToolCategory::File,
                    access: ToolAccess::Read,
                    network_effect: None,
                    preview: ToolPreviewCapability::None,
                },
            ],
        )
        .into_iter()
        .map(|spec| spec.name)
        .collect::<Vec<_>>();
    assert!(
        names
            .iter()
            .any(|name| name == sigil_kernel::RUN_PENDING_PLAN_TOOL_NAME)
    );
    assert!(
        names
            .iter()
            .any(|name| name == sigil_kernel::REQUEST_USER_INPUT_TOOL_NAME)
    );
    assert!(
        names.iter().any(|name| name == "inspect_workspace"),
        "a pending Plan must not replace ordinary business tools with a routing-only turn"
    );
    Ok(())
}

#[test]
fn coordinator_uses_the_exact_durable_url_and_attachment_projection() -> Result<()> {
    let session = Session::new("mock", "model");
    let coordinator = ConversationCoordinator::new(true, TaskRoutingPolicy::Auto)
        .with_route_capability_evidence(crate::RouteCapabilityEvidence {
            provider_supports_routing_tools: true,
            task_executor_available: true,
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
    let task = session.task_state_projection().tasks[&action.task_id].clone();
    let admission = task
        .direct_execution_admission
        .expect("explicit Task authority");
    assert_eq!(
        admission.source,
        sigil_kernel::TaskDirectExecutionSourceV1::TaskRequest
    );
    assert!(admission.matches_objective("execute a durable task"));
    assert_eq!(admission.admitted_at_ms, 17);
    assert_eq!(
        session
            .entries()
            .iter()
            .filter(|entry| matches!(
                entry,
                SessionLogEntry::Control(ControlEntry::TaskDirectExecutionAdmittedV1(_))
            ))
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
    let admission = task
        .direct_execution_admission
        .expect("reconciled direct Task authority");
    assert_eq!(
        admission.source,
        sigil_kernel::TaskDirectExecutionSourceV1::TaskRequest
    );
    assert!(admission.matches_objective(&task.objective));
    assert_eq!(admission.admitted_at_ms, 50);
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
            task_executor_available: true,
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
        "available executor binds direct task handoff"
    );
    assert!(
        context.plan_review.is_some(),
        "available executor also binds plan review"
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
            task_executor_available: true,
        });
    assert_eq!(
        unsupported_tools.resolve_route_capability(&session),
        sigil_kernel::AutomaticRouteCapability::Unsupported
    );
    let without_executor = ConversationCoordinator::new(true, TaskRoutingPolicy::Auto);
    assert_eq!(
        without_executor.resolve_route_capability(&session),
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
    let with_executor = ConversationCoordinator::new(true, TaskRoutingPolicy::Auto)
        .with_route_capability_evidence(crate::RouteCapabilityEvidence {
            provider_supports_routing_tools: true,
            task_executor_available: true,
        });
    assert_eq!(
        with_executor.resolve_route_capability(&session),
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
    assert_eq!(without_memory.route_tool_specs(capability).len(), 1);
    let with_memory_specs = with_memory.route_tool_specs(capability);
    assert_eq!(with_memory_specs.len(), 3);
    assert_eq!(
        with_memory_specs
            .iter()
            .map(|spec| spec.name.as_str())
            .collect::<Vec<_>>(),
        vec![
            sigil_kernel::REQUEST_PLAN_REVIEW_TOOL_NAME,
            sigil_kernel::REMEMBER_USER_PREFERENCE_TOOL_NAME,
            sigil_kernel::REMEMBER_PROJECT_FACT_TOOL_NAME,
        ]
    );
    for spec in &with_memory_specs[1..] {
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

#[test]
fn ordinary_auto_surface_preserves_business_tools_and_optional_handoffs() {
    let coordinator = ConversationCoordinator::new(true, TaskRoutingPolicy::Auto);
    let session = Session::new("route-surface", "model");
    let capability = AutomaticRouteCapability::ReviewFirst;
    let ordinary = sigil_kernel::writable_memory_route_tool_specs();
    let tools =
        coordinator.conversation_tool_specs_for_session(&session, capability, ordinary.clone());
    assert_eq!(
        coordinator.conversation_contract_for_session(&session, capability),
        Some(sigil_kernel::conversation_auto_execution_contract_material())
    );
    for expected in &ordinary {
        assert!(tools.iter().any(|tool| tool.name == expected.name));
    }
    assert!(
        tools
            .iter()
            .any(|tool| tool.name == sigil_kernel::REQUEST_USER_INPUT_TOOL_NAME)
    );
    assert!(
        tools
            .iter()
            .any(|tool| tool.name == sigil_kernel::REQUEST_PLAN_REVIEW_TOOL_NAME)
    );
    assert!(
        !tools
            .iter()
            .any(|tool| tool.name == sigil_kernel::START_TASK_TOOL_NAME)
    );
    assert!(
        coordinator
            .conversation_contract_for_session(&session, AutomaticRouteCapability::Unsupported)
            .is_none()
    );
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
    let admission = crate::direct_plan_fixture::append(session, &task_id, objective)?;
    session.append_control(run(TaskRunStatus::Started))?;
    session.append_control(ControlEntry::TaskDirectExecutionAdmittedV1(admission))?;
    session.append_control(run(TaskRunStatus::Paused))?;
    Ok(task_id)
}

#[test]
fn reconciliation_pauses_orphaned_direct_task_without_replacing_its_attempt() -> Result<()> {
    let coordinator = ConversationCoordinator::new(true, TaskRoutingPolicy::Auto);
    let mut session = Session::new("orphaned-direct-task", "model");
    let task_id = TaskId::new("task-orphaned-direct")?;
    let objective = "finish the existing direct task";
    let admission = crate::direct_plan_fixture::append(&mut session, &task_id, objective)?;
    let run = |status| {
        ControlEntry::TaskRun(TaskRunEntry {
            task_id: task_id.clone(),
            parent_session_ref: parent_ref().expect("valid parent ref"),
            objective: objective.to_owned(),
            title: None,
            status,
            reason: None,
        })
    };
    session.append_control(run(TaskRunStatus::Started))?;
    session.append_control(ControlEntry::TaskDirectExecutionAdmittedV1(
        admission.clone(),
    ))?;
    session.append_control(run(TaskRunStatus::Running))?;
    let attempt = TaskDirectExecutionAttemptV1::started(&admission, 1);
    session.append_control(ControlEntry::TaskDirectExecutionAttemptV1(attempt.clone()))?;

    assert!(
        coordinator
            .reconcile(&mut session, &parent_ref()?, 50)?
            .is_empty()
    );
    let task = &session.task_state_projection().tasks[&task_id];
    assert_eq!(task.status, TaskRunStatus::Paused);
    assert_eq!(task.direct_execution_attempts[&attempt.attempt_id], attempt);
    let entry_count = session.entries().len();
    assert!(
        coordinator
            .reconcile(&mut session, &parent_ref()?, 60)?
            .is_empty()
    );
    assert_eq!(session.entries().len(), entry_count);
    Ok(())
}

#[test]
fn reconciliation_preserves_direct_task_waiting_for_background_agents() -> Result<()> {
    let coordinator = ConversationCoordinator::new(true, TaskRoutingPolicy::Auto);
    let mut session = Session::new("background-direct-task", "model");
    let task_id = TaskId::new("task-background-direct")?;
    let objective = "wait for child results";
    let admission = crate::direct_plan_fixture::append(&mut session, &task_id, objective)?;
    let run = |status| {
        ControlEntry::TaskRun(TaskRunEntry {
            task_id: task_id.clone(),
            parent_session_ref: parent_ref().expect("valid parent ref"),
            objective: objective.to_owned(),
            title: None,
            status,
            reason: None,
        })
    };
    session.append_control(run(TaskRunStatus::Started))?;
    session.append_control(ControlEntry::TaskDirectExecutionAdmittedV1(
        admission.clone(),
    ))?;
    let mut attempt = TaskDirectExecutionAttemptV1::started(&admission, 1);
    session.append_control(ControlEntry::TaskDirectExecutionAttemptV1(attempt.clone()))?;
    attempt.status = TaskExecutionAttemptStatus::Completed;
    attempt.final_message_id = Some("message-1".to_owned());
    attempt.output_hash = Some(format!("sha256:{}", "a".repeat(64)));
    session.append_control(ControlEntry::TaskDirectExecutionAttemptV1(attempt))?;
    session.append_control(run(TaskRunStatus::Running))?;

    let entry_count = session.entries().len();
    assert!(
        coordinator
            .reconcile(&mut session, &parent_ref()?, 50)?
            .is_empty()
    );
    assert_eq!(session.entries().len(), entry_count);
    assert_eq!(
        session.task_state_projection().tasks[&task_id].status,
        TaskRunStatus::Running
    );
    Ok(())
}

#[test]
fn coordinator_recovers_direct_continuation_after_chat_clears_focus() -> Result<()> {
    let coordinator = ConversationCoordinator::new(true, TaskRoutingPolicy::Auto)
        .with_route_capability_evidence(crate::RouteCapabilityEvidence {
            provider_supports_routing_tools: true,
            task_executor_available: true,
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
    assert_eq!(continuation.task_status, TaskRunStatus::Paused);
    Ok(())
}

#[test]
fn accepted_started_crash_gap_restores_exact_direct_admission_once() -> Result<()> {
    let coordinator = ConversationCoordinator::new(true, TaskRoutingPolicy::Auto);
    let mut session = Session::new("mock", "model");
    let source = append_source_turn(&mut session, "durable objective")?;
    append_requested(&mut session, &source)?;
    let handoff_id = handoff_id_for_source(&source)?;
    let task_id = task_id_for_handoff(&handoff_id)?;
    session.append_controls(vec![
        ControlEntry::TaskHandoffResolved(TaskHandoffResolvedEntry {
            handoff_id,
            decision: TaskHandoffDecision::Accepted,
            task_id: Some(task_id.clone()),
            decided_at_ms: 43,
        }),
        ControlEntry::TaskRun(TaskRunEntry {
            task_id: task_id.clone(),
            parent_session_ref: parent_ref()?,
            objective: "durable objective".into(),
            title: None,
            status: TaskRunStatus::Started,
            reason: None,
        }),
    ])?;
    let before = session.entries().len();
    let actions = coordinator.reconcile(&mut session, &parent_ref()?, 50)?;
    assert_eq!(actions.len(), 1);
    assert_eq!(session.entries().len(), before + 1);
    let task = session.task_state_projection().tasks[&task_id].clone();
    let admission = task
        .direct_execution_admission
        .expect("recovered execution authority");
    assert_eq!(
        admission.admitted_at_ms, 43,
        "bind original accepted decision, not retry time"
    );
    assert_eq!(
        admission.source,
        sigil_kernel::TaskDirectExecutionSourceV1::TaskRequest
    );
    assert!(admission.matches_objective("durable objective"));
    assert_eq!(
        coordinator.reconcile(&mut session, &parent_ref()?, 60)?,
        actions
    );
    assert_eq!(session.entries().len(), before + 1);
    Ok(())
}

#[test]
fn accepted_handoff_rejects_foreign_direct_authority_without_mutation() -> Result<()> {
    let coordinator = ConversationCoordinator::new(true, TaskRoutingPolicy::Auto);
    let mut session = Session::new("mock", "model");
    let source = append_source_turn(&mut session, "durable objective")?;
    append_requested(&mut session, &source)?;
    let handoff_id = handoff_id_for_source(&source)?;
    let task_id = task_id_for_handoff(&handoff_id)?;
    session.append_controls(vec![
        ControlEntry::TaskHandoffResolved(TaskHandoffResolvedEntry {
            handoff_id,
            decision: TaskHandoffDecision::Accepted,
            task_id: Some(task_id.clone()),
            decided_at_ms: 43,
        }),
        ControlEntry::TaskRun(TaskRunEntry {
            task_id: task_id.clone(),
            parent_session_ref: parent_ref()?,
            objective: "durable objective".into(),
            title: None,
            status: TaskRunStatus::Started,
            reason: None,
        }),
        ControlEntry::TaskDirectExecutionAdmittedV1(
            sigil_kernel::TaskDirectExecutionAdmittedV1::approved_plan(
                task_id,
                "durable objective",
                sigil_kernel::PlanId::new("another-plan")?,
                format!("sha256:{}", "a".repeat(64)),
                43,
            ),
        ),
    ])?;
    let before = session.entries().len();
    let error = coordinator
        .reconcile(&mut session, &parent_ref()?, 50)
        .expect_err("conflicting direct execution authority must be rejected");
    assert!(
        error
            .to_string()
            .contains("conflicting direct execution authority")
    );
    assert_eq!(session.entries().len(), before);
    Ok(())
}

#[test]
fn accepted_handoff_does_not_grant_authority_after_legacy_execution_started() -> Result<()> {
    let coordinator = ConversationCoordinator::new(true, TaskRoutingPolicy::Auto);
    let mut session = Session::new("mock", "model");
    let source = append_source_turn(&mut session, "durable objective")?;
    append_requested(&mut session, &source)?;
    let handoff_id = handoff_id_for_source(&source)?;
    let task_id = task_id_for_handoff(&handoff_id)?;
    session.append_controls(vec![
        ControlEntry::TaskHandoffResolved(TaskHandoffResolvedEntry {
            handoff_id,
            decision: TaskHandoffDecision::Accepted,
            task_id: Some(task_id.clone()),
            decided_at_ms: 43,
        }),
        ControlEntry::TaskRun(TaskRunEntry {
            task_id,
            parent_session_ref: parent_ref()?,
            objective: "durable objective".into(),
            title: None,
            status: TaskRunStatus::Running,
            reason: None,
        }),
    ])?;
    let before = session.entries().len();
    let error = coordinator
        .reconcile(&mut session, &parent_ref()?, 50)
        .expect_err("missing direct execution authority must be rejected");
    assert!(
        error
            .to_string()
            .contains("missing direct execution authority after start")
    );
    assert_eq!(session.entries().len(), before);
    Ok(())
}

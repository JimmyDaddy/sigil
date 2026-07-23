use anyhow::Result;
use sha2::{Digest, Sha256};
use sigil_kernel::{
    AgentFinalAnswerRef, AgentRole, AgentRunInput, AgentRunPurpose, AssistantMessageKind,
    ControlEntry, ConversationTurnRef, ImageAttachment, ImageMimeType, JsonlSessionStore,
    ModelMessage, RunCancellationRequestedEntry, RunCancellationTarget, Session, SessionLogEntry,
    SessionRef, TaskAdmissionReason, TaskAdmissionTrigger, TaskHandoffDecision,
    TaskHandoffRequestedEntry, TaskHandoffResolvedEntry, TaskId, TaskParticipantAttemptEntry,
    TaskParticipantAttemptStatus, TaskParticipantPurpose, TaskParticipantResultEntry,
    TaskPlanEntry, TaskPlanStatus, TaskRoutingPolicy, TaskRunCancellationScopeBoundEntry,
    TaskRunEntry, TaskRunStatus, TaskStepEntry, TaskStepId, TaskStepSpec, TaskStepStatus,
    WriteIsolationMode, WriteLeaseAcquired, WriteLeaseId, WriteLeaseScope,
    durable_task_cancellation_requested, task_participant_attempt_id, task_participant_session_ref,
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
            reason_codes: vec![TaskAdmissionReason::MultiStageChange],
            recovery_objective: None,
            policy_snapshot_hash: automatic_policy_snapshot_hash(),
            requested_at_ms: 42,
        },
    ))
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
    let coordinator = ConversationCoordinator::new(true, TaskRoutingPolicy::Auto);
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
fn coordinator_uses_the_exact_durable_url_and_attachment_projection() -> Result<()> {
    let session = Session::new("mock", "model");
    let coordinator = ConversationCoordinator::new(true, TaskRoutingPolicy::Auto);
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
fn running_task_recovery_interrupts_stale_steps_and_requires_explicit_continue() -> Result<()> {
    let coordinator = ConversationCoordinator::new(true, TaskRoutingPolicy::Auto);
    let mut session = Session::new("mock", "model");
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
            mode: None,
            isolation: None,
        }],
        reason: None,
    }))?;
    session.append_control(ControlEntry::TaskRun(TaskRunEntry {
        task_id: action.task_id.clone(),
        parent_session_ref: parent_ref()?,
        objective: "durable objective".to_owned(),
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
    let mut session = Session::new("mock", "model").with_store(store);
    let task_id = sigil_kernel::TaskId::new("task-result-only-prefix")?;
    session.append_control(ControlEntry::TaskRun(TaskRunEntry {
        task_id: task_id.clone(),
        parent_session_ref: parent_ref()?,
        objective: "recover result-only synthesis".to_owned(),
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
    let mut child_session = Session::new("mock", "model").with_store(child_store);
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
            mode: None,
            isolation: None,
        }],
        reason: None,
    }))?;
    session.append_control(ControlEntry::TaskRun(TaskRunEntry {
        task_id: task_id.clone(),
        parent_session_ref: parent_ref()?,
        objective: "change the workspace once".to_owned(),
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

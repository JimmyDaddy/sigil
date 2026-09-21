use super::*;
use crate::{
    AutomaticRouteCapability, ConversationTurnRef, ModelMessage, SecretString, SessionRef,
    TaskDirectExecutionAdmittedV1, TaskId,
};

fn direct_continuation_fixture() -> Result<(Session, TaskContinuationHandoffBinding)> {
    let mut session = Session::new("fixture", "fixture");
    let task_id = TaskId::new("direct-continuation")?;
    let objective = "finish the approved task";
    session.append_control(ControlEntry::TaskRun(TaskRunEntry {
        task_id: task_id.clone(),
        parent_session_ref: SessionRef::new_relative("parent.jsonl")?,
        objective: objective.to_owned(),
        title: None,
        status: TaskRunStatus::Paused,
        reason: None,
    }))?;
    session.append_control(ControlEntry::TaskDirectExecutionAdmittedV1(
        TaskDirectExecutionAdmittedV1::task_request(task_id.clone(), objective, 1),
    ))?;
    let guidance = "continue the existing task";
    let message = ModelMessage::user(guidance);
    let source_turn = ConversationTurnRef::new(
        session.session_scope_id(),
        message.id.clone(),
        "continuation-run",
    )?;
    session.append_user_message(message)?;
    let safe = crate::project_conversation_prompt_for_persistence(guidance);
    let binding = TaskContinuationHandoffBinding {
        task_id,
        source_turn,
        task_status: TaskRunStatus::Paused,
        effective_capability: AutomaticRouteCapability::DirectTask,
        policy_snapshot_hash: "policy-fixture".to_owned(),
        route_contract_fingerprint: "route-fixture".to_owned(),
        decided_at_ms: 1,
        exact_guidance: SecretString::new(guidance),
        prompt_hash: safe.prompt_hash,
        exact_prompt_required: safe.exact_prompt_required,
        safe_guidance: safe.safe_prompt,
    };
    Ok((session, binding))
}

#[test]
fn start_task_atomically_admits_direct_execution() -> Result<()> {
    let mut session = Session::new("fixture", "fixture");
    let objective = "inspect the repository and fix the reported issue";
    let user_message = ModelMessage::user(objective);
    let source_turn = ConversationTurnRef::new(
        session.session_scope_id(),
        user_message.id.clone(),
        "task-handoff-run",
    )?;
    session.append_user_message(user_message)?;

    let binding = TaskStartHandoffBinding {
        handoff_id: crate::TaskHandoffId::new("start-task-admission")?,
        task_id: TaskId::new("direct-task-admission")?,
        source_turn,
        parent_session_ref: crate::SessionRef::new_relative("parent.jsonl")?,
        objective: objective.to_owned(),
        policy_snapshot_hash: "sha256:policy".to_owned(),
        route_contract_fingerprint: "sha256:route".to_owned(),
        requested_at_ms: 1,
        decided_at_ms: 2,
    };
    let call = ToolCall {
        id: "call-start-task".to_owned(),
        name: START_TASK_TOOL_NAME.to_owned(),
        args_json: "{}".to_owned(),
    };
    let mut outcome = AgentRunOutcome::default();
    let mut results = Vec::new();
    let accepted = handle_start_task_call(
        &mut session,
        &mut crate::event::NoopEventHandler,
        &mut outcome,
        &call,
        &binding,
        "task-handoff-run",
        &mut results,
    )?
    .expect("the exact host-bound handoff should be accepted");
    assert_eq!(accepted.task_id, binding.task_id);

    let task = session
        .task_state_projection()
        .tasks
        .get(&binding.task_id)
        .cloned()
        .expect("accepted handoff should durably create its Task");
    let admission = task
        .direct_execution_admission
        .expect("accepted handoff should durably admit direct execution");
    admission.validate()?;
    assert!(admission.matches_objective(objective));
    assert_eq!(
        admission.source,
        TaskDirectExecutionAdmittedV1::task_request(binding.task_id.clone(), objective, 2).source
    );

    let task_run_index = session
        .entries()
        .iter()
        .position(|entry| {
            matches!(
                entry,
                SessionLogEntry::Control(ControlEntry::TaskRun(task_run))
                    if task_run.task_id == binding.task_id
            )
        })
        .expect("TaskRun should be durable");
    assert!(matches!(
        session.entries().get(task_run_index + 1),
        Some(SessionLogEntry::Control(ControlEntry::TaskDirectExecutionAdmittedV1(entry)))
            if entry == &admission
    ));
    Ok(())
}

#[test]
fn direct_task_continuation_requires_no_executable_plan() -> Result<()> {
    for action in ["resume_task", "apply_current_request_as_guidance"] {
        let (mut session, binding) = direct_continuation_fixture()?;
        let mut outcome = AgentRunOutcome::default();
        let mut results = Vec::new();
        let call = ToolCall {
            id: "continue-call".to_owned(),
            name: CONTINUE_EXISTING_TASK_TOOL_NAME.to_owned(),
            args_json: json!({"reason": "continue_current_task", "action": action}).to_string(),
        };
        let result = handle_continue_existing_task_call(
            &mut session,
            &mut crate::event::NoopEventHandler,
            &mut outcome,
            &call,
            &binding,
            None,
            &mut results,
        )?;
        let accepted = result.expect("a direct admission must resume without an executable plan");
        assert_eq!(accepted.task_id, binding.task_id);
        assert!(
            session
                .entries()
                .iter()
                .all(|entry| !matches!(entry, SessionLogEntry::Control(ControlEntry::TaskPlan(_))))
        );
    }
    Ok(())
}

#[test]
fn continuation_rejects_a_task_without_execution_authority() -> Result<()> {
    let (session, binding) = direct_continuation_fixture()?;
    let mut unbound = Session::new("fixture", "fixture");
    for entry in session.entries() {
        match entry {
            SessionLogEntry::Control(ControlEntry::TaskRun(run)) => {
                unbound.append_control(ControlEntry::TaskRun(run.clone()))?;
            }
            SessionLogEntry::User(message) => unbound.append_user_message(message.clone())?,
            _ => {}
        }
    }
    let mut binding = binding;
    binding.source_turn.session_scope_id = unbound.session_scope_id().to_owned();
    assert!(validate_task_continuation_binding(&unbound, &binding).is_err());
    Ok(())
}

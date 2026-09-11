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
        TaskDirectExecutionAdmittedV1::planner_fallback(
            task_id.clone(),
            objective,
            "planner-attempt",
            1,
        ),
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
        plan_version: None,
        task_status: TaskRunStatus::Paused,
        plan_status: None,
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
        assert_eq!(accepted.plan_version, None);
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

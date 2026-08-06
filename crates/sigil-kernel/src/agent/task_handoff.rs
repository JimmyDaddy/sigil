use anyhow::{Result, anyhow, bail};
use serde_json::json;

use crate::{
    CONTINUE_WITHOUT_TASK_PLANNING_TOOL_NAME, ControlEntry, ConversationTurnRef, EventHandler,
    REQUEST_TASK_PLANNING_TOOL_NAME, RunEvent, Session, SessionLogEntry, StartDurableTaskAction,
    TaskAdmissionTrigger, TaskHandoffDecision, TaskHandoffRequestedEntry, TaskHandoffResolvedEntry,
    TaskPlanningHandoffBinding, TaskRunCancellationScopeBoundEntry, TaskRunEntry, TaskRunStatus,
    ToolCall, ToolErrorKind, ToolExecutionStatus, ToolResult, ToolResultMeta,
    task_planning_reason_codes, validate_continue_without_task_planning_call,
};

use super::{
    AgentRunOutcome,
    tool_audit::{append_tool_execution_audit, attach_tool_call_context},
    tool_results::record_tool_result_to_batch,
};

pub(super) fn task_planning_request_call_is_accepted(call: &ToolCall) -> bool {
    call.name == REQUEST_TASK_PLANNING_TOOL_NAME && task_planning_reason_codes(call).is_ok()
}

pub(super) fn continue_without_task_planning_call_is_accepted(call: &ToolCall) -> bool {
    call.name == CONTINUE_WITHOUT_TASK_PLANNING_TOOL_NAME
        && validate_continue_without_task_planning_call(call).is_ok()
}

pub(super) fn handle_continue_without_task_planning_call(
    session: &mut Session,
    outcome: &mut AgentRunOutcome,
    call: &ToolCall,
    assistant_batch_results: &mut Vec<(crate::ToolCall, ToolResult)>,
) -> Result<bool> {
    append_tool_execution_audit(session, call, &[], ToolExecutionStatus::Started, None, None)?;
    if let Err(error) = validate_continue_without_task_planning_call(call) {
        let mut result = ToolResult::error(
            call.id.clone(),
            call.name.clone(),
            ToolErrorKind::InvalidInput,
            error.to_string(),
        );
        attach_tool_call_context(&mut result, call, &[]);
        append_tool_execution_audit(
            session,
            call,
            &[],
            ToolExecutionStatus::Failed,
            None,
            Some(&result),
        )?;
        record_tool_result_to_batch(outcome, call, result, assistant_batch_results);
        return Ok(false);
    }

    let result = ToolResult::ok(
        call.id.clone(),
        call.name.clone(),
        "ordinary conversation routing accepted; continue with the user's request",
        ToolResultMeta {
            details: json!({"status": "accepted"}),
            ..ToolResultMeta::default()
        },
    );
    append_tool_execution_audit(
        session,
        call,
        &[],
        ToolExecutionStatus::Completed,
        None,
        Some(&result),
    )?;
    record_tool_result_to_batch(outcome, call, result, assistant_batch_results);
    Ok(true)
}

pub(super) fn handle_task_planning_request_call<H>(
    session: &mut Session,
    handler: &mut H,
    outcome: &mut AgentRunOutcome,
    call: &ToolCall,
    binding: &TaskPlanningHandoffBinding,
    run_scope_id: &str,
    assistant_batch_results: &mut Vec<(crate::ToolCall, ToolResult)>,
) -> Result<Option<StartDurableTaskAction>>
where
    H: EventHandler + Send,
{
    append_tool_execution_audit(session, call, &[], ToolExecutionStatus::Started, None, None)?;
    let reason_codes = match task_planning_reason_codes(call) {
        Ok(reason_codes) => reason_codes,
        Err(error) => {
            let mut result = ToolResult::error(
                call.id.clone(),
                call.name.clone(),
                ToolErrorKind::InvalidInput,
                error.to_string(),
            );
            attach_tool_call_context(&mut result, call, &[]);
            append_tool_execution_audit(
                session,
                call,
                &[],
                ToolExecutionStatus::Failed,
                None,
                Some(&result),
            )?;
            record_tool_result_to_batch(outcome, call, result, assistant_batch_results);
            return Ok(None);
        }
    };

    validate_binding_against_session(session, binding)?;
    let projection = session.task_handoff_projection();
    if projection.has_conflicts() {
        bail!("task handoff projection contains conflicting durable facts");
    }
    if let Some(existing) = projection.handoff_for_source(&binding.source_turn)
        && existing
            .request
            .as_ref()
            .is_some_and(|request| request.handoff_id != binding.handoff_id)
    {
        bail!("source turn is already bound to a different task handoff");
    }

    append_task_route_decision(session, handler, binding)?;

    let existing = projection.handoffs.get(&binding.handoff_id);
    let latest_bound_scope = session
        .entries()
        .iter()
        .rev()
        .find_map(|entry| match entry {
            SessionLogEntry::Control(ControlEntry::TaskRunCancellationScopeBound(bound))
                if bound.task_id == binding.task_id =>
            {
                Some(bound.run_scope_id.as_str())
            }
            _ => None,
        });
    if latest_bound_scope != Some(run_scope_id) {
        append_control(
            session,
            handler,
            ControlEntry::TaskRunCancellationScopeBound(TaskRunCancellationScopeBoundEntry {
                task_id: binding.task_id.clone(),
                run_scope_id: run_scope_id.to_owned(),
            }),
        )?;
    }
    match existing.and_then(|state| state.request.as_ref()) {
        Some(request)
            if request.source_turn != binding.source_turn
                || request.trigger != TaskAdmissionTrigger::ModelRequested
                || request.policy_snapshot_hash != binding.policy_snapshot_hash =>
        {
            bail!("task handoff request facts conflict with the host binding");
        }
        Some(_) => {}
        None => append_control(
            session,
            handler,
            ControlEntry::TaskHandoffRequested(TaskHandoffRequestedEntry {
                handoff_id: binding.handoff_id.clone(),
                source_turn: binding.source_turn.clone(),
                trigger: TaskAdmissionTrigger::ModelRequested,
                reason_codes,
                recovery_objective: None,
                policy_snapshot_hash: binding.policy_snapshot_hash.clone(),
                requested_at_ms: binding.requested_at_ms,
            }),
        )?,
    }

    match existing.and_then(|state| state.resolution.as_ref()) {
        Some(resolution)
            if resolution.decision != TaskHandoffDecision::Accepted
                || resolution.task_id.as_ref() != Some(&binding.task_id) =>
        {
            bail!("task handoff resolution conflicts with the host binding");
        }
        Some(_) => {}
        None => append_control(
            session,
            handler,
            ControlEntry::TaskHandoffResolved(TaskHandoffResolvedEntry {
                handoff_id: binding.handoff_id.clone(),
                decision: TaskHandoffDecision::Accepted,
                task_id: Some(binding.task_id.clone()),
                decided_at_ms: binding.decided_at_ms,
            }),
        )?,
    }

    ensure_task_started(session, handler, binding)?;

    let metadata = ToolResultMeta {
        details: json!({
            "handoff_id": binding.handoff_id.as_str(),
            "task_id": binding.task_id.as_str(),
            "status": "accepted",
        }),
        ..ToolResultMeta::default()
    };
    let result = ToolResult::ok(
        call.id.clone(),
        call.name.clone(),
        "durable task planning accepted; the conversation coordinator will continue the root run",
        metadata,
    );
    append_tool_execution_audit(
        session,
        call,
        &[],
        ToolExecutionStatus::Completed,
        None,
        Some(&result),
    )?;
    record_tool_result_to_batch(outcome, call, result, assistant_batch_results);
    Ok(Some(StartDurableTaskAction {
        handoff_id: binding.handoff_id.clone(),
        task_id: binding.task_id.clone(),
        source_turn: binding.source_turn.clone(),
    }))
}

fn append_task_route_decision<H>(
    session: &mut Session,
    handler: &mut H,
    binding: &TaskPlanningHandoffBinding,
) -> Result<()>
where
    H: EventHandler + Send,
{
    use crate::conversation_route::{
        AutomaticRouteCapability, ConversationRoute, ConversationRouteDecisionProjection,
        ConversationRouteDecisionRecordedEntry, conversation_route_decision_id_for_source,
    };
    let projection = ConversationRouteDecisionProjection::from_entries(session.entries());
    if projection.has_conflicts() {
        bail!("conversation route decision projection contains conflicting durable facts");
    }
    let decision_id = conversation_route_decision_id_for_source(&binding.source_turn);
    if let Some(existing) = projection.decision_id_for_source(&binding.source_turn)
        && existing != &decision_id
    {
        bail!("source turn is already bound to a different route decision");
    }
    let entry = ConversationRouteDecisionRecordedEntry {
        decision_id,
        source_turn: binding.source_turn.clone(),
        route: ConversationRoute::Task,
        // Task decision reasons stay typed in TaskHandoffRequestedEntry.reason_codes; the route
        // decision's bounded reason enum is plan-review-specific.
        reason_codes: Vec::new(),
        configured_policy: crate::TaskRoutingPolicy::Auto,
        effective_capability: AutomaticRouteCapability::DirectTask,
        policy_snapshot_hash: binding.policy_snapshot_hash.clone(),
        route_contract_fingerprint: binding.route_contract_fingerprint.clone(),
        decided_at_ms: binding.decided_at_ms,
    };
    match projection.decision(&entry.decision_id) {
        None => append_control(
            session,
            handler,
            ControlEntry::ConversationRouteDecisionRecorded(entry),
        )?,
        Some(previous) if previous == &entry => {}
        Some(_) => {
            bail!(
                "route decision {} has conflicting durable facts",
                entry.decision_id.as_str()
            );
        }
    }
    Ok(())
}

pub(super) fn append_tool_ignored_after_task_handoff(
    session: &mut Session,
    outcome: &mut AgentRunOutcome,
    call: &ToolCall,
    assistant_batch_results: &mut Vec<(crate::ToolCall, ToolResult)>,
) -> Result<()> {
    let mut result = ToolResult::error(
        call.id.clone(),
        call.name.clone(),
        ToolErrorKind::Unsupported,
        "durable task handoff was accepted; additional tool calls in this model response were ignored",
    );
    attach_tool_call_context(&mut result, call, &[]);
    append_tool_execution_audit(
        session,
        call,
        &[],
        ToolExecutionStatus::Cancelled,
        None,
        Some(&result),
    )?;
    record_tool_result_to_batch(outcome, call, result, assistant_batch_results);
    Ok(())
}

pub(super) fn append_tool_ignored_after_routing_decision(
    session: &mut Session,
    outcome: &mut AgentRunOutcome,
    call: &ToolCall,
    assistant_batch_results: &mut Vec<(crate::ToolCall, ToolResult)>,
) -> Result<()> {
    let mut result = ToolResult::error(
        call.id.clone(),
        call.name.clone(),
        ToolErrorKind::Unsupported,
        "a typed task-routing decision was accepted; additional tool calls in this routing microturn were ignored",
    );
    attach_tool_call_context(&mut result, call, &[]);
    append_tool_execution_audit(
        session,
        call,
        &[],
        ToolExecutionStatus::Cancelled,
        None,
        Some(&result),
    )?;
    record_tool_result_to_batch(outcome, call, result, assistant_batch_results);
    Ok(())
}

pub(super) fn append_tool_rejected_during_task_routing(
    session: &mut Session,
    outcome: &mut AgentRunOutcome,
    call: &ToolCall,
    assistant_batch_results: &mut Vec<(crate::ToolCall, ToolResult)>,
) -> Result<()> {
    let mut result = ToolResult::error(
        call.id.clone(),
        call.name.clone(),
        ToolErrorKind::Unsupported,
        "ordinary tools are not available during the typed task-routing microturn",
    );
    attach_tool_call_context(&mut result, call, &[]);
    append_tool_execution_audit(
        session,
        call,
        &[],
        ToolExecutionStatus::Failed,
        None,
        Some(&result),
    )?;
    record_tool_result_to_batch(outcome, call, result, assistant_batch_results);
    Ok(())
}

fn validate_binding_against_session(
    session: &Session,
    binding: &TaskPlanningHandoffBinding,
) -> Result<()> {
    if binding.source_turn.session_scope_id != session.session_scope_id() {
        bail!("task handoff source belongs to a different session");
    }
    let objective = source_turn_objective(session, &binding.source_turn).ok_or_else(|| {
        anyhow!(
            "task handoff source user turn {} is not present",
            binding.source_turn.message_id
        )
    })?;
    if objective != binding.objective {
        bail!("task handoff objective does not match the persisted source turn");
    }
    if binding.policy_snapshot_hash.trim().is_empty() {
        bail!("task handoff policy snapshot hash is empty");
    }
    Ok(())
}

fn source_turn_objective(session: &Session, source_turn: &ConversationTurnRef) -> Option<String> {
    session.entries().iter().find_map(|entry| match entry {
        SessionLogEntry::User(message) if message.id == source_turn.message_id => {
            Some(message.content.clone().unwrap_or_default())
        }
        SessionLogEntry::Control(ControlEntry::ConversationInputPromoted(promoted))
            if promoted.durable_user_message.id == source_turn.message_id =>
        {
            Some(
                promoted
                    .durable_user_message
                    .content
                    .clone()
                    .unwrap_or_default(),
            )
        }
        _ => None,
    })
}

fn ensure_task_started<H>(
    session: &mut Session,
    handler: &mut H,
    binding: &TaskPlanningHandoffBinding,
) -> Result<()>
where
    H: EventHandler + Send,
{
    if let Some(task) = session.task_state_projection().tasks.get(&binding.task_id) {
        if task.parent_session_ref != binding.parent_session_ref
            || task.objective != binding.objective
        {
            bail!("task handoff target already exists with conflicting facts");
        }
        return Ok(());
    }
    append_control(
        session,
        handler,
        ControlEntry::TaskRun(TaskRunEntry {
            task_id: binding.task_id.clone(),
            parent_session_ref: binding.parent_session_ref.clone(),
            objective: binding.objective.clone(),
            status: TaskRunStatus::Started,
            reason: Some("admitted from conversation handoff".to_owned()),
        }),
    )
}

fn append_control<H>(session: &mut Session, handler: &mut H, control: ControlEntry) -> Result<()>
where
    H: EventHandler + Send,
{
    session.append_control(control.clone())?;
    handler.handle(RunEvent::Control(control))
}

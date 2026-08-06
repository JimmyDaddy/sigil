use anyhow::{Result, anyhow, bail};
use serde_json::json;

use crate::{
    AutomaticRouteCapability, ControlEntry, ConversationRoute, ConversationRouteDecisionProjection,
    ConversationRouteDecisionRecordedEntry, ConversationTurnRef, EventHandler,
    PlanReviewHandoffBinding, RunEvent, Session, SessionLogEntry, StartPlanReviewAction,
    TaskRoutingPolicy, ToolCall, ToolErrorKind, ToolExecutionStatus, ToolResult, ToolResultMeta,
    plan_review_reason_codes,
};

use super::{
    AgentRunOutcome,
    tool_audit::{append_tool_execution_audit, attach_tool_call_context},
    tool_results::record_tool_result_to_batch,
};

pub(super) fn plan_review_call_is_accepted(
    _binding: &PlanReviewHandoffBinding,
    call: &ToolCall,
) -> bool {
    call.name == crate::REQUEST_PLAN_REVIEW_TOOL_NAME && plan_review_reason_codes(call).is_ok()
}

pub(super) fn handle_request_plan_review_call<H>(
    session: &mut Session,
    handler: &mut H,
    outcome: &mut AgentRunOutcome,
    call: &ToolCall,
    binding: &PlanReviewHandoffBinding,
    _run_scope_id: &str,
    assistant_batch_results: &mut Vec<(crate::ToolCall, ToolResult)>,
) -> Result<Option<StartPlanReviewAction>>
where
    H: EventHandler + Send,
{
    append_tool_execution_audit(session, call, &[], ToolExecutionStatus::Started, None, None)?;
    let reason_codes = match plan_review_reason_codes(call) {
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
    let projection = ConversationRouteDecisionProjection::from_entries(session.entries());
    if projection.has_conflicts() {
        bail!("conversation route decision projection contains conflicting durable facts");
    }
    if let Some(existing) = projection.decision_id_for_source(&binding.source_turn)
        && existing != &binding.decision_id
    {
        bail!("source turn is already bound to a different route decision");
    }

    let entry = ConversationRouteDecisionRecordedEntry {
        decision_id: binding.decision_id.clone(),
        source_turn: binding.source_turn.clone(),
        route: ConversationRoute::PlanReview,
        reason_codes,
        configured_policy: TaskRoutingPolicy::Auto,
        effective_capability: AutomaticRouteCapability::ReviewFirst,
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

    let metadata = ToolResultMeta {
        details: json!({
            "decision_id": binding.decision_id.as_str(),
            "plan_review_id": binding.plan_review_id.as_str(),
            "status": "accepted",
        }),
        ..ToolResultMeta::default()
    };
    let result = ToolResult::ok(
        call.id.clone(),
        call.name.clone(),
        "read-only plan review accepted; the conversation coordinator will continue the root run",
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
    Ok(Some(StartPlanReviewAction {
        decision_id: binding.decision_id.clone(),
        plan_review_id: binding.plan_review_id.clone(),
        plan_id: binding.plan_id.clone(),
        source_turn: binding.source_turn.clone(),
    }))
}

pub(super) fn append_tool_ignored_after_plan_review_decision(
    session: &mut Session,
    outcome: &mut AgentRunOutcome,
    call: &ToolCall,
    assistant_batch_results: &mut Vec<(crate::ToolCall, ToolResult)>,
) -> Result<()> {
    let mut result = ToolResult::error(
        call.id.clone(),
        call.name.clone(),
        ToolErrorKind::Unsupported,
        "a typed plan review routing decision was accepted; additional tool calls in this routing microturn were ignored",
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

fn validate_binding_against_session(
    session: &Session,
    binding: &PlanReviewHandoffBinding,
) -> Result<()> {
    if binding.source_turn.session_scope_id != session.session_scope_id() {
        bail!("plan review source belongs to a different session");
    }
    let objective = source_turn_objective(session, &binding.source_turn).ok_or_else(|| {
        anyhow!(
            "plan review source user turn {} is not present",
            binding.source_turn.message_id
        )
    })?;
    if objective != binding.objective {
        bail!("plan review objective does not match the persisted source turn");
    }
    if binding.policy_snapshot_hash.trim().is_empty() {
        bail!("plan review policy snapshot hash is empty");
    }
    if binding.route_contract_fingerprint.trim().is_empty() {
        bail!("plan review route contract fingerprint is empty");
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

fn append_control<H>(session: &mut Session, handler: &mut H, control: ControlEntry) -> Result<()>
where
    H: EventHandler + Send,
{
    session.append_control(control.clone())?;
    handler.handle(RunEvent::Control(control))
}

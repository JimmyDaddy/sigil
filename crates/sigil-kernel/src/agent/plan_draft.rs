use anyhow::Result;
use serde_json::json;

use crate::{
    ControlEntry, EventHandler, PlanReviewDraftContext, Session, ToolCall, ToolErrorKind,
    ToolExecutionStatus, ToolResult, ToolResultMeta,
    plan::{
        PLAN_REVIEW_RESULT_TOOL_NAME, PlanReviewResult, PlanReviewResultOutcome,
        submit_plan_review_result,
    },
};

use super::{
    AgentRunOutcome,
    tool_audit::{append_tool_execution_audit, attach_tool_call_context},
    tool_results::record_tool_result_to_batch,
};

/// Returns the typed outcome of a Plan review result call when the call is valid for this context.
///
/// This preflight is used by the agent loop to stop processing additional calls in the same
/// response. `NoPlan` is accepted as a terminal Plan-review result while remaining distinct from
/// `Draft`; callers must not map it to `DraftReady`.
pub(crate) fn submit_plan_review_result_call_outcome(
    context: &PlanReviewDraftContext,
    call: &ToolCall,
) -> Option<PlanReviewResultOutcome> {
    if call.name != PLAN_REVIEW_RESULT_TOOL_NAME {
        return None;
    }
    submit_plan_review_result(
        &call.args_json,
        context.plan_id.clone(),
        context.source.clone(),
        0,
        context.workspace_snapshot_id.clone(),
    )
    .ok()
    .map(|result| result.outcome())
}

/// Intercepts the typed provider-neutral Plan review result tool.
///
/// Only the `Draft` branch appends `PlanDraftCreated`. The `NoPlan` branch records a successful
/// tool result carrying the safe explanation and deliberately emits no Plan control entry; the
/// coordinator owns the corresponding `CompletedWithoutDraft` attempt transition.
pub(crate) fn handle_submit_plan_review_result_call<H>(
    session: &mut Session,
    handler: &mut H,
    outcome: &mut AgentRunOutcome,
    call: &ToolCall,
    context: &PlanReviewDraftContext,
    created_at_ms: u64,
    assistant_batch_results: &mut Vec<(crate::ToolCall, ToolResult)>,
) -> Result<Option<PlanReviewResultOutcome>>
where
    H: EventHandler + Send,
{
    append_tool_execution_audit(session, call, &[], ToolExecutionStatus::Started, None, None)?;
    let mut parsed_outcome = None;
    let result = match submit_plan_review_result(
        &call.args_json,
        context.plan_id.clone(),
        context.source.clone(),
        created_at_ms,
        context.workspace_snapshot_id.clone(),
    ) {
        Ok(PlanReviewResult::Draft(entry)) => {
            let result_outcome = PlanReviewResultOutcome::Draft;
            parsed_outcome = Some(result_outcome);
            let plan_id = entry.plan_id.as_str().to_owned();
            handler.commit_controls(session, vec![ControlEntry::PlanDraftCreated(*entry)])?;
            let result = ToolResult::ok(
                call.id.clone(),
                call.name.clone(),
                "validated plan review draft recorded; the host will surface the plan for your decision",
                ToolResultMeta {
                    details: json!({
                        "plan_id": plan_id,
                        "outcome": result_outcome.as_str(),
                        "status": "draft_ready",
                    }),
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
            result
        }
        Ok(PlanReviewResult::NoPlan { reason }) => {
            let result_outcome = PlanReviewResultOutcome::NoPlan;
            parsed_outcome = Some(result_outcome);
            let result = ToolResult::ok(
                call.id.clone(),
                call.name.clone(),
                // Tool-result metadata is re-projected by the batch capture layer. Keep the
                // typed no_plan outcome in the bounded content as well so recovery can identify
                // it without inferring semantics from natural-language text.
                json!({
                    "schema_version": crate::PLAN_REVIEW_RESULT_SCHEMA_VERSION,
                    "outcome": result_outcome.as_str(),
                    "content": reason.clone(),
                })
                .to_string(),
                ToolResultMeta {
                    details: json!({
                        "outcome": result_outcome.as_str(),
                        "status": "completed_without_draft",
                        "reason": reason,
                    }),
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
            result
        }
        Err(error) => {
            let result = ToolResult::error(
                call.id.clone(),
                call.name.clone(),
                ToolErrorKind::InvalidInput,
                error.to_string(),
            );
            append_tool_execution_audit(
                session,
                call,
                &[],
                ToolExecutionStatus::Failed,
                None,
                Some(&result),
            )?;
            result
        }
    };
    record_tool_result_to_batch(outcome, call, result, assistant_batch_results);
    Ok(parsed_outcome)
}

pub(super) fn append_tool_ignored_after_plan_draft(
    session: &mut Session,
    outcome: &mut AgentRunOutcome,
    call: &ToolCall,
    assistant_batch_results: &mut Vec<(crate::ToolCall, ToolResult)>,
) -> Result<()> {
    let mut result = ToolResult::error(
        call.id.clone(),
        call.name.clone(),
        ToolErrorKind::Unsupported,
        "plan draft was accepted; additional tool calls in this response were ignored",
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

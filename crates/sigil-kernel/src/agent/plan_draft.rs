use anyhow::Result;
use serde_json::json;

use crate::{
    ControlEntry, EventHandler, PlanReviewDraftContext, RunEvent, SUBMIT_PLAN_DRAFT_TOOL_NAME,
    Session, ToolCall, ToolErrorKind, ToolExecutionStatus, ToolResult, ToolResultMeta,
    submit_plan_draft_entry,
};

use super::{
    AgentRunOutcome,
    tool_audit::{append_tool_execution_audit, attach_tool_call_context},
    tool_results::record_tool_result_to_batch,
};

pub(super) fn submit_plan_draft_call_is_accepted(
    context: &PlanReviewDraftContext,
    call: &ToolCall,
) -> bool {
    if call.name != SUBMIT_PLAN_DRAFT_TOOL_NAME {
        return false;
    }
    submit_plan_draft_entry(
        &call.args_json,
        context.plan_id.clone(),
        context.source.clone(),
        0,
        context.workspace_snapshot_id.clone(),
    )
    .is_ok_and(|entry| entry.is_some())
}

/// Intercepts the typed `submit_plan_draft` tool and records the validated draft.
///
/// The draft is appended to the plan review run session; the shared runtime coordinator commits
/// the draft and attempt status to the parent session. The model never supplies identity,
/// timestamps, or authority.
pub(super) fn handle_submit_plan_draft_call<H>(
    session: &mut Session,
    handler: &mut H,
    outcome: &mut AgentRunOutcome,
    call: &ToolCall,
    context: &PlanReviewDraftContext,
    created_at_ms: u64,
    assistant_batch_results: &mut Vec<(crate::ToolCall, ToolResult)>,
) -> Result<bool>
where
    H: EventHandler + Send,
{
    append_tool_execution_audit(session, call, &[], ToolExecutionStatus::Started, None, None)?;
    let mut accepted = false;
    let result = match submit_plan_draft_entry(
        &call.args_json,
        context.plan_id.clone(),
        context.source.clone(),
        created_at_ms,
        context.workspace_snapshot_id.clone(),
    ) {
        Ok(Some(entry)) => {
            accepted = true;
            let plan_id = entry.plan_id.as_str().to_owned();
            let control = ControlEntry::PlanDraftCreated(entry);
            session.append_control(control.clone())?;
            handler.handle(RunEvent::Control(control))?;
            let result = ToolResult::ok(
                call.id.clone(),
                call.name.clone(),
                "validated plan draft recorded; the host will surface the plan for your decision",
                ToolResultMeta {
                    details: json!({
                        "plan_id": plan_id,
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
        Ok(None) => {
            let result = ToolResult::error(
                call.id.clone(),
                call.name.clone(),
                ToolErrorKind::InvalidInput,
                "submit_plan_draft did not produce a valid executable draft",
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
    Ok(accepted)
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

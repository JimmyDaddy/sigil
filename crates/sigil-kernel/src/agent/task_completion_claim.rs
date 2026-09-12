use anyhow::Result;

use crate::{
    AgentRunOutcome, EventHandler, TaskCompletionClaimSubjectV1, TaskCompletionClaimV1, ToolCall,
    ToolErrorKind, ToolResult, ToolResultMeta,
    session::{Session, ToolExecutionStatus},
    task::{task_completion_claim_from_call, task_completion_claim_result_content},
};

use super::tool_audit::{append_tool_execution_audit, attach_tool_call_context};
use super::tool_results::record_tool_result_to_batch;

/// Handles one model-reported completion claim and keeps it in the current run's result slot.
pub(super) fn handle_task_completion_claim_call<H>(
    session: &mut Session,
    handler: &mut H,
    outcome: &mut AgentRunOutcome,
    call: &ToolCall,
    expected_subject: &TaskCompletionClaimSubjectV1,
    expected_attempt_id: &str,
    claim_slot: &mut Option<TaskCompletionClaimV1>,
    assistant_batch_results: &mut Vec<(ToolCall, ToolResult)>,
) -> Result<()>
where
    H: EventHandler + Send,
{
    append_tool_execution_audit(session, call, &[], ToolExecutionStatus::Started, None, None)?;
    let result = match task_completion_claim_from_call(call, expected_subject, expected_attempt_id)
    {
        Ok(_claim) if claim_slot.is_some() => ToolResult::error(
            call.id.clone(),
            call.name.clone(),
            ToolErrorKind::InvalidInput,
            "only one task completion claim may be submitted per run",
        ),
        Ok(claim) => {
            *claim_slot = Some(claim.clone());
            let result = ToolResult::ok(
                call.id.clone(),
                call.name.clone(),
                task_completion_claim_result_content(&claim),
                ToolResultMeta::default(),
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
        Err(error) => ToolResult::error(
            call.id.clone(),
            call.name.clone(),
            ToolErrorKind::InvalidInput,
            error.to_string(),
        ),
    };
    if result.is_error() {
        let mut result = result;
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
    } else {
        record_tool_result_to_batch(outcome, call, result, assistant_batch_results);
    }
    let _ = handler;
    Ok(())
}

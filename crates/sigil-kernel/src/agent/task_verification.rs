use anyhow::Result;

use crate::{
    AgentRunOptions, DirectTaskVerificationContext, EventHandler, RunCancellationHandle, ToolCall,
    ToolErrorKind, ToolResult, ToolResultMeta,
    session::{Session, ToolExecutionStatus},
};

use super::{
    AgentRunOutcome,
    tool_audit::{append_tool_execution_audit, attach_tool_call_context},
    tool_results::record_tool_result_to_batch,
};

#[allow(clippy::too_many_arguments)]
pub(super) async fn handle_verification_feedback_call<H: EventHandler + Send>(
    session: &mut Session,
    handler: &mut H,
    outcome: &mut AgentRunOutcome,
    call: &ToolCall,
    context: Option<&mut DirectTaskVerificationContext>,
    options: &AgentRunOptions,
    cancellation: Option<&RunCancellationHandle>,
    results: &mut Vec<(ToolCall, ToolResult)>,
) -> Result<()> {
    append_tool_execution_audit(session, call, &[], ToolExecutionStatus::Started, None, None)?;
    let selected = match context {
        Some(context) => {
            context
                .select(session, options, handler, call, cancellation)
                .await
        }
        None => Err(anyhow::anyhow!(
            "verification feedback is unavailable for this run"
        )),
    };
    let mut result = match selected {
        Ok(()) => ToolResult::ok(
            call.id.clone(),
            call.name.clone(),
            "verification response recorded; existing tool permissions and real verification still apply",
            ToolResultMeta::default(),
        ),
        Err(error) => ToolResult::error(
            call.id.clone(),
            call.name.clone(),
            ToolErrorKind::InvalidInput,
            error.to_string(),
        ),
    };
    attach_tool_call_context(&mut result, call, &[]);
    append_tool_execution_audit(
        session,
        call,
        &[],
        if result.is_error() {
            ToolExecutionStatus::Failed
        } else {
            ToolExecutionStatus::Completed
        },
        None,
        Some(&result),
    )?;
    record_tool_result_to_batch(outcome, call, result, results);
    Ok(())
}

pub(super) fn reject_before_verification_choice(
    session: &mut Session,
    outcome: &mut AgentRunOutcome,
    call: &ToolCall,
    results: &mut Vec<(ToolCall, ToolResult)>,
) -> Result<()> {
    let mut result = ToolResult::error(
        call.id.clone(),
        call.name.clone(),
        ToolErrorKind::InvalidInput,
        "respond to the current failed checks before running repair tools; put respond_to_task_verification repair first in the same batch, or request_user_input; no additional permission is granted",
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
    record_tool_result_to_batch(outcome, call, result, results);
    Ok(())
}

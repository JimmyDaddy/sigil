use anyhow::Result;

use crate::{
    ControlEntry, EventHandler, RunEvent, Session, TASK_GUIDANCE_APPLY_TOOL_NAME,
    TaskGuidanceAssessmentContext, ToolCall, ToolErrorKind, ToolExecutionStatus, ToolResult,
    ToolResultMeta, task_guidance_applied_entry, task_guidance_apply_result_content,
};

use super::{
    AgentRunOutcome,
    tool_audit::{append_tool_execution_audit, attach_tool_call_context},
    tool_results::record_tool_result_to_batch,
};

pub(super) fn task_guidance_apply_call_is_accepted(
    context: &TaskGuidanceAssessmentContext,
    call: &ToolCall,
) -> bool {
    call.name == TASK_GUIDANCE_APPLY_TOOL_NAME && task_guidance_applied_entry(context, call).is_ok()
}

pub(super) fn handle_task_guidance_apply_call<H>(
    session: &mut Session,
    handler: &mut H,
    outcome: &mut AgentRunOutcome,
    call: &ToolCall,
    context: &TaskGuidanceAssessmentContext,
    assistant_batch_results: &mut Vec<(crate::ToolCall, ToolResult)>,
) -> Result<bool>
where
    H: EventHandler + Send,
{
    append_tool_execution_audit(session, call, &[], ToolExecutionStatus::Started, None, None)?;
    let mut accepted = false;
    let result = match task_guidance_applied_entry(context, call) {
        Ok(entry) => {
            accepted = true;
            let decision = ControlEntry::TaskGuidanceApplied(entry.clone());
            session.append_control(decision.clone())?;
            handler.handle(RunEvent::Control(decision))?;
            let plan = ControlEntry::TaskPlan(context.accepted_plan.clone());
            session.append_control(plan.clone())?;
            handler.handle(RunEvent::Control(plan))?;
            let result = ToolResult::ok(
                call.id.clone(),
                call.name.clone(),
                task_guidance_apply_result_content(&entry),
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

pub(super) fn append_tool_ignored_after_task_guidance_acceptance(
    session: &mut Session,
    outcome: &mut AgentRunOutcome,
    call: &ToolCall,
    assistant_batch_results: &mut Vec<(crate::ToolCall, ToolResult)>,
) -> Result<()> {
    let mut result = ToolResult::error(
        call.id.clone(),
        call.name.clone(),
        ToolErrorKind::Unsupported,
        "task guidance was accepted for pending steps; additional planner tool calls are ignored",
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

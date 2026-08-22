use anyhow::Result;

use crate::{
    EventHandler, RunEvent, TaskChecklistUpdateContextV1, ToolCall, ToolErrorKind, ToolResult,
    ToolResultMeta,
    session::{ControlEntry, Session, ToolExecutionStatus},
    task_checklist_update_entry,
};

use super::{
    AgentRunOutcome,
    tool_audit::{append_tool_execution_audit, attach_tool_call_context},
    tool_results::record_tool_result_to_batch,
};

pub(super) fn handle_task_checklist_update_call<H>(
    session: &mut Session,
    handler: &mut H,
    outcome: &mut AgentRunOutcome,
    call: &ToolCall,
    context: &mut TaskChecklistUpdateContextV1,
    assistant_batch_results: &mut Vec<(ToolCall, ToolResult)>,
) -> Result<()>
where
    H: EventHandler + Send,
{
    append_tool_execution_audit(session, call, &[], ToolExecutionStatus::Started, None, None)?;
    let result = match task_checklist_update_entry(context, call) {
        Ok(entry) => {
            context.current_revision = entry.revision;
            session.append_control(ControlEntry::TaskChecklistUpdatedV1(entry.clone()))?;
            handler.handle(RunEvent::Control(ControlEntry::TaskChecklistUpdatedV1(
                entry.clone(),
            )))?;
            let result = ToolResult::ok(
                call.id.clone(),
                call.name.clone(),
                format!("task checklist updated to revision {}", entry.revision),
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
            result
        }
    };
    record_tool_result_to_batch(outcome, call, result, assistant_batch_results);
    Ok(())
}

use anyhow::Result;

use crate::{
    event::{EventHandler, RunEvent},
    provider::ToolCall,
    session::{ControlEntry, Session, ToolExecutionStatus},
    task::{
        TASK_PLAN_UPDATE_TOOL_NAME, TaskPlanStatus, TaskPlanUpdateContext,
        task_plan_update_commit_v2, task_plan_update_entry, task_plan_update_result_content,
    },
    tool::{ToolErrorKind, ToolResult, ToolResultMeta},
};

use super::{
    AgentRunOutcome,
    tool_audit::{append_tool_execution_audit, attach_tool_call_context},
    tool_results::record_tool_result_to_batch,
};

pub(super) fn task_plan_update_call_is_accepted(
    context: &TaskPlanUpdateContext,
    call: &ToolCall,
) -> bool {
    if call.name != TASK_PLAN_UPDATE_TOOL_NAME {
        return false;
    }
    task_plan_update_entry(context, call)
        .map(|entry| entry.status == TaskPlanStatus::Accepted)
        .unwrap_or(false)
}

pub(super) fn handle_task_plan_update_call<H>(
    session: &mut Session,
    handler: &mut H,
    outcome: &mut AgentRunOutcome,
    call: &ToolCall,
    context: &TaskPlanUpdateContext,
    assistant_batch_results: &mut Vec<(crate::ToolCall, ToolResult)>,
) -> Result<bool>
where
    H: EventHandler + Send,
{
    append_tool_execution_audit(session, call, &[], ToolExecutionStatus::Started, None, None)?;
    let mut accepted = false;
    let result = match task_plan_update_commit_v2(context, call) {
        Ok(commit) => {
            let entry = commit.plan;
            accepted = entry.status == TaskPlanStatus::Accepted;
            let mut controls = Vec::with_capacity(commit.step_contracts.len().saturating_add(1));
            let contract_set_commit =
                crate::TaskPlanContractSetCommittedV2::new(&entry, &commit.step_contracts)?;
            controls.push(ControlEntry::TaskPlan(entry.clone()));
            controls.extend(
                commit
                    .step_contracts
                    .into_iter()
                    .map(ControlEntry::TaskStepContractBoundV2),
            );
            controls.push(ControlEntry::TaskPlanContractSetCommittedV2(
                contract_set_commit,
            ));
            session.append_controls(controls.clone())?;
            for control in controls {
                handler.handle(RunEvent::Control(control))?;
            }
            let result = ToolResult::ok(
                call.id.clone(),
                call.name.clone(),
                task_plan_update_result_content(&entry),
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

pub(super) fn append_tool_ignored_after_task_plan_acceptance(
    session: &mut Session,
    outcome: &mut AgentRunOutcome,
    call: &ToolCall,
    assistant_batch_results: &mut Vec<(crate::ToolCall, ToolResult)>,
) -> Result<()> {
    let mut result = ToolResult::error(
        call.id.clone(),
        call.name.clone(),
        ToolErrorKind::Unsupported,
        "task plan was accepted; additional planner tool calls are ignored and orchestration will continue",
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

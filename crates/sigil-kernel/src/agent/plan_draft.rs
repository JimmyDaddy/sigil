use anyhow::Result;
use serde::Deserialize;
use serde_json::json;

use crate::{
    CONFIRM_PLAN_REVIEW_CANDIDATE_TOOL_NAME, ControlEntry, EventHandler, PlanReviewDraftContext,
    SUBMIT_PLAN_DRAFT_TOOL_NAME, Session, ToolCall, ToolErrorKind, ToolExecutionStatus, ToolResult,
    ToolResultMeta,
    plan::{
        PLAN_REVIEW_RESULT_TOOL_NAME, PlanReviewResult, PlanReviewResultOutcome,
        submit_plan_review_result,
    },
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
    if call.name != SUBMIT_PLAN_DRAFT_TOOL_NAME || context.candidate_content.is_some() {
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
    if context.candidate_content.is_some() {
        let result = ToolResult::error(
            call.id.clone(),
            call.name.clone(),
            ToolErrorKind::Protocol,
            "a complete host-preserved candidate must be confirmed with confirm_plan_review_candidate",
        );
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
            handler.commit_controls(session, vec![control])?;
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

/// Returns the typed outcome of a Plan review result call when the call is valid for this context.
///
/// This preflight is used by the agent loop to stop processing additional calls in the same
/// response. `NoPlan` is accepted as a terminal Plan-review result while remaining distinct from
/// `Draft`; callers must not map it to `DraftReady`.
pub(crate) fn submit_plan_review_result_call_outcome(
    context: &PlanReviewDraftContext,
    call: &ToolCall,
) -> Option<PlanReviewResultOutcome> {
    if call.name != PLAN_REVIEW_RESULT_TOOL_NAME || context.candidate_content.is_some() {
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

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
struct ConfirmPlanReviewCandidateArgs {
    decision: ConfirmPlanReviewCandidateDecision,
}

#[derive(Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum ConfirmPlanReviewCandidateDecision {
    Accept,
}

/// Returns true only for the host-bound, body-free candidate confirmation call.
pub(crate) fn confirm_plan_review_candidate_call_is_accepted(
    context: &PlanReviewDraftContext,
    call: &ToolCall,
) -> bool {
    if call.name != CONFIRM_PLAN_REVIEW_CANDIDATE_TOOL_NAME
        || context
            .candidate_content
            .as_deref()
            .is_none_or(|content| content.trim().is_empty())
    {
        return false;
    }
    serde_json::from_str::<ConfirmPlanReviewCandidateArgs>(&call.args_json)
        .is_ok_and(|args| args.decision == ConfirmPlanReviewCandidateDecision::Accept)
}

/// Intercepts a body-free confirmation of the exact candidate previously preserved by the host.
/// The synthetic envelope is constructed from the bound context and therefore cannot change the
/// candidate's durable text or hash.
pub(crate) fn handle_confirm_plan_review_candidate_call<H>(
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
    let result = match serde_json::from_str::<ConfirmPlanReviewCandidateArgs>(&call.args_json) {
        Ok(args) if args.decision == ConfirmPlanReviewCandidateDecision::Accept => {
            let Some(content) = context.candidate_content.as_deref() else {
                let result = ToolResult::error(
                    call.id.clone(),
                    call.name.clone(),
                    ToolErrorKind::Unsupported,
                    "confirm_plan_review_candidate is not available without a complete host-preserved candidate",
                );
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
            };
            let envelope = json!({
                "schema_version": crate::PLAN_REVIEW_RESULT_SCHEMA_VERSION,
                "outcome": PlanReviewResultOutcome::Draft,
                "content": content,
            });
            match submit_plan_review_result(
                &envelope.to_string(),
                context.plan_id.clone(),
                context.source.clone(),
                created_at_ms,
                context.workspace_snapshot_id.clone(),
            ) {
                Ok(PlanReviewResult::Draft(entry)) => {
                    parsed_outcome = Some(PlanReviewResultOutcome::Draft);
                    let plan_id = entry.plan_id.as_str().to_owned();
                    handler
                        .commit_controls(session, vec![ControlEntry::PlanDraftCreated(*entry)])?;
                    let result = ToolResult::ok(
                        call.id.clone(),
                        call.name.clone(),
                        "host-preserved Plan candidate confirmed; the host will surface the plan for your decision",
                        ToolResultMeta {
                            details: json!({
                                "plan_id": plan_id,
                                "outcome": PlanReviewResultOutcome::Draft.as_str(),
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
                Ok(PlanReviewResult::NoPlan { .. }) | Err(_) => {
                    let result = ToolResult::error(
                        call.id.clone(),
                        call.name.clone(),
                        ToolErrorKind::InvalidInput,
                        "the host-preserved Plan candidate could not be validated",
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
            }
        }
        Ok(_) | Err(_) => {
            let result = ToolResult::error(
                call.id.clone(),
                call.name.clone(),
                ToolErrorKind::InvalidInput,
                "confirm_plan_review_candidate requires decision accept and no other fields",
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
    if context.candidate_content.is_some() {
        let result = ToolResult::error(
            call.id.clone(),
            call.name.clone(),
            ToolErrorKind::Protocol,
            "a complete host-preserved candidate must be confirmed with confirm_plan_review_candidate",
        );
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

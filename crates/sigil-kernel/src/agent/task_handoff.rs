use anyhow::{Result, anyhow, bail};
use serde_json::json;

use crate::{
    CONTINUE_EXISTING_TASK_TOOL_NAME, ContinueDurableTaskAction, ControlEntry, EventHandler,
    PlanReviewAttemptStatus, PlanReviewHandoffBinding, RUN_PENDING_PLAN_TOOL_NAME,
    RunPendingPlanAction, START_TASK_TOOL_NAME, Session, SessionLogEntry, StartDurableTaskAction,
    TaskAdmissionTrigger, TaskContinuationHandoffBinding, TaskContinuationSelectedEntry,
    TaskDirectExecutionAdmittedV1, TaskDirectExecutionSourceV1, TaskHandoffDecision,
    TaskHandoffRequestedEntry, TaskHandoffResolvedEntry, TaskRunCancellationScopeBoundEntry,
    TaskRunEntry, TaskRunStatus, TaskStartHandoffBinding, ToolCall, ToolErrorKind,
    ToolExecutionStatus, ToolResult, ToolResultMeta, start_task_title,
    validate_continue_existing_task_call, validate_run_pending_plan_call,
};

use super::{
    AgentRunOutcome,
    tool_audit::{append_tool_execution_audit, attach_tool_call_context},
    tool_results::record_tool_result_to_batch,
};

pub(super) fn start_task_call_is_accepted(call: &ToolCall) -> bool {
    call.name == START_TASK_TOOL_NAME && start_task_title(call).is_ok()
}

pub(super) fn continue_existing_task_call_is_accepted(call: &ToolCall) -> bool {
    call.name == CONTINUE_EXISTING_TASK_TOOL_NAME
        && validate_continue_existing_task_call(call).is_ok()
}

pub(super) fn run_pending_plan_call_is_accepted(call: &ToolCall) -> bool {
    call.name == RUN_PENDING_PLAN_TOOL_NAME && validate_run_pending_plan_call(call).is_ok()
}

pub(super) fn handle_run_pending_plan_call(
    session: &mut Session,
    outcome: &mut AgentRunOutcome,
    call: &ToolCall,
    binding: &PlanReviewHandoffBinding,
    assistant_batch_results: &mut Vec<(crate::ToolCall, ToolResult)>,
) -> Result<Option<RunPendingPlanAction>> {
    append_tool_execution_audit(session, call, &[], ToolExecutionStatus::Started, None, None)?;
    if let Err(error) = validate_run_pending_plan_call(call) {
        return reject_pending_plan_decision(
            session,
            outcome,
            call,
            assistant_batch_results,
            &error.to_string(),
        );
    }
    let pending = validate_pending_plan_binding(session, binding)?;
    let result = ToolResult::ok(
        call.id.clone(),
        call.name.clone(),
        "pending plan execution accepted; the host will create and run a durable Task",
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
    Ok(Some(RunPendingPlanAction {
        plan_id: pending.plan_id.clone(),
        plan_hash: pending.plan_hash.clone(),
        source_turn: binding.source_turn.clone(),
    }))
}

fn reject_pending_plan_decision<T>(
    session: &mut Session,
    outcome: &mut AgentRunOutcome,
    call: &ToolCall,
    assistant_batch_results: &mut Vec<(crate::ToolCall, ToolResult)>,
    message: &str,
) -> Result<Option<T>> {
    let mut result = ToolResult::error(
        call.id.clone(),
        call.name.clone(),
        ToolErrorKind::InvalidInput,
        message,
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
    Ok(None)
}

fn validate_pending_plan_binding<'a>(
    session: &Session,
    binding: &'a PlanReviewHandoffBinding,
) -> Result<&'a crate::PendingPlanHandoffBinding> {
    binding.validate_shape()?;
    if binding.source_turn.session_scope_id != session.session_scope_id() {
        bail!("pending plan decision belongs to another session");
    }
    if session
        .source_user_message(&binding.source_turn.message_id)
        .is_none()
    {
        bail!("pending plan decision source turn is no longer present");
    }
    let pending = binding
        .pending_plan
        .as_ref()
        .ok_or_else(|| anyhow!("no pending plan was bound before provider dispatch"))?;
    let artifacts = session.plan_artifact_projection();
    let latest = artifacts
        .latest_pending_plan()
        .ok_or_else(|| anyhow!("the bound pending plan is no longer actionable"))?;
    if latest.plan_id != pending.plan_id || latest.plan_hash != pending.plan_hash {
        bail!("pending plan changed after routing was frozen");
    }
    let reviews = crate::PlanReviewProjection::from_entries(session.entries());
    if !reviews.conflicts.is_empty() {
        bail!("pending plan review projection contains conflicting durable facts");
    }
    let attempt = reviews
        .attempt_for_plan(&pending.plan_id)
        .ok_or_else(|| anyhow!("pending plan is not bound to a review attempt"))?;
    if attempt.status != PlanReviewAttemptStatus::DraftReady {
        bail!("pending plan review is not draft-ready");
    }
    // RFC-0067 6.1: DraftReady means the exact candidate and ready marker are durable; a legacy
    // or crash-incomplete draft is not runnable through the model route.
    let artifacts = crate::PlanArtifactProjection::from_entries(session.entries());
    if !artifacts.plan_is_ready(&pending.plan_id) {
        bail!("pending plan review is not executable: no ready candidate");
    }
    Ok(pending)
}

pub(super) fn handle_continue_existing_task_call<H>(
    session: &mut Session,
    handler: &mut H,
    outcome: &mut AgentRunOutcome,
    call: &ToolCall,
    binding: &TaskContinuationHandoffBinding,
    _run_scope_id: Option<&str>,
    assistant_batch_results: &mut Vec<(crate::ToolCall, ToolResult)>,
) -> Result<Option<ContinueDurableTaskAction>>
where
    H: EventHandler + Send,
{
    append_tool_execution_audit(session, call, &[], ToolExecutionStatus::Started, None, None)?;
    if let Err(error) = validate_continue_existing_task_call(call) {
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

    validate_task_continuation_binding(session, binding)?;
    let guidance_projection =
        crate::project_conversation_prompt_for_persistence(binding.exact_guidance.expose_secret());
    if guidance_projection.prompt_hash != binding.prompt_hash
        || guidance_projection.safe_prompt != binding.safe_guidance
        || guidance_projection.exact_prompt_required != binding.exact_prompt_required
    {
        bail!("task continuation exact guidance drifted from its host binding");
    }
    let continuation_kind = crate::continue_existing_task_control_kind(call)?;
    let is_resume = continuation_kind == crate::TaskContinuationControlKind::ResumeTask;
    let action_guidance = binding.exact_guidance.expose_secret().to_owned();
    let selected = TaskContinuationSelectedEntry {
        task_id: binding.task_id.clone(),
        source_turn: binding.source_turn.clone(),
        task_status: binding.task_status,
        route_contract_fingerprint: binding.route_contract_fingerprint.clone(),
        control: continuation_kind,
        prompt_hash: binding.prompt_hash.clone(),
        exact_prompt_required: binding.exact_prompt_required,
        guidance: binding.safe_guidance.clone(),
        selected_at_ms: binding.decided_at_ms,
    };
    if selected.control != continuation_kind {
        return reject_task_continuation_recovery(
            session,
            outcome,
            call,
            assistant_batch_results,
            "Task continuation action conflicts with the unfinished durable selection",
        );
    }
    let existing = session.entries().iter().find_map(|entry| match entry {
        SessionLogEntry::Control(ControlEntry::TaskContinuationSelected(entry))
            if entry.source_turn == selected.source_turn =>
        {
            Some(entry)
        }
        _ => None,
    });
    let selection_missing = match existing {
        Some(previous) if previous == &selected => false,
        Some(_) => bail!("source turn is already bound to a different Task continuation"),
        None => true,
    };
    let route_decision = task_continuation_route_decision_control(session, binding)?;
    let mut continuation_controls = Vec::with_capacity(4);
    if let Some(route_decision) = route_decision {
        continuation_controls.push(ControlEntry::ConversationRouteDecisionRecorded(
            route_decision,
        ));
    }
    if selection_missing {
        continuation_controls.push(ControlEntry::TaskContinuationSelected(selected.clone()));
    }
    if !continuation_controls.is_empty() {
        append_control_batch(session, handler, continuation_controls)?;
    }

    let result = ToolResult::ok(
        call.id.clone(),
        call.name.clone(),
        "exact durable Task continuation accepted; the conversation coordinator will resume it",
        ToolResultMeta {
            details: json!({
                "task_id": binding.task_id.as_str(),
                "task_status": binding.task_status,
                "status": "accepted",
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
    record_tool_result_to_batch(outcome, call, result, assistant_batch_results);
    Ok(Some(ContinueDurableTaskAction {
        task_id: selected.task_id.clone(),
        source_turn: selected.source_turn.clone(),
        task_status: selected.task_status,
        route_contract_fingerprint: selected.route_contract_fingerprint.clone(),
        control: if is_resume {
            crate::TaskContinuationControl::ResumeTask
        } else {
            crate::TaskContinuationControl::ApplyTaskGuidance(action_guidance.clone())
        },
        guidance: crate::SecretString::new(action_guidance),
        guidance_receipt: selected,
    }))
}

fn reject_task_continuation_recovery(
    session: &mut Session,
    outcome: &mut AgentRunOutcome,
    call: &ToolCall,
    assistant_batch_results: &mut Vec<(crate::ToolCall, ToolResult)>,
    message: &str,
) -> Result<Option<ContinueDurableTaskAction>> {
    let mut result = ToolResult::error(
        call.id.clone(),
        call.name.clone(),
        ToolErrorKind::InvalidInput,
        message,
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
    Ok(None)
}

pub(super) fn handle_start_task_call<H>(
    session: &mut Session,
    handler: &mut H,
    outcome: &mut AgentRunOutcome,
    call: &ToolCall,
    binding: &TaskStartHandoffBinding,
    run_scope_id: &str,
    assistant_batch_results: &mut Vec<(crate::ToolCall, ToolResult)>,
) -> Result<Option<StartDurableTaskAction>>
where
    H: EventHandler + Send,
{
    append_tool_execution_audit(session, call, &[], ToolExecutionStatus::Started, None, None)?;
    let title = match start_task_title(call) {
        Ok(title) => title,
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
                title: title.clone(),
                recovery_objective: None,
                policy_snapshot_hash: binding.policy_snapshot_hash.clone(),
                requested_at_ms: binding.requested_at_ms,
            }),
        )?,
    }

    let task_title = existing
        .and_then(|state| state.request.as_ref())
        .and_then(|request| request.title.clone())
        .or(title);

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

    ensure_task_started(session, handler, binding, task_title.as_deref())?;

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
        "direct Task started for the current user request",
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
    binding: &TaskStartHandoffBinding,
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

fn task_continuation_route_decision_control(
    session: &Session,
    binding: &TaskContinuationHandoffBinding,
) -> Result<Option<crate::ConversationRouteDecisionRecordedEntry>> {
    use crate::conversation_route::{
        ConversationRoute, ConversationRouteDecisionProjection,
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
        reason_codes: Vec::new(),
        configured_policy: crate::TaskRoutingPolicy::Auto,
        effective_capability: binding.effective_capability,
        policy_snapshot_hash: binding.policy_snapshot_hash.clone(),
        route_contract_fingerprint: binding.route_contract_fingerprint.clone(),
        decided_at_ms: binding.decided_at_ms,
    };
    match projection.decision(&entry.decision_id) {
        None => Ok(Some(entry)),
        Some(previous) if previous == &entry => Ok(None),
        Some(_) => bail!(
            "route decision {} has conflicting durable facts",
            entry.decision_id.as_str()
        ),
    }
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

fn validate_binding_against_session(
    session: &Session,
    binding: &TaskStartHandoffBinding,
) -> Result<()> {
    if binding.source_turn.session_scope_id != session.session_scope_id() {
        bail!("task handoff source belongs to a different session");
    }
    let objective = session
        .source_user_message(&binding.source_turn.message_id)
        .map(|message| message.content.clone().unwrap_or_default())
        .ok_or_else(|| {
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

fn validate_task_continuation_binding(
    session: &Session,
    binding: &TaskContinuationHandoffBinding,
) -> Result<crate::TaskExecutionBindingV1> {
    if binding.source_turn.session_scope_id != session.session_scope_id() {
        bail!("task continuation source belongs to a different session");
    }
    if session
        .source_user_message(&binding.source_turn.message_id)
        .is_none()
    {
        bail!(
            "task continuation source user turn {} is not present",
            binding.source_turn.message_id
        );
    }
    if binding.policy_snapshot_hash.trim().is_empty()
        || binding.route_contract_fingerprint.trim().is_empty()
    {
        bail!("task continuation route binding is incomplete");
    }
    if !matches!(
        binding.task_status,
        TaskRunStatus::Started
            | TaskRunStatus::Paused
            | TaskRunStatus::Failed
            | TaskRunStatus::Interrupted
    ) {
        bail!("task continuation binding is not resumable");
    }
    let projection = session.task_state_projection();
    let task = projection
        .tasks
        .get(&binding.task_id)
        .ok_or_else(|| anyhow!("task continuation target is no longer present"))?;
    if task.status != binding.task_status || task.latest_plan_version.is_some() {
        bail!("task continuation target changed after routing was frozen");
    }
    let admission = task
        .direct_execution_admission
        .as_ref()
        .ok_or_else(|| anyhow!("task continuation has no execution authority"))?;
    admission.validate()?;
    if !admission.matches_objective(&task.objective) {
        bail!("direct execution admission does not match the durable Task objective");
    }
    Ok(crate::TaskExecutionBindingV1::Direct {
        admission_id: admission.admission_id.clone(),
    })
}

fn ensure_task_started<H>(
    session: &mut Session,
    handler: &mut H,
    binding: &TaskStartHandoffBinding,
    suggested_title: Option<&str>,
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
        let admission = task
            .direct_execution_admission
            .as_ref()
            .ok_or_else(|| anyhow!("task handoff target has no direct execution admission"))?;
        admission.validate()?;
        if !admission.matches_objective(&task.objective)
            || admission.source != TaskDirectExecutionSourceV1::TaskRequest
        {
            bail!("task handoff target has conflicting direct execution authority");
        }
        return Ok(());
    }
    append_control_batch(
        session,
        handler,
        vec![
            ControlEntry::TaskRun(TaskRunEntry {
                task_id: binding.task_id.clone(),
                parent_session_ref: binding.parent_session_ref.clone(),
                objective: binding.objective.clone(),
                title: Some(suggested_title.map_or_else(
                    || crate::task_semantic_title(&binding.objective),
                    str::to_owned,
                )),
                status: TaskRunStatus::Started,
                reason: Some("admitted from conversation handoff".to_owned()),
            }),
            ControlEntry::TaskDirectExecutionAdmittedV1(
                TaskDirectExecutionAdmittedV1::task_request(
                    binding.task_id.clone(),
                    &binding.objective,
                    binding.decided_at_ms,
                ),
            ),
        ],
    )
}

fn append_control<H>(session: &mut Session, handler: &mut H, control: ControlEntry) -> Result<()>
where
    H: EventHandler + Send,
{
    handler.commit_controls(session, vec![control]).map(|_| ())
}

fn append_control_batch<H>(
    session: &mut Session,
    handler: &mut H,
    controls: Vec<ControlEntry>,
) -> Result<()>
where
    H: EventHandler + Send,
{
    handler.commit_controls(session, controls).map(|_| ())
}

#[cfg(test)]
#[path = "tests/task_handoff_tests.rs"]
mod tests;

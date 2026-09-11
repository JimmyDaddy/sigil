use super::*;

pub(super) struct InitialTaskGuidance {
    pub selection: crate::TaskContinuationSelectedEntry,
    pub guidance: String,
    pub source_index: usize,
}

pub(super) fn accept_initial_continuation_guidance<H: EventHandler + Send>(
    session: &mut Session,
    request: &SequentialTaskRequest,
    guidance: &str,
    source: &crate::ConversationTurnRef,
    handler: &mut H,
) -> Result<()> {
    let projection = session.task_state_projection();
    let task = projection
        .tasks
        .get(&request.task_id)
        .context("initial Task guidance requires a durable task")?;
    if task.parent_session_ref != request.parent_session_ref
        || task.objective != crate::safe_persistence_text(&request.objective)
        || task.latest_plan_version.is_some()
        || task.direct_execution_admission.is_some()
        || matches!(
            task.status,
            TaskRunStatus::Completed | TaskRunStatus::Cancelled
        )
    {
        bail!("initial Task guidance does not match the unplanned task generation");
    }
    accept_task_continuation_guidance(
        session,
        &request.task_id,
        task.status,
        guidance,
        source,
        handler,
    )
}

/// The source is checked at its original generation, before later planning changes Task state.
#[cfg(test)]
pub(super) fn initial_task_guidance(
    session: &Session,
    task_id: &TaskId,
    exact_guidance: Option<&str>,
) -> Result<Option<InitialTaskGuidance>> {
    Ok(initial_task_guidances(session, task_id, exact_guidance)?.pop())
}

pub(super) fn initial_task_guidances(
    session: &Session,
    task_id: &TaskId,
    exact_guidance: Option<&str>,
) -> Result<Vec<InitialTaskGuidance>> {
    let mut projection = crate::TaskStateProjection::default();
    let mut selected = Vec::<InitialTaskGuidance>::new();
    for (source_index, entry) in session.entries().iter().enumerate() {
        let SessionLogEntry::Control(control) = entry else {
            continue;
        };
        if let ControlEntry::TaskContinuationSelected(selection) = control
            && &selection.task_id == task_id
            && selection.plan_version.is_none()
            && selection.control
                == crate::TaskContinuationControlKind::ApplyCurrentRequestAsGuidance
            && !selection.guidance.trim().is_empty()
        {
            selection.validate_for_session(session.session_scope_id())?;
            let task = projection
                .tasks
                .get(task_id)
                .context("initial Task guidance has no source task")?;
            if task.direct_execution_admission.is_some() {
                projection.apply_control_entry(control);
                continue;
            }
            if task.latest_plan_version.is_some() || task.status != selection.task_status {
                bail!("initial Task guidance does not match its source generation");
            }
            validate_initial_guidance_source(session, selection, source_index)?;
            if let Some(existing) = selected
                .iter()
                .find(|existing| existing.selection.source_turn == selection.source_turn)
            {
                if existing.selection != *selection {
                    bail!("initial Task guidance changed an already accepted source");
                }
            } else {
                selected.push(InitialTaskGuidance {
                    selection: selection.clone(),
                    guidance: selection.guidance.clone(),
                    source_index,
                });
            }
        }
        projection.apply_control_entry(control);
    }
    // Only the current source has process-local exact text; earlier sources retain their safe
    // durable material. Replaying an earlier source never makes it newer than later user input.
    if let Some(exact) = exact_guidance {
        let projected = crate::project_conversation_prompt_for_persistence(exact);
        if let Some(selected) = selected
            .iter_mut()
            .rev()
            .find(|entry| entry.selection.guidance == projected.safe_prompt)
        {
            selected.guidance = recover_guidance_review_text(
                &selected.selection.prompt_hash,
                &selected.selection.guidance,
                selected.selection.exact_prompt_required,
                Some(exact),
            )?;
        } else if !selected.is_empty() {
            bail!("explicit guidance conflicts with accepted initial Task guidance");
        }
    }
    Ok(selected)
}

pub(super) fn initial_planner_guidance_for_attempt(
    session: &Session,
    guidances: &[InitialTaskGuidance],
    attempt_id: &TaskParticipantAttemptId,
) -> Option<String> {
    let frontier = planner_attempt_input_frontier(session, attempt_id);
    let values = guidances
        .iter()
        .filter(|guidance| guidance.source_index < frontier)
        .map(|guidance| guidance.guidance.as_str())
        .collect::<Vec<_>>();
    (!values.is_empty()).then(|| values.join("\n\n"))
}

pub(super) fn initial_guidance_review_context(
    session: &Session,
    selection: &crate::TaskContinuationSelectedEntry,
    exact_guidance: &str,
) -> Result<Option<String>> {
    if selection.plan_version.is_some() {
        return Ok(None);
    }
    let pending = initial_task_guidances(session, &selection.task_id, Some(exact_guidance))?
        .into_iter()
        .filter(|guidance| !initial_guidance_consumed_by_plan(session, guidance))
        .collect::<Vec<_>>();
    if pending.len() <= 1 {
        return Ok(None);
    }
    if pending
        .last()
        .is_none_or(|guidance| guidance.selection != *selection)
    {
        bail!("initial Task guidance review must use the latest accepted source");
    }
    Ok(Some(
        pending
            .iter()
            .map(|guidance| guidance.guidance.as_str())
            .collect::<Vec<_>>()
            .join("\n\n"),
    ))
}

fn validate_initial_guidance_source(
    session: &Session,
    selection: &crate::TaskContinuationSelectedEntry,
    source_index: usize,
) -> Result<()> {
    let source = session.entries()[..source_index]
        .iter()
        .find_map(|entry| match entry {
            SessionLogEntry::User(message) if message.id == selection.source_turn.message_id => {
                Some(message)
            }
            _ => None,
        })
        .context("initial Task guidance is missing its durable source message")?;
    if source.content.as_deref() != Some(selection.guidance.as_str())
        || source.role != crate::MessageRole::User
        || source.assistant_kind.is_some()
        || !source.tool_calls.is_empty()
        || source.tool_call_id.is_some()
        || source
            .logical_run_id
            .as_ref()
            .is_some_and(|run| run.as_str() != selection.source_turn.logical_run_id)
    {
        bail!("initial Task guidance source message conflicts with its durable selection");
    }
    Ok(())
}

pub(super) fn planner_attempt_input_frontier(
    session: &Session,
    attempt_id: &TaskParticipantAttemptId,
) -> usize {
    let projection = session.task_state_projection();
    let mut original = attempt_id;
    // Retry schedules form a backwards chain to the physical request's first input.
    while let Some(schedule) = projection
        .tasks
        .values()
        .find_map(|task| task.participant_retry_schedules.get(original))
    {
        original = &schedule.failed_attempt_id;
    }
    session.entries().iter().position(|entry| matches!(entry,
        SessionLogEntry::Control(ControlEntry::TaskParticipantAttempt(attempt))
            if &attempt.attempt_id == original && attempt.status == TaskParticipantAttemptStatus::Started
    )).unwrap_or(0)
}

pub(super) fn initial_guidance_consumed_by_plan(
    session: &Session,
    guidance: &InitialTaskGuidance,
) -> bool {
    let mut planner = None;
    for entry in session.entries() {
        match entry {
            SessionLogEntry::Control(ControlEntry::TaskParticipantAttempt(attempt))
                if attempt.task_id == guidance.selection.task_id
                    && attempt.purpose == TaskParticipantPurpose::Planner
                    && attempt.plan_version.is_none()
                    && attempt.status == TaskParticipantAttemptStatus::Started =>
            {
                planner = Some(&attempt.attempt_id);
            }
            SessionLogEntry::Control(ControlEntry::TaskPlan(plan))
                if plan.task_id == guidance.selection.task_id
                    && plan.plan_version == 1
                    && plan.status == TaskPlanStatus::Accepted =>
            {
                if planner.is_some_and(|attempt| {
                    guidance.source_index < planner_attempt_input_frontier(session, attempt)
                }) {
                    return true;
                }
            }
            SessionLogEntry::Control(ControlEntry::TaskPlan(plan))
                if plan.task_id == guidance.selection.task_id
                    && plan.plan_version > 1
                    && plan.status == TaskPlanStatus::Accepted =>
            {
                return true;
            }
            _ => {}
        }
    }
    false
}

pub(super) fn initial_guidance_needs_review(
    session: &Session,
    selection: &crate::TaskContinuationSelectedEntry,
) -> Result<bool> {
    if selection.plan_version.is_some() {
        return Ok(false);
    }
    Ok(initial_task_guidances(session, &selection.task_id, None)?
        .into_iter()
        .rfind(|guidance| !initial_guidance_consumed_by_plan(session, guidance))
        .is_some_and(|guidance| guidance.selection == *selection))
}

pub(super) fn initial_planner_prompt(
    objective: &str,
    worktree_availability: crate::TaskPlannerWorktreeAvailability,
    guidance: Option<&str>,
) -> String {
    let prompt = planner_prompt(objective, worktree_availability);
    match guidance {
        Some(guidance) => format!("{prompt}\n\nUser guidance for this task:\n{guidance}"),
        None => prompt,
    }
}

/// A parent plan is committed only after the original child AgentRun has completed. Repair the
/// old publication gap before dispatching a later review or step; never replay a completed request.
pub(super) fn reconcile_committed_initial_planner<H: EventHandler + Send>(
    session: &mut Session,
    request: &SequentialTaskRequest,
    handler: &mut H,
) -> Result<()> {
    let Some(plan_index) = session.entries().iter().position(|entry| matches!(entry,
        SessionLogEntry::Control(ControlEntry::TaskPlan(plan))
            if plan.task_id == request.task_id && plan.plan_version == 1 && plan.status == TaskPlanStatus::Accepted
    )) else { return Ok(()) };
    let projection = session.task_state_projection();
    let task = projection
        .tasks
        .get(&request.task_id)
        .context("committed initial plan has no Task")?;
    if task.parent_session_ref != request.parent_session_ref
        || task.objective != crate::safe_persistence_text(&request.objective)
    {
        bail!("committed initial planner recovery changed its Task identity");
    }
    let mut original = task.participant_attempts.values().filter(|attempt| {
        attempt.purpose == TaskParticipantPurpose::Planner && attempt.status == TaskParticipantAttemptStatus::Started
            && attempt.plan_version.is_none() && session.entries()[..plan_index].iter().any(|entry| matches!(entry,
                SessionLogEntry::Control(ControlEntry::TaskParticipantAttempt(started))
                    if started.attempt_id == attempt.attempt_id && started.status == TaskParticipantAttemptStatus::Started
            ))
    });
    let Some(attempt) = original.next().cloned() else {
        return Ok(());
    };
    if original.next().is_some() {
        bail!("committed initial plan has multiple unsettled planner owners");
    }
    let plan = task
        .plans
        .get(&1)
        .context("committed initial Task plan disappeared")?;
    let output = TaskPlannerSessionRunOutput {
        attempt_id: attempt.attempt_id.clone(),
        accepted_plan: TaskPlanEntry {
            task_id: request.task_id.clone(),
            plan_version: 1,
            status: plan.status,
            steps: plan.steps.clone(),
            reason: plan.reason.clone(),
        },
        step_contracts: plan
            .step_contracts
            .iter()
            .map(|(step_id, contract)| crate::TaskStepContractBoundEntryV2 {
                task_id: request.task_id.clone(),
                plan_version: 1,
                step_id: step_id.clone(),
                contract: contract.clone(),
            })
            .collect(),
        guidance_applied: None,
        child_session_ref: attempt.child_session_ref.clone(),
    };
    commit_task_planner_output(
        session,
        handler,
        request,
        &attempt.attempt_id,
        &attempt.child_session_ref,
        &output,
    )?;
    Ok(())
}

#[cfg(test)]
#[path = "tests/initial_guidance_tests.rs"]
mod tests;

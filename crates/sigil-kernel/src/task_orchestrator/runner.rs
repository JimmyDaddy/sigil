use super::*;
use crate::RunCancellationHandle;
use crate::verification::VerificationExecutionPortV1;
use anyhow::Context;

/// Executes each Task as one model-owned root loop.
pub struct DirectTaskRuntime<R> {
    child_runner: R,
    verification_execution_port: Option<Arc<dyn VerificationExecutionPortV1>>,
    cancellation: Option<RunCancellationHandle>,
    tool_artifact_read_budget: Option<crate::ToolArtifactReadBudgetV1>,
}

impl<R> DirectTaskRuntime<R>
where
    R: TaskChildSessionRunner,
{
    pub fn new_with_child_runner(child_runner: R) -> Self {
        Self {
            child_runner,
            verification_execution_port: None,
            cancellation: None,
            tool_artifact_read_budget: None,
        }
    }

    #[must_use]
    pub fn with_cancellation(mut self, cancellation: RunCancellationHandle) -> Self {
        self.cancellation = Some(cancellation);
        self
    }

    fn bind_cancellation(&self, input: AgentRunInput) -> AgentRunInput {
        let input = match self.tool_artifact_read_budget.as_ref() {
            Some(budget) => input.with_tool_artifact_read_budget(budget.clone()),
            None => input,
        };
        self.cancellation.as_ref().map_or(input.clone(), |handle| {
            input.with_child_cancellation(handle.clone())
        })
    }

    /// Binds root, participant, and child runs to one root artifact-read budget.
    ///
    /// Orchestrator children always inherit the remaining budget: they must not reset the
    /// per-model-turn window themselves, per RFC-0059 §10.3.
    #[must_use]
    pub fn with_tool_artifact_read_budget(
        mut self,
        budget: crate::ToolArtifactReadBudgetV1,
    ) -> Self {
        self.tool_artifact_read_budget = Some(budget.without_turn_reset());
        self
    }

    /// Returns an orchestrator that uses the managed port for verification check execution.
    #[must_use]
    pub fn with_verification_execution_port(
        mut self,
        execution_port: Arc<dyn VerificationExecutionPortV1>,
    ) -> Self {
        self.verification_execution_port = Some(execution_port);
        self
    }

    /// Runs a durable Task through the model-owned root loop.
    ///
    /// # Errors
    ///
    /// Returns an error when durable task state cannot be appended or when either agent run fails.
    #[allow(clippy::too_many_arguments)]
    pub async fn run<H, A>(
        &self,
        session: &mut Session,
        request: DirectTaskRequest,
        executor_options: AgentRunOptions,
        handler: &mut H,
        approval_handler: &mut A,
    ) -> Result<DirectTaskRunOutput>
    where
        H: EventHandler + Send,
        A: ApprovalHandler + Send,
    {
        self.run_with_initial_guidance(
            session,
            request,
            executor_options,
            None,
            handler,
            approval_handler,
        )
        .await
    }

    /// Runs a Task with source-bound user guidance through the direct root loop.
    ///
    /// # Errors
    ///
    /// Returns an error for stale or conflicting guidance, or when Task execution cannot continue.
    #[allow(clippy::too_many_arguments)]
    pub async fn run_with_initial_guidance<H, A>(
        &self,
        session: &mut Session,
        request: DirectTaskRequest,
        executor_options: AgentRunOptions,
        guidance: Option<(&str, &crate::ConversationTurnRef)>,
        handler: &mut H,
        approval_handler: &mut A,
    ) -> Result<DirectTaskRunOutput>
    where
        H: EventHandler + Send,
        A: ApprovalHandler + Send,
    {
        let admission = admit_or_validate_task_run(session, handler, &request)?;
        self.run_direct_execution(
            session,
            request,
            admission,
            executor_options,
            guidance,
            handler,
            approval_handler,
        )
        .await
    }

    /// Continues a first-class direct Task and optionally applies the exact current follow-up.
    ///
    /// Direct guidance is accepted as a durable source-bound continuation after recovery
    /// validation. The direct admission remains bound to the original objective.
    ///
    /// # Errors
    ///
    /// Returns an error when the Task no longer has matching direct authority, when a prior
    /// physical attempt still requires exact recovery, or when direct execution fails.
    pub async fn continue_direct_run<H, A>(
        &self,
        session: &mut Session,
        request: DirectTaskRequest,
        executor_options: AgentRunOptions,
        guidance: Option<(&str, &crate::ConversationTurnRef)>,
        handler: &mut H,
        approval_handler: &mut A,
    ) -> Result<DirectTaskRunOutput>
    where
        H: EventHandler + Send,
        A: ApprovalHandler + Send,
    {
        let admission = admit_or_validate_task_run(session, handler, &request)?;
        self.run_direct_execution(
            session,
            request,
            admission,
            executor_options,
            guidance,
            handler,
            approval_handler,
        )
        .await
    }

    async fn run_direct_execution<H, A>(
        &self,
        session: &mut Session,
        request: DirectTaskRequest,
        admission: crate::TaskDirectExecutionAdmittedV1,
        executor_options: AgentRunOptions,
        guidance: Option<(&str, &crate::ConversationTurnRef)>,
        handler: &mut H,
        approval_handler: &mut A,
    ) -> Result<DirectTaskRunOutput>
    where
        H: EventHandler + Send,
        A: ApprovalHandler + Send,
    {
        admission.validate()?;
        if admission.task_id != request.task_id || !admission.matches_objective(&request.objective)
        {
            bail!("direct execution admission does not match the durable Task objective");
        }
        let projection = session.task_state_projection();
        let task = projection
            .tasks
            .get(&request.task_id)
            .context("direct Task disappeared before attempt admission")?;
        let mut started = task
            .direct_execution_attempts
            .values()
            .filter(|attempt| {
                attempt.admission_id == admission.admission_id
                    && attempt.status == TaskExecutionAttemptStatus::Started
            })
            .cloned()
            .collect::<Vec<_>>();
        if started.len() > 1 {
            bail!("direct Task has multiple started execution attempts");
        }
        let recovering = !started.is_empty();
        if recovering && guidance.is_some_and(|(value, _)| !value.trim().is_empty()) {
            bail!("direct Task recovery cannot mix a new follow-up with an unsettled attempt");
        }
        if let Some((guidance, source)) = guidance.filter(|(value, _)| !value.trim().is_empty()) {
            accept_task_continuation_guidance(
                session,
                &request.task_id,
                task.status,
                guidance,
                source,
                handler,
            )?;
        }
        append_task_run(
            session,
            handler,
            &request,
            TaskRunStatus::Running,
            Some("running direct Task objective".to_owned()),
        )?;
        let attempt = match started.pop() {
            Some(attempt) => attempt,
            None => {
                let ordinal = task
                    .direct_execution_attempts
                    .values()
                    .filter(|attempt| attempt.admission_id == admission.admission_id)
                    .map(|attempt| attempt.ordinal)
                    .max()
                    .unwrap_or(0)
                    .checked_add(1)
                    .ok_or_else(|| anyhow!("direct Task execution attempt ordinal overflow"))?;
                let attempt = crate::TaskDirectExecutionAttemptV1::started(&admission, ordinal);
                append_task_control(
                    session,
                    handler,
                    ControlEntry::TaskDirectExecutionAttemptV1(attempt.clone()),
                )?;
                attempt
            }
        };
        let checklist_revision = session
            .task_state_projection()
            .tasks
            .get(&request.task_id)
            .and_then(|task| task.checklist.as_ref())
            .map_or(0, |checklist| checklist.revision);
        let input = if recovering {
            AgentRunInput::without_persisted_user_message(Vec::new())
                .with_durable_provider_recovery_only()
        } else {
            AgentRunInput::without_persisted_user_message(vec![ModelMessage::user(
                direct_execution_prompt(&request.objective, guidance.map(|(text, _)| text)),
            )])
        }
        .with_task_checklist_update(crate::TaskChecklistUpdateContextV1 {
            task_id: request.task_id.clone(),
            current_revision: checklist_revision,
        })
        .with_run_purpose(AgentRunPurpose::TaskDirectExecution(
            TaskDirectExecutionContext {
                task_id: request.task_id.clone(),
                admission_id: admission.admission_id.clone(),
                attempt_id: attempt.attempt_id.clone(),
            },
        ))
        .with_logical_run_id(crate::task_direct_execution_logical_run_id(
            &attempt.attempt_id,
        ));
        let input = self.bind_cancellation(input);
        let output = self
            .child_runner
            .run_direct_execution_session(
                session,
                TaskDirectExecutionSessionRunRequest {
                    task: request.clone(),
                    admission: admission.clone(),
                    attempt: attempt.clone(),
                    input,
                    options: executor_options.clone(),
                },
                handler,
                approval_handler,
            )
            .await;
        let output = match output {
            Ok(output) => output,
            Err(error) => {
                use crate::agent::execution::{
                    ExecutionDisposition, execution_failure_disposition,
                };
                let disposition = execution_failure_disposition(&error);
                let cancelled = self.cancellation_requested();
                if cancelled {
                    return Err(error.context(
                        "direct Task execution stopped after cancellation won terminal authority",
                    ));
                }
                let mut terminal = attempt;
                terminal.status = match disposition {
                    ExecutionDisposition::Blocked => TaskExecutionAttemptStatus::Blocked,
                    ExecutionDisposition::Cancelled => TaskExecutionAttemptStatus::Cancelled,
                    ExecutionDisposition::Interrupted => TaskExecutionAttemptStatus::Interrupted,
                    _ => TaskExecutionAttemptStatus::Failed,
                };
                terminal.reason = Some(crate::safe_persistence_text(&format!("{error:#}")));
                append_task_control(
                    session,
                    handler,
                    ControlEntry::TaskDirectExecutionAttemptV1(terminal),
                )?;
                let status = match disposition {
                    ExecutionDisposition::Blocked => TaskRunStatus::Paused,
                    ExecutionDisposition::Interrupted => TaskRunStatus::Interrupted,
                    ExecutionDisposition::Cancelled => TaskRunStatus::Cancelled,
                    _ => TaskRunStatus::Failed,
                };
                append_task_run(
                    session,
                    handler,
                    &request,
                    status,
                    Some(if disposition == ExecutionDisposition::Blocked {
                        format!("direct execution is blocked pending recovery: {error:#}")
                    } else {
                        format!("direct execution failed: {error:#}")
                    }),
                )?;
                return Ok(DirectTaskRunOutput {
                    task_id: request.task_id,
                    status,
                });
            }
        };
        if output.attempt_id != attempt.attempt_id {
            bail!("direct execution runtime returned another attempt identity");
        }
        if self.cancellation_requested() {
            bail!("direct Task execution stopped after cancellation won terminal authority");
        }
        let mut status = match output.disposition {
            crate::AgentRunDisposition::FinalAnswer => TaskRunStatus::Completed,
            crate::AgentRunDisposition::AwaitingUserInput(_) => TaskRunStatus::Paused,
            crate::AgentRunDisposition::Interrupted | crate::AgentRunDisposition::Blocked => {
                TaskRunStatus::Paused
            }
            _ => TaskRunStatus::Paused,
        };
        let final_text = crate::safe_persistence_text(&output.final_text);
        let mut completed_attempt_waiting_for_background_agents = false;
        if status == TaskRunStatus::Completed
            && (final_text.trim().is_empty() || output.final_message_id.is_none())
        {
            status = TaskRunStatus::Paused;
        }
        let mut completion_blocker = None;
        if status == TaskRunStatus::Completed {
            let projection = session.task_state_projection();
            let task = projection
                .tasks
                .get(&request.task_id)
                .ok_or_else(|| anyhow!("direct completion Task is unavailable"))?;
            if completion_blocker.is_none()
                && (task.direct_execution_attempts.get(&attempt.attempt_id) != Some(&attempt)
                    || task
                        .direct_execution_admission
                        .as_ref()
                        .is_none_or(|admission| admission.admission_id != attempt.admission_id))
            {
                bail!("direct completion authority changed during execution");
            }
            if completion_blocker.is_none()
                && output.outcome.execution_disposition(&final_text)
                    != crate::agent::execution::ExecutionDisposition::Completed
            {
                completion_blocker = Some("direct execution has unsettled run output".to_owned());
            } else if completion_blocker.is_none()
                && self.cancellation.as_ref().is_some_and(|handle| {
                    !handle.cleanup_complete() || handle.active_effects() != 0
                })
            {
                completion_blocker = Some("direct execution effects have not settled".to_owned());
            } else if completion_blocker.is_none()
                && let Some(evaluation) = projection.evaluate_root_terminal(
                    &request.task_id,
                    TaskRunStatus::Completed,
                    Some(&crate::task::TaskRootTerminalCandidateV1::DirectExecution {
                        attempt_id: attempt.attempt_id.clone(),
                        status: TaskExecutionAttemptStatus::Completed,
                    }),
                )
                && !evaluation.allows_completed()
            {
                completed_attempt_waiting_for_background_agents = !evaluation
                    .unfinished_direct_task_background_agents
                    .is_empty()
                    && evaluation.completion_blockers.iter().all(|blocker| {
                        *blocker
                            == crate::task::TaskRootCompletionBlockerV1::UnfinishedBackgroundAgent
                    });
                completion_blocker = Some(if completed_attempt_waiting_for_background_agents {
                    "direct Task is waiting for its owned background agents".to_owned()
                } else {
                    "direct execution has unfinished Task dependencies".to_owned()
                });
            } else if completion_blocker.is_none() {
                let (mut readiness, auto_run, blocker) =
                    super::readiness::direct_task_completion_readiness(
                        session,
                        &request,
                        &output.outcome,
                        &executor_options,
                    )
                    .await?;
                completion_blocker = blocker;
                if completion_blocker.is_none()
                    && auto_run == VerificationAutoRunPolicy::TrustedOnly
                    && super::readiness::run_task_scope_verification_checks(
                        session,
                        handler,
                        self.verification_execution_port.as_deref(),
                        &request.task_id,
                        EvidenceScope::Task(request.task_id.as_str().to_owned()),
                        &executor_options,
                        &readiness,
                    )
                    .await?
                {
                    let (updated, _, blocker) = super::readiness::direct_task_completion_readiness(
                        session,
                        &request,
                        &output.outcome,
                        &executor_options,
                    )
                    .await?;
                    readiness = updated;
                    completion_blocker = blocker;
                }
                if completion_blocker.is_none() && readiness_blocks_task(&readiness) {
                    completion_blocker = Some(
                        "direct execution requires verification or workspace recovery".to_owned(),
                    );
                }
                append_task_readiness(session, handler, readiness)?;
            }
            if completion_blocker.is_some() {
                status = if completed_attempt_waiting_for_background_agents {
                    TaskRunStatus::Running
                } else {
                    TaskRunStatus::Paused
                };
            }
        }
        let mut terminal = attempt;
        let attempt_completed =
            status == TaskRunStatus::Completed || completed_attempt_waiting_for_background_agents;
        terminal.status = if attempt_completed {
            TaskExecutionAttemptStatus::Completed
        } else {
            TaskExecutionAttemptStatus::Blocked
        };
        terminal.reason = Some(crate::safe_persistence_text(&if let Some(reason) =
            completion_blocker.as_ref()
        {
            reason.clone()
        } else if final_text.trim().is_empty() {
            "direct execution produced no final text".to_owned()
        } else {
            bounded_task_participant_summary(&final_text)
        }));
        if attempt_completed {
            terminal.final_message_id = output.final_message_id;
            terminal.output_hash = Some(format!("sha256:{}", hash_task_text(&final_text)));
        }
        append_task_control(
            session,
            handler,
            ControlEntry::TaskDirectExecutionAttemptV1(terminal),
        )?;
        append_task_run(
            session,
            handler,
            &request,
            status,
            Some(if status == TaskRunStatus::Completed {
                "direct Task objective completed".to_owned()
            } else {
                completion_blocker
                    .unwrap_or_else(|| "direct Task execution paused before completion".to_owned())
            }),
        )?;
        Ok(DirectTaskRunOutput {
            task_id: request.task_id,
            status,
        })
    }

    fn cancellation_requested(&self) -> bool {
        self.cancellation
            .as_ref()
            .is_some_and(RunCancellationHandle::is_cancel_requested)
    }
}

fn hash_task_text(value: &str) -> String {
    let mut digest = Sha256::new();
    digest.update(value.as_bytes());
    format!("{:x}", digest.finalize())
}

fn admit_or_validate_task_run<H>(
    session: &mut Session,
    handler: &mut H,
    request: &DirectTaskRequest,
) -> Result<crate::TaskDirectExecutionAdmittedV1>
where
    H: EventHandler + Send,
{
    let safe_objective = crate::safe_persistence_text(&request.objective);
    let projection = session.task_state_projection();
    let Some(task) = projection.tasks.get(&request.task_id) else {
        append_task_run(
            session,
            handler,
            request,
            TaskRunStatus::Started,
            Some("task execution started".to_owned()),
        )?;
        let admission = crate::TaskDirectExecutionAdmittedV1::task_request(
            request.task_id.clone(),
            &request.objective,
            unix_time_ms(),
        );
        append_task_controls(
            session,
            handler,
            vec![ControlEntry::TaskDirectExecutionAdmittedV1(
                admission.clone(),
            )],
        )?;
        return Ok(admission);
    };
    if task.parent_session_ref != request.parent_session_ref {
        bail!(
            "task {} admission conflicts with its durable parent session",
            request.task_id.as_str()
        );
    }
    if task.objective != safe_objective {
        bail!(
            "task {} admission conflicts with its durable objective",
            request.task_id.as_str()
        );
    }
    let admission = task
        .direct_execution_admission
        .as_ref()
        .context("task has no direct execution admission; current direct Task data is required")?;
    admission.validate()?;
    if !admission.matches_objective(&task.objective) {
        bail!("direct execution admission does not match the durable Task objective");
    }
    Ok(admission.clone())
}

fn accept_task_continuation_guidance<H: EventHandler + Send>(
    session: &mut Session,
    task_id: &TaskId,
    task_status: TaskRunStatus,
    guidance: &str,
    source: &crate::ConversationTurnRef,
    handler: &mut H,
) -> Result<()> {
    crate::ConversationTurnRef::new(
        source.session_scope_id.clone(),
        source.message_id.clone(),
        source.logical_run_id.clone(),
    )?;
    if source.session_scope_id != session.session_scope_id() {
        bail!("direct Task guidance source belongs to a different session");
    }
    let projected = crate::project_conversation_prompt_for_persistence(guidance);
    let entries = if session.store_path().is_some() {
        session
            .read_durable_event_records()?
            .iter()
            .filter_map(|record| record.session_log_entry().transpose())
            .collect::<Result<Vec<_>>>()?
    } else {
        session.entries().to_vec()
    };
    let mut existing_selection = None;
    for entry in &entries {
        if let SessionLogEntry::Control(ControlEntry::TaskContinuationSelected(selection)) = entry
            && selection.source_turn == *source
        {
            selection.validate_shape()?;
            if selection.task_id != *task_id
                || selection.control
                    != crate::TaskContinuationControlKind::ApplyCurrentRequestAsGuidance
                || selection.prompt_hash != projected.prompt_hash
                || selection.exact_prompt_required != projected.exact_prompt_required
                || selection.guidance != projected.safe_prompt
            {
                bail!("direct Task guidance conflicts with its durable source selection");
            }
            existing_selection = Some(selection);
        }
    }
    let existing_user = entries.iter().find_map(|entry| match entry {
        SessionLogEntry::User(message) if message.id == source.message_id => Some(message),
        _ => None,
    });
    if let Some(message) = existing_user {
        if message.role != crate::MessageRole::User
            || message.content.as_deref() != Some(projected.safe_prompt.as_str())
            || message.assistant_kind.is_some()
            || !message.tool_calls.is_empty()
            || message.tool_call_id.is_some()
            || message
                .logical_run_id
                .as_ref()
                .is_some_and(|run| run.as_str() != source.logical_run_id)
        {
            bail!("direct Task guidance source message conflicts with durable content");
        }
    } else if existing_selection.is_some() {
        bail!("direct Task guidance selection is missing its durable source message");
    }
    if existing_selection.is_some() {
        return Ok(());
    }
    let selection = crate::TaskContinuationSelectedEntry {
        task_id: task_id.clone(),
        source_turn: source.clone(),
        task_status,
        route_contract_fingerprint: "explicit-task-guidance-v1".to_owned(),
        control: crate::TaskContinuationControlKind::ApplyCurrentRequestAsGuidance,
        prompt_hash: projected.prompt_hash,
        exact_prompt_required: projected.exact_prompt_required,
        guidance: projected.safe_prompt.clone(),
        selected_at_ms: unix_time_ms(),
    };
    selection.validate_shape()?;
    let mut entries = Vec::new();
    if existing_user.is_none() {
        let mut message = ModelMessage::user(projected.safe_prompt);
        message.id = source.message_id.clone();
        message.logical_run_id = Some(crate::LogicalRunId::new(source.logical_run_id.clone())?);
        entries.push(SessionLogEntry::User(message));
    }
    entries.push(SessionLogEntry::Control(
        ControlEntry::TaskContinuationSelected(selection),
    ));
    // User input and this private selection have no public run-event projection. Commit
    // both sources atomically, then notify private consumers; the public outbox API
    // requires at least one projected event and must not manufacture one for guidance.
    session.append_session_entries(entries.clone())?;
    handler.handle_committed_session_publications(entries, Vec::new())?;
    Ok(())
}

pub(super) fn direct_execution_prompt(objective: &str, guidance: Option<&str>) -> String {
    let follow_up = guidance
        .filter(|value| !value.trim().is_empty())
        .map(|value| {
            format!(
                "\n\nThe user supplied this follow-up while the same Task was active. Address it as part of the same execution, keep the existing checklist, and then continue or finish the approved objective:\n\n{value}"
            )
        })
        .unwrap_or_default();
    format!(
        "Execute the following complete, user-approved Task objective now. Use the existing conversation and tool results to preserve completed work and execute only what remains; do not replay completed tool calls. Use the available tools, keep the optional display checklist current when it helps the user, verify the result, and finish with a concise outcome. Checklist updates are progress reporting only and never execution authority.\n\n{objective}{follow_up}"
    )
}

fn unix_time_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| {
            duration.as_millis().min(u128::from(u64::MAX)) as u64
        })
}

fn readiness_blocks_task(readiness: &ReadinessEvaluatedEntry) -> bool {
    readiness
        .evaluation
        .required_actions
        .iter()
        .any(|action| !matches!(action, RequiredAction::ProvideVerificationConfig))
}

#[cfg(test)]
#[path = "tests/direct_guidance_tests.rs"]
mod direct_guidance_tests;

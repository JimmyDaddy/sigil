use super::*;
use crate::{RunCancellationHandle, RunEffectClass, RunEffectKind};

/// Sequential planner/executor task orchestrator.
pub struct SequentialTaskOrchestrator<R> {
    child_runner: R,
    execution_backend: Option<Arc<dyn ExecutionBackend>>,
    cancellation: Option<RunCancellationHandle>,
    max_parallel_read_steps: usize,
}

impl<R> SequentialTaskOrchestrator<R>
where
    R: TaskChildSessionRunner,
{
    pub fn new_with_child_runner(child_runner: R) -> Self {
        Self {
            child_runner,
            execution_backend: None,
            cancellation: None,
            max_parallel_read_steps: DEFAULT_TASK_READ_ONLY_CONCURRENCY,
        }
    }

    #[must_use]
    pub fn with_cancellation(mut self, cancellation: RunCancellationHandle) -> Self {
        self.cancellation = Some(cancellation);
        self
    }

    fn bind_cancellation(&self, input: AgentRunInput) -> AgentRunInput {
        self.cancellation.as_ref().map_or(input.clone(), |handle| {
            input.with_child_cancellation(handle.clone())
        })
    }

    /// Returns an orchestrator that uses the provided backend for verification check execution.
    #[must_use]
    pub fn with_execution_backend(mut self, execution_backend: Arc<dyn ExecutionBackend>) -> Self {
        self.execution_backend = Some(execution_backend);
        self
    }

    /// Sets the maximum number of independent shared-read-only steps launched together.
    #[must_use]
    pub fn with_max_parallel_read_steps(mut self, max_parallel_read_steps: usize) -> Self {
        self.max_parallel_read_steps = max_parallel_read_steps.max(1);
        self
    }

    /// Runs planner once and then executes accepted plan steps sequentially.
    ///
    /// # Errors
    ///
    /// Returns an error when durable task state cannot be appended or when either agent run fails.
    #[allow(clippy::too_many_arguments)]
    pub async fn run<H, A>(
        &self,
        session: &mut Session,
        request: SequentialTaskRequest,
        planner_options: AgentRunOptions,
        executor_options: AgentRunOptions,
        subagent_read_options: AgentRunOptions,
        subagent_write_options: AgentRunOptions,
        max_plan_steps: usize,
        handler: &mut H,
        approval_handler: &mut A,
    ) -> Result<SequentialTaskRunOutput>
    where
        H: EventHandler + Send,
        A: ApprovalHandler + Send,
    {
        let has_accepted_plan = admit_or_validate_task_run(session, handler, &request)?;
        if !has_accepted_plan {
            let attempt = begin_participant_attempt(
                session,
                handler,
                &request,
                TaskParticipantPurpose::Planner,
                None,
                None,
                AgentRole::Planner,
            )?;
            let planner_input = self.bind_cancellation(
                AgentRunInput::without_persisted_user_message(vec![ModelMessage::user(
                    planner_prompt(&request.objective),
                )])
                .with_task_plan_update(TaskPlanUpdateContext {
                    task_id: request.task_id.clone(),
                    max_plan_steps,
                    max_plan_versions: crate::DEFAULT_TASK_MAX_PLAN_VERSIONS,
                })
                .with_run_purpose(AgentRunPurpose::TaskPlanner(TaskPlannerContext {
                    task_id: request.task_id.clone(),
                    attempt_id: Some(attempt.attempt_id.clone()),
                }))
                .with_logical_run_id(task_participant_logical_run_id(&attempt.attempt_id)),
            );
            let planner_output = self
                .child_runner
                .run_planner_session(
                    session,
                    TaskPlannerSessionRunRequest {
                        task: request.clone(),
                        attempt_id: attempt.attempt_id.clone(),
                        child_session_ref: attempt.child_session_ref.clone(),
                        child_input: planner_input,
                        options: planner_options,
                        discovery_options: subagent_read_options.clone(),
                    },
                    handler,
                    approval_handler,
                )
                .await;
            match planner_output {
                Ok(output) => {
                    validate_isolated_planner_output(&request, &attempt, &output)?;
                    append_task_control(
                        session,
                        handler,
                        ControlEntry::TaskPlan(output.accepted_plan.clone()),
                    )?;
                    let result = participant_result_entry(
                        &attempt,
                        &format!(
                            "accepted task plan v{} with {} steps",
                            output.accepted_plan.plan_version,
                            output.accepted_plan.steps.len()
                        ),
                        None,
                        Vec::new(),
                        Vec::new(),
                        Vec::new(),
                    )?;
                    append_participant_result_and_terminal(
                        session,
                        handler,
                        &attempt,
                        result,
                        TaskParticipantAttemptStatus::Completed,
                        None,
                    )?;
                }
                Err(error) => {
                    append_participant_terminal(
                        session,
                        handler,
                        &attempt,
                        TaskParticipantAttemptStatus::Failed,
                        Some(format!("planner failed: {error:#}")),
                    )?;
                    append_task_run(
                        session,
                        handler,
                        &request,
                        TaskRunStatus::Failed,
                        Some(format!(
                            "task orchestration failed: planner failed: {error:#}"
                        )),
                    )?;
                    return Err(error);
                }
            }
        }

        match self
            .continue_run(
                session,
                request.clone(),
                executor_options,
                subagent_read_options,
                subagent_write_options,
                None,
                handler,
                approval_handler,
            )
            .await
        {
            Ok(output) => Ok(output),
            Err(error) => {
                // If the planner never produced an executable plan, preserve the failed task
                // state before surfacing the orchestration error.
                append_task_run(
                    session,
                    handler,
                    &request,
                    TaskRunStatus::Failed,
                    Some(format!("task orchestration failed: {error:#}")),
                )?;
                Err(error)
            }
        }
    }

    /// Continues an existing task from the latest durable accepted plan.
    ///
    /// Completed steps are skipped. Pending, running, blocked, failed, cancelled, and interrupted
    /// steps are eligible for explicit user-triggered continue.
    ///
    /// # Errors
    ///
    /// Returns an error when no executable task plan exists or a resumed run cannot be appended.
    #[allow(clippy::too_many_arguments)]
    pub async fn continue_run<H, A>(
        &self,
        session: &mut Session,
        request: SequentialTaskRequest,
        executor_options: AgentRunOptions,
        subagent_read_options: AgentRunOptions,
        subagent_write_options: AgentRunOptions,
        guidance: Option<String>,
        handler: &mut H,
        approval_handler: &mut A,
    ) -> Result<SequentialTaskRunOutput>
    where
        H: EventHandler + Send,
        A: ApprovalHandler + Send,
    {
        let projection = session.task_state_projection();
        let task = projection.tasks.get(&request.task_id).ok_or_else(|| {
            anyhow!(
                "task {} is not present in session",
                request.task_id.as_str()
            )
        })?;
        let (plan_version, steps) = latest_executable_plan(task)?;
        let guidance = normalize_task_guidance(guidance);
        append_task_run(
            session,
            handler,
            &request,
            TaskRunStatus::Running,
            Some(task_continue_reason(plan_version, guidance.as_deref())),
        )?;

        let mut step_outputs = Vec::new();
        let max_scheduler_batches = steps.len().saturating_add(1).max(1);
        for _ in 0..max_scheduler_batches {
            let projection = session.task_state_projection();
            let task = projection.tasks.get(&request.task_id).ok_or_else(|| {
                anyhow!(
                    "task {} disappeared from session projection",
                    request.task_id.as_str()
                )
            })?;
            let runnable = runnable_steps_for_continue(
                session,
                task,
                plan_version,
                &steps,
                self.max_parallel_read_steps,
                [
                    &executor_options,
                    &subagent_read_options,
                    &subagent_write_options,
                ],
            )?;
            if runnable.steps.is_empty() {
                let status = if let Some(reason) = runnable.paused_reason {
                    append_task_run(
                        session,
                        handler,
                        &request,
                        TaskRunStatus::Paused,
                        Some(reason),
                    )?;
                    TaskRunStatus::Paused
                } else {
                    self.complete_task_with_synthesis(
                        session,
                        &request,
                        plan_version,
                        subagent_read_options.clone(),
                        handler,
                        approval_handler,
                    )
                    .await?
                };
                return Ok(SequentialTaskRunOutput {
                    task_id: request.task_id,
                    plan_version,
                    steps: step_outputs,
                    status,
                });
            }

            let is_parallel_read_batch = runnable.steps.len() > 1
                && runnable.steps.iter().all(|step| {
                    matches!(
                        step.effective_mode(),
                        TaskStepMode::Read | TaskStepMode::Review | TaskStepMode::Verify
                    ) && step.effective_isolation() == TaskIsolationMode::SharedReadOnly
                });
            if is_parallel_read_batch {
                let mut batch_contexts = Vec::with_capacity(runnable.steps.len());
                let mut batch_requests = Vec::with_capacity(runnable.steps.len());
                let mut child_effects = Vec::with_capacity(runnable.steps.len());
                for step in runnable.steps {
                    let step_options = match step.role {
                        AgentRole::Planner | AgentRole::Executor => executor_options.clone(),
                        AgentRole::SubagentRead => subagent_read_options.clone(),
                        AgentRole::SubagentWrite => subagent_write_options.clone(),
                    };
                    append_task_step(
                        session,
                        handler,
                        &request.task_id,
                        plan_version,
                        &step,
                        TaskStepStatus::Running,
                        None,
                        None,
                    )?;
                    let attempt = begin_participant_attempt(
                        session,
                        handler,
                        &request,
                        TaskParticipantPurpose::Step,
                        Some(plan_version),
                        Some(&step.step_id),
                        step.role,
                    )?;
                    let prompt = if step.role == AgentRole::Executor {
                        executor_step_prompt(
                            &request.objective,
                            plan_version,
                            &step,
                            guidance.as_deref(),
                        )
                    } else {
                        subagent_step_prompt(
                            &request.objective,
                            plan_version,
                            &step,
                            guidance.as_deref(),
                        )
                    };
                    let child_input = self.bind_cancellation(
                        AgentRunInput::without_persisted_user_message(vec![ModelMessage::user(
                            prompt,
                        )])
                        .with_run_purpose(AgentRunPurpose::TaskParticipant(
                            TaskParticipantContext {
                                task_id: request.task_id.clone(),
                                plan_version,
                                step_id: step.step_id.clone(),
                                attempt_id: attempt.attempt_id.clone(),
                            },
                        ))
                        .with_logical_run_id(task_participant_logical_run_id(&attempt.attempt_id)),
                    );
                    child_effects.push(
                        self.cancellation
                            .as_ref()
                            .map(|handle| {
                                handle
                                    .begin_effect(RunEffectClass::Forward, RunEffectKind::ChildWork)
                            })
                            .transpose()?,
                    );
                    batch_requests.push(TaskChildSessionRunRequest {
                        task: request.clone(),
                        attempt_id: attempt.attempt_id.clone(),
                        child_session_ref: attempt.child_session_ref.clone(),
                        plan_version,
                        step: step.clone(),
                        child_input,
                        options: step_options.clone(),
                        changeset_only_base_snapshot_id: None,
                    });
                    batch_contexts.push((step, attempt, step_options));
                }

                let batch_results = self
                    .child_runner
                    .run_child_session_batch(session, batch_requests, handler, approval_handler)
                    .await?;
                drop(child_effects);
                if batch_results.len() != batch_contexts.len() {
                    bail!(
                        "task child batch returned {} results for {} requests",
                        batch_results.len(),
                        batch_contexts.len()
                    );
                }

                let mut first_problem = None;
                for ((step, attempt, step_options), child_result) in
                    batch_contexts.into_iter().zip(batch_results)
                {
                    let step_output = match child_result {
                        Ok(output) => {
                            validate_participant_output_identity(
                                &attempt,
                                &output.attempt_id,
                                &output.child_session_ref,
                            )?;
                            self.commit_step_output(
                                session,
                                handler,
                                &request,
                                plan_version,
                                &steps,
                                &step,
                                &attempt,
                                &step_options,
                                None,
                                StepRunOutput {
                                    final_text: output.final_text,
                                    outcome: output.outcome,
                                    final_answer_ref: output.final_answer_ref,
                                    artifact_refs: output.artifact_refs,
                                    changeset_proposal: output.changeset_proposal,
                                    changeset_only_after_snapshot_id: output
                                        .changeset_only_after_snapshot_id,
                                },
                            )
                            .await?
                        }
                        Err(error) => {
                            let step_output = self
                                .commit_step_failure(
                                    session,
                                    handler,
                                    &request,
                                    plan_version,
                                    &steps,
                                    &step,
                                    &attempt,
                                    &step_options,
                                    None,
                                    &error,
                                )
                                .await?;
                            if first_problem.is_none() {
                                first_problem = Some((
                                    TaskRunStatus::Failed,
                                    format!("step {} failed: {error:#}", step.step_id.as_str()),
                                ));
                            }
                            step_output
                        }
                    };
                    if step_output.status != TaskStepStatus::Completed && first_problem.is_none() {
                        first_problem = Some((
                            task_status_from_step_status(step_output.status),
                            step_terminal_reason(&step.step_id, step_output.status),
                        ));
                    }
                    step_outputs.push(step_output);
                }
                if let Some((status, reason)) = first_problem {
                    append_task_run(session, handler, &request, status, Some(reason))?;
                    return Ok(SequentialTaskRunOutput {
                        task_id: request.task_id,
                        plan_version,
                        steps: step_outputs,
                        status,
                    });
                }
                continue;
            }

            for step in runnable.steps {
                let step_options = match step.role {
                    AgentRole::Planner | AgentRole::Executor => executor_options.clone(),
                    AgentRole::SubagentRead => subagent_read_options.clone(),
                    AgentRole::SubagentWrite => subagent_write_options.clone(),
                };
                append_task_step(
                    session,
                    handler,
                    &request.task_id,
                    plan_version,
                    &step,
                    TaskStepStatus::Running,
                    None,
                    None,
                )?;
                let attempt = begin_participant_attempt(
                    session,
                    handler,
                    &request,
                    TaskParticipantPurpose::Step,
                    Some(plan_version),
                    Some(&step.step_id),
                    step.role,
                )?;
                let write_lease_id = acquire_task_write_lease(
                    session,
                    handler,
                    &request,
                    plan_version,
                    &step,
                    &step_options,
                )?;
                let step_run_result = self
                    .run_child_step(
                        session,
                        &request,
                        &attempt,
                        plan_version,
                        &step,
                        step_options.clone(),
                        guidance.as_deref(),
                        handler,
                        approval_handler,
                    )
                    .await;
                let output = match step_run_result {
                    Ok(output) => output,
                    Err(error) => {
                        let step_output = self
                            .commit_step_failure(
                                session,
                                handler,
                                &request,
                                plan_version,
                                &steps,
                                &step,
                                &attempt,
                                &step_options,
                                write_lease_id,
                                &error,
                            )
                            .await?;
                        step_outputs.push(step_output);
                        append_task_run(
                            session,
                            handler,
                            &request,
                            TaskRunStatus::Failed,
                            Some(format!("step {} failed: {error:#}", step.step_id.as_str())),
                        )?;
                        return Ok(SequentialTaskRunOutput {
                            task_id: request.task_id,
                            plan_version,
                            steps: step_outputs,
                            status: TaskRunStatus::Failed,
                        });
                    }
                };
                let step_output = self
                    .commit_step_output(
                        session,
                        handler,
                        &request,
                        plan_version,
                        &steps,
                        &step,
                        &attempt,
                        &step_options,
                        write_lease_id,
                        output,
                    )
                    .await?;
                let status = step_output.status;
                step_outputs.push(step_output);
                if status != TaskStepStatus::Completed {
                    let task_status = task_status_from_step_status(status);
                    append_task_run(
                        session,
                        handler,
                        &request,
                        task_status,
                        Some(step_terminal_reason(&step.step_id, status)),
                    )?;
                    return Ok(SequentialTaskRunOutput {
                        task_id: request.task_id,
                        plan_version,
                        steps: step_outputs,
                        status: task_status,
                    });
                }
            }
        }

        bail!(
            "task {} did not reach a terminal or paused scheduler state after {} scheduler batches",
            request.task_id.as_str(),
            max_scheduler_batches
        )
    }

    #[allow(clippy::too_many_arguments)]
    async fn commit_step_failure<H>(
        &self,
        session: &mut Session,
        handler: &mut H,
        request: &SequentialTaskRequest,
        plan_version: u32,
        plan_steps: &[TaskStepSpec],
        step: &TaskStepSpec,
        attempt: &TaskParticipantAttemptEntry,
        step_options: &AgentRunOptions,
        write_lease_id: Option<WriteLeaseId>,
        error: &anyhow::Error,
    ) -> Result<SequentialTaskStepOutput>
    where
        H: EventHandler + Send,
    {
        release_task_write_lease(
            session,
            handler,
            write_lease_id,
            WriteLeaseReleaseStatus::Interrupted,
        )?;
        append_participant_terminal(
            session,
            handler,
            attempt,
            TaskParticipantAttemptStatus::Failed,
            Some(format!("step failed: {error:#}")),
        )?;
        let readiness =
            task_step_failure_readiness_nonblocking(session, request, step, step_options).await?;
        append_task_step(
            session,
            handler,
            &request.task_id,
            plan_version,
            step,
            TaskStepStatus::Failed,
            None,
            Some(format!("{error:#}")),
        )?;
        append_cancelled_dependent_steps(
            session,
            handler,
            &request.task_id,
            plan_version,
            plan_steps,
            &step.step_id,
            TaskStepStatus::Failed,
        )?;
        append_task_readiness(session, handler, readiness.clone())?;
        Ok(SequentialTaskStepOutput {
            step_id: step.step_id.clone(),
            status: TaskStepStatus::Failed,
            verification_verdict: readiness.evaluation.verification_verdict,
            visible_state: readiness.evaluation.visible_state,
            outcome: AgentRunOutcome::default(),
        })
    }

    #[allow(clippy::too_many_arguments)]
    async fn commit_step_output<H>(
        &self,
        session: &mut Session,
        handler: &mut H,
        request: &SequentialTaskRequest,
        plan_version: u32,
        plan_steps: &[TaskStepSpec],
        step: &TaskStepSpec,
        attempt: &TaskParticipantAttemptEntry,
        step_options: &AgentRunOptions,
        write_lease_id: Option<WriteLeaseId>,
        output: StepRunOutput,
    ) -> Result<SequentialTaskStepOutput>
    where
        H: EventHandler + Send,
    {
        let initial_status = step_status_from_outcome(&output);
        let participant_status = participant_status_from_step_status(initial_status);
        let participant_result = participant_result_entry(
            attempt,
            &output.final_text,
            output.final_answer_ref.clone(),
            output.artifact_refs.clone(),
            output.outcome.changed_files.clone(),
            Vec::new(),
        )?;
        append_participant_result_and_terminal(
            session,
            handler,
            attempt,
            participant_result,
            participant_status,
            step_reason_from_output(initial_status, &output),
        )?;
        release_task_write_lease(
            session,
            handler,
            write_lease_id,
            write_lease_release_status_from_step_status(initial_status),
        )?;
        let mut readiness = task_step_readiness_nonblocking(
            session,
            request,
            step,
            initial_status,
            &output,
            step_options,
        )
        .await?;
        if initial_status == TaskStepStatus::Completed
            && task_step_auto_run_policy(session, request, step, step_options)?
                == VerificationAutoRunPolicy::TrustedOnly
            && run_task_step_verification_checks(
                session,
                handler,
                self.execution_backend.as_deref(),
                request,
                step,
                step_options,
                &readiness,
            )
            .await?
        {
            readiness = task_step_readiness_nonblocking(
                session,
                request,
                step,
                initial_status,
                &output,
                step_options,
            )
            .await?;
        }
        let status = step_status_after_readiness(initial_status, &readiness);
        if status != initial_status {
            readiness = task_step_readiness_nonblocking(
                session,
                request,
                step,
                status,
                &output,
                step_options,
            )
            .await?;
        }
        append_task_step(
            session,
            handler,
            &request.task_id,
            plan_version,
            step,
            status,
            Some(bounded_task_participant_summary(&output.final_text)),
            step_reason_from_output(status, &output),
        )?;
        if cancels_dependent_steps(status) {
            append_cancelled_dependent_steps(
                session,
                handler,
                &request.task_id,
                plan_version,
                plan_steps,
                &step.step_id,
                status,
            )?;
        }
        append_task_readiness(session, handler, readiness.clone())?;
        Ok(SequentialTaskStepOutput {
            step_id: step.step_id.clone(),
            status,
            verification_verdict: readiness.evaluation.verification_verdict,
            visible_state: readiness.evaluation.visible_state,
            outcome: output.outcome,
        })
    }

    /// Runs one explicit child-session task step without invoking the planner.
    ///
    /// This is intended for user-invoked workflows that already resolved to a single
    /// child-session action, such as a `run_as = child_session` skill.
    ///
    /// # Errors
    ///
    /// Returns an error when the step is not a subagent role, durable task state cannot be
    /// appended, or the child agent run fails before a terminal task status can be recorded.
    #[allow(clippy::too_many_arguments)]
    pub async fn run_direct_child_session<H, A>(
        &self,
        session: &mut Session,
        request: SequentialTaskRequest,
        step: TaskStepSpec,
        child_input: AgentRunInput,
        subagent_read_options: AgentRunOptions,
        subagent_write_options: AgentRunOptions,
        handler: &mut H,
        approval_handler: &mut A,
    ) -> Result<SequentialTaskRunOutput>
    where
        H: EventHandler + Send,
        A: ApprovalHandler + Send,
    {
        if !matches!(
            step.role,
            AgentRole::SubagentRead | AgentRole::SubagentWrite
        ) {
            bail!("direct child session requires a subagent role");
        }
        let plan_version = 1;
        append_task_run(
            session,
            handler,
            &request,
            TaskRunStatus::Started,
            Some("direct child session started".to_owned()),
        )?;
        append_task_control(
            session,
            handler,
            ControlEntry::TaskPlan(TaskPlanEntry {
                task_id: request.task_id.clone(),
                plan_version,
                status: TaskPlanStatus::Accepted,
                steps: vec![TaskStepSpec {
                    title: crate::safe_persistence_text(&step.title),
                    display_name: step
                        .display_name
                        .as_deref()
                        .map(crate::safe_persistence_text),
                    detail: step.detail.as_deref().map(crate::safe_persistence_text),
                    ..step.clone()
                }],
                reason: Some("direct child session invocation".to_owned()),
            }),
        )?;
        append_task_run(
            session,
            handler,
            &request,
            TaskRunStatus::Running,
            Some(format!("running direct child session plan v{plan_version}")),
        )?;
        append_task_step(
            session,
            handler,
            &request.task_id,
            plan_version,
            &step,
            TaskStepStatus::Running,
            None,
            None,
        )?;

        let attempt = begin_participant_attempt(
            session,
            handler,
            &request,
            TaskParticipantPurpose::Step,
            Some(plan_version),
            Some(&step.step_id),
            step.role,
        )?;
        let synthesis_options = subagent_read_options.clone();

        let options = match step.role {
            AgentRole::SubagentRead => subagent_read_options,
            AgentRole::SubagentWrite => subagent_write_options,
            AgentRole::Planner | AgentRole::Executor => unreachable!("role checked above"),
        };
        let readiness_options = options.clone();
        let write_lease_id = acquire_task_write_lease(
            session,
            handler,
            &request,
            plan_version,
            &step,
            &readiness_options,
        )?;
        let output = match self
            .run_child_step_with_input(
                session,
                &request,
                &attempt,
                plan_version,
                &step,
                options,
                child_input
                    .with_run_purpose(AgentRunPurpose::TaskParticipant(TaskParticipantContext {
                        task_id: request.task_id.clone(),
                        plan_version,
                        step_id: step.step_id.clone(),
                        attempt_id: attempt.attempt_id.clone(),
                    }))
                    .with_logical_run_id(task_participant_logical_run_id(&attempt.attempt_id)),
                handler,
                approval_handler,
            )
            .await
        {
            Ok(output) => output,
            Err(error) => {
                release_task_write_lease(
                    session,
                    handler,
                    write_lease_id,
                    WriteLeaseReleaseStatus::Interrupted,
                )?;
                append_participant_terminal(
                    session,
                    handler,
                    &attempt,
                    TaskParticipantAttemptStatus::Failed,
                    Some(format!("step failed: {error:#}")),
                )?;
                let readiness = task_step_failure_readiness_nonblocking(
                    session,
                    &request,
                    &step,
                    &readiness_options,
                )
                .await?;
                append_task_step(
                    session,
                    handler,
                    &request.task_id,
                    plan_version,
                    &step,
                    TaskStepStatus::Failed,
                    None,
                    Some(format!("{error:#}")),
                )?;
                append_task_readiness(session, handler, readiness.clone())?;
                append_task_run(
                    session,
                    handler,
                    &request,
                    TaskRunStatus::Failed,
                    Some(format!("step {} failed: {error:#}", step.step_id.as_str())),
                )?;
                return Ok(SequentialTaskRunOutput {
                    task_id: request.task_id,
                    plan_version,
                    steps: vec![SequentialTaskStepOutput {
                        step_id: step.step_id,
                        status: TaskStepStatus::Failed,
                        verification_verdict: readiness.evaluation.verification_verdict,
                        visible_state: readiness.evaluation.visible_state,
                        outcome: AgentRunOutcome::default(),
                    }],
                    status: TaskRunStatus::Failed,
                });
            }
        };
        let initial_status = step_status_from_outcome(&output);
        let participant_result = participant_result_entry(
            &attempt,
            &output.final_text,
            output.final_answer_ref.clone(),
            output.artifact_refs.clone(),
            output.outcome.changed_files.clone(),
            Vec::new(),
        )?;
        append_participant_result_and_terminal(
            session,
            handler,
            &attempt,
            participant_result,
            participant_status_from_step_status(initial_status),
            step_reason_from_output(initial_status, &output),
        )?;
        release_task_write_lease(
            session,
            handler,
            write_lease_id,
            write_lease_release_status_from_step_status(initial_status),
        )?;
        let mut readiness = task_step_readiness_nonblocking(
            session,
            &request,
            &step,
            initial_status,
            &output,
            &readiness_options,
        )
        .await?;
        if initial_status == TaskStepStatus::Completed
            && task_step_auto_run_policy(session, &request, &step, &readiness_options)?
                == VerificationAutoRunPolicy::TrustedOnly
            && run_task_step_verification_checks(
                session,
                handler,
                self.execution_backend.as_deref(),
                &request,
                &step,
                &readiness_options,
                &readiness,
            )
            .await?
        {
            readiness = task_step_readiness_nonblocking(
                session,
                &request,
                &step,
                initial_status,
                &output,
                &readiness_options,
            )
            .await?;
        }
        let status = step_status_after_readiness(initial_status, &readiness);
        if status != initial_status {
            readiness = task_step_readiness_nonblocking(
                session,
                &request,
                &step,
                status,
                &output,
                &readiness_options,
            )
            .await?;
        }
        append_task_step(
            session,
            handler,
            &request.task_id,
            plan_version,
            &step,
            status,
            Some(bounded_task_participant_summary(&output.final_text)),
            step_reason_from_output(status, &output),
        )?;
        append_task_readiness(session, handler, readiness.clone())?;
        let task_status = if status == TaskStepStatus::Completed {
            self.complete_task_with_synthesis(
                session,
                &request,
                plan_version,
                synthesis_options,
                handler,
                approval_handler,
            )
            .await?
        } else {
            task_status_from_step_status(status)
        };
        if task_status != TaskRunStatus::Completed {
            append_task_run(
                session,
                handler,
                &request,
                task_status,
                Some(step_terminal_reason(&step.step_id, status)),
            )?;
        }
        Ok(SequentialTaskRunOutput {
            task_id: request.task_id,
            plan_version,
            steps: vec![SequentialTaskStepOutput {
                step_id: step.step_id,
                status,
                verification_verdict: readiness.evaluation.verification_verdict,
                visible_state: readiness.evaluation.visible_state,
                outcome: output.outcome,
            }],
            status: task_status,
        })
    }

    pub(super) async fn run_child_step<H, A>(
        &self,
        parent_session: &mut Session,
        request: &SequentialTaskRequest,
        attempt: &TaskParticipantAttemptEntry,
        plan_version: u32,
        step: &TaskStepSpec,
        options: AgentRunOptions,
        guidance: Option<&str>,
        handler: &mut H,
        approval_handler: &mut A,
    ) -> Result<StepRunOutput>
    where
        H: EventHandler + Send,
        A: ApprovalHandler + Send,
    {
        let prompt = if step.role == AgentRole::Executor {
            executor_step_prompt(&request.objective, plan_version, step, guidance)
        } else {
            subagent_step_prompt(&request.objective, plan_version, step, guidance)
        };
        let child_input =
            AgentRunInput::without_persisted_user_message(vec![ModelMessage::user(prompt)])
                .with_run_purpose(AgentRunPurpose::TaskParticipant(TaskParticipantContext {
                    task_id: request.task_id.clone(),
                    plan_version,
                    step_id: step.step_id.clone(),
                    attempt_id: attempt.attempt_id.clone(),
                }))
                .with_logical_run_id(task_participant_logical_run_id(&attempt.attempt_id));
        self.run_child_step_with_input(
            parent_session,
            request,
            attempt,
            plan_version,
            step,
            options,
            child_input,
            handler,
            approval_handler,
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    async fn run_child_step_with_input<H, A>(
        &self,
        parent_session: &mut Session,
        request: &SequentialTaskRequest,
        attempt: &TaskParticipantAttemptEntry,
        plan_version: u32,
        step: &TaskStepSpec,
        options: AgentRunOptions,
        child_input: AgentRunInput,
        handler: &mut H,
        approval_handler: &mut A,
    ) -> Result<StepRunOutput>
    where
        H: EventHandler + Send,
        A: ApprovalHandler + Send,
    {
        let changeset_only_base_snapshot_id =
            if step.effective_isolation() == TaskIsolationMode::ChangesetOnly {
                Some(capture_changeset_only_parent_snapshot_id(
                    parent_session,
                    request,
                    plan_version,
                    step,
                    &options,
                    "base",
                )?)
            } else {
                None
            };
        let child_input = if changeset_only_base_snapshot_id.is_some() {
            with_changeset_only_child_contract(child_input)
        } else {
            child_input
        };
        let child_input = self.bind_cancellation(child_input);
        let _child_effect = self
            .cancellation
            .as_ref()
            .map(|handle| handle.begin_effect(RunEffectClass::Forward, RunEffectKind::ChildWork))
            .transpose()?;
        let output = self
            .child_runner
            .run_child_session(
                parent_session,
                TaskChildSessionRunRequest {
                    task: request.clone(),
                    attempt_id: attempt.attempt_id.clone(),
                    child_session_ref: attempt.child_session_ref.clone(),
                    plan_version,
                    step: step.clone(),
                    child_input,
                    options: options.clone(),
                    changeset_only_base_snapshot_id: changeset_only_base_snapshot_id.clone(),
                },
                handler,
                approval_handler,
            )
            .await?;
        validate_participant_output_identity(
            attempt,
            &output.attempt_id,
            &output.child_session_ref,
        )?;
        let step_output = StepRunOutput {
            final_text: output.final_text,
            outcome: output.outcome,
            final_answer_ref: output.final_answer_ref,
            artifact_refs: output.artifact_refs,
            changeset_proposal: output.changeset_proposal,
            changeset_only_after_snapshot_id: output.changeset_only_after_snapshot_id,
        };
        if let Some(base_snapshot_id) = changeset_only_base_snapshot_id {
            record_changeset_only_child_output(
                parent_session,
                handler,
                request,
                plan_version,
                step,
                &base_snapshot_id,
                &step_output,
            )?;
        }
        Ok(step_output)
    }

    async fn complete_task_with_synthesis<H, A>(
        &self,
        session: &mut Session,
        request: &SequentialTaskRequest,
        plan_version: u32,
        synthesis_options: AgentRunOptions,
        handler: &mut H,
        approval_handler: &mut A,
    ) -> Result<TaskRunStatus>
    where
        H: EventHandler + Send,
        A: ApprovalHandler + Send,
    {
        let projection = session.task_state_projection();
        let task = projection
            .tasks
            .get(&request.task_id)
            .ok_or_else(|| anyhow!("task disappeared before final synthesis"))?;
        if task.final_answer.is_some() {
            append_task_run(
                session,
                handler,
                request,
                TaskRunStatus::Completed,
                Some(format!(
                    "completed plan v{plan_version} after final synthesis"
                )),
            )?;
            return Ok(TaskRunStatus::Completed);
        }

        if let Some((attempt, result)) = latest_completed_synthesis_result(task, plan_version) {
            let recovered_final_text = load_participant_final_answer(session, result)?;
            commit_task_final_answer(
                session,
                handler,
                request,
                attempt,
                &recovered_final_text,
                self.cancellation.as_ref(),
            )?;
            append_task_run(
                session,
                handler,
                request,
                TaskRunStatus::Completed,
                Some(format!(
                    "completed plan v{plan_version} after recovered synthesis"
                )),
            )?;
            return Ok(TaskRunStatus::Completed);
        }

        let attempt = begin_participant_attempt(
            session,
            handler,
            request,
            TaskParticipantPurpose::Synthesis,
            Some(plan_version),
            None,
            AgentRole::Planner,
        )?;
        let synthesis_prompt = task_synthesis_prompt(session, request, plan_version)?;
        let child_input = self.bind_cancellation(
            AgentRunInput::without_persisted_user_message(vec![ModelMessage::user(
                synthesis_prompt,
            )])
            .with_run_purpose(AgentRunPurpose::TaskSynthesis(TaskSynthesisContext {
                task_id: request.task_id.clone(),
                plan_version,
                attempt_id: attempt.attempt_id.clone(),
            }))
            .with_logical_run_id(task_participant_logical_run_id(&attempt.attempt_id)),
        );
        let output = self
            .child_runner
            .run_synthesis_session(
                session,
                TaskSynthesisSessionRunRequest {
                    task: request.clone(),
                    attempt_id: attempt.attempt_id.clone(),
                    child_session_ref: attempt.child_session_ref.clone(),
                    plan_version,
                    child_input,
                    options: synthesis_options,
                },
                handler,
                approval_handler,
            )
            .await;
        let output = match output {
            Ok(output) => output,
            Err(error) => {
                append_participant_terminal(
                    session,
                    handler,
                    &attempt,
                    TaskParticipantAttemptStatus::Failed,
                    Some(format!("final synthesis failed: {error:#}")),
                )?;
                append_task_run(
                    session,
                    handler,
                    request,
                    TaskRunStatus::Paused,
                    Some(format!(
                        "final synthesis failed and may be retried: {error:#}"
                    )),
                )?;
                return Ok(TaskRunStatus::Paused);
            }
        };
        validate_participant_output_identity(
            &attempt,
            &output.attempt_id,
            &output.child_session_ref,
        )?;
        let final_text = crate::safe_persistence_text(&output.final_text);
        if final_text.is_empty() {
            append_participant_terminal(
                session,
                handler,
                &attempt,
                TaskParticipantAttemptStatus::Failed,
                Some("final synthesis returned an empty result".to_owned()),
            )?;
            append_task_run(
                session,
                handler,
                request,
                TaskRunStatus::Paused,
                Some("final synthesis returned an empty result and may be retried".to_owned()),
            )?;
            return Ok(TaskRunStatus::Paused);
        }
        let result = participant_result_entry(
            &attempt,
            &final_text,
            Some(output.final_answer_ref),
            output.artifact_refs,
            output.outcome.changed_files,
            Vec::new(),
        )?;
        append_participant_result_and_terminal(
            session,
            handler,
            &attempt,
            result,
            TaskParticipantAttemptStatus::Completed,
            None,
        )?;
        commit_task_final_answer(
            session,
            handler,
            request,
            &attempt,
            &final_text,
            self.cancellation.as_ref(),
        )?;
        append_task_run(
            session,
            handler,
            request,
            TaskRunStatus::Completed,
            Some(format!(
                "completed plan v{plan_version} after final synthesis"
            )),
        )?;
        Ok(TaskRunStatus::Completed)
    }
}

/// Repairs a crash-interrupted task final-answer prefix without dispatching a provider request.
///
/// Synthesis output is durable in its child transcript before the parent-visible Assistant and
/// final commit are appended. This function replays that stable prefix idempotently, then closes
/// the task run. It returns `true` only when at least one missing parent record was appended.
///
/// # Errors
///
/// Returns an error when the completed synthesis result cannot be resolved or conflicts with an
/// already-written parent Assistant record.
pub fn reconcile_task_final_answer_prefix(session: &mut Session, task_id: &TaskId) -> Result<bool> {
    let projection = session.task_state_projection();
    let task = projection
        .tasks
        .get(task_id)
        .cloned()
        .ok_or_else(|| anyhow!("task is missing during final-answer reconciliation"))?;
    if task.status == TaskRunStatus::Completed {
        return Ok(false);
    }
    if !matches!(task.status, TaskRunStatus::Started | TaskRunStatus::Running) {
        bail!(
            "task final-answer recovery is not allowed from terminal or explicitly paused status {:?}",
            task.status
        );
    }
    let plan_version = task
        .latest_plan_version
        .ok_or_else(|| anyhow!("task final-answer reconciliation has no accepted plan"))?;
    let request = SequentialTaskRequest {
        task_id: task.task_id.clone(),
        parent_session_ref: task.parent_session_ref.clone(),
        objective: task.objective.clone(),
    };
    let mut handler = crate::NoopEventHandler;

    if task.final_answer.is_none() {
        let (attempt, result) = latest_completed_synthesis_result(&task, plan_version)
            .ok_or_else(|| anyhow!("task has no completed synthesis result to reconcile"))?;
        let final_text = recover_parent_or_child_final_answer(session, attempt, result)?;
        commit_task_final_answer(session, &mut handler, &request, attempt, &final_text, None)?;
    }
    append_task_run(
        session,
        &mut handler,
        &request,
        TaskRunStatus::Completed,
        Some(format!(
            "completed plan v{plan_version} after final synthesis recovery"
        )),
    )?;
    Ok(true)
}

fn begin_participant_attempt<H>(
    session: &mut Session,
    handler: &mut H,
    request: &SequentialTaskRequest,
    purpose: TaskParticipantPurpose,
    plan_version: Option<u32>,
    step_id: Option<&TaskStepId>,
    role: AgentRole,
) -> Result<TaskParticipantAttemptEntry>
where
    H: EventHandler + Send,
{
    let projection = session.task_state_projection();
    let task = projection
        .tasks
        .get(&request.task_id)
        .ok_or_else(|| anyhow!("task is missing before participant attempt admission"))?;
    if task
        .participant_attempts_for(purpose, plan_version, step_id)
        .into_iter()
        .any(|attempt| attempt.status == TaskParticipantAttemptStatus::Started)
    {
        bail!(
            "task {} has an uncertain {} participant attempt; explicit recovery is required",
            request.task_id.as_str(),
            purpose.as_str()
        );
    }
    let ordinal = task.next_participant_ordinal(purpose, plan_version, step_id);
    let attempt_id =
        task_participant_attempt_id(&request.task_id, purpose, plan_version, step_id, ordinal)?;
    let entry = TaskParticipantAttemptEntry {
        child_session_ref: task_participant_session_ref(&request.task_id, &attempt_id)?,
        attempt_id,
        task_id: request.task_id.clone(),
        purpose,
        ordinal,
        plan_version,
        step_id: step_id.cloned(),
        role,
        status: TaskParticipantAttemptStatus::Started,
        reason: None,
    };
    entry.validate_shape()?;
    append_task_control(
        session,
        handler,
        ControlEntry::TaskParticipantAttempt(entry.clone()),
    )?;
    Ok(entry)
}

fn append_participant_terminal<H>(
    session: &mut Session,
    handler: &mut H,
    attempt: &TaskParticipantAttemptEntry,
    status: TaskParticipantAttemptStatus,
    reason: Option<String>,
) -> Result<()>
where
    H: EventHandler + Send,
{
    if !status.is_terminal() {
        bail!("participant terminal append requires a terminal status");
    }
    let mut terminal = attempt.clone();
    terminal.status = status;
    terminal.reason = reason.as_deref().map(crate::safe_persistence_text);
    append_task_control(
        session,
        handler,
        ControlEntry::TaskParticipantAttempt(terminal),
    )
}

fn append_participant_result_and_terminal<H>(
    session: &mut Session,
    handler: &mut H,
    attempt: &TaskParticipantAttemptEntry,
    mut result: TaskParticipantResultEntry,
    status: TaskParticipantAttemptStatus,
    reason: Option<String>,
) -> Result<()>
where
    H: EventHandler + Send,
{
    if result.attempt_id != attempt.attempt_id || result.task_id != attempt.task_id {
        bail!("participant result identity does not match its attempt");
    }
    if !status.is_terminal() {
        bail!("participant result requires a terminal attempt status");
    }
    result.terminal_status = Some(status);
    result.validate_shape()?;
    append_task_control(
        session,
        handler,
        ControlEntry::TaskParticipantResult(result),
    )?;
    append_participant_terminal(session, handler, attempt, status, reason)
}

pub(super) fn participant_result_entry(
    attempt: &TaskParticipantAttemptEntry,
    final_text: &str,
    final_answer_ref: Option<AgentFinalAnswerRef>,
    artifact_refs: Vec<AgentArtifactRef>,
    changed_paths: Vec<String>,
    verification_refs: Vec<String>,
) -> Result<TaskParticipantResultEntry> {
    let safe_final_text = crate::safe_persistence_text(final_text);
    let summary = if safe_final_text.is_empty() {
        "participant produced no final text".to_owned()
    } else {
        bounded_task_participant_summary(&safe_final_text)
    };
    if final_answer_ref
        .as_ref()
        .is_some_and(|reference| reference.session_ref != attempt.child_session_ref)
    {
        bail!("participant final answer ref points outside its owned child session");
    }
    let summary_hash = format!("sha256:{}", hash_task_text(&summary));
    let output_hash = format!("sha256:{}", hash_task_text(&safe_final_text));
    let artifact_refs = artifact_refs
        .into_iter()
        .take(crate::TASK_PARTICIPANT_RESULT_ARTIFACT_MAX_ITEMS)
        .map(|mut artifact| {
            artifact.kind = bounded_participant_result_field(
                &artifact.kind,
                crate::TASK_PARTICIPANT_RESULT_ARTIFACT_KIND_MAX_CHARS,
            );
            artifact.path = bounded_participant_result_field(
                &artifact.path,
                crate::TASK_PARTICIPANT_RESULT_REF_MAX_CHARS,
            );
            artifact.hash = artifact.hash.as_deref().map(|hash| {
                bounded_participant_result_field(hash, crate::TASK_PARTICIPANT_RESULT_REF_MAX_CHARS)
            });
            artifact
        })
        .filter(|artifact| !artifact.kind.is_empty() && !artifact.path.is_empty())
        .collect();
    let entry = TaskParticipantResultEntry {
        attempt_id: attempt.attempt_id.clone(),
        task_id: attempt.task_id.clone(),
        summary,
        summary_hash,
        output_hash,
        terminal_status: None,
        final_answer_ref,
        artifact_refs,
        changed_paths: changed_paths
            .into_iter()
            .take(crate::TASK_PARTICIPANT_RESULT_CHANGED_PATH_MAX_ITEMS)
            .map(|path| {
                bounded_participant_result_field(
                    &path,
                    crate::TASK_PARTICIPANT_RESULT_REF_MAX_CHARS,
                )
            })
            .filter(|path| !path.is_empty())
            .collect(),
        verification_refs: verification_refs
            .into_iter()
            .take(crate::TASK_PARTICIPANT_RESULT_VERIFICATION_REF_MAX_ITEMS)
            .map(|reference| {
                bounded_participant_result_field(
                    &reference,
                    crate::TASK_PARTICIPANT_RESULT_REF_MAX_CHARS,
                )
            })
            .filter(|reference| !reference.is_empty())
            .collect(),
    };
    entry.validate_shape()?;
    Ok(entry)
}

fn bounded_participant_result_field(value: &str, max_chars: usize) -> String {
    crate::safe_persistence_text(value)
        .chars()
        .take(max_chars)
        .collect()
}

fn validate_isolated_planner_output(
    request: &SequentialTaskRequest,
    attempt: &TaskParticipantAttemptEntry,
    output: &TaskPlannerSessionRunOutput,
) -> Result<()> {
    validate_participant_output_identity(attempt, &output.attempt_id, &output.child_session_ref)?;
    let plan = &output.accepted_plan;
    if plan.task_id != request.task_id {
        bail!("isolated planner returned a plan for a different task");
    }
    if plan.status != TaskPlanStatus::Accepted || plan.steps.is_empty() {
        bail!("isolated planner did not return a non-empty accepted plan");
    }
    TaskGraphProjection::from_plan_entry(plan)?;
    Ok(())
}

fn validate_participant_output_identity(
    attempt: &TaskParticipantAttemptEntry,
    output_attempt_id: &TaskParticipantAttemptId,
    output_child_session_ref: &SessionRef,
) -> Result<()> {
    if output_attempt_id != &attempt.attempt_id {
        bail!("participant output attempt id does not match the admitted attempt");
    }
    if output_child_session_ref != &attempt.child_session_ref {
        bail!("participant output child session ref does not match the admitted attempt");
    }
    Ok(())
}

fn latest_completed_synthesis_result(
    task: &TaskRunProjection,
    plan_version: u32,
) -> Option<(&TaskParticipantAttemptEntry, &TaskParticipantResultEntry)> {
    task.participant_attempts
        .values()
        .filter(|attempt| {
            attempt.purpose == TaskParticipantPurpose::Synthesis
                && attempt.plan_version == Some(plan_version)
                && attempt.status == TaskParticipantAttemptStatus::Completed
        })
        .filter_map(|attempt| {
            task.participant_results
                .get(&attempt.attempt_id)
                .map(|result| (attempt, result))
        })
        .max_by_key(|(attempt, _)| attempt.ordinal)
}

fn load_participant_final_answer(
    parent_session: &Session,
    result: &TaskParticipantResultEntry,
) -> Result<String> {
    let reference = result
        .final_answer_ref
        .as_ref()
        .ok_or_else(|| anyhow!("completed synthesis result has no child final-answer ref"))?;
    let parent_path = parent_session.store_path().ok_or_else(|| {
        anyhow!("cannot recover synthesis final answer from an in-memory child session")
    })?;
    let parent_dir = parent_path.parent().unwrap_or_else(|| Path::new("."));
    let store = JsonlSessionStore::new(reference.session_ref.resolve(parent_dir))?;
    let child_session = Session::load_from_store(
        parent_session.provider_name(),
        parent_session.model_name(),
        store,
    )?;
    let final_text = child_session
        .entries()
        .iter()
        .find_map(|entry| match entry {
            SessionLogEntry::Assistant(message) if message.id == reference.message_id => {
                message.content.clone()
            }
            _ => None,
        })
        .ok_or_else(|| anyhow!("synthesis child final-answer ref cannot be resolved"))?;
    let safe_final_text = crate::safe_persistence_text(&final_text);
    let output_hash = format!("sha256:{}", hash_task_text(&safe_final_text));
    if output_hash != result.output_hash
        || hash_task_text(&safe_final_text) != reference.content_hash
    {
        bail!("synthesis child final answer conflicts with its durable result hashes");
    }
    Ok(safe_final_text)
}

fn recover_parent_or_child_final_answer(
    parent_session: &Session,
    attempt: &TaskParticipantAttemptEntry,
    result: &TaskParticipantResultEntry,
) -> Result<String> {
    let message_id = task_final_message_id(&attempt.task_id, &attempt.attempt_id);
    if let Some(message) = parent_session
        .entries()
        .iter()
        .find_map(|entry| match entry {
            SessionLogEntry::Assistant(message) if message.id == message_id => Some(message),
            _ => None,
        })
    {
        if message.assistant_kind != Some(AssistantMessageKind::FinalAnswer) {
            bail!("stable task final message id has a non-final Assistant kind");
        }
        let final_text =
            crate::safe_persistence_text(message.content.as_deref().unwrap_or_default());
        if format!("sha256:{}", hash_task_text(&final_text)) != result.output_hash {
            bail!("stable task final message conflicts with the synthesis result hash");
        }
        return Ok(final_text);
    }
    load_participant_final_answer(parent_session, result)
}

fn commit_task_final_answer<H>(
    session: &mut Session,
    handler: &mut H,
    request: &SequentialTaskRequest,
    attempt: &TaskParticipantAttemptEntry,
    final_text: &str,
    cancellation: Option<&RunCancellationHandle>,
) -> Result<()>
where
    H: EventHandler + Send,
{
    let final_text = crate::safe_persistence_text(final_text);
    if final_text.trim().is_empty() {
        bail!("cannot commit an empty task final answer");
    }
    let message_id = task_final_message_id(&request.task_id, &attempt.attempt_id);
    let content_hash = format!("sha256:{}", hash_task_text(&final_text));
    let projection = session.task_state_projection();
    if let Some(existing) = projection
        .tasks
        .get(&request.task_id)
        .and_then(|task| task.final_answer.as_ref())
    {
        if existing.synthesis_attempt_id != attempt.attempt_id
            || existing.plan_version != attempt.plan_version.unwrap_or_default()
            || existing.message_id != message_id
            || existing.content_hash != content_hash
        {
            bail!("task already has a conflicting committed final answer");
        }
        return Ok(());
    }

    if let Some(cancellation) = cancellation
        && !cancellation.is_naturally_finalized()
        && !cancellation.try_finalize_naturally()
    {
        bail!("run cancellation won before task final answer commit");
    }

    let existing_message = session.entries().iter().find_map(|entry| match entry {
        SessionLogEntry::Assistant(message) if message.id == message_id => Some(message),
        _ => None,
    });
    if let Some(existing) = existing_message {
        if existing.assistant_kind != Some(AssistantMessageKind::FinalAnswer)
            || existing.content.as_deref() != Some(final_text.as_str())
        {
            bail!("stable task final message id already carries conflicting content");
        }
    } else {
        let mut exact = ModelMessage::assistant_with_kind(
            Some(final_text),
            Vec::new(),
            AssistantMessageKind::FinalAnswer,
        );
        exact.id.clone_from(&message_id);
        let (message, _) = crate::project_message_for_persistence(exact)?;
        session.append_assistant_message(message.clone())?;
        handler.handle(RunEvent::AssistantMessage(message))?;
    }
    append_task_control(
        session,
        handler,
        ControlEntry::TaskFinalAnswerCommitted(TaskFinalAnswerCommittedEntry {
            task_id: request.task_id.clone(),
            plan_version: attempt
                .plan_version
                .ok_or_else(|| anyhow!("synthesis final commit is missing its plan version"))?,
            synthesis_attempt_id: attempt.attempt_id.clone(),
            message_id,
            content_hash,
        }),
    )
}

fn participant_status_from_step_status(status: TaskStepStatus) -> TaskParticipantAttemptStatus {
    match status {
        TaskStepStatus::Completed => TaskParticipantAttemptStatus::Completed,
        TaskStepStatus::Failed => TaskParticipantAttemptStatus::Failed,
        TaskStepStatus::Blocked => TaskParticipantAttemptStatus::Blocked,
        TaskStepStatus::Cancelled | TaskStepStatus::Superseded => {
            TaskParticipantAttemptStatus::Cancelled
        }
        TaskStepStatus::Interrupted => TaskParticipantAttemptStatus::Interrupted,
        TaskStepStatus::Pending | TaskStepStatus::Running => {
            TaskParticipantAttemptStatus::Interrupted
        }
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
    request: &SequentialTaskRequest,
) -> Result<bool>
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
            Some("planning started".to_owned()),
        )?;
        return Ok(false);
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
    let has_accepted_plan = task
        .latest_plan_version
        .and_then(|version| task.plans.get(&version))
        .is_some_and(|plan| plan.status == TaskPlanStatus::Accepted);
    Ok(has_accepted_plan)
}

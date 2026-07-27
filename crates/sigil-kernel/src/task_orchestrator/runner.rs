use super::*;
use crate::{RunCancellationHandle, RunEffectClass, RunEffectKind};

/// Sequential planner/executor task orchestrator.
pub struct SequentialTaskOrchestrator<R> {
    child_runner: R,
    execution_backend: Option<Arc<dyn ExecutionBackend>>,
    cancellation: Option<RunCancellationHandle>,
    max_parallel_read_steps: usize,
    max_parallel_changeset_steps: usize,
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
            max_parallel_changeset_steps: 1,
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

    /// Sets the maximum number of independent changeset-only proposals launched together.
    #[must_use]
    pub fn with_max_parallel_changeset_steps(
        mut self,
        max_parallel_changeset_steps: usize,
    ) -> Self {
        self.max_parallel_changeset_steps = max_parallel_changeset_steps.max(1);
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
            let worktree_availability = self
                .child_runner
                .planner_worktree_availability(&subagent_write_options)
                .await;
            loop {
                let projection = session.task_state_projection();
                let task = projection
                    .tasks
                    .get(&request.task_id)
                    .ok_or_else(|| anyhow!("task disappeared before planner retry admission"))?;
                if !await_pending_participant_retry(
                    task,
                    TaskParticipantPurpose::Planner,
                    None,
                    self.cancellation.as_ref(),
                )
                .await
                {
                    append_task_run(
                        session,
                        handler,
                        &request,
                        TaskRunStatus::Cancelled,
                        Some("task cancelled during planner provider retry backoff".to_owned()),
                    )?;
                    bail!("task cancelled during planner provider retry backoff");
                }
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
                        planner_prompt(&request.objective, worktree_availability),
                    )])
                    .with_task_plan_update(TaskPlanUpdateContext {
                        task_id: request.task_id.clone(),
                        max_plan_steps,
                        max_plan_versions: crate::DEFAULT_TASK_MAX_PLAN_VERSIONS,
                        worktree_availability,
                    })
                    .with_run_purpose(AgentRunPurpose::TaskPlanner(TaskPlannerContext {
                        task_id: request.task_id.clone(),
                        attempt_id: Some(attempt.attempt_id.clone()),
                    }))
                    .with_logical_run_id(task_participant_logical_run_id(&attempt.attempt_id)),
                );
                validate_scheduled_retry_input(session, &attempt, &planner_input)?;
                let planner_output = self
                    .child_runner
                    .run_planner_session(
                        session,
                        TaskPlannerSessionRunRequest {
                            task: request.clone(),
                            attempt_id: attempt.attempt_id.clone(),
                            child_session_ref: attempt.child_session_ref.clone(),
                            child_input: planner_input,
                            options: planner_options.clone(),
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
                        break;
                    }
                    Err(error) => {
                        if self.abort_participant_failure_for_cancellation(
                            session,
                            handler,
                            &attempt,
                            "task planner cancelled by the root run owner",
                        )? {
                            return Err(error);
                        }
                        if schedule_control_participant_retry(
                            session,
                            handler,
                            &request,
                            TaskParticipantPurpose::Planner,
                            None,
                            &attempt,
                            &error,
                        )? {
                            continue;
                        }
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
                // Cancellation owns the durable terminal and must observe the task as active.
                // Other failures still receive a task terminal before surfacing the error.
                if !self.cancellation_requested() {
                    append_task_run(
                        session,
                        handler,
                        &request,
                        TaskRunStatus::Failed,
                        Some(format!("task orchestration failed: {error:#}")),
                    )?;
                }
                Err(error)
            }
        }
    }

    /// Reviews promoted natural-language guidance with the planner model before continuing.
    ///
    /// The host does not classify prompt content. The isolated planner must call either
    /// `task_guidance_apply` for selected not-yet-started steps or `task_plan_update` for a new
    /// accepted plan version.
    ///
    /// # Errors
    ///
    /// Returns an error when the promotion is stale, the planner decision is inconsistent with
    /// its host binding, or the resumed task cannot be appended.
    #[allow(clippy::too_many_arguments)]
    pub async fn continue_run_with_guidance_review<H, A>(
        &self,
        session: &mut Session,
        request: SequentialTaskRequest,
        planner_options: AgentRunOptions,
        executor_options: AgentRunOptions,
        subagent_read_options: AgentRunOptions,
        subagent_write_options: AgentRunOptions,
        max_plan_steps: usize,
        guidance: String,
        promotion: TaskGuidancePromotedEntry,
        handler: &mut H,
        approval_handler: &mut A,
    ) -> Result<SequentialTaskRunOutput>
    where
        H: EventHandler + Send,
        A: ApprovalHandler + Send,
    {
        promotion.validate_for_session(session.session_scope_id())?;
        if !session.entries().iter().any(|entry| {
            matches!(
                entry,
                SessionLogEntry::Control(ControlEntry::TaskGuidancePromoted(recorded))
                    if recorded == &promotion
            )
        }) {
            bail!("task guidance review requires its exact durable promotion record");
        }
        let guidance = normalize_task_guidance(Some(guidance))
            .ok_or_else(|| anyhow!("promoted task guidance is empty"))?;
        let prompt_projection = crate::project_conversation_prompt_for_persistence(&guidance);
        if prompt_projection.prompt_hash != promotion.prompt_hash
            || prompt_projection.safe_prompt != promotion.guidance
            || prompt_projection.exact_prompt_required != promotion.exact_prompt_required
        {
            bail!("promoted task guidance no longer matches its exact prompt material");
        }
        if promotion.task_id != request.task_id {
            bail!("promoted task guidance targets a different task");
        }

        let projection = session.task_state_projection();
        let task = projection.tasks.get(&request.task_id).ok_or_else(|| {
            anyhow!(
                "task {} is not present in session",
                request.task_id.as_str()
            )
        })?;
        if matches!(
            task.status,
            TaskRunStatus::Completed | TaskRunStatus::Cancelled
        ) {
            bail!("promoted task guidance cannot revive a completed or cancelled task");
        }
        let (plan_version, steps) = latest_executable_plan(task)?;
        if promotion.plan_version != plan_version {
            bail!(
                "promoted task guidance plan v{} is stale against accepted plan v{}",
                promotion.plan_version,
                plan_version
            );
        }
        let plan = task
            .plans
            .get(&plan_version)
            .ok_or_else(|| anyhow!("accepted task plan v{plan_version} disappeared"))?;
        let accepted_plan = TaskPlanEntry {
            task_id: request.task_id.clone(),
            plan_version,
            status: plan.status,
            steps: plan.steps.clone(),
            reason: plan.reason.clone(),
        };
        let eligible_pending_step_ids = steps
            .iter()
            .filter(|step| {
                task.steps
                    .get(&(plan_version, step.step_id.clone()))
                    .is_none_or(|projected| projected.status == TaskStepStatus::Pending)
            })
            .map(|step| step.step_id.clone())
            .collect::<Vec<_>>();
        let assessment = TaskGuidanceAssessmentContext {
            queue_id: promotion.queue_id.clone(),
            task_id: request.task_id.clone(),
            plan_version,
            dispatch_run_id: promotion.dispatch_run_id.clone(),
            accepted_plan: accepted_plan.clone(),
            eligible_pending_step_ids: eligible_pending_step_ids.clone(),
        };
        assessment.validate_shape()?;
        let worktree_availability = self
            .child_runner
            .planner_worktree_availability(&subagent_write_options)
            .await;
        let assessment_prompt = task_guidance_assessment_prompt(
            &request.objective,
            &accepted_plan,
            &eligible_pending_step_ids,
            &guidance,
            worktree_availability,
        );

        loop {
            let projection = session.task_state_projection();
            let task = projection.tasks.get(&request.task_id).ok_or_else(|| {
                anyhow!("task disappeared before guidance-review retry admission")
            })?;
            if !await_pending_participant_retry(
                task,
                TaskParticipantPurpose::Planner,
                None,
                self.cancellation.as_ref(),
            )
            .await
            {
                append_task_run(
                    session,
                    handler,
                    &request,
                    TaskRunStatus::Cancelled,
                    Some("task cancelled during guidance review retry backoff".to_owned()),
                )?;
                bail!("task cancelled during guidance review retry backoff");
            }
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
                    assessment_prompt.clone(),
                )])
                .with_task_plan_update(TaskPlanUpdateContext {
                    task_id: request.task_id.clone(),
                    max_plan_steps,
                    max_plan_versions: crate::DEFAULT_TASK_MAX_PLAN_VERSIONS,
                    worktree_availability,
                })
                .with_task_guidance_assessment(assessment.clone())
                .with_run_purpose(AgentRunPurpose::TaskPlanner(TaskPlannerContext {
                    task_id: request.task_id.clone(),
                    attempt_id: Some(attempt.attempt_id.clone()),
                }))
                .with_logical_run_id(task_participant_logical_run_id(&attempt.attempt_id)),
            );
            validate_scheduled_retry_input(session, &attempt, &planner_input)?;
            let planner_output = self
                .child_runner
                .run_planner_session(
                    session,
                    TaskPlannerSessionRunRequest {
                        task: request.clone(),
                        attempt_id: attempt.attempt_id.clone(),
                        child_session_ref: attempt.child_session_ref.clone(),
                        child_input: planner_input,
                        options: planner_options.clone(),
                        discovery_options: subagent_read_options.clone(),
                    },
                    handler,
                    approval_handler,
                )
                .await;
            let output = match planner_output {
                Ok(output) => output,
                Err(error) => {
                    if self.abort_participant_failure_for_cancellation(
                        session,
                        handler,
                        &attempt,
                        "task guidance review cancelled by the root run owner",
                    )? {
                        return Err(error);
                    }
                    if schedule_control_participant_retry(
                        session,
                        handler,
                        &request,
                        TaskParticipantPurpose::Planner,
                        None,
                        &attempt,
                        &error,
                    )? {
                        continue;
                    }
                    append_participant_terminal(
                        session,
                        handler,
                        &attempt,
                        TaskParticipantAttemptStatus::Failed,
                        Some(format!("task guidance review failed: {error:#}")),
                    )?;
                    append_task_run(
                        session,
                        handler,
                        &request,
                        TaskRunStatus::Paused,
                        Some(
                            "task guidance review failed; explicit recovery is required".to_owned(),
                        ),
                    )?;
                    return Err(error);
                }
            };
            validate_isolated_planner_output(&request, &attempt, &output)?;

            let (continued_guidance, target_step_ids, summary) = if let Some(applied) =
                output.guidance_applied.as_ref()
            {
                applied.validate_against(&assessment)?;
                if output.accepted_plan != accepted_plan {
                    bail!("guidance supplement decision returned a different accepted task plan");
                }
                append_task_control(
                    session,
                    handler,
                    ControlEntry::TaskGuidanceApplied(applied.clone()),
                )?;
                (
                    Some(guidance.clone()),
                    Some(applied.target_step_ids.iter().cloned().collect()),
                    format!(
                        "applied queued guidance to {} pending step(s) in task plan v{}",
                        applied.target_step_ids.len(),
                        plan_version
                    ),
                )
            } else {
                validate_guidance_replan(
                    task,
                    plan_version,
                    &accepted_plan,
                    &output.accepted_plan,
                )?;
                let carried_steps = completed_steps_for_replan(
                    task,
                    plan_version,
                    &accepted_plan,
                    &output.accepted_plan,
                )?;
                append_task_control(
                    session,
                    handler,
                    ControlEntry::TaskPlan(output.accepted_plan.clone()),
                )?;
                for step in carried_steps {
                    append_task_control(session, handler, ControlEntry::TaskStep(step))?;
                }
                (
                    None,
                    None,
                    format!(
                        "accepted task guidance replan v{} with {} steps",
                        output.accepted_plan.plan_version,
                        output.accepted_plan.steps.len()
                    ),
                )
            };
            let result = participant_result_entry(
                &attempt,
                &summary,
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
            return self
                .continue_run_scoped(
                    session,
                    request,
                    executor_options,
                    subagent_read_options,
                    subagent_write_options,
                    continued_guidance,
                    target_step_ids,
                    handler,
                    approval_handler,
                )
                .await;
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
        self.continue_run_scoped(
            session,
            request,
            executor_options,
            subagent_read_options,
            subagent_write_options,
            guidance,
            None,
            handler,
            approval_handler,
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    async fn continue_run_scoped<H, A>(
        &self,
        session: &mut Session,
        request: SequentialTaskRequest,
        executor_options: AgentRunOptions,
        subagent_read_options: AgentRunOptions,
        subagent_write_options: AgentRunOptions,
        guidance: Option<String>,
        guidance_target_step_ids: Option<BTreeSet<TaskStepId>>,
        handler: &mut H,
        approval_handler: &mut A,
    ) -> Result<SequentialTaskRunOutput>
    where
        H: EventHandler + Send,
        A: ApprovalHandler + Send,
    {
        if !session
            .task_state_projection()
            .tasks
            .contains_key(&request.task_id)
        {
            bail!(
                "task {} is not present in session",
                request.task_id.as_str()
            );
        }
        let integration_projection = IntegrationProjection::from_entries(session.entries());
        reconcile_promoted_integration_steps(
            session,
            handler,
            &request.task_id,
            &integration_projection,
        )?;
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
        let max_scheduler_batches = steps
            .len()
            .saturating_mul(MAX_TASK_PARTICIPANT_AUTO_RETRIES.saturating_add(1))
            .saturating_add(1)
            .max(1);
        'scheduler: for _ in 0..max_scheduler_batches {
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
                self.max_parallel_changeset_steps,
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
            if !await_pending_step_retries(
                task,
                plan_version,
                &runnable.steps,
                self.cancellation.as_ref(),
            )
            .await
            {
                append_task_run(
                    session,
                    handler,
                    &request,
                    TaskRunStatus::Cancelled,
                    Some("task cancelled during provider retry backoff".to_owned()),
                )?;
                return Ok(SequentialTaskRunOutput {
                    task_id: request.task_id,
                    plan_version,
                    steps: step_outputs,
                    status: TaskRunStatus::Cancelled,
                });
            }

            let is_parallel_read_batch = runnable.steps.len() > 1
                && runnable.steps.iter().all(|step| {
                    matches!(
                        step.effective_mode(),
                        TaskStepMode::Read | TaskStepMode::Review | TaskStepMode::Verify
                    ) && step.effective_isolation() == TaskIsolationMode::SharedReadOnly
                });
            let is_parallel_isolated_write_batch = runnable.steps.len() > 1
                && runnable.steps.iter().all(|step| {
                    step.role == AgentRole::SubagentWrite
                        && step.effective_mode() == TaskStepMode::Write
                        && matches!(
                            step.effective_isolation(),
                            TaskIsolationMode::ChangesetOnly | TaskIsolationMode::Worktree
                        )
                });
            if is_parallel_read_batch || is_parallel_isolated_write_batch {
                let changeset_batch_base_snapshot_id = if is_parallel_isolated_write_batch {
                    let first_step = runnable
                        .steps
                        .first()
                        .ok_or_else(|| anyhow!("parallel changeset batch is unexpectedly empty"))?;
                    Some(capture_isolated_parent_snapshot_id(
                        session,
                        &request,
                        plan_version,
                        first_step,
                        &subagent_write_options,
                        "base",
                    )?)
                } else {
                    None
                };
                let mut batch_contexts = Vec::with_capacity(runnable.steps.len());
                let mut batch_requests = Vec::with_capacity(runnable.steps.len());
                let mut child_effects = Vec::with_capacity(runnable.steps.len());
                for step in runnable.steps {
                    let dependency_results = task_step_dependency_result_context(
                        session,
                        &request.task_id,
                        plan_version,
                        &step,
                    )?;
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
                    bind_task_step_intent_execution(
                        session,
                        &request,
                        plan_version,
                        &step,
                        &attempt,
                    )?;
                    let prompt = if step.role == AgentRole::Executor {
                        executor_step_prompt(
                            &request.objective,
                            plan_version,
                            &step,
                            dependency_results.as_deref(),
                            guidance_for_step(
                                guidance.as_deref(),
                                guidance_target_step_ids.as_ref(),
                                &step.step_id,
                            ),
                        )
                    } else {
                        subagent_step_prompt(
                            &request.objective,
                            plan_version,
                            &step,
                            dependency_results.as_deref(),
                            guidance_for_step(
                                guidance.as_deref(),
                                guidance_target_step_ids.as_ref(),
                                &step.step_id,
                            ),
                        )
                    };
                    let child_input =
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
                        .with_logical_run_id(task_participant_logical_run_id(&attempt.attempt_id));
                    let child_input = if changeset_batch_base_snapshot_id.is_some() {
                        with_changeset_only_child_contract(child_input)
                    } else {
                        child_input
                    };
                    let child_input = self.bind_cancellation(child_input);
                    validate_scheduled_retry_input(session, &attempt, &child_input)?;
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
                        isolated_base_snapshot_id: changeset_batch_base_snapshot_id.clone(),
                    });
                    batch_contexts.push((
                        step,
                        attempt,
                        step_options,
                        changeset_batch_base_snapshot_id.clone(),
                    ));
                }

                let batch_preparation = self.child_runner.prepare_child_session_batch(
                    session,
                    batch_requests,
                    handler,
                    approval_handler,
                )?;
                let settled_batch =
                    settle_task_child_session_batch_preparation(batch_preparation).await?;
                let batch_results = match settled_batch {
                    SettledTaskChildSessionBatch::Detached(commit) => {
                        commit.commit(session, handler)?
                    }
                    SettledTaskChildSessionBatch::Fallback(batch_requests) => {
                        self.child_runner
                            .run_child_session_batch(
                                session,
                                batch_requests,
                                handler,
                                approval_handler,
                            )
                            .await?
                    }
                };
                drop(child_effects);
                if batch_results.len() != batch_contexts.len() {
                    bail!(
                        "task child batch returned {} results for {} requests",
                        batch_results.len(),
                        batch_contexts.len()
                    );
                }

                let mut first_problem = None;
                let mut retry_scheduled = false;
                let mut integration_proposals = Vec::new();
                for ((step, attempt, step_options, changeset_base_snapshot_id), child_result) in
                    batch_contexts.into_iter().zip(batch_results)
                {
                    let step_output = match child_result {
                        Ok(output) => {
                            validate_participant_output_identity(
                                &attempt,
                                &output.attempt_id,
                                &output.child_session_ref,
                            )?;
                            let step_output = StepRunOutput {
                                final_text: output.final_text,
                                outcome: output.outcome,
                                final_answer_ref: output.final_answer_ref,
                                artifact_refs: output.artifact_refs,
                                changeset_proposal: output.changeset_proposal,
                                isolated_parent_snapshot_id: output.isolated_parent_snapshot_id,
                            };
                            if let Some(base_snapshot_id) = changeset_base_snapshot_id.as_deref() {
                                record_isolated_child_output(
                                    session,
                                    handler,
                                    &request,
                                    plan_version,
                                    &step,
                                    &attempt,
                                    base_snapshot_id,
                                    &step_output,
                                )?;
                                if let Some(proposal) = step_output.changeset_proposal.clone() {
                                    integration_proposals.push(TaskIntegrationProposal {
                                        step_id: step.step_id.clone(),
                                        depends_on: step.depends_on.clone(),
                                        base_snapshot_id: base_snapshot_id.to_owned(),
                                        proposal,
                                    });
                                }
                            }
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
                                step_output,
                            )
                            .await?
                        }
                        Err(error) => {
                            let Some(step_output) = self
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
                                .await?
                            else {
                                retry_scheduled = true;
                                continue;
                            };
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
                self.integrate_isolated_batch(
                    session,
                    handler,
                    &request,
                    plan_version,
                    &subagent_write_options,
                    integration_proposals,
                )
                .await?;
                if let Some((status, reason)) = first_problem {
                    append_task_run(session, handler, &request, status, Some(reason))?;
                    return Ok(SequentialTaskRunOutput {
                        task_id: request.task_id,
                        plan_version,
                        steps: step_outputs,
                        status,
                    });
                }
                if retry_scheduled {
                    continue 'scheduler;
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
                bind_task_step_intent_execution(session, &request, plan_version, &step, &attempt)?;
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
                        guidance_for_step(
                            guidance.as_deref(),
                            guidance_target_step_ids.as_ref(),
                            &step.step_id,
                        ),
                        handler,
                        approval_handler,
                    )
                    .await;
                let output = match step_run_result {
                    Ok(output) => output,
                    Err(error) => {
                        let Some(step_output) = self
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
                            .await?
                        else {
                            continue 'scheduler;
                        };
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
    ) -> Result<Option<SequentialTaskStepOutput>>
    where
        H: EventHandler + Send,
    {
        if self.abort_step_failure_for_cancellation(
            session,
            handler,
            attempt,
            write_lease_id.clone(),
        )? {
            bail!("task step cancellation won the terminal-state race: {error:#}");
        }
        if schedule_participant_retry(
            session,
            handler,
            request,
            plan_version,
            step,
            attempt,
            error,
        )? {
            return Ok(None);
        }
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
        Ok(Some(SequentialTaskStepOutput {
            step_id: step.step_id.clone(),
            status: TaskStepStatus::Failed,
            verification_verdict: readiness.evaluation.verification_verdict,
            visible_state: readiness.evaluation.visible_state,
            outcome: AgentRunOutcome::default(),
        }))
    }

    fn abort_step_failure_for_cancellation<H>(
        &self,
        session: &mut Session,
        handler: &mut H,
        attempt: &TaskParticipantAttemptEntry,
        write_lease_id: Option<WriteLeaseId>,
    ) -> Result<bool>
    where
        H: EventHandler + Send,
    {
        if !self.cancellation_requested() {
            return Ok(false);
        }
        release_task_write_lease(
            session,
            handler,
            write_lease_id,
            WriteLeaseReleaseStatus::Cancelled,
        )?;
        self.abort_participant_failure_for_cancellation(
            session,
            handler,
            attempt,
            "task step cancelled by the root run owner",
        )
    }

    fn abort_participant_failure_for_cancellation<H>(
        &self,
        session: &mut Session,
        handler: &mut H,
        attempt: &TaskParticipantAttemptEntry,
        reason: &str,
    ) -> Result<bool>
    where
        H: EventHandler + Send,
    {
        if !self.cancellation_requested() {
            return Ok(false);
        }
        append_participant_terminal(
            session,
            handler,
            attempt,
            TaskParticipantAttemptStatus::Cancelled,
            Some(reason.to_owned()),
        )?;
        Ok(true)
    }

    fn cancellation_requested(&self) -> bool {
        self.cancellation
            .as_ref()
            .is_some_and(RunCancellationHandle::is_cancel_requested)
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
        let participant_status = participant_status_from_step_output(initial_status, &output);
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
                if self.abort_step_failure_for_cancellation(
                    session,
                    handler,
                    &attempt,
                    write_lease_id.clone(),
                )? {
                    bail!("task step cancellation won the terminal-state race: {error:#}");
                }
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
            participant_status_from_step_output(initial_status, &output),
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
        let dependency_results = task_step_dependency_result_context(
            parent_session,
            &request.task_id,
            plan_version,
            step,
        )?;
        let prompt = if step.role == AgentRole::Executor {
            executor_step_prompt(
                &request.objective,
                plan_version,
                step,
                dependency_results.as_deref(),
                guidance,
            )
        } else {
            subagent_step_prompt(
                &request.objective,
                plan_version,
                step,
                dependency_results.as_deref(),
                guidance,
            )
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
        let isolated_base_snapshot_id = if matches!(
            step.effective_isolation(),
            TaskIsolationMode::ChangesetOnly | TaskIsolationMode::Worktree
        ) {
            Some(capture_isolated_parent_snapshot_id(
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
        let child_input = if step.effective_isolation() == TaskIsolationMode::ChangesetOnly {
            with_changeset_only_child_contract(child_input)
        } else {
            child_input
        };
        let child_input = self.bind_cancellation(child_input);
        validate_scheduled_retry_input(parent_session, attempt, &child_input)?;
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
                    isolated_base_snapshot_id: isolated_base_snapshot_id.clone(),
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
            isolated_parent_snapshot_id: output.isolated_parent_snapshot_id,
        };
        if let Some(base_snapshot_id) = isolated_base_snapshot_id {
            record_isolated_child_output(
                parent_session,
                handler,
                request,
                plan_version,
                step,
                attempt,
                &base_snapshot_id,
                &step_output,
            )?;
        }
        Ok(step_output)
    }

    async fn integrate_isolated_batch<H>(
        &self,
        session: &mut Session,
        handler: &mut H,
        request: &SequentialTaskRequest,
        plan_version: u32,
        options: &AgentRunOptions,
        proposals: Vec<TaskIntegrationProposal>,
    ) -> Result<()>
    where
        H: EventHandler + Send,
    {
        if proposals.len() < 2 || !self.child_runner.supports_integration_lanes() {
            return Ok(());
        }
        let changeset_by_step = proposals
            .iter()
            .map(|proposal| {
                (
                    proposal.step_id.clone(),
                    proposal.proposal.change_set.id.clone(),
                )
            })
            .collect::<BTreeMap<_, _>>();
        let proposal_specs = proposals
            .iter()
            .map(|proposal| {
                let generated_artifacts =
                    generated_integration_roots(&proposal.proposal.integration_facts);
                let depends_on = proposal
                    .depends_on
                    .iter()
                    .filter_map(|step_id| changeset_by_step.get(step_id).cloned())
                    .collect();
                IntegrationProposalSpec::from_changeset(
                    &proposal.proposal.change_set,
                    proposal.step_id.clone(),
                    proposal.base_snapshot_id.clone(),
                    depends_on,
                    generated_artifacts,
                    proposal.proposal.integration_facts.declared_effect,
                    DEFAULT_TASK_VERIFICATION_SCOPE_HASH,
                    proposal.proposal.integration_facts.clone(),
                )
            })
            .collect::<Result<Vec<_>>>()?;
        let mut stable_changeset_ids = proposal_specs
            .iter()
            .map(|proposal| proposal.change_set_id.as_str())
            .collect::<Vec<_>>();
        stable_changeset_ids.sort_unstable();
        let seed = format!(
            "{}:{}:{}",
            request.task_id.as_str(),
            plan_version,
            stable_changeset_ids.join(",")
        );
        let plan_id = IntegrationPlanId::new(format!(
            "integration-{}",
            stable_event_uuid("sigil-task-integration-plan", &seed)
        ))?;
        let plan = build_integration_plan(
            plan_id.clone(),
            request.task_id.clone(),
            plan_version,
            proposal_specs,
        )?;
        append_task_control(
            session,
            handler,
            ControlEntry::IntegrationPlanRecorded(IntegrationPlanRecorded { plan: plan.clone() }),
        )?;
        for lane in &plan.lanes {
            append_task_control(
                session,
                handler,
                ControlEntry::IntegrationLaneChanged(IntegrationLaneChanged {
                    plan_id: plan_id.clone(),
                    lane_id: lane.lane_id.clone(),
                    status: IntegrationLaneStatus::Pending,
                    candidate: None,
                    verification_check_ids: Vec::new(),
                    reason: None,
                }),
            )?;
        }
        if plan.requires_manual_review() {
            let reason = if matches!(
                plan.base_representation,
                crate::IntegrationBaseRepresentation::SnapshotWorkspace { .. }
            ) {
                "integration requires the snapshot-workspace lane; clean-ref integration was not started"
            } else {
                "integration facts are incomplete or unsupported; proposals require serial manual review"
            };
            for lane in &plan.lanes {
                append_task_control(
                    session,
                    handler,
                    ControlEntry::IntegrationLaneChanged(IntegrationLaneChanged {
                        plan_id: plan_id.clone(),
                        lane_id: lane.lane_id.clone(),
                        status: IntegrationLaneStatus::Pending,
                        candidate: None,
                        verification_check_ids: Vec::new(),
                        reason: Some(reason.to_owned()),
                    }),
                )?;
            }
            let _ = handler.handle(RunEvent::Notice(reason.to_owned()));
            return Ok(());
        }
        let output = self
            .child_runner
            .run_integration_lanes(
                session,
                TaskIntegrationRunRequest {
                    plan: plan.clone(),
                    workspace_root: options.workspace_root.clone(),
                    proposals,
                },
                handler,
            )
            .await;
        let output = match output {
            Ok(output) => output,
            Err(error) => {
                let reason = format!("physical integration plan failed: {error:#}");
                for lane in &plan.lanes {
                    append_task_control(
                        session,
                        handler,
                        ControlEntry::IntegrationLaneChanged(IntegrationLaneChanged {
                            plan_id: plan_id.clone(),
                            lane_id: lane.lane_id.clone(),
                            status: IntegrationLaneStatus::Failed,
                            candidate: None,
                            verification_check_ids: Vec::new(),
                            reason: Some(reason.clone()),
                        }),
                    )?;
                }
                let _ = handler.handle(RunEvent::Notice(reason));
                return Ok(());
            }
        };
        let expected_lane_ids = plan
            .lanes
            .iter()
            .map(|lane| lane.lane_id.clone())
            .collect::<BTreeSet<_>>();
        let observed_lane_ids = output
            .lanes
            .iter()
            .map(|lane| lane.lane_id.clone())
            .collect::<BTreeSet<_>>();
        if output.lanes.len() != expected_lane_ids.len()
            || observed_lane_ids != expected_lane_ids
            || output.lanes.iter().any(|lane| lane.plan_id != plan_id)
        {
            bail!("physical integration returned inconsistent lane identities");
        }
        append_integration_run_output(session, handler, output)
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

        if let Some(reason) =
            integration_synthesis_block_reason(session, &request.task_id, plan_version)
        {
            append_task_run(
                session,
                handler,
                request,
                TaskRunStatus::Paused,
                Some(reason.clone()),
            )?;
            let _ = handler.handle(RunEvent::Notice(reason));
            return Ok(TaskRunStatus::Paused);
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

        loop {
            let projection = session.task_state_projection();
            let task = projection
                .tasks
                .get(&request.task_id)
                .ok_or_else(|| anyhow!("task disappeared before synthesis retry admission"))?;
            if !await_pending_participant_retry(
                task,
                TaskParticipantPurpose::Synthesis,
                Some(plan_version),
                self.cancellation.as_ref(),
            )
            .await
            {
                append_task_run(
                    session,
                    handler,
                    request,
                    TaskRunStatus::Cancelled,
                    Some("task cancelled during synthesis provider retry backoff".to_owned()),
                )?;
                return Ok(TaskRunStatus::Cancelled);
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
            validate_scheduled_retry_input(session, &attempt, &child_input)?;
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
                        options: synthesis_options.clone(),
                    },
                    handler,
                    approval_handler,
                )
                .await;
            let output = match output {
                Ok(output) => output,
                Err(error) => {
                    if self.abort_participant_failure_for_cancellation(
                        session,
                        handler,
                        &attempt,
                        "task synthesis cancelled by the root run owner",
                    )? {
                        return Err(error);
                    }
                    if schedule_control_participant_retry(
                        session,
                        handler,
                        request,
                        TaskParticipantPurpose::Synthesis,
                        Some(plan_version),
                        &attempt,
                        &error,
                    )? {
                        continue;
                    }
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
            return Ok(TaskRunStatus::Completed);
        }
    }
}

pub(super) fn reconcile_promoted_integration_steps<H>(
    session: &mut Session,
    handler: &mut H,
    task_id: &TaskId,
    integration: &IntegrationProjection,
) -> Result<usize>
where
    H: EventHandler + Send,
{
    let task_projection = session.task_state_projection();
    let task = task_projection
        .tasks
        .get(task_id)
        .ok_or_else(|| anyhow!("task is missing during promoted integration reconciliation"))?;
    let mut bindings =
        BTreeMap::<TaskStepId, (IntegrationPlanId, crate::IntegrationPromotionAttemptId)>::new();
    for state in integration.plans.values().filter(|state| {
        state.recorded.plan.task_id == *task_id
            && task.latest_plan_version == Some(state.recorded.plan.plan_version)
    }) {
        let Some(attempt_id) = state.synthesis_ready_attempt() else {
            continue;
        };
        for proposal in &state.recorded.plan.proposals {
            let binding = (state.recorded.plan.plan_id.clone(), attempt_id.clone());
            if bindings
                .insert(proposal.step_id.clone(), binding.clone())
                .is_some_and(|existing| existing != binding)
            {
                bail!(
                    "task step {} belongs to multiple promoted integration plans",
                    proposal.step_id.as_str()
                );
            }
        }
    }
    if bindings.is_empty() {
        return Ok(0);
    }

    let plan_version = task
        .latest_plan_version
        .ok_or_else(|| anyhow!("promoted integration task has no accepted plan"))?;
    let plan = task
        .plans
        .get(&plan_version)
        .ok_or_else(|| anyhow!("promoted integration task plan v{plan_version} is missing"))?;
    let mut completions = Vec::new();
    for (step_id, (plan_id, attempt_id)) in bindings {
        let step = plan
            .steps
            .iter()
            .find(|step| step.step_id == step_id)
            .cloned()
            .ok_or_else(|| {
                anyhow!(
                    "promoted integration plan {} references task step {} outside accepted plan v{}",
                    plan_id.as_str(),
                    step_id.as_str(),
                    plan_version
                )
            })?;
        let projected = task
            .steps
            .get(&(plan_version, step_id.clone()))
            .ok_or_else(|| {
                anyhow!(
                    "promoted integration source step {} has no durable task status",
                    step_id.as_str()
                )
            })?;
        match projected.status {
            TaskStepStatus::Completed => {}
            TaskStepStatus::Blocked => completions.push((
                step,
                projected.summary.clone(),
                format!(
                    "integration plan {} was promoted by attempt {} and passed parent verification",
                    plan_id.as_str(),
                    attempt_id.as_str()
                ),
            )),
            status => {
                bail!(
                    "promoted integration source step {} must be blocked or completed, observed {}",
                    step_id.as_str(),
                    super::scheduler::task_step_status_label(status)
                );
            }
        }
    }
    let count = completions.len();
    for (step, summary, reason) in completions {
        append_task_step(
            session,
            handler,
            task_id,
            plan_version,
            &step,
            TaskStepStatus::Completed,
            summary,
            Some(reason),
        )?;
    }
    Ok(count)
}

fn integration_synthesis_block_reason(
    session: &Session,
    task_id: &TaskId,
    plan_version: u32,
) -> Option<String> {
    let projection = IntegrationProjection::from_entries(session.entries());
    for state in projection.plans.values().filter(|state| {
        state.recorded.plan.task_id == *task_id && state.recorded.plan.plan_version == plan_version
    }) {
        if state.inconsistent {
            return Some(format!(
                "integration state is inconsistent for plan v{plan_version}; resolve integration audit before synthesis"
            ));
        }
        if state.synthesis_ready_attempt().is_none() {
            return Some(format!(
                "integration review, promotion, and authoritative parent verification are incomplete for plan v{plan_version}"
            ));
        }
    }
    None
}

enum SettledTaskChildSessionBatch {
    Fallback(Vec<TaskChildSessionRunRequest>),
    Detached(TaskChildSessionBatchCommitEnvelope),
}

async fn settle_task_child_session_batch_preparation(
    preparation: TaskChildSessionBatchPreparation<'_>,
) -> Result<SettledTaskChildSessionBatch> {
    match preparation {
        TaskChildSessionBatchPreparation::Fallback(requests) => {
            Ok(SettledTaskChildSessionBatch::Fallback(requests))
        }
        TaskChildSessionBatchPreparation::Detached(batch_future) => batch_future
            .await
            .map(SettledTaskChildSessionBatch::Detached),
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

async fn await_pending_step_retries(
    task: &TaskRunProjection,
    plan_version: u32,
    steps: &[TaskStepSpec],
    cancellation: Option<&RunCancellationHandle>,
) -> bool {
    let not_before = steps
        .iter()
        .filter_map(|step| {
            task.pending_participant_retry(
                TaskParticipantPurpose::Step,
                Some(plan_version),
                Some(&step.step_id),
            )
        })
        .map(|schedule| schedule.not_before_unix_ms)
        .max();
    let Some(not_before) = not_before else {
        return true;
    };
    let now = unix_time_ms();
    if not_before > now {
        let sleep = tokio::time::sleep(std::time::Duration::from_millis(not_before - now));
        if let Some(cancellation) = cancellation {
            tokio::select! {
                _ = cancellation.cancelled() => return false,
                () = sleep => {}
            }
        } else {
            sleep.await;
        }
    }
    true
}

async fn await_pending_participant_retry(
    task: &TaskRunProjection,
    purpose: TaskParticipantPurpose,
    plan_version: Option<u32>,
    cancellation: Option<&RunCancellationHandle>,
) -> bool {
    let Some(schedule) = task.pending_participant_retry(purpose, plan_version, None) else {
        return true;
    };
    let now = unix_time_ms();
    if schedule.not_before_unix_ms <= now {
        return true;
    }
    let sleep = tokio::time::sleep(std::time::Duration::from_millis(
        schedule.not_before_unix_ms - now,
    ));
    if let Some(cancellation) = cancellation {
        tokio::select! {
            _ = cancellation.cancelled() => false,
            () = sleep => true,
        }
    } else {
        sleep.await;
        true
    }
}

fn validate_scheduled_retry_input(
    session: &Session,
    attempt: &TaskParticipantAttemptEntry,
    input: &AgentRunInput,
) -> Result<()> {
    let projection = session.task_state_projection();
    let Some(task) = projection.tasks.get(&attempt.task_id) else {
        if attempt.ordinal == 1 {
            return Ok(());
        }
        bail!("task disappeared while validating scheduled retry input");
    };
    let Some(schedule) = task.participant_retry_schedules.get(&attempt.attempt_id) else {
        return Ok(());
    };
    let input_hash = task_participant_input_hash(input)?;
    if input_hash != schedule.input_hash {
        bail!("scheduled task participant retry input drifted before provider dispatch");
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn schedule_participant_retry<H>(
    session: &mut Session,
    handler: &mut H,
    request: &SequentialTaskRequest,
    plan_version: u32,
    step: &TaskStepSpec,
    attempt: &TaskParticipantAttemptEntry,
    error: &anyhow::Error,
) -> Result<bool>
where
    H: EventHandler + Send,
{
    if !matches!(
        step.effective_mode(),
        TaskStepMode::Read | TaskStepMode::Review | TaskStepMode::Verify
    ) || step.effective_isolation() != TaskIsolationMode::SharedReadOnly
    {
        return Ok(false);
    }
    let Some((schedule, retry_count)) = build_participant_retry_schedule(
        session,
        request,
        TaskParticipantPurpose::Step,
        Some(plan_version),
        Some(&step.step_id),
        attempt,
        error,
    )?
    else {
        return Ok(false);
    };

    let mut terminal = attempt.clone();
    terminal.status = TaskParticipantAttemptStatus::Failed;
    terminal.reason = Some(crate::safe_persistence_text(&format!(
        "provider pressure retry scheduled after {} ms",
        schedule.retry_after_ms
    )));
    let pending = TaskStepEntry {
        task_id: request.task_id.clone(),
        plan_version,
        step_id: step.step_id.clone(),
        role: step.role,
        status: TaskStepStatus::Pending,
        title: Some(crate::safe_persistence_text(&step.title)),
        summary: None,
        reason: Some(format!(
            "provider pressure retry {} scheduled after {} ms",
            retry_count.saturating_add(1),
            schedule.retry_after_ms
        )),
    };
    append_task_controls(
        session,
        handler,
        vec![
            ControlEntry::TaskParticipantAttempt(terminal),
            ControlEntry::TaskParticipantRetryScheduled(schedule),
            ControlEntry::TaskStep(pending),
        ],
    )?;
    Ok(true)
}

fn schedule_control_participant_retry<H>(
    session: &mut Session,
    handler: &mut H,
    request: &SequentialTaskRequest,
    purpose: TaskParticipantPurpose,
    plan_version: Option<u32>,
    attempt: &TaskParticipantAttemptEntry,
    error: &anyhow::Error,
) -> Result<bool>
where
    H: EventHandler + Send,
{
    if purpose == TaskParticipantPurpose::Step {
        bail!("step retries must use the step-aware retry scheduler");
    }
    let Some((schedule, _retry_count)) = build_participant_retry_schedule(
        session,
        request,
        purpose,
        plan_version,
        None,
        attempt,
        error,
    )?
    else {
        return Ok(false);
    };
    let mut terminal = attempt.clone();
    terminal.status = TaskParticipantAttemptStatus::Failed;
    terminal.reason = Some(crate::safe_persistence_text(&format!(
        "provider pressure retry scheduled after {} ms",
        schedule.retry_after_ms
    )));
    append_task_controls(
        session,
        handler,
        vec![
            ControlEntry::TaskParticipantAttempt(terminal),
            ControlEntry::TaskParticipantRetryScheduled(schedule),
        ],
    )?;
    Ok(true)
}

fn build_participant_retry_schedule(
    session: &Session,
    request: &SequentialTaskRequest,
    purpose: TaskParticipantPurpose,
    plan_version: Option<u32>,
    step_id: Option<&TaskStepId>,
    attempt: &TaskParticipantAttemptEntry,
    error: &anyhow::Error,
) -> Result<Option<(TaskParticipantRetryScheduledEntry, usize)>> {
    let Some(retry) = error.downcast_ref::<TaskParticipantRetryError>() else {
        return Ok(None);
    };
    if attempt.purpose != purpose
        || attempt.plan_version != plan_version
        || attempt.step_id.as_ref() != step_id
    {
        bail!("task participant retry request conflicts with the failed attempt identity");
    }
    let projection = session.task_state_projection();
    let task = projection
        .tasks
        .get(&request.task_id)
        .ok_or_else(|| anyhow!("task disappeared before retry scheduling"))?;
    let retry_count = task
        .participant_retry_schedules
        .values()
        .filter(|schedule| {
            schedule.purpose == purpose
                && schedule.plan_version == plan_version
                && schedule.step_id.as_ref() == step_id
        })
        .count();
    let cumulative_wait = task.participant_retry_wait_ms(purpose, plan_version, step_id);
    if retry_count >= MAX_TASK_PARTICIPANT_AUTO_RETRIES
        || cumulative_wait.saturating_add(retry.retry_after_ms())
            > MAX_TASK_PARTICIPANT_AUTO_RETRY_WAIT_MS
    {
        return Ok(None);
    }
    let retry_ordinal = attempt.ordinal.saturating_add(1);
    let retry_attempt_id = task_participant_attempt_id(
        &request.task_id,
        purpose,
        plan_version,
        step_id,
        retry_ordinal,
    )?;
    let scheduled_at_unix_ms = unix_time_ms();
    let schedule = TaskParticipantRetryScheduledEntry {
        task_id: request.task_id.clone(),
        failed_attempt_id: attempt.attempt_id.clone(),
        retry_attempt_id,
        purpose,
        retry_ordinal,
        plan_version,
        step_id: step_id.cloned(),
        route_fingerprint: retry.route_fingerprint().to_owned(),
        input_hash: retry.input_hash().to_owned(),
        scheduled_at_unix_ms,
        not_before_unix_ms: scheduled_at_unix_ms.saturating_add(retry.retry_after_ms()),
        retry_after_ms: retry.retry_after_ms(),
        proof: retry.proof().clone(),
    };
    schedule.validate_shape()?;
    Ok(Some((schedule, retry_count)))
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
    if let Some(schedule) = task.pending_participant_retry(purpose, plan_version, step_id)
        && (schedule.retry_ordinal != ordinal || schedule.retry_attempt_id != attempt_id)
    {
        bail!("pending task participant retry identity conflicts with next attempt admission");
    }
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

fn bind_task_step_intent_execution(
    session: &Session,
    request: &SequentialTaskRequest,
    plan_version: u32,
    step: &TaskStepSpec,
    attempt: &TaskParticipantAttemptEntry,
) -> Result<Option<crate::IntentExecutionId>> {
    if step.effective_mode() != TaskStepMode::Write || step.intent_refs.is_empty() {
        return Ok(None);
    }
    let [intent_ref] = step.intent_refs.as_slice() else {
        bail!(
            "intent-bound write step {} must carry exactly one immutable intent ref",
            step.step_id.as_str()
        );
    };
    crate::append_task_intent_execution_binding(
        session,
        intent_ref.clone(),
        &request.task_id,
        plan_version,
        &step.step_id,
        &attempt.attempt_id,
    )
    .map(|outcome| outcome.execution_id)
}

fn unix_time_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| u64::try_from(duration.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or(1)
        .max(1)
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
    let normalized_final_text = safe_final_text.trim();
    let summary = if normalized_final_text.is_empty() {
        "participant produced no final text".to_owned()
    } else {
        bounded_task_participant_summary(&safe_final_text)
    };
    let summary_truncated =
        normalized_final_text.chars().count() > crate::TASK_PARTICIPANT_RESULT_SUMMARY_MAX_CHARS;
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
        summary_truncated,
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

fn guidance_for_step<'a>(
    guidance: Option<&'a str>,
    target_step_ids: Option<&BTreeSet<TaskStepId>>,
    step_id: &TaskStepId,
) -> Option<&'a str> {
    guidance.filter(|_| {
        target_step_ids
            .map(|targets| targets.contains(step_id))
            .unwrap_or(true)
    })
}

fn validate_guidance_replan(
    task: &TaskRunProjection,
    current_plan_version: u32,
    current_plan: &TaskPlanEntry,
    next_plan: &TaskPlanEntry,
) -> Result<()> {
    let expected_version = current_plan_version
        .checked_add(1)
        .ok_or_else(|| anyhow!("task plan version overflow during guidance replan"))?;
    if next_plan.task_id != current_plan.task_id {
        bail!("task guidance replan returned a plan for a different task");
    }
    if next_plan.plan_version != expected_version {
        bail!(
            "task guidance replan must produce exact next plan version {expected_version}, got {}",
            next_plan.plan_version
        );
    }
    if task.plans.contains_key(&next_plan.plan_version) {
        bail!(
            "task guidance replan version {} already exists",
            next_plan.plan_version
        );
    }
    Ok(())
}

fn completed_steps_for_replan(
    task: &TaskRunProjection,
    current_plan_version: u32,
    current_plan: &TaskPlanEntry,
    next_plan: &TaskPlanEntry,
) -> Result<Vec<TaskStepEntry>> {
    let mut carried = Vec::new();
    for step in &current_plan.steps {
        let Some(completed) = task
            .steps
            .get(&(current_plan_version, step.step_id.clone()))
            .filter(|projected| projected.status == TaskStepStatus::Completed)
        else {
            continue;
        };
        let Some(next_step) = next_plan
            .steps
            .iter()
            .find(|candidate| candidate.step_id == step.step_id)
        else {
            bail!(
                "task guidance replan omitted completed step {}",
                step.step_id.as_str()
            );
        };
        if !task_steps_semantically_equal(next_step, step) {
            bail!(
                "task guidance replan changed completed step {}",
                step.step_id.as_str()
            );
        }
        carried.push(TaskStepEntry {
            task_id: next_plan.task_id.clone(),
            plan_version: next_plan.plan_version,
            step_id: step.step_id.clone(),
            role: step.role,
            status: TaskStepStatus::Completed,
            title: Some(step.title.clone()),
            summary: completed.summary.clone(),
            reason: Some(format!(
                "completion carried forward from accepted plan v{current_plan_version}"
            )),
        });
    }
    Ok(carried)
}

fn task_steps_semantically_equal(left: &TaskStepSpec, right: &TaskStepSpec) -> bool {
    left.step_id == right.step_id
        && left.title == right.title
        && left.display_name == right.display_name
        && left.detail == right.detail
        && left.role == right.role
        && left.depends_on == right.depends_on
        && left.effective_mode() == right.effective_mode()
        && left.effective_isolation() == right.effective_isolation()
}

pub(super) fn append_integration_run_output<H>(
    session: &mut Session,
    handler: &mut H,
    output: TaskIntegrationRunOutput,
) -> Result<()>
where
    H: EventHandler + Send,
{
    for lane in output.lanes {
        append_task_control(session, handler, ControlEntry::IntegrationLaneChanged(lane))?;
    }
    if let Some(preview) = output.promotion_preview {
        preview.validate()?;
        append_task_control(
            session,
            handler,
            ControlEntry::TaskPromotionPreviewRecorded(crate::TaskPromotionPreviewRecorded {
                preview,
            }),
        )?;
    }
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

fn generated_integration_roots(facts: &crate::IntegrationProposalFacts) -> Vec<String> {
    if !facts
        .observed_effects
        .contains(&crate::IntegrationObservedEffect::SharedGeneratedRoot)
    {
        return Vec::new();
    }
    facts
        .paths
        .iter()
        .filter_map(|fact| {
            let path = Path::new(&fact.path);
            let mut root = PathBuf::new();
            for component in path.components() {
                root.push(component.as_os_str());
                if component
                    .as_os_str()
                    .to_str()
                    .is_some_and(|part| matches!(part, "generated" | "gen"))
                {
                    return Some(root.to_string_lossy().replace('\\', "/"));
                }
            }
            path.file_name()
                .and_then(|name| name.to_str())
                .filter(|name| name.contains(".generated."))
                .and_then(|_| path.parent())
                .map(|parent| parent.to_string_lossy().replace('\\', "/"))
        })
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
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

fn participant_status_from_step_output(
    status: TaskStepStatus,
    output: &StepRunOutput,
) -> TaskParticipantAttemptStatus {
    if status == TaskStepStatus::Blocked
        && output.changeset_proposal.is_some()
        && output.outcome.approval_denials == 0
        && output.outcome.tool_errors.is_empty()
    {
        TaskParticipantAttemptStatus::Completed
    } else {
        participant_status_from_step_status(status)
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

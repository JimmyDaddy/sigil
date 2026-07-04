use super::*;

/// Sequential planner/executor task orchestrator.
pub struct SequentialTaskOrchestrator<R> {
    planner: BoxedAgent,
    executor: BoxedAgent,
    child_runner: R,
    execution_backend: Option<Arc<dyn ExecutionBackend>>,
}

impl<R> SequentialTaskOrchestrator<R>
where
    R: TaskChildSessionRunner,
{
    pub fn new_with_child_runner(
        planner: BoxedAgent,
        executor: BoxedAgent,
        child_runner: R,
    ) -> Self {
        Self {
            planner,
            executor,
            child_runner,
            execution_backend: None,
        }
    }

    /// Returns an orchestrator that uses the provided backend for verification check execution.
    #[must_use]
    pub fn with_execution_backend(mut self, execution_backend: Arc<dyn ExecutionBackend>) -> Self {
        self.execution_backend = Some(execution_backend);
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
        append_task_run(
            session,
            handler,
            &request,
            TaskRunStatus::Started,
            Some("planning started".to_owned()),
        )?;
        let planner_input = AgentRunInput::user(planner_prompt(&request.objective))
            .with_task_plan_update(TaskPlanUpdateContext {
                task_id: request.task_id.clone(),
                max_plan_steps,
                max_plan_versions: crate::DEFAULT_TASK_MAX_PLAN_VERSIONS,
            });
        if let Err(error) = self
            .planner
            .run_with_approval_input(
                session,
                planner_input,
                planner_options,
                handler,
                approval_handler,
            )
            .await
        {
            append_task_run(
                session,
                handler,
                &request,
                TaskRunStatus::Failed,
                Some(format!("planner failed: {error:#}")),
            )?;
            return Err(error);
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
                [
                    &executor_options,
                    &subagent_read_options,
                    &subagent_write_options,
                ],
            )?;
            if runnable.steps.is_empty() {
                let (status, reason) = if let Some(reason) = runnable.paused_reason {
                    (TaskRunStatus::Paused, reason)
                } else {
                    (
                        TaskRunStatus::Completed,
                        format!("completed plan v{plan_version}"),
                    )
                };
                append_task_run(session, handler, &request, status, Some(reason))?;
                return Ok(SequentialTaskRunOutput {
                    task_id: request.task_id,
                    plan_version,
                    steps: step_outputs,
                    status,
                });
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
                let write_lease_id = acquire_task_write_lease(
                    session,
                    handler,
                    &request,
                    plan_version,
                    &step,
                    &step_options,
                )?;
                let step_run_result = match step.role {
                    AgentRole::Planner | AgentRole::Executor => {
                        self.run_parent_step(
                            session,
                            &request,
                            plan_version,
                            &step,
                            step_options.clone(),
                            guidance.as_deref(),
                            handler,
                            approval_handler,
                        )
                        .await
                    }
                    AgentRole::SubagentRead => {
                        self.run_child_step(
                            session,
                            &request,
                            plan_version,
                            &step,
                            step_options.clone(),
                            guidance.as_deref(),
                            handler,
                            approval_handler,
                        )
                        .await
                    }
                    AgentRole::SubagentWrite => {
                        self.run_child_step(
                            session,
                            &request,
                            plan_version,
                            &step,
                            step_options.clone(),
                            guidance.as_deref(),
                            handler,
                            approval_handler,
                        )
                        .await
                    }
                };
                let output = match step_run_result {
                    Ok(output) => output,
                    Err(error) => {
                        release_task_write_lease(
                            session,
                            handler,
                            write_lease_id,
                            WriteLeaseReleaseStatus::Interrupted,
                        )?;
                        let readiness = task_step_failure_readiness_nonblocking(
                            session,
                            &request,
                            &step,
                            &step_options,
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
                        append_cancelled_dependent_steps(
                            session,
                            handler,
                            &request.task_id,
                            plan_version,
                            &steps,
                            &step.step_id,
                            TaskStepStatus::Failed,
                        )?;
                        append_task_readiness(session, handler, readiness)?;
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
                let initial_status = step_status_from_outcome(&output);
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
                    &step_options,
                )
                .await?;
                if initial_status == TaskStepStatus::Completed
                    && task_step_auto_run_policy(session, &request, &step, &step_options)?
                        == VerificationAutoRunPolicy::TrustedOnly
                    && run_task_step_verification_checks(
                        session,
                        handler,
                        self.execution_backend.as_deref(),
                        &request,
                        &step,
                        &step_options,
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
                        &step_options,
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
                        &step_options,
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
                    Some(output.final_text.clone()),
                    step_reason_from_output(status, &output),
                )?;
                if cancels_dependent_steps(status) {
                    append_cancelled_dependent_steps(
                        session,
                        handler,
                        &request.task_id,
                        plan_version,
                        &steps,
                        &step.step_id,
                        status,
                    )?;
                }
                append_task_readiness(session, handler, readiness.clone())?;
                step_outputs.push(SequentialTaskStepOutput {
                    step_id: step.step_id.clone(),
                    status,
                    verification_verdict: readiness.evaluation.verification_verdict,
                    visible_state: readiness.evaluation.visible_state,
                    outcome: output.outcome,
                });
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
                steps: vec![step.clone()],
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
                plan_version,
                &step,
                options,
                child_input,
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
            Some(output.final_text.clone()),
            step_reason_from_output(status, &output),
        )?;
        append_task_readiness(session, handler, readiness.clone())?;
        let task_status = if status == TaskStepStatus::Completed {
            TaskRunStatus::Completed
        } else {
            task_status_from_step_status(status)
        };
        append_task_run(
            session,
            handler,
            &request,
            task_status,
            Some(if task_status == TaskRunStatus::Completed {
                format!("completed direct child session plan v{plan_version}")
            } else {
                step_terminal_reason(&step.step_id, status)
            }),
        )?;
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

    async fn run_parent_step<H, A>(
        &self,
        session: &mut Session,
        request: &SequentialTaskRequest,
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
        let executor_input =
            AgentRunInput::without_persisted_user_message(vec![ModelMessage::user(
                executor_step_prompt(&request.objective, plan_version, step, guidance),
            )]);
        let output = self
            .executor
            .run_with_approval_input(session, executor_input, options, handler, approval_handler)
            .await?;
        Ok(StepRunOutput {
            final_text: output.result.final_text,
            outcome: output.outcome,
            changeset_proposal: None,
            changeset_only_after_snapshot_id: None,
        })
    }

    pub(super) async fn run_child_step<H, A>(
        &self,
        parent_session: &mut Session,
        request: &SequentialTaskRequest,
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
        let child_input = AgentRunInput::without_persisted_user_message(vec![ModelMessage::user(
            subagent_step_prompt(&request.objective, plan_version, step, guidance),
        )]);
        self.run_child_step_with_input(
            parent_session,
            request,
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
        let output = self
            .child_runner
            .run_child_session(
                parent_session,
                TaskChildSessionRunRequest {
                    task: request.clone(),
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
        let step_output = StepRunOutput {
            final_text: output.final_text,
            outcome: output.outcome,
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
}

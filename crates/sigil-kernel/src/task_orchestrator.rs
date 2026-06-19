use std::path::Path;

use anyhow::{Result, anyhow, bail};
use sha2::{Digest, Sha256};

use crate::{
    Agent, AgentRunInput, AgentRunOptions, AgentRunOutcome, AgentRunTerminalReason,
    ApprovalHandler, EventHandler, JsonlSessionStore, ModelMessage, Provider, RunEvent, Session,
    ToolApproval, ToolCall, ToolErrorKind, ToolSpec,
    session::ControlEntry,
    task::{
        AgentRole, SessionRef, TaskChildSessionEntry, TaskChildSessionStatus, TaskId,
        TaskPlanEntry, TaskPlanStatus, TaskPlanUpdateContext, TaskRouteId, TaskRouteStatus,
        TaskRunEntry, TaskRunProjection, TaskRunStatus, TaskStepEntry, TaskStepId, TaskStepSpec,
        TaskStepStatus, TaskSubagentApprovalRouteEntry, child_session_ref,
    },
};

type BoxedAgent = Agent<Box<dyn Provider>>;

/// Request for one sequential planner/executor task run.
#[derive(Debug, Clone)]
pub struct SequentialTaskRequest {
    pub task_id: TaskId,
    pub parent_session_ref: SessionRef,
    pub objective: String,
}

/// Result of one sequential task run.
#[derive(Debug, Clone)]
pub struct SequentialTaskRunOutput {
    pub task_id: TaskId,
    pub plan_version: u32,
    pub steps: Vec<SequentialTaskStepOutput>,
    pub status: TaskRunStatus,
}

#[derive(Debug, Clone)]
pub struct SequentialTaskStepOutput {
    pub step_id: TaskStepId,
    pub status: TaskStepStatus,
    pub outcome: AgentRunOutcome,
}

/// Sequential planner/executor task orchestrator.
pub struct SequentialTaskOrchestrator {
    planner: BoxedAgent,
    executor: BoxedAgent,
    subagent_read: BoxedAgent,
    subagent_write: BoxedAgent,
}

impl SequentialTaskOrchestrator {
    pub fn new(
        planner: BoxedAgent,
        executor: BoxedAgent,
        subagent_read: BoxedAgent,
        subagent_write: BoxedAgent,
    ) -> Self {
        Self {
            planner,
            executor,
            subagent_read,
            subagent_write,
        }
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
        let pending_steps = resumable_steps(task, plan_version, &steps);
        let guidance = normalize_task_guidance(guidance);
        append_task_run(
            session,
            handler,
            &request,
            TaskRunStatus::Running,
            Some(task_continue_reason(plan_version, guidance.as_deref())),
        )?;

        let mut step_outputs = Vec::new();
        for step in pending_steps {
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
            let step_run_result = match step.role {
                AgentRole::Planner | AgentRole::Executor => {
                    self.run_parent_step(
                        session,
                        &request,
                        plan_version,
                        &step,
                        executor_options.clone(),
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
                        subagent_read_options.clone(),
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
                        subagent_write_options.clone(),
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
            let status = step_status_from_outcome(&output);
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
            step_outputs.push(SequentialTaskStepOutput {
                step_id: step.step_id.clone(),
                status,
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

        append_task_run(
            session,
            handler,
            &request,
            TaskRunStatus::Completed,
            Some(format!("completed plan v{plan_version}")),
        )?;
        Ok(SequentialTaskRunOutput {
            task_id: request.task_id,
            plan_version,
            steps: step_outputs,
            status: TaskRunStatus::Completed,
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
                        outcome: AgentRunOutcome::default(),
                    }],
                    status: TaskRunStatus::Failed,
                });
            }
        };
        let status = step_status_from_outcome(&output);
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
        })
    }

    async fn run_child_step<H, A>(
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
        let child_task_id =
            TaskId::new(format!("child_v{plan_version}_{}", step.step_id.as_str()))?;
        let child_session_ref = child_session_ref(&request.task_id, &step.step_id, &child_task_id)?;
        append_child_session(
            parent_session,
            handler,
            request,
            plan_version,
            step,
            &child_task_id,
            &child_session_ref,
            TaskChildSessionStatus::Started,
            None,
        )?;
        let mut child_session = build_child_session(parent_session, &child_session_ref)?;
        let mut route_handler = TaskApprovalRouteHandler {
            inner: approval_handler,
            parent_session,
            request,
            plan_version,
            step,
            child_session_ref: &child_session_ref,
        };
        let agent = match step.role {
            AgentRole::SubagentRead => &self.subagent_read,
            AgentRole::SubagentWrite => &self.subagent_write,
            AgentRole::Planner | AgentRole::Executor => &self.executor,
        };
        let output = match agent
            .run_with_approval_input(
                &mut child_session,
                child_input,
                options,
                handler,
                &mut route_handler,
            )
            .await
        {
            Ok(output) => output,
            Err(error) => {
                append_child_session(
                    route_handler.parent_session,
                    handler,
                    request,
                    plan_version,
                    step,
                    &child_task_id,
                    &child_session_ref,
                    TaskChildSessionStatus::Failed,
                    None,
                )?;
                return Err(error);
            }
        };
        let step_output = StepRunOutput {
            final_text: output.result.final_text,
            outcome: output.outcome,
        };
        let status = child_status_from_output(&step_output);
        let summary_hash = Some(hash_text(&step_output.final_text));
        append_child_session(
            route_handler.parent_session,
            handler,
            request,
            plan_version,
            step,
            &child_task_id,
            &child_session_ref,
            status,
            summary_hash,
        )?;
        Ok(step_output)
    }
}

struct StepRunOutput {
    final_text: String,
    outcome: AgentRunOutcome,
}

struct TaskApprovalRouteHandler<'a, A> {
    inner: &'a mut A,
    parent_session: &'a mut Session,
    request: &'a SequentialTaskRequest,
    plan_version: u32,
    step: &'a TaskStepSpec,
    child_session_ref: &'a SessionRef,
}

impl<A> ApprovalHandler for TaskApprovalRouteHandler<'_, A>
where
    A: ApprovalHandler,
{
    fn approve_tool_call(&mut self, call: &ToolCall, spec: &ToolSpec) -> Result<ToolApproval> {
        let route_id = route_id_for_call(&self.request.task_id, &self.step.step_id, &call.id)?;
        append_approval_route(
            self.parent_session,
            self.request,
            self.plan_version,
            self.step,
            self.child_session_ref,
            &route_id,
            call,
            TaskRouteStatus::Requested,
        )?;
        let approval = self.inner.approve_tool_call(call, spec)?;
        let status = match approval {
            ToolApproval::Approve => TaskRouteStatus::Resolved,
            ToolApproval::Deny { .. } => TaskRouteStatus::Rejected,
        };
        append_approval_route(
            self.parent_session,
            self.request,
            self.plan_version,
            self.step,
            self.child_session_ref,
            &route_id,
            call,
            status,
        )?;
        Ok(approval)
    }
}

fn append_task_control<H>(
    session: &mut Session,
    handler: &mut H,
    control: ControlEntry,
) -> Result<()>
where
    H: EventHandler + Send,
{
    session.append_control(control.clone())?;
    handler.handle(RunEvent::Control(control))
}

fn append_task_run<H>(
    session: &mut Session,
    handler: &mut H,
    request: &SequentialTaskRequest,
    status: TaskRunStatus,
    reason: Option<String>,
) -> Result<()>
where
    H: EventHandler + Send,
{
    append_task_control(
        session,
        handler,
        ControlEntry::TaskRun(TaskRunEntry {
            task_id: request.task_id.clone(),
            parent_session_ref: request.parent_session_ref.clone(),
            objective: request.objective.clone(),
            status,
            reason,
        }),
    )
}

fn append_task_step<H>(
    session: &mut Session,
    handler: &mut H,
    task_id: &TaskId,
    plan_version: u32,
    step: &TaskStepSpec,
    status: TaskStepStatus,
    summary: Option<String>,
    reason: Option<String>,
) -> Result<()>
where
    H: EventHandler + Send,
{
    append_task_control(
        session,
        handler,
        ControlEntry::TaskStep(TaskStepEntry {
            task_id: task_id.clone(),
            plan_version,
            step_id: step.step_id.clone(),
            role: step.role,
            status,
            title: Some(step.title.clone()),
            summary,
            reason,
        }),
    )
}

#[allow(clippy::too_many_arguments)]
fn append_child_session<H>(
    session: &mut Session,
    handler: &mut H,
    request: &SequentialTaskRequest,
    plan_version: u32,
    step: &TaskStepSpec,
    child_task_id: &TaskId,
    child_session_ref: &SessionRef,
    status: TaskChildSessionStatus,
    summary_hash: Option<String>,
) -> Result<()>
where
    H: EventHandler + Send,
{
    append_task_control(
        session,
        handler,
        ControlEntry::TaskChildSession(TaskChildSessionEntry {
            task_id: request.task_id.clone(),
            plan_version,
            step_id: step.step_id.clone(),
            child_task_id: child_task_id.clone(),
            child_session_ref: child_session_ref.clone(),
            role: step.role,
            status,
            summary_hash,
        }),
    )
}

#[allow(clippy::too_many_arguments)]
fn append_approval_route(
    session: &mut Session,
    request: &SequentialTaskRequest,
    plan_version: u32,
    step: &TaskStepSpec,
    child_session_ref: &SessionRef,
    route_id: &TaskRouteId,
    call: &ToolCall,
    status: TaskRouteStatus,
) -> Result<()> {
    session.append_control(ControlEntry::TaskSubagentApprovalRoute(
        TaskSubagentApprovalRouteEntry {
            route_id: route_id.clone(),
            task_id: request.task_id.clone(),
            plan_version,
            step_id: step.step_id.clone(),
            role: step.role,
            child_session_ref: child_session_ref.clone(),
            call_id: call.id.clone(),
            tool_name: call.name.clone(),
            status,
        },
    ))
}

fn latest_executable_plan(task: &TaskRunProjection) -> Result<(u32, Vec<TaskStepSpec>)> {
    let plan_version = task
        .latest_plan_version
        .ok_or_else(|| anyhow!("planner did not create task plan"))?;
    let plan = task
        .plans
        .get(&plan_version)
        .ok_or_else(|| anyhow!("missing projected task plan v{plan_version}"))?;
    if plan.status != TaskPlanStatus::Accepted {
        return Err(anyhow!("task plan v{plan_version} is not accepted"));
    }
    Ok((plan_version, plan.steps.clone()))
}

fn resumable_steps(
    task: &TaskRunProjection,
    plan_version: u32,
    plan_steps: &[TaskStepSpec],
) -> Vec<TaskStepSpec> {
    plan_steps
        .iter()
        .filter(|step| {
            !matches!(
                task.steps
                    .get(&(plan_version, step.step_id.clone()))
                    .map(|projected| projected.status),
                Some(TaskStepStatus::Completed)
            )
        })
        .cloned()
        .collect()
}

fn step_status_from_outcome(output: &StepRunOutput) -> TaskStepStatus {
    if output.outcome.terminal_reason == AgentRunTerminalReason::MaxTurns
        || !output.outcome.interrupted_tool_calls.is_empty()
    {
        TaskStepStatus::Interrupted
    } else if output.outcome.approval_denials > 0 || has_blocking_tool_error(&output.outcome) {
        TaskStepStatus::Blocked
    } else if !output.outcome.tool_errors.is_empty() && output.final_text.trim().is_empty() {
        TaskStepStatus::Failed
    } else {
        TaskStepStatus::Completed
    }
}

fn step_reason_from_output(status: TaskStepStatus, output: &StepRunOutput) -> Option<String> {
    let error = output.outcome.tool_errors.first()?;
    if status == TaskStepStatus::Completed {
        Some(format!("recovered tool error: {}", error.message))
    } else {
        Some(error.message.clone())
    }
}

fn task_status_from_step_status(status: TaskStepStatus) -> TaskRunStatus {
    match status {
        TaskStepStatus::Completed => TaskRunStatus::Completed,
        TaskStepStatus::Failed => TaskRunStatus::Failed,
        TaskStepStatus::Cancelled => TaskRunStatus::Cancelled,
        TaskStepStatus::Interrupted => TaskRunStatus::Interrupted,
        TaskStepStatus::Pending | TaskStepStatus::Running | TaskStepStatus::Blocked => {
            TaskRunStatus::Paused
        }
    }
}

fn step_terminal_reason(step_id: &TaskStepId, status: TaskStepStatus) -> String {
    match status {
        TaskStepStatus::Failed => format!("step {} failed", step_id.as_str()),
        TaskStepStatus::Blocked => format!("step {} blocked", step_id.as_str()),
        TaskStepStatus::Cancelled => format!("step {} cancelled", step_id.as_str()),
        TaskStepStatus::Interrupted => format!("step {} interrupted", step_id.as_str()),
        TaskStepStatus::Pending | TaskStepStatus::Running | TaskStepStatus::Completed => {
            format!("step {} stopped", step_id.as_str())
        }
    }
}

fn child_status_from_output(output: &StepRunOutput) -> TaskChildSessionStatus {
    if output.outcome.terminal_reason == AgentRunTerminalReason::MaxTurns
        || !output.outcome.interrupted_tool_calls.is_empty()
    {
        TaskChildSessionStatus::Interrupted
    } else if output.outcome.approval_denials > 0
        || has_blocking_tool_error(&output.outcome)
        || (!output.outcome.tool_errors.is_empty() && output.final_text.trim().is_empty())
    {
        TaskChildSessionStatus::Failed
    } else {
        TaskChildSessionStatus::Completed
    }
}

fn has_blocking_tool_error(outcome: &AgentRunOutcome) -> bool {
    outcome.tool_errors.iter().any(|error| {
        matches!(
            error.kind,
            ToolErrorKind::ApprovalRequired
                | ToolErrorKind::ApprovalDenied
                | ToolErrorKind::PermissionDenied
                | ToolErrorKind::PathOutsideWorkspace
                | ToolErrorKind::ExternalDirectoryRequired
        )
    })
}

fn build_child_session(
    parent_session: &Session,
    child_session_ref: &SessionRef,
) -> Result<Session> {
    if let Some(parent_path) = parent_session.store_path() {
        let parent_dir = parent_path.parent().unwrap_or_else(|| Path::new("."));
        let store = JsonlSessionStore::new(child_session_ref.resolve(parent_dir))?;
        return Session::load_from_store(
            parent_session.provider_name(),
            parent_session.model_name(),
            store,
        );
    }
    Ok(Session::new(
        parent_session.provider_name(),
        parent_session.model_name(),
    ))
}

fn route_id_for_call(task_id: &TaskId, step_id: &TaskStepId, call_id: &str) -> Result<TaskRouteId> {
    let mut hasher = Sha256::new();
    hasher.update(task_id.as_str().as_bytes());
    hasher.update(b":");
    hasher.update(step_id.as_str().as_bytes());
    hasher.update(b":");
    hasher.update(call_id.as_bytes());
    let digest = hasher.finalize();
    TaskRouteId::new(format!(
        "route_{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        digest[0], digest[1], digest[2], digest[3], digest[4], digest[5]
    ))
}

fn hash_text(value: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(value.as_bytes());
    format!("{:x}", hasher.finalize())
}

fn planner_prompt(objective: &str) -> String {
    format!(
        "Create an executable plan for this task. Call task_plan_update with an accepted plan before any execution. Do not call a task or subagent tool. To delegate verification or implementation, add plan steps with role subagent_read or subagent_write; the orchestrator will run those steps in child sessions.\n\nObjective:\n{objective}"
    )
}

fn normalize_task_guidance(guidance: Option<String>) -> Option<String> {
    guidance
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

fn task_continue_reason(plan_version: u32, guidance: Option<&str>) -> String {
    match guidance {
        Some(value) => format!(
            "continuing plan v{plan_version}; user guidance: {}",
            value.trim()
        ),
        None => format!("continuing plan v{plan_version}"),
    }
}

fn executor_step_prompt(
    objective: &str,
    plan_version: u32,
    step: &TaskStepSpec,
    guidance: Option<&str>,
) -> String {
    role_step_prompt(
        "Execute task step.",
        objective,
        plan_version,
        step,
        guidance,
    )
}

fn subagent_step_prompt(
    objective: &str,
    plan_version: u32,
    step: &TaskStepSpec,
    guidance: Option<&str>,
) -> String {
    role_step_prompt(
        "Execute this delegated subagent step in the child session. Keep output bounded and focused on the step result.",
        objective,
        plan_version,
        step,
        guidance,
    )
}

fn role_step_prompt(
    heading: &str,
    objective: &str,
    plan_version: u32,
    step: &TaskStepSpec,
    guidance: Option<&str>,
) -> String {
    let detail = step
        .detail
        .as_deref()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or("-");
    let mut prompt = format!(
        "{heading}\n\nObjective:\n{objective}\nPlan version: {plan_version}\nStep: {}\nTitle: {}\nDetail: {detail}\nRole: {}",
        step.step_id.as_str(),
        step.title,
        step.role.as_str()
    );
    if let Some(guidance) = guidance.filter(|value| !value.trim().is_empty()) {
        prompt.push_str("\n\nUser guidance for this continuation:\n");
        prompt.push_str(guidance.trim());
    }
    prompt
}

#[cfg(test)]
#[path = "tests/task_orchestrator_tests.rs"]
mod tests;

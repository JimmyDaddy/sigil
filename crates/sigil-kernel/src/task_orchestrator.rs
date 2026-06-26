use std::path::Path;

use anyhow::{Result, anyhow, bail};
use async_trait::async_trait;
use sha2::{Digest, Sha256};

use crate::{
    Agent, AgentRunInput, AgentRunOptions, AgentRunOutcome, AgentRunTerminalReason,
    ApprovalHandler, CompletionCriteria, DEFAULT_TASK_VERIFICATION_SCOPE_HASH, DurableEventType,
    EventHandler, EvidenceScope, JsonlSessionStore, ModelMessage, MutationCommitted,
    MutationReconciled, MutationResolution, Provider, ReadinessEvaluatedEntry, ReadinessInput,
    RequiredAction, RunEvent, RunStatus, Session, SessionStreamRecord, ToolApproval, ToolCall,
    ToolErrorKind, ToolSpec, VerificationCheckRunRequest, VerificationPolicy, VerificationScope,
    VerificationVerdict, VisibleCompletionState, WorkspaceKnowledge, WorkspaceMutationEvidence,
    WorkspaceTrust, build_workspace_snapshot_for_event, evaluate_readiness, run_verification_check,
    session::ControlEntry,
    stable_workspace_id,
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
    pub verification_verdict: VerificationVerdict,
    pub visible_state: VisibleCompletionState,
    pub outcome: AgentRunOutcome,
}

/// Input passed from the task orchestrator to a runtime-owned child-session runner.
#[derive(Debug, Clone)]
pub struct TaskChildSessionRunRequest {
    pub task: SequentialTaskRequest,
    pub plan_version: u32,
    pub step: TaskStepSpec,
    pub child_input: AgentRunInput,
    pub options: AgentRunOptions,
}

/// Output returned by a child-session runner after a terminal child run.
#[derive(Debug, Clone)]
pub struct TaskChildSessionRunOutput {
    pub final_text: String,
    pub outcome: AgentRunOutcome,
}

/// Runtime-neutral contract for launching task child sessions.
///
/// The kernel owns task control-plane semantics, but runtime implementations own concrete child
/// session creation, profile snapshots, provider/tool assembly, and route-aware child lifecycle.
#[async_trait]
pub trait TaskChildSessionRunner: Send + Sync {
    /// Runs one task child session and returns its bounded terminal output.
    ///
    /// # Errors
    ///
    /// Returns an error when child session creation, control-log append, approval routing, or the
    /// child agent run fails before a terminal result can be recorded.
    async fn run_child_session<H, A>(
        &self,
        parent_session: &mut Session,
        request: TaskChildSessionRunRequest,
        handler: &mut H,
        approval_handler: &mut A,
    ) -> Result<TaskChildSessionRunOutput>
    where
        H: EventHandler + Send,
        A: ApprovalHandler + Send;
}

/// Legacy in-kernel child-session runner retained for compatibility tests and non-runtime callers.
pub struct LegacyTaskChildSessionRunner {
    subagent_read: BoxedAgent,
    subagent_write: BoxedAgent,
}

impl LegacyTaskChildSessionRunner {
    pub fn new(subagent_read: BoxedAgent, subagent_write: BoxedAgent) -> Self {
        Self {
            subagent_read,
            subagent_write,
        }
    }
}

/// Sequential planner/executor task orchestrator.
pub struct SequentialTaskOrchestrator<R = LegacyTaskChildSessionRunner> {
    planner: BoxedAgent,
    executor: BoxedAgent,
    child_runner: R,
}

impl SequentialTaskOrchestrator<LegacyTaskChildSessionRunner> {
    pub fn new(
        planner: BoxedAgent,
        executor: BoxedAgent,
        subagent_read: BoxedAgent,
        subagent_write: BoxedAgent,
    ) -> Self {
        Self {
            planner,
            executor,
            child_runner: LegacyTaskChildSessionRunner::new(subagent_read, subagent_write),
        }
    }
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
                && run_task_step_verification_checks(
                    session,
                    handler,
                    &request,
                    &step,
                    &step_options,
                    &readiness,
                )?
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
        let readiness_options = options.clone();
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
            && run_task_step_verification_checks(
                session,
                handler,
                &request,
                &step,
                &readiness_options,
                &readiness,
            )?
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
        let output = self
            .child_runner
            .run_child_session(
                parent_session,
                TaskChildSessionRunRequest {
                    task: request.clone(),
                    plan_version,
                    step: step.clone(),
                    child_input,
                    options,
                },
                handler,
                approval_handler,
            )
            .await?;
        Ok(StepRunOutput {
            final_text: output.final_text,
            outcome: output.outcome,
        })
    }
}

#[derive(Clone)]
struct StepRunOutput {
    final_text: String,
    outcome: AgentRunOutcome,
}

#[async_trait]
impl TaskChildSessionRunner for LegacyTaskChildSessionRunner {
    async fn run_child_session<H, A>(
        &self,
        parent_session: &mut Session,
        request: TaskChildSessionRunRequest,
        handler: &mut H,
        approval_handler: &mut A,
    ) -> Result<TaskChildSessionRunOutput>
    where
        H: EventHandler + Send,
        A: ApprovalHandler + Send,
    {
        let TaskChildSessionRunRequest {
            task,
            plan_version,
            step,
            child_input,
            options,
        } = request;
        let child_task_id =
            TaskId::new(format!("child_v{plan_version}_{}", step.step_id.as_str()))?;
        let child_session_ref = child_session_ref(&task.task_id, &step.step_id, &child_task_id)?;
        append_child_session(
            parent_session,
            handler,
            &task,
            plan_version,
            &step,
            &child_task_id,
            &child_session_ref,
            TaskChildSessionStatus::Started,
            None,
        )?;
        let mut child_session = build_child_session(parent_session, &child_session_ref)?;
        let mut route_handler = TaskApprovalRouteHandler {
            inner: approval_handler,
            parent_session,
            request: &task,
            plan_version,
            step: &step,
            child_session_ref: &child_session_ref,
        };
        let agent = match step.role {
            AgentRole::SubagentRead => &self.subagent_read,
            AgentRole::SubagentWrite => &self.subagent_write,
            AgentRole::Planner | AgentRole::Executor => {
                bail!("task child session runner requires a subagent role")
            }
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
                    &task,
                    plan_version,
                    &step,
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
            &task,
            plan_version,
            &step,
            &child_task_id,
            &child_session_ref,
            status,
            summary_hash,
        )?;
        Ok(TaskChildSessionRunOutput {
            final_text: step_output.final_text,
            outcome: step_output.outcome,
        })
    }
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
            ToolApproval::Approve | ToolApproval::ApproveWithArgs { .. } => {
                TaskRouteStatus::Resolved
            }
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

fn append_task_readiness<H>(
    session: &mut Session,
    handler: &mut H,
    entry: ReadinessEvaluatedEntry,
) -> Result<()>
where
    H: EventHandler + Send,
{
    append_task_control(session, handler, ControlEntry::ReadinessEvaluated(entry))
}

fn run_task_step_verification_checks<H>(
    session: &mut Session,
    handler: &mut H,
    request: &SequentialTaskRequest,
    step: &TaskStepSpec,
    options: &AgentRunOptions,
    readiness: &ReadinessEvaluatedEntry,
) -> Result<bool>
where
    H: EventHandler + Send,
{
    let check_ids = readiness
        .evaluation
        .required_actions
        .iter()
        .filter_map(|action| match action {
            RequiredAction::RunCheck { check_spec_id } => Some(check_spec_id.clone()),
            _ => None,
        })
        .collect::<std::collections::BTreeSet<_>>();
    if check_ids.is_empty() {
        return Ok(false);
    }

    let projection = session.verification_state_projection();
    let step_scope = task_step_evidence_scope(&request.task_id, &step.step_id);
    let task_scope = EvidenceScope::Task(request.task_id.as_str().to_owned());
    let policy_entry = projection
        .latest_policy(&step_scope)
        .or_else(|| projection.latest_policy(&task_scope));
    let policy = policy_entry
        .map(|entry| entry.policy.clone())
        .unwrap_or_else(|| task_step_default_policy(&projection, &step_scope, &task_scope));
    let policy_hash = match policy_entry {
        Some(entry) => Some(entry.policy_hash.clone()),
        None => Some(policy.stable_hash()?),
    };
    let workspace_id = stable_workspace_id(&options.workspace_root)?;
    let trust_entry = projection.workspace_trust.get(&workspace_id);
    let workspace_trust = trust_entry
        .map(|entry| entry.trust)
        .unwrap_or(WorkspaceTrust::Unknown);
    let workspace_trust_snapshot_id = trust_entry
        .map(|entry| entry.workspace_trust_snapshot_id.clone())
        .unwrap_or_else(|| "unknown".to_owned());
    let scopes = [step_scope.clone(), task_scope];
    for check_id in check_ids {
        let check_entry = scopes
            .iter()
            .find_map(|scope| projection.check_spec(scope, &check_id))
            .ok_or_else(|| anyhow!("missing trusted verification check spec {check_id}"))?;
        let recorded = run_verification_check(
            session,
            VerificationCheckRunRequest {
                workspace_root: options.workspace_root.clone(),
                scope: step_scope.clone(),
                trusted_check: check_entry.trusted_check.clone(),
                policy: policy.clone(),
                policy_hash: policy_hash.clone(),
                workspace_trust,
                workspace_trust_snapshot_id: workspace_trust_snapshot_id.clone(),
                workspace_trust_approval_event_id: None,
                workspace_trust_sandbox_decision_id: None,
            },
        )?;
        append_task_control(
            session,
            handler,
            ControlEntry::VerificationRecorded(recorded),
        )?;
    }
    Ok(true)
}

fn task_step_readiness(
    session: &Session,
    request: &SequentialTaskRequest,
    step: &TaskStepSpec,
    status: TaskStepStatus,
    output: &StepRunOutput,
    options: &AgentRunOptions,
) -> Result<ReadinessEvaluatedEntry> {
    let scope = task_step_evidence_scope(&request.task_id, &step.step_id);
    let task_scope = EvidenceScope::Task(request.task_id.as_str().to_owned());
    let projection = session.verification_state_projection();
    let step_has_workspace_mutation = !output.outcome.changed_files.is_empty();
    let mut policy = projection
        .latest_policy(&scope)
        .map(|entry| entry.policy.clone())
        .or_else(|| {
            projection
                .latest_policy(&task_scope)
                .map(|entry| entry.policy.clone())
        })
        .unwrap_or_else(|| task_step_default_policy(&projection, &scope, &task_scope));
    if !step_has_workspace_mutation {
        policy.required_checks.clear();
        policy.completion_criteria = CompletionCriteria::NoChecksRequired;
        policy.allow_unverified_completion = true;
    }
    let policy_hash = policy.stable_hash()?;
    let mut input = ReadinessInput::new_run(run_status_from_step_status(status), policy);
    let workspace_id = stable_workspace_id(&options.workspace_root)?;
    input.workspace_trust = projection
        .workspace_trust
        .get(&workspace_id)
        .map(|entry| entry.trust)
        .unwrap_or(WorkspaceTrust::Unknown);
    let source_stream_sequence = (session.entries().len() as u64).saturating_add(1);
    if step_has_workspace_mutation {
        let snapshot_event_id = format!(
            "readiness-snapshot:{}:{}",
            request.task_id.as_str(),
            step.step_id.as_str()
        );
        let snapshot = build_workspace_snapshot_for_event(
            &options.workspace_root,
            workspace_id,
            &input.policy.verification_scope,
            0,
            snapshot_event_id,
            source_stream_sequence,
        )?;
        input.current_workspace_snapshot_id = snapshot.workspace_snapshot_id;
        input.workspace_knowledge = snapshot.workspace_knowledge;
        if let Some(evidence) = snapshot.unknown_dirty_evidence {
            input.mutations.push(evidence);
        }
    }
    if status == TaskStepStatus::Completed && !output.outcome.tool_errors.is_empty() {
        input.recovered_tool_error_event_ids = output
            .outcome
            .tool_errors
            .iter()
            .enumerate()
            .map(|(index, _)| {
                format!(
                    "task-step-recovered-tool-error:{}:{}:{}:{}",
                    request.task_id.as_str(),
                    step.step_id.as_str(),
                    source_stream_sequence,
                    index
                )
            })
            .collect();
    }
    input.verification_receipts = projection
        .receipts
        .values()
        .map(|entry| entry.receipt.clone())
        .collect();
    if step_has_workspace_mutation {
        let mut mutation_evidence =
            durable_workspace_mutation_evidence(session, &input.policy.verification_scope);
        if mutation_evidence.is_empty() {
            mutation_evidence.push(changed_files_mutation_evidence(
                request,
                step,
                &input.policy.verification_scope.scope_hash,
                input.current_workspace_snapshot_id.as_deref(),
                1,
            ));
        }
        input.mutations.extend(mutation_evidence);
    }
    if step_has_workspace_mutation && !input.workspace_knowledge.is_unknown_dirty() {
        let latest_mutation_sequence = input
            .mutations
            .iter()
            .map(|mutation| mutation.recorded_at_stream_sequence)
            .max()
            .unwrap_or(source_stream_sequence);
        input.workspace_knowledge = WorkspaceKnowledge::Dirty(latest_mutation_sequence);
    }
    let evaluation = evaluate_readiness(&input);
    Ok(ReadinessEvaluatedEntry {
        scope,
        evaluation,
        policy_hash: Some(policy_hash),
        workspace_snapshot_id: input.current_workspace_snapshot_id,
    })
}

async fn task_step_readiness_nonblocking(
    session: &Session,
    request: &SequentialTaskRequest,
    step: &TaskStepSpec,
    status: TaskStepStatus,
    output: &StepRunOutput,
    options: &AgentRunOptions,
) -> Result<ReadinessEvaluatedEntry> {
    let session_snapshot = Session::from_entries(
        session.provider_name().to_owned(),
        session.model_name().to_owned(),
        session.entries().to_vec(),
    );
    let request = request.clone();
    let step = step.clone();
    let output = output.clone();
    let options = options.clone();
    tokio::task::spawn_blocking(move || {
        task_step_readiness(
            &session_snapshot,
            &request,
            &step,
            status,
            &output,
            &options,
        )
    })
    .await
    .map_err(|error| anyhow!("task step readiness worker failed: {error}"))?
}

async fn task_step_failure_readiness_nonblocking(
    session: &Session,
    request: &SequentialTaskRequest,
    step: &TaskStepSpec,
    options: &AgentRunOptions,
) -> Result<ReadinessEvaluatedEntry> {
    let output = StepRunOutput {
        final_text: String::new(),
        outcome: AgentRunOutcome::default(),
    };
    task_step_readiness_nonblocking(
        session,
        request,
        step,
        TaskStepStatus::Failed,
        &output,
        options,
    )
    .await
}

fn task_step_evidence_scope(task_id: &TaskId, step_id: &TaskStepId) -> EvidenceScope {
    EvidenceScope::Step(format!("{}:{}", task_id.as_str(), step_id.as_str()))
}

fn task_step_verification_scope_hash() -> &'static str {
    DEFAULT_TASK_VERIFICATION_SCOPE_HASH
}

fn task_step_default_policy(
    projection: &crate::VerificationStateProjection,
    step_scope: &EvidenceScope,
    task_scope: &EvidenceScope,
) -> VerificationPolicy {
    let checks = projection
        .check_specs_for_scopes(&[step_scope.clone(), task_scope.clone()])
        .into_iter()
        .map(|entry| entry.trusted_check.check_spec.clone())
        .collect::<Vec<_>>();
    if checks.is_empty() {
        return VerificationPolicy::no_checks_required(task_step_verification_scope_hash());
    }
    VerificationPolicy {
        required_checks: checks,
        completion_criteria: CompletionCriteria::AllRequiredChecks,
        verification_scope: VerificationScope::all_tracked(task_step_verification_scope_hash()),
        sandbox_profile: crate::SandboxProfileRequirement::None,
        workspace_trust_requirement: crate::WorkspaceTrustRequirement::None,
        allow_unverified_completion: false,
        timeout_ms: None,
    }
}

fn durable_workspace_mutation_evidence(
    session: &Session,
    scope: &VerificationScope,
) -> Vec<WorkspaceMutationEvidence> {
    let Some(path) = session.store_path() else {
        return Vec::new();
    };
    let Ok(records) = JsonlSessionStore::read_event_records(path) else {
        return Vec::new();
    };
    records
        .into_iter()
        .filter_map(|record| {
            let SessionStreamRecord::Stored(event) = record else {
                return None;
            };
            match DurableEventType::from_event_type(&event.event_type) {
                Some(DurableEventType::MutationCommitted) => {
                    let payload =
                        serde_json::from_value::<MutationCommitted>(event.payload.clone()).ok()?;
                    Some(WorkspaceMutationEvidence {
                        event_id: event.event_id,
                        source_event_type: DurableEventType::MutationCommitted.as_str().to_owned(),
                        scope_hash: scope.scope_hash.clone(),
                        recorded_at_stream_sequence: event.stream_sequence,
                        from_workspace_snapshot_id: None,
                        to_workspace_snapshot_id: Some(payload.workspace_snapshot_id),
                        tool_effect: crate::ToolEffect::WorkspaceWrite,
                        unknown_dirty: false,
                    })
                }
                Some(DurableEventType::MutationReconciled) => {
                    let payload =
                        serde_json::from_value::<MutationReconciled>(event.payload.clone()).ok()?;
                    let unknown_dirty = payload.resolution == MutationResolution::MarkUnknownDirty;
                    Some(WorkspaceMutationEvidence {
                        event_id: event.event_id,
                        source_event_type: DurableEventType::MutationReconciled.as_str().to_owned(),
                        scope_hash: scope.scope_hash.clone(),
                        recorded_at_stream_sequence: event.stream_sequence,
                        from_workspace_snapshot_id: None,
                        to_workspace_snapshot_id: payload.workspace_snapshot_id,
                        tool_effect: if unknown_dirty {
                            crate::ToolEffect::Unknown
                        } else {
                            crate::ToolEffect::WorkspaceWrite
                        },
                        unknown_dirty,
                    })
                }
                Some(DurableEventType::WorkspaceMutationDetected) => {
                    Some(WorkspaceMutationEvidence {
                        event_id: event.event_id,
                        source_event_type: DurableEventType::WorkspaceMutationDetected
                            .as_str()
                            .to_owned(),
                        scope_hash: scope.scope_hash.clone(),
                        recorded_at_stream_sequence: event.stream_sequence,
                        from_workspace_snapshot_id: None,
                        to_workspace_snapshot_id: None,
                        tool_effect: crate::ToolEffect::Unknown,
                        unknown_dirty: true,
                    })
                }
                _ => None,
            }
        })
        .collect()
}

fn changed_files_mutation_evidence(
    request: &SequentialTaskRequest,
    step: &TaskStepSpec,
    scope_hash: &str,
    from_workspace_snapshot_id: Option<&str>,
    recorded_at_stream_sequence: u64,
) -> WorkspaceMutationEvidence {
    WorkspaceMutationEvidence {
        event_id: format!(
            "task-step-mutation:{}:{}",
            request.task_id.as_str(),
            step.step_id.as_str()
        ),
        source_event_type: "task_step_changed_files".to_owned(),
        scope_hash: scope_hash.to_owned(),
        recorded_at_stream_sequence,
        from_workspace_snapshot_id: from_workspace_snapshot_id.map(str::to_owned),
        to_workspace_snapshot_id: None,
        tool_effect: crate::ToolEffect::WorkspaceWrite,
        unknown_dirty: false,
    }
}

fn run_status_from_step_status(status: TaskStepStatus) -> RunStatus {
    match status {
        TaskStepStatus::Pending | TaskStepStatus::Running => RunStatus::Running,
        TaskStepStatus::Completed => RunStatus::Completed,
        TaskStepStatus::Failed => RunStatus::Failed,
        TaskStepStatus::Blocked => RunStatus::Blocked,
        TaskStepStatus::Cancelled => RunStatus::Cancelled,
        TaskStepStatus::Interrupted => RunStatus::Interrupted,
    }
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

fn step_status_after_readiness(
    status: TaskStepStatus,
    readiness: &ReadinessEvaluatedEntry,
) -> TaskStepStatus {
    if status == TaskStepStatus::Completed && !readiness.evaluation.required_actions.is_empty() {
        TaskStepStatus::Blocked
    } else {
        status
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

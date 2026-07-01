use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Component, Path, PathBuf},
    sync::Arc,
};

use anyhow::{Result, anyhow, bail};
use async_trait::async_trait;
use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::{
    Agent, AgentRunInput, AgentRunOptions, AgentRunOutcome, AgentRunTerminalReason,
    ApprovalHandler, ChangeSet, CheckPromotion, CheckpointRestored, CompletionCriteria,
    DEFAULT_TASK_VERIFICATION_SCOPE_HASH, DurableEventType, EventHandler, EvidenceScope,
    ExecutionBackend, ExecutionMutationProfile, FileType, JsonlSessionStore, MergeReviewId,
    MergeReviewRequested, ModelMessage, MutationCommitted, MutationPrepared, MutationReconciled,
    MutationResolution, MutationSubject, Provider, ReadinessEvaluatedEntry, ReadinessInput,
    RequiredAction, RunEvent, RunStatus, Session, SessionLogEntry, SessionStreamRecord,
    StoredEvent, ToolAccess, ToolApproval, ToolCall, ToolCategory, ToolErrorKind,
    ToolExecutionStatus, ToolRegistry, ToolRegistryScope, ToolResultMeta, ToolSpec,
    VerificationAutoRunPolicy, VerificationCheckRunEntry, VerificationCheckRunRequest,
    VerificationCheckRunStatus, VerificationPolicy, VerificationReceipt, VerificationScope,
    VerificationVerdict, VisibleCompletionState, WorkspaceKnowledge, WorkspaceMutationDetected,
    WorkspaceMutationEvidence, WorkspaceTrust, WriteIsolationMode, WriteLeaseAcquired,
    WriteLeaseId, WriteLeaseReleaseStatus, WriteLeaseReleased, WriteLeaseScope,
    build_workspace_snapshot_for_event, evaluate_readiness, run_verification_check,
    session::ControlEntry,
    stable_event_uuid, stable_workspace_id,
    task::{
        AgentRole, SessionRef, TaskChildSessionEntry, TaskChildSessionStatus, TaskId,
        TaskIsolationMode, TaskPlanEntry, TaskPlanStatus, TaskPlanUpdateContext,
        TaskReadyDeferredReason, TaskReadyQueueOptions, TaskRouteId, TaskRouteStatus, TaskRunEntry,
        TaskRunProjection, TaskRunStatus, TaskStepEntry, TaskStepId, TaskStepMode, TaskStepSpec,
        TaskStepStatus, TaskSubagentApprovalRouteEntry, child_session_ref,
    },
    verification_check_run_id,
};

type BoxedAgent = Agent<Box<dyn Provider>>;

const CHANGESET_ONLY_CHILD_TOOL_NAMES: &[&str] = &[
    "read_file",
    "ls",
    "glob",
    "grep",
    "code_symbols",
    "code_workspace_symbols",
    "code_definition",
    "code_references",
    "code_diagnostics",
    "load_skill",
];

/// Returns the only tool surface allowed for a `ChangesetOnly` child writer.
///
/// The child proposes a structured changeset through its final result. It must not execute
/// mutating tools such as `write_file`, `edit_file`, `delete_file`, `apply_changeset`, `bash`,
/// terminal tools, MCP tools, or plugin tools in the parent workspace.
pub fn changeset_only_child_tool_scope() -> ToolRegistryScope {
    ToolRegistryScope::from_names_and_prefixes(
        CHANGESET_ONLY_CHILD_TOOL_NAMES.iter().copied(),
        std::iter::empty::<&'static str>(),
    )
}

/// Returns a capability-filtered tool registry for `ChangesetOnly` child writers.
///
/// The initial scope is name-based for provider schema stability, but this function also validates
/// the resolved tool specs so a replaced same-name tool cannot carry write/execute/network access.
pub fn changeset_only_child_tool_registry(registry: &ToolRegistry) -> ToolRegistry {
    let scoped = registry.scoped(changeset_only_child_tool_scope());
    let safe_names = scoped
        .specs()
        .into_iter()
        .filter(changeset_only_child_tool_spec_is_safe)
        .map(|spec| spec.name)
        .collect::<Vec<_>>();
    registry
        .scoped(ToolRegistryScope::from_names_and_prefixes(
            safe_names,
            std::iter::empty::<String>(),
        ))
        .into_registry()
}

fn changeset_only_child_tool_spec_is_safe(spec: &ToolSpec) -> bool {
    spec.access == ToolAccess::Read
        && matches!(
            spec.category,
            ToolCategory::File | ToolCategory::Search | ToolCategory::Custom
        )
}

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
    pub changeset_only_base_snapshot_id: Option<String>,
}

/// Output returned by a child-session runner after a terminal child run.
#[derive(Debug, Clone)]
pub struct TaskChildSessionRunOutput {
    pub final_text: String,
    pub outcome: AgentRunOutcome,
    pub changeset_proposal: Option<TaskChildChangeSetProposal>,
    pub changeset_only_after_snapshot_id: Option<String>,
}

/// Structured output contract returned by a `ChangesetOnly` child writer.
#[derive(Debug, Clone)]
pub struct TaskChildChangeSetProposal {
    pub change_set: ChangeSet,
    pub artifact_ref: String,
    pub artifact: TaskChildChangeSetArtifact,
}

/// Reviewable artifact material emitted by a `ChangesetOnly` child writer.
#[derive(Debug, Clone)]
pub struct TaskChildChangeSetArtifact {
    pub media_type: String,
    pub content: String,
    pub content_sha256: String,
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
    execution_backend: Option<Arc<dyn ExecutionBackend>>,
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
            execution_backend: None,
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

#[derive(Clone)]
struct StepRunOutput {
    final_text: String,
    outcome: AgentRunOutcome,
    changeset_proposal: Option<TaskChildChangeSetProposal>,
    changeset_only_after_snapshot_id: Option<String>,
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
            changeset_only_base_snapshot_id,
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
        let output = match run_child_agent_for_step(
            agent,
            &mut child_session,
            child_input,
            options.clone(),
            &step,
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
        let changeset_proposal = if step.effective_isolation() == TaskIsolationMode::ChangesetOnly {
            match decode_changeset_only_child_output(&output.result.final_text) {
                Ok(proposal) => Some(proposal),
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
            }
        } else {
            None
        };
        let changeset_only_after_snapshot_id =
            if let Some(base_snapshot_id) = changeset_only_base_snapshot_id.as_deref() {
                Some(validate_changeset_only_parent_snapshot_unchanged_for_task(
                    route_handler.parent_session,
                    &task,
                    plan_version,
                    &step,
                    &options,
                    base_snapshot_id,
                )?)
            } else {
                None
            };
        let step_output = StepRunOutput {
            final_text: output.result.final_text,
            outcome: output.outcome,
            changeset_proposal,
            changeset_only_after_snapshot_id,
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
            changeset_proposal: step_output.changeset_proposal,
            changeset_only_after_snapshot_id: step_output.changeset_only_after_snapshot_id,
        })
    }
}

#[allow(clippy::too_many_arguments)]
async fn run_child_agent_for_step<H, A>(
    agent: &BoxedAgent,
    child_session: &mut Session,
    child_input: AgentRunInput,
    options: AgentRunOptions,
    step: &TaskStepSpec,
    handler: &mut H,
    approval_handler: &mut A,
) -> Result<crate::AgentRunOutput>
where
    H: EventHandler + Send,
    A: ApprovalHandler + Send,
{
    if step.effective_isolation() == TaskIsolationMode::ChangesetOnly {
        let scoped_tools = changeset_only_child_tool_registry(agent.tool_registry());
        agent
            .run_with_approval_input_and_tool_registry(
                child_session,
                child_input,
                options,
                scoped_tools,
                handler,
                approval_handler,
            )
            .await
    } else {
        agent
            .run_with_approval_input(
                child_session,
                child_input,
                options,
                handler,
                approval_handler,
            )
            .await
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

fn acquire_task_write_lease<H>(
    session: &mut Session,
    handler: &mut H,
    request: &SequentialTaskRequest,
    plan_version: u32,
    step: &TaskStepSpec,
    options: &AgentRunOptions,
) -> Result<Option<WriteLeaseId>>
where
    H: EventHandler + Send,
{
    if step.effective_mode() != TaskStepMode::Write {
        return Ok(None);
    }
    match step.effective_isolation() {
        TaskIsolationMode::SequentialWorkspaceWrite => {}
        TaskIsolationMode::SharedReadOnly => {
            bail!(
                "write task step {} cannot acquire a shared-read-only write lease",
                step.step_id.as_str()
            );
        }
        TaskIsolationMode::ChangesetOnly => {
            if step.role != AgentRole::SubagentWrite {
                bail!(
                    "changeset-only write task step {} requires a subagent_write role",
                    step.step_id.as_str()
                );
            }
            return Ok(None);
        }
        TaskIsolationMode::Worktree => {
            bail!(
                "write task step {} uses unsupported isolation mode {}",
                step.step_id.as_str(),
                step.effective_isolation().as_str()
            );
        }
    }

    let workspace_id = stable_workspace_id(&options.workspace_root)?;
    let lease_seed = format!(
        "{}:{}:{}:{}",
        request.task_id.as_str(),
        plan_version,
        step.step_id.as_str(),
        workspace_id
    );
    let lease_id = WriteLeaseId::new(format!(
        "lease-{}",
        stable_event_uuid("sigil-write-lease", &lease_seed)
    ))?;
    let entry = WriteLeaseAcquired {
        lease_id: lease_id.clone(),
        workspace_id,
        owner_agent_id: task_step_owner_agent_id(request, plan_version, step),
        isolation_mode: WriteIsolationMode::SharedWorkspaceExclusive,
        scope: WriteLeaseScope::Workspace,
    };
    session
        .write_isolation_projection()
        .validate_can_acquire_shared_workspace_lease(&entry)?;
    append_task_control(session, handler, ControlEntry::WriteLeaseAcquired(entry))?;
    Ok(Some(lease_id))
}

fn release_task_write_lease<H>(
    session: &mut Session,
    handler: &mut H,
    lease_id: Option<WriteLeaseId>,
    status: WriteLeaseReleaseStatus,
) -> Result<()>
where
    H: EventHandler + Send,
{
    let Some(lease_id) = lease_id else {
        return Ok(());
    };
    append_task_control(
        session,
        handler,
        ControlEntry::WriteLeaseReleased(WriteLeaseReleased { lease_id, status }),
    )
}

fn write_lease_release_status_from_step_status(status: TaskStepStatus) -> WriteLeaseReleaseStatus {
    match status {
        TaskStepStatus::Cancelled => WriteLeaseReleaseStatus::Cancelled,
        TaskStepStatus::Interrupted => WriteLeaseReleaseStatus::Interrupted,
        TaskStepStatus::Failed | TaskStepStatus::Superseded => WriteLeaseReleaseStatus::Stale,
        TaskStepStatus::Pending | TaskStepStatus::Running => WriteLeaseReleaseStatus::Interrupted,
        TaskStepStatus::Completed | TaskStepStatus::Blocked => WriteLeaseReleaseStatus::Completed,
    }
}

fn with_changeset_only_child_contract(mut input: AgentRunInput) -> AgentRunInput {
    input
        .transient_context
        .push(ModelMessage::system(changeset_only_child_contract_prompt()));
    input
}

fn changeset_only_child_contract_prompt() -> &'static str {
    r#"This delegated write step uses changeset-only isolation.

You must not modify files, run shell commands, use terminal tools, call apply_changeset, or call any MCP/plugin tool.

Return the proposed edit as structured JSON only. Use a raw JSON object or a fenced block tagged sigil_changeset. The schema is:

```sigil_changeset
{
  "change_set": {
    "id": "change-brief-stable-id",
    "title": "short user-facing title",
    "summary": "what the change would do",
    "risk": "low",
    "files": [
      {
        "path": "relative/path",
        "action": "update",
        "risk": "low",
        "additions": 0,
        "deletions": 0
      }
    ],
    "validations": []
  },
  "artifact": {
    "media_type": "text/x-diff",
    "content": "reviewable patch, diff, or exact change artifact content"
  }
}
```

Do not claim the changes were applied. They will be reviewed and applied by the parent session later."#
}

fn capture_changeset_only_parent_snapshot_id(
    session: &Session,
    request: &SequentialTaskRequest,
    plan_version: u32,
    step: &TaskStepSpec,
    options: &AgentRunOptions,
    label: &str,
) -> Result<String> {
    if step.role != AgentRole::SubagentWrite || step.effective_mode() != TaskStepMode::Write {
        bail!(
            "changeset-only task step {} requires a subagent_write write step",
            step.step_id.as_str()
        );
    }
    let scope = VerificationScope::all_tracked(task_step_verification_scope_hash());
    let workspace_id = stable_workspace_id(&options.workspace_root)?;
    let seed = format!(
        "{}:{}:{}:{}:{}",
        request.task_id.as_str(),
        plan_version,
        step.step_id.as_str(),
        workspace_id,
        label
    );
    let source_event_id = format!(
        "changeset-only-{label}-snapshot-{}",
        stable_event_uuid("sigil-changeset-only-snapshot", &seed)
    );
    let snapshot = build_workspace_snapshot_for_event(
        &options.workspace_root,
        workspace_id,
        &scope,
        0,
        source_event_id,
        session.next_stream_sequence_hint().unwrap_or(1),
    )?;
    snapshot.workspace_snapshot_id.ok_or_else(|| {
        anyhow!(
            "changeset-only task step {} cannot bind {label} parent workspace snapshot",
            step.step_id.as_str()
        )
    })
}

pub fn validate_changeset_only_parent_snapshot_unchanged_for_task(
    session: &Session,
    request: &SequentialTaskRequest,
    plan_version: u32,
    step: &TaskStepSpec,
    options: &AgentRunOptions,
    base_snapshot_id: &str,
) -> Result<String> {
    let after_snapshot_id = capture_changeset_only_parent_snapshot_id(
        session,
        request,
        plan_version,
        step,
        options,
        "after",
    )?;
    if after_snapshot_id != base_snapshot_id {
        bail!(
            "changeset-only task step {} changed parent workspace snapshot",
            step.step_id.as_str()
        );
    }
    Ok(after_snapshot_id)
}

#[allow(clippy::too_many_arguments)]
fn record_changeset_only_child_output<H>(
    session: &mut Session,
    handler: &mut H,
    request: &SequentialTaskRequest,
    plan_version: u32,
    step: &TaskStepSpec,
    base_snapshot_id: &str,
    output: &StepRunOutput,
) -> Result<()>
where
    H: EventHandler + Send,
{
    if !output.outcome.changed_files.is_empty() {
        bail!(
            "changeset-only task step {} mutated parent workspace files: {}",
            step.step_id.as_str(),
            output.outcome.changed_files.join(", ")
        );
    }
    let parent_snapshot_id = output
        .changeset_only_after_snapshot_id
        .as_deref()
        .ok_or_else(|| {
            anyhow!(
                "changeset-only task step {} missing validated parent snapshot",
                step.step_id.as_str()
            )
        })?;
    let proposal = output.changeset_proposal.as_ref().ok_or_else(|| {
        anyhow!(
            "changeset-only task step {} did not return a structured changeset proposal",
            step.step_id.as_str()
        )
    })?;
    let touched_subjects = changeset_touched_subjects(&proposal.change_set);
    append_task_control(
        session,
        handler,
        ControlEntry::ChangeSetProposed(proposal.change_set.clone()),
    )?;
    append_task_control(
        session,
        handler,
        ControlEntry::IsolatedChangeSetProduced(crate::IsolatedChangeSetProduced {
            changeset_id: proposal.change_set.id.clone(),
            owner_agent_id: task_step_owner_agent_id(request, plan_version, step),
            base_snapshot_id: base_snapshot_id.to_owned(),
            child_snapshot_id: None,
            source_isolation: WriteIsolationMode::ChangesetOnly,
            artifact_ref: Some(proposal.artifact_ref.clone()),
            touched_subjects,
        }),
    )?;
    append_task_control(
        session,
        handler,
        ControlEntry::MergeReviewRequested(MergeReviewRequested {
            review_id: changeset_only_merge_review_id(
                request,
                plan_version,
                step,
                &proposal.change_set.id,
            )?,
            changeset_id: proposal.change_set.id.clone(),
            parent_workspace_snapshot_id: parent_snapshot_id.to_owned(),
        }),
    )
}

fn changeset_only_merge_review_id(
    request: &SequentialTaskRequest,
    plan_version: u32,
    step: &TaskStepSpec,
    change_set_id: &crate::ChangeSetId,
) -> Result<MergeReviewId> {
    let seed = format!(
        "{}:{}:{}:{}",
        request.task_id.as_str(),
        plan_version,
        step.step_id.as_str(),
        change_set_id.as_str()
    );
    MergeReviewId::new(format!(
        "review-{}",
        stable_event_uuid("sigil-merge-review", &seed)
    ))
}

fn task_step_owner_agent_id(
    request: &SequentialTaskRequest,
    plan_version: u32,
    step: &TaskStepSpec,
) -> String {
    format!(
        "task:{}:v{}:{}",
        request.task_id.as_str(),
        plan_version,
        step.step_id.as_str()
    )
}

/// Decodes the strict structured output expected from a changeset-only child writer.
///
/// # Errors
///
/// Returns an error when the final output is not raw JSON or a `sigil_changeset` fenced JSON
/// block, or when the decoded changeset is empty or contains unsafe paths.
pub fn decode_changeset_only_child_output(final_text: &str) -> Result<TaskChildChangeSetProposal> {
    let json_text = extract_changeset_only_json(final_text).ok_or_else(|| {
        anyhow!("changeset-only child output must be raw JSON or a sigil_changeset fenced block")
    })?;
    let envelope: TaskChildChangeSetProposalEnvelope = serde_json::from_str(json_text)
        .map_err(|error| anyhow!("invalid changeset-only child output JSON: {error}"))?;
    let proposal = envelope.into_proposal()?;
    validate_changeset_only_proposal(&proposal.change_set)?;
    Ok(proposal)
}

#[derive(Deserialize)]
struct TaskChildChangeSetProposalEnvelope {
    #[serde(alias = "changeset")]
    change_set: ChangeSet,
    artifact: TaskChildChangeSetArtifactWire,
}

#[derive(Deserialize)]
struct TaskChildChangeSetArtifactWire {
    media_type: String,
    content: String,
}

impl TaskChildChangeSetProposalEnvelope {
    fn into_proposal(self) -> Result<TaskChildChangeSetProposal> {
        let media_type = self.artifact.media_type.trim();
        if media_type.is_empty() {
            bail!(
                "changeset-only proposal {} artifact media_type must be non-empty",
                self.change_set.id.as_str()
            );
        }
        let content = self.artifact.content;
        if content.trim().is_empty() {
            bail!(
                "changeset-only proposal {} artifact content must be non-empty",
                self.change_set.id.as_str()
            );
        }
        let content_sha256 = format!("{:x}", Sha256::digest(content.as_bytes()));
        Ok(TaskChildChangeSetProposal {
            change_set: self.change_set,
            artifact_ref: format!("inline:sha256:{content_sha256}"),
            artifact: TaskChildChangeSetArtifact {
                media_type: media_type.to_owned(),
                content,
                content_sha256,
            },
        })
    }
}

fn extract_changeset_only_json(final_text: &str) -> Option<&str> {
    let trimmed = final_text.trim();
    if trimmed.starts_with('{') {
        return Some(trimmed);
    }
    let marker = "```sigil_changeset";
    let start = trimmed.find(marker)? + marker.len();
    let after_marker = trimmed[start..]
        .strip_prefix("\r\n")
        .or_else(|| trimmed[start..].strip_prefix('\n'))
        .unwrap_or(&trimmed[start..]);
    let end = after_marker.find("```")?;
    Some(after_marker[..end].trim())
}

fn validate_changeset_only_proposal(change_set: &ChangeSet) -> Result<()> {
    if change_set.files.is_empty() {
        bail!(
            "changeset-only proposal {} must include at least one touched file",
            change_set.id.as_str()
        );
    }
    for file in &change_set.files {
        validate_changeset_path(&file.path)?;
        if let Some(previous_path) = &file.previous_path {
            validate_changeset_path(previous_path)?;
        }
    }
    Ok(())
}

fn validate_changeset_path(path: &str) -> Result<()> {
    let trimmed = path.trim();
    if trimmed.is_empty() {
        bail!("changeset proposal file path cannot be empty");
    }
    let path = Path::new(trimmed);
    if path.is_absolute()
        || path
            .components()
            .any(|component| matches!(component, Component::ParentDir))
    {
        bail!("changeset proposal file path must stay inside the workspace: {trimmed}");
    }
    Ok(())
}

fn changeset_touched_subjects(change_set: &ChangeSet) -> Vec<MutationSubject> {
    let mut subjects = Vec::new();
    for file in &change_set.files {
        push_file_subject(&mut subjects, &file.path);
        if let Some(previous_path) = &file.previous_path {
            push_file_subject(&mut subjects, previous_path);
        }
    }
    subjects
}

fn push_file_subject(subjects: &mut Vec<MutationSubject>, path: &str) {
    let subject = MutationSubject::File {
        path: PathBuf::from(path),
        file_type: FileType::File,
    };
    if !subjects.contains(&subject) {
        subjects.push(subject);
    }
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

async fn run_task_step_verification_checks<H>(
    session: &mut Session,
    handler: &mut H,
    execution_backend: Option<&dyn ExecutionBackend>,
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
    let execution_backend = execution_backend
        .ok_or_else(|| anyhow!("verification check execution requires an execution backend"))?;

    let projection = session.verification_state_projection();
    let step_scope = task_step_evidence_scope(&request.task_id, &step.step_id);
    let task_scope = EvidenceScope::Task(request.task_id.as_str().to_owned());
    let workspace_id = stable_workspace_id(&options.workspace_root)?;
    let workspace_scope = EvidenceScope::Workspace(workspace_id.clone());
    let policy_entry = projection
        .latest_policy(&step_scope)
        .or_else(|| projection.latest_policy(&task_scope));
    let policy = policy_entry
        .map(|entry| entry.policy.clone())
        .unwrap_or_else(|| {
            task_step_default_policy(&projection, &step_scope, &task_scope, &workspace_scope)
        });
    let policy_hash = match policy_entry {
        Some(entry) => Some(entry.policy_hash.clone()),
        None => Some(policy.stable_hash()?),
    };
    let trust_entry = projection.workspace_trust.get(&workspace_id);
    let workspace_trust = trust_entry
        .map(|entry| entry.trust)
        .unwrap_or(WorkspaceTrust::Unknown);
    let workspace_trust_snapshot_id = trust_entry
        .map(|entry| entry.workspace_trust_snapshot_id.clone())
        .unwrap_or_else(|| "unknown".to_owned());
    let scopes = [step_scope.clone(), task_scope, workspace_scope];
    for check_id in check_ids {
        let check_entry = scopes
            .iter()
            .find_map(|scope| projection.check_spec(scope, &check_id))
            .ok_or_else(|| anyhow!("missing trusted verification check spec {check_id}"))?;
        let check_spec = &check_entry.trusted_check.check_spec;
        let run_id = verification_check_run_id(
            &step_scope,
            check_spec,
            policy_hash.as_deref(),
            readiness.workspace_snapshot_id.as_deref(),
            session.next_stream_sequence_hint()?,
        )?;
        append_task_control(
            session,
            handler,
            ControlEntry::VerificationCheckRun(
                VerificationCheckRunEntry::new(
                    run_id.clone(),
                    step_scope.clone(),
                    check_spec,
                    VerificationCheckRunStatus::Queued,
                )
                .with_timeout_ms(policy.timeout_ms),
            ),
        )?;
        append_task_control(
            session,
            handler,
            ControlEntry::VerificationCheckRun(
                VerificationCheckRunEntry::new(
                    run_id.clone(),
                    step_scope.clone(),
                    check_spec,
                    VerificationCheckRunStatus::Running,
                )
                .with_timeout_ms(policy.timeout_ms),
            ),
        )?;
        let recorded = match run_verification_check(
            session,
            execution_backend,
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
        )
        .await
        {
            Ok(recorded) => recorded,
            Err(error) => {
                append_task_control(
                    session,
                    handler,
                    ControlEntry::VerificationCheckRun(
                        VerificationCheckRunEntry::new(
                            run_id,
                            step_scope.clone(),
                            check_spec,
                            VerificationCheckRunStatus::Errored,
                        )
                        .with_timeout_ms(policy.timeout_ms)
                        .with_error(error.to_string()),
                    ),
                )?;
                return Err(error);
            }
        };
        let recorded_receipt = recorded.receipt.clone();
        append_task_control(
            session,
            handler,
            ControlEntry::VerificationRecorded(recorded),
        )?;
        append_task_control(
            session,
            handler,
            ControlEntry::VerificationCheckRun(
                VerificationCheckRunEntry::new(
                    run_id,
                    step_scope.clone(),
                    check_spec,
                    VerificationCheckRunStatus::Running,
                )
                .with_timeout_ms(policy.timeout_ms)
                .with_terminal_receipt(&recorded_receipt),
            ),
        )?;
    }
    Ok(true)
}

fn task_step_auto_run_policy(
    session: &Session,
    request: &SequentialTaskRequest,
    step: &TaskStepSpec,
    options: &AgentRunOptions,
) -> Result<VerificationAutoRunPolicy> {
    let projection = session.verification_state_projection();
    let step_scope = task_step_evidence_scope(&request.task_id, &step.step_id);
    let task_scope = EvidenceScope::Task(request.task_id.as_str().to_owned());
    let workspace_id = stable_workspace_id(&options.workspace_root)?;
    let workspace_scope = EvidenceScope::Workspace(workspace_id);
    Ok(projection
        .latest_policy(&step_scope)
        .or_else(|| projection.latest_policy(&task_scope))
        .map(|entry| entry.policy.auto_run)
        .unwrap_or_else(|| {
            task_step_default_policy(&projection, &step_scope, &task_scope, &workspace_scope)
                .auto_run
        }))
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
    let workspace_id = stable_workspace_id(&options.workspace_root)?;
    let workspace_scope = EvidenceScope::Workspace(workspace_id.clone());
    let projection = session.verification_state_projection();
    let source_stream_sequence = session.next_stream_sequence_hint().unwrap_or(1);
    let mut policy = projection
        .latest_policy(&scope)
        .map(|entry| entry.policy.clone())
        .or_else(|| {
            projection
                .latest_policy(&task_scope)
                .map(|entry| entry.policy.clone())
        })
        .unwrap_or_else(|| {
            task_step_default_policy(&projection, &scope, &task_scope, &workspace_scope)
        });
    let baseline_policy_hash = policy.stable_hash()?;
    let latest_successful_verification_sequence = latest_relevant_successful_verification_sequence(
        &projection,
        &[scope.clone(), task_scope.clone()],
        &policy,
        &baseline_policy_hash,
    );
    let mut durable_mutation_evidence = match durable_workspace_mutation_evidence(
        session,
        &request.task_id,
        &VerificationScope::all_tracked(task_step_verification_scope_hash()),
        &output.outcome.tool_call_ids,
        latest_successful_verification_sequence,
    ) {
        Ok(evidence) => evidence,
        Err(_) => vec![durable_mutation_replay_failed_evidence(
            request,
            step,
            task_step_verification_scope_hash(),
            source_stream_sequence,
        )],
    };
    let step_has_workspace_mutation =
        !output.outcome.changed_files.is_empty() || !durable_mutation_evidence.is_empty();
    if !step_has_workspace_mutation {
        policy.required_checks.clear();
        policy.completion_criteria = CompletionCriteria::NoChecksRequired;
        policy.allow_unverified_completion = true;
    }
    let policy_hash = policy.stable_hash()?;
    let mut input = ReadinessInput::new_run(run_status_from_step_status(status), policy);
    input.workspace_trust = projection
        .workspace_trust
        .get(&workspace_id)
        .map(|entry| entry.trust)
        .unwrap_or(WorkspaceTrust::Unknown);
    let trust_ids = check_scope_trust_ids(
        &projection,
        &[scope.clone(), task_scope.clone(), workspace_scope.clone()],
    );
    input.workspace_trust_approval_event_id = trust_ids.approval_event_id;
    input.workspace_trust_sandbox_decision_id = trust_ids.sandbox_decision_id;
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
    input.verification_receipts = relevant_verification_receipts(
        &projection,
        &[scope.clone(), task_scope.clone()],
        &input.policy,
        &policy_hash,
    );
    if step_has_workspace_mutation {
        if durable_mutation_evidence.is_empty() {
            durable_mutation_evidence.push(changed_files_mutation_evidence(
                request,
                step,
                &input.policy.verification_scope.scope_hash,
                input.current_workspace_snapshot_id.as_deref(),
                1,
            ));
        }
        input.mutations.extend(durable_mutation_evidence);
    }
    if input
        .mutations
        .iter()
        .any(|mutation| mutation.unknown_dirty)
    {
        input.workspace_knowledge = WorkspaceKnowledge::UnknownDirty;
    } else if step_has_workspace_mutation && !input.workspace_knowledge.is_unknown_dirty() {
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
    let mut session_snapshot = Session::from_entries(
        session.provider_name().to_owned(),
        session.model_name().to_owned(),
        session.entries().to_vec(),
    );
    if let Some(store_path) = session.store_path() {
        session_snapshot = session_snapshot.with_store(JsonlSessionStore::new(store_path)?);
    }
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
        changeset_proposal: None,
        changeset_only_after_snapshot_id: None,
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
    workspace_scope: &EvidenceScope,
) -> VerificationPolicy {
    let check_entries = projection
        .check_specs_for_scopes(&[
            step_scope.clone(),
            task_scope.clone(),
            workspace_scope.clone(),
        ])
        .into_iter()
        .collect::<Vec<_>>();
    let checks = check_entries
        .iter()
        .map(|entry| entry.trusted_check.check_spec.clone())
        .collect::<Vec<_>>();
    if checks.is_empty() {
        return VerificationPolicy::no_checks_required(task_step_verification_scope_hash());
    }
    let workspace_trust_requirement = check_entries_workspace_trust_requirement(&check_entries);
    VerificationPolicy {
        required_checks: checks,
        completion_criteria: CompletionCriteria::AllRequiredChecks,
        verification_scope: VerificationScope::all_tracked(task_step_verification_scope_hash()),
        sandbox_profile: crate::SandboxProfileRequirement::None,
        workspace_trust_requirement,
        allow_unverified_completion: false,
        timeout_ms: None,
        auto_run: crate::VerificationAutoRunPolicy::Manual,
    }
}

struct CheckScopeTrustIds {
    approval_event_id: Option<String>,
    sandbox_decision_id: Option<String>,
}

fn check_scope_trust_ids(
    projection: &crate::VerificationStateProjection,
    scopes: &[EvidenceScope],
) -> CheckScopeTrustIds {
    let mut approval_event_id = None;
    let mut sandbox_decision_id = None;
    for entry in projection.check_specs_for_scopes(scopes) {
        approval_event_id =
            approval_event_id.or_else(|| entry.trusted_check.approval_event_id.clone());
        sandbox_decision_id =
            sandbox_decision_id.or_else(|| entry.trusted_check.sandbox_decision_id.clone());
    }
    CheckScopeTrustIds {
        approval_event_id,
        sandbox_decision_id,
    }
}

fn check_entries_workspace_trust_requirement(
    check_entries: &[&crate::CheckSpecRecordedEntry],
) -> crate::WorkspaceTrustRequirement {
    if check_entries.iter().any(|entry| {
        matches!(
            entry.trusted_check.promoted_by,
            CheckPromotion::WorkspaceTrusted { .. }
        )
    }) {
        return crate::WorkspaceTrustRequirement::Trusted;
    }
    if check_entries.iter().any(|entry| {
        matches!(
            entry.trusted_check.promoted_by,
            CheckPromotion::UserApproved { .. } | CheckPromotion::Sandboxed { .. }
        )
    }) {
        return crate::WorkspaceTrustRequirement::ApprovalOrSandbox;
    }
    crate::WorkspaceTrustRequirement::None
}

fn relevant_verification_receipts(
    projection: &crate::VerificationStateProjection,
    scopes: &[EvidenceScope],
    policy: &VerificationPolicy,
    policy_hash: &str,
) -> Vec<VerificationReceipt> {
    projection
        .receipts
        .values()
        .filter(|entry| {
            scopes
                .iter()
                .any(|scope| scope == &entry.receipt.receipt.scope)
        })
        .filter(|entry| entry.receipt.receipt.policy_hash.as_deref() == Some(policy_hash))
        .filter(|entry| {
            entry.receipt.binding.verification_scope_hash == policy.verification_scope.scope_hash
        })
        .filter(|entry| {
            policy.required_checks.iter().any(|check| {
                check.check_spec_id == entry.receipt.check_spec_id
                    && check.check_spec_hash == entry.receipt.binding.check_spec_hash
            })
        })
        .map(|entry| entry.receipt.clone())
        .collect()
}

fn latest_relevant_successful_verification_sequence(
    projection: &crate::VerificationStateProjection,
    scopes: &[EvidenceScope],
    policy: &VerificationPolicy,
    policy_hash: &str,
) -> u64 {
    relevant_verification_receipts(projection, scopes, policy, policy_hash)
        .into_iter()
        .filter(|receipt| receipt.check_status == crate::ReceiptStatus::Succeeded)
        .map(|receipt| receipt.receipt.recorded_at_stream_sequence)
        .max()
        .unwrap_or(0)
}

fn durable_workspace_mutation_evidence(
    session: &Session,
    task_id: &TaskId,
    scope: &VerificationScope,
    tool_call_ids: &[String],
    latest_successful_verification_sequence: u64,
) -> Result<Vec<WorkspaceMutationEvidence>> {
    let Some(path) = session.store_path() else {
        return Ok(Vec::new());
    };
    let records = JsonlSessionStore::read_event_records(path)?;
    let baseline_sequence = latest_successful_verification_sequence.max(
        task_started_stream_sequence(&records, task_id)
            .unwrap_or(0)
            .saturating_sub(1),
    );
    let mut prepared_tool_calls = BTreeMap::<String, Option<String>>::new();
    for record in &records {
        let SessionStreamRecord::Stored(event) = record else {
            continue;
        };
        if DurableEventType::from_event_type(&event.event_type)
            == Some(DurableEventType::MutationPrepared)
            && let Ok(payload) = serde_json::from_value::<MutationPrepared>(event.payload.clone())
        {
            prepared_tool_calls.insert(payload.operation_id, payload.tool_call_id);
        }
    }
    let running_evidence = running_execution_mutation_evidence(&records, scope);
    let mut evidence = records
        .into_iter()
        .filter_map(|record| {
            let SessionStreamRecord::Stored(event) = record else {
                return None;
            };
            match DurableEventType::from_event_type(&event.event_type) {
                Some(DurableEventType::MutationCommitted) => {
                    let payload =
                        serde_json::from_value::<MutationCommitted>(event.payload.clone()).ok()?;
                    if !mutation_matches_tool_call(
                        &payload.operation_id,
                        &prepared_tool_calls,
                        tool_call_ids,
                        event.stream_sequence,
                        baseline_sequence,
                    ) {
                        return None;
                    }
                    Some(WorkspaceMutationEvidence {
                        event_id: event.event_id,
                        source_event_type: DurableEventType::MutationCommitted.as_str().to_owned(),
                        source_label: None,
                        recovery_hint: None,
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
                    if !mutation_matches_tool_call(
                        &payload.operation_id,
                        &prepared_tool_calls,
                        tool_call_ids,
                        event.stream_sequence,
                        baseline_sequence,
                    ) {
                        return None;
                    }
                    let unknown_dirty = payload.resolution == MutationResolution::MarkUnknownDirty;
                    Some(WorkspaceMutationEvidence {
                        event_id: event.event_id,
                        source_event_type: DurableEventType::MutationReconciled.as_str().to_owned(),
                        source_label: None,
                        recovery_hint: None,
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
                Some(DurableEventType::CheckpointRestored) => {
                    let payload =
                        serde_json::from_value::<CheckpointRestored>(event.payload.clone()).ok()?;
                    if !mutation_detection_matches_filter(
                        payload.tool_call_id.as_deref(),
                        tool_call_ids,
                        event.stream_sequence,
                        baseline_sequence,
                    ) {
                        return None;
                    }
                    Some(WorkspaceMutationEvidence {
                        event_id: event.event_id,
                        source_event_type: DurableEventType::CheckpointRestored.as_str().to_owned(),
                        source_label: None,
                        recovery_hint: None,
                        scope_hash: scope.scope_hash.clone(),
                        recorded_at_stream_sequence: event.stream_sequence,
                        from_workspace_snapshot_id: None,
                        to_workspace_snapshot_id: Some(payload.workspace_snapshot_id),
                        tool_effect: crate::ToolEffect::WorkspaceWrite,
                        unknown_dirty: false,
                    })
                }
                Some(DurableEventType::WorkspaceMutationDetected) => {
                    if let Ok(payload) =
                        serde_json::from_value::<WorkspaceMutationDetected>(event.payload.clone())
                    {
                        if !mutation_detection_matches_filter(
                            payload.tool_call_id.as_deref(),
                            tool_call_ids,
                            event.stream_sequence,
                            baseline_sequence,
                        ) {
                            return None;
                        }
                        return Some(WorkspaceMutationEvidence::from_detected_event(
                            event.event_id,
                            event.stream_sequence,
                            payload,
                        ));
                    }
                    Some(WorkspaceMutationEvidence {
                        event_id: event.event_id,
                        source_event_type: DurableEventType::WorkspaceMutationDetected
                            .as_str()
                            .to_owned(),
                        source_label: None,
                        recovery_hint: None,
                        scope_hash: scope.scope_hash.clone(),
                        recorded_at_stream_sequence: event.stream_sequence,
                        from_workspace_snapshot_id: None,
                        to_workspace_snapshot_id: None,
                        tool_effect: crate::ToolEffect::Unknown,
                        unknown_dirty: true,
                    })
                }
                Some(DurableEventType::ChildChangesetMerged)
                | Some(DurableEventType::AgentMergeApplied) => {
                    if event.stream_sequence <= baseline_sequence {
                        return None;
                    }
                    Some(merge_workspace_mutation_evidence(&event, scope))
                }
                _ => None,
            }
        })
        .collect::<Vec<_>>();
    evidence.extend(running_evidence);
    evidence.sort_by_key(|entry| entry.recorded_at_stream_sequence);
    Ok(evidence)
}

#[derive(Debug, Clone)]
struct RunningExecutionProfile {
    profile: ExecutionMutationProfile,
    event_id: String,
    stream_sequence: u64,
}

#[derive(Debug, Clone)]
struct ActiveTerminalTask {
    event_id: String,
    stream_sequence: u64,
}

fn running_execution_mutation_evidence(
    records: &[SessionStreamRecord],
    scope: &VerificationScope,
) -> Vec<WorkspaceMutationEvidence> {
    let mut open_profiles = BTreeMap::<String, RunningExecutionProfile>::new();
    let mut terminal_profiles = BTreeMap::<String, RunningExecutionProfile>::new();
    let mut active_terminals = BTreeMap::<String, ActiveTerminalTask>::new();

    for record in records {
        let SessionStreamRecord::Stored(event) = record else {
            continue;
        };
        let Some(entry) = session_entry_from_event(event) else {
            continue;
        };
        match entry {
            SessionLogEntry::Control(ControlEntry::ToolExecution(execution)) => {
                if execution.status == ToolExecutionStatus::Started {
                    if let Some(profile) =
                        execution_mutation_profile_from_metadata(&execution.metadata)
                    {
                        open_profiles.insert(
                            execution.call_id.clone(),
                            RunningExecutionProfile {
                                profile,
                                event_id: event.event_id.clone(),
                                stream_sequence: event.stream_sequence,
                            },
                        );
                    }
                    continue;
                }

                if let Some(task_id) = terminal_task_id_from_tool_metadata(&execution.metadata)
                    && let Some(profile) = open_profiles.get(&execution.call_id)
                {
                    terminal_profiles.insert(task_id, profile.clone());
                }
                open_profiles.remove(&execution.call_id);
            }
            SessionLogEntry::Control(ControlEntry::TerminalTask(entry)) => {
                let task_id = entry.handle.task_id.as_str().to_owned();
                if entry.status.is_active() {
                    active_terminals.insert(
                        task_id,
                        ActiveTerminalTask {
                            event_id: event.event_id.clone(),
                            stream_sequence: event.stream_sequence,
                        },
                    );
                } else {
                    active_terminals.remove(&task_id);
                }
            }
            SessionLogEntry::User(_)
            | SessionLogEntry::Assistant(_)
            | SessionLogEntry::ToolResult(_)
            | SessionLogEntry::Control(_) => {}
        }
    }

    let mut emitted_call_ids = BTreeSet::<String>::new();
    let mut evidence = Vec::new();
    for (call_id, running) in open_profiles {
        if !running.profile.effect.may_mutate_workspace() {
            continue;
        }
        emitted_call_ids.insert(call_id);
        evidence.push(running_profile_evidence(
            &running,
            scope,
            "running_tool_execution",
        ));
    }

    for (task_id, active) in active_terminals {
        let Some(running) = terminal_profiles.get(&task_id) else {
            continue;
        };
        if emitted_call_ids.contains(&running.profile.tool_call_id)
            || !running.profile.effect.may_mutate_workspace()
        {
            continue;
        }
        let mut terminal_running = running.clone();
        terminal_running.event_id = active.event_id.clone();
        terminal_running.stream_sequence = active.stream_sequence;
        evidence.push(running_profile_evidence(
            &terminal_running,
            scope,
            "running_terminal_task",
        ));
    }

    evidence
}

fn running_profile_evidence(
    running: &RunningExecutionProfile,
    scope: &VerificationScope,
    source_event_type: &str,
) -> WorkspaceMutationEvidence {
    let scope_hash = if running.profile.scan_scope_hash.is_empty() {
        scope.scope_hash.clone()
    } else {
        running.profile.scan_scope_hash.clone()
    };
    WorkspaceMutationEvidence {
        event_id: running.event_id.clone(),
        source_event_type: source_event_type.to_owned(),
        source_label: None,
        recovery_hint: None,
        scope_hash,
        recorded_at_stream_sequence: running.stream_sequence,
        from_workspace_snapshot_id: running.profile.pre_execution_snapshot_id.clone(),
        to_workspace_snapshot_id: None,
        tool_effect: running.profile.effect,
        unknown_dirty: true,
    }
}

fn session_entry_from_event(event: &StoredEvent) -> Option<SessionLogEntry> {
    event
        .payload
        .get("session_log_entry")
        .cloned()
        .and_then(|value| serde_json::from_value::<SessionLogEntry>(value).ok())
}

fn execution_mutation_profile_from_metadata(
    metadata: &ToolResultMeta,
) -> Option<ExecutionMutationProfile> {
    metadata
        .details
        .get("execution_mutation_profile")
        .cloned()
        .and_then(|value| serde_json::from_value(value).ok())
}

fn terminal_task_id_from_tool_metadata(metadata: &ToolResultMeta) -> Option<String> {
    metadata
        .details
        .get("task_id")
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned)
}

fn merge_workspace_mutation_evidence(
    event: &StoredEvent,
    scope: &VerificationScope,
) -> WorkspaceMutationEvidence {
    let from_workspace_snapshot_id = first_payload_string(
        &event.payload,
        &[
            "from_workspace_snapshot_id",
            "parent_workspace_snapshot_before_id",
            "before_workspace_snapshot_id",
        ],
    );
    let to_workspace_snapshot_id = first_payload_string(
        &event.payload,
        &[
            "to_workspace_snapshot_id",
            "parent_workspace_snapshot_after_id",
            "parent_workspace_snapshot_id",
            "workspace_snapshot_id",
        ],
    );
    WorkspaceMutationEvidence {
        event_id: event.event_id.clone(),
        source_event_type: event.event_type.clone(),
        source_label: None,
        recovery_hint: None,
        scope_hash: scope.scope_hash.clone(),
        recorded_at_stream_sequence: event.stream_sequence,
        from_workspace_snapshot_id,
        unknown_dirty: to_workspace_snapshot_id.is_none(),
        to_workspace_snapshot_id,
        tool_effect: crate::ToolEffect::WorkspaceWrite,
    }
}

fn first_payload_string(payload: &serde_json::Value, keys: &[&str]) -> Option<String> {
    keys.iter()
        .find_map(|key| payload.get(*key).and_then(|value| value.as_str()))
        .map(str::to_owned)
}

fn mutation_matches_tool_call(
    operation_id: &str,
    prepared_tool_calls: &BTreeMap<String, Option<String>>,
    tool_call_ids: &[String],
    stream_sequence: u64,
    baseline_sequence: u64,
) -> bool {
    prepared_tool_calls
        .get(operation_id)
        .and_then(|tool_call_id| tool_call_id.as_ref())
        .is_some_and(|tool_call_id| tool_call_ids.contains(tool_call_id))
        || stream_sequence > baseline_sequence
}

fn mutation_detection_matches_filter(
    tool_call_id: Option<&str>,
    tool_call_ids: &[String],
    stream_sequence: u64,
    baseline_sequence: u64,
) -> bool {
    tool_call_id.is_some_and(|call_id| tool_call_ids.iter().any(|current| current == call_id))
        || stream_sequence > baseline_sequence
}

fn task_started_stream_sequence(records: &[SessionStreamRecord], task_id: &TaskId) -> Option<u64> {
    records.iter().find_map(|record| {
        let SessionStreamRecord::Stored(event) = record else {
            return None;
        };
        let payload = event.payload.get("session_log_entry")?.clone();
        let entry = serde_json::from_value::<crate::SessionLogEntry>(payload).ok()?;
        let crate::SessionLogEntry::Control(ControlEntry::TaskRun(task)) = entry else {
            return None;
        };
        (task.task_id == *task_id).then_some(event.stream_sequence)
    })
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
        source_label: None,
        recovery_hint: None,
        scope_hash: scope_hash.to_owned(),
        recorded_at_stream_sequence,
        from_workspace_snapshot_id: from_workspace_snapshot_id.map(str::to_owned),
        to_workspace_snapshot_id: None,
        tool_effect: crate::ToolEffect::WorkspaceWrite,
        unknown_dirty: false,
    }
}

fn durable_mutation_replay_failed_evidence(
    request: &SequentialTaskRequest,
    step: &TaskStepSpec,
    scope_hash: &str,
    recorded_at_stream_sequence: u64,
) -> WorkspaceMutationEvidence {
    WorkspaceMutationEvidence {
        event_id: format!(
            "task-step-durable-mutation-replay-failed:{}:{}",
            request.task_id.as_str(),
            step.step_id.as_str()
        ),
        source_event_type: "durable_mutation_replay_failed".to_owned(),
        source_label: None,
        recovery_hint: None,
        scope_hash: scope_hash.to_owned(),
        recorded_at_stream_sequence,
        from_workspace_snapshot_id: None,
        to_workspace_snapshot_id: None,
        tool_effect: crate::ToolEffect::Unknown,
        unknown_dirty: true,
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
        TaskStepStatus::Superseded => RunStatus::Cancelled,
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

struct TaskRunnableSelection {
    steps: Vec<TaskStepSpec>,
    paused_reason: Option<String>,
}

fn runnable_steps_for_continue(
    session: &Session,
    task: &TaskRunProjection,
    plan_version: u32,
    plan_steps: &[TaskStepSpec],
    step_options: [&AgentRunOptions; 3],
) -> Result<TaskRunnableSelection> {
    let Some(plan) = task.plans.get(&plan_version) else {
        return Ok(TaskRunnableSelection {
            steps: resumable_steps(task, plan_version, plan_steps),
            paused_reason: None,
        });
    };
    let Some(graph) = plan.graph.as_ref() else {
        if let Some(error) = plan.graph_validation_error.as_deref() {
            bail!("task plan v{plan_version} graph is invalid: {error}");
        }
        return Ok(TaskRunnableSelection {
            steps: resumable_steps(task, plan_version, plan_steps),
            paused_reason: None,
        });
    };

    let active_write_lease = has_active_task_write_lease(session, step_options)?;
    let queue = graph.ready_queue_with_active_write_lease(
        &task.steps,
        TaskReadyQueueOptions::new(DEFAULT_TASK_READ_ONLY_CONCURRENCY),
        active_write_lease,
    );
    let step_ids = if !queue.read_only_batch.is_empty() {
        queue
            .read_only_batch
            .iter()
            .map(|step| step.step_id.clone())
            .collect::<Vec<_>>()
    } else if let Some(step) = queue.sequential_step.as_ref() {
        vec![step.step_id.clone()]
    } else {
        Vec::new()
    };
    let steps = step_ids
        .iter()
        .map(|step_id| {
            plan_steps
                .iter()
                .find(|step| &step.step_id == step_id)
                .cloned()
                .ok_or_else(|| anyhow!("task graph references missing step {}", step_id.as_str()))
        })
        .collect::<Result<Vec<_>>>()?;
    let paused_reason = if steps.is_empty() {
        if queue.deferred.is_empty() {
            if plan_steps_all_completed(task, plan_version, plan_steps) {
                None
            } else {
                Some(format!(
                    "plan v{plan_version} has no ready steps; waiting for dependencies"
                ))
            }
        } else {
            Some(format!(
                "plan v{plan_version} has deferred steps: {}",
                queue
                    .deferred
                    .iter()
                    .map(|step| format!(
                        "{}:{}",
                        step.step_id.as_str(),
                        task_ready_deferred_reason_label(step.reason)
                    ))
                    .collect::<Vec<_>>()
                    .join(", ")
            ))
        }
    } else {
        None
    };
    Ok(TaskRunnableSelection {
        steps,
        paused_reason,
    })
}

const DEFAULT_TASK_READ_ONLY_CONCURRENCY: usize = 4;

fn task_ready_deferred_reason_label(reason: TaskReadyDeferredReason) -> &'static str {
    match reason {
        TaskReadyDeferredReason::ActiveWriteLease => "active_write_lease",
        TaskReadyDeferredReason::ConcurrencyBudget => "concurrency_budget",
        TaskReadyDeferredReason::RunningReadOnly => "running_read_only",
        TaskReadyDeferredReason::RunningWrite => "running_write",
        TaskReadyDeferredReason::SequentialWrite => "sequential_write",
    }
}

fn has_active_task_write_lease(
    session: &Session,
    step_options: [&AgentRunOptions; 3],
) -> Result<bool> {
    let projection = session.write_isolation_projection();
    for options in step_options {
        let workspace_id = stable_workspace_id(&options.workspace_root)?;
        if projection.has_active_write_lease(&workspace_id) {
            return Ok(true);
        }
    }
    Ok(false)
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

fn plan_steps_all_completed(
    task: &TaskRunProjection,
    plan_version: u32,
    plan_steps: &[TaskStepSpec],
) -> bool {
    plan_steps.iter().all(|step| {
        task.steps
            .get(&(plan_version, step.step_id.clone()))
            .is_some_and(|projected| projected.status == TaskStepStatus::Completed)
    })
}

fn cancels_dependent_steps(status: TaskStepStatus) -> bool {
    matches!(
        status,
        TaskStepStatus::Failed | TaskStepStatus::Cancelled | TaskStepStatus::Interrupted
    )
}

fn append_cancelled_dependent_steps<H>(
    session: &mut Session,
    handler: &mut H,
    task_id: &TaskId,
    plan_version: u32,
    plan_steps: &[TaskStepSpec],
    failed_step_id: &TaskStepId,
    failed_status: TaskStepStatus,
) -> Result<usize>
where
    H: EventHandler + Send,
{
    let projected = session.task_state_projection();
    let Some(task) = projected.tasks.get(task_id) else {
        return Ok(0);
    };
    let mut cancelled = BTreeSet::<TaskStepId>::new();
    loop {
        let mut changed = false;
        for step in plan_steps {
            if &step.step_id == failed_step_id || cancelled.contains(&step.step_id) {
                continue;
            }
            let depends_on_failed = step
                .depends_on
                .iter()
                .any(|dependency| dependency == failed_step_id || cancelled.contains(dependency));
            if !depends_on_failed {
                continue;
            }
            if task
                .steps
                .get(&(plan_version, step.step_id.clone()))
                .is_some_and(|projection| projection.status.is_terminal())
            {
                continue;
            }
            cancelled.insert(step.step_id.clone());
            changed = true;
        }
        if !changed {
            break;
        }
    }
    let mut count = 0;
    for step_id in cancelled {
        let Some(step) = plan_steps.iter().find(|step| step.step_id == step_id) else {
            continue;
        };
        append_task_step(
            session,
            handler,
            task_id,
            plan_version,
            step,
            TaskStepStatus::Cancelled,
            None,
            Some(format!(
                "dependency {} ended with {}",
                failed_step_id.as_str(),
                task_step_status_label(failed_status)
            )),
        )?;
        count += 1;
    }
    Ok(count)
}

fn task_step_status_label(status: TaskStepStatus) -> &'static str {
    match status {
        TaskStepStatus::Pending => "pending",
        TaskStepStatus::Running => "running",
        TaskStepStatus::Completed => "completed",
        TaskStepStatus::Failed => "failed",
        TaskStepStatus::Blocked => "blocked",
        TaskStepStatus::Cancelled => "cancelled",
        TaskStepStatus::Interrupted => "interrupted",
        TaskStepStatus::Superseded => "superseded",
    }
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
    } else if output.changeset_proposal.is_some() {
        TaskStepStatus::Blocked
    } else {
        TaskStepStatus::Completed
    }
}

fn step_status_after_readiness(
    status: TaskStepStatus,
    readiness: &ReadinessEvaluatedEntry,
) -> TaskStepStatus {
    if status == TaskStepStatus::Completed && readiness_blocks_step(readiness) {
        TaskStepStatus::Blocked
    } else {
        status
    }
}

fn readiness_blocks_step(readiness: &ReadinessEvaluatedEntry) -> bool {
    readiness
        .evaluation
        .required_actions
        .iter()
        .any(required_action_blocks_task_step)
}

fn required_action_blocks_task_step(action: &RequiredAction) -> bool {
    !matches!(action, RequiredAction::ProvideVerificationConfig)
}

fn step_reason_from_output(status: TaskStepStatus, output: &StepRunOutput) -> Option<String> {
    if status == TaskStepStatus::Blocked && output.changeset_proposal.is_some() {
        return Some("changeset ready for merge review".to_owned());
    }
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
        TaskStepStatus::Pending
        | TaskStepStatus::Running
        | TaskStepStatus::Blocked
        | TaskStepStatus::Superseded => TaskRunStatus::Paused,
    }
}

fn step_terminal_reason(step_id: &TaskStepId, status: TaskStepStatus) -> String {
    match status {
        TaskStepStatus::Failed => format!("step {} failed", step_id.as_str()),
        TaskStepStatus::Blocked => format!("step {} blocked", step_id.as_str()),
        TaskStepStatus::Cancelled => format!("step {} cancelled", step_id.as_str()),
        TaskStepStatus::Interrupted => format!("step {} interrupted", step_id.as_str()),
        TaskStepStatus::Superseded => format!("step {} superseded", step_id.as_str()),
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
        "Create an executable plan for this task. Call task_plan_update with an accepted plan before any execution. After task_plan_update succeeds, stop; do not inspect files, execute steps, or summarize execution progress. Do not call a task or subagent tool. To delegate verification or implementation, add plan steps with role subagent_read or subagent_write; the orchestrator will run those steps in child sessions. If the objective contains a user-approved plan, preserve its stated scope and order; only add, remove, or reorder steps when needed for correctness, and include the reason in the affected step detail.\n\nObjective:\n{objective}"
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

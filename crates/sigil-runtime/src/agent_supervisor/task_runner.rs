use std::{
    collections::BTreeSet,
    path::Path,
    sync::{Arc, Mutex},
};

use anyhow::Result;
use async_trait::async_trait;
use sigil_kernel::{
    AgentApprovalRouteEntry, AgentInvocationMode, AgentInvocationSource, AgentRole,
    AgentRouteStatus, AgentRunInput, AgentRunOptions, AgentThreadId, AgentUsageSummary,
    ApprovalHandler, ControlEntry, EventHandler, JsonlSessionStore, MultiAgentMode,
    ProviderCapabilities, ProviderPhysicalAttemptOutcome, ProviderRequestRejection,
    ProviderRouteCooldownError, RunEvent, SequentialTaskRequest, Session, SessionLogEntry,
    SessionRef, SessionStats, TaskChildSessionBatchCommitEnvelope,
    TaskChildSessionBatchPreparation, TaskChildSessionEntry, TaskChildSessionRunOutput,
    TaskChildSessionRunRequest, TaskChildSessionRunner, TaskChildSessionStatus, TaskId,
    TaskParticipantAttemptId, TaskParticipantRetryError, TaskParticipantRetryProof,
    TaskPlannerSessionRunOutput, TaskPlannerSessionRunRequest, TaskRouteId, TaskRouteStatus,
    TaskStepId, TaskStepMode, TaskStepSpec, TaskSubagentApprovalRouteEntry,
    TaskSynthesisSessionRunOutput, TaskSynthesisSessionRunRequest, ToolApproval, ToolCall,
    ToolErrorKind, ToolSpec, changeset_only_child_tool_registry,
    decode_changeset_only_child_output, task_participant_child_task_id,
    task_participant_input_hash, task_participant_logical_run_id,
};

use crate::{
    agent_completion::{AgentCompletionHub, AgentCompletionRegistration},
    provider_pressure::{
        TaskProviderPressure, TaskProviderRouteConsumer, wrap_task_agent_provider,
    },
    task_completion_progress::{TaskCompletionOutcome, TaskCompletionProgressRegistration},
};

use super::{
    AgentSupervisor, AgentTaskChildStart, AgentTaskChildThread, BoxedAgent, append_control,
    hash_text,
    ids::{agent_route_id_for_call, task_route_id_for_call},
    materialize_child_agent_final_answer,
    task_discovery::{
        MAX_TASK_DISCOVERY_PROBES, TaskDiscoveryDelegate, planner_tools_with_discovery,
    },
};

/// Runtime child runner that connects kernel task orchestration to the supervisor.
pub struct AgentSupervisorTaskChildRunner {
    supervisor: AgentSupervisor,
    planner: Option<Arc<BoxedAgent>>,
    executor: Option<Arc<BoxedAgent>>,
    subagent_read: Arc<BoxedAgent>,
    subagent_write: Arc<BoxedAgent>,
    synthesis: Option<Arc<BoxedAgent>>,
    planner_discovery_max_probes: usize,
    provider_pressure: TaskProviderPressure,
}

impl AgentSupervisorTaskChildRunner {
    pub fn new(
        supervisor: AgentSupervisor,
        subagent_read: BoxedAgent,
        subagent_write: BoxedAgent,
    ) -> Self {
        let provider_pressure = supervisor.provider_pressure().clone();
        Self {
            supervisor,
            planner: None,
            executor: None,
            subagent_read: Arc::new(wrap_task_agent_provider(
                subagent_read,
                provider_pressure.clone(),
                TaskProviderRouteConsumer::SubagentRead,
            )),
            subagent_write: Arc::new(wrap_task_agent_provider(
                subagent_write,
                provider_pressure.clone(),
                TaskProviderRouteConsumer::SubagentWrite,
            )),
            synthesis: None,
            planner_discovery_max_probes: 0,
            provider_pressure,
        }
    }

    pub fn new_with_task_roles(
        supervisor: AgentSupervisor,
        planner: BoxedAgent,
        executor: BoxedAgent,
        subagent_read: BoxedAgent,
        subagent_write: BoxedAgent,
        synthesis: BoxedAgent,
    ) -> Self {
        let provider_pressure = supervisor.provider_pressure().clone();
        Self {
            supervisor,
            planner: Some(Arc::new(wrap_task_agent_provider(
                planner,
                provider_pressure.clone(),
                TaskProviderRouteConsumer::Planner,
            ))),
            executor: Some(Arc::new(wrap_task_agent_provider(
                executor,
                provider_pressure.clone(),
                TaskProviderRouteConsumer::Executor,
            ))),
            subagent_read: Arc::new(wrap_task_agent_provider(
                subagent_read,
                provider_pressure.clone(),
                TaskProviderRouteConsumer::SubagentRead,
            )),
            subagent_write: Arc::new(wrap_task_agent_provider(
                subagent_write,
                provider_pressure.clone(),
                TaskProviderRouteConsumer::SubagentWrite,
            )),
            synthesis: Some(Arc::new(wrap_task_agent_provider(
                synthesis,
                provider_pressure.clone(),
                TaskProviderRouteConsumer::Synthesis,
            ))),
            planner_discovery_max_probes: 0,
            provider_pressure,
        }
    }

    #[must_use]
    pub fn with_planner_discovery_policy(
        mut self,
        multi_agent_mode: MultiAgentMode,
        max_probes: usize,
    ) -> Self {
        self.planner_discovery_max_probes = if multi_agent_mode == MultiAgentMode::None {
            0
        } else {
            max_probes.min(MAX_TASK_DISCOVERY_PROBES)
        };
        self
    }

    /// Sets the process-local upper bound for each provider/model route's adaptive concurrency
    /// window.
    #[must_use]
    pub fn with_provider_route_concurrency_limit(self, max_concurrency: usize) -> Self {
        self.provider_pressure
            .set_max_concurrency(max_concurrency.max(1));
        self
    }

    #[allow(clippy::too_many_arguments)]
    fn begin_isolated_participant<H>(
        &self,
        parent_session: &mut Session,
        handler: &mut H,
        task: &SequentialTaskRequest,
        attempt_id: TaskParticipantAttemptId,
        plan_version: u32,
        child_session_ref: SessionRef,
        child_input: AgentRunInput,
        options: AgentRunOptions,
        step: TaskStepSpec,
        agent: &BoxedAgent,
    ) -> Result<(Session, AgentTaskChildThread, TaskId)>
    where
        H: EventHandler + Send,
    {
        let child_task_id = task_participant_child_task_id(&task.task_id, &attempt_id)?;
        let child_session = build_child_session(parent_session, &child_session_ref)?;
        let child_thread = self.supervisor.begin_task_child_thread(
            parent_session,
            handler,
            AgentTaskChildStart {
                task_id: task.task_id.clone(),
                parent_thread_id: main_thread_id()?,
                parent_depth: 0,
                batch_id: None,
                batch_member_key: None,
                parent_session_ref: task.parent_session_ref.clone(),
                plan_version,
                step,
                child_task_id: child_task_id.clone(),
                child_session_ref: child_session_ref.clone(),
                child_input,
                objective: task.objective.clone(),
                workspace_root: options.workspace_root,
                provider_capabilities: child_provider_capabilities(agent),
                role: AgentRole::Planner,
                invocation_mode: AgentInvocationMode::Foreground,
                invocation_source: AgentInvocationSource::Task,
            },
        )?;
        Ok((child_session, child_thread, child_task_id))
    }

    fn agent_for_step(&self, step: &TaskStepSpec) -> Result<Arc<BoxedAgent>> {
        match step.role {
            AgentRole::Planner => self
                .planner
                .clone()
                .ok_or_else(|| anyhow::anyhow!("task planner role is not configured")),
            AgentRole::Executor => self
                .executor
                .clone()
                .ok_or_else(|| anyhow::anyhow!("task executor role is not configured")),
            AgentRole::SubagentRead => Ok(Arc::clone(&self.subagent_read)),
            AgentRole::SubagentWrite => Ok(Arc::clone(&self.subagent_write)),
        }
    }

    fn preflight_parallel_read_child(
        &self,
        parent_session: &Session,
        request: TaskChildSessionRunRequest,
    ) -> Result<PreflightParallelTaskChild> {
        if !matches!(
            request.step.effective_mode(),
            TaskStepMode::Read | TaskStepMode::Review | TaskStepMode::Verify
        ) || request.step.effective_isolation()
            != sigil_kernel::TaskIsolationMode::SharedReadOnly
        {
            anyhow::bail!("parallel task child batch accepts only shared-read-only steps");
        }
        let agent = self.agent_for_step(&request.step)?;
        let child_task_id =
            task_participant_child_task_id(&request.task.task_id, &request.attempt_id)?;
        let child_session_ref = request.child_session_ref.clone();
        let child_session = build_child_session(parent_session, &child_session_ref)?;
        if let Err(error) = self
            .provider_pressure
            .check(agent.provider().name(), child_session.model_name())
        {
            return Err(self.retryable_admission_error(&request, &agent, &child_session, error));
        }
        let start = AgentTaskChildStart {
            task_id: request.task.task_id.clone(),
            parent_thread_id: main_thread_id()?,
            parent_depth: 0,
            batch_id: None,
            batch_member_key: None,
            parent_session_ref: request.task.parent_session_ref.clone(),
            plan_version: request.plan_version,
            step: request.step.clone(),
            child_task_id: child_task_id.clone(),
            child_session_ref: child_session_ref.clone(),
            child_input: request.child_input.clone(),
            objective: request.task.objective.clone(),
            workspace_root: request.options.workspace_root.clone(),
            provider_capabilities: child_provider_capabilities(&agent),
            role: request.step.role,
            invocation_mode: AgentInvocationMode::JoinBeforeFinal,
            invocation_source: AgentInvocationSource::Task,
        };
        Ok(PreflightParallelTaskChild {
            request,
            child_task_id,
            child_session_ref,
            agent,
            start,
            child_session,
        })
    }

    fn start_parallel_read_child<H>(
        &self,
        parent_session: &mut Session,
        preflight: PreflightParallelTaskChild,
        handler: &mut H,
    ) -> Result<PreparedParallelTaskChild>
    where
        H: EventHandler + Send,
    {
        let PreflightParallelTaskChild {
            request,
            child_task_id,
            child_session_ref,
            agent,
            start,
            child_session,
        } = preflight;
        let child_thread =
            self.supervisor
                .begin_task_child_thread(parent_session, handler, start)?;
        let thread_release = TaskChildThreadReleaseGuard::new(&self.supervisor, &child_thread);
        if let Err(error) = append_task_child_session(
            parent_session,
            handler,
            &request,
            &child_task_id,
            &child_session_ref,
            TaskChildSessionStatus::Started,
            None,
        ) {
            let _ = self.supervisor.record_task_child_failure(
                parent_session,
                handler,
                &child_thread,
                format!("failed to persist task child start: {error:#}"),
            );
            return Err(error);
        }
        Ok(PreparedParallelTaskChild {
            request,
            child_task_id,
            child_session_ref,
            agent,
            child_thread,
            child_session,
            _thread_release: thread_release,
        })
    }

    async fn execute_parallel_read_child<H, A>(
        &self,
        mut prepared: PreparedParallelTaskChild,
        handler: &mut H,
        approval_handler: &mut A,
    ) -> ExecutedParallelTaskChild
    where
        H: EventHandler + Send,
        A: ApprovalHandler + Send,
    {
        let mut route_handler = BufferedSupervisorTaskApprovalRouteHandler {
            inner: approval_handler,
            task_request: &prepared.request,
            child_session_ref: &prepared.child_session_ref,
            source_thread_id: &prepared.child_thread.thread_id,
            controls: Vec::new(),
        };
        let child_run = {
            let mut participant_handler = TaskParticipantEventHandler { inner: handler };
            run_task_child_agent_for_step(
                &prepared.agent,
                &mut prepared.child_session,
                prepared.request.child_input.clone(),
                prepared.request.options.clone(),
                &prepared.request.step,
                &mut participant_handler,
                &mut route_handler,
            )
            .await
        };
        let controls = route_handler.controls;
        let result = match child_run {
            Ok(output) => {
                let outcome = output.outcome;
                match materialize_child_agent_final_answer(
                    &mut prepared.child_session,
                    &prepared.child_session_ref,
                    &prepared.child_thread.thread_id,
                    &output.result,
                )
                .await
                {
                    Ok(materialized) => Ok(ParallelTaskChildSuccess {
                        materialized,
                        outcome,
                        usage: usage_summary_from_stats(prepared.child_session.stats()),
                    }),
                    Err(error) => Err(error),
                }
            }
            Err(error) => Err(self.retryable_child_error(
                &prepared.request,
                &prepared.agent,
                &prepared.child_session,
                error,
            )),
        };
        ExecutedParallelTaskChild {
            prepared,
            controls,
            result,
        }
    }

    fn retryable_admission_error(
        &self,
        request: &TaskChildSessionRunRequest,
        agent: &BoxedAgent,
        child_session: &Session,
        error: anyhow::Error,
    ) -> anyhow::Error {
        if !retry_safe_step(&request.step)
            || error.downcast_ref::<ProviderRouteCooldownError>().is_none()
        {
            return error;
        }
        self.wrap_retryable_error(
            &request.attempt_id,
            &request.child_input,
            agent,
            child_session,
            TaskParticipantRetryProof::AdmissionRejectedBeforeDispatch {
                zero_output: true,
                zero_tool: true,
                zero_effect: true,
            },
            error,
        )
    }

    fn retryable_child_error(
        &self,
        request: &TaskChildSessionRunRequest,
        agent: &BoxedAgent,
        child_session: &Session,
        error: anyhow::Error,
    ) -> anyhow::Error {
        if !retry_safe_step(&request.step) {
            return error;
        }
        self.retryable_zero_effect_error(
            &request.attempt_id,
            &request.child_input,
            agent,
            child_session,
            error,
        )
    }

    fn retryable_zero_effect_error(
        &self,
        attempt_id: &TaskParticipantAttemptId,
        input: &AgentRunInput,
        agent: &BoxedAgent,
        child_session: &Session,
        error: anyhow::Error,
    ) -> anyhow::Error {
        let Ok(projection) = child_session.provider_physical_attempt_projection() else {
            return error;
        };
        let logical_run_id = task_participant_logical_run_id(attempt_id);
        let attempts = projection.attempts_for_logical_run_id(&logical_run_id);
        let [attempt] = attempts.as_slice() else {
            return error;
        };
        let Some(terminal) = attempt.terminal.as_ref() else {
            return error;
        };
        if terminal.outcome != ProviderPhysicalAttemptOutcome::ConfirmedNoModelConsumption
            || terminal.rejection != Some(ProviderRequestRejection::RateLimited)
            || !terminal.durable_output_event_ids.is_empty()
            || !terminal.durable_side_effect_event_ids.is_empty()
            || child_session.entries().iter().any(|entry| {
                matches!(
                    entry,
                    SessionLogEntry::Assistant(_) | SessionLogEntry::ToolResult(_)
                ) || matches!(
                    entry,
                    SessionLogEntry::Control(
                        ControlEntry::ToolExecution(_)
                            | ControlEntry::ToolEgress(_)
                            | ControlEntry::ChangeSetProposed(_)
                            | ControlEntry::ChangeSetApplied(_)
                            | ControlEntry::TaskPlan(_)
                    )
                )
            })
        {
            return error;
        }
        self.wrap_retryable_error(
            attempt_id,
            input,
            agent,
            child_session,
            TaskParticipantRetryProof::ProviderConfirmedNoConsumption {
                physical_attempt_id: attempt.entry.physical_attempt_id.clone(),
                request_material_fingerprint: attempt.entry.request_material_fingerprint.clone(),
                zero_output: true,
                zero_tool: true,
                zero_effect: true,
            },
            error,
        )
    }

    fn wrap_retryable_error(
        &self,
        attempt_id: &TaskParticipantAttemptId,
        input: &AgentRunInput,
        agent: &BoxedAgent,
        child_session: &Session,
        proof: TaskParticipantRetryProof,
        error: anyhow::Error,
    ) -> anyhow::Error {
        let Some((retry_after_ms, route_fingerprint)) =
            self.provider_pressure.retry_schedule_delay(
                agent.provider().name(),
                child_session.model_name(),
                attempt_id,
            )
        else {
            return error;
        };
        let Ok(input_hash) = task_participant_input_hash(input) else {
            return error;
        };
        TaskParticipantRetryError::new(retry_after_ms, route_fingerprint, input_hash, proof, error)
            .map(anyhow::Error::new)
            .unwrap_or_else(|construction_error| construction_error)
    }

    fn commit_parallel_read_child<H>(
        supervisor: &AgentSupervisor,
        parent_session: &mut Session,
        handler: &mut H,
        executed: ExecutedParallelTaskChild,
    ) -> Result<TaskChildSessionRunOutput>
    where
        H: EventHandler + Send + ?Sized,
    {
        let ExecutedParallelTaskChild {
            prepared,
            controls,
            result,
        } = executed;
        for control in controls {
            parent_session.append_control(control)?;
        }
        let success = match result {
            Ok(success) => success,
            Err(error) => {
                append_task_child_session(
                    parent_session,
                    handler,
                    &prepared.request,
                    &prepared.child_task_id,
                    &prepared.child_session_ref,
                    TaskChildSessionStatus::Failed,
                    None,
                )?;
                supervisor.record_task_child_failure(
                    parent_session,
                    handler,
                    &prepared.child_thread,
                    format!("{error:#}"),
                )?;
                return Err(error);
            }
        };
        let budget_warning = supervisor
            .validate_usage_budget(&prepared.request.task.task_id, &success.usage)
            .err()
            .map(|error| format!("{error:#}"));
        let status =
            task_child_status_from_outcome(&success.materialized.final_text, &success.outcome);
        append_task_child_session(
            parent_session,
            handler,
            &prepared.request,
            &prepared.child_task_id,
            &prepared.child_session_ref,
            status,
            Some(hash_text(&success.materialized.final_text)),
        )?;
        supervisor.record_task_child_result(
            parent_session,
            handler,
            &prepared.child_thread,
            prepared.child_session_ref.clone(),
            status,
            &success.materialized,
            &success.outcome,
            Some(success.usage),
        )?;
        if let Some(warning) = budget_warning {
            let _ = handler.handle(RunEvent::Notice(format!(
                "agent budget warning after child completion: {warning}"
            )));
        }
        Ok(TaskChildSessionRunOutput {
            attempt_id: prepared.request.attempt_id,
            final_text: success.materialized.final_text,
            outcome: success.outcome,
            child_session_ref: prepared.child_session_ref,
            final_answer_ref: success.materialized.final_answer_ref,
            artifact_refs: success.materialized.extra_artifacts,
            changeset_proposal: None,
            changeset_only_after_snapshot_id: None,
        })
    }
}

struct PreflightParallelTaskChild {
    request: TaskChildSessionRunRequest,
    child_task_id: TaskId,
    child_session_ref: SessionRef,
    agent: Arc<BoxedAgent>,
    start: AgentTaskChildStart,
    child_session: Session,
}

struct PreparedParallelTaskChild {
    request: TaskChildSessionRunRequest,
    child_task_id: TaskId,
    child_session_ref: SessionRef,
    agent: Arc<BoxedAgent>,
    child_thread: AgentTaskChildThread,
    child_session: Session,
    _thread_release: TaskChildThreadReleaseGuard,
}

struct ParallelTaskChildSuccess {
    materialized: super::AgentResultMaterialization,
    outcome: sigil_kernel::AgentRunOutcome,
    usage: AgentUsageSummary,
}

struct ExecutedParallelTaskChild {
    prepared: PreparedParallelTaskChild,
    controls: Vec<ControlEntry>,
    result: Result<ParallelTaskChildSuccess>,
}

struct ParallelTaskCompletionContext {
    request_index: usize,
}

struct TaskChildThreadReleaseGuard {
    supervisor: AgentSupervisor,
    thread_id: AgentThreadId,
}

impl TaskChildThreadReleaseGuard {
    fn new(supervisor: &AgentSupervisor, thread: &AgentTaskChildThread) -> Self {
        Self {
            supervisor: supervisor.clone(),
            thread_id: thread.thread_id.clone(),
        }
    }
}

impl Drop for TaskChildThreadReleaseGuard {
    fn drop(&mut self) {
        self.supervisor.release_runtime_thread(&self.thread_id);
    }
}

fn participant_control_step(step_id: &str, title: &str, role: AgentRole) -> Result<TaskStepSpec> {
    Ok(TaskStepSpec {
        step_id: TaskStepId::new(step_id)?,
        title: title.to_owned(),
        display_name: None,
        detail: None,
        role,
        depends_on: Vec::new(),
        mode: Some(TaskStepMode::Read),
        isolation: Some(sigil_kernel::TaskIsolationMode::SharedReadOnly),
    })
}

#[async_trait]
impl TaskChildSessionRunner for AgentSupervisorTaskChildRunner {
    async fn run_planner_session<H, A>(
        &self,
        parent_session: &mut Session,
        request: TaskPlannerSessionRunRequest,
        handler: &mut H,
        approval_handler: &mut A,
    ) -> Result<TaskPlannerSessionRunOutput>
    where
        H: EventHandler + Send,
        A: ApprovalHandler + Send,
    {
        let planner = self
            .planner
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("task planner role is not configured"))?;
        let step = participant_control_step("planner", "Plan task", AgentRole::Planner)?;
        let (mut child_session, child_thread, _child_task_id) = self.begin_isolated_participant(
            parent_session,
            handler,
            &request.task,
            request.attempt_id.clone(),
            0,
            request.child_session_ref.clone(),
            request.child_input.clone(),
            request.options.clone(),
            step,
            planner,
        )?;
        let _thread_release = TaskChildThreadReleaseGuard::new(&self.supervisor, &child_thread);
        let planner_run = {
            let mut participant_handler = TaskParticipantEventHandler { inner: handler };
            if self.planner_discovery_max_probes == 0 {
                planner
                    .run_with_approval_input(
                        &mut child_session,
                        request.child_input.clone(),
                        request.options.clone(),
                        &mut participant_handler,
                        approval_handler,
                    )
                    .await
            } else {
                let tools = planner_tools_with_discovery(
                    planner.tool_registry(),
                    self.planner_discovery_max_probes,
                );
                let mut discovery_delegate = TaskDiscoveryDelegate::new(
                    self.supervisor.clone(),
                    parent_session,
                    request.task.clone(),
                    request.attempt_id.clone(),
                    child_thread.thread_id.clone(),
                    Arc::clone(&self.subagent_read),
                    request.discovery_options.clone(),
                    self.planner_discovery_max_probes,
                );
                planner
                    .run_with_approval_input_tool_registry_and_agent_delegate(
                        &mut child_session,
                        request.child_input.clone(),
                        request.options.clone(),
                        tools,
                        &mut participant_handler,
                        approval_handler,
                        &mut discovery_delegate,
                    )
                    .await
            }
        };
        let output = match planner_run {
            Ok(output) => output,
            Err(error) => {
                let error = self.retryable_zero_effect_error(
                    &request.attempt_id,
                    &request.child_input,
                    planner,
                    &child_session,
                    error,
                );
                self.supervisor.record_task_child_failure(
                    parent_session,
                    handler,
                    &child_thread,
                    format!("{error:#}"),
                )?;
                return Err(error);
            }
        };
        let postprocessed = (|| -> Result<TaskPlannerSessionRunOutput> {
            let accepted_plan = child_session
                .task_state_projection()
                .tasks
                .get(&request.task.task_id)
                .and_then(|task| task.latest_plan_version)
                .and_then(|version| {
                    child_session
                        .entries()
                        .iter()
                        .rev()
                        .find_map(|entry| match entry {
                            sigil_kernel::SessionLogEntry::Control(ControlEntry::TaskPlan(
                                plan,
                            )) if plan.task_id == request.task.task_id
                                && plan.plan_version == version
                                && plan.status == sigil_kernel::TaskPlanStatus::Accepted =>
                            {
                                Some(plan.clone())
                            }
                            _ => None,
                        })
                })
                .ok_or_else(|| {
                    anyhow::anyhow!("isolated planner did not produce an accepted plan")
                })?;
            let materialized = super::AgentResultMaterialization::inline(
                format!("accepted task plan v{}", accepted_plan.plan_version),
                None,
            );
            self.supervisor.record_task_child_result(
                parent_session,
                handler,
                &child_thread,
                request.child_session_ref.clone(),
                TaskChildSessionStatus::Completed,
                &materialized,
                &output.outcome,
                Some(usage_summary_from_stats(child_session.stats())),
            )?;
            Ok(TaskPlannerSessionRunOutput {
                attempt_id: request.attempt_id.clone(),
                accepted_plan,
                child_session_ref: request.child_session_ref.clone(),
            })
        })();
        match postprocessed {
            Ok(output) => Ok(output),
            Err(error) => {
                self.supervisor.record_task_child_failure(
                    parent_session,
                    handler,
                    &child_thread,
                    format!("{error:#}"),
                )?;
                Err(error)
            }
        }
    }

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
        let child_task_id =
            task_participant_child_task_id(&request.task.task_id, &request.attempt_id)?;
        let child_session_ref = request.child_session_ref.clone();
        let agent = match request.step.role {
            AgentRole::Planner => self
                .planner
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("task planner role is not configured"))?,
            AgentRole::Executor => self
                .executor
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("task executor role is not configured"))?,
            AgentRole::SubagentRead => &self.subagent_read,
            AgentRole::SubagentWrite => &self.subagent_write,
        };
        let child_thread = self.supervisor.begin_task_child_thread(
            parent_session,
            handler,
            AgentTaskChildStart {
                task_id: request.task.task_id.clone(),
                parent_thread_id: main_thread_id()?,
                parent_depth: 0,
                batch_id: None,
                batch_member_key: None,
                parent_session_ref: request.task.parent_session_ref.clone(),
                plan_version: request.plan_version,
                step: request.step.clone(),
                child_task_id: child_task_id.clone(),
                child_session_ref: child_session_ref.clone(),
                child_input: request.child_input.clone(),
                objective: request.task.objective.clone(),
                workspace_root: request.options.workspace_root.clone(),
                provider_capabilities: child_provider_capabilities(agent),
                role: request.step.role,
                invocation_mode: AgentInvocationMode::Foreground,
                invocation_source: AgentInvocationSource::Task,
            },
        )?;
        let _thread_release = TaskChildThreadReleaseGuard::new(&self.supervisor, &child_thread);
        append_task_child_session(
            parent_session,
            handler,
            &request,
            &child_task_id,
            &child_session_ref,
            TaskChildSessionStatus::Started,
            None,
        )?;
        let mut child_session = match build_child_session(parent_session, &child_session_ref) {
            Ok(session) => session,
            Err(error) => {
                append_task_child_session(
                    parent_session,
                    handler,
                    &request,
                    &child_task_id,
                    &child_session_ref,
                    TaskChildSessionStatus::Failed,
                    None,
                )?;
                self.supervisor.record_task_child_failure(
                    parent_session,
                    handler,
                    &child_thread,
                    format!("{error:#}"),
                )?;
                return Err(error);
            }
        };
        let mut route_handler = SupervisorTaskApprovalRouteHandler {
            inner: approval_handler,
            parent_session,
            task_request: &request,
            child_session_ref: &child_session_ref,
            source_thread_id: &child_thread.thread_id,
        };
        let child_input = request.child_input.clone();
        let options = request.options.clone();
        let child_run = {
            let mut participant_handler = TaskParticipantEventHandler { inner: handler };
            run_task_child_agent_for_step(
                agent,
                &mut child_session,
                child_input,
                options,
                &request.step,
                &mut participant_handler,
                &mut route_handler,
            )
            .await
        };
        let output = match child_run {
            Ok(output) => output,
            Err(error) => {
                let error = self.retryable_child_error(&request, agent, &child_session, error);
                append_task_child_session(
                    route_handler.parent_session,
                    handler,
                    &request,
                    &child_task_id,
                    &child_session_ref,
                    TaskChildSessionStatus::Failed,
                    None,
                )?;
                self.supervisor.record_task_child_failure(
                    route_handler.parent_session,
                    handler,
                    &child_thread,
                    format!("{error:#}"),
                )?;
                return Err(error);
            }
        };
        let postprocessed = async {
            let changeset_proposal = if request.step.effective_isolation()
                == sigil_kernel::TaskIsolationMode::ChangesetOnly
            {
                Some(decode_changeset_only_child_output(
                    &output.result.final_text,
                )?)
            } else {
                None
            };
            let changeset_only_after_snapshot_id = if let Some(base_snapshot_id) =
                request.changeset_only_base_snapshot_id.as_deref()
            {
                Some(
                    sigil_kernel::validate_changeset_only_parent_snapshot_unchanged_for_task(
                        route_handler.parent_session,
                        &request.task,
                        request.plan_version,
                        &request.step,
                        &request.options,
                        base_snapshot_id,
                    )?,
                )
            } else {
                None
            };
            let materialized = materialize_child_agent_final_answer(
                &mut child_session,
                &child_session_ref,
                &child_thread.thread_id,
                &output.result,
            )
            .await?;
            let outcome = output.outcome;
            let usage = usage_summary_from_stats(child_session.stats());
            let budget_warning = self
                .supervisor
                .validate_usage_budget(&request.task.task_id, &usage)
                .err()
                .map(|error| format!("{error:#}"));
            let status = task_child_status_from_outcome(&materialized.final_text, &outcome);
            append_task_child_session(
                route_handler.parent_session,
                handler,
                &request,
                &child_task_id,
                &child_session_ref,
                status,
                Some(hash_text(&materialized.final_text)),
            )?;
            self.supervisor.record_task_child_result(
                route_handler.parent_session,
                handler,
                &child_thread,
                child_session_ref.clone(),
                status,
                &materialized,
                &outcome,
                Some(usage),
            )?;
            if let Some(warning) = budget_warning {
                let _ = handler.handle(RunEvent::Notice(format!(
                    "agent budget warning after child completion: {warning}"
                )));
            }
            Ok(TaskChildSessionRunOutput {
                attempt_id: request.attempt_id.clone(),
                final_text: materialized.final_text,
                outcome,
                child_session_ref: child_session_ref.clone(),
                final_answer_ref: materialized.final_answer_ref,
                artifact_refs: materialized.extra_artifacts,
                changeset_proposal,
                changeset_only_after_snapshot_id,
            })
        }
        .await;
        match postprocessed {
            Ok(output) => Ok(output),
            Err(error) => {
                append_task_child_session(
                    route_handler.parent_session,
                    handler,
                    &request,
                    &child_task_id,
                    &child_session_ref,
                    TaskChildSessionStatus::Failed,
                    None,
                )?;
                self.supervisor.record_task_child_failure(
                    route_handler.parent_session,
                    handler,
                    &child_thread,
                    format!("{error:#}"),
                )?;
                Err(error)
            }
        }
    }

    fn prepare_child_session_batch<'a, H, A>(
        &'a self,
        parent_session: &mut Session,
        requests: Vec<TaskChildSessionRunRequest>,
        handler: &'a mut H,
        approval_handler: &'a mut A,
    ) -> Result<TaskChildSessionBatchPreparation<'a>>
    where
        H: EventHandler + Send + 'a,
        A: ApprovalHandler + Send + 'a,
    {
        if requests.is_empty() {
            return Ok(detached_task_batch_results(Vec::new()));
        }
        let member_count = requests.len();
        let batch_task_id = requests[0].task.task_id.clone();
        let batch_plan_version = requests[0].plan_version;
        let mut attempt_ids = BTreeSet::new();
        for request in &requests {
            if request.task.task_id != batch_task_id || request.plan_version != batch_plan_version {
                return Ok(detached_task_batch_results(rejected_parallel_read_batch(
                    &requests,
                    anyhow::anyhow!("parallel task child batch mixes task or plan identities"),
                )));
            }
            if !attempt_ids.insert(request.attempt_id.clone()) {
                return Ok(detached_task_batch_results(rejected_parallel_read_batch(
                    &requests,
                    anyhow::anyhow!(
                        "parallel task child batch contains duplicate attempt {}",
                        request.attempt_id.as_str()
                    ),
                )));
            }
        }
        let mut preflight = Vec::with_capacity(member_count);
        for request in requests.iter().cloned() {
            match self.preflight_parallel_read_child(parent_session, request) {
                Ok(member) => preflight.push(member),
                Err(error) => {
                    return Ok(detached_task_batch_results(rejected_parallel_read_batch(
                        &requests, error,
                    )));
                }
            }
        }
        let starts = preflight
            .iter()
            .map(|member| member.start.clone())
            .collect::<Vec<_>>();
        let reservation = match self.supervisor.reserve_task_child_batch(&starts) {
            Ok(reservation) => reservation,
            Err(error) => {
                return Ok(detached_task_batch_results(rejected_parallel_read_batch(
                    &requests, error,
                )));
            }
        };
        let mut prepared = Vec::with_capacity(member_count);
        for member in preflight {
            match self.start_parallel_read_child(parent_session, member, handler) {
                Ok(member) => prepared.push(member),
                Err(error) => {
                    let reason =
                        "parallel task child batch start rolled back before provider dispatch";
                    for started in &prepared {
                        let _ = append_task_child_session(
                            parent_session,
                            handler,
                            &started.request,
                            &started.child_task_id,
                            &started.child_session_ref,
                            TaskChildSessionStatus::Failed,
                            None,
                        );
                        let _ = self.supervisor.record_task_child_failure(
                            parent_session,
                            handler,
                            &started.child_thread,
                            reason.to_owned(),
                        );
                    }
                    return Ok(detached_task_batch_results(rejected_parallel_read_batch(
                        &requests, error,
                    )));
                }
            }
        }
        reservation.commit();
        let completion_progress = prepared
            .iter()
            .map(|member| TaskCompletionProgressRegistration {
                step_id: member.request.step.step_id.clone(),
                title: member
                    .request
                    .step
                    .display_name
                    .clone()
                    .unwrap_or_else(|| member.request.step.title.clone()),
            })
            .collect::<Vec<_>>();
        let shared_handler = SharedTaskEventHandler {
            inner: Arc::new(Mutex::new(handler)),
        };
        let shared_approval = SharedTaskApprovalHandler {
            inner: Arc::new(Mutex::new(approval_handler)),
        };
        let registrations = prepared
            .into_iter()
            .enumerate()
            .map(|(request_index, member)| {
                let key = member.request.attempt_id.clone();
                let context = ParallelTaskCompletionContext { request_index };
                let mut member_handler = shared_handler.clone();
                let mut member_approval = shared_approval.clone();
                AgentCompletionRegistration::new(key, request_index as u64, context, async move {
                    Ok::<_, anyhow::Error>(
                        self.execute_parallel_read_child(
                            member,
                            &mut member_handler,
                            &mut member_approval,
                        )
                        .await,
                    )
                })
            })
            .collect::<Vec<_>>();
        let completion_hub = match AgentCompletionHub::from_batch(registrations) {
            Ok(completion_hub) => completion_hub,
            Err(rejection) => {
                let (error, registrations) = rejection.into_parts();
                drop(registrations);
                drop(shared_handler);
                drop(shared_approval);
                return Err(anyhow::Error::new(error).context(
                    "task completion registration violated prevalidated unique attempt identity",
                ));
            }
        };
        drop(shared_handler);
        drop(shared_approval);
        let progress_registry = self.supervisor.completion_progress().clone();
        let generation =
            progress_registry.begin(&batch_task_id, batch_plan_version, completion_progress);
        let supervisor = self.supervisor.clone();

        Ok(TaskChildSessionBatchPreparation::Detached(Box::pin(
            async move {
                let mut completed = completion_hub
                    .collect_with(|envelope| {
                        let outcome = match envelope.result.as_ref() {
                            Ok(executed) if executed.result.is_ok() => {
                                TaskCompletionOutcome::Succeeded
                            }
                            Ok(_) | Err(_) => TaskCompletionOutcome::Failed,
                        };
                        progress_registry.record_arrival(
                            generation,
                            envelope.context.request_index,
                            usize::try_from(envelope.completion_index).unwrap_or(usize::MAX),
                            outcome,
                        );
                    })
                    .await;
                completed.sort_by_key(|envelope| envelope.sequence);
                Ok(TaskChildSessionBatchCommitEnvelope::new(
                    member_count,
                    move |parent_session, handler| {
                        Ok(completed
                            .into_iter()
                            .map(|envelope| {
                                envelope.result.and_then(|executed| {
                                    Self::commit_parallel_read_child(
                                        &supervisor,
                                        parent_session,
                                        handler,
                                        executed,
                                    )
                                })
                            })
                            .collect())
                    },
                ))
            },
        )))
    }

    async fn run_child_session_batch<H, A>(
        &self,
        parent_session: &mut Session,
        requests: Vec<TaskChildSessionRunRequest>,
        handler: &mut H,
        approval_handler: &mut A,
    ) -> Result<Vec<Result<TaskChildSessionRunOutput>>>
    where
        H: EventHandler + Send,
        A: ApprovalHandler + Send,
    {
        let preparation =
            self.prepare_child_session_batch(parent_session, requests, handler, approval_handler)?;
        let settled = settle_runtime_task_child_batch(preparation).await?;
        match settled {
            SettledRuntimeTaskChildBatch::Detached(commit) => {
                commit.commit(parent_session, handler)
            }
            SettledRuntimeTaskChildBatch::Fallback(requests) => {
                let mut outputs = Vec::with_capacity(requests.len());
                for request in requests {
                    outputs.push(
                        self.run_child_session(parent_session, request, handler, approval_handler)
                            .await,
                    );
                }
                Ok(outputs)
            }
        }
    }

    async fn run_synthesis_session<H, A>(
        &self,
        parent_session: &mut Session,
        request: TaskSynthesisSessionRunRequest,
        handler: &mut H,
        approval_handler: &mut A,
    ) -> Result<TaskSynthesisSessionRunOutput>
    where
        H: EventHandler + Send,
        A: ApprovalHandler + Send,
    {
        let synthesis = self
            .synthesis
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("task synthesis role is not configured"))?;
        let step = participant_control_step("synthesis", "Synthesize task", AgentRole::Planner)?;
        let (mut child_session, child_thread, _child_task_id) = self.begin_isolated_participant(
            parent_session,
            handler,
            &request.task,
            request.attempt_id.clone(),
            request.plan_version,
            request.child_session_ref.clone(),
            request.child_input.clone(),
            request.options.clone(),
            step,
            synthesis,
        )?;
        let _thread_release = TaskChildThreadReleaseGuard::new(&self.supervisor, &child_thread);
        let synthesis_run = {
            let mut participant_handler = TaskParticipantEventHandler { inner: handler };
            synthesis
                .run_with_approval_input(
                    &mut child_session,
                    request.child_input.clone(),
                    request.options.clone(),
                    &mut participant_handler,
                    approval_handler,
                )
                .await
        };
        let output = match synthesis_run {
            Ok(output) => output,
            Err(error) => {
                let error = self.retryable_zero_effect_error(
                    &request.attempt_id,
                    &request.child_input,
                    synthesis,
                    &child_session,
                    error,
                );
                self.supervisor.record_task_child_failure(
                    parent_session,
                    handler,
                    &child_thread,
                    format!("{error:#}"),
                )?;
                return Err(error);
            }
        };
        let postprocessed = (|| -> Result<TaskSynthesisSessionRunOutput> {
            let final_text = sigil_kernel::safe_persistence_text(&output.result.final_text);
            let final_answer_ref = output
                .result
                .final_message_id
                .as_ref()
                .map(|message_id| sigil_kernel::AgentFinalAnswerRef {
                    session_ref: request.child_session_ref.clone(),
                    message_id: message_id.clone(),
                    content_hash: hash_text(&final_text),
                    char_count: final_text.chars().count(),
                })
                .ok_or_else(|| anyhow::anyhow!("synthesis child did not persist a final answer"))?;
            let materialized = super::AgentResultMaterialization::inline(
                final_text.clone(),
                Some(final_answer_ref.clone()),
            );
            let outcome = output.outcome.clone();
            self.supervisor.record_task_child_result(
                parent_session,
                handler,
                &child_thread,
                request.child_session_ref.clone(),
                task_child_status_from_outcome(&final_text, &outcome),
                &materialized,
                &outcome,
                Some(usage_summary_from_stats(child_session.stats())),
            )?;
            Ok(TaskSynthesisSessionRunOutput {
                attempt_id: request.attempt_id.clone(),
                final_text,
                outcome,
                child_session_ref: request.child_session_ref.clone(),
                final_answer_ref,
                artifact_refs: Vec::new(),
            })
        })();
        match postprocessed {
            Ok(output) => Ok(output),
            Err(error) => {
                self.supervisor.record_task_child_failure(
                    parent_session,
                    handler,
                    &child_thread,
                    format!("{error:#}"),
                )?;
                Err(error)
            }
        }
    }
}

fn detached_task_batch_results<'a>(
    results: Vec<Result<TaskChildSessionRunOutput>>,
) -> TaskChildSessionBatchPreparation<'a> {
    let request_count = results.len();
    TaskChildSessionBatchPreparation::Detached(Box::pin(async move {
        Ok(TaskChildSessionBatchCommitEnvelope::new(
            request_count,
            move |_parent_session, _handler| Ok(results),
        ))
    }))
}

enum SettledRuntimeTaskChildBatch {
    Fallback(Vec<TaskChildSessionRunRequest>),
    Detached(TaskChildSessionBatchCommitEnvelope),
}

async fn settle_runtime_task_child_batch(
    preparation: TaskChildSessionBatchPreparation<'_>,
) -> Result<SettledRuntimeTaskChildBatch> {
    match preparation {
        TaskChildSessionBatchPreparation::Fallback(requests) => {
            Ok(SettledRuntimeTaskChildBatch::Fallback(requests))
        }
        TaskChildSessionBatchPreparation::Detached(batch_future) => batch_future
            .await
            .map(SettledRuntimeTaskChildBatch::Detached),
    }
}

fn rejected_parallel_read_batch(
    requests: &[TaskChildSessionRunRequest],
    error: anyhow::Error,
) -> Vec<Result<TaskChildSessionRunOutput>> {
    if let Some(retry) = error.downcast_ref::<TaskParticipantRetryError>() {
        return requests
            .iter()
            .map(|request| {
                let input_hash = task_participant_input_hash(&request.child_input)?;
                Err(anyhow::Error::new(TaskParticipantRetryError::new(
                    retry.retry_after_ms(),
                    retry.route_fingerprint(),
                    input_hash,
                    TaskParticipantRetryProof::AdmissionRejectedBeforeDispatch {
                        zero_output: true,
                        zero_tool: true,
                        zero_effect: true,
                    },
                    anyhow::Error::new(ProviderRouteCooldownError::new(
                        retry.retry_after_ms(),
                        retry.route_fingerprint(),
                    ))
                    .context("parallel task child batch rejected before provider dispatch"),
                )?))
            })
            .collect();
    }
    if let Some(cooldown) = error.downcast_ref::<ProviderRouteCooldownError>().cloned() {
        return (0..requests.len())
            .map(|_| {
                Err(anyhow::Error::new(cooldown.clone())
                    .context("parallel task child batch rejected before provider dispatch"))
            })
            .collect();
    }
    let reason = format!("parallel task child batch rejected before provider dispatch: {error:#}");
    (0..requests.len())
        .map(|_| Err(anyhow::anyhow!(reason.clone())))
        .collect()
}

fn retry_safe_step(step: &TaskStepSpec) -> bool {
    matches!(
        step.effective_mode(),
        TaskStepMode::Read | TaskStepMode::Review | TaskStepMode::Verify
    ) && step.effective_isolation() == sigil_kernel::TaskIsolationMode::SharedReadOnly
}

struct TaskParticipantEventHandler<'a, H> {
    inner: &'a mut H,
}

impl<H> EventHandler for TaskParticipantEventHandler<'_, H>
where
    H: EventHandler,
{
    fn handle(&mut self, event: RunEvent) -> Result<()> {
        match event {
            RunEvent::AssistantMessage(_)
            | RunEvent::TextDelta(_)
            | RunEvent::ReasoningDelta(_)
            | RunEvent::ContinuationState(_)
            | RunEvent::Control(_) => Ok(()),
            event => self.inner.handle(event),
        }
    }
}

struct SharedTaskEventHandler<'a, H> {
    inner: Arc<Mutex<&'a mut H>>,
}

impl<H> Clone for SharedTaskEventHandler<'_, H> {
    fn clone(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
        }
    }
}

impl<H> EventHandler for SharedTaskEventHandler<'_, H>
where
    H: EventHandler + Send,
{
    fn handle(&mut self, event: RunEvent) -> Result<()> {
        self.inner
            .lock()
            .map_err(|_| anyhow::anyhow!("task event handler lock poisoned"))?
            .handle(event)
    }
}

struct SharedTaskApprovalHandler<'a, A> {
    inner: Arc<Mutex<&'a mut A>>,
}

impl<A> Clone for SharedTaskApprovalHandler<'_, A> {
    fn clone(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
        }
    }
}

impl<A> ApprovalHandler for SharedTaskApprovalHandler<'_, A>
where
    A: ApprovalHandler + Send,
{
    fn approve_tool_call(&mut self, call: &ToolCall, spec: &ToolSpec) -> Result<ToolApproval> {
        self.inner
            .lock()
            .map_err(|_| anyhow::anyhow!("task approval handler lock poisoned"))?
            .approve_tool_call(call, spec)
    }

    fn approval_is_explicit_user_action(&self) -> bool {
        self.inner
            .lock()
            .map(|handler| handler.approval_is_explicit_user_action())
            .unwrap_or(false)
    }
}

#[allow(clippy::too_many_arguments)]
async fn run_task_child_agent_for_step<H, A>(
    agent: &BoxedAgent,
    child_session: &mut Session,
    child_input: AgentRunInput,
    options: AgentRunOptions,
    step: &TaskStepSpec,
    handler: &mut H,
    approval_handler: &mut A,
) -> Result<sigil_kernel::AgentRunOutput>
where
    H: EventHandler + Send,
    A: ApprovalHandler + Send,
{
    if step.effective_isolation() == sigil_kernel::TaskIsolationMode::ChangesetOnly {
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

struct BufferedSupervisorTaskApprovalRouteHandler<'a, A> {
    inner: &'a mut A,
    task_request: &'a TaskChildSessionRunRequest,
    child_session_ref: &'a SessionRef,
    source_thread_id: &'a AgentThreadId,
    controls: Vec<ControlEntry>,
}

impl<A> ApprovalHandler for BufferedSupervisorTaskApprovalRouteHandler<'_, A>
where
    A: ApprovalHandler,
{
    fn approval_is_explicit_user_action(&self) -> bool {
        self.inner.approval_is_explicit_user_action()
    }

    fn approve_tool_call(&mut self, call: &ToolCall, spec: &ToolSpec) -> Result<ToolApproval> {
        let task_route_id = task_route_id_for_call(
            &self.task_request.task.task_id,
            &self.task_request.step.step_id,
            &call.id,
        )?;
        let agent_route_id = agent_route_id_for_call(self.source_thread_id, &call.id)?;
        self.controls.extend([
            task_approval_route_control(
                self.task_request,
                self.child_session_ref,
                task_route_id.clone(),
                call,
                TaskRouteStatus::Requested,
            ),
            ControlEntry::AgentApprovalRoute(AgentApprovalRouteEntry {
                route_id: agent_route_id.clone(),
                source_thread_id: self.source_thread_id.clone(),
                target_thread_id: None,
                call_id: call.id.clone(),
                tool_name: call.name.clone(),
                status: AgentRouteStatus::Requested,
            }),
        ]);
        let approval = self.inner.approve_tool_call(call, spec)?;
        let (task_status, agent_status) = match approval {
            ToolApproval::Approve
            | ToolApproval::ApproveForSession
            | ToolApproval::ApproveWithArgs { .. } => {
                (TaskRouteStatus::Resolved, AgentRouteStatus::Resolved)
            }
            ToolApproval::Deny { .. } => (TaskRouteStatus::Rejected, AgentRouteStatus::Rejected),
        };
        self.controls.extend([
            task_approval_route_control(
                self.task_request,
                self.child_session_ref,
                task_route_id,
                call,
                task_status,
            ),
            ControlEntry::AgentApprovalRoute(AgentApprovalRouteEntry {
                route_id: agent_route_id,
                source_thread_id: self.source_thread_id.clone(),
                target_thread_id: None,
                call_id: call.id.clone(),
                tool_name: call.name.clone(),
                status: agent_status,
            }),
        ]);
        Ok(approval)
    }
}

struct SupervisorTaskApprovalRouteHandler<'a, A> {
    inner: &'a mut A,
    parent_session: &'a mut Session,
    task_request: &'a TaskChildSessionRunRequest,
    child_session_ref: &'a SessionRef,
    source_thread_id: &'a AgentThreadId,
}

impl<A> ApprovalHandler for SupervisorTaskApprovalRouteHandler<'_, A>
where
    A: ApprovalHandler,
{
    fn approval_is_explicit_user_action(&self) -> bool {
        self.inner.approval_is_explicit_user_action()
    }

    fn approve_tool_call(&mut self, call: &ToolCall, spec: &ToolSpec) -> Result<ToolApproval> {
        let task_route_id = task_route_id_for_call(
            &self.task_request.task.task_id,
            &self.task_request.step.step_id,
            &call.id,
        )?;
        let agent_route_id = agent_route_id_for_call(self.source_thread_id, &call.id)?;
        append_task_approval_route(
            self.parent_session,
            self.task_request,
            self.child_session_ref,
            &task_route_id,
            call,
            TaskRouteStatus::Requested,
        )?;
        self.parent_session
            .append_control(ControlEntry::AgentApprovalRoute(AgentApprovalRouteEntry {
                route_id: agent_route_id.clone(),
                source_thread_id: self.source_thread_id.clone(),
                target_thread_id: None,
                call_id: call.id.clone(),
                tool_name: call.name.clone(),
                status: AgentRouteStatus::Requested,
            }))?;
        let approval = self.inner.approve_tool_call(call, spec)?;
        let (task_status, agent_status) = match approval {
            ToolApproval::Approve
            | ToolApproval::ApproveForSession
            | ToolApproval::ApproveWithArgs { .. } => {
                (TaskRouteStatus::Resolved, AgentRouteStatus::Resolved)
            }
            ToolApproval::Deny { .. } => (TaskRouteStatus::Rejected, AgentRouteStatus::Rejected),
        };
        append_task_approval_route(
            self.parent_session,
            self.task_request,
            self.child_session_ref,
            &task_route_id,
            call,
            task_status,
        )?;
        self.parent_session
            .append_control(ControlEntry::AgentApprovalRoute(AgentApprovalRouteEntry {
                route_id: agent_route_id,
                source_thread_id: self.source_thread_id.clone(),
                target_thread_id: None,
                call_id: call.id.clone(),
                tool_name: call.name.clone(),
                status: agent_status,
            }))?;
        Ok(approval)
    }
}

fn append_task_child_session<H>(
    session: &mut Session,
    handler: &mut H,
    request: &TaskChildSessionRunRequest,
    child_task_id: &TaskId,
    child_session_ref: &SessionRef,
    status: TaskChildSessionStatus,
    summary_hash: Option<String>,
) -> Result<()>
where
    H: EventHandler + Send + ?Sized,
{
    append_control(
        session,
        handler,
        ControlEntry::TaskChildSession(TaskChildSessionEntry {
            task_id: request.task.task_id.clone(),
            plan_version: request.plan_version,
            step_id: request.step.step_id.clone(),
            child_task_id: child_task_id.clone(),
            child_session_ref: child_session_ref.clone(),
            role: request.step.role,
            status,
            summary_hash,
        }),
    )
}

fn append_task_approval_route(
    session: &mut Session,
    request: &TaskChildSessionRunRequest,
    child_session_ref: &SessionRef,
    route_id: &TaskRouteId,
    call: &ToolCall,
    status: TaskRouteStatus,
) -> Result<()> {
    session.append_control(task_approval_route_control(
        request,
        child_session_ref,
        route_id.clone(),
        call,
        status,
    ))
}

fn task_approval_route_control(
    request: &TaskChildSessionRunRequest,
    child_session_ref: &SessionRef,
    route_id: TaskRouteId,
    call: &ToolCall,
    status: TaskRouteStatus,
) -> ControlEntry {
    ControlEntry::TaskSubagentApprovalRoute(TaskSubagentApprovalRouteEntry {
        route_id,
        task_id: request.task.task_id.clone(),
        plan_version: request.plan_version,
        step_id: request.step.step_id.clone(),
        role: request.step.role,
        child_session_ref: child_session_ref.clone(),
        call_id: call.id.clone(),
        tool_name: call.name.clone(),
        status,
    })
}

pub(super) fn build_child_session(
    parent_session: &Session,
    child_session_ref: &SessionRef,
) -> Result<Session> {
    if let Some(parent_path) = parent_session.store_path() {
        let parent_dir = parent_path.parent().unwrap_or_else(|| Path::new("."));
        let store = JsonlSessionStore::new(child_session_ref.resolve(parent_dir))?;
        let mut session = Session::load_from_store(
            parent_session.provider_name(),
            parent_session.model_name(),
            store,
        )?;
        crate::attach_session_url_capability_store(&mut session)?;
        return Ok(session);
    }
    let mut session = Session::new(parent_session.provider_name(), parent_session.model_name());
    crate::attach_session_url_capability_store(&mut session)?;
    Ok(session)
}

pub(crate) fn task_child_status_from_outcome(
    final_text: &str,
    outcome: &sigil_kernel::AgentRunOutcome,
) -> TaskChildSessionStatus {
    if outcome.terminal_reason == sigil_kernel::AgentRunTerminalReason::MaxTurns
        || !outcome.interrupted_tool_calls.is_empty()
    {
        TaskChildSessionStatus::Interrupted
    } else if outcome.approval_denials > 0
        || outcome.tool_errors.iter().any(|error| {
            matches!(
                error.kind,
                ToolErrorKind::ApprovalRequired
                    | ToolErrorKind::ApprovalDenied
                    | ToolErrorKind::PermissionDenied
                    | ToolErrorKind::PathOutsideWorkspace
                    | ToolErrorKind::ExternalDirectoryRequired
            )
        })
        || (!outcome.tool_errors.is_empty() && final_text.trim().is_empty())
    {
        TaskChildSessionStatus::Failed
    } else {
        TaskChildSessionStatus::Completed
    }
}

fn child_provider_capabilities(agent: &BoxedAgent) -> ProviderCapabilities {
    agent.provider_capabilities()
}

pub(super) fn usage_summary_from_stats(stats: &SessionStats) -> AgentUsageSummary {
    let input_tokens = stats.prompt_tokens;
    let output_tokens = stats.completion_tokens;
    AgentUsageSummary {
        input_tokens,
        output_tokens,
        total_tokens: input_tokens + output_tokens,
        cached_tokens: Some(stats.cache_hit_tokens),
    }
}

fn main_thread_id() -> Result<AgentThreadId> {
    AgentThreadId::new("main")
}

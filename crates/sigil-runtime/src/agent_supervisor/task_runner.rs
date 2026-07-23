use std::{path::Path, sync::Arc};

use anyhow::Result;
use async_trait::async_trait;
use sigil_kernel::{
    AgentApprovalRouteEntry, AgentInvocationMode, AgentInvocationSource, AgentRole,
    AgentRouteStatus, AgentRunInput, AgentRunOptions, AgentThreadId, AgentUsageSummary,
    ApprovalHandler, ControlEntry, EventHandler, JsonlSessionStore, MultiAgentMode,
    ProviderCapabilities, RunEvent, SequentialTaskRequest, Session, SessionRef, SessionStats,
    TaskChildSessionEntry, TaskChildSessionRunOutput, TaskChildSessionRunRequest,
    TaskChildSessionRunner, TaskChildSessionStatus, TaskId, TaskParticipantAttemptId,
    TaskPlannerSessionRunOutput, TaskPlannerSessionRunRequest, TaskRouteId, TaskRouteStatus,
    TaskStepId, TaskStepMode, TaskStepSpec, TaskSubagentApprovalRouteEntry,
    TaskSynthesisSessionRunOutput, TaskSynthesisSessionRunRequest, ToolApproval, ToolCall,
    ToolErrorKind, ToolSpec, changeset_only_child_tool_registry,
    decode_changeset_only_child_output, task_participant_child_task_id,
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
}

impl AgentSupervisorTaskChildRunner {
    pub fn new(
        supervisor: AgentSupervisor,
        subagent_read: BoxedAgent,
        subagent_write: BoxedAgent,
    ) -> Self {
        Self {
            supervisor,
            planner: None,
            executor: None,
            subagent_read: Arc::new(subagent_read),
            subagent_write: Arc::new(subagent_write),
            synthesis: None,
            planner_discovery_max_probes: 0,
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
        Self {
            supervisor,
            planner: Some(Arc::new(planner)),
            executor: Some(Arc::new(executor)),
            subagent_read: Arc::new(subagent_read),
            subagent_write: Arc::new(subagent_write),
            synthesis: Some(Arc::new(synthesis)),
            planner_discovery_max_probes: 0,
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
                    request.child_input,
                    request.options,
                    &mut participant_handler,
                    approval_handler,
                )
                .await
        };
        let output = match synthesis_run {
            Ok(output) => output,
            Err(error) => {
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
    H: EventHandler + Send,
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
    session.append_control(ControlEntry::TaskSubagentApprovalRoute(
        TaskSubagentApprovalRouteEntry {
            route_id: route_id.clone(),
            task_id: request.task.task_id.clone(),
            plan_version: request.plan_version,
            step_id: request.step.step_id.clone(),
            role: request.step.role,
            child_session_ref: child_session_ref.clone(),
            call_id: call.id.clone(),
            tool_name: call.name.clone(),
            status,
        },
    ))
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

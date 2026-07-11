use std::path::Path;

use anyhow::{Result, bail};
use async_trait::async_trait;
use sigil_kernel::{
    AgentApprovalRouteEntry, AgentInvocationMode, AgentInvocationSource, AgentRole,
    AgentRouteStatus, AgentRunInput, AgentRunOptions, AgentThreadId, AgentUsageSummary,
    ApprovalHandler, ControlEntry, EventHandler, JsonlSessionStore, ProviderCapabilities, RunEvent,
    Session, SessionRef, SessionStats, TaskChildSessionEntry, TaskChildSessionRunOutput,
    TaskChildSessionRunRequest, TaskChildSessionRunner, TaskChildSessionStatus, TaskId,
    TaskRouteId, TaskRouteStatus, TaskStepSpec, TaskSubagentApprovalRouteEntry, ToolApproval,
    ToolCall, ToolErrorKind, ToolSpec, changeset_only_child_tool_registry, child_session_ref,
    decode_changeset_only_child_output,
};

use super::{
    AgentSupervisor, AgentTaskChildStart, BoxedAgent, append_control, hash_text,
    ids::{agent_route_id_for_call, task_route_id_for_call},
    materialize_child_agent_final_answer,
};

/// Runtime child runner that connects kernel task orchestration to the supervisor.
pub struct AgentSupervisorTaskChildRunner {
    supervisor: AgentSupervisor,
    subagent_read: BoxedAgent,
    subagent_write: BoxedAgent,
}

impl AgentSupervisorTaskChildRunner {
    pub fn new(
        supervisor: AgentSupervisor,
        subagent_read: BoxedAgent,
        subagent_write: BoxedAgent,
    ) -> Self {
        Self {
            supervisor,
            subagent_read,
            subagent_write,
        }
    }
}

#[async_trait]
impl TaskChildSessionRunner for AgentSupervisorTaskChildRunner {
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
        if !matches!(
            request.step.role,
            AgentRole::SubagentRead | AgentRole::SubagentWrite
        ) {
            bail!("supervisor child runner requires a subagent role");
        }
        let child_task_id = TaskId::new(format!(
            "child_v{}_{}",
            request.plan_version,
            request.step.step_id.as_str()
        ))?;
        let child_session_ref =
            child_session_ref(&request.task.task_id, &request.step.step_id, &child_task_id)?;
        let agent = match request.step.role {
            AgentRole::SubagentRead => &self.subagent_read,
            AgentRole::SubagentWrite => &self.subagent_write,
            AgentRole::Planner | AgentRole::Executor => unreachable!("role checked above"),
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
        append_task_child_session(
            parent_session,
            handler,
            &request,
            &child_task_id,
            &child_session_ref,
            TaskChildSessionStatus::Started,
            None,
        )?;
        let mut child_session = build_child_session(parent_session, &child_session_ref)?;
        let mut route_handler = SupervisorTaskApprovalRouteHandler {
            inner: approval_handler,
            parent_session,
            task_request: &request,
            child_session_ref: &child_session_ref,
            source_thread_id: &child_thread.thread_id,
        };
        let child_input = request.child_input.clone();
        let options = request.options.clone();
        let output = match run_task_child_agent_for_step(
            agent,
            &mut child_session,
            child_input,
            options,
            &request.step,
            handler,
            &mut route_handler,
        )
        .await
        {
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
        let changeset_proposal = if request.step.effective_isolation()
            == sigil_kernel::TaskIsolationMode::ChangesetOnly
        {
            match decode_changeset_only_child_output(&output.result.final_text) {
                Ok(proposal) => Some(proposal),
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
            }
        } else {
            None
        };
        let changeset_only_after_snapshot_id =
            if let Some(base_snapshot_id) = request.changeset_only_base_snapshot_id.as_deref() {
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
            final_text: materialized.final_text,
            outcome,
            changeset_proposal,
            changeset_only_after_snapshot_id,
        })
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

fn build_child_session(
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

fn usage_summary_from_stats(stats: &SessionStats) -> AgentUsageSummary {
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

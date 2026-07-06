use super::scheduler::has_blocking_tool_error;
use super::*;

pub(crate) struct TestAgentTaskChildSessionRunner {
    subagent_read: BoxedAgent,
    subagent_write: BoxedAgent,
}

impl TestAgentTaskChildSessionRunner {
    pub fn new(subagent_read: BoxedAgent, subagent_write: BoxedAgent) -> Self {
        Self {
            subagent_read,
            subagent_write,
        }
    }
}

#[async_trait]
impl TaskChildSessionRunner for TestAgentTaskChildSessionRunner {
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
            ToolApproval::Approve
            | ToolApproval::ApproveForSession
            | ToolApproval::ApproveWithArgs { .. } => TaskRouteStatus::Resolved,
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

pub(super) fn child_status_from_output(output: &StepRunOutput) -> TaskChildSessionStatus {
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

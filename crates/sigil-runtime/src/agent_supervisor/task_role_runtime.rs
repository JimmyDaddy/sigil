use std::sync::Arc;

use anyhow::{Context, Result};
use async_trait::async_trait;
use sigil_kernel::verification::VerificationExecutionPortV1;
use sigil_kernel::{
    AgentRole, AgentRouteStatus, AgentRunOptions, AgentUserInputRouteEntryV1, ControlEntry,
    Provider, RootConfig, SequentialTaskOrchestrator, Session, TaskConfig,
    TaskParticipantAttemptStatus, TaskParticipantPurpose, TaskRunStatus, TaskStepStatus,
    ToolRegistry, UserInputDecisionCommandV1, UserInputDecisionReceiptV1, UserInputSourceV1,
};

use super::{AgentSupervisor, AgentSupervisorTaskChildRunner};

/// Provider construction seam shared by TUI, application adapters, and evaluation harnesses.
#[async_trait]
pub trait TaskRoleProviderBuilder: Send + Sync {
    async fn build(&self, root_config: &RootConfig, role: AgentRole) -> Result<Box<dyn Provider>>;
}

/// Default role-provider builder backed by the configured runtime provider registry.
pub struct RuntimeTaskRoleProviderBuilder;

#[async_trait]
impl TaskRoleProviderBuilder for RuntimeTaskRoleProviderBuilder {
    async fn build(&self, root_config: &RootConfig, role: AgentRole) -> Result<Box<dyn Provider>> {
        crate::build_role_provider_async(root_config, role).await
    }
}

/// Fully assembled role-specific runtime for one durable task.
pub struct TaskRoleRuntime {
    pub orchestrator: SequentialTaskOrchestrator<AgentSupervisorTaskChildRunner>,
    pub planner_options: AgentRunOptions,
    pub executor_options: AgentRunOptions,
    pub subagent_read_options: AgentRunOptions,
    pub subagent_write_options: AgentRunOptions,
}

/// Fully prepared planner-answer continuation. Construction finishes every provider and role-tool
/// failure point before the child-session answer is durably accepted.
pub struct PreparedTaskPlannerUserInputContinuation {
    pub runtime: TaskRoleRuntime,
    pub receipt: UserInputDecisionReceiptV1,
    pub route: AgentUserInputRouteEntryV1,
}

/// Roles required by one task execution segment.  A planned continuation may carry a much
/// smaller set than the initial planner run; keeping the demand explicit prevents unrelated role
/// credentials from becoming a preflight dependency.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct TaskRoleDemand {
    pub(super) planner: bool,
    pub(super) executor: bool,
    pub(super) subagent_read: bool,
    pub(super) subagent_write: bool,
    pub(super) synthesis: bool,
}

impl TaskRoleDemand {
    pub(super) const fn all() -> Self {
        Self {
            planner: true,
            executor: true,
            subagent_read: true,
            subagent_write: true,
            synthesis: true,
        }
    }

    pub(super) const fn planner_only() -> Self {
        Self {
            planner: true,
            executor: false,
            subagent_read: false,
            subagent_write: false,
            synthesis: false,
        }
    }

    pub(super) const fn executor_only() -> Self {
        Self {
            planner: false,
            executor: true,
            subagent_read: false,
            subagent_write: false,
            synthesis: false,
        }
    }
}

/// Builds every task role, validates the exact parent/child route, then durably accepts one
/// submitted planner answer as the final fallible preparation step.
pub async fn prepare_task_planner_user_input_continuation(
    root_config: &RootConfig,
    options: &AgentRunOptions,
    base_registry: &ToolRegistry,
    agent_supervisor: AgentSupervisor,
    role_provider_builder: &dyn TaskRoleProviderBuilder,
    verification_execution_port: Arc<dyn VerificationExecutionPortV1>,
    parent_session: &mut Session,
    route: &AgentUserInputRouteEntryV1,
    command: &UserInputDecisionCommandV1,
) -> Result<PreparedTaskPlannerUserInputContinuation> {
    validate_task_planner_user_input_route(parent_session, route)?;
    if route.request.identity != command.identity
        || route.request.request_hash != command.request_hash
        || !matches!(
            command.decision,
            sigil_kernel::UserInputDecisionV1::Submitted { .. }
        )
    {
        anyhow::bail!("task planner answer does not match its submitted durable route");
    }
    let runtime = build_task_role_runtime_for_route_with_demand(
        root_config,
        options,
        base_registry,
        agent_supervisor,
        role_provider_builder,
        verification_execution_port,
        super::task_execution::ResolvedTaskExecutionRoute::NeedsPlanning,
        TaskRoleDemand::planner_only(),
    )
    .await?;
    let mut child = super::build_child_session(parent_session, &route.child_session_ref)?;
    sigil_kernel::preview_user_input_decision(&child, command, crate::current_unix_time_ms())?;
    let receipt = sigil_kernel::accept_user_input_decision(
        &mut child,
        command.clone(),
        crate::current_unix_time_ms(),
    )?;
    let mut accepted_route = route.clone();
    accepted_route.request = receipt.request.clone();
    accepted_route.updated_at_unix_ms = crate::current_unix_time_ms();
    parent_session.append_control(ControlEntry::AgentUserInputRoute(accepted_route.clone()))?;
    Ok(PreparedTaskPlannerUserInputContinuation {
        runtime,
        receipt,
        route: accepted_route,
    })
}

/// Validates the immutable parent facts needed to resume an initial planner transcript.
pub fn validate_task_planner_user_input_route(
    session: &Session,
    route: &AgentUserInputRouteEntryV1,
) -> Result<()> {
    route.validate()?;
    if !matches!(
        route.status,
        AgentRouteStatus::Requested | AgentRouteStatus::Registered
    ) {
        anyhow::bail!("task planner user-input route is no longer pending");
    }
    let task_id = match &route.request.source {
        UserInputSourceV1::Planner { task_id } => task_id,
        _ => anyhow::bail!("user-input route is not owned by a task planner"),
    };
    if task_id != &route.budget_scope_id {
        anyhow::bail!("task planner user-input route has a mismatched task binding");
    }
    let projection = session.task_state_projection();
    let task = projection
        .tasks
        .get(task_id)
        .context("task planner user-input route references an unknown task")?;
    if task.status != TaskRunStatus::Paused || task.latest_plan_version.is_some() {
        anyhow::bail!("task planner answer requires an unplanned paused task");
    }
    let matching = task
        .participant_attempts_for(TaskParticipantPurpose::Planner, None, None)
        .into_iter()
        .filter(|attempt| {
            attempt.status == TaskParticipantAttemptStatus::Started
                && attempt.child_session_ref == route.child_session_ref
        })
        .count();
    if matching != 1 {
        anyhow::bail!("task planner user-input route has no unique started participant");
    }
    Ok(())
}

/// Applies a decline/cancel decision to the authoritative planner child and atomically closes the
/// parent route, participant and task lifecycle without starting a provider continuation.
pub fn settle_task_planner_user_input_without_continuation(
    parent_session: &mut Session,
    route: &AgentUserInputRouteEntryV1,
    command: UserInputDecisionCommandV1,
) -> Result<(UserInputDecisionReceiptV1, Vec<ControlEntry>)> {
    validate_task_planner_user_input_route(parent_session, route)?;
    if matches!(
        command.decision,
        sigil_kernel::UserInputDecisionV1::Submitted { .. }
    ) {
        anyhow::bail!("submitted planner answers require a supervised continuation");
    }
    let mut child = super::build_child_session(parent_session, &route.child_session_ref)?;
    sigil_kernel::preview_user_input_decision(&child, &command, crate::current_unix_time_ms())?;
    let receipt = sigil_kernel::accept_user_input_decision(
        &mut child,
        command.clone(),
        crate::current_unix_time_ms(),
    )?;
    let task = parent_session
        .task_state_projection()
        .tasks
        .get(&route.budget_scope_id)
        .cloned()
        .context("task planner settlement lost its task")?;
    let mut attempts = task
        .participant_attempts_for(TaskParticipantPurpose::Planner, None, None)
        .into_iter()
        .filter(|attempt| {
            attempt.status == TaskParticipantAttemptStatus::Started
                && attempt.child_session_ref == route.child_session_ref
        });
    let mut attempt = attempts
        .next()
        .cloned()
        .context("task planner settlement lost its participant")?;
    if attempts.next().is_some() {
        anyhow::bail!("task planner settlement has multiple started participants");
    }
    let cancelled = matches!(
        command.decision,
        sigil_kernel::UserInputDecisionV1::RunCancelled
    );
    let reason = if cancelled {
        "task planning cancelled by user"
    } else {
        "task planner question declined by user"
    };
    attempt.status = if cancelled {
        TaskParticipantAttemptStatus::Cancelled
    } else {
        TaskParticipantAttemptStatus::Interrupted
    };
    attempt.reason = Some(reason.to_owned());
    let mut route_update = route.clone();
    route_update.request = receipt.request.clone();
    route_update.status = if cancelled {
        sigil_kernel::AgentRouteStatus::Cancelled
    } else {
        sigil_kernel::AgentRouteStatus::Resolved
    };
    route_update.updated_at_unix_ms = crate::current_unix_time_ms();
    let controls = vec![
        ControlEntry::AgentUserInputRoute(route_update),
        ControlEntry::AgentThreadStatusChanged(sigil_kernel::AgentThreadStatusChangedEntry {
            thread_id: route.source_thread_id.clone(),
            status: if cancelled {
                sigil_kernel::AgentThreadStatus::Cancelled
            } else {
                sigil_kernel::AgentThreadStatus::Interrupted
            },
            reason: Some(reason.to_owned()),
            updated_at_ms: Some(crate::current_unix_time_ms()),
        }),
        ControlEntry::TaskParticipantAttempt(attempt),
        ControlEntry::TaskRun(sigil_kernel::TaskRunEntry {
            task_id: task.task_id,
            parent_session_ref: task.parent_session_ref,
            objective: task.objective,
            title: None,
            status: if cancelled {
                TaskRunStatus::Cancelled
            } else {
                TaskRunStatus::Paused
            },
            reason: Some(if cancelled {
                reason.to_owned()
            } else {
                "task planner question declined; explicit continuation may retry planning"
                    .to_owned()
            }),
        }),
    ];
    parent_session.append_controls(controls.clone())?;
    Ok((receipt, controls))
}

/// Builds the provider-neutral task runtime shared by every product adapter.
///
/// # Errors
///
/// Returns an error when any configured role provider, scoped tool registry, or execution backend
/// cannot be constructed before task participant dispatch.
pub async fn build_task_role_runtime(
    root_config: &RootConfig,
    options: &AgentRunOptions,
    base_registry: &ToolRegistry,
    agent_supervisor: AgentSupervisor,
    role_provider_builder: &dyn TaskRoleProviderBuilder,
    verification_execution_port: Arc<dyn VerificationExecutionPortV1>,
) -> Result<TaskRoleRuntime> {
    build_task_role_runtime_for_route_with_demand(
        root_config,
        options,
        base_registry,
        agent_supervisor,
        role_provider_builder,
        verification_execution_port,
        super::task_execution::ResolvedTaskExecutionRoute::Planned,
        TaskRoleDemand::all(),
    )
    .await
}

pub(super) async fn build_task_role_runtime_for_route(
    root_config: &RootConfig,
    options: &AgentRunOptions,
    base_registry: &ToolRegistry,
    agent_supervisor: AgentSupervisor,
    role_provider_builder: &dyn TaskRoleProviderBuilder,
    verification_execution_port: Arc<dyn VerificationExecutionPortV1>,
    route: super::task_execution::ResolvedTaskExecutionRoute,
) -> Result<TaskRoleRuntime> {
    let demand = if route == super::task_execution::ResolvedTaskExecutionRoute::Direct {
        TaskRoleDemand::executor_only()
    } else {
        TaskRoleDemand::all()
    };
    build_task_role_runtime_for_route_with_demand(
        root_config,
        options,
        base_registry,
        agent_supervisor,
        role_provider_builder,
        verification_execution_port,
        route,
        demand,
    )
    .await
}

pub(super) async fn build_task_role_runtime_for_route_with_demand(
    root_config: &RootConfig,
    options: &AgentRunOptions,
    base_registry: &ToolRegistry,
    agent_supervisor: AgentSupervisor,
    role_provider_builder: &dyn TaskRoleProviderBuilder,
    verification_execution_port: Arc<dyn VerificationExecutionPortV1>,
    route: super::task_execution::ResolvedTaskExecutionRoute,
    mut demand: TaskRoleDemand,
) -> Result<TaskRoleRuntime> {
    if route == super::task_execution::ResolvedTaskExecutionRoute::Direct {
        demand = TaskRoleDemand::executor_only();
    }
    // Planner discovery is an actual read-role dispatch.  Include that role whenever the
    // configured planner can use discovery, while still leaving the write role optional.
    if demand.planner
        && root_config.task.multi_agent_mode != sigil_kernel::MultiAgentMode::None
        && root_config.task.max_planning_research_agents > 0
    {
        demand.subagent_read = true;
    }

    let build_agent = async |role: AgentRole, tools: ToolRegistry| -> Result<super::BoxedAgent> {
        let provider = build_role_provider(role_provider_builder, root_config, role).await?;
        crate::configured_agent(root_config, provider, tools)
    };
    let executor = if demand.executor {
        Some(
            build_agent(
                AgentRole::Executor,
                crate::build_role_tool_registry(base_registry, root_config, AgentRole::Executor)
                    .into_registry(),
            )
            .await?,
        )
    } else {
        None
    };
    let planner = if demand.planner {
        Some(
            build_agent(
                AgentRole::Planner,
                crate::build_role_tool_registry(base_registry, root_config, AgentRole::Planner)
                    .into_registry(),
            )
            .await?,
        )
    } else {
        None
    };
    let subagent_read = if demand.subagent_read {
        Some(
            build_agent(
                AgentRole::SubagentRead,
                crate::build_role_tool_registry(
                    base_registry,
                    root_config,
                    AgentRole::SubagentRead,
                )
                .into_registry(),
            )
            .await?,
        )
    } else {
        None
    };
    let subagent_write = if demand.subagent_write {
        Some(
            build_agent(
                AgentRole::SubagentWrite,
                crate::build_role_tool_registry(
                    base_registry,
                    root_config,
                    AgentRole::SubagentWrite,
                )
                .into_registry(),
            )
            .await?,
        )
    } else {
        None
    };
    let synthesis = if demand.synthesis {
        Some(build_agent(AgentRole::Planner, ToolRegistry::new()).await?)
    } else {
        None
    };
    let child_runner = AgentSupervisorTaskChildRunner::new_with_available_task_roles(
        agent_supervisor,
        planner,
        executor,
        subagent_read,
        subagent_write,
        synthesis,
    )
    .with_provider_route_concurrency_limit(configured_provider_route_concurrency_limit(
        &root_config.task,
    ))
    .with_planner_discovery_policy(
        root_config.task.multi_agent_mode,
        root_config.task.max_planning_research_agents,
    )
    .with_integration_verification_port(verification_execution_port.clone());
    let workspace_root = options.workspace_root.clone();
    let interaction_mode = options.interaction_mode;
    // Role-specific options are derived from the persisted role configuration, but the
    // authority attachments belong to the already-admitted root run.  Dropping this field here
    // makes a direct Task look correctly bootstrapped until its first managed file tool reaches
    // the physical execution boundary, where it is (correctly) rejected as authority-less.
    let tool_authority = options.tool_authority.clone();
    let mut planner_options = crate::build_role_run_options(
        root_config,
        workspace_root.clone(),
        interaction_mode,
        AgentRole::Planner,
    );
    let mut executor_options = crate::build_role_run_options(
        root_config,
        workspace_root.clone(),
        interaction_mode,
        AgentRole::Executor,
    );
    let mut subagent_read_options = crate::build_role_run_options(
        root_config,
        workspace_root.clone(),
        interaction_mode,
        AgentRole::SubagentRead,
    );
    let mut subagent_write_options = crate::build_role_run_options(
        root_config,
        workspace_root,
        interaction_mode,
        AgentRole::SubagentWrite,
    );
    if let Some(tool_authority) = tool_authority {
        planner_options = planner_options.with_tool_authority(Arc::clone(&tool_authority));
        executor_options = executor_options.with_tool_authority(Arc::clone(&tool_authority));
        subagent_read_options =
            subagent_read_options.with_tool_authority(Arc::clone(&tool_authority));
        subagent_write_options = subagent_write_options.with_tool_authority(tool_authority);
    }
    Ok(TaskRoleRuntime {
        orchestrator: SequentialTaskOrchestrator::new_with_child_runner(child_runner)
            .with_max_parallel_read_steps(configured_max_parallel_read_steps(&root_config.task))
            .with_max_parallel_changeset_steps(configured_max_parallel_changeset_steps(
                &root_config.task,
            ))
            .with_verification_execution_port(verification_execution_port),
        planner_options,
        executor_options,
        subagent_read_options,
        subagent_write_options,
    })
}

/// Derives the role set for a continuation from the accepted plan's unfinished steps.  Planner is
/// requested by the caller only when guidance/replanning needs it; synthesis remains required for
/// every accepted plan because it is the owner of the final task settlement.
pub(super) fn task_role_demand_for_continuation(
    session: &Session,
    task_id: &sigil_kernel::TaskId,
    planner_required: bool,
) -> Result<TaskRoleDemand> {
    let projection = session.task_state_projection();
    let task = projection
        .tasks
        .get(task_id)
        .context("task role demand references an unknown task")?;
    let plan_version = task
        .latest_plan_version
        .context("planned continuation is missing its accepted plan")?;
    let plan = task
        .plans
        .get(&plan_version)
        .context("planned continuation is missing its plan projection")?;
    let mut demand = TaskRoleDemand {
        planner: planner_required,
        executor: false,
        subagent_read: false,
        subagent_write: false,
        // Final synthesis is a separate planner-provider transcript and may be reached after the
        // currently pending step batch completes, so it is part of the accepted-plan contract.
        synthesis: true,
    };
    for step in &plan.steps {
        let completed = task
            .steps
            .get(&(plan_version, step.step_id.clone()))
            .is_some_and(|status| {
                matches!(
                    status.status,
                    TaskStepStatus::Completed | TaskStepStatus::Superseded
                )
            });
        if completed {
            continue;
        }
        match step.role {
            AgentRole::Planner => demand.planner = true,
            AgentRole::Executor => demand.executor = true,
            AgentRole::SubagentRead => demand.subagent_read = true,
            AgentRole::SubagentWrite => demand.subagent_write = true,
        }
    }
    Ok(demand)
}

async fn build_role_provider(
    builder: &dyn TaskRoleProviderBuilder,
    root_config: &RootConfig,
    role: AgentRole,
) -> Result<Box<dyn Provider>> {
    builder
        .build(root_config, role)
        .await
        .with_context(|| format!("failed to build {} task provider", role.as_str()))
}

#[must_use]
pub fn configured_max_parallel_read_steps(config: &TaskConfig) -> usize {
    config.max_parallel_read_steps.max(1)
}

#[must_use]
pub fn configured_max_parallel_changeset_steps(config: &TaskConfig) -> usize {
    config.max_parallel_changeset_steps.max(1)
}

#[must_use]
pub fn configured_provider_route_concurrency_limit(config: &TaskConfig) -> usize {
    configured_max_parallel_read_steps(config).max(configured_max_parallel_changeset_steps(config))
}

#[cfg(test)]
#[path = "tests/task_role_runtime_tests.rs"]
mod tests;

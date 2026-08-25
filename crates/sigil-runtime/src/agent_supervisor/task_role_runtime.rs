use std::sync::Arc;

use anyhow::{Context, Result};
use async_trait::async_trait;
use sigil_kernel::verification::VerificationExecutionPortV1;
use sigil_kernel::{
    AgentRole, AgentRouteStatus, AgentRunOptions, AgentUserInputRouteEntryV1, ControlEntry,
    Provider, RootConfig, SequentialTaskOrchestrator, Session, TaskConfig,
    TaskParticipantAttemptStatus, TaskParticipantPurpose, TaskRunStatus, ToolRegistry,
    UserInputDecisionCommandV1, UserInputDecisionReceiptV1, UserInputSourceV1,
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
    let runtime = build_task_role_runtime(
        root_config,
        options,
        base_registry,
        agent_supervisor,
        role_provider_builder,
        verification_execution_port,
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
    let planner_provider =
        build_role_provider(role_provider_builder, root_config, AgentRole::Planner).await?;
    let executor_provider =
        build_role_provider(role_provider_builder, root_config, AgentRole::Executor).await?;
    let synthesis_provider =
        build_role_provider(role_provider_builder, root_config, AgentRole::Planner).await?;
    let subagent_read_provider =
        build_role_provider(role_provider_builder, root_config, AgentRole::SubagentRead).await?;
    let subagent_write_provider =
        build_role_provider(role_provider_builder, root_config, AgentRole::SubagentWrite).await?;
    let planner_registry =
        crate::build_role_tool_registry(base_registry, root_config, AgentRole::Planner)
            .into_registry();
    let executor_registry =
        crate::build_role_tool_registry(base_registry, root_config, AgentRole::Executor)
            .into_registry();
    let subagent_read_registry =
        crate::build_role_tool_registry(base_registry, root_config, AgentRole::SubagentRead)
            .into_registry();
    let subagent_write_registry =
        crate::build_role_tool_registry(base_registry, root_config, AgentRole::SubagentWrite)
            .into_registry();
    let workspace_root = options.workspace_root.clone();
    let interaction_mode = options.interaction_mode;
    let child_runner = AgentSupervisorTaskChildRunner::new_with_task_roles(
        agent_supervisor,
        crate::configured_agent(root_config, planner_provider, planner_registry)?,
        crate::configured_agent(root_config, executor_provider, executor_registry)?,
        crate::configured_agent(root_config, subagent_read_provider, subagent_read_registry)?,
        crate::configured_agent(
            root_config,
            subagent_write_provider,
            subagent_write_registry,
        )?,
        crate::configured_agent(root_config, synthesis_provider, ToolRegistry::new())?,
    )
    .with_provider_route_concurrency_limit(configured_provider_route_concurrency_limit(
        &root_config.task,
    ))
    .with_planner_discovery_policy(
        root_config.task.multi_agent_mode,
        root_config.task.max_planning_research_agents,
    )
    .with_integration_verification_port(verification_execution_port.clone());
    Ok(TaskRoleRuntime {
        orchestrator: SequentialTaskOrchestrator::new_with_child_runner(child_runner)
            .with_max_parallel_read_steps(configured_max_parallel_read_steps(&root_config.task))
            .with_max_parallel_changeset_steps(configured_max_parallel_changeset_steps(
                &root_config.task,
            ))
            .with_verification_execution_port(verification_execution_port),
        planner_options: crate::build_role_run_options(
            root_config,
            workspace_root.clone(),
            interaction_mode,
            AgentRole::Planner,
        ),
        executor_options: crate::build_role_run_options(
            root_config,
            workspace_root.clone(),
            interaction_mode,
            AgentRole::Executor,
        ),
        subagent_read_options: crate::build_role_run_options(
            root_config,
            workspace_root.clone(),
            interaction_mode,
            AgentRole::SubagentRead,
        ),
        subagent_write_options: crate::build_role_run_options(
            root_config,
            workspace_root,
            interaction_mode,
            AgentRole::SubagentWrite,
        ),
    })
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

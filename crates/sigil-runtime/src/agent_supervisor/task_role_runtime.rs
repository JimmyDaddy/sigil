use anyhow::{Context, Result};
use sigil_kernel::{
    Agent, AgentRole, AgentRunOptions, Provider, RootConfig, SequentialTaskOrchestrator,
    TaskConfig, ToolRegistry,
};

use super::{AgentSupervisor, AgentSupervisorTaskChildRunner};

/// Provider construction seam shared by TUI, application adapters, and evaluation harnesses.
pub trait TaskRoleProviderBuilder: Send + Sync {
    fn build(&self, root_config: &RootConfig, role: AgentRole) -> Result<Box<dyn Provider>>;
}

/// Default role-provider builder backed by the configured runtime provider registry.
pub struct RuntimeTaskRoleProviderBuilder;

impl TaskRoleProviderBuilder for RuntimeTaskRoleProviderBuilder {
    fn build(&self, root_config: &RootConfig, role: AgentRole) -> Result<Box<dyn Provider>> {
        crate::build_role_provider(root_config, role)
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

/// Builds the provider-neutral task runtime shared by every product adapter.
///
/// # Errors
///
/// Returns an error when any configured role provider, scoped tool registry, or execution backend
/// cannot be constructed before task participant dispatch.
pub fn build_task_role_runtime(
    root_config: &RootConfig,
    options: &AgentRunOptions,
    base_registry: &ToolRegistry,
    agent_supervisor: AgentSupervisor,
    role_provider_builder: &dyn TaskRoleProviderBuilder,
) -> Result<TaskRoleRuntime> {
    let planner_provider =
        build_role_provider(role_provider_builder, root_config, AgentRole::Planner)?;
    let executor_provider =
        build_role_provider(role_provider_builder, root_config, AgentRole::Executor)?;
    let synthesis_provider =
        build_role_provider(role_provider_builder, root_config, AgentRole::Planner)?;
    let subagent_read_provider =
        build_role_provider(role_provider_builder, root_config, AgentRole::SubagentRead)?;
    let subagent_write_provider =
        build_role_provider(role_provider_builder, root_config, AgentRole::SubagentWrite)?;
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
    let execution_backend = crate::build_configured_execution_backend(root_config)
        .context("failed to build task verification execution backend")?;
    let child_runner = AgentSupervisorTaskChildRunner::new_with_task_roles(
        agent_supervisor,
        Agent::new(planner_provider, planner_registry),
        Agent::new(executor_provider, executor_registry),
        Agent::new(subagent_read_provider, subagent_read_registry),
        Agent::new(subagent_write_provider, subagent_write_registry),
        Agent::new(synthesis_provider, ToolRegistry::new()),
    )
    .with_provider_route_concurrency_limit(configured_provider_route_concurrency_limit(
        &root_config.task,
    ))
    .with_planner_discovery_policy(
        root_config.task.multi_agent_mode,
        root_config.task.max_planning_research_agents,
    )
    .with_integration_verification_backend(execution_backend.clone());
    Ok(TaskRoleRuntime {
        orchestrator: SequentialTaskOrchestrator::new_with_child_runner(child_runner)
            .with_max_parallel_read_steps(configured_max_parallel_read_steps(&root_config.task))
            .with_max_parallel_changeset_steps(configured_max_parallel_changeset_steps(
                &root_config.task,
            ))
            .with_execution_backend(execution_backend),
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

fn build_role_provider(
    builder: &dyn TaskRoleProviderBuilder,
    root_config: &RootConfig,
    role: AgentRole,
) -> Result<Box<dyn Provider>> {
    builder
        .build(root_config, role)
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

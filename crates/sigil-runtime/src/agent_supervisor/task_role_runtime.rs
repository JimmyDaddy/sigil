use std::sync::Arc;

use anyhow::{Context, Result};
use async_trait::async_trait;
use sigil_kernel::verification::VerificationExecutionPortV1;
use sigil_kernel::{
    AgentRole, AgentRunOptions, DirectTaskRuntime, Provider, RootConfig, TaskConfig, ToolRegistry,
};

use super::{AgentSupervisor, AgentSupervisorTaskChildRunner};

/// Provider construction seam shared by product adapters.
#[async_trait]
pub trait TaskRoleProviderBuilder: Send + Sync {
    async fn build(&self, root_config: &RootConfig, role: AgentRole) -> Result<Box<dyn Provider>>;
}

pub struct RuntimeTaskRoleProviderBuilder;

#[async_trait]
impl TaskRoleProviderBuilder for RuntimeTaskRoleProviderBuilder {
    async fn build(&self, root_config: &RootConfig, role: AgentRole) -> Result<Box<dyn Provider>> {
        crate::build_role_provider_async(root_config, role).await
    }
}

/// Direct Task runtime. The root model owns the loop and invokes agent tools when it chooses to
/// delegate.
pub struct TaskRoleRuntime {
    pub direct_task_runtime: DirectTaskRuntime<AgentSupervisorTaskChildRunner>,
    pub executor_options: AgentRunOptions,
}

#[must_use]
pub fn configured_provider_route_concurrency_limit(config: &TaskConfig) -> usize {
    config.max_concurrent_provider_routes.max(1)
}

pub async fn build_task_role_runtime(
    root_config: &RootConfig,
    options: &AgentRunOptions,
    base_registry: &ToolRegistry,
    agent_supervisor: AgentSupervisor,
    role_provider_builder: &dyn TaskRoleProviderBuilder,
    verification_execution_port: Arc<dyn VerificationExecutionPortV1>,
) -> Result<TaskRoleRuntime> {
    // A direct Task has its own model-owned execution loop. Its root model must therefore receive
    // the agent-tool schemas in the same registry used to resolve those calls; keeping only the
    // delegate runtime wired (without registering the schemas) makes spawn/read/wait impossible
    // for the model to select.
    let mut task_registry = base_registry.clone();
    crate::register_agent_tools(&mut task_registry, root_config)?;
    let provider = role_provider_builder
        .build(root_config, AgentRole::Executor)
        .await
        .context("failed to build direct Task executor provider")?;
    let executor = crate::configured_agent(
        root_config,
        provider,
        crate::build_role_tool_registry(&task_registry, root_config, AgentRole::Executor)
            .into_registry(),
    )?;
    let direct_agent_tool_runtime =
        crate::AgentToolRuntime::new(agent_supervisor.clone(), root_config.clone(), task_registry);
    let child_runner =
        AgentSupervisorTaskChildRunner::new_with_executor(agent_supervisor, executor)
            .with_direct_agent_tool_runtime(direct_agent_tool_runtime)
            .with_provider_route_concurrency_limit(configured_provider_route_concurrency_limit(
                &root_config.task,
            ));
    let executor_options = crate::build_role_run_options(
        root_config,
        options.workspace_root.clone(),
        options.interaction_mode,
        AgentRole::Executor,
    );
    let executor_options = match options.tool_authority.clone() {
        Some(authority) => executor_options.with_tool_authority(authority),
        None => executor_options,
    };
    let direct_task_runtime = DirectTaskRuntime::new_with_child_runner(child_runner)
        .with_verification_execution_port(verification_execution_port);
    Ok(TaskRoleRuntime {
        direct_task_runtime,
        executor_options,
    })
}

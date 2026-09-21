use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;
use sigil_kernel::{
    ApprovalHandler, EventHandler, Session, TaskChildSessionRunner,
    TaskDirectExecutionSessionRunOutput, TaskDirectExecutionSessionRunRequest,
};

use super::{AgentSupervisor, BoxedAgent};
use crate::provider_pressure::{
    TaskProviderPressure, TaskProviderRouteConsumer, wrap_task_agent_provider,
};

/// Runtime adapter for the model-owned Task root loop.
///
/// Task scheduling, continuation, and terminal authority live in the kernel and are driven by the
/// root model. This adapter only supplies the executor provider and its agent-tool delegate.
pub struct AgentSupervisorTaskChildRunner {
    executor: Option<Arc<BoxedAgent>>,
    provider_pressure: TaskProviderPressure,
    direct_agent_tool_runtime:
        Option<Arc<tokio::sync::Mutex<crate::agent_tools::AgentToolRuntime>>>,
}

impl AgentSupervisorTaskChildRunner {
    pub fn new_with_executor(supervisor: AgentSupervisor, executor: BoxedAgent) -> Self {
        let provider_pressure = supervisor.provider_pressure().clone();
        Self {
            executor: Some(Arc::new(wrap_task_agent_provider(
                executor,
                provider_pressure.clone(),
                TaskProviderRouteConsumer::Executor,
            ))),
            provider_pressure,
            direct_agent_tool_runtime: None,
        }
    }

    /// Installs the process-local delegate used by the root model's agent tools.
    pub fn with_direct_agent_tool_runtime(
        mut self,
        runtime: crate::agent_tools::AgentToolRuntime,
    ) -> Self {
        self.direct_agent_tool_runtime = Some(Arc::new(tokio::sync::Mutex::new(runtime)));
        self
    }

    /// Keeps the provider pressure bound explicit at the runtime seam.
    #[must_use]
    pub fn with_provider_route_concurrency_limit(self, max_concurrency: usize) -> Self {
        self.provider_pressure
            .set_max_concurrency(max_concurrency.max(1));
        self
    }
}

#[async_trait]
impl TaskChildSessionRunner for AgentSupervisorTaskChildRunner {
    async fn run_direct_execution_session<H, A>(
        &self,
        parent_session: &mut Session,
        request: TaskDirectExecutionSessionRunRequest,
        handler: &mut H,
        approval_handler: &mut A,
    ) -> Result<TaskDirectExecutionSessionRunOutput>
    where
        H: EventHandler + Send,
        A: ApprovalHandler + Send,
    {
        request.admission.validate()?;
        request.attempt.validate()?;
        if request.task.task_id != request.admission.task_id
            || request.attempt.task_id != request.task.task_id
            || request.attempt.admission_id != request.admission.admission_id
            || !request.admission.matches_objective(&request.task.objective)
        {
            anyhow::bail!("direct execution request does not match its durable Task authority");
        }
        let executor = self
            .executor
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("task executor role is not configured"))?;
        let direct_agent_tool_runtime = self
            .direct_agent_tool_runtime
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("direct Task agent-tool runtime is not configured"))?;
        let mut direct_agent_tool_runtime = direct_agent_tool_runtime.lock().await;
        let output = executor
            .run_with_approval_input_and_agent_delegate(
                parent_session,
                request.input,
                request.options,
                handler,
                approval_handler,
                &mut *direct_agent_tool_runtime,
            )
            .await?;
        Ok(TaskDirectExecutionSessionRunOutput {
            attempt_id: request.attempt.attempt_id,
            final_text: output.result.final_text,
            final_message_id: output.result.final_message_id,
            outcome: output.outcome,
            disposition: output.disposition,
        })
    }
}

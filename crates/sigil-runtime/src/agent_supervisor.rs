use std::sync::{Arc, Mutex};

use anyhow::{Result, anyhow};
use sigil_kernel::{Agent, AgentUsageSummary, Provider, ProviderCapabilities, TaskId};

use crate::AgentProfileRegistry;
use crate::provider_pressure::{TaskProviderPressure, TaskProviderRouteDiagnosticsSnapshot};
use crate::task_completion_progress::{
    TaskCompletionProgressRegistry, TaskCompletionProgressSnapshot,
};

mod batch;
mod begin;
mod budget;
mod control;
mod guard;
mod hash;
mod ids;
mod projection;
mod record;
mod task_discovery;
pub(crate) use task_discovery::{planner_tools_with_discovery, task_discovery_system_prompt};
pub mod task_execution;
pub mod task_role_runtime;
mod task_runner;
mod thread_ops;
mod thread_state;
pub use budget::AgentBudgetPolicy;
use control::{agent_terminal_status_from_task_child, append_control};
#[cfg(test)]
use guard::tool_scope_is_write_capable;
use hash::{hash_json, hash_text, short_digest};
pub use ids::chat_agent_thread_id_for_call;
use projection::build_agent_thread_result;
pub(crate) use projection::{AgentResultMaterialization, materialize_child_agent_final_answer};
pub use task_discovery::{MAX_TASK_DISCOVERY_PROBES, REQUEST_TASK_DISCOVERY_TOOL_NAME};
pub use task_runner::AgentSupervisorTaskChildRunner;
#[cfg(test)]
pub(crate) use task_runner::task_child_status_from_outcome;
use thread_state::AgentSupervisorState;
pub use thread_state::{
    AgentChatChildStart, AgentChatChildThread, AgentInterruptedThread, AgentMailboxMessage,
    AgentTaskChildStart, AgentTaskChildThread, ForegroundCancelImpact,
};

type BoxedAgent = Agent<Box<dyn Provider>>;

/// Process-local families whose observable supervisor snapshot changed.
///
/// Notifications are hints only. Consumers must read the latest snapshot from
/// [`AgentSupervisor`] and must not treat a notification as durable authority.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentSupervisorChange {
    /// Provider-route concurrency, waiting, or cooldown diagnostics changed.
    ProviderRouteDiagnostics,
    /// Parallel task completion-arrival progress changed.
    TaskCompletionProgress,
}

/// Receives process-local supervisor change notifications.
///
/// Notifications are delivered after the corresponding registry lock is released, so an
/// implementation may read the latest supervisor snapshot. Implementations must return promptly
/// and must not panic.
pub trait AgentSupervisorEventSink: Send + Sync {
    fn handle_supervisor_change(&self, change: AgentSupervisorChange);
}

#[derive(Clone, Default)]
pub(crate) struct AgentSupervisorChangeNotifier {
    sink: Arc<Mutex<Option<Arc<dyn AgentSupervisorEventSink>>>>,
}

impl std::fmt::Debug for AgentSupervisorChangeNotifier {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let registered = self.sink.lock().map(|sink| sink.is_some()).unwrap_or(false);
        formatter
            .debug_struct("AgentSupervisorChangeNotifier")
            .field("registered", &registered)
            .finish()
    }
}

impl AgentSupervisorChangeNotifier {
    pub(crate) fn set_sink(&self, sink: Arc<dyn AgentSupervisorEventSink>) {
        if let Ok(mut registered) = self.sink.lock() {
            *registered = Some(sink);
        }
    }

    pub(crate) fn notify(&self, change: AgentSupervisorChange) {
        let sink = self
            .sink
            .lock()
            .ok()
            .and_then(|registered| registered.clone());
        if let Some(sink) = sink {
            sink.handle_supervisor_change(change);
        }
    }
}

/// Runtime-owned supervisor for agent thread lifecycle, budget, and durable control entries.
#[derive(Debug, Clone)]
pub struct AgentSupervisor {
    registry: AgentProfileRegistry,
    budget: AgentBudgetPolicy,
    provider_capabilities: ProviderCapabilities,
    state: Arc<Mutex<AgentSupervisorState>>,
    provider_pressure: TaskProviderPressure,
    task_completion_progress: TaskCompletionProgressRegistry,
}

impl AgentSupervisor {
    #[must_use]
    pub fn new(
        registry: AgentProfileRegistry,
        budget: AgentBudgetPolicy,
        provider_capabilities: ProviderCapabilities,
    ) -> Self {
        Self {
            registry,
            budget,
            provider_capabilities,
            state: Arc::new(Mutex::new(AgentSupervisorState::default())),
            provider_pressure: TaskProviderPressure::default(),
            task_completion_progress: TaskCompletionProgressRegistry::default(),
        }
    }

    /// Installs a process-local change sink for event-driven observers.
    #[must_use]
    pub fn with_event_sink(self, sink: Arc<dyn AgentSupervisorEventSink>) -> Self {
        self.provider_pressure.set_change_sink(Arc::clone(&sink));
        self.task_completion_progress.set_change_sink(sink);
        self
    }

    #[must_use]
    pub fn registry(&self) -> &AgentProfileRegistry {
        &self.registry
    }

    #[must_use]
    pub fn budget(&self) -> &AgentBudgetPolicy {
        &self.budget
    }

    pub(crate) fn provider_pressure(&self) -> &TaskProviderPressure {
        &self.provider_pressure
    }

    pub(crate) fn completion_progress(&self) -> &TaskCompletionProgressRegistry {
        &self.task_completion_progress
    }

    /// Returns live task provider-route pressure for user-facing diagnostics.
    ///
    /// This process-local snapshot is observational only and must not be persisted or used as
    /// restart authority.
    #[must_use]
    pub fn task_provider_route_diagnostics(&self) -> TaskProviderRouteDiagnosticsSnapshot {
        self.provider_pressure.diagnostics()
    }

    /// Returns live completion-arrival order for the latest parallel task batch.
    ///
    /// This process-local snapshot is observational only. Durable parent commits remain ordered by
    /// stable request sequence and this snapshot must not be used as restart authority.
    #[must_use]
    pub fn task_completion_progress(&self) -> TaskCompletionProgressSnapshot {
        self.task_completion_progress.snapshot()
    }

    #[must_use]
    pub fn supports_background_resume(&self) -> bool {
        self.provider_capabilities.supports_agent_background_resume
    }

    pub fn validate_usage_budget(&self, task_id: &TaskId, usage: &AgentUsageSummary) -> Result<()> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| anyhow!("agent supervisor state lock poisoned"))?;
        let current_tokens = *state.task_token_usage.get(task_id).unwrap_or(&0);
        let total_tokens = current_tokens.saturating_add(usage.total_tokens);
        state.task_token_usage.insert(task_id.clone(), total_tokens);
        Ok(())
    }
}

#[cfg(test)]
#[path = "tests/agent_supervisor_tests.rs"]
mod tests;

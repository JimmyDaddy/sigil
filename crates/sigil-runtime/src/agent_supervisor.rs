use std::sync::{Arc, Mutex};

use anyhow::{Result, anyhow};
use sigil_kernel::{Agent, AgentUsageSummary, Provider, ProviderCapabilities, TaskId};

use crate::AgentProfileRegistry;

mod begin;
mod budget;
mod control;
mod guard;
mod hash;
mod ids;
mod projection;
mod record;
mod task_runner;
mod thread_ops;
mod thread_state;
pub use budget::AgentBudgetPolicy;
use control::{agent_terminal_status_from_task_child, append_control};
#[cfg(test)]
use guard::tool_scope_is_write_capable;
use hash::{hash_json, hash_text, short_digest};
pub use ids::chat_agent_thread_id_for_call;
pub(crate) use projection::agent_final_answer_ref;
use projection::build_agent_thread_result;
pub use task_runner::AgentSupervisorTaskChildRunner;
#[cfg(test)]
pub(crate) use task_runner::task_child_status_from_outcome;
use thread_state::AgentSupervisorState;
pub use thread_state::{
    AgentChatChildStart, AgentChatChildThread, AgentInterruptedThread, AgentMailboxMessage,
    AgentTaskChildStart, AgentTaskChildThread, ForegroundCancelImpact,
};

type BoxedAgent = Agent<Box<dyn Provider>>;

/// Runtime-owned supervisor for agent thread lifecycle, budget, and durable control entries.
#[derive(Debug, Clone)]
pub struct AgentSupervisor {
    registry: AgentProfileRegistry,
    budget: AgentBudgetPolicy,
    provider_capabilities: ProviderCapabilities,
    state: Arc<Mutex<AgentSupervisorState>>,
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
        }
    }

    #[must_use]
    pub fn registry(&self) -> &AgentProfileRegistry {
        &self.registry
    }

    #[must_use]
    pub fn budget(&self) -> &AgentBudgetPolicy {
        &self.budget
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

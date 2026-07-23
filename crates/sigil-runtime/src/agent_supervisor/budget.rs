use anyhow::Result;
use serde_json::json;

use super::hash_json;

const MAX_AGENT_DEPTH_WITH_PLANNER_DISCOVERY: usize = 2;

/// Runtime-enforced limits for agent/thread fan-out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentBudgetPolicy {
    pub max_subagents: usize,
    pub max_depth: usize,
}

impl AgentBudgetPolicy {
    #[must_use]
    pub fn from_root_config(root_config: &sigil_kernel::RootConfig) -> Self {
        let task = &root_config.task;
        Self {
            max_subagents: task.max_subagents,
            max_depth: MAX_AGENT_DEPTH_WITH_PLANNER_DISCOVERY,
        }
    }

    pub(super) fn hash(&self) -> Result<String> {
        hash_json(&json!({
            "max_subagents": self.max_subagents,
            "max_depth": self.max_depth,
        }))
    }
}

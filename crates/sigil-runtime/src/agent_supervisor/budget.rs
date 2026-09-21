use std::{error::Error, fmt};

use anyhow::Result;
use serde_json::json;

use super::hash_json;

const MAX_AGENT_DEPTH_WITH_DIRECT_DELEGATION: usize = 2;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum AgentBudgetDenial {
    ActiveThreadLimit {
        active_threads: usize,
        requested_threads: usize,
        max_subagents: usize,
    },
    DepthLimit {
        parent_depth: usize,
        max_depth: usize,
    },
}

impl AgentBudgetDenial {
    pub(crate) const fn config_path(&self) -> &'static str {
        match self {
            Self::ActiveThreadLimit { .. } => "[task].max_subagents",
            Self::DepthLimit { .. } => "[task]",
        }
    }

    pub(crate) const fn kind(&self) -> &'static str {
        match self {
            Self::ActiveThreadLimit { .. } => "active_thread_limit",
            Self::DepthLimit { .. } => "depth_limit",
        }
    }

    pub(crate) const fn limit(&self) -> usize {
        match self {
            Self::ActiveThreadLimit { max_subagents, .. } => *max_subagents,
            Self::DepthLimit { max_depth, .. } => *max_depth,
        }
    }

    pub(crate) const fn active_threads(&self) -> Option<usize> {
        match self {
            Self::ActiveThreadLimit { active_threads, .. } => Some(*active_threads),
            Self::DepthLimit { .. } => None,
        }
    }

    pub(crate) const fn requested_threads(&self) -> Option<usize> {
        match self {
            Self::ActiveThreadLimit {
                requested_threads, ..
            } => Some(*requested_threads),
            Self::DepthLimit { .. } => None,
        }
    }

    pub(crate) const fn current_depth(&self) -> Option<usize> {
        match self {
            Self::ActiveThreadLimit { .. } => None,
            Self::DepthLimit { parent_depth, .. } => Some(*parent_depth),
        }
    }

    pub(crate) const fn retryable_after_slot_available(&self) -> bool {
        match self {
            Self::ActiveThreadLimit {
                requested_threads,
                max_subagents,
                ..
            } => *requested_threads <= *max_subagents,
            Self::DepthLimit { .. } => false,
        }
    }
}

impl fmt::Display for AgentBudgetDenial {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ActiveThreadLimit {
                active_threads,
                requested_threads,
                max_subagents,
            } => {
                write!(
                    formatter,
                    "agent thread budget exceeded: active={active_threads} requested={requested_threads} [task].max_subagents={max_subagents}"
                )
            }
            Self::DepthLimit {
                parent_depth,
                max_depth,
            } => {
                write!(
                    formatter,
                    "agent depth budget exceeded: parent_depth={parent_depth} max_depth={max_depth}"
                )
            }
        }
    }
}

#[derive(Debug)]
pub(crate) enum AgentReservationError {
    StateLockPoisoned,
    AlreadyActive { thread_id: String },
    Budget(AgentBudgetDenial),
}

impl AgentReservationError {
    pub(crate) const fn budget_denial(&self) -> Option<&AgentBudgetDenial> {
        match self {
            Self::Budget(denial) => Some(denial),
            Self::StateLockPoisoned | Self::AlreadyActive { .. } => None,
        }
    }
}

impl fmt::Display for AgentReservationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::StateLockPoisoned => formatter.write_str("agent supervisor state lock poisoned"),
            Self::AlreadyActive { thread_id } => {
                write!(formatter, "agent thread {thread_id} is already active")
            }
            Self::Budget(denial) => denial.fmt(formatter),
        }
    }
}

impl Error for AgentReservationError {}

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
            max_depth: MAX_AGENT_DEPTH_WITH_DIRECT_DELEGATION,
        }
    }

    pub(super) fn hash(&self) -> Result<String> {
        hash_json(&json!({
            "max_subagents": self.max_subagents,
            "max_depth": self.max_depth,
        }))
    }
}

use std::{collections::BTreeSet, path::Path};

use anyhow::Result;
use sigil_kernel::{
    AgentResultContinuationProjection, AgentThreadProjection, AgentThreadStateProjection,
    AgentThreadStatus, JsonlSessionStore, Session, SessionLogEntry,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentGraphProductSummary {
    pub total_agents: usize,
    pub active_agents: usize,
    pub terminal_agents: usize,
    pub open_routes: u64,
    pub total_tokens: u64,
    pub projection_degraded: bool,
}

impl AgentGraphProductSummary {
    #[must_use]
    pub fn display_line(&self) -> String {
        let mut parts = vec![format!("graph: {} agents", self.total_agents)];
        if self.active_agents > 0 {
            parts.push(format!("{} active", self.active_agents));
        }
        if self.terminal_agents > 0 {
            parts.push(format!("{} terminal", self.terminal_agents));
        }
        if self.open_routes > 0 {
            parts.push(format!("{} open routes", self.open_routes));
        }
        if self.total_tokens > 0 {
            parts.push(format!("{} tokens", self.total_tokens));
        }
        if self.projection_degraded {
            parts.push("projection degraded".to_owned());
        }
        parts.join(" · ")
    }

    #[must_use]
    pub fn with_projection_degraded(mut self) -> Self {
        self.projection_degraded = true;
        self
    }
}

/// Rebuilds an agent graph product summary from the durable session stream.
///
/// This is the reusable product-view adapter for TUI today and desktop/server surfaces later.
/// Live control state should still come from the runtime event path; this view is for historical
/// and audit-oriented summaries.
///
/// # Errors
///
/// Returns an error when the durable stream cannot be replayed.
pub fn agent_graph_product_summary_from_session_log(
    session_log_path: &Path,
) -> Result<Option<AgentGraphProductSummary>> {
    if !session_log_path.exists() {
        return Ok(None);
    }
    let store = JsonlSessionStore::new(session_log_path)?;
    let session = Session::from_entries("", "", Vec::new()).with_store(store);
    let Some(projection) = session.try_agent_graph_projection_from_durable()? else {
        return Ok(None);
    };
    let Some(continuation_projection) =
        session.try_agent_result_continuation_projection_from_durable()?
    else {
        return Ok(None);
    };
    Ok(agent_graph_product_summary_from_projections(
        &projection,
        &continuation_projection,
        false,
    ))
}

#[must_use]
pub fn agent_graph_product_summary_from_entries(
    entries: &[SessionLogEntry],
) -> Option<AgentGraphProductSummary> {
    let projection = AgentThreadStateProjection::from_entries(entries);
    let continuation_projection = AgentResultContinuationProjection::from_entries(entries);
    agent_graph_product_summary_from_projections(&projection, &continuation_projection, false)
}

fn agent_graph_product_summary_from_projections(
    projection: &AgentThreadStateProjection,
    continuation_projection: &AgentResultContinuationProjection,
    projection_degraded: bool,
) -> Option<AgentGraphProductSummary> {
    let mut seen = BTreeSet::new();
    let visible_threads = projection
        .thread_replay_order
        .iter()
        .filter(|thread_id| seen.insert((*thread_id).clone()))
        .filter_map(|thread_id| projection.threads.get(thread_id))
        .filter(|thread| !thread.closed && thread.status != AgentThreadStatus::Closed)
        .collect::<Vec<_>>();
    if visible_threads.is_empty() {
        return None;
    }
    let active_agents = visible_threads
        .iter()
        .filter(|thread| {
            let continuation_unresolved = continuation_projection
                .statuses
                .get(&thread.thread_id)
                .is_some_and(|status| status.is_unresolved());
            !agent_thread_effective_status(thread, continuation_unresolved).is_terminal()
        })
        .count();
    let terminal_agents = visible_threads.len().saturating_sub(active_agents);
    let total_tokens = visible_threads.iter().fold(0u64, |total, thread| {
        total
            + thread
                .result
                .as_ref()
                .and_then(|result| result.usage.as_ref())
                .map(|usage| usage.total_tokens)
                .unwrap_or_default()
    });
    Some(AgentGraphProductSummary {
        total_agents: visible_threads.len(),
        active_agents,
        terminal_agents,
        open_routes: projection.graph_summary().open_routes,
        total_tokens,
        projection_degraded,
    })
}

fn agent_thread_effective_status(
    thread: &AgentThreadProjection,
    continuation_unresolved: bool,
) -> AgentThreadStatus {
    if continuation_unresolved && thread.status == AgentThreadStatus::Failed {
        AgentThreadStatus::Running
    } else {
        thread.status
    }
}

#[cfg(test)]
#[path = "tests/product_view_tests.rs"]
mod tests;

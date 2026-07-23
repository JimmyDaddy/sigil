use std::{collections::BTreeSet, path::Path};

use anyhow::Result;
use sigil_kernel::{
    AgentResultContinuationProjection, AgentThreadProjection, AgentThreadStateProjection,
    AgentThreadStatus, JsonlSessionStore, Session, SessionLogEntry, safe_persistence_text,
};

const MAX_AGENT_ACTIVITY_ITEMS: usize = 100;
const MAX_AGENT_ACTIVITY_TEXT_BYTES: usize = 32 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApplicationAgentActivityStatus {
    Started,
    Running,
    Blocked,
    Completed,
    Failed,
    Cancelled,
    Interrupted,
    Unavailable,
    Unknown,
}

impl ApplicationAgentActivityStatus {
    #[must_use]
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Completed
                | Self::Failed
                | Self::Cancelled
                | Self::Interrupted
                | Self::Unavailable
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApplicationAgentHandoffStatus {
    Pending,
    ResultReady,
    ResultRead,
    Returned,
    Unavailable,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApplicationAgentUsageSummary {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub total_tokens: u64,
    pub cached_tokens: Option<u64>,
}

/// One renderer-safe child-agent lifecycle projected from append-only session truth.
///
/// Paths, hashes, session references, provider handles, raw prompts, and tool arguments are
/// intentionally absent from this product view.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApplicationAgentActivityItem {
    pub thread_id: String,
    pub profile_id: Option<String>,
    pub display_name: Option<String>,
    pub objective: String,
    pub status: ApplicationAgentActivityStatus,
    pub reason: Option<String>,
    pub handoff_status: ApplicationAgentHandoffStatus,
    pub result_summary: Option<String>,
    pub result_summary_truncated: bool,
    pub usage: Option<ApplicationAgentUsageSummary>,
}

/// Bounded agent activity for one parent session, newest child first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApplicationAgentActivityView {
    pub total_agents: usize,
    pub active_agents: usize,
    pub terminal_agents: usize,
    pub items: Vec<ApplicationAgentActivityItem>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentGraphProductSummary {
    pub total_agents: usize,
    pub active_agents: usize,
    pub terminal_agents: usize,
    pub total_batches: usize,
    pub active_batches: usize,
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
        if self.total_batches > 0 {
            let batch_label = if self.total_batches == 1 {
                "batch"
            } else {
                "batches"
            };
            parts.push(format!("{} {batch_label}", self.total_batches));
        }
        if self.active_batches > 0 {
            let active_batch_label = if self.active_batches == 1 {
                "active batch"
            } else {
                "active batches"
            };
            parts.push(format!("{} {active_batch_label}", self.active_batches));
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

/// Projects the latest child-agent activity without exposing child session locations or raw
/// execution payloads.
#[must_use]
pub fn agent_activity_product_view_from_entries(
    entries: &[SessionLogEntry],
) -> ApplicationAgentActivityView {
    let projection = AgentThreadStateProjection::from_entries(entries);
    let continuation_projection = AgentResultContinuationProjection::from_entries(entries);
    let mut seen = BTreeSet::new();
    let visible_threads = projection
        .thread_replay_order
        .iter()
        .rev()
        .filter(|thread_id| seen.insert((*thread_id).clone()))
        .filter_map(|thread_id| projection.threads.get(thread_id))
        .filter(|thread| !thread.closed && thread.status != AgentThreadStatus::Closed)
        .collect::<Vec<_>>();
    let total_agents = visible_threads.len();
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
    let items = visible_threads
        .into_iter()
        .take(MAX_AGENT_ACTIVITY_ITEMS)
        .map(|thread| agent_activity_item(thread, &continuation_projection))
        .collect();
    ApplicationAgentActivityView {
        total_agents,
        active_agents,
        terminal_agents: total_agents.saturating_sub(active_agents),
        items,
    }
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
    let visible_thread_ids = visible_threads
        .iter()
        .map(|thread| &thread.thread_id)
        .collect::<BTreeSet<_>>();
    let visible_batches = projection
        .batches
        .values()
        .filter(|batch| {
            batch
                .member_thread_ids
                .iter()
                .any(|thread_id| visible_thread_ids.contains(thread_id))
        })
        .collect::<Vec<_>>();
    let active_batches = visible_batches
        .iter()
        .filter(|batch| {
            batch.member_thread_ids.iter().any(|thread_id| {
                projection.threads.get(thread_id).is_some_and(|thread| {
                    visible_thread_ids.contains(&thread.thread_id)
                        && !agent_thread_effective_status(
                            thread,
                            continuation_projection
                                .statuses
                                .get(thread_id)
                                .is_some_and(|status| status.is_unresolved()),
                        )
                        .is_terminal()
                })
            })
        })
        .count();
    let graph_summary = projection.graph_summary();
    Some(AgentGraphProductSummary {
        total_agents: visible_threads.len(),
        active_agents,
        terminal_agents,
        total_batches: visible_batches.len(),
        active_batches,
        open_routes: graph_summary.open_routes,
        total_tokens,
        projection_degraded: projection_degraded
            || visible_batches.iter().any(|batch| batch.is_degraded())
            || visible_threads
                .iter()
                .any(|thread| thread.batch_identity_incomplete),
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

fn agent_activity_item(
    thread: &AgentThreadProjection,
    continuation_projection: &AgentResultContinuationProjection,
) -> ApplicationAgentActivityItem {
    let continuation_unresolved = continuation_projection
        .statuses
        .get(&thread.thread_id)
        .is_some_and(|status| status.is_unresolved());
    let effective_status = agent_thread_effective_status(thread, continuation_unresolved);
    let handoff_status = if !thread.merge_safe_points.is_empty() {
        ApplicationAgentHandoffStatus::Returned
    } else if thread.result_delivered {
        ApplicationAgentHandoffStatus::ResultRead
    } else if thread.result.is_some() {
        ApplicationAgentHandoffStatus::ResultReady
    } else if effective_status.is_terminal() {
        ApplicationAgentHandoffStatus::Unavailable
    } else {
        ApplicationAgentHandoffStatus::Pending
    };
    ApplicationAgentActivityItem {
        thread_id: thread.thread_id.as_str().to_owned(),
        profile_id: thread
            .profile_id
            .as_ref()
            .map(|profile_id| profile_id.as_str().to_owned()),
        display_name: thread
            .display_name
            .as_deref()
            .map(bounded_agent_activity_text),
        objective: bounded_agent_activity_text(&thread.objective),
        status: application_agent_activity_status(effective_status),
        reason: thread.reason.as_deref().map(bounded_agent_activity_text),
        handoff_status,
        result_summary: thread
            .result
            .as_ref()
            .map(|result| bounded_agent_activity_text(&result.summary)),
        result_summary_truncated: thread.result.as_ref().is_some_and(|result| {
            result.summary_truncated || result.summary.len() > MAX_AGENT_ACTIVITY_TEXT_BYTES
        }),
        usage: thread
            .result
            .as_ref()
            .and_then(|result| result.usage.as_ref())
            .map(|usage| ApplicationAgentUsageSummary {
                input_tokens: usage.input_tokens,
                output_tokens: usage.output_tokens,
                total_tokens: usage.total_tokens,
                cached_tokens: usage.cached_tokens,
            }),
    }
}

fn application_agent_activity_status(status: AgentThreadStatus) -> ApplicationAgentActivityStatus {
    match status {
        AgentThreadStatus::Started => ApplicationAgentActivityStatus::Started,
        AgentThreadStatus::Running => ApplicationAgentActivityStatus::Running,
        AgentThreadStatus::Blocked => ApplicationAgentActivityStatus::Blocked,
        AgentThreadStatus::Completed | AgentThreadStatus::Closed => {
            ApplicationAgentActivityStatus::Completed
        }
        AgentThreadStatus::Failed => ApplicationAgentActivityStatus::Failed,
        AgentThreadStatus::Cancelled => ApplicationAgentActivityStatus::Cancelled,
        AgentThreadStatus::Interrupted => ApplicationAgentActivityStatus::Interrupted,
        AgentThreadStatus::Unavailable => ApplicationAgentActivityStatus::Unavailable,
        AgentThreadStatus::Unknown => ApplicationAgentActivityStatus::Unknown,
    }
}

fn bounded_agent_activity_text(value: &str) -> String {
    let safe = safe_persistence_text(value);
    if safe.len() <= MAX_AGENT_ACTIVITY_TEXT_BYTES {
        return safe;
    }
    let boundary = safe
        .char_indices()
        .take_while(|(index, _)| *index <= MAX_AGENT_ACTIVITY_TEXT_BYTES)
        .map(|(index, _)| index)
        .last()
        .unwrap_or_default();
    safe[..boundary].to_owned()
}

#[cfg(test)]
#[path = "tests/product_view_tests.rs"]
mod tests;

use std::collections::BTreeMap;

use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};

use crate::{
    agent_thread::AgentThreadId,
    provider::ReasoningEffort,
    session::{ControlEntry, SessionLogEntry},
};

/// Stable identifier for one queued conversation input.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(transparent)]
pub struct ConversationInputQueueId(String);

impl ConversationInputQueueId {
    /// Creates a path-safe queue identifier.
    ///
    /// # Errors
    ///
    /// Returns an error when `value` is empty, too long, or contains unstable characters.
    pub fn new(value: impl Into<String>) -> Result<Self> {
        let value = value.into();
        validate_stable_id("conversation input queue id", &value)?;
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Destination for a queued conversation input.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ConversationInputTarget {
    MainThread,
    AgentThread { thread_id: AgentThreadId },
}

/// Product-level class of one queued input.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ConversationInputKind {
    Chat,
    PlanPrompt,
    AgentMention,
    AgentMessage,
    Unknown,
}

/// Append-only control entry recording a queued prompt outside provider-visible chat history.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct ConversationInputQueuedEntry {
    pub queue_id: ConversationInputQueueId,
    pub target: ConversationInputTarget,
    pub kind: ConversationInputKind,
    pub prompt_hash: String,
    pub prompt: String,
    pub reasoning_effort: Option<ReasoningEffort>,
    pub created_at_ms: Option<u64>,
}

/// Durable whole-queue control action.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ConversationInputQueueControlAction {
    Pause,
    Resume,
}

/// Append-only control entry recording queue-level controls.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct ConversationInputQueueControlEntry {
    pub action: ConversationInputQueueControlAction,
    pub reason: Option<String>,
    pub updated_at_ms: Option<u64>,
}

/// Append-only control entry replacing a queued prompt before it is dispatched.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct ConversationInputEditedEntry {
    pub queue_id: ConversationInputQueueId,
    pub prompt_hash: String,
    pub prompt: String,
    pub reasoning_effort: Option<ReasoningEffort>,
    pub updated_at_ms: Option<u64>,
}

/// Append-only control entry moving a queued prompt within the active queue.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct ConversationInputReorderedEntry {
    pub queue_id: ConversationInputQueueId,
    pub after_queue_id: Option<ConversationInputQueueId>,
    pub updated_at_ms: Option<u64>,
}

/// Runtime status for one queued input.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ConversationInputStatus {
    Queued,
    Dispatching,
    Delivered,
    Rejected,
    Cancelled,
    Stale,
    Unknown,
}

impl ConversationInputStatus {
    #[must_use]
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Delivered | Self::Rejected | Self::Cancelled | Self::Stale
        )
    }
}

/// Append-only control entry recording a queue item status transition.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct ConversationInputStatusEntry {
    pub queue_id: ConversationInputQueueId,
    pub status: ConversationInputStatus,
    pub reason: Option<String>,
    pub updated_at_ms: Option<u64>,
}

/// Current projected state for one active queued input.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConversationQueueItemProjection {
    pub queued: ConversationInputQueuedEntry,
    pub status: ConversationInputStatus,
    pub reason: Option<String>,
}

/// Durable FIFO projection for conversation input queue state.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ConversationQueueProjection {
    pub items: Vec<ConversationQueueItemProjection>,
    pub paused: bool,
    pub next_dispatchable: Option<ConversationInputQueueId>,
}

impl ConversationQueueProjection {
    #[must_use]
    pub fn from_entries(entries: &[SessionLogEntry]) -> Self {
        let mut projection = Self::default();

        for entry in entries {
            let SessionLogEntry::Control(control) = entry else {
                continue;
            };
            projection.apply_control_entry(control);
        }

        projection
    }

    pub(crate) fn apply_control_entry(&mut self, control: &ControlEntry) {
        let mut indexed = self
            .items
            .iter()
            .map(|item| (item.queued.queue_id.clone(), item.clone()))
            .collect::<BTreeMap<_, _>>();
        let mut order = self
            .items
            .iter()
            .map(|item| item.queued.queue_id.clone())
            .collect::<Vec<_>>();

        match control {
            ControlEntry::ConversationInputQueued(queued) => {
                if !indexed.contains_key(&queued.queue_id) {
                    order.push(queued.queue_id.clone());
                }
                indexed.insert(
                    queued.queue_id.clone(),
                    ConversationQueueItemProjection {
                        queued: queued.clone(),
                        status: ConversationInputStatus::Queued,
                        reason: None,
                    },
                );
            }
            ControlEntry::ConversationInputEdited(edited) => {
                if let Some(item) = indexed.get_mut(&edited.queue_id) {
                    item.queued.prompt_hash = edited.prompt_hash.clone();
                    item.queued.prompt = edited.prompt.clone();
                    item.queued.reasoning_effort = edited.reasoning_effort.clone();
                }
            }
            ControlEntry::ConversationInputReordered(reordered) => {
                if indexed.contains_key(&reordered.queue_id) {
                    move_order_entry(
                        &mut order,
                        &reordered.queue_id,
                        reordered.after_queue_id.as_ref(),
                    );
                }
            }
            ControlEntry::ConversationInputQueueControl(control) => {
                self.paused = control.action == ConversationInputQueueControlAction::Pause;
            }
            ControlEntry::ConversationInputStatusChanged(status) => {
                if let Some(item) = indexed.get_mut(&status.queue_id) {
                    item.status = status.status;
                    item.reason = status.reason.clone();
                }
            }
            _ => return,
        }

        self.items = order
            .into_iter()
            .filter_map(|queue_id| indexed.remove(&queue_id))
            .filter(|item| !item.status.is_terminal())
            .collect();
        self.next_dispatchable = self
            .items
            .iter()
            .filter(|_| !self.paused)
            .find(|item| {
                item.status == ConversationInputStatus::Queued
                    && item.queued.target == ConversationInputTarget::MainThread
            })
            .map(|item| item.queued.queue_id.clone());
    }
}

fn move_order_entry(
    order: &mut Vec<ConversationInputQueueId>,
    queue_id: &ConversationInputQueueId,
    after_queue_id: Option<&ConversationInputQueueId>,
) {
    let Some(current_index) = order.iter().position(|id| id == queue_id) else {
        return;
    };
    let moved = order.remove(current_index);
    let insert_index = after_queue_id
        .and_then(|after| {
            order
                .iter()
                .position(|id| id == after)
                .map(|index| index + 1)
        })
        .unwrap_or(0)
        .min(order.len());
    order.insert(insert_index, moved);
}

fn validate_stable_id(label: &str, value: &str) -> Result<()> {
    if value.is_empty() {
        bail!("{label} cannot be empty");
    }
    if value.len() > 128 {
        bail!("{label} is too long");
    }
    if !value
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-'))
    {
        bail!("{label} contains unsupported characters");
    }
    Ok(())
}

#[cfg(test)]
#[path = "tests/conversation_queue_tests.rs"]
mod tests;

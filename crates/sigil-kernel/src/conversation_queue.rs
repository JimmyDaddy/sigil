use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{
    EventId, ProjectionApplyDecision, ProjectionCursor, StoredEventDecode,
    WebUrlCapabilityDescriptor,
    agent_thread::AgentThreadId,
    event::{canonical_json_content_hash, decode_stored_event},
    projection_apply_decision,
    provider::{MessageRole, ModelMessage, ReasoningEffort},
    session::{ControlEntry, SessionLogEntry, SessionStreamRecord},
};

/// Durable hash prefix proving that dispatch requires process-local exact prompt material.
pub const CONVERSATION_EXACT_PROMPT_REQUIRED_HASH_PREFIX: &str = "exact-required:";

/// Schema version for the durable queue-promotion projection cursor.
pub const CONVERSATION_QUEUE_DURABLE_PROJECTION_SCHEMA_VERSION: u16 = 1;

/// Stable event identity used when a queue has not appended any mutation yet.
pub const CONVERSATION_QUEUE_INITIAL_REVISION_EVENT_ID: &str = "conversation-queue-initial";

/// Maximum URL capabilities bound to one promoted queued input.
pub const MAX_CONVERSATION_PROMOTION_CAPABILITY_DESCRIPTORS: usize = 64;

/// Secret-safe durable projection for one queued or edited conversation prompt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConversationPromptPersistenceProjection {
    pub safe_prompt: String,
    pub prompt_hash: String,
    pub exact_prompt_required: bool,
}

#[must_use]
pub fn project_conversation_prompt_for_persistence(
    raw_prompt: &str,
) -> ConversationPromptPersistenceProjection {
    let safe_prompt = crate::safe_persistence_text(raw_prompt);
    let exact_prompt_required = safe_prompt != raw_prompt;
    let mut hasher = Sha256::new();
    hasher.update(safe_prompt.as_bytes());
    let safe_hash = format!("sha256:{:x}", hasher.finalize());
    let prompt_hash = if exact_prompt_required {
        format!("{CONVERSATION_EXACT_PROMPT_REQUIRED_HASH_PREFIX}{safe_hash}")
    } else {
        format!("safe:{safe_hash}")
    };
    ConversationPromptPersistenceProjection {
        safe_prompt,
        prompt_hash,
        exact_prompt_required,
    }
}

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

/// A precise durable queue cursor used by mutation and promotion compare-and-swap checks.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct ConversationQueueRevision {
    pub stream_sequence: u64,
    pub event_id: EventId,
}

impl ConversationQueueRevision {
    /// Returns the compare-and-swap revision for a queue with no durable mutations.
    #[must_use]
    pub fn initial() -> Self {
        Self {
            stream_sequence: 0,
            event_id: CONVERSATION_QUEUE_INITIAL_REVISION_EVENT_ID.to_owned(),
        }
    }

    /// Returns whether this is the explicit empty-queue revision.
    #[must_use]
    pub fn is_initial(&self) -> bool {
        self.stream_sequence == 0 && self.event_id == CONVERSATION_QUEUE_INITIAL_REVISION_EVENT_ID
    }

    fn from_record(record: &SessionStreamRecord) -> Self {
        Self {
            stream_sequence: record.stream_sequence(),
            event_id: record.event_id().to_owned(),
        }
    }

    fn validate(&self) -> Result<()> {
        if self.stream_sequence == 0 {
            bail!("conversation queue revision stream sequence must be non-zero");
        }
        validate_stable_id("conversation queue revision event id", &self.event_id)
    }

    fn validate_expected(&self) -> Result<()> {
        if self.is_initial() {
            return Ok(());
        }
        self.validate()
    }
}

/// One provider-neutral append-only queue mutation.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", tag = "action", deny_unknown_fields)]
pub enum ConversationQueueMutation {
    Enqueue {
        entry: ConversationInputQueuedEntry,
    },
    Edit {
        entry: ConversationInputEditedEntry,
    },
    Remove {
        queue_id: ConversationInputQueueId,
        reason: Option<String>,
        updated_at_ms: Option<u64>,
    },
    Reorder {
        entry: ConversationInputReorderedEntry,
    },
    Pause {
        reason: Option<String>,
        updated_at_ms: Option<u64>,
    },
    Resume {
        reason: Option<String>,
        updated_at_ms: Option<u64>,
    },
}

impl ConversationQueueMutation {
    pub(crate) fn control_entry(&self) -> ControlEntry {
        match self {
            Self::Enqueue { entry } => ControlEntry::ConversationInputQueued(entry.clone()),
            Self::Edit { entry } => ControlEntry::ConversationInputEdited(entry.clone()),
            Self::Remove {
                queue_id,
                reason,
                updated_at_ms,
            } => ControlEntry::ConversationInputStatusChanged(ConversationInputStatusEntry {
                queue_id: queue_id.clone(),
                status: ConversationInputStatus::Cancelled,
                reason: reason.clone(),
                updated_at_ms: *updated_at_ms,
            }),
            Self::Reorder { entry } => ControlEntry::ConversationInputReordered(entry.clone()),
            Self::Pause {
                reason,
                updated_at_ms,
            } => ControlEntry::ConversationInputQueueControl(ConversationInputQueueControlEntry {
                action: ConversationInputQueueControlAction::Pause,
                reason: reason.clone(),
                updated_at_ms: *updated_at_ms,
            }),
            Self::Resume {
                reason,
                updated_at_ms,
            } => ControlEntry::ConversationInputQueueControl(ConversationInputQueueControlEntry {
                action: ConversationInputQueueControlAction::Resume,
                reason: reason.clone(),
                updated_at_ms: *updated_at_ms,
            }),
        }
    }
}

/// Exact compare-and-swap command for one durable queue mutation.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct ConversationQueueMutationCommand {
    pub expected_queue_revision: ConversationQueueRevision,
    pub mutation: ConversationQueueMutation,
}

/// Durable result of one accepted queue mutation.
#[derive(Debug, Clone, PartialEq)]
pub struct ConversationQueueMutationReceipt {
    pub revision: ConversationQueueRevision,
    pub event: crate::StoredEvent,
}

/// Exact durable tail observed while deriving one promoted run's terminal evidence.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct ConversationInputTerminalFrontier {
    pub stream_sequence: u64,
    pub event_id: EventId,
    pub record_checksum: String,
}

impl ConversationInputTerminalFrontier {
    /// Captures the exact append-only tail represented by one durable record.
    #[must_use]
    pub fn from_record(record: &SessionStreamRecord) -> Self {
        Self {
            stream_sequence: record.stream_sequence(),
            event_id: record.event_id().to_owned(),
            record_checksum: record.record_checksum().to_owned(),
        }
    }

    fn validate(&self) -> Result<()> {
        if self.stream_sequence == 0 {
            bail!("conversation input terminal frontier sequence must be non-zero");
        }
        validate_stable_id(
            "conversation input terminal frontier event id",
            &self.event_id,
        )?;
        if self.record_checksum.trim().is_empty() {
            bail!("conversation input terminal frontier checksum is empty");
        }
        Ok(())
    }

    pub(crate) fn matches_record(&self, record: &SessionStreamRecord) -> bool {
        self.stream_sequence == record.stream_sequence()
            && self.event_id == record.event_id()
            && self.record_checksum == record.record_checksum()
    }
}

/// Durable predicate that must still hold when one queue item reaches a terminal state.
///
/// Preparation failures bind to the exact pre-promotion revision and safe prompt hash. Once a
/// promotion exists, terminal delivery binds instead to the promotion's logical dispatch run id;
/// unrelated queue mutations must not invalidate that already-owned run.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", tag = "phase", deny_unknown_fields)]
pub enum ConversationInputTerminalExpectation {
    Queued {
        expected_queue_revision: ConversationQueueRevision,
        queue_id: ConversationInputQueueId,
        expected_prompt_hash: String,
    },
    Promoted {
        queue_id: ConversationInputQueueId,
        dispatch_run_id: String,
        expected_frontier: ConversationInputTerminalFrontier,
    },
}

impl ConversationInputTerminalExpectation {
    fn queue_id(&self) -> &ConversationInputQueueId {
        match self {
            Self::Queued { queue_id, .. } | Self::Promoted { queue_id, .. } => queue_id,
        }
    }
}

/// Conditional terminal queue append owned by the runtime that observed the matching phase.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct ConversationInputTerminalCommand {
    pub expectation: ConversationInputTerminalExpectation,
    pub terminal: ConversationInputStatusEntry,
}

impl ConversationInputTerminalCommand {
    pub(crate) fn validate_shape(&self) -> Result<()> {
        if !self.terminal.status.is_terminal() {
            bail!("conversation input terminal command requires a terminal status");
        }
        if &self.terminal.queue_id != self.expectation.queue_id() {
            bail!("conversation input terminal command queue id does not match its expectation");
        }
        match &self.expectation {
            ConversationInputTerminalExpectation::Queued {
                expected_queue_revision,
                expected_prompt_hash,
                ..
            } => {
                expected_queue_revision.validate_expected()?;
                if expected_prompt_hash.trim().is_empty() {
                    bail!("conversation input terminal expected prompt hash is empty");
                }
            }
            ConversationInputTerminalExpectation::Promoted {
                dispatch_run_id,
                expected_frontier,
                ..
            } => {
                validate_stable_id(
                    "conversation input terminal dispatch run id",
                    dispatch_run_id,
                )?;
                expected_frontier.validate()?;
            }
        }
        Ok(())
    }
}

/// Critical direct event which atomically binds one queued input to its safe durable user
/// message. The exact prompt remains process-local and is never represented here.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct ConversationInputPromotedEntry {
    pub queue_id: ConversationInputQueueId,
    pub expected_queue_revision: ConversationQueueRevision,
    pub prompt_hash: String,
    pub exact_prompt_required: bool,
    pub durable_user_message: ModelMessage,
    pub capability_descriptors: Vec<WebUrlCapabilityDescriptor>,
    pub capability_digest: String,
    pub dispatch_run_id: String,
    pub promoted_at_ms: u64,
}

impl ConversationInputPromotedEntry {
    /// Validates content-free shape and safe-message ownership without relying on a stream.
    pub fn validate_shape(&self) -> Result<()> {
        self.expected_queue_revision.validate()?;
        if self.prompt_hash.trim().is_empty() {
            bail!("conversation promotion prompt hash is empty");
        }
        validate_stable_id(
            "conversation promotion dispatch run id",
            &self.dispatch_run_id,
        )?;
        if self.promoted_at_ms == 0 {
            bail!("conversation promotion timestamp must be non-zero");
        }
        validate_promoted_user_message(
            &self.durable_user_message,
            &self.prompt_hash,
            self.exact_prompt_required,
        )?;
        if self.capability_descriptors.len() > MAX_CONVERSATION_PROMOTION_CAPABILITY_DESCRIPTORS {
            bail!(
                "conversation promotion has more than {} capability descriptors",
                MAX_CONVERSATION_PROMOTION_CAPABILITY_DESCRIPTORS
            );
        }
        let mut previous_source_id = None;
        let mut source_ids = BTreeSet::new();
        for descriptor in &self.capability_descriptors {
            descriptor.validate()?;
            if descriptor.durable_entry_id != self.durable_user_message.id {
                bail!(
                    "conversation promotion capability descriptor belongs to a different message"
                );
            }
            if !source_ids.insert(descriptor.source_id.clone()) {
                bail!("conversation promotion capability descriptor source id is duplicated");
            }
            if previous_source_id
                .as_ref()
                .is_some_and(|previous| previous >= &descriptor.source_id)
            {
                bail!("conversation promotion capability descriptors must be sorted by source id");
            }
            previous_source_id = Some(descriptor.source_id.clone());
        }
        let digest = conversation_promotion_capability_digest(&self.capability_descriptors)?;
        if self.capability_digest != digest {
            bail!("conversation promotion capability digest does not match descriptors");
        }
        Ok(())
    }

    /// Validates that all capability descriptors are owned by the same durable session stream.
    pub fn validate_for_session(&self, session_id: &str) -> Result<()> {
        self.validate_shape()?;
        if session_id.trim().is_empty() {
            bail!("conversation promotion session id is empty");
        }
        if self
            .capability_descriptors
            .iter()
            .any(|descriptor| descriptor.session_scope_id != session_id)
        {
            bail!("conversation promotion capability descriptor belongs to a different session");
        }
        Ok(())
    }
}

/// Canonical digest committed by one promoted input's URL capability descriptors.
pub fn conversation_promotion_capability_digest(
    descriptors: &[WebUrlCapabilityDescriptor],
) -> Result<String> {
    canonical_json_content_hash(
        &serde_json::to_value(descriptors)
            .context("failed to encode conversation promotion capability descriptors")?,
    )
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

/// Queue state rebuilt from the complete durable event stream, including its exact CAS revision.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ConversationQueueDurableProjection {
    pub queue: ConversationQueueProjection,
    pub revision: Option<ConversationQueueRevision>,
    seen_queue_ids: BTreeSet<ConversationInputQueueId>,
    cursor: Option<ProjectionCursor>,
}

impl ConversationQueueDurableProjection {
    /// Rebuilds queue state and its compare-and-swap revision from a validated durable stream.
    ///
    /// # Errors
    ///
    /// Returns an error when a promoted record is malformed, cross-session, stale, or conflicts
    /// with the queue state reconstructed immediately before it.
    pub fn from_records(records: &[SessionStreamRecord]) -> Result<Self> {
        let mut projection = Self::default();
        for record in records {
            projection.apply_record(record)?;
        }
        Ok(projection)
    }

    /// Returns the exact revision callers must bind to their next queue mutation.
    #[must_use]
    pub fn current_revision(&self) -> ConversationQueueRevision {
        self.revision
            .clone()
            .unwrap_or_else(ConversationQueueRevision::initial)
    }

    /// Returns whether one previously known queue id has already left the active projection.
    ///
    /// Queue ids are append-only and cannot be reused. Active items remain in `queue.items`, while
    /// delivered, rejected, cancelled, and stale items are removed from that bounded projection.
    /// This helper lets application adapters preserve a typed terminal rejection instead of
    /// collapsing an already-finished id into an unknown-entry conflict.
    #[must_use]
    pub fn is_terminal_queue_id(&self, queue_id: &ConversationInputQueueId) -> bool {
        self.seen_queue_ids.contains(queue_id)
            && !self
                .queue
                .items
                .iter()
                .any(|item| item.queued.queue_id == *queue_id)
    }

    /// Validates one not-yet-appended mutation against the exact durable queue state.
    ///
    /// # Errors
    ///
    /// Returns an error for a stale revision, malformed safe prompt projection, missing item,
    /// non-queued item, invalid reorder target, or a pause/resume no-op.
    pub fn validate_mutation(&self, command: &ConversationQueueMutationCommand) -> Result<()> {
        command.expected_queue_revision.validate_expected()?;
        if self.current_revision() != command.expected_queue_revision {
            bail!("conversation queue mutation revision is stale");
        }

        match &command.mutation {
            ConversationQueueMutation::Enqueue { entry } => {
                validate_queue_prompt_projection(&entry.prompt, &entry.prompt_hash)?;
                if self.seen_queue_ids.contains(&entry.queue_id) {
                    bail!("conversation queue mutation queue id already exists");
                }
            }
            ConversationQueueMutation::Edit { entry } => {
                validate_queue_prompt_projection(&entry.prompt, &entry.prompt_hash)?;
                require_queued_item(&self.queue, &entry.queue_id)?;
            }
            ConversationQueueMutation::Remove { queue_id, .. } => {
                require_queued_item(&self.queue, queue_id)?;
            }
            ConversationQueueMutation::Reorder { entry } => {
                require_queued_item(&self.queue, &entry.queue_id)?;
                if entry.after_queue_id.as_ref() == Some(&entry.queue_id) {
                    bail!("conversation queue mutation cannot reorder an item after itself");
                }
                if let Some(after_queue_id) = &entry.after_queue_id {
                    require_queued_item(&self.queue, after_queue_id)?;
                }
            }
            ConversationQueueMutation::Pause { .. } => {
                if self.queue.paused {
                    bail!("conversation queue mutation queue is already paused");
                }
            }
            ConversationQueueMutation::Resume { .. } => {
                if !self.queue.paused {
                    bail!("conversation queue mutation queue is not paused");
                }
            }
        }
        Ok(())
    }

    /// Validates a not-yet-appended promotion against the exact current durable queue state.
    pub fn validate_promotion(&self, entry: &ConversationInputPromotedEntry) -> Result<()> {
        entry.validate_shape()?;
        if self.revision.as_ref() != Some(&entry.expected_queue_revision) {
            bail!("conversation promotion queue revision is stale");
        }
        if self.queue.paused {
            bail!("conversation promotion cannot proceed while the queue is paused");
        }
        if self.queue.next_dispatchable.as_ref() != Some(&entry.queue_id) {
            bail!("conversation promotion queue item is no longer next dispatchable");
        }
        let item = self
            .queue
            .items
            .iter()
            .find(|item| item.queued.queue_id == entry.queue_id)
            .context("conversation promotion references an unknown queue item")?;
        if item.status != ConversationInputStatus::Queued {
            bail!("conversation promotion queue item is not queued");
        }
        if item.queued.target != ConversationInputTarget::MainThread {
            bail!("conversation promotion queue item is not a main-thread input");
        }
        if item.queued.prompt_hash != entry.prompt_hash {
            bail!("conversation promotion prompt hash does not match the queue item");
        }
        if entry.durable_user_message.content.as_deref() != Some(item.queued.prompt.as_str()) {
            bail!(
                "conversation promotion durable user message does not match the queued safe prompt"
            );
        }
        Ok(())
    }

    fn apply_record(&mut self, record: &SessionStreamRecord) -> Result<()> {
        let event = record.stored_event();
        let decision = projection_apply_decision(self.cursor.as_ref(), event)?;
        if decision == ProjectionApplyDecision::IgnoreAlreadyApplied {
            return Ok(());
        }
        match decode_stored_event(event.clone())? {
            StoredEventDecode::Known(_) | StoredEventDecode::UnknownNonCritical(_) => {}
        }

        match event.event_kind() {
            Some(crate::DurableEventType::ConversationInputPromoted) => {
                let entry: ConversationInputPromotedEntry =
                    serde_json::from_value(event.payload.clone())
                        .context("failed to decode conversation input promoted payload")?;
                entry.validate_for_session(&event.session_id)?;
                self.validate_promotion(&entry)?;
                self.queue
                    .apply_control_entry(&ControlEntry::ConversationInputPromoted(entry));
                self.revision = Some(ConversationQueueRevision::from_record(record));
            }
            Some(event_type)
                if event_type.payload_metadata().storage
                    == crate::DurableEventPayloadStorage::SessionLogEntry =>
            {
                if let Some(value) = event.payload.get("session_log_entry") {
                    let entry: SessionLogEntry = serde_json::from_value(value.clone())
                        .context("failed to decode conversation queue session log entry")?;
                    if let SessionLogEntry::Control(control) = entry
                        && is_queue_affecting_control(&control)
                    {
                        if let ControlEntry::ConversationInputQueued(queued) = &control {
                            self.seen_queue_ids.insert(queued.queue_id.clone());
                        }
                        self.queue.apply_control_entry(&control);
                        self.revision = Some(ConversationQueueRevision::from_record(record));
                    }
                }
            }
            Some(_) | None => {}
        }

        self.cursor =
            Some(record.projection_cursor(CONVERSATION_QUEUE_DURABLE_PROJECTION_SCHEMA_VERSION));
        Ok(())
    }
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
                let mut queued = queued.clone();
                normalize_queue_prompt_for_projection(&mut queued.prompt, &mut queued.prompt_hash);
                if !indexed.contains_key(&queued.queue_id) {
                    order.push(queued.queue_id.clone());
                }
                indexed.insert(
                    queued.queue_id.clone(),
                    ConversationQueueItemProjection {
                        queued,
                        status: ConversationInputStatus::Queued,
                        reason: None,
                    },
                );
            }
            ControlEntry::ConversationInputEdited(edited) => {
                let mut prompt = edited.prompt.clone();
                let mut prompt_hash = edited.prompt_hash.clone();
                normalize_queue_prompt_for_projection(&mut prompt, &mut prompt_hash);
                if let Some(item) = indexed.get_mut(&edited.queue_id) {
                    item.queued.prompt_hash = prompt_hash;
                    item.queued.prompt = prompt;
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
            ControlEntry::ConversationInputPromoted(promoted) => {
                if let Some(item) = indexed.get_mut(&promoted.queue_id) {
                    item.status = ConversationInputStatus::Dispatching;
                    item.reason = Some("promotion_bound".to_owned());
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

fn is_queue_affecting_control(control: &ControlEntry) -> bool {
    matches!(
        control,
        ControlEntry::ConversationInputQueued(_)
            | ControlEntry::ConversationInputQueueControl(_)
            | ControlEntry::ConversationInputEdited(_)
            | ControlEntry::ConversationInputReordered(_)
            | ControlEntry::ConversationInputStatusChanged(_)
            | ControlEntry::ConversationInputPromoted(_)
    )
}

fn require_queued_item<'a>(
    queue: &'a ConversationQueueProjection,
    queue_id: &ConversationInputQueueId,
) -> Result<&'a ConversationQueueItemProjection> {
    let item = queue
        .items
        .iter()
        .find(|item| &item.queued.queue_id == queue_id)
        .context("conversation queue mutation references an unknown queue item")?;
    if item.status != ConversationInputStatus::Queued {
        bail!("conversation queue mutation requires a queued item");
    }
    Ok(item)
}

fn validate_queue_prompt_projection(prompt: &str, prompt_hash: &str) -> Result<()> {
    if prompt.trim().is_empty() {
        bail!("conversation queue mutation prompt is empty");
    }
    let mut hasher = Sha256::new();
    hasher.update(prompt.as_bytes());
    let safe_hash = format!("sha256:{:x}", hasher.finalize());
    let expected_exact_hash =
        format!("{CONVERSATION_EXACT_PROMPT_REQUIRED_HASH_PREFIX}{safe_hash}");
    if prompt_hash == expected_exact_hash && is_known_safe_redacted_text(prompt) {
        return Ok(());
    }

    let projected = project_conversation_prompt_for_persistence(prompt);
    if projected.exact_prompt_required
        || projected.safe_prompt != prompt
        || projected.prompt_hash != prompt_hash
    {
        bail!("conversation queue mutation prompt projection is not safe or does not match hash");
    }
    Ok(())
}

fn normalize_queue_prompt_for_projection(prompt: &mut String, prompt_hash: &mut String) {
    let mut hasher = Sha256::new();
    hasher.update(prompt.as_bytes());
    let safe_hash = format!("sha256:{:x}", hasher.finalize());
    let already_safe_exact_projection = *prompt_hash
        == format!("{CONVERSATION_EXACT_PROMPT_REQUIRED_HASH_PREFIX}{safe_hash}")
        && is_known_safe_redacted_text(prompt);
    if already_safe_exact_projection {
        return;
    }
    let safe = project_conversation_prompt_for_persistence(prompt);
    if safe.exact_prompt_required {
        *prompt = safe.safe_prompt;
        *prompt_hash = safe.prompt_hash;
    }
}

fn validate_promoted_user_message(
    message: &ModelMessage,
    prompt_hash: &str,
    exact_prompt_required: bool,
) -> Result<()> {
    if message.role != MessageRole::User
        || message.tool_call_id.is_some()
        || message.assistant_kind.is_some()
        || !message.tool_calls.is_empty()
    {
        bail!("conversation promotion durable message must be a plain user message");
    }
    if message.id.trim().is_empty() || message.id.len() > 512 {
        bail!("conversation promotion durable user message id is invalid");
    }
    if crate::safe_persistence_text(&message.id) != message.id {
        bail!("conversation promotion durable user message id is not safe");
    }
    let content = message
        .content
        .as_deref()
        .filter(|content| !content.trim().is_empty())
        .context("conversation promotion durable user message content is empty")?;
    let persistence_projection = crate::safe_persistence_text(content);
    if persistence_projection != content && !is_known_safe_redacted_text(content) {
        bail!("conversation promotion durable user message content is not safe");
    }
    let mut hasher = Sha256::new();
    hasher.update(content.as_bytes());
    let safe_hash = format!("sha256:{:x}", hasher.finalize());
    let expected_prompt_hash = if exact_prompt_required {
        format!("{CONVERSATION_EXACT_PROMPT_REQUIRED_HASH_PREFIX}{safe_hash}")
    } else {
        format!("safe:{safe_hash}")
    };
    if prompt_hash != expected_prompt_hash {
        bail!("conversation promotion prompt hash does not match durable user message");
    }
    Ok(())
}

fn is_known_safe_redacted_text(value: &str) -> bool {
    if !value.contains("[redacted]") {
        return false;
    }
    for token in value.split_whitespace() {
        let lower = token.to_ascii_lowercase();
        for marker in [
            "token=",
            "secret=",
            "password=",
            "api_key=",
            "apikey=",
            "authorization=",
        ] {
            if let Some(index) = lower.find(marker)
                && !token[index + marker.len()..].starts_with("[redacted]")
            {
                return false;
            }
        }
        if (lower.starts_with("https://") || lower.starts_with("http://"))
            && let Some((_, query)) = token.split_once('?')
            && query != "[redacted]"
        {
            return false;
        }
    }
    true
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

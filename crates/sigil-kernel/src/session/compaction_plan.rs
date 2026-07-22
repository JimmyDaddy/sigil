use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use super::*;
use crate::{EventId, MessageRole, ProjectionCursor, decode_stored_event};

/// Schema version for the durable-cursor safe-fold planning shape.
pub const COMPACTION_FOLD_PLAN_SCHEMA_VERSION: u16 = 1;

/// Stable reference to one raw durable event without copying its payload into a plan.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct CompactionEventRef {
    pub stream_sequence: u64,
    pub event_id: EventId,
}

/// Why a durable event cannot be folded from the provider-visible raw history.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum CompactionFoldProtectionReason {
    /// This message is already represented by the active V2 checkpoint boundary and must not be
    /// folded into a later checkpoint a second time.
    ExistingCompactionBoundary,
    /// This is an append-only control entry whose state must remain independently replayable.
    ControlState,
    /// This direct or unknown non-critical event is not a provider-visible chat message.
    NonMessageDurableEvent,
    /// The message has malformed role/tool fields and cannot safely participate in a fold.
    MalformedMessage,
    /// A tool-call assistant message is missing, duplicates, or ambiguously binds a result.
    UnsafeToolPair,
    /// A tool result has no uniquely matching prior assistant tool call.
    UnpairedToolResult,
}

/// One protected raw event and the reason it remains outside the fold range.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct ProtectedCompactionEventRef {
    pub event: CompactionEventRef,
    pub reason: CompactionFoldProtectionReason,
}

/// A replay-safe plan for folding only durable chat history.
///
/// This is intentionally a planning contract, not an apply operation: it never removes JSONL
/// records, changes the active V2 boundary, materializes a summary, or sends a provider request.
/// `base_stream_cursor`, the exact folded ids, and the exact retained ids make later apply-time
/// stale-plan checks possible without putting raw message contents in the plan.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct CompactionFoldPlan {
    pub schema_version: u16,
    /// Session scope shared by every event reference in this plan.
    pub session_id: crate::SessionId,
    /// Exact durable tail observed when the plan was built.
    pub base_stream_cursor: ProjectionCursor,
    /// Requested raw-tail message count, normalized to at least one.
    pub requested_tail_message_count: usize,
    /// The latest previously activated fold boundary, when this is a repeated compaction plan.
    ///
    /// All messages at or before this durable cursor are already represented by the previous
    /// checkpoint and therefore remain outside the next fold input.
    pub prior_folded_through: Option<CompactionCursor>,
    /// The greatest folded message cursor, when at least one message is safe to fold.
    pub folded_through: Option<CompactionCursor>,
    /// Provider-visible message event ids that a later checkpoint may cover.
    pub folded_event_ids: Vec<EventId>,
    /// Provider-visible message event ids that must remain raw in the next projection.
    pub retained_event_ids: Vec<EventId>,
    /// Controls, non-message events, and unsafe message pairs that are never candidates here.
    pub protected_events: Vec<ProtectedCompactionEventRef>,
}

/// Read-only V2 compaction planning result for a durable session stream.
///
/// This contains only the safe fold decision and the currently activated V2 boundary. It does
/// not create an attempt, checkpoint, task memory, token estimate, or provider request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct V2CompactionPreview {
    /// Exact fold/retain/protection decision reconstructed from the durable stream.
    pub plan: CompactionFoldPlan,
    /// The active compaction that supplied the prior boundary, when one exists.
    pub active_compaction_id: Option<CompactionId>,
}

impl CompactionFoldPlan {
    /// Builds a safe fold plan from the complete, validated V2 durable session stream.
    ///
    /// Every non-message/control event is protected. A tool-call assistant message and all of its
    /// uniquely matching tool results move as one unit; incomplete or ambiguous pairs are
    /// protected. The most recent requested raw messages are retained, then expanded to include
    /// any complete tool pair they intersect.
    ///
    /// # Errors
    ///
    /// Returns an error for an empty, malformed, cross-session, non-contiguous, or checksum
    /// invalid stream. A caller must rebuild rather than applying a plan to a changed stream.
    pub fn from_records(
        records: &[SessionStreamRecord],
        requested_tail_message_count: usize,
    ) -> Result<Self> {
        Self::from_records_after(records, requested_tail_message_count, None)
    }

    /// Builds a safe fold plan for history after an already activated V2 boundary.
    ///
    /// The previous boundary must name an exact event in the same complete stream. Earlier
    /// messages stay durable but are explicitly protected so repeated compaction never treats an
    /// already checkpointed prefix as new semantic input.
    ///
    /// # Errors
    ///
    /// Returns an error when the supplied boundary does not identify an exact record in the
    /// stream, in addition to the validation errors from [`Self::from_records`].
    pub fn from_records_after(
        records: &[SessionStreamRecord],
        requested_tail_message_count: usize,
        prior_folded_through: Option<&CompactionCursor>,
    ) -> Result<Self> {
        let requested_tail_message_count = requested_tail_message_count.max(1);
        let session_id = validate_complete_stream(records)?;
        validate_prior_folded_through(records, &session_id, prior_folded_through)?;

        let visible_promotions = super::conversation_promotion_projection::
            provider_visible_conversation_promotion_event_ids(records)?;
        let promoted_message_ids = records
            .iter()
            .filter(|record| visible_promotions.contains(record.event_id()))
            .filter_map(|record| session_entry_from_stored_event(record.stored_event()).transpose())
            .collect::<Result<Vec<_>>>()?
            .into_iter()
            .filter_map(|entry| match entry {
                SessionLogEntry::Control(ControlEntry::ConversationInputPromoted(promotion)) => {
                    Some(promotion.durable_user_message.id)
                }
                _ => None,
            })
            .collect::<BTreeSet<_>>();
        let mut messages = Vec::new();
        let mut protected = BTreeMap::new();
        for record in records {
            let event = record.stored_event();
            // Decode first so manually supplied malformed known events cannot be silently
            // classified as harmless non-message records.
            decode_stored_event(event.clone())?;
            let reference = event_ref(event);
            match session_entry_from_stored_event(event)? {
                Some(SessionLogEntry::Control(ControlEntry::ConversationInputPromoted(
                    promotion,
                ))) if visible_promotions.contains(&event.event_id) => {
                    messages.push(FoldMessage {
                        event: reference,
                        message: promotion.durable_user_message,
                    });
                }
                Some(SessionLogEntry::User(message))
                    if promoted_message_ids.contains(&message.id) =>
                {
                    protected.insert(reference, CompactionFoldProtectionReason::ControlState);
                }
                Some(SessionLogEntry::User(message))
                | Some(SessionLogEntry::Assistant(message))
                | Some(SessionLogEntry::ToolResult(message)) => {
                    messages.push(FoldMessage {
                        event: reference,
                        message,
                    });
                }
                Some(SessionLogEntry::Control(_)) => {
                    protected.insert(reference, CompactionFoldProtectionReason::ControlState);
                }
                None => {
                    protected.insert(
                        reference,
                        CompactionFoldProtectionReason::NonMessageDurableEvent,
                    );
                }
            }
        }

        let groups = classify_tool_pair_groups(&messages, &mut protected);
        if let Some(prior) = prior_folded_through {
            for candidate in &messages {
                if candidate.event.stream_sequence <= prior.through_stream_sequence {
                    protected.insert(
                        candidate.event.clone(),
                        CompactionFoldProtectionReason::ExistingCompactionBoundary,
                    );
                }
            }
        }
        let mut retained_indexes = messages
            .iter()
            .enumerate()
            .rev()
            .filter(|(_, candidate)| {
                prior_folded_through.is_none() || !protected.contains_key(&candidate.event)
            })
            .take(requested_tail_message_count)
            .map(|(index, _)| index)
            .collect::<BTreeSet<_>>();

        // Retaining any member of a complete pair retains every member. Repeat because a pair
        // can pull an older assistant into the tail, and ordering must remain explicit.
        let mut changed = true;
        while changed {
            changed = false;
            for group in &groups {
                if group.iter().any(|index| retained_indexes.contains(index)) {
                    for index in group {
                        changed |= retained_indexes.insert(*index);
                    }
                }
            }
        }

        let mut folded_event_ids = Vec::new();
        let mut retained_event_ids = Vec::new();
        let mut folded_through = None;
        for (index, candidate) in messages.iter().enumerate() {
            if protected.contains_key(&candidate.event) {
                continue;
            }
            if retained_indexes.contains(&index) {
                retained_event_ids.push(candidate.event.event_id.clone());
            } else {
                folded_through = Some(CompactionCursor {
                    session_id: session_id.clone(),
                    through_stream_sequence: candidate.event.stream_sequence,
                    through_event_id: candidate.event.event_id.clone(),
                });
                folded_event_ids.push(candidate.event.event_id.clone());
            }
        }

        let base_stream_cursor = records
            .last()
            .expect("validated complete stream is non-empty")
            .projection_cursor(COMPACTION_FOLD_PLAN_SCHEMA_VERSION);
        Ok(Self {
            schema_version: COMPACTION_FOLD_PLAN_SCHEMA_VERSION,
            session_id,
            base_stream_cursor,
            requested_tail_message_count,
            prior_folded_through: prior_folded_through.cloned(),
            folded_through,
            folded_event_ids,
            retained_event_ids,
            protected_events: protected
                .into_iter()
                .map(|(event, reason)| ProtectedCompactionEventRef { event, reason })
                .collect(),
        })
    }

    /// Rebuilds and compares the plan against the current complete durable stream.
    ///
    /// # Errors
    ///
    /// Returns an error when the session changed or the rebuilt protection/fold decision differs.
    /// A later compaction apply must perform this check under its single-writer CAS boundary.
    pub fn validate_against(&self, records: &[SessionStreamRecord]) -> Result<()> {
        if self.schema_version != COMPACTION_FOLD_PLAN_SCHEMA_VERSION {
            bail!("unsupported compaction fold-plan schema version");
        }
        let current = Self::from_records_after(
            records,
            self.requested_tail_message_count,
            self.prior_folded_through.as_ref(),
        )?;
        if &current != self {
            bail!("compaction fold plan is stale against the current durable stream");
        }
        Ok(())
    }

    /// Returns whether this plan contains any newly safe foldable history.
    #[must_use]
    pub fn has_foldable_history(&self) -> bool {
        !self.folded_event_ids.is_empty()
    }
}

impl JsonlSessionStore {
    /// Rebuilds a read-only V2 compaction preview from the current durable stream.
    ///
    /// The preview is unavailable when there is no newly foldable history. An unfinished V2
    /// attempt is rejected rather than being implicitly recovered or overwritten. This query
    /// never appends a lifecycle event or rewrites the JSONL transcript.
    ///
    /// # Errors
    ///
    /// Returns an error when the V2 stream, lifecycle, or active sidecar is invalid, or when an
    /// unfinished compaction attempt makes a new plan unsafe.
    pub fn v2_compaction_preview(
        &self,
        requested_tail_message_count: usize,
        branch_id: Option<&str>,
    ) -> Result<Option<V2CompactionPreview>> {
        let records = Self::read_event_records(self.path())?;
        if records.is_empty() {
            return Ok(None);
        }
        let lifecycle = CompactionLifecycleProjection::from_records(&records)?;
        if !lifecycle.unfinished_attempts().is_empty() {
            bail!("cannot preview V2 compaction while another attempt is unfinished");
        }
        let sidecars = CompactionSidecarProjection::from_records(&records)?;
        let active = sidecars.latest_for_branch(branch_id);
        let prior_folded_through = active.map(|sidecar| sidecar.folded_through.clone());
        let plan = CompactionFoldPlan::from_records_after(
            &records,
            requested_tail_message_count,
            prior_folded_through.as_ref(),
        )?;
        if !plan.has_foldable_history() {
            return Ok(None);
        }
        Ok(Some(V2CompactionPreview {
            plan,
            active_compaction_id: active.map(|sidecar| sidecar.compaction_id.clone()),
        }))
    }
}

fn validate_prior_folded_through(
    records: &[SessionStreamRecord],
    session_id: &str,
    prior_folded_through: Option<&CompactionCursor>,
) -> Result<()> {
    let Some(prior) = prior_folded_through else {
        return Ok(());
    };
    if prior.session_id != session_id || prior.through_stream_sequence == 0 {
        bail!("compaction fold-plan prior boundary has an invalid session or sequence");
    }
    let prior_index = usize::try_from(prior.through_stream_sequence - 1)
        .context("compaction fold-plan prior boundary sequence overflows usize")?;
    let record = records
        .get(prior_index)
        .context("compaction fold-plan prior boundary is outside the durable stream")?;
    if record.event_id() != prior.through_event_id {
        bail!("compaction fold-plan prior boundary event id does not match the durable stream");
    }
    Ok(())
}

#[derive(Debug, Clone)]
struct FoldMessage {
    event: CompactionEventRef,
    message: crate::ModelMessage,
}

fn validate_complete_stream(records: &[SessionStreamRecord]) -> Result<crate::SessionId> {
    let Some(first) = records.first() else {
        bail!("cannot build a compaction fold plan for an empty durable stream");
    };
    let session_id = first.session_id().to_owned();
    if session_id.trim().is_empty() {
        bail!("compaction fold-plan stream session id is empty");
    }
    for (offset, record) in records.iter().enumerate() {
        let event = record.stored_event();
        event
            .verify_record_checksum()
            .context("compaction fold-plan record checksum is invalid")?;
        if event.session_id != session_id {
            bail!("compaction fold-plan stream spans multiple sessions");
        }
        let expected_sequence = offset as u64 + 1;
        if event.stream_sequence != expected_sequence {
            bail!("compaction fold-plan stream sequence is not contiguous");
        }
        if event.event_id.trim().is_empty() {
            bail!("compaction fold-plan event id is empty");
        }
    }
    Ok(session_id)
}

fn event_ref(event: &crate::StoredEvent) -> CompactionEventRef {
    CompactionEventRef {
        stream_sequence: event.stream_sequence,
        event_id: event.event_id.clone(),
    }
}

fn classify_tool_pair_groups(
    messages: &[FoldMessage],
    protected: &mut BTreeMap<CompactionEventRef, CompactionFoldProtectionReason>,
) -> Vec<BTreeSet<usize>> {
    let mut owners = BTreeMap::<String, usize>::new();
    let mut calls_by_assistant = BTreeMap::<usize, Vec<String>>::new();

    for (index, candidate) in messages.iter().enumerate() {
        let message = &candidate.message;
        match message.role {
            MessageRole::User => {
                if !message.tool_calls.is_empty() || message.tool_call_id.is_some() {
                    protect(
                        protected,
                        candidate,
                        CompactionFoldProtectionReason::MalformedMessage,
                    );
                }
            }
            MessageRole::Assistant => {
                if message.tool_call_id.is_some() {
                    protect(
                        protected,
                        candidate,
                        CompactionFoldProtectionReason::MalformedMessage,
                    );
                    continue;
                }
                if message.tool_calls.is_empty() {
                    continue;
                }
                let mut call_ids = BTreeSet::new();
                let mut malformed = false;
                for call in &message.tool_calls {
                    if call.id.trim().is_empty() || !call_ids.insert(call.id.clone()) {
                        malformed = true;
                        continue;
                    }
                    if let Some(previous) = owners.insert(call.id.clone(), index) {
                        malformed = true;
                        protect(
                            protected,
                            &messages[previous],
                            CompactionFoldProtectionReason::UnsafeToolPair,
                        );
                    }
                }
                if malformed {
                    protect(
                        protected,
                        candidate,
                        CompactionFoldProtectionReason::UnsafeToolPair,
                    );
                } else {
                    calls_by_assistant.insert(index, call_ids.into_iter().collect());
                }
            }
            MessageRole::Tool => {
                if !message.tool_calls.is_empty()
                    || message
                        .tool_call_id
                        .as_deref()
                        .is_none_or(|tool_call_id| tool_call_id.trim().is_empty())
                {
                    protect(
                        protected,
                        candidate,
                        CompactionFoldProtectionReason::MalformedMessage,
                    );
                }
            }
            MessageRole::System => protect(
                protected,
                candidate,
                CompactionFoldProtectionReason::MalformedMessage,
            ),
        }
    }

    let mut results_by_call = BTreeMap::<String, Vec<usize>>::new();
    for (index, candidate) in messages.iter().enumerate() {
        let message = &candidate.message;
        if !matches!(message.role, MessageRole::Tool) || protected.contains_key(&candidate.event) {
            continue;
        }
        let Some(call_id) = message.tool_call_id.as_deref() else {
            continue;
        };
        results_by_call
            .entry(call_id.to_owned())
            .or_default()
            .push(index);
    }

    let mut groups = Vec::new();
    for (assistant_index, call_ids) in &calls_by_assistant {
        let assistant = &messages[*assistant_index];
        let mut group = BTreeSet::from([*assistant_index]);
        let mut safe = !protected.contains_key(&assistant.event);
        for call_id in call_ids {
            let result_indexes = results_by_call.get(call_id).cloned().unwrap_or_default();
            if result_indexes.len() != 1 || result_indexes[0] <= *assistant_index {
                safe = false;
                for result_index in result_indexes {
                    protect(
                        protected,
                        &messages[result_index],
                        CompactionFoldProtectionReason::UnsafeToolPair,
                    );
                }
                continue;
            }
            group.insert(result_indexes[0]);
        }
        if safe {
            groups.push(group);
        } else {
            protect(
                protected,
                assistant,
                CompactionFoldProtectionReason::UnsafeToolPair,
            );
        }
    }

    for (index, candidate) in messages.iter().enumerate() {
        if !matches!(candidate.message.role, MessageRole::Tool)
            || protected.contains_key(&candidate.event)
        {
            continue;
        }
        let Some(call_id) = candidate.message.tool_call_id.as_deref() else {
            continue;
        };
        let Some(owner) = owners.get(call_id) else {
            protect(
                protected,
                candidate,
                CompactionFoldProtectionReason::UnpairedToolResult,
            );
            continue;
        };
        if protected.contains_key(&messages[*owner].event)
            || !calls_by_assistant.contains_key(owner)
            || index <= *owner
        {
            protect(
                protected,
                candidate,
                CompactionFoldProtectionReason::UnsafeToolPair,
            );
        }
    }
    groups
}

fn protect(
    protected: &mut BTreeMap<CompactionEventRef, CompactionFoldProtectionReason>,
    candidate: &FoldMessage,
    reason: CompactionFoldProtectionReason,
) {
    protected.entry(candidate.event.clone()).or_insert(reason);
}

#[cfg(test)]
#[path = "tests/compaction_plan_tests.rs"]
mod tests;

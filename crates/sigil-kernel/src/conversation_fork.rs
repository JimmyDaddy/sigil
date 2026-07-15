use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};
use serde::{Deserialize, Serialize};

use crate::{
    ControlEntry, ControlledCheckpointProjection, DurableEventType, EventClass,
    ExternalProvenanceEntry, JsonlSessionStore, Session, SessionLogEntry, SessionRef,
    SessionStreamRecord, StoredEvent, stable_event_hash, stable_event_uuid,
};

/// Stable, append-only binding for one finalized user turn that can be forked safely.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct ConversationForkPoint {
    pub source_session_id: String,
    pub source_turn_index: usize,
    pub source_boundary_event_id: String,
    pub source_boundary_stream_sequence: u64,
    pub source_finalized_event_id: String,
    pub source_finalized_stream_sequence: u64,
    pub source_turn_digest: String,
}

/// Rebuildable view of all complete conversation turns in one durable session stream.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ConversationForkProjection {
    pub points: Vec<ConversationForkPoint>,
}

impl ConversationForkProjection {
    /// Projects finalized user turns without requiring a controlled file mutation.
    ///
    /// # Errors
    ///
    /// Returns an error when a stored session entry cannot be decoded.
    pub fn from_records(records: &[SessionStreamRecord]) -> Result<Self> {
        let mut points = Vec::new();
        let mut current = None::<ConversationTurnBuilder>;
        let mut turn_index = 0usize;

        for record in records {
            if matches!(session_entry(record)?, Some(SessionLogEntry::User(_))) {
                turn_index = turn_index.saturating_add(1);
                current = Some(ConversationTurnBuilder {
                    source_session_id: record.session_id().to_owned(),
                    source_turn_index: turn_index,
                    source_boundary_event_id: record.event_id().to_owned(),
                    source_boundary_stream_sequence: record.stream_sequence(),
                });
                continue;
            }
            let Some(builder) = current.as_ref() else {
                continue;
            };
            if matches!(
                record,
                SessionStreamRecord::Stored(event)
                    if event.event_kind() == Some(DurableEventType::RunFinalized)
            ) {
                points.push(builder.finish(record)?);
                current = None;
            }
        }
        Ok(Self { points })
    }

    #[must_use]
    pub fn latest(&self) -> Option<&ConversationForkPoint> {
        self.points.last()
    }

    #[must_use]
    pub fn point(&self, digest: &str) -> Option<&ConversationForkPoint> {
        self.points
            .iter()
            .find(|point| point.source_turn_digest == digest)
    }
}

#[derive(Debug)]
struct ConversationTurnBuilder {
    source_session_id: String,
    source_turn_index: usize,
    source_boundary_event_id: String,
    source_boundary_stream_sequence: u64,
}

impl ConversationTurnBuilder {
    fn finish(&self, finalized: &SessionStreamRecord) -> Result<ConversationForkPoint> {
        let digest = stable_event_hash(
            serde_json::to_vec(&(
                &self.source_session_id,
                self.source_turn_index,
                &self.source_boundary_event_id,
                self.source_boundary_stream_sequence,
                finalized.event_id(),
                finalized.stream_sequence(),
            ))
            .context("failed to encode conversation fork point")?,
        );
        Ok(ConversationForkPoint {
            source_session_id: self.source_session_id.clone(),
            source_turn_index: self.source_turn_index,
            source_boundary_event_id: self.source_boundary_event_id.clone(),
            source_boundary_stream_sequence: self.source_boundary_stream_sequence,
            source_finalized_event_id: finalized.event_id().to_owned(),
            source_finalized_stream_sequence: finalized.stream_sequence(),
            source_turn_digest: digest,
        })
    }
}

/// Exact source binding and destination identity for one conversation fork.
#[derive(Debug, Clone)]
pub struct ConversationForkRequest {
    pub checkpoint_id: String,
    pub checkpoint_digest: String,
    pub source_session_ref: SessionRef,
    pub destination_path: PathBuf,
    pub provider_name: String,
    pub model_name: String,
}

/// Exact source-turn binding and destination identity for a general local conversation fork.
#[derive(Debug, Clone)]
pub struct ConversationTurnForkRequest {
    pub source_turn_digest: String,
    pub source_session_ref: SessionRef,
    pub destination_path: PathBuf,
    pub provider_name: String,
    pub model_name: String,
}

/// Durable provenance written into the destination before its safe conversation prefix.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct ConversationForked {
    pub fork_id: String,
    pub parent_session_ref: SessionRef,
    pub source_session_id: String,
    pub source_turn_index: usize,
    pub source_boundary_event_id: String,
    pub source_boundary_stream_sequence: u64,
    pub source_turn_digest: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_checkpoint_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_checkpoint_digest: Option<String>,
    pub destination_session_id: String,
    pub copied_message_count: usize,
    pub copied_external_provenance_count: usize,
}

/// Result of creating a new append-only conversation branch.
#[derive(Debug)]
pub struct ConversationForkOutput {
    pub destination_session_ref: SessionRef,
    pub destination_path: PathBuf,
    pub destination_session_id: String,
    pub fork_event: StoredEvent,
    pub copied_message_count: usize,
    pub copied_external_provenance_count: usize,
}

/// Creates a new session containing only a complete safe conversation prefix and rebound
/// external provenance. The parent stream is read-only and active control/mutation state is never
/// copied.
///
/// # Errors
///
/// Returns an error when the checkpoint binding is stale, the selected turn is incomplete, the
/// destination is outside the parent session directory or already exists, or safe provenance
/// cannot be rebound to the destination session scope.
pub fn fork_conversation_at_checkpoint(
    source_store: &JsonlSessionStore,
    records: &[SessionStreamRecord],
    request: &ConversationForkRequest,
) -> Result<ConversationForkOutput> {
    validate_source_and_destination(
        source_store.path(),
        &request.source_session_ref,
        &request.destination_path,
    )?;
    let projection = ControlledCheckpointProjection::from_records(records)?;
    let checkpoint = projection
        .checkpoint(&request.checkpoint_id)
        .cloned()
        .ok_or_else(|| anyhow!("controlled checkpoint is no longer available"))?;
    if checkpoint.checkpoint_digest != request.checkpoint_digest {
        bail!("controlled checkpoint changed since the fork action was rendered");
    }

    let point = ConversationForkProjection::from_records(records)?
        .points
        .into_iter()
        .find(|point| {
            point.source_boundary_event_id == checkpoint.turn_boundary_event_id
                && point.source_boundary_stream_sequence == checkpoint.turn_boundary_stream_sequence
        })
        .ok_or_else(|| anyhow!("conversation fork requires a finalized user turn"))?;
    create_conversation_fork(
        source_store,
        records,
        request.source_session_ref.clone(),
        request.destination_path.clone(),
        request.provider_name.clone(),
        request.model_name.clone(),
        point,
        Some((
            checkpoint.checkpoint_id.clone(),
            checkpoint.checkpoint_digest.clone(),
        )),
    )
}

/// Creates a new session from any finalized user turn, including turns without file mutations.
///
/// # Errors
///
/// Returns an error when the turn binding is stale, the destination is unsafe, or the safe
/// conversation prefix cannot be persisted.
pub fn fork_conversation_at_turn(
    source_store: &JsonlSessionStore,
    records: &[SessionStreamRecord],
    request: &ConversationTurnForkRequest,
) -> Result<ConversationForkOutput> {
    validate_source_and_destination(
        source_store.path(),
        &request.source_session_ref,
        &request.destination_path,
    )?;
    let point = ConversationForkProjection::from_records(records)?
        .point(&request.source_turn_digest)
        .cloned()
        .ok_or_else(|| anyhow!("conversation fork turn changed or is no longer available"))?;
    create_conversation_fork(
        source_store,
        records,
        request.source_session_ref.clone(),
        request.destination_path.clone(),
        request.provider_name.clone(),
        request.model_name.clone(),
        point,
        None,
    )
}

#[allow(clippy::too_many_arguments)]
fn create_conversation_fork(
    source_store: &JsonlSessionStore,
    records: &[SessionStreamRecord],
    source_session_ref: SessionRef,
    destination_path: PathBuf,
    provider_name: String,
    model_name: String,
    point: ConversationForkPoint,
    checkpoint: Option<(String, String)>,
) -> Result<ConversationForkOutput> {
    validate_source_and_destination(source_store.path(), &source_session_ref, &destination_path)?;
    let prefix = safe_prefix_for_complete_turn(records, &point)?;
    let destination_store = JsonlSessionStore::new(&destination_path)?;
    let mut destination = Session::new(&provider_name, &model_name).with_store(destination_store);
    destination.append_control(ControlEntry::SessionIdentity {
        provider_name: provider_name.clone(),
        model_name: model_name.clone(),
    })?;
    let destination_session_id = destination.session_scope_id().to_owned();
    let destination_session_ref = SessionRef::new_relative(
        destination_path
            .file_name()
            .ok_or_else(|| anyhow!("conversation fork destination has no file name"))?,
    )?;
    let fork_id = format!(
        "conversation-fork:{}",
        stable_event_uuid(
            "sigil-conversation-fork",
            &format!(
                "{}:{}:{}",
                point.source_session_id, point.source_turn_digest, destination_session_id
            ),
        )
    );
    let payload = ConversationForked {
        fork_id,
        parent_session_ref: source_session_ref,
        source_session_id: point.source_session_id.clone(),
        source_turn_index: point.source_turn_index,
        source_boundary_event_id: point.source_boundary_event_id.clone(),
        source_boundary_stream_sequence: point.source_boundary_stream_sequence,
        source_turn_digest: point.source_turn_digest,
        source_checkpoint_id: checkpoint.as_ref().map(|(id, _)| id.clone()),
        source_checkpoint_digest: checkpoint.map(|(_, digest)| digest),
        destination_session_id: destination_session_id.clone(),
        copied_message_count: prefix.messages.len(),
        copied_external_provenance_count: prefix.provenance.len(),
    };
    let fork_event = destination
        .append_durable_event(
            DurableEventType::ConversationForked,
            EventClass::Critical,
            serde_json::to_value(&payload).context("failed to encode conversation fork")?,
        )?
        .ok_or_else(|| anyhow!("conversation fork destination is not durable"))?;

    for entry in &prefix.messages {
        destination.append(entry.clone())?;
    }
    for provenance in prefix.provenance {
        destination.append_external_provenance(rebind_external_provenance(
            provenance,
            &destination_session_id,
        )?)?;
    }

    Ok(ConversationForkOutput {
        destination_session_ref,
        destination_path,
        destination_session_id,
        fork_event,
        copied_message_count: prefix.messages.len(),
        copied_external_provenance_count: payload.copied_external_provenance_count,
    })
}

#[derive(Debug)]
struct SafePrefix {
    messages: Vec<SessionLogEntry>,
    provenance: Vec<ExternalProvenanceEntry>,
}

fn safe_prefix_for_complete_turn(
    records: &[SessionStreamRecord],
    point: &ConversationForkPoint,
) -> Result<SafePrefix> {
    let mut reached_boundary = false;
    let mut reached_finalization = false;
    let mut messages = Vec::new();
    let mut message_ids = BTreeSet::new();
    let mut provenance = Vec::new();

    for record in records {
        let entry = session_entry(record)?;
        if reached_boundary
            && matches!(entry, Some(SessionLogEntry::User(_)))
            && record.event_id() != point.source_boundary_event_id
        {
            break;
        }
        if record.event_id() == point.source_boundary_event_id
            && record.stream_sequence() == point.source_boundary_stream_sequence
        {
            if !matches!(entry, Some(SessionLogEntry::User(_))) {
                bail!("conversation fork boundary is not a user message");
            }
            reached_boundary = true;
        }

        if let Some(entry) = entry {
            match entry {
                SessionLogEntry::User(message) => {
                    message_ids.insert(message.id.clone());
                    messages.push(SessionLogEntry::User(message));
                }
                SessionLogEntry::Assistant(message) => {
                    message_ids.insert(message.id.clone());
                    messages.push(SessionLogEntry::Assistant(message));
                }
                SessionLogEntry::ToolResult(message) => {
                    message_ids.insert(message.id.clone());
                    messages.push(SessionLogEntry::ToolResult(message));
                }
                SessionLogEntry::Control(ControlEntry::ExternalProvenance(entry)) => {
                    provenance.push(entry);
                }
                SessionLogEntry::Control(_) => {}
            }
        }
        if reached_boundary
            && record.event_id() == point.source_finalized_event_id
            && record.stream_sequence() == point.source_finalized_stream_sequence
        {
            reached_finalization = true;
            break;
        }
    }
    if !reached_boundary {
        bail!("conversation fork boundary is missing from the source stream");
    }
    if !reached_finalization {
        bail!("conversation fork requires a finalized user turn");
    }
    provenance.retain(|entry| message_ids.contains(&entry.message_id));
    Ok(SafePrefix {
        messages,
        provenance,
    })
}

fn session_entry(record: &SessionStreamRecord) -> Result<Option<SessionLogEntry>> {
    match record {
        SessionStreamRecord::Stored(event) => event
            .payload
            .get("session_log_entry")
            .cloned()
            .map(serde_json::from_value)
            .transpose()
            .context("failed to decode conversation fork session entry"),
    }
}

fn validate_source_and_destination(
    source_path: &Path,
    source_session_ref: &SessionRef,
    destination_path: &Path,
) -> Result<()> {
    if destination_path.exists() {
        bail!("conversation fork destination already exists");
    }
    let source_path = fs::canonicalize(source_path)
        .with_context(|| format!("failed to resolve source session {}", source_path.display()))?;
    let source_parent = source_path
        .parent()
        .ok_or_else(|| anyhow!("source session has no parent directory"))?;
    let referenced_source = source_session_ref.resolve(source_parent);
    if fs::canonicalize(&referenced_source).ok().as_deref() != Some(source_path.as_path()) {
        bail!("conversation fork parent session ref does not identify the source store");
    }
    let destination_parent = destination_path
        .parent()
        .ok_or_else(|| anyhow!("conversation fork destination has no parent directory"))?;
    let destination_parent = fs::canonicalize(destination_parent).with_context(|| {
        format!(
            "failed to resolve conversation fork destination directory {}",
            destination_parent.display()
        )
    })?;
    if destination_parent != source_parent {
        bail!("conversation fork destination must share the parent session directory");
    }
    Ok(())
}

fn rebind_external_provenance(
    mut provenance: ExternalProvenanceEntry,
    destination_session_id: &str,
) -> Result<ExternalProvenanceEntry> {
    let mut source_ids = BTreeMap::new();
    for source in &mut provenance.sources {
        let source_id = format!(
            "src_{}",
            stable_event_uuid(
                "sigil-conversation-fork-source",
                &format!(
                    "{}:{}:{}",
                    destination_session_id, provenance.message_id, source.source_id
                ),
            )
            .replace('-', "")
        );
        source_ids.insert(source.source_id.clone(), source_id.clone());
        source.session_scope_id = destination_session_id.to_owned();
        source.source_id = source_id;
    }
    for citation in &mut provenance.citations {
        citation.session_scope_id = destination_session_id.to_owned();
        citation.source_id = source_ids
            .get(&citation.source_id)
            .cloned()
            .ok_or_else(|| anyhow!("conversation fork citation references an unknown source"))?;
    }
    provenance.session_scope_id = destination_session_id.to_owned();
    Ok(provenance)
}

#[cfg(test)]
#[path = "tests/conversation_fork_tests.rs"]
mod tests;

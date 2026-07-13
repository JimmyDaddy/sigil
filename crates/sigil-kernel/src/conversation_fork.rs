use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};
use serde::{Deserialize, Serialize};

use crate::{
    ControlEntry, ControlledCheckpointProjection, DurableEventType, EventClass,
    ExternalProvenanceEntry, JsonlSessionStore, Session, SessionLogEntry, SessionRef,
    SessionStreamRecord, StoredEvent, stable_event_uuid,
};

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
    pub source_checkpoint_id: String,
    pub source_checkpoint_digest: String,
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
    validate_source_and_destination(source_store.path(), request)?;
    let projection = ControlledCheckpointProjection::from_records(records)?;
    let checkpoint = projection
        .checkpoint(&request.checkpoint_id)
        .cloned()
        .ok_or_else(|| anyhow!("controlled checkpoint is no longer available"))?;
    if checkpoint.checkpoint_digest != request.checkpoint_digest {
        bail!("controlled checkpoint changed since the fork action was rendered");
    }

    let prefix = safe_prefix_for_complete_turn(records, &checkpoint)?;
    let destination_store = JsonlSessionStore::new(&request.destination_path)?;
    let mut destination =
        Session::new(&request.provider_name, &request.model_name).with_store(destination_store);
    destination.append_control(ControlEntry::SessionIdentity {
        provider_name: request.provider_name.clone(),
        model_name: request.model_name.clone(),
    })?;
    let destination_session_id = destination.session_scope_id().to_owned();
    let destination_session_ref = SessionRef::new_relative(
        request
            .destination_path
            .file_name()
            .ok_or_else(|| anyhow!("conversation fork destination has no file name"))?,
    )?;
    let fork_id = format!(
        "conversation-fork:{}",
        stable_event_uuid(
            "sigil-conversation-fork",
            &format!(
                "{}:{}:{}",
                checkpoint.source_session_id, checkpoint.checkpoint_id, destination_session_id
            ),
        )
    );
    let payload = ConversationForked {
        fork_id,
        parent_session_ref: request.source_session_ref.clone(),
        source_session_id: checkpoint.source_session_id.clone(),
        source_turn_index: checkpoint.turn_index,
        source_boundary_event_id: checkpoint.turn_boundary_event_id.clone(),
        source_boundary_stream_sequence: checkpoint.turn_boundary_stream_sequence,
        source_checkpoint_id: checkpoint.checkpoint_id.clone(),
        source_checkpoint_digest: checkpoint.checkpoint_digest.clone(),
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
        destination_path: request.destination_path.clone(),
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
    checkpoint: &crate::ControlledCheckpoint,
) -> Result<SafePrefix> {
    let mut reached_boundary = false;
    let mut finalized = false;
    let mut messages = Vec::new();
    let mut message_ids = BTreeSet::new();
    let mut provenance = Vec::new();

    for record in records {
        let entry = session_entry(record)?;
        if reached_boundary
            && matches!(entry, Some(SessionLogEntry::User(_)))
            && record.event_id() != checkpoint.turn_boundary_event_id
        {
            break;
        }
        if record.event_id() == checkpoint.turn_boundary_event_id
            && record.stream_sequence() == checkpoint.turn_boundary_stream_sequence
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
            && matches!(
                record,
                SessionStreamRecord::Stored(event)
                    if event.event_kind() == Some(DurableEventType::RunFinalized)
            )
        {
            finalized = true;
        }
    }
    if !reached_boundary {
        bail!("conversation fork boundary is missing from the source stream");
    }
    if !finalized {
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
        SessionStreamRecord::Legacy { entry, .. } => Ok(Some((**entry).clone())),
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
    request: &ConversationForkRequest,
) -> Result<()> {
    if request.destination_path.exists() {
        bail!("conversation fork destination already exists");
    }
    let source_path = fs::canonicalize(source_path)
        .with_context(|| format!("failed to resolve source session {}", source_path.display()))?;
    let source_parent = source_path
        .parent()
        .ok_or_else(|| anyhow!("source session has no parent directory"))?;
    let referenced_source = request.source_session_ref.resolve(source_parent);
    if fs::canonicalize(&referenced_source).ok().as_deref() != Some(source_path.as_path()) {
        bail!("conversation fork parent session ref does not identify the source store");
    }
    let destination_parent = request
        .destination_path
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

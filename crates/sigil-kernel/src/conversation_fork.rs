use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};
use serde::{Deserialize, Serialize};

#[cfg(test)]
use crate::EventClass;
use crate::session::ToolArtifactDescriptorV1;
use crate::session::ToolArtifactStore;
use crate::{
    ControlEntry, ControlledCheckpointProjection, DurableEventType, ExternalProvenanceEntry,
    JsonlSessionStore, ResolvedModelRoute, Session, SessionCompositionSnapshotV1, SessionLogEntry,
    SessionRef, SessionStreamRecord, StoredEvent, ToolArtifactBindingV1, ToolResultRecordedV3,
    stable_event_hash, stable_event_uuid,
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
        crate::ConversationQueueDurableProjection::from_records(records)?;
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
    pub resolved_model_route: Option<ResolvedModelRoute>,
}

/// Exact source-turn binding and destination identity for a general local conversation fork.
#[derive(Debug, Clone)]
pub struct ConversationTurnForkRequest {
    pub source_turn_digest: String,
    pub source_session_ref: SessionRef,
    pub destination_path: PathBuf,
    pub provider_name: String,
    pub model_name: String,
    pub resolved_model_route: Option<ResolvedModelRoute>,
}

/// Durable provenance committed atomically with the complete safe conversation prefix.
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

/// Source-side audit of a completed copy. It grants no authority in either session and does
/// not change source conversation messages or workspace files.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ConversationForkCommittedV1 {
    pub source_session_id: String,
    pub source_turn_digest: String,
    pub destination_session_ref: SessionRef,
    pub destination_session_id: String,
    pub target_model_ref: crate::ModelRef,
    pub copied_message_count: usize,
    pub copied_external_provenance_count: usize,
}

impl ConversationForkCommittedV1 {
    /// Builds an audit only from the host's successfully created or exactly recovered branch.
    pub fn from_output(
        source_session_id: &str,
        source_turn_digest: &str,
        target_model_ref: &crate::ModelRef,
        output: &ConversationForkOutput,
    ) -> Result<Self> {
        let receipt = Self {
            source_session_id: source_session_id.to_owned(),
            source_turn_digest: source_turn_digest.to_owned(),
            destination_session_ref: output.destination_session_ref.clone(),
            destination_session_id: output.destination_session_id.clone(),
            target_model_ref: target_model_ref.clone(),
            copied_message_count: output.copied_message_count,
            copied_external_provenance_count: output.copied_external_provenance_count,
        };
        receipt.validate()?;
        Ok(receipt)
    }
    pub fn validate(&self) -> Result<()> {
        for value in [
            &self.source_session_id,
            &self.source_turn_digest,
            &self.destination_session_id,
        ] {
            if value.trim().is_empty() || value.len() > 512 || value.chars().any(char::is_control) {
                bail!("invalid conversation fork receipt identity");
            }
        }
        if self.source_session_id == self.destination_session_id {
            bail!("conversation fork must name a distinct destination");
        }
        Ok(())
    }
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
        request.resolved_model_route.clone(),
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
        request.resolved_model_route.clone(),
        point,
        None,
    )
}

/// Copies an exact finalized turn into a host-admitted empty session writer.
///
/// Logical catalog references describe lineage; they are not filesystem authority. The host
/// must bind the source records and both catalog references to its workspace and retain the
/// destination writer/artifact leases throughout this call. Published tool output is copied
/// through the supplied source and destination artifact capabilities, never inferred paths.
///
/// # Errors
/// Rejects a stale turn, mixed source identities, a nonempty or same-session destination,
/// unavailable artifact capabilities, and incomplete artifact or transcript publication.
pub fn fork_conversation_at_turn_into(
    records: &[SessionStreamRecord],
    source_session_ref: SessionRef,
    source_turn_digest: &str,
    destination: &mut Session,
    destination_session_ref: SessionRef,
    source_artifacts: Option<&ToolArtifactStore>,
) -> Result<ConversationForkOutput> {
    let point = ConversationForkProjection::from_records(records)?
        .point(source_turn_digest)
        .cloned()
        .context("conversation fork turn changed or is no longer available")?;
    persist_conversation_fork(
        records,
        source_session_ref,
        destination,
        destination_session_ref,
        point,
        None,
        source_artifacts,
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
    resolved_model_route: Option<ResolvedModelRoute>,
    point: ConversationForkPoint,
    checkpoint: Option<(String, String)>,
) -> Result<ConversationForkOutput> {
    validate_source_and_destination(source_store.path(), &source_session_ref, &destination_path)?;
    if let Some(route) = resolved_model_route.as_ref() {
        anyhow::ensure!(
            route.model_ref.model_id == model_name,
            "conversation fork route model does not match destination identity"
        );
    }
    let destination_store = JsonlSessionStore::new(&destination_path)?;
    let mut destination = resolved_model_route
        .map_or_else(
            || Session::new(&provider_name, &model_name),
            |route| Session::new_with_route(&provider_name, route),
        )
        .with_store(destination_store);
    #[cfg(not(any(test, feature = "test-support")))]
    let source_artifacts = None::<ToolArtifactStore>;
    #[cfg(any(test, feature = "test-support"))]
    let source_artifacts = {
        destination.attach_tool_artifact_store_override(ToolArtifactStore::for_session_path(
            &destination_path,
        ));
        Some(ToolArtifactStore::for_session_store(source_store))
    };
    let destination_session_ref = SessionRef::new_relative(
        destination_path
            .file_name()
            .context("conversation fork destination has no file name")?,
    )?;
    persist_conversation_fork(
        records,
        source_session_ref,
        &mut destination,
        destination_session_ref,
        point,
        checkpoint,
        source_artifacts.as_ref(),
    )
}

fn persist_conversation_fork(
    records: &[SessionStreamRecord],
    source_session_ref: SessionRef,
    destination: &mut Session,
    destination_session_ref: SessionRef,
    point: ConversationForkPoint,
    checkpoint: Option<(String, String)>,
    source_artifacts: Option<&ToolArtifactStore>,
) -> Result<ConversationForkOutput> {
    anyhow::ensure!(
        records
            .iter()
            .all(|record| record.session_id() == point.source_session_id),
        "conversation fork source contains mixed session identities"
    );
    let destination_path = destination
        .store_path()
        .context("conversation fork requires a durable store")?
        .to_path_buf();
    let destination_session_id = destination.session_scope_id().to_owned();
    anyhow::ensure!(
        destination_session_id != point.source_session_id,
        "conversation fork requires a distinct destination session"
    );
    let composition = conversation_fork_source_composition(records)?;
    let mut prefix = safe_prefix_for_complete_turn(records, &point)?;
    let destination_artifacts = destination.tool_artifact_store();
    remap_forked_tool_artifacts(
        source_artifacts,
        destination_artifacts.as_ref(),
        &point.source_session_id,
        &destination_session_id,
        &mut prefix.messages,
    )?;
    let mut entries = vec![SessionLogEntry::Control(ControlEntry::SessionIdentity {
        provider_name: destination.provider_name().to_owned(),
        model_name: destination.model_name().to_owned(),
        resolved_model_route: destination.resolved_model_route().cloned(),
    })];
    if let Some(composition) = composition {
        entries.push(SessionLogEntry::Control(
            ControlEntry::SessionCompositionBound(composition),
        ));
    }
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
    entries.extend(prefix.messages.iter().cloned());
    for provenance in prefix.provenance {
        entries.push(SessionLogEntry::Control(ControlEntry::ExternalProvenance(
            rebind_external_provenance(provenance, &destination_session_id)?,
        )));
    }
    let fork_event = destination.append_initial_conversation_fork(&payload, entries)?;

    Ok(ConversationForkOutput {
        destination_session_ref,
        destination_path,
        destination_session_id,
        fork_event,
        copied_message_count: prefix.messages.len(),
        copied_external_provenance_count: payload.copied_external_provenance_count,
    })
}

/// Validates the copied prefix against its exact source before recovering a fork receipt.
/// Later branch conversation remains outside this immutable creation prefix.
///
/// # Errors
/// Rejects missing, altered, or partially copied messages/provenance and mismatched lineage.
pub fn validate_conversation_fork_copy(
    source_records: &[SessionStreamRecord],
    destination_records: &[SessionStreamRecord],
    forked: &ConversationForked,
) -> Result<()> {
    let point = ConversationForkProjection::from_records(source_records)?
        .point(&forked.source_turn_digest)
        .cloned()
        .context("conversation fork source turn is no longer available")?;
    anyhow::ensure!(
        point.source_session_id == forked.source_session_id
            && point.source_turn_index == forked.source_turn_index
            && point.source_boundary_event_id == forked.source_boundary_event_id
            && point.source_boundary_stream_sequence == forked.source_boundary_stream_sequence
            && destination_records
                .iter()
                .all(|record| record.session_id() == forked.destination_session_id),
        "conversation fork lineage differs from its exact source binding"
    );
    let prefix = safe_prefix_for_complete_turn(source_records, &point)?;
    anyhow::ensure!(
        prefix.messages.len() == forked.copied_message_count
            && prefix.provenance.len() == forked.copied_external_provenance_count,
        "conversation fork copy counts differ from its source prefix"
    );
    let mut messages = Vec::new();
    let mut provenance = Vec::new();
    for record in destination_records {
        match session_entry(record)? {
            Some(SessionLogEntry::Control(ControlEntry::ExternalProvenance(entry))) => {
                provenance.push(entry)
            }
            Some(SessionLogEntry::Control(_)) | None => {}
            Some(entry) => messages.push(entry),
        }
    }
    anyhow::ensure!(
        messages.len() >= prefix.messages.len() && provenance.len() >= prefix.provenance.len(),
        "conversation fork destination contains an incomplete copied prefix"
    );
    for (mut expected, actual) in prefix.messages.into_iter().zip(messages) {
        if let (SessionLogEntry::ToolResultV3(expected), SessionLogEntry::ToolResultV3(actual)) =
            (&mut expected, &actual)
            && let (
                ToolArtifactBindingV1::Published { descriptor: source },
                ToolArtifactBindingV1::Published { descriptor: target },
            ) = (&expected.artifact, &actual.artifact)
        {
            // A copied artifact has a new opaque ref but the same complete content contract.
            let mut descriptor = source.clone();
            descriptor.artifact_ref = target.artifact_ref.clone();
            descriptor.session_scope_id_hash =
                stable_event_hash(forked.destination_session_id.as_bytes());
            descriptor.retention_class = crate::ToolArtifactRetentionClass::SessionBound;
            anyhow::ensure!(
                serde_json::to_value(&descriptor)? == serde_json::to_value(target)?,
                "conversation fork artifact binding differs from the source"
            );
            remap_tool_result_artifact(expected, descriptor)?;
        }
        anyhow::ensure!(
            serde_json::to_value(&expected)? == serde_json::to_value(&actual)?,
            "conversation fork copied message differs from the source prefix"
        );
    }
    for (expected, actual) in prefix.provenance.into_iter().zip(provenance) {
        let expected = rebind_external_provenance(expected, &forked.destination_session_id)?;
        anyhow::ensure!(
            serde_json::to_value(expected)? == serde_json::to_value(actual)?,
            "conversation fork copied provenance differs from the source prefix"
        );
    }
    Ok(())
}

/// Returns the source execution contract without inferring one for low-level unbound sources.
/// Product adapters require a bound source before invoking the generic transcript operation.
///
/// # Errors
///
/// Rejects invalid critical records, unsupported contracts, duplicate bindings, and bindings
/// recorded after model output or effect history.
pub fn conversation_fork_source_composition(
    records: &[SessionStreamRecord],
) -> Result<Option<SessionCompositionSnapshotV1>> {
    let mut composition = None;
    let mut has_execution_history = false;
    for record in records {
        match record.session_log_entry()? {
            Some(SessionLogEntry::Control(ControlEntry::SessionCompositionBound(snapshot))) => {
                snapshot.validate()?;
                if composition.is_some() {
                    bail!("conversation fork source has duplicate composition bindings");
                }
                if has_execution_history {
                    bail!("conversation fork source composition follows execution history");
                }
                composition = Some(snapshot);
            }
            Some(
                SessionLogEntry::Assistant(_)
                | SessionLogEntry::ToolResultV3(_)
                | SessionLogEntry::Control(
                    ControlEntry::ToolExecution(_)
                    | ControlEntry::TaskRun(_)
                    | ControlEntry::TerminalTask(_),
                ),
            ) => has_execution_history = true,
            _ => {}
        }
    }
    Ok(composition)
}

fn remap_forked_tool_artifacts(
    source_artifacts: Option<&ToolArtifactStore>,
    destination_artifacts: Option<&ToolArtifactStore>,
    source_session_id: &str,
    destination_session_id: &str,
    messages: &mut [SessionLogEntry],
) -> Result<()> {
    for entry in messages {
        let SessionLogEntry::ToolResultV3(result) = entry else {
            continue;
        };
        let ToolArtifactBindingV1::Published { descriptor } = &result.artifact else {
            continue;
        };
        let source = source_artifacts
            .context("conversation fork source artifact capability is unavailable")?;
        let destination = destination_artifacts
            .context("conversation fork destination artifact capability is unavailable")?;
        anyhow::ensure!(
            source.session_scope_id() == source_session_id
                && destination.session_scope_id() == destination_session_id,
            "conversation fork artifact capability belongs to another session"
        );
        let descriptor = destination.fork_descriptor_from(source, descriptor)?;
        remap_tool_result_artifact(result, descriptor)?;
    }
    Ok(())
}

fn remap_tool_result_artifact(
    result: &mut ToolResultRecordedV3,
    descriptor: ToolArtifactDescriptorV1,
) -> Result<()> {
    result.initial_model_view.artifact_ref = Some(descriptor.artifact_ref.clone());
    result.artifact = ToolArtifactBindingV1::Published { descriptor };
    result.initial_model_view_sha256 = stable_event_hash(result.model_content()?.as_bytes());
    result.validate()
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
                SessionLogEntry::RuntimeContextSnapshotV2(snapshot) => {
                    message_ids.insert(snapshot.message.id.clone());
                    messages.push(SessionLogEntry::RuntimeContextSnapshotV2(snapshot));
                }
                SessionLogEntry::ToolResultV3(result) => {
                    message_ids.insert(result.message_id.clone());
                    messages.push(SessionLogEntry::ToolResultV3(result));
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
    crate::conversation_transcript_entry_from_record(record)
        .context("failed to decode conversation fork session entry")
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

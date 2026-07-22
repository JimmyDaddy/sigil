use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::{
    DomainEvent, MutationArtifactId, MutationArtifactLifecycleRecorded,
    MutationArtifactLifecycleStatus, MutationCommitted, MutationPrepared, MutationSubject,
    SessionLogEntry, SessionStreamRecord, SnapshotCoverage, WorkspaceId, stable_event_hash,
    stable_event_uuid,
};

/// Whether one controlled checkpoint file can be restored from durable evidence.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ControlledCheckpointFileAvailability {
    Restorable,
    Sensitive,
    Unsupported,
    Unavailable,
}

/// The filesystem effect required to return one file to its pre-turn state.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ControlledCheckpointRestoreKind {
    RestoreContent,
    RemoveCreatedFile,
}

/// Exact UI-to-runtime binding for one controlled checkpoint operation.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct ControlledCheckpointRestoreRequest {
    pub checkpoint_id: String,
    pub checkpoint_digest: String,
}

/// Read-only preflight state for one file in a checkpoint restore preview.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct ControlledCheckpointRestorePreviewFile {
    pub path: PathBuf,
    pub restore_kind: ControlledCheckpointRestoreKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_current_hash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actual_current_hash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conflict_reason: Option<crate::CheckpointRestoreConflictReason>,
}

/// Read-only exact restore preview. `ready` is true only when every controlled file can restore.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct ControlledCheckpointRestorePreview {
    pub checkpoint_id: String,
    pub checkpoint_digest: String,
    pub files: Vec<ControlledCheckpointRestorePreviewFile>,
    pub unknown_mutation_count: usize,
    pub ready: bool,
}

/// Durable result of one exact controlled checkpoint batch restore.
#[derive(Debug, Clone, PartialEq)]
pub struct ControlledCheckpointRestoreOutput {
    pub preview: ControlledCheckpointRestorePreview,
    pub batch_id: String,
    pub restored: Vec<crate::RestoredFileMutation>,
}

/// Exact durable binding for one ordinary file in a controlled checkpoint.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct ControlledCheckpointFile {
    pub path: PathBuf,
    pub workspace_id: WorkspaceId,
    pub first_operation_id: String,
    pub latest_operation_id: String,
    pub prepared_event_id: String,
    pub prepared_stream_sequence: u64,
    pub committed_event_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub before_hash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_current_hash: Option<String>,
    pub snapshot_coverage: SnapshotCoverage,
    pub restore_kind: ControlledCheckpointRestoreKind,
    pub availability: ControlledCheckpointFileAvailability,
}

impl ControlledCheckpointFile {
    #[must_use]
    pub fn is_restorable(&self) -> bool {
        self.availability == ControlledCheckpointFileAvailability::Restorable
    }
}

/// One user-turn checkpoint derived from committed controlled file mutations.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct ControlledCheckpoint {
    pub checkpoint_id: String,
    pub checkpoint_digest: String,
    pub source_session_id: String,
    pub turn_index: usize,
    pub turn_boundary_event_id: String,
    pub turn_boundary_stream_sequence: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt: Option<String>,
    pub files: Vec<ControlledCheckpointFile>,
    pub unknown_mutation_count: usize,
}

impl ControlledCheckpoint {
    #[must_use]
    pub fn is_fully_restorable(&self) -> bool {
        !self.files.is_empty()
            && self
                .files
                .iter()
                .all(ControlledCheckpointFile::is_restorable)
    }
}

/// Rebuildable checkpoint view over one session's v2 durable stream.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ControlledCheckpointProjection {
    pub checkpoints: Vec<ControlledCheckpoint>,
}

impl ControlledCheckpointProjection {
    /// Replays a v2 durable stream into user-turn controlled checkpoints.
    ///
    /// # Errors
    ///
    /// Returns an error when a known recovery-critical mutation payload cannot be decoded or a
    /// checkpoint digest cannot be serialized deterministically.
    pub fn from_records(records: &[SessionStreamRecord]) -> Result<Self> {
        crate::ConversationQueueDurableProjection::from_records(records)?;
        let unavailable_artifacts = unavailable_artifacts(records)?;
        let restored_operation_ids = restored_operation_ids(records)?;
        let mut checkpoints = Vec::new();
        let mut current = None::<CheckpointBuilder>;
        let mut turn_index = 0usize;

        for record in records {
            if let Some(SessionLogEntry::User(message)) = session_entry(record)? {
                finalize_checkpoint(&mut checkpoints, current.take(), &unavailable_artifacts)?;
                turn_index = turn_index.saturating_add(1);
                current = Some(CheckpointBuilder::new(
                    record.session_id().to_owned(),
                    turn_index,
                    record.event_id().to_owned(),
                    record.stream_sequence(),
                    message.content,
                ));
            }

            let Some(builder) = current.as_mut() else {
                continue;
            };
            let Some(domain_record) = record.domain_event_record()? else {
                continue;
            };
            match domain_record.event {
                DomainEvent::MutationPrepared(payload) => {
                    let prepared = serde_json::from_value::<MutationPrepared>(payload.payload)
                        .context("failed to decode checkpoint mutation prepared payload")?;
                    if restored_operation_ids.contains(&prepared.operation_id) {
                        continue;
                    }
                    builder.prepared.insert(
                        prepared.operation_id.clone(),
                        PreparedBinding {
                            prepared,
                            event_id: record.event_id().to_owned(),
                            stream_sequence: record.stream_sequence(),
                        },
                    );
                }
                DomainEvent::MutationCommitted(payload) => {
                    let committed = serde_json::from_value::<MutationCommitted>(payload.payload)
                        .context("failed to decode checkpoint mutation committed payload")?;
                    if restored_operation_ids.contains(&committed.operation_id) {
                        continue;
                    }
                    builder.apply_committed(committed, record.event_id());
                }
                DomainEvent::WorkspaceMutationDetected(payload) => {
                    let detected =
                        serde_json::from_value::<crate::WorkspaceMutationDetected>(payload.payload)
                            .context("failed to decode checkpoint workspace mutation payload")?;
                    if detected.unknown_dirty {
                        builder.unknown_mutation_count =
                            builder.unknown_mutation_count.saturating_add(1);
                    }
                }
                _ => {}
            }
        }
        finalize_checkpoint(&mut checkpoints, current, &unavailable_artifacts)?;
        Ok(Self { checkpoints })
    }

    #[must_use]
    pub fn latest(&self) -> Option<&ControlledCheckpoint> {
        self.checkpoints.last()
    }

    #[must_use]
    pub fn checkpoint(&self, checkpoint_id: &str) -> Option<&ControlledCheckpoint> {
        self.checkpoints
            .iter()
            .find(|checkpoint| checkpoint.checkpoint_id == checkpoint_id)
    }
}

#[derive(Debug)]
struct PreparedBinding {
    prepared: MutationPrepared,
    event_id: String,
    stream_sequence: u64,
}

#[derive(Debug)]
struct CheckpointBuilder {
    source_session_id: String,
    turn_index: usize,
    turn_boundary_event_id: String,
    turn_boundary_stream_sequence: u64,
    prompt: Option<String>,
    prepared: BTreeMap<String, PreparedBinding>,
    files: BTreeMap<PathBuf, ControlledCheckpointFile>,
    unknown_mutation_count: usize,
}

impl CheckpointBuilder {
    fn new(
        source_session_id: String,
        turn_index: usize,
        turn_boundary_event_id: String,
        turn_boundary_stream_sequence: u64,
        prompt: Option<String>,
    ) -> Self {
        Self {
            source_session_id,
            turn_index,
            turn_boundary_event_id,
            turn_boundary_stream_sequence,
            prompt,
            prepared: BTreeMap::new(),
            files: BTreeMap::new(),
            unknown_mutation_count: 0,
        }
    }

    fn apply_committed(&mut self, committed: MutationCommitted, committed_event_id: &str) {
        let Some(prepared) = self.prepared.get(&committed.operation_id) else {
            return;
        };
        let MutationSubject::File {
            path: prepared_path,
            ..
        } = &prepared.prepared.subject
        else {
            return;
        };
        let MutationSubject::File {
            path: committed_path,
            ..
        } = &committed.committed_subject
        else {
            return;
        };
        if prepared_path != committed_path
            || committed
                .workspace_id
                .as_deref()
                .is_some_and(|workspace_id| workspace_id != prepared.prepared.workspace_id)
        {
            return;
        }

        if let Some(file) = self.files.get_mut(prepared_path) {
            file.latest_operation_id = committed.operation_id;
            file.committed_event_id = committed_event_id.to_owned();
            file.expected_current_hash = committed.observed_after_hash;
            return;
        }
        let restore_kind = match prepared.prepared.snapshot_coverage {
            SnapshotCoverage::NoPriorContent => ControlledCheckpointRestoreKind::RemoveCreatedFile,
            _ => ControlledCheckpointRestoreKind::RestoreContent,
        };
        self.files.insert(
            prepared_path.clone(),
            ControlledCheckpointFile {
                path: prepared_path.clone(),
                workspace_id: prepared.prepared.workspace_id.clone(),
                first_operation_id: prepared.prepared.operation_id.clone(),
                latest_operation_id: committed.operation_id,
                prepared_event_id: prepared.event_id.clone(),
                prepared_stream_sequence: prepared.stream_sequence,
                committed_event_id: committed_event_id.to_owned(),
                before_hash: prepared.prepared.before_hash.clone(),
                expected_current_hash: committed.observed_after_hash,
                snapshot_coverage: prepared.prepared.snapshot_coverage.clone(),
                restore_kind,
                availability: coverage_base_availability(&prepared.prepared.snapshot_coverage),
            },
        );
    }
}

fn finalize_checkpoint(
    checkpoints: &mut Vec<ControlledCheckpoint>,
    builder: Option<CheckpointBuilder>,
    unavailable_artifacts: &BTreeMap<MutationArtifactId, u64>,
) -> Result<()> {
    let Some(builder) = builder else {
        return Ok(());
    };
    if builder.files.is_empty() && builder.unknown_mutation_count == 0 {
        return Ok(());
    }
    let mut files = builder.files.into_values().collect::<Vec<_>>();
    for file in &mut files {
        file.availability = coverage_availability(
            &file.snapshot_coverage,
            file.prepared_stream_sequence,
            unavailable_artifacts,
        );
        if file.restore_kind == ControlledCheckpointRestoreKind::RemoveCreatedFile
            && file.expected_current_hash.is_none()
        {
            file.availability = ControlledCheckpointFileAvailability::Unavailable;
        }
    }
    let checkpoint_id = format!(
        "checkpoint:{}",
        stable_event_uuid(
            "sigil-controlled-checkpoint",
            &format!(
                "{}:{}:{}",
                builder.source_session_id, builder.turn_index, builder.turn_boundary_event_id
            ),
        )
    );
    let digest_payload = serde_json::to_vec(&(
        &checkpoint_id,
        &builder.source_session_id,
        builder.turn_index,
        &files,
        builder.unknown_mutation_count,
    ))
    .context("failed to encode controlled checkpoint digest")?;
    checkpoints.push(ControlledCheckpoint {
        checkpoint_id,
        checkpoint_digest: stable_event_hash(digest_payload),
        source_session_id: builder.source_session_id,
        turn_index: builder.turn_index,
        turn_boundary_event_id: builder.turn_boundary_event_id,
        turn_boundary_stream_sequence: builder.turn_boundary_stream_sequence,
        prompt: builder.prompt,
        files,
        unknown_mutation_count: builder.unknown_mutation_count,
    });
    Ok(())
}

fn coverage_availability(
    coverage: &SnapshotCoverage,
    prepared_stream_sequence: u64,
    unavailable_artifacts: &BTreeMap<MutationArtifactId, u64>,
) -> ControlledCheckpointFileAvailability {
    match coverage {
        SnapshotCoverage::Captured(artifact_id) => {
            if unavailable_artifacts
                .get(artifact_id)
                .is_some_and(|sequence| *sequence > prepared_stream_sequence)
            {
                ControlledCheckpointFileAvailability::Unavailable
            } else {
                ControlledCheckpointFileAvailability::Restorable
            }
        }
        SnapshotCoverage::NoPriorContent => ControlledCheckpointFileAvailability::Restorable,
        SnapshotCoverage::SkippedSensitive => ControlledCheckpointFileAvailability::Sensitive,
        SnapshotCoverage::Unsupported => ControlledCheckpointFileAvailability::Unsupported,
        SnapshotCoverage::Unavailable => ControlledCheckpointFileAvailability::Unavailable,
    }
}

fn coverage_base_availability(coverage: &SnapshotCoverage) -> ControlledCheckpointFileAvailability {
    match coverage {
        SnapshotCoverage::Captured(_) | SnapshotCoverage::NoPriorContent => {
            ControlledCheckpointFileAvailability::Restorable
        }
        SnapshotCoverage::SkippedSensitive => ControlledCheckpointFileAvailability::Sensitive,
        SnapshotCoverage::Unsupported => ControlledCheckpointFileAvailability::Unsupported,
        SnapshotCoverage::Unavailable => ControlledCheckpointFileAvailability::Unavailable,
    }
}

fn unavailable_artifacts(
    records: &[SessionStreamRecord],
) -> Result<BTreeMap<MutationArtifactId, u64>> {
    let mut unavailable = BTreeMap::new();
    for record in records {
        let Some(domain_record) = record.domain_event_record()? else {
            continue;
        };
        let DomainEvent::MutationArtifactLifecycleRecorded(payload) = domain_record.event else {
            continue;
        };
        let lifecycle =
            serde_json::from_value::<MutationArtifactLifecycleRecorded>(payload.payload)
                .context("failed to decode mutation artifact lifecycle payload")?;
        if matches!(
            lifecycle.status,
            MutationArtifactLifecycleStatus::Deleted
                | MutationArtifactLifecycleStatus::Expired
                | MutationArtifactLifecycleStatus::Unavailable
        ) {
            unavailable.insert(lifecycle.artifact_id, record.stream_sequence());
        }
    }
    Ok(unavailable)
}

fn restored_operation_ids(records: &[SessionStreamRecord]) -> Result<BTreeSet<String>> {
    let mut restored = BTreeSet::new();
    for record in records {
        let Some(domain_record) = record.domain_event_record()? else {
            continue;
        };
        let DomainEvent::CheckpointRestored(payload) = domain_record.event else {
            continue;
        };
        let checkpoint = serde_json::from_value::<crate::CheckpointRestored>(payload.payload)
            .context("failed to decode checkpoint restored payload")?;
        restored.insert(checkpoint.operation_id);
    }
    Ok(restored)
}

fn session_entry(record: &SessionStreamRecord) -> Result<Option<SessionLogEntry>> {
    crate::conversation_transcript_entry_from_record(record)
        .context("failed to decode checkpoint session entry")
}

#[cfg(test)]
#[path = "tests/checkpoint_tests.rs"]
mod tests;

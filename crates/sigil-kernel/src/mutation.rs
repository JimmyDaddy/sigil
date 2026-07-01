//! RFC-0002 crash-consistent mutation protocol foundation.
//!
//! This module covers controlled file mutations and the first durable evidence path for
//! workspace mutations caused by unknown-effect executions. Full shell, MCP, plugin and
//! persistent terminal lifecycle coverage remains staged by RFC-0002.

use std::{
    collections::{BTreeMap, BTreeSet},
    env,
    ffi::OsString,
    fs::{self, File},
    io::Write,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, anyhow, bail};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};

use crate::{
    DurableEventType, EventClass, EventId, JsonlSessionStore, StoredEvent, stable_event_uuid,
    verification::{
        DEFAULT_TASK_VERIFICATION_SCOPE_HASH, FileType, SnapshotEntryState, ToolEffect,
        VerificationScope, VerificationScopeHash, WorkspaceId, WorkspaceKnowledge,
        WorkspaceRevision, WorkspaceSnapshotBuild, WorkspaceSnapshotEntry, WorkspaceSnapshotId,
        WorkspaceSnapshotManifestV1, build_workspace_snapshot, stable_workspace_id,
    },
};

pub type MutationBatchId = String;
pub type OperationId = String;
pub type ToolCallId = String;
pub type MutationArtifactId = String;

/// Snapshot coverage captured before a controlled mutation.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SnapshotCoverage {
    Captured(MutationArtifactId),
    NoPriorContent,
    SkippedSensitive,
    Unsupported,
    Unavailable,
}

/// Subject touched by one mutation operation.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MutationSubject {
    File { path: PathBuf, file_type: FileType },
    Directory { path: PathBuf },
    Workspace { scope_hash: String },
    External { description: String },
    Unknown,
}

/// Sync criticality for one mutation.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MutationSyncClass {
    RecoveryCritical,
}

/// Durable prepare event payload.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct MutationPrepared {
    pub operation_id: OperationId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub batch_id: Option<MutationBatchId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<ToolCallId>,
    pub causation_event_id: EventId,
    pub subject: MutationSubject,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub before_hash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub intended_after_hash: Option<String>,
    pub snapshot_coverage: SnapshotCoverage,
    pub workspace_id: WorkspaceId,
    pub base_workspace_revision: WorkspaceRevision,
    pub sync_class: MutationSyncClass,
}

/// Durable commit event payload.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct MutationCommitted {
    pub operation_id: OperationId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub batch_id: Option<MutationBatchId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<WorkspaceId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observed_after_hash: Option<String>,
    pub workspace_revision: WorkspaceRevision,
    pub workspace_snapshot_id: WorkspaceSnapshotId,
    pub committed_subject: MutationSubject,
}

/// Durable reconciliation event payload.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct MutationReconciled {
    pub operation_id: OperationId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub batch_id: Option<MutationBatchId>,
    pub observed_state: MutationObservedState,
    pub resolution: MutationResolution,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_revision: Option<WorkspaceRevision>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_snapshot_id: Option<WorkspaceSnapshotId>,
}

/// Workspace snapshot captured before or after a tool with unknown side effects.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct WorkspaceMutationScan {
    pub workspace_id: WorkspaceId,
    pub scope_hash: VerificationScopeHash,
    pub scope: VerificationScope,
    pub workspace_revision: WorkspaceRevision,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_snapshot_id: Option<WorkspaceSnapshotId>,
    pub workspace_knowledge: WorkspaceKnowledge,
}

/// Why an unknown-effect execution produced workspace mutation evidence.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WorkspaceMutationDetectionReason {
    SnapshotChanged,
    SnapshotIncompleteBefore,
    SnapshotIncompleteAfter,
    DeclaredWriteEffect,
    ScanUnavailable,
}

/// Durable event payload for workspace mutations detected outside controlled file tools.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct WorkspaceMutationDetected {
    pub operation_id: OperationId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<ToolCallId>,
    pub tool_name: String,
    pub tool_effect: ToolEffect,
    pub workspace_id: WorkspaceId,
    pub scope_hash: VerificationScopeHash,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from_workspace_snapshot_id: Option<WorkspaceSnapshotId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to_workspace_snapshot_id: Option<WorkspaceSnapshotId>,
    pub base_workspace_revision: WorkspaceRevision,
    pub workspace_revision: WorkspaceRevision,
    pub reason: WorkspaceMutationDetectionReason,
    pub unknown_dirty: bool,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub metadata: BTreeMap<String, String>,
}

/// Pre-execution mutation profile persisted with write-capable tool execution starts.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct ExecutionMutationProfile {
    pub tool_call_id: ToolCallId,
    pub tool_name: String,
    pub effect: ToolEffect,
    pub workspace_id: WorkspaceId,
    pub scan_scope_hash: VerificationScopeHash,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pre_execution_snapshot_id: Option<WorkspaceSnapshotId>,
    pub pre_execution_workspace_revision: WorkspaceRevision,
    pub workspace_knowledge: WorkspaceKnowledge,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MutationObservedState {
    NotApplied,
    AppliedAsIntended,
    AppliedDifferently,
    Unknown,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MutationResolution {
    MarkNotApplied,
    MarkCommitted,
    MarkConflict,
    MarkUnknownDirty,
}

/// Batch lifecycle status for multi-file changesets.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MutationBatchStatus {
    Applied,
    PartiallyApplied,
    Failed,
}

/// One prepared controlled file mutation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreparedFileMutation {
    pub prepared_event_id: EventId,
    pub prepared_stream_sequence: u64,
    pub operation_id: OperationId,
    pub batch_id: Option<MutationBatchId>,
    pub tool_call_id: Option<ToolCallId>,
    pub workspace_id: WorkspaceId,
    pub workspace_root: PathBuf,
    pub relative_path: PathBuf,
    pub absolute_path: PathBuf,
    pub before_hash: Option<String>,
    pub intended_after_hash: Option<String>,
    pub base_workspace_revision: WorkspaceRevision,
}

/// One prepared controlled directory mutation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreparedDirectoryMutation {
    pub prepared_event_id: EventId,
    pub prepared_stream_sequence: u64,
    pub operation_id: OperationId,
    pub batch_id: Option<MutationBatchId>,
    pub tool_call_id: Option<ToolCallId>,
    pub workspace_id: WorkspaceId,
    pub workspace_root: PathBuf,
    pub relative_path: PathBuf,
    pub absolute_path: PathBuf,
    pub before_hash: Option<String>,
    pub intended_after_hash: Option<String>,
    pub base_workspace_revision: WorkspaceRevision,
}

/// Result of one committed file mutation.
#[derive(Debug, Clone, PartialEq)]
pub struct CommittedFileMutation {
    pub committed_event: StoredEvent,
    pub write_event: StoredEvent,
    pub operation_id: OperationId,
    pub batch_id: Option<MutationBatchId>,
    pub workspace_revision: WorkspaceRevision,
    pub workspace_snapshot_id: WorkspaceSnapshotId,
    pub observed_after_hash: Option<String>,
}

/// Result of one committed directory mutation.
#[derive(Debug, Clone, PartialEq)]
pub struct CommittedDirectoryMutation {
    pub committed_event: StoredEvent,
    pub operation_id: OperationId,
    pub batch_id: Option<MutationBatchId>,
    pub workspace_revision: WorkspaceRevision,
    pub workspace_snapshot_id: WorkspaceSnapshotId,
    pub observed_after_hash: Option<String>,
}

/// Durable payload recorded after a checkpoint restore commits a workspace mutation.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct CheckpointRestored {
    pub operation_id: OperationId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub batch_id: Option<MutationBatchId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<ToolCallId>,
    pub restored_subject: MutationSubject,
    pub restored_from: SnapshotCoverage,
    pub mutation_committed_event_id: EventId,
    pub workspace_revision: WorkspaceRevision,
    pub workspace_snapshot_id: WorkspaceSnapshotId,
}

/// Result of one checkpoint restore mutation.
#[derive(Debug, Clone, PartialEq)]
pub struct RestoredFileMutation {
    pub committed: CommittedFileMutation,
    pub checkpoint_event: StoredEvent,
    pub restored_from: SnapshotCoverage,
}

/// Lifecycle status for mutation artifact content.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MutationArtifactLifecycleStatus {
    Deleted,
    Expired,
    Unavailable,
}

/// Durable payload recorded when mutation artifact content is removed or becomes unavailable.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct MutationArtifactLifecycleRecorded {
    pub artifact_id: MutationArtifactId,
    pub status: MutationArtifactLifecycleStatus,
    pub reason: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_hash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub operation_ids: Vec<OperationId>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub source_paths: Vec<PathBuf>,
}

/// Durable payload recorded when a user or maintenance flow explicitly starts artifact cleanup.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct MutationArtifactCleanupRequested {
    pub target: MutationArtifactCleanupTarget,
    pub policy: MutationArtifactRetentionPolicy,
    pub scanned_artifacts: usize,
    pub scanned_bytes: u64,
    pub candidate_artifacts: usize,
    pub candidate_bytes: u64,
}

/// Retention and quota limits for mutation artifacts.
///
/// The scanner only removes artifact content through audited lifecycle events. It does not rewrite
/// historical mutation events that reference the artifact id.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct MutationArtifactRetentionPolicy {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_artifacts: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_bytes: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expire_older_than_ms: Option<u64>,
}

/// Coarse cleanup target selected by product surfaces.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", tag = "target", content = "workspace_id")]
pub enum MutationArtifactCleanupTarget {
    /// Use the configured retention policy: expired/quota-selected artifacts and unavailable blobs.
    Recommended,
    /// Clean only artifacts selected by age/count/byte retention limits.
    Expired,
    /// Clean only artifacts whose metadata exists but blob content is missing or corrupt.
    Unavailable,
    /// Clean artifact blobs that are not referenced by the current session event stream.
    Unreferenced,
    /// Clean all artifact blobs captured for the provided workspace id.
    Workspace(WorkspaceId),
}

/// Summary produced by one mutation artifact retention scan.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct MutationArtifactRetentionReport {
    pub scanned_artifacts: usize,
    pub scanned_bytes: u64,
    pub expired_artifacts: usize,
    pub expired_bytes: u64,
    pub deleted_artifacts: usize,
    pub deleted_bytes: u64,
    pub unavailable_artifacts: usize,
    pub lifecycle_events: Vec<StoredEvent>,
}

impl MutationArtifactRetentionReport {
    /// Number of artifacts selected by the recommended cleanup preview.
    #[must_use]
    pub fn cleanup_candidate_artifacts(&self) -> usize {
        self.expired_artifacts
            .saturating_add(self.deleted_artifacts)
            .saturating_add(self.unavailable_artifacts)
    }

    /// Bytes selected by the recommended cleanup preview.
    #[must_use]
    pub fn cleanup_candidate_bytes(&self) -> u64 {
        self.expired_bytes.saturating_add(self.deleted_bytes)
    }

    /// Whether a product surface should show a cleanup recommendation.
    #[must_use]
    pub fn has_cleanup_candidates(&self) -> bool {
        self.cleanup_candidate_artifacts() > 0
    }
}

/// Read-only metadata for mutation artifact inventory views.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MutationArtifactInventoryItem {
    pub artifact_id: MutationArtifactId,
    pub size: u64,
    pub created_at_ms: Option<u64>,
    pub blob_available: bool,
    pub operation_ids: Vec<OperationId>,
    pub source_paths: Vec<PathBuf>,
}

#[derive(Debug)]
struct MutationArtifactRetentionSelection {
    groups: Vec<MutationArtifactGroup>,
    selected: Vec<MutationArtifactCleanupSelection>,
}

#[derive(Debug, Clone)]
struct MutationArtifactCleanupSelection {
    artifact_id: MutationArtifactId,
    requested_status: MutationArtifactLifecycleStatus,
    reason: &'static str,
}

/// Store-backed durable mutation recorder.
#[derive(Debug, Clone)]
pub struct MutationEventRecorder {
    store: JsonlSessionStore,
    artifact_root: PathBuf,
}

impl MutationEventRecorder {
    #[must_use]
    pub fn new(store: JsonlSessionStore) -> Self {
        let artifact_root = default_mutation_artifact_root(store.path());
        Self {
            store,
            artifact_root,
        }
    }

    /// Creates a recorder with an explicit mutation artifact root.
    ///
    /// This is primarily used by tests and entrypoints that already resolved the user state
    /// directory. The root must not point inside the workspace repository.
    #[must_use]
    pub fn with_artifact_root(store: JsonlSessionStore, artifact_root: impl Into<PathBuf>) -> Self {
        Self {
            store,
            artifact_root: artifact_root.into(),
        }
    }

    pub fn coordinator(
        &self,
        workspace_root: impl AsRef<Path>,
        tool_call_id: impl Into<ToolCallId>,
        batch_id: Option<MutationBatchId>,
    ) -> Result<MutationCoordinator> {
        let workspace_root = workspace_root.as_ref();
        let workspace_id = stable_workspace_id(workspace_root)?;
        let canonical_root = fs::canonicalize(workspace_root)
            .with_context(|| format!("failed to canonicalize {}", workspace_root.display()))?;
        Ok(MutationCoordinator {
            recorder: self.clone(),
            workspace_root: canonical_root,
            workspace_id,
            tool_call_id: tool_call_id.into(),
            batch_id,
        })
    }

    pub fn append_prepared(&self, payload: &MutationPrepared) -> Result<StoredEvent> {
        self.store.append_event(
            DurableEventType::MutationPrepared,
            EventClass::Critical,
            serde_json::to_value(payload).context("failed to encode mutation prepared payload")?,
        )
    }

    pub fn append_committed(&self, payload: &MutationCommitted) -> Result<StoredEvent> {
        self.store.append_event(
            DurableEventType::MutationCommitted,
            EventClass::Critical,
            serde_json::to_value(payload).context("failed to encode mutation committed payload")?,
        )
    }

    pub fn append_reconciled(&self, payload: &MutationReconciled) -> Result<StoredEvent> {
        self.store.append_event(
            DurableEventType::MutationReconciled,
            EventClass::Critical,
            serde_json::to_value(payload)
                .context("failed to encode mutation reconciled payload")?,
        )
    }

    pub fn append_workspace_mutation_detected(
        &self,
        payload: &WorkspaceMutationDetected,
    ) -> Result<StoredEvent> {
        self.store.append_event(
            DurableEventType::WorkspaceMutationDetected,
            EventClass::Critical,
            serde_json::to_value(payload)
                .context("failed to encode workspace mutation detected payload")?,
        )
    }

    pub fn append_write_committed(
        &self,
        committed: &CommittedFileMutation,
        committed_event_id: &str,
    ) -> Result<StoredEvent> {
        self.store.append_event(
            DurableEventType::WriteCommitted,
            EventClass::Critical,
            json!({
                "operation_id": committed.operation_id,
                "batch_id": committed.batch_id,
                "mutation_committed_event_id": committed_event_id,
                "workspace_revision": committed.workspace_revision,
                "workspace_snapshot_id": committed.workspace_snapshot_id,
                "observed_after_hash": committed.observed_after_hash,
            }),
        )
    }

    pub fn append_checkpoint_restored(&self, payload: &CheckpointRestored) -> Result<StoredEvent> {
        self.store.append_event(
            DurableEventType::CheckpointRestored,
            EventClass::Critical,
            serde_json::to_value(payload)
                .context("failed to encode checkpoint restored payload")?,
        )
    }

    pub fn append_artifact_lifecycle_recorded(
        &self,
        payload: &MutationArtifactLifecycleRecorded,
    ) -> Result<StoredEvent> {
        self.store.append_event(
            DurableEventType::MutationArtifactLifecycleRecorded,
            EventClass::Critical,
            serde_json::to_value(payload)
                .context("failed to encode mutation artifact lifecycle payload")?,
        )
    }

    pub fn append_artifact_cleanup_requested(
        &self,
        payload: &MutationArtifactCleanupRequested,
    ) -> Result<StoredEvent> {
        self.store.append_event(
            DurableEventType::MutationArtifactCleanupRequested,
            EventClass::Critical,
            serde_json::to_value(payload)
                .context("failed to encode mutation artifact cleanup request payload")?,
        )
    }

    /// Deletes mutation artifact content because the user explicitly requested cleanup.
    ///
    /// The session history remains append-only: cleanup appends a lifecycle event instead of
    /// rewriting historical mutation events that referenced the artifact.
    pub fn delete_mutation_artifact(
        &self,
        artifact_id: impl Into<MutationArtifactId>,
        reason: impl Into<String>,
    ) -> Result<StoredEvent> {
        self.remove_mutation_artifact(
            artifact_id.into(),
            MutationArtifactLifecycleStatus::Deleted,
            reason.into(),
        )
    }

    /// Expires mutation artifact content due to retention or quota policy.
    ///
    /// Callers may invoke this directly for explicit maintenance, or through
    /// `enforce_artifact_retention` for policy-driven cleanup.
    pub fn expire_mutation_artifact(
        &self,
        artifact_id: impl Into<MutationArtifactId>,
        reason: impl Into<String>,
    ) -> Result<StoredEvent> {
        self.remove_mutation_artifact(
            artifact_id.into(),
            MutationArtifactLifecycleStatus::Expired,
            reason.into(),
        )
    }

    /// Applies artifact retention and quota policy to the recorder artifact root.
    ///
    /// Missing or corrupt artifact content is treated as unavailable and emits a lifecycle event.
    /// Age and quota expiration emit `Expired` lifecycle events. The session log remains
    /// append-only; historical mutation evidence is not rewritten.
    pub fn enforce_artifact_retention(
        &self,
        policy: &MutationArtifactRetentionPolicy,
    ) -> Result<MutationArtifactRetentionReport> {
        self.enforce_artifact_cleanup_at(
            &MutationArtifactCleanupTarget::Recommended,
            policy,
            unix_time_ms(),
        )
    }

    /// Previews artifact retention and quota impact without removing content or appending events.
    pub fn preview_artifact_retention(
        &self,
        policy: &MutationArtifactRetentionPolicy,
    ) -> Result<MutationArtifactRetentionReport> {
        self.preview_artifact_cleanup_at(
            &MutationArtifactCleanupTarget::Recommended,
            policy,
            unix_time_ms(),
        )
    }

    /// Previews artifact retention using an explicit clock value.
    ///
    /// This is a read-only scan: missing or corrupt content is counted as unavailable, but no
    /// lifecycle event is appended and artifact content remains untouched.
    pub fn preview_artifact_retention_at(
        &self,
        policy: &MutationArtifactRetentionPolicy,
        now_ms: u64,
    ) -> Result<MutationArtifactRetentionReport> {
        self.preview_artifact_cleanup_at(
            &MutationArtifactCleanupTarget::Recommended,
            policy,
            now_ms,
        )
    }

    /// Previews a coarse artifact cleanup target without removing content or appending events.
    pub fn preview_artifact_cleanup(
        &self,
        target: &MutationArtifactCleanupTarget,
        policy: &MutationArtifactRetentionPolicy,
    ) -> Result<MutationArtifactRetentionReport> {
        self.preview_artifact_cleanup_at(target, policy, unix_time_ms())
    }

    /// Previews a coarse artifact cleanup target using an explicit clock value.
    pub fn preview_artifact_cleanup_at(
        &self,
        target: &MutationArtifactCleanupTarget,
        policy: &MutationArtifactRetentionPolicy,
        now_ms: u64,
    ) -> Result<MutationArtifactRetentionReport> {
        let selection = select_artifacts_for_cleanup(
            &self.artifact_root,
            self.store.path(),
            target,
            policy,
            now_ms,
        )?;
        Ok(retention_report_from_selection(&selection))
    }

    /// Lists mutation artifact metadata without reading artifact content or modifying storage.
    pub fn list_mutation_artifacts(&self) -> Result<Vec<MutationArtifactInventoryItem>> {
        let mut groups = scan_mutation_artifact_groups(&self.artifact_root)?;
        groups.sort_by(|left, right| {
            left.created_at_ms
                .cmp(&right.created_at_ms)
                .then_with(|| left.artifact_id.cmp(&right.artifact_id))
        });
        Ok(groups
            .into_iter()
            .map(|artifact| MutationArtifactInventoryItem {
                artifact_id: artifact.artifact_id,
                size: artifact.size,
                created_at_ms: artifact.created_at_ms,
                blob_available: artifact.blob_available,
                operation_ids: artifact.operation_ids,
                source_paths: artifact.source_paths,
            })
            .collect())
    }

    /// Applies artifact retention using an explicit clock value.
    ///
    /// This is primarily useful for deterministic tests and offline maintenance jobs.
    pub fn enforce_artifact_retention_at(
        &self,
        policy: &MutationArtifactRetentionPolicy,
        now_ms: u64,
    ) -> Result<MutationArtifactRetentionReport> {
        self.enforce_artifact_cleanup_at(
            &MutationArtifactCleanupTarget::Recommended,
            policy,
            now_ms,
        )
    }

    /// Applies a coarse artifact cleanup target.
    ///
    /// Cleanup appends lifecycle records for every removed artifact. It never rewrites historical
    /// mutation events that may still reference cleaned artifact ids.
    pub fn enforce_artifact_cleanup(
        &self,
        target: &MutationArtifactCleanupTarget,
        policy: &MutationArtifactRetentionPolicy,
    ) -> Result<MutationArtifactRetentionReport> {
        self.enforce_artifact_cleanup_at(target, policy, unix_time_ms())
    }

    /// Applies a coarse artifact cleanup target using an explicit clock value.
    pub fn enforce_artifact_cleanup_at(
        &self,
        target: &MutationArtifactCleanupTarget,
        policy: &MutationArtifactRetentionPolicy,
        now_ms: u64,
    ) -> Result<MutationArtifactRetentionReport> {
        let selection = select_artifacts_for_cleanup(
            &self.artifact_root,
            self.store.path(),
            target,
            policy,
            now_ms,
        )?;
        let preview_report = retention_report_from_selection(&selection);
        self.append_artifact_cleanup_requested(&MutationArtifactCleanupRequested {
            target: target.clone(),
            policy: policy.clone(),
            scanned_artifacts: preview_report.scanned_artifacts,
            scanned_bytes: preview_report.scanned_bytes,
            candidate_artifacts: preview_report.cleanup_candidate_artifacts(),
            candidate_bytes: preview_report.cleanup_candidate_bytes(),
        })?;
        let artifact_sizes = selection
            .groups
            .iter()
            .map(|artifact| (artifact.artifact_id.clone(), artifact.size))
            .collect::<BTreeMap<_, _>>();
        let mut report = MutationArtifactRetentionReport {
            scanned_artifacts: selection.groups.len(),
            scanned_bytes: selection
                .groups
                .iter()
                .fold(0_u64, |total, artifact| total.saturating_add(artifact.size)),
            ..MutationArtifactRetentionReport::default()
        };
        for selection in selection.selected {
            let artifact_id = selection.artifact_id;
            let event = self.remove_mutation_artifact(
                artifact_id.clone(),
                selection.requested_status,
                selection.reason.to_owned(),
            )?;
            let payload =
                serde_json::from_value::<MutationArtifactLifecycleRecorded>(event.payload.clone())
                    .context("failed to decode mutation artifact lifecycle event")?;
            update_artifact_cleanup_report_counts(
                &mut report,
                payload.status,
                *artifact_sizes.get(&artifact_id).unwrap_or(&0),
            );
            report.lifecycle_events.push(event);
        }

        Ok(report)
    }

    fn remove_mutation_artifact(
        &self,
        artifact_id: MutationArtifactId,
        requested_status: MutationArtifactLifecycleStatus,
        reason: String,
    ) -> Result<StoredEvent> {
        let located = locate_mutation_artifacts(&self.artifact_root, &artifact_id)?;
        if located.is_empty() {
            let payload = MutationArtifactLifecycleRecorded {
                artifact_id,
                status: MutationArtifactLifecycleStatus::Unavailable,
                reason,
                content_hash: None,
                size: None,
                operation_ids: Vec::new(),
                source_paths: Vec::new(),
            };
            return self.append_artifact_lifecycle_recorded(&payload);
        }

        let content_hash = located
            .first()
            .map(|artifact| artifact.metadata.content_hash.clone());
        let size = located.first().map(|artifact| artifact.metadata.size);
        let mut operation_ids = located
            .iter()
            .map(|artifact| artifact.metadata.operation_id.clone())
            .collect::<Vec<_>>();
        operation_ids.sort();
        operation_ids.dedup();
        let mut source_paths = located
            .iter()
            .map(|artifact| artifact.metadata.source_path.clone())
            .collect::<Vec<_>>();
        source_paths.sort();
        source_paths.dedup();
        let any_blob_available = located.iter().any(|artifact| artifact.blob_available);
        let status = if any_blob_available {
            requested_status
        } else {
            MutationArtifactLifecycleStatus::Unavailable
        };
        let mut synced_parents = Vec::<PathBuf>::new();
        for artifact in &located {
            remove_file_if_exists(&artifact.blob_path)?;
            remove_file_if_exists(&artifact.metadata_path)?;
            if let Some(parent) = artifact.blob_path.parent() {
                synced_parents.push(parent.to_path_buf());
            }
            if let Some(parent) = artifact.metadata_path.parent() {
                synced_parents.push(parent.to_path_buf());
            }
        }
        synced_parents.sort();
        synced_parents.dedup();
        for parent in synced_parents {
            sync_existing_dir(&parent)?;
        }
        if self.artifact_root.exists() {
            sync_existing_dir(&self.artifact_root)?;
        }

        let payload = MutationArtifactLifecycleRecorded {
            artifact_id,
            status,
            reason,
            content_hash,
            size,
            operation_ids,
            source_paths,
        };
        self.append_artifact_lifecycle_recorded(&payload)
    }

    /// Reconciles prepared mutations that were persisted without a terminal commit.
    ///
    /// This is intentionally conservative: it never replays a mutation. It only records what the
    /// workspace currently looks like so downstream verification can treat the affected workspace
    /// snapshot as stale or conflicted.
    pub fn reconcile_prepared_mutations(
        &self,
        workspace_root: impl AsRef<Path>,
    ) -> Result<Vec<StoredEvent>> {
        let workspace_root = fs::canonicalize(workspace_root.as_ref()).with_context(|| {
            format!(
                "failed to canonicalize {}",
                workspace_root.as_ref().display()
            )
        })?;
        let mut prepared = Vec::new();
        let mut terminal_operation_ids = std::collections::BTreeSet::new();
        let mut latest_revision = 0;

        for record in JsonlSessionStore::read_event_records(self.store.path())? {
            let crate::SessionStreamRecord::Stored(event) = record else {
                continue;
            };
            match DurableEventType::from_event_type(&event.event_type) {
                Some(DurableEventType::MutationPrepared) => {
                    let payload = serde_json::from_value::<MutationPrepared>(event.payload.clone())
                        .with_context(|| {
                            format!(
                                "failed to decode {}",
                                DurableEventType::MutationPrepared.as_str()
                            )
                        })?;
                    latest_revision = latest_revision.max(payload.base_workspace_revision);
                    prepared.push(payload);
                }
                Some(DurableEventType::MutationCommitted) => {
                    let payload =
                        serde_json::from_value::<MutationCommitted>(event.payload.clone())
                            .with_context(|| {
                                format!(
                                    "failed to decode {}",
                                    DurableEventType::MutationCommitted.as_str()
                                )
                            })?;
                    latest_revision = latest_revision.max(payload.workspace_revision);
                    terminal_operation_ids.insert(payload.operation_id);
                }
                Some(DurableEventType::MutationReconciled) => {
                    let payload =
                        serde_json::from_value::<MutationReconciled>(event.payload.clone())
                            .with_context(|| {
                                format!(
                                    "failed to decode {}",
                                    DurableEventType::MutationReconciled.as_str()
                                )
                            })?;
                    if let Some(revision) = payload.workspace_revision {
                        latest_revision = latest_revision.max(revision);
                    }
                    terminal_operation_ids.insert(payload.operation_id);
                }
                _ => {}
            }
        }

        let mut events = Vec::new();
        for payload in prepared {
            if terminal_operation_ids.contains(&payload.operation_id) {
                continue;
            }
            let (relative_path, observed_hash, subject_kind) = match &payload.subject {
                MutationSubject::File { path, .. } => (
                    path,
                    file_content_hash(&workspace_root.join(path))?,
                    FileType::File,
                ),
                MutationSubject::Directory { path } => (
                    path,
                    directory_state_hash(&workspace_root.join(path))?,
                    FileType::Directory,
                ),
                _ => {
                    let event = self.append_reconciled(&MutationReconciled {
                        operation_id: payload.operation_id,
                        batch_id: payload.batch_id,
                        observed_state: MutationObservedState::Unknown,
                        resolution: MutationResolution::MarkUnknownDirty,
                        workspace_revision: None,
                        workspace_snapshot_id: None,
                    })?;
                    events.push(event);
                    continue;
                }
            };
            let (observed_state, resolution, workspace_revision, workspace_snapshot_id) =
                if observed_hash == payload.before_hash {
                    (
                        MutationObservedState::NotApplied,
                        MutationResolution::MarkNotApplied,
                        None,
                        None,
                    )
                } else if observed_hash == payload.intended_after_hash {
                    let revision = latest_revision.saturating_add(1);
                    latest_revision = revision;
                    let snapshot_id = single_subject_snapshot_id(
                        &payload.workspace_id,
                        relative_path,
                        subject_kind,
                        observed_hash,
                    )?;
                    (
                        MutationObservedState::AppliedAsIntended,
                        MutationResolution::MarkCommitted,
                        Some(revision),
                        Some(snapshot_id),
                    )
                } else {
                    let revision = latest_revision.saturating_add(1);
                    latest_revision = revision;
                    let snapshot_id = single_subject_snapshot_id(
                        &payload.workspace_id,
                        relative_path,
                        subject_kind,
                        observed_hash,
                    )?;
                    (
                        MutationObservedState::AppliedDifferently,
                        MutationResolution::MarkConflict,
                        Some(revision),
                        Some(snapshot_id),
                    )
                };

            let event = self.append_reconciled(&MutationReconciled {
                operation_id: payload.operation_id,
                batch_id: payload.batch_id,
                observed_state,
                resolution,
                workspace_revision,
                workspace_snapshot_id,
            })?;
            events.push(event);
        }
        Ok(events)
    }

    pub fn append_batch_started(
        &self,
        batch_id: &str,
        operation_id: &str,
        expected_subjects: &[MutationSubject],
    ) -> Result<StoredEvent> {
        self.store.append_event(
            DurableEventType::MutationBatchStarted,
            EventClass::Critical,
            json!({
                "batch_id": batch_id,
                "operation_id": operation_id,
                "expected_subjects": expected_subjects,
            }),
        )
    }

    pub fn append_batch_finished(
        &self,
        batch_id: &str,
        status: MutationBatchStatus,
        committed_operations: &[OperationId],
        failed_operations: &[OperationId],
    ) -> Result<StoredEvent> {
        self.store.append_event(
            DurableEventType::MutationBatchFinished,
            EventClass::Critical,
            json!({
                "batch_id": batch_id,
                "status": status,
                "committed_operations": committed_operations,
                "failed_operations": failed_operations,
            }),
        )
    }

    /// Captures the verification-scope workspace state before or after an unknown-effect tool.
    ///
    /// The snapshot id is content-bound when every entry is complete. Incomplete coverage is
    /// preserved as `WorkspaceKnowledge::UnknownDirty` so verification can fail closed.
    pub fn capture_workspace_scan(
        &self,
        workspace_root: impl AsRef<Path>,
        scope: &VerificationScope,
    ) -> Result<WorkspaceMutationScan> {
        let workspace_root = workspace_root.as_ref();
        let workspace_id = stable_workspace_id(workspace_root)?;
        let workspace_revision = latest_workspace_revision(&self.store, &workspace_id)?;
        let snapshot = build_workspace_snapshot(
            workspace_root,
            workspace_id.clone(),
            scope,
            workspace_revision,
        )?;
        Ok(workspace_scan_from_snapshot(
            workspace_id,
            scope.clone(),
            workspace_revision,
            snapshot,
        ))
    }

    /// Records a workspace mutation if the after-snapshot differs from the before-snapshot.
    ///
    /// # Errors
    ///
    /// Returns an error if the after snapshot cannot be captured or if the durable mutation event
    /// cannot be appended.
    pub fn record_workspace_mutation_if_changed(
        &self,
        before: &WorkspaceMutationScan,
        workspace_root: impl AsRef<Path>,
        tool_call_id: impl Into<ToolCallId>,
        tool_name: impl Into<String>,
        tool_effect: ToolEffect,
    ) -> Result<Option<StoredEvent>> {
        let after = self.capture_workspace_scan(workspace_root, &before.scope)?;
        self.record_workspace_mutation_scan_result(
            before,
            &after,
            tool_call_id,
            tool_name,
            tool_effect,
        )
    }

    pub fn record_workspace_mutation_scan_result(
        &self,
        before: &WorkspaceMutationScan,
        after: &WorkspaceMutationScan,
        tool_call_id: impl Into<ToolCallId>,
        tool_name: impl Into<String>,
        tool_effect: ToolEffect,
    ) -> Result<Option<StoredEvent>> {
        let reason = workspace_mutation_detection_reason(before, after);
        let Some(reason) = reason else {
            return Ok(None);
        };
        let tool_call_id = tool_call_id.into();
        let tool_name = tool_name.into();
        let workspace_revision = latest_workspace_revision(&self.store, &before.workspace_id)?
            .max(before.workspace_revision)
            .saturating_add(1);
        let payload = WorkspaceMutationDetected {
            operation_id: workspace_detection_operation_id(
                &before.workspace_id,
                &tool_call_id,
                before.workspace_snapshot_id.as_deref(),
                after.workspace_snapshot_id.as_deref(),
                reason,
            ),
            tool_call_id: Some(tool_call_id),
            tool_name,
            tool_effect,
            workspace_id: before.workspace_id.clone(),
            scope_hash: before.scope_hash.clone(),
            from_workspace_snapshot_id: before.workspace_snapshot_id.clone(),
            to_workspace_snapshot_id: after.workspace_snapshot_id.clone(),
            base_workspace_revision: before.workspace_revision,
            workspace_revision,
            reason,
            unknown_dirty: before.workspace_knowledge.is_unknown_dirty()
                || after.workspace_knowledge.is_unknown_dirty(),
            metadata: BTreeMap::new(),
        };
        self.append_workspace_mutation_detected(&payload).map(Some)
    }

    /// Records a non-tool external process mutation if its after-snapshot differs.
    ///
    /// This is used for process lifecycle paths such as MCP server startup, where the runtime
    /// has no model-visible tool call id but can still compare workspace snapshots before and
    /// after the process boundary.
    pub fn record_external_process_mutation_scan_result(
        &self,
        before: &WorkspaceMutationScan,
        after: &WorkspaceMutationScan,
        process_name: impl Into<String>,
        tool_effect: ToolEffect,
        metadata: BTreeMap<String, String>,
    ) -> Result<Option<StoredEvent>> {
        let reason = workspace_mutation_detection_reason(before, after);
        let Some(reason) = reason else {
            return Ok(None);
        };
        let process_name = process_name.into();
        let workspace_revision = latest_workspace_revision(&self.store, &before.workspace_id)?
            .max(before.workspace_revision)
            .saturating_add(1);
        let payload = WorkspaceMutationDetected {
            operation_id: workspace_detection_operation_id(
                &before.workspace_id,
                &process_name,
                before.workspace_snapshot_id.as_deref(),
                after.workspace_snapshot_id.as_deref(),
                reason,
            ),
            tool_call_id: None,
            tool_name: process_name,
            tool_effect,
            workspace_id: before.workspace_id.clone(),
            scope_hash: before.scope_hash.clone(),
            from_workspace_snapshot_id: before.workspace_snapshot_id.clone(),
            to_workspace_snapshot_id: after.workspace_snapshot_id.clone(),
            base_workspace_revision: before.workspace_revision,
            workspace_revision,
            reason,
            unknown_dirty: before.workspace_knowledge.is_unknown_dirty()
                || after.workspace_knowledge.is_unknown_dirty(),
            metadata,
        };
        self.append_workspace_mutation_detected(&payload).map(Some)
    }

    /// Records an unknown-dirty mutation when scan coverage is unavailable.
    pub fn record_workspace_scan_unavailable(
        &self,
        workspace_root: impl AsRef<Path>,
        tool_call_id: impl Into<ToolCallId>,
        tool_name: impl Into<String>,
        tool_effect: ToolEffect,
    ) -> Result<StoredEvent> {
        let workspace_root = workspace_root.as_ref();
        let workspace_id = stable_workspace_id(workspace_root)?;
        let workspace_revision = latest_workspace_revision(&self.store, &workspace_id)?;
        let tool_call_id = tool_call_id.into();
        let tool_name = tool_name.into();
        let payload = WorkspaceMutationDetected {
            operation_id: workspace_detection_operation_id(
                &workspace_id,
                &tool_call_id,
                None,
                None,
                WorkspaceMutationDetectionReason::ScanUnavailable,
            ),
            tool_call_id: Some(tool_call_id),
            tool_name,
            tool_effect,
            workspace_id,
            scope_hash: DEFAULT_TASK_VERIFICATION_SCOPE_HASH.to_owned(),
            from_workspace_snapshot_id: None,
            to_workspace_snapshot_id: None,
            base_workspace_revision: workspace_revision,
            workspace_revision: workspace_revision.saturating_add(1),
            reason: WorkspaceMutationDetectionReason::ScanUnavailable,
            unknown_dirty: true,
            metadata: BTreeMap::new(),
        };
        self.append_workspace_mutation_detected(&payload)
    }

    /// Records an unknown-dirty non-tool external process mutation after a failed after-scan.
    pub fn record_external_process_scan_unavailable_after(
        &self,
        before: &WorkspaceMutationScan,
        process_name: impl Into<String>,
        tool_effect: ToolEffect,
        metadata: BTreeMap<String, String>,
    ) -> Result<StoredEvent> {
        let process_name = process_name.into();
        let workspace_revision = latest_workspace_revision(&self.store, &before.workspace_id)?
            .max(before.workspace_revision)
            .saturating_add(1);
        let payload = WorkspaceMutationDetected {
            operation_id: workspace_detection_operation_id(
                &before.workspace_id,
                &process_name,
                before.workspace_snapshot_id.as_deref(),
                None,
                WorkspaceMutationDetectionReason::ScanUnavailable,
            ),
            tool_call_id: None,
            tool_name: process_name,
            tool_effect,
            workspace_id: before.workspace_id.clone(),
            scope_hash: before.scope_hash.clone(),
            from_workspace_snapshot_id: before.workspace_snapshot_id.clone(),
            to_workspace_snapshot_id: None,
            base_workspace_revision: before.workspace_revision,
            workspace_revision,
            reason: WorkspaceMutationDetectionReason::ScanUnavailable,
            unknown_dirty: true,
            metadata,
        };
        self.append_workspace_mutation_detected(&payload)
    }

    /// Records a conservative unknown-dirty mutation for a non-tool external process lifecycle.
    ///
    /// This is used for processes such as TUI-triggered MCP server activation where there is no
    /// model tool call id, but the process may continue mutating the workspace after startup.
    pub fn record_external_process_unknown_dirty(
        &self,
        workspace_root: impl AsRef<Path>,
        process_name: impl Into<String>,
        tool_effect: ToolEffect,
    ) -> Result<StoredEvent> {
        self.record_external_process_unknown_dirty_with_metadata(
            workspace_root,
            process_name,
            tool_effect,
            BTreeMap::new(),
        )
    }

    /// Records a conservative unknown-dirty external process lifecycle with audit metadata.
    ///
    /// Metadata is not model-visible evidence. It exists to make durable session audit and product
    /// surfaces explicit about the external process boundary that caused the unknown-dirty state.
    pub fn record_external_process_unknown_dirty_with_metadata(
        &self,
        workspace_root: impl AsRef<Path>,
        process_name: impl Into<String>,
        tool_effect: ToolEffect,
        metadata: BTreeMap<String, String>,
    ) -> Result<StoredEvent> {
        let workspace_root = workspace_root.as_ref();
        let workspace_id = stable_workspace_id(workspace_root)?;
        let workspace_revision = latest_workspace_revision(&self.store, &workspace_id)?;
        let process_name = process_name.into();
        let operation_seed = format!("{process_name}:{workspace_revision}");
        let payload = WorkspaceMutationDetected {
            operation_id: workspace_detection_operation_id(
                &workspace_id,
                &operation_seed,
                None,
                None,
                WorkspaceMutationDetectionReason::DeclaredWriteEffect,
            ),
            tool_call_id: None,
            tool_name: process_name,
            tool_effect,
            workspace_id,
            scope_hash: DEFAULT_TASK_VERIFICATION_SCOPE_HASH.to_owned(),
            from_workspace_snapshot_id: None,
            to_workspace_snapshot_id: None,
            base_workspace_revision: workspace_revision,
            workspace_revision: workspace_revision.saturating_add(1),
            reason: WorkspaceMutationDetectionReason::DeclaredWriteEffect,
            unknown_dirty: true,
            metadata,
        };
        self.append_workspace_mutation_detected(&payload)
    }

    pub fn record_workspace_scan_unavailable_after(
        &self,
        before: &WorkspaceMutationScan,
        tool_call_id: impl Into<ToolCallId>,
        tool_name: impl Into<String>,
        tool_effect: ToolEffect,
    ) -> Result<StoredEvent> {
        let tool_call_id = tool_call_id.into();
        let tool_name = tool_name.into();
        let workspace_revision = latest_workspace_revision(&self.store, &before.workspace_id)?
            .max(before.workspace_revision)
            .saturating_add(1);
        let payload = WorkspaceMutationDetected {
            operation_id: workspace_detection_operation_id(
                &before.workspace_id,
                &tool_call_id,
                before.workspace_snapshot_id.as_deref(),
                None,
                WorkspaceMutationDetectionReason::ScanUnavailable,
            ),
            tool_call_id: Some(tool_call_id),
            tool_name,
            tool_effect,
            workspace_id: before.workspace_id.clone(),
            scope_hash: before.scope_hash.clone(),
            from_workspace_snapshot_id: before.workspace_snapshot_id.clone(),
            to_workspace_snapshot_id: None,
            base_workspace_revision: before.workspace_revision,
            workspace_revision,
            reason: WorkspaceMutationDetectionReason::ScanUnavailable,
            unknown_dirty: true,
            metadata: BTreeMap::new(),
        };
        self.append_workspace_mutation_detected(&payload)
    }

    pub fn execution_mutation_profile(
        &self,
        workspace_root: impl AsRef<Path>,
        scope: &VerificationScope,
        tool_call_id: impl Into<ToolCallId>,
        tool_name: impl Into<String>,
        effect: ToolEffect,
    ) -> Result<ExecutionMutationProfile> {
        let scan = self.capture_workspace_scan(workspace_root, scope)?;
        let profile = ExecutionMutationProfile {
            tool_call_id: tool_call_id.into(),
            tool_name: tool_name.into(),
            effect,
            workspace_id: scan.workspace_id,
            scan_scope_hash: scan.scope_hash,
            pre_execution_snapshot_id: scan.workspace_snapshot_id,
            pre_execution_workspace_revision: scan.workspace_revision,
            workspace_knowledge: scan.workspace_knowledge,
        };
        Ok(profile)
    }

    pub fn reconcile_execution_mutation_profile(
        &self,
        workspace_root: impl AsRef<Path>,
        profile: &ExecutionMutationProfile,
    ) -> Result<Option<StoredEvent>> {
        let latest_detection =
            self.latest_workspace_detection_for_tool_call(&profile.tool_call_id)?;
        let scope = VerificationScope::all_tracked(profile.scan_scope_hash.clone());
        let before = WorkspaceMutationScan {
            workspace_id: profile.workspace_id.clone(),
            scope_hash: profile.scan_scope_hash.clone(),
            scope: scope.clone(),
            workspace_revision: profile.pre_execution_workspace_revision,
            workspace_snapshot_id: profile.pre_execution_snapshot_id.clone(),
            workspace_knowledge: profile.workspace_knowledge.clone(),
        };
        let workspace_root = workspace_root.as_ref();
        if let Ok(after) = self.capture_workspace_scan(workspace_root, &scope) {
            if latest_detection
                .as_ref()
                .is_some_and(|detection| detection_already_covers_after_scan(detection, &after))
            {
                return Ok(None);
            }
            return self.record_workspace_mutation_scan_result(
                &before,
                &after,
                profile.tool_call_id.clone(),
                profile.tool_name.clone(),
                profile.effect,
            );
        }
        if latest_detection.as_ref().is_some_and(|detection| {
            detection.unknown_dirty
                && detection.reason == WorkspaceMutationDetectionReason::ScanUnavailable
        }) {
            return Ok(None);
        }
        let call_id = profile.tool_call_id.clone();
        let tool_name = profile.tool_name.clone();
        let effect = profile.effect;
        let event =
            self.record_workspace_scan_unavailable(workspace_root, call_id, tool_name, effect)?;
        Ok(Some(event))
    }

    fn latest_workspace_detection_for_tool_call(
        &self,
        tool_call_id: &str,
    ) -> Result<Option<WorkspaceMutationDetected>> {
        let mut latest = None;
        for record in JsonlSessionStore::read_event_records(self.store.path())? {
            if let crate::SessionStreamRecord::Stored(event) = record {
                if DurableEventType::from_event_type(&event.event_type)
                    != Some(DurableEventType::WorkspaceMutationDetected)
                {
                    continue;
                }
                let payload = serde_json::from_value::<WorkspaceMutationDetected>(event.payload)
                    .context("failed to decode workspace mutation detected payload")?;
                if payload.tool_call_id.as_deref() == Some(tool_call_id) {
                    latest = Some(payload);
                }
            }
        }
        Ok(latest)
    }
}

/// Controlled mutation coordinator for one tool call.
#[derive(Debug, Clone)]
pub struct MutationCoordinator {
    recorder: MutationEventRecorder,
    workspace_root: PathBuf,
    workspace_id: WorkspaceId,
    tool_call_id: ToolCallId,
    batch_id: Option<MutationBatchId>,
}

impl MutationCoordinator {
    pub fn prepare_directory(
        &self,
        relative_path: impl Into<PathBuf>,
        absolute_path: impl Into<PathBuf>,
        intended_after_hash: Option<String>,
    ) -> Result<PreparedDirectoryMutation> {
        let relative_path = normalize_relative_path(relative_path.into())?;
        let absolute_path = absolute_path.into();
        ensure_absolute_path_matches_subject(&self.workspace_root, &relative_path, &absolute_path)?;
        let before_hash = directory_state_hash(&absolute_path)?;
        let base_workspace_revision =
            latest_workspace_revision(&self.recorder.store, &self.workspace_id)?;
        let operation_id = operation_id_for(
            &self.workspace_id,
            &self.tool_call_id,
            self.batch_id.as_deref(),
            &relative_path,
            before_hash.as_deref(),
            intended_after_hash.as_deref(),
        );
        let payload = MutationPrepared {
            operation_id: operation_id.clone(),
            batch_id: self.batch_id.clone(),
            tool_call_id: Some(self.tool_call_id.clone()),
            causation_event_id: self.tool_call_id.clone(),
            subject: MutationSubject::Directory {
                path: relative_path.clone(),
            },
            before_hash: before_hash.clone(),
            intended_after_hash: intended_after_hash.clone(),
            snapshot_coverage: if before_hash.is_some() {
                SnapshotCoverage::Unsupported
            } else {
                SnapshotCoverage::NoPriorContent
            },
            workspace_id: self.workspace_id.clone(),
            base_workspace_revision,
            sync_class: MutationSyncClass::RecoveryCritical,
        };
        let event = self.recorder.append_prepared(&payload)?;
        Ok(PreparedDirectoryMutation {
            prepared_event_id: event.event_id,
            prepared_stream_sequence: event.stream_sequence,
            operation_id,
            batch_id: self.batch_id.clone(),
            tool_call_id: Some(self.tool_call_id.clone()),
            workspace_id: self.workspace_id.clone(),
            workspace_root: self.workspace_root.clone(),
            relative_path,
            absolute_path,
            before_hash,
            intended_after_hash,
            base_workspace_revision,
        })
    }

    pub fn prepare_file(
        &self,
        relative_path: impl Into<PathBuf>,
        absolute_path: impl Into<PathBuf>,
        intended_after_hash: Option<String>,
    ) -> Result<PreparedFileMutation> {
        let relative_path = normalize_relative_path(relative_path.into())?;
        let absolute_path = absolute_path.into();
        ensure_absolute_path_matches_subject(&self.workspace_root, &relative_path, &absolute_path)?;
        let before_hash = file_content_hash(&absolute_path)?;
        let base_workspace_revision =
            latest_workspace_revision(&self.recorder.store, &self.workspace_id)?;
        let operation_id = operation_id_for(
            &self.workspace_id,
            &self.tool_call_id,
            self.batch_id.as_deref(),
            &relative_path,
            before_hash.as_deref(),
            intended_after_hash.as_deref(),
        );
        let snapshot_coverage = snapshot_coverage_for_pre_mutation_content(
            &self.recorder.artifact_root,
            &self.workspace_id,
            &operation_id,
            &relative_path,
            &absolute_path,
            before_hash.as_deref(),
        )?;
        let payload = MutationPrepared {
            operation_id: operation_id.clone(),
            batch_id: self.batch_id.clone(),
            tool_call_id: Some(self.tool_call_id.clone()),
            causation_event_id: self.tool_call_id.clone(),
            subject: MutationSubject::File {
                path: relative_path.clone(),
                file_type: FileType::File,
            },
            before_hash: before_hash.clone(),
            intended_after_hash: intended_after_hash.clone(),
            snapshot_coverage,
            workspace_id: self.workspace_id.clone(),
            base_workspace_revision,
            sync_class: MutationSyncClass::RecoveryCritical,
        };
        let event = self.recorder.append_prepared(&payload)?;
        Ok(PreparedFileMutation {
            prepared_event_id: event.event_id,
            prepared_stream_sequence: event.stream_sequence,
            operation_id,
            batch_id: self.batch_id.clone(),
            tool_call_id: Some(self.tool_call_id.clone()),
            workspace_id: self.workspace_id.clone(),
            workspace_root: self.workspace_root.clone(),
            relative_path,
            absolute_path,
            before_hash,
            intended_after_hash,
            base_workspace_revision,
        })
    }

    pub fn create_missing_parent_directories(
        &self,
        target_absolute_path: &Path,
    ) -> Result<Vec<CommittedDirectoryMutation>> {
        let Some(parent) = target_absolute_path.parent() else {
            return Ok(Vec::new());
        };
        let normalized_parent = normalize_absolute_path_for_subject(parent)?;
        let relative_parent = normalized_parent
            .strip_prefix(&self.workspace_root)
            .with_context(|| {
                format!(
                    "target parent {} is outside workspace {}",
                    parent.display(),
                    self.workspace_root.display()
                )
            })?;
        let mut committed = Vec::new();
        let mut relative = PathBuf::new();
        for component in relative_parent.components() {
            let std::path::Component::Normal(part) = component else {
                bail!(
                    "unsupported directory component in {}",
                    relative_parent.display()
                );
            };
            relative.push(part);
            let absolute = self.workspace_root.join(&relative);
            match fs::symlink_metadata(&absolute) {
                Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {}
                Ok(_) => bail!("parent path is not a directory: {}", absolute.display()),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    let prepared = self.prepare_directory(
                        relative.clone(),
                        absolute.clone(),
                        Some(directory_present_hash()),
                    )?;
                    committed.push(self.commit_create_directory(&prepared)?);
                }
                Err(error) => {
                    return Err(error)
                        .with_context(|| format!("failed to inspect {}", absolute.display()));
                }
            }
        }
        Ok(committed)
    }

    pub fn commit_write(
        &self,
        prepared: &PreparedFileMutation,
        content: &[u8],
    ) -> Result<CommittedFileMutation> {
        let intended_hash = bytes_hash(content);
        if prepared.intended_after_hash.as_deref() != Some(intended_hash.as_str()) {
            bail!("prepared mutation intended hash does not match write content");
        }
        compare_current_hash(&prepared.absolute_path, prepared.before_hash.as_deref())?;
        if let Some(parent) = prepared.absolute_path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("failed to create {}", parent.display()))?;
        }
        atomic_replace(&prepared.absolute_path, content)?;
        let observed_after_hash = file_content_hash(&prepared.absolute_path)?;
        ensure_observed_after_hash_matches_intent(&observed_after_hash, &intended_hash)?;
        self.record_commit(prepared, observed_after_hash)
    }

    pub fn commit_create_directory(
        &self,
        prepared: &PreparedDirectoryMutation,
    ) -> Result<CommittedDirectoryMutation> {
        if prepared.intended_after_hash.as_deref() != Some(directory_present_hash().as_str()) {
            bail!("directory create mutation must intend a present directory");
        }
        compare_current_directory_hash(&prepared.absolute_path, prepared.before_hash.as_deref())?;
        fs::create_dir(&prepared.absolute_path)
            .with_context(|| format!("failed to create {}", prepared.absolute_path.display()))?;
        sync_parent(&prepared.absolute_path)?;
        let observed_after_hash = directory_state_hash(&prepared.absolute_path)?;
        if observed_after_hash.as_deref() != Some(directory_present_hash().as_str()) {
            bail!("observed directory state does not match intended state after create");
        }
        self.record_directory_commit(prepared, observed_after_hash)
    }

    pub fn commit_delete_directory(
        &self,
        prepared: &PreparedDirectoryMutation,
    ) -> Result<CommittedDirectoryMutation> {
        if prepared.intended_after_hash.is_some() {
            bail!("directory delete mutation must not have an intended after hash");
        }
        compare_current_directory_hash(&prepared.absolute_path, prepared.before_hash.as_deref())?;
        fs::remove_dir(&prepared.absolute_path)
            .with_context(|| format!("failed to delete {}", prepared.absolute_path.display()))?;
        sync_parent(&prepared.absolute_path)?;
        let observed_after_hash = directory_state_hash(&prepared.absolute_path)?;
        if observed_after_hash.is_some() {
            bail!("observed directory state does not match intended state after delete");
        }
        self.record_directory_commit(prepared, observed_after_hash)
    }

    pub fn commit_delete(&self, prepared: &PreparedFileMutation) -> Result<CommittedFileMutation> {
        if prepared.intended_after_hash.is_some() {
            bail!("delete mutation must not have an intended after hash");
        }
        compare_current_hash(&prepared.absolute_path, prepared.before_hash.as_deref())?;
        fs::remove_file(&prepared.absolute_path)
            .with_context(|| format!("failed to delete {}", prepared.absolute_path.display()))?;
        sync_parent(&prepared.absolute_path)?;
        self.record_commit(prepared, None)
    }

    fn record_commit(
        &self,
        prepared: &PreparedFileMutation,
        observed_after_hash: Option<String>,
    ) -> Result<CommittedFileMutation> {
        let workspace_revision =
            latest_workspace_revision(&self.recorder.store, &prepared.workspace_id)?
                .max(prepared.base_workspace_revision)
                .saturating_add(1);
        let workspace_snapshot_id = single_subject_snapshot_id(
            &prepared.workspace_id,
            &prepared.relative_path,
            FileType::File,
            observed_after_hash.clone(),
        )?;
        let payload = MutationCommitted {
            operation_id: prepared.operation_id.clone(),
            batch_id: prepared.batch_id.clone(),
            workspace_id: Some(prepared.workspace_id.clone()),
            observed_after_hash: observed_after_hash.clone(),
            workspace_revision,
            workspace_snapshot_id: workspace_snapshot_id.clone(),
            committed_subject: MutationSubject::File {
                path: prepared.relative_path.clone(),
                file_type: FileType::File,
            },
        };
        let committed_event = self.recorder.append_committed(&payload)?;
        let mut committed = CommittedFileMutation {
            committed_event: committed_event.clone(),
            write_event: committed_event.clone(),
            operation_id: prepared.operation_id.clone(),
            batch_id: prepared.batch_id.clone(),
            workspace_revision,
            workspace_snapshot_id,
            observed_after_hash,
        };
        let write_event = self
            .recorder
            .append_write_committed(&committed, &committed_event.event_id)?;
        committed.write_event = write_event;
        Ok(committed)
    }

    fn record_directory_commit(
        &self,
        prepared: &PreparedDirectoryMutation,
        observed_after_hash: Option<String>,
    ) -> Result<CommittedDirectoryMutation> {
        let workspace_revision =
            latest_workspace_revision(&self.recorder.store, &prepared.workspace_id)?
                .max(prepared.base_workspace_revision)
                .saturating_add(1);
        let workspace_snapshot_id = single_subject_snapshot_id(
            &prepared.workspace_id,
            &prepared.relative_path,
            FileType::Directory,
            observed_after_hash.clone(),
        )?;
        let payload = MutationCommitted {
            operation_id: prepared.operation_id.clone(),
            batch_id: prepared.batch_id.clone(),
            workspace_id: Some(prepared.workspace_id.clone()),
            observed_after_hash: observed_after_hash.clone(),
            workspace_revision,
            workspace_snapshot_id: workspace_snapshot_id.clone(),
            committed_subject: MutationSubject::Directory {
                path: prepared.relative_path.clone(),
            },
        };
        let committed_event = self.recorder.append_committed(&payload)?;
        Ok(CommittedDirectoryMutation {
            committed_event,
            operation_id: prepared.operation_id.clone(),
            batch_id: prepared.batch_id.clone(),
            workspace_revision,
            workspace_snapshot_id,
            observed_after_hash,
        })
    }
}

pub fn write_file_with_mutation(
    recorder: Option<&MutationEventRecorder>,
    workspace_root: &Path,
    tool_call_id: &str,
    relative_path: impl Into<PathBuf>,
    absolute_path: impl Into<PathBuf>,
    content: &[u8],
) -> Result<Option<CommittedFileMutation>> {
    write_file_with_mutation_in_batch(
        recorder,
        workspace_root,
        tool_call_id,
        None,
        relative_path,
        absolute_path,
        content,
    )
}

/// Writes a file with RFC-0002 mutation evidence when a recorder is available.
///
/// `recorder = None` is a legacy compatibility path for non-durable callers and tests. Durable
/// agent/tool runs must pass a recorder; otherwise the file write cannot produce
/// `MutationPrepared` / `MutationCommitted` evidence and must not be treated as verified-clean.
pub fn write_file_with_mutation_in_batch(
    recorder: Option<&MutationEventRecorder>,
    workspace_root: &Path,
    tool_call_id: &str,
    batch_id: Option<MutationBatchId>,
    relative_path: impl Into<PathBuf>,
    absolute_path: impl Into<PathBuf>,
    content: &[u8],
) -> Result<Option<CommittedFileMutation>> {
    let Some(recorder) = recorder else {
        let absolute_path = absolute_path.into();
        if let Some(parent) = absolute_path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("failed to create {}", parent.display()))?;
        }
        atomic_replace(&absolute_path, content)?;
        return Ok(None);
    };
    let coordinator = recorder.coordinator(workspace_root, tool_call_id.to_owned(), batch_id)?;
    let relative_path = normalize_relative_path(relative_path.into())?;
    let absolute_path = absolute_path.into();
    ensure_absolute_path_matches_subject(
        &coordinator.workspace_root,
        &relative_path,
        &absolute_path,
    )?;
    coordinator.create_missing_parent_directories(&absolute_path)?;
    let intended_after_hash = Some(bytes_hash(content));
    let prepared = coordinator.prepare_file(relative_path, absolute_path, intended_after_hash)?;
    coordinator.commit_write(&prepared, content).map(Some)
}

pub fn create_directory_with_mutation(
    recorder: Option<&MutationEventRecorder>,
    workspace_root: &Path,
    tool_call_id: &str,
    relative_path: impl Into<PathBuf>,
    absolute_path: impl Into<PathBuf>,
) -> Result<Option<CommittedDirectoryMutation>> {
    let absolute_path = absolute_path.into();
    let Some(recorder) = recorder else {
        fs::create_dir(&absolute_path)
            .with_context(|| format!("failed to create {}", absolute_path.display()))?;
        sync_parent(&absolute_path)?;
        return Ok(None);
    };
    let coordinator = recorder.coordinator(workspace_root, tool_call_id.to_owned(), None)?;
    let prepared = coordinator.prepare_directory(
        relative_path,
        &absolute_path,
        Some(directory_present_hash()),
    )?;
    coordinator.commit_create_directory(&prepared).map(Some)
}

pub fn delete_directory_with_mutation(
    recorder: Option<&MutationEventRecorder>,
    workspace_root: &Path,
    tool_call_id: &str,
    relative_path: impl Into<PathBuf>,
    absolute_path: impl Into<PathBuf>,
) -> Result<Option<CommittedDirectoryMutation>> {
    let absolute_path = absolute_path.into();
    let Some(recorder) = recorder else {
        fs::remove_dir(&absolute_path)
            .with_context(|| format!("failed to delete {}", absolute_path.display()))?;
        sync_parent(&absolute_path)?;
        return Ok(None);
    };
    ensure_empty_directory(&absolute_path)?;
    let coordinator = recorder.coordinator(workspace_root, tool_call_id.to_owned(), None)?;
    let prepared = coordinator.prepare_directory(relative_path, &absolute_path, None)?;
    coordinator.commit_delete_directory(&prepared).map(Some)
}

pub fn delete_file_with_mutation(
    recorder: Option<&MutationEventRecorder>,
    workspace_root: &Path,
    tool_call_id: &str,
    relative_path: impl Into<PathBuf>,
    absolute_path: impl Into<PathBuf>,
) -> Result<Option<CommittedFileMutation>> {
    delete_file_with_mutation_in_batch(
        recorder,
        workspace_root,
        tool_call_id,
        None,
        relative_path,
        absolute_path,
    )
}

/// Deletes a file with RFC-0002 mutation evidence when a recorder is available.
///
/// `recorder = None` is a legacy compatibility path for non-durable callers and tests. Durable
/// agent/tool runs must pass a recorder; otherwise the delete cannot produce
/// `MutationPrepared` / `MutationCommitted` evidence and must not be treated as verified-clean.
pub fn delete_file_with_mutation_in_batch(
    recorder: Option<&MutationEventRecorder>,
    workspace_root: &Path,
    tool_call_id: &str,
    batch_id: Option<MutationBatchId>,
    relative_path: impl Into<PathBuf>,
    absolute_path: impl Into<PathBuf>,
) -> Result<Option<CommittedFileMutation>> {
    let absolute_path = absolute_path.into();
    let Some(recorder) = recorder else {
        fs::remove_file(&absolute_path)
            .with_context(|| format!("failed to delete {}", absolute_path.display()))?;
        sync_parent(&absolute_path)?;
        return Ok(None);
    };
    let coordinator = recorder.coordinator(workspace_root, tool_call_id.to_owned(), batch_id)?;
    let prepared = coordinator.prepare_file(relative_path, &absolute_path, None)?;
    coordinator.commit_delete(&prepared).map(Some)
}

pub fn restore_file_from_snapshot_with_mutation(
    recorder: &MutationEventRecorder,
    workspace_root: &Path,
    tool_call_id: &str,
    relative_path: impl Into<PathBuf>,
    absolute_path: impl Into<PathBuf>,
    snapshot_coverage: SnapshotCoverage,
    expected_current_hash: Option<&str>,
) -> Result<RestoredFileMutation> {
    let relative_path = normalize_relative_path(relative_path.into())?;
    let absolute_path = absolute_path.into();
    let coordinator = recorder.coordinator(workspace_root, tool_call_id.to_owned(), None)?;
    ensure_absolute_path_matches_subject(
        &coordinator.workspace_root,
        &relative_path,
        &absolute_path,
    )?;
    let current_hash = file_content_hash(&absolute_path)?;
    if current_hash.as_deref() != expected_current_hash {
        bail!(
            "file changed before checkpoint restore: {}",
            absolute_path.display()
        );
    }

    let committed = match &snapshot_coverage {
        SnapshotCoverage::Captured(artifact_id) => {
            let content = read_mutation_artifact_content(&recorder.artifact_root, artifact_id)?;
            let intended_after_hash = Some(bytes_hash(&content));
            let prepared = coordinator.prepare_file(
                relative_path.clone(),
                absolute_path.clone(),
                intended_after_hash,
            )?;
            coordinator.commit_write(&prepared, &content)?
        }
        SnapshotCoverage::NoPriorContent => {
            if current_hash.is_none() {
                bail!(
                    "checkpoint restore target already absent: {}",
                    absolute_path.display()
                );
            }
            let prepared =
                coordinator.prepare_file(relative_path.clone(), absolute_path.clone(), None)?;
            coordinator.commit_delete(&prepared)?
        }
        SnapshotCoverage::SkippedSensitive => {
            bail!("checkpoint restore cannot read skipped sensitive snapshot")
        }
        SnapshotCoverage::Unsupported => bail!("checkpoint restore snapshot is unsupported"),
        SnapshotCoverage::Unavailable => bail!("checkpoint restore snapshot is unavailable"),
    };

    let restored_subject = MutationSubject::File {
        path: relative_path,
        file_type: FileType::File,
    };
    let checkpoint_payload = CheckpointRestored {
        operation_id: committed.operation_id.clone(),
        batch_id: committed.batch_id.clone(),
        tool_call_id: Some(tool_call_id.to_owned()),
        restored_subject,
        restored_from: snapshot_coverage.clone(),
        mutation_committed_event_id: committed.committed_event.event_id.clone(),
        workspace_revision: committed.workspace_revision,
        workspace_snapshot_id: committed.workspace_snapshot_id.clone(),
    };
    let checkpoint_event = recorder.append_checkpoint_restored(&checkpoint_payload)?;
    Ok(RestoredFileMutation {
        committed,
        checkpoint_event,
        restored_from: snapshot_coverage,
    })
}

pub fn file_content_hash(path: &Path) -> Result<Option<String>> {
    match fs::read(path) {
        Ok(bytes) => Ok(Some(bytes_hash(&bytes))),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error).with_context(|| format!("failed to read {}", path.display())),
    }
}

fn directory_state_hash(path: &Path) -> Result<Option<String>> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {
            Ok(Some(directory_present_hash()))
        }
        Ok(_) => bail!("path is not a directory: {}", path.display()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error).with_context(|| format!("failed to read {}", path.display())),
    }
}

fn compare_current_directory_hash(path: &Path, expected: Option<&str>) -> Result<()> {
    let current = directory_state_hash(path)?;
    if current.as_deref() != expected {
        bail!(
            "directory changed before controlled mutation commit: {}",
            path.display()
        );
    }
    Ok(())
}

fn ensure_empty_directory(path: &Path) -> Result<()> {
    let metadata = fs::symlink_metadata(path)
        .with_context(|| format!("failed to inspect {}", path.display()))?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        bail!("path is not a directory: {}", path.display());
    }
    let mut entries = fs::read_dir(path)
        .with_context(|| format!("failed to read directory {}", path.display()))?;
    if entries.next().transpose()?.is_some() {
        bail!(
            "non-empty directory delete is not supported by controlled mutation protocol: {}",
            path.display()
        );
    }
    Ok(())
}

fn directory_present_hash() -> String {
    bytes_hash(b"sigil:directory:present:v1")
}

pub fn bytes_hash(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    format!("sha256:{digest:x}")
}

fn compare_current_hash(path: &Path, expected: Option<&str>) -> Result<()> {
    let current = file_content_hash(path)?;
    if current.as_deref() != expected {
        bail!(
            "file changed before controlled mutation commit: {}",
            path.display()
        );
    }
    Ok(())
}

fn atomic_replace(path: &Path, content: &[u8]) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| anyhow!("target path has no parent: {}", path.display()))?;
    fs::create_dir_all(parent).with_context(|| format!("failed to create {}", parent.display()))?;
    let temp_path = temp_path_for(path);
    {
        let mut temp_file = File::create(&temp_path)
            .with_context(|| format!("failed to create {}", temp_path.display()))?;
        temp_file
            .write_all(content)
            .with_context(|| format!("failed to write {}", temp_path.display()))?;
        temp_file
            .sync_all()
            .with_context(|| format!("failed to sync {}", temp_path.display()))?;
    }
    fs::rename(&temp_path, path).with_context(|| atomic_replace_error_message(path, &temp_path))?;
    let file = File::open(path).with_context(|| format!("failed to open {}", path.display()))?;
    file.sync_all()
        .with_context(|| format!("failed to sync {}", path.display()))?;
    sync_parent(path)
}

fn ensure_observed_after_hash_matches_intent(
    observed_after_hash: &Option<String>,
    intended_hash: &str,
) -> Result<()> {
    if observed_after_hash.as_deref() != Some(intended_hash) {
        bail!("observed file hash does not match intended hash after write");
    }
    Ok(())
}

fn atomic_replace_error_message(path: &Path, temp_path: &Path) -> String {
    format!(
        "failed to atomically replace {} with {}",
        path.display(),
        temp_path.display()
    )
}

fn sync_parent(path: &Path) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| anyhow!("target path has no parent: {}", path.display()))?;
    let parent_file =
        File::open(parent).with_context(|| format!("failed to open {}", parent.display()))?;
    parent_file
        .sync_all()
        .with_context(|| format!("failed to sync {}", parent.display()))
}

fn temp_path_for(path: &Path) -> PathBuf {
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("mutation");
    let temp_name = format!(".{file_name}.sigil-tmp-{}", std::process::id());
    path.with_file_name(temp_name)
}

fn default_mutation_artifact_root(session_path: &Path) -> PathBuf {
    let Some(parent) = session_path.parent() else {
        return PathBuf::from(".sigil-state")
            .join("artifacts")
            .join("mutations");
    };
    let base = if parent.file_name().is_some_and(|name| name == "sessions") {
        let session_base = parent.parent().unwrap_or(parent);
        if session_base
            .file_name()
            .is_some_and(|name| name == ".sigil")
        {
            return default_user_state_mutation_artifact_root();
        }
        session_base
    } else {
        parent
    };
    base.join("artifacts").join("mutations")
}

fn default_user_state_mutation_artifact_root() -> PathBuf {
    user_state_root()
        .unwrap_or_else(|| PathBuf::from(".sigil-state"))
        .join("artifacts")
        .join("mutations")
}

fn user_state_root() -> Option<PathBuf> {
    if let Some(root) = env::var_os("SIGIL_STATE_HOME") {
        return Some(PathBuf::from(root));
    }
    match env::consts::OS {
        "macos" => env::var_os("HOME").map(|home| {
            PathBuf::from(home)
                .join("Library")
                .join("Application Support")
                .join("sigil")
                .join("state")
        }),
        "windows" => env::var_os("LOCALAPPDATA")
            .map(|root| PathBuf::from(root).join("sigil").join("state"))
            .or_else(|| {
                env::var_os("USERPROFILE").map(|home| {
                    PathBuf::from(home)
                        .join("AppData")
                        .join("Local")
                        .join("sigil")
                        .join("state")
                })
            }),
        _ => env::var_os("XDG_STATE_HOME")
            .map(|root| PathBuf::from(root).join("sigil"))
            .or_else(|| {
                env::var_os("HOME").map(|home| {
                    PathBuf::from(home)
                        .join(".local")
                        .join("state")
                        .join("sigil")
                })
            }),
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
struct MutationArtifactMetadata {
    artifact_id: MutationArtifactId,
    content_hash: String,
    size: u64,
    workspace_id_hash: String,
    operation_id: OperationId,
    source_path: PathBuf,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    created_at_ms: Option<u64>,
}

fn snapshot_coverage_for_pre_mutation_content(
    artifact_root: &Path,
    workspace_id: &str,
    operation_id: &str,
    relative_path: &Path,
    absolute_path: &Path,
    before_hash: Option<&str>,
) -> Result<SnapshotCoverage> {
    let Some(before_hash) = before_hash else {
        return Ok(SnapshotCoverage::NoPriorContent);
    };
    if is_sensitive_snapshot_path(relative_path) {
        return Ok(SnapshotCoverage::SkippedSensitive);
    }
    let bytes = fs::read(absolute_path)
        .with_context(|| format!("failed to read {}", absolute_path.display()))?;
    let content_hash = bytes_hash(&bytes);
    if content_hash != before_hash {
        bail!(
            "pre-mutation artifact hash changed while capturing {}",
            absolute_path.display()
        );
    }
    let artifact_id = store_mutation_artifact(
        artifact_root,
        workspace_id,
        operation_id,
        relative_path,
        &bytes,
    )?;
    Ok(SnapshotCoverage::Captured(artifact_id))
}

fn store_mutation_artifact(
    artifact_root: &Path,
    workspace_id: &str,
    operation_id: &str,
    relative_path: &Path,
    bytes: &[u8],
) -> Result<MutationArtifactId> {
    let content_hash = bytes_hash(bytes);
    let workspace_id_hash = short_hash(workspace_id);
    let operation_id_hash = short_hash(operation_id);
    let digest = content_hash
        .strip_prefix("sha256:")
        .unwrap_or(content_hash.as_str())
        .to_owned();
    let artifact_id = format!("mutation-artifact:sha256:{digest}");
    let dir = artifact_root
        .join(&workspace_id_hash)
        .join(&operation_id_hash);
    fs::create_dir_all(&dir).with_context(|| format!("failed to create {}", dir.display()))?;
    harden_artifact_dir(&dir)?;
    let blob_path = dir.join(format!("{digest}.blob"));
    if !artifact_blob_matches(&blob_path, &content_hash)? {
        atomic_write_artifact(&blob_path, bytes)?;
    }
    harden_artifact_file(&blob_path)?;
    let metadata = MutationArtifactMetadata {
        artifact_id: artifact_id.clone(),
        content_hash,
        size: bytes.len() as u64,
        workspace_id_hash,
        operation_id: operation_id.to_owned(),
        source_path: relative_path.to_path_buf(),
        created_at_ms: Some(unix_time_ms()),
    };
    let metadata_path = dir.join(format!("{digest}.json"));
    let metadata_bytes = serde_json::to_vec_pretty(&metadata)
        .context("failed to encode mutation artifact metadata")?;
    let mut metadata_file = File::create(&metadata_path)
        .with_context(|| format!("failed to create {}", metadata_path.display()))?;
    metadata_file
        .write_all(&metadata_bytes)
        .with_context(|| format!("failed to write {}", metadata_path.display()))?;
    metadata_file
        .sync_all()
        .with_context(|| format!("failed to sync {}", metadata_path.display()))?;
    harden_artifact_file(&metadata_path)?;
    let dir_file = File::open(&dir).with_context(|| format!("failed to open {}", dir.display()))?;
    dir_file
        .sync_all()
        .with_context(|| format!("failed to sync {}", dir.display()))?;
    sync_parent(&dir)?;
    Ok(artifact_id)
}

fn read_mutation_artifact_content(
    artifact_root: &Path,
    artifact_id: &MutationArtifactId,
) -> Result<Vec<u8>> {
    let located = locate_mutation_artifacts(artifact_root, artifact_id)?;
    for artifact in located {
        if !artifact.blob_available {
            continue;
        }
        let bytes = fs::read(&artifact.blob_path).with_context(|| {
            format!(
                "failed to read artifact blob {}",
                artifact.blob_path.display()
            )
        })?;
        let content_hash = bytes_hash(&bytes);
        if content_hash != artifact.metadata.content_hash {
            bail!(
                "mutation artifact content hash mismatch for {}",
                artifact.blob_path.display()
            );
        }
        return Ok(bytes);
    }
    bail!("mutation artifact not found: {artifact_id}")
}

#[derive(Debug)]
struct LocatedMutationArtifact {
    metadata: MutationArtifactMetadata,
    metadata_path: PathBuf,
    blob_path: PathBuf,
    blob_available: bool,
}

#[derive(Debug)]
struct MutationArtifactGroup {
    artifact_id: MutationArtifactId,
    size: u64,
    created_at_ms: Option<u64>,
    blob_available: bool,
    workspace_id_hashes: Vec<String>,
    operation_ids: Vec<OperationId>,
    source_paths: Vec<PathBuf>,
}

fn select_artifacts_for_cleanup(
    artifact_root: &Path,
    session_log_path: &Path,
    target: &MutationArtifactCleanupTarget,
    policy: &MutationArtifactRetentionPolicy,
    now_ms: u64,
) -> Result<MutationArtifactRetentionSelection> {
    let mut groups = scan_mutation_artifact_groups(artifact_root)?;
    groups.sort_by(|left, right| {
        left.created_at_ms
            .cmp(&right.created_at_ms)
            .then_with(|| left.artifact_id.cmp(&right.artifact_id))
    });

    match target {
        MutationArtifactCleanupTarget::Recommended => {
            Ok(select_recommended_artifacts(groups, policy, now_ms))
        }
        MutationArtifactCleanupTarget::Expired => {
            Ok(select_expired_artifacts(groups, policy, now_ms))
        }
        MutationArtifactCleanupTarget::Unavailable => Ok(select_unavailable_artifacts(groups)),
        MutationArtifactCleanupTarget::Unreferenced => {
            select_unreferenced_artifacts(groups, session_log_path)
        }
        MutationArtifactCleanupTarget::Workspace(workspace_id) => {
            Ok(select_workspace_artifacts(groups, workspace_id))
        }
    }
}

fn select_recommended_artifacts(
    groups: Vec<MutationArtifactGroup>,
    policy: &MutationArtifactRetentionPolicy,
    now_ms: u64,
) -> MutationArtifactRetentionSelection {
    let mut selected = Vec::<MutationArtifactCleanupSelection>::new();
    let mut selected_ids = BTreeSet::<MutationArtifactId>::new();
    for artifact in &groups {
        if !artifact.blob_available {
            if selected_ids.insert(artifact.artifact_id.clone()) {
                selected.push(MutationArtifactCleanupSelection {
                    artifact_id: artifact.artifact_id.clone(),
                    requested_status: MutationArtifactLifecycleStatus::Expired,
                    reason: "retention scan found unavailable content",
                });
            }
            continue;
        }
        if policy.expire_older_than_ms.is_some_and(|limit| {
            artifact
                .created_at_ms
                .is_some_and(|created_at| now_ms.saturating_sub(created_at) >= limit)
        }) && selected_ids.insert(artifact.artifact_id.clone())
        {
            selected.push(MutationArtifactCleanupSelection {
                artifact_id: artifact.artifact_id.clone(),
                requested_status: MutationArtifactLifecycleStatus::Expired,
                reason: "retention age limit",
            });
        }
    }

    let mut remaining_count = groups
        .iter()
        .filter(|artifact| !selected_ids.contains(&artifact.artifact_id))
        .count();
    let mut remaining_bytes = groups
        .iter()
        .filter(|artifact| !selected_ids.contains(&artifact.artifact_id))
        .fold(0_u64, |total, artifact| total.saturating_add(artifact.size));

    for artifact in &groups {
        if selected_ids.contains(&artifact.artifact_id) {
            continue;
        }
        let exceeds_count = policy
            .max_artifacts
            .is_some_and(|max_artifacts| remaining_count > max_artifacts);
        let exceeds_bytes = policy
            .max_bytes
            .is_some_and(|max_bytes| remaining_bytes > max_bytes);
        if !exceeds_count && !exceeds_bytes {
            continue;
        }
        selected_ids.insert(artifact.artifact_id.clone());
        selected.push(MutationArtifactCleanupSelection {
            artifact_id: artifact.artifact_id.clone(),
            requested_status: MutationArtifactLifecycleStatus::Expired,
            reason: "retention quota limit",
        });
        remaining_count = remaining_count.saturating_sub(1);
        remaining_bytes = remaining_bytes.saturating_sub(artifact.size);
    }

    MutationArtifactRetentionSelection { groups, selected }
}

fn select_expired_artifacts(
    groups: Vec<MutationArtifactGroup>,
    policy: &MutationArtifactRetentionPolicy,
    now_ms: u64,
) -> MutationArtifactRetentionSelection {
    let mut selected = Vec::<MutationArtifactCleanupSelection>::new();
    let mut selected_ids = BTreeSet::<MutationArtifactId>::new();
    for artifact in &groups {
        if artifact.blob_available
            && policy.expire_older_than_ms.is_some_and(|limit| {
                artifact
                    .created_at_ms
                    .is_some_and(|created_at| now_ms.saturating_sub(created_at) >= limit)
            })
            && selected_ids.insert(artifact.artifact_id.clone())
        {
            selected.push(MutationArtifactCleanupSelection {
                artifact_id: artifact.artifact_id.clone(),
                requested_status: MutationArtifactLifecycleStatus::Expired,
                reason: "retention age limit",
            });
        }
    }

    let mut remaining_count = groups
        .iter()
        .filter(|artifact| !selected_ids.contains(&artifact.artifact_id))
        .count();
    let mut remaining_bytes = groups
        .iter()
        .filter(|artifact| !selected_ids.contains(&artifact.artifact_id))
        .fold(0_u64, |total, artifact| total.saturating_add(artifact.size));

    for artifact in &groups {
        if selected_ids.contains(&artifact.artifact_id) {
            continue;
        }
        let exceeds_count = policy
            .max_artifacts
            .is_some_and(|max_artifacts| remaining_count > max_artifacts);
        let exceeds_bytes = policy
            .max_bytes
            .is_some_and(|max_bytes| remaining_bytes > max_bytes);
        if !artifact.blob_available || (!exceeds_count && !exceeds_bytes) {
            continue;
        }
        selected_ids.insert(artifact.artifact_id.clone());
        selected.push(MutationArtifactCleanupSelection {
            artifact_id: artifact.artifact_id.clone(),
            requested_status: MutationArtifactLifecycleStatus::Expired,
            reason: "retention quota limit",
        });
        remaining_count = remaining_count.saturating_sub(1);
        remaining_bytes = remaining_bytes.saturating_sub(artifact.size);
    }

    MutationArtifactRetentionSelection { groups, selected }
}

fn select_unavailable_artifacts(
    groups: Vec<MutationArtifactGroup>,
) -> MutationArtifactRetentionSelection {
    let selected = groups
        .iter()
        .filter(|artifact| !artifact.blob_available)
        .map(|artifact| MutationArtifactCleanupSelection {
            artifact_id: artifact.artifact_id.clone(),
            requested_status: MutationArtifactLifecycleStatus::Unavailable,
            reason: "artifact content unavailable",
        })
        .collect();
    MutationArtifactRetentionSelection { groups, selected }
}

fn select_unreferenced_artifacts(
    groups: Vec<MutationArtifactGroup>,
    session_log_path: &Path,
) -> Result<MutationArtifactRetentionSelection> {
    let referenced_artifacts = referenced_mutation_artifact_ids(session_log_path)?;
    let selected = groups
        .iter()
        .filter(|artifact| !referenced_artifacts.contains(&artifact.artifact_id))
        .map(|artifact| MutationArtifactCleanupSelection {
            artifact_id: artifact.artifact_id.clone(),
            requested_status: MutationArtifactLifecycleStatus::Deleted,
            reason: "artifact metadata is not referenced by session events",
        })
        .collect();
    Ok(MutationArtifactRetentionSelection { groups, selected })
}

fn select_workspace_artifacts(
    groups: Vec<MutationArtifactGroup>,
    workspace_id: &WorkspaceId,
) -> MutationArtifactRetentionSelection {
    let workspace_id_hash = short_hash(workspace_id);
    let selected = groups
        .iter()
        .filter(|artifact| artifact.workspace_id_hashes.contains(&workspace_id_hash))
        .map(|artifact| MutationArtifactCleanupSelection {
            artifact_id: artifact.artifact_id.clone(),
            requested_status: MutationArtifactLifecycleStatus::Deleted,
            reason: "user requested workspace artifact cleanup",
        })
        .collect();
    MutationArtifactRetentionSelection { groups, selected }
}

fn retention_report_from_selection(
    selection: &MutationArtifactRetentionSelection,
) -> MutationArtifactRetentionReport {
    let artifact_groups = selection
        .groups
        .iter()
        .map(|artifact| (artifact.artifact_id.clone(), artifact))
        .collect::<BTreeMap<_, _>>();
    let mut report = MutationArtifactRetentionReport {
        scanned_artifacts: selection.groups.len(),
        scanned_bytes: selection
            .groups
            .iter()
            .fold(0_u64, |total, artifact| total.saturating_add(artifact.size)),
        ..MutationArtifactRetentionReport::default()
    };
    for selection in &selection.selected {
        let Some(group) = artifact_groups.get(&selection.artifact_id) else {
            continue;
        };
        let status = effective_artifact_lifecycle_status(selection.requested_status, group);
        update_artifact_cleanup_report_counts(&mut report, status, group.size);
    }
    report
}

fn effective_artifact_lifecycle_status(
    requested_status: MutationArtifactLifecycleStatus,
    artifact: &MutationArtifactGroup,
) -> MutationArtifactLifecycleStatus {
    if artifact.blob_available {
        requested_status
    } else {
        MutationArtifactLifecycleStatus::Unavailable
    }
}

fn update_artifact_cleanup_report_counts(
    report: &mut MutationArtifactRetentionReport,
    status: MutationArtifactLifecycleStatus,
    size: u64,
) {
    match status {
        MutationArtifactLifecycleStatus::Deleted => {
            report.deleted_artifacts += 1;
            report.deleted_bytes = report.deleted_bytes.saturating_add(size);
        }
        MutationArtifactLifecycleStatus::Expired => {
            report.expired_artifacts += 1;
            report.expired_bytes = report.expired_bytes.saturating_add(size);
        }
        MutationArtifactLifecycleStatus::Unavailable => {
            report.unavailable_artifacts += 1;
        }
    }
}

fn referenced_mutation_artifact_ids(
    session_log_path: &Path,
) -> Result<BTreeSet<MutationArtifactId>> {
    let mut artifact_ids = BTreeSet::new();
    for record in JsonlSessionStore::read_event_records(session_log_path)? {
        let crate::SessionStreamRecord::Stored(event) = record else {
            continue;
        };
        if event.event_type != DurableEventType::MutationPrepared.as_str() {
            continue;
        }
        let payload =
            serde_json::from_value::<MutationPrepared>(event.payload).with_context(|| {
                format!(
                    "failed to decode {}",
                    DurableEventType::MutationPrepared.as_str()
                )
            })?;
        if let SnapshotCoverage::Captured(artifact_id) = payload.snapshot_coverage {
            artifact_ids.insert(artifact_id);
        }
    }
    Ok(artifact_ids)
}

fn scan_mutation_artifact_groups(artifact_root: &Path) -> Result<Vec<MutationArtifactGroup>> {
    let mut by_id = BTreeMap::<MutationArtifactId, Vec<LocatedMutationArtifact>>::new();
    for artifact in scan_mutation_artifacts(artifact_root)? {
        by_id
            .entry(artifact.metadata.artifact_id.clone())
            .or_default()
            .push(artifact);
    }
    let mut groups = Vec::with_capacity(by_id.len());
    for (artifact_id, located) in by_id {
        let size = located.iter().fold(0_u64, |total, artifact| {
            total.saturating_add(artifact.metadata.size)
        });
        let created_at_ms = located
            .iter()
            .filter_map(|artifact| {
                artifact
                    .metadata
                    .created_at_ms
                    .or_else(|| file_modified_ms(&artifact.metadata_path))
            })
            .min();
        let blob_available = located.iter().any(|artifact| artifact.blob_available);
        let mut operation_ids = located
            .iter()
            .map(|artifact| artifact.metadata.operation_id.clone())
            .collect::<Vec<_>>();
        operation_ids.sort();
        operation_ids.dedup();
        let mut source_paths = located
            .iter()
            .map(|artifact| artifact.metadata.source_path.clone())
            .collect::<Vec<_>>();
        source_paths.sort();
        source_paths.dedup();
        let mut workspace_id_hashes = located
            .iter()
            .map(|artifact| artifact.metadata.workspace_id_hash.clone())
            .collect::<Vec<_>>();
        workspace_id_hashes.sort();
        workspace_id_hashes.dedup();
        groups.push(MutationArtifactGroup {
            artifact_id,
            size,
            created_at_ms,
            blob_available,
            workspace_id_hashes,
            operation_ids,
            source_paths,
        });
    }
    Ok(groups)
}

fn scan_mutation_artifacts(artifact_root: &Path) -> Result<Vec<LocatedMutationArtifact>> {
    let mut located = Vec::new();
    if !artifact_root.exists() {
        return Ok(located);
    }
    let mut pending = vec![artifact_root.to_path_buf()];
    let mut visited = BTreeSet::<PathBuf>::new();
    while let Some(dir) = pending.pop() {
        if !visited.insert(dir.clone()) {
            continue;
        }
        let entries =
            fs::read_dir(&dir).with_context(|| format!("failed to read {}", dir.display()))?;
        for entry in entries {
            let entry = entry.with_context(|| format!("failed to read {}", dir.display()))?;
            let path = entry.path();
            if path.is_dir() {
                pending.push(path);
                continue;
            }
            if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
                continue;
            }
            let metadata = read_mutation_artifact_metadata(&path)?;
            let digest = mutation_artifact_digest(&metadata.artifact_id)?;
            let blob_path = path.with_file_name(format!("{digest}.blob"));
            let blob_available = artifact_blob_matches(&blob_path, &metadata.content_hash)?;
            located.push(LocatedMutationArtifact {
                metadata,
                metadata_path: path,
                blob_path,
                blob_available,
            });
        }
    }
    Ok(located)
}

fn locate_mutation_artifacts(
    artifact_root: &Path,
    artifact_id: &MutationArtifactId,
) -> Result<Vec<LocatedMutationArtifact>> {
    let digest = mutation_artifact_digest(artifact_id)?;
    let metadata_name = format!("{digest}.json");
    let blob_name = format!("{digest}.blob");
    let mut located = Vec::new();
    let mut pending = vec![artifact_root.to_path_buf()];
    while let Some(dir) = pending.pop() {
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries {
            let entry = entry.with_context(|| format!("failed to read {}", dir.display()))?;
            let path = entry.path();
            if path.is_dir() {
                pending.push(path);
                continue;
            }
            if path.file_name().and_then(|name| name.to_str()) != Some(metadata_name.as_str()) {
                continue;
            }
            let metadata = read_mutation_artifact_metadata(&path)?;
            if metadata.artifact_id != *artifact_id {
                continue;
            }
            let blob_path = path.with_file_name(&blob_name);
            let blob_available = artifact_blob_matches(&blob_path, &metadata.content_hash)?;
            located.push(LocatedMutationArtifact {
                metadata,
                metadata_path: path,
                blob_path,
                blob_available,
            });
        }
    }
    Ok(located)
}

fn read_mutation_artifact_metadata(path: &Path) -> Result<MutationArtifactMetadata> {
    let metadata_bytes = fs::read(path)
        .with_context(|| format!("failed to read artifact metadata {}", path.display()))?;
    serde_json::from_slice(&metadata_bytes)
        .with_context(|| format!("failed to decode artifact metadata {}", path.display()))
}

fn mutation_artifact_digest(artifact_id: &MutationArtifactId) -> Result<&str> {
    artifact_id
        .strip_prefix("mutation-artifact:sha256:")
        .ok_or_else(|| anyhow!("unsupported mutation artifact id: {artifact_id}"))
}

fn short_hash(value: &str) -> String {
    let digest = Sha256::digest(value.as_bytes());
    format!("{digest:x}").chars().take(16).collect()
}

fn unix_time_ms() -> u64 {
    system_time_to_unix_ms(SystemTime::now()).unwrap_or(0)
}

fn file_modified_ms(path: &Path) -> Option<u64> {
    fs::metadata(path)
        .ok()
        .and_then(|metadata| metadata.modified().ok())
        .and_then(system_time_to_unix_ms)
}

fn system_time_to_unix_ms(time: SystemTime) -> Option<u64> {
    let duration = time.duration_since(UNIX_EPOCH).ok()?;
    Some(
        duration
            .as_secs()
            .saturating_mul(1_000)
            .saturating_add(u64::from(duration.subsec_millis())),
    )
}

fn artifact_blob_matches(path: &Path, expected_hash: &str) -> Result<bool> {
    match fs::read(path) {
        Ok(bytes) => Ok(bytes_hash(&bytes) == expected_hash),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error).with_context(|| format!("failed to read {}", path.display())),
    }
}

fn atomic_write_artifact(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| anyhow!("artifact path has no parent: {}", path.display()))?;
    let temp_path = temp_path_for(path);
    {
        let mut temp_file = File::create(&temp_path)
            .with_context(|| format!("failed to create {}", temp_path.display()))?;
        temp_file
            .write_all(bytes)
            .with_context(|| format!("failed to write {}", temp_path.display()))?;
        temp_file
            .sync_all()
            .with_context(|| format!("failed to sync {}", temp_path.display()))?;
    }
    fs::rename(&temp_path, path).with_context(|| {
        format!(
            "failed to atomically replace artifact {} with {}",
            path.display(),
            temp_path.display()
        )
    })?;
    let file = File::open(path).with_context(|| format!("failed to open {}", path.display()))?;
    file.sync_all()
        .with_context(|| format!("failed to sync {}", path.display()))?;
    let parent_file =
        File::open(parent).with_context(|| format!("failed to open {}", parent.display()))?;
    parent_file
        .sync_all()
        .with_context(|| format!("failed to sync {}", parent.display()))
}

fn remove_file_if_exists(path: &Path) -> Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).with_context(|| format!("failed to remove {}", path.display())),
    }
}

fn sync_existing_dir(path: &Path) -> Result<()> {
    match File::open(path) {
        Ok(file) => file
            .sync_all()
            .with_context(|| format!("failed to sync {}", path.display())),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).with_context(|| format!("failed to open {}", path.display())),
    }
}

#[cfg(unix)]
fn harden_artifact_dir(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;

    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
        .with_context(|| format!("failed to set permissions on {}", path.display()))
}

#[cfg(not(unix))]
fn harden_artifact_dir(_path: &Path) -> Result<()> {
    Ok(())
}

#[cfg(unix)]
fn harden_artifact_file(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;

    fs::set_permissions(path, fs::Permissions::from_mode(0o600))
        .with_context(|| format!("failed to set permissions on {}", path.display()))
}

#[cfg(not(unix))]
fn harden_artifact_file(_path: &Path) -> Result<()> {
    Ok(())
}

fn is_sensitive_snapshot_path(path: &Path) -> bool {
    let components = path
        .components()
        .filter_map(|component| match component {
            std::path::Component::Normal(value) => value.to_str(),
            _ => None,
        })
        .map(|value| value.to_ascii_lowercase())
        .collect::<Vec<_>>();
    let Some(file_name) = components.last() else {
        return false;
    };
    const SENSITIVE_FILE_NAMES: &[&str] = &[
        ".env",
        ".netrc",
        ".npmrc",
        ".pypirc",
        ".yarnrc",
        "credentials",
        "credentials.json",
        "service-account.json",
        "service_account.json",
        "known_hosts",
        "config",
        "id_rsa",
        "id_dsa",
        "id_ecdsa",
        "id_ed25519",
    ];
    const SENSITIVE_NAME_PARTS: &[&str] = &[
        "api_key",
        "apikey",
        "auth",
        "credential",
        "oauth",
        "password",
        "private_key",
        "secret",
        "service-account",
        "service_account",
        "token",
    ];
    file_name == ".env"
        || file_name.starts_with(".env.")
        || SENSITIVE_FILE_NAMES.contains(&file_name.as_str())
        || file_name.ends_with(".pem")
        || file_name.ends_with(".key")
        || SENSITIVE_NAME_PARTS
            .iter()
            .any(|part| file_name.contains(part))
        || components
            .iter()
            .any(|component| matches!(component.as_str(), ".ssh" | ".aws" | ".azure" | ".gnupg"))
        || components
            .windows(2)
            .any(|pair| pair[0] == ".config" && pair[1] == "gcloud")
}

fn operation_id_for(
    workspace_id: &str,
    tool_call_id: &str,
    batch_id: Option<&str>,
    relative_path: &Path,
    before_hash: Option<&str>,
    intended_after_hash: Option<&str>,
) -> OperationId {
    stable_event_uuid(
        "sigil-mutation-operation",
        &format!(
            "{workspace_id}:{tool_call_id}:{:?}:{}:{:?}:{:?}",
            batch_id,
            relative_path.display(),
            before_hash,
            intended_after_hash
        ),
    )
}

fn latest_workspace_revision(
    store: &JsonlSessionStore,
    workspace_id: &str,
) -> Result<WorkspaceRevision> {
    let mut latest = 0;
    for record in JsonlSessionStore::read_event_records(store.path())? {
        let crate::SessionStreamRecord::Stored(event) = record else {
            continue;
        };
        if event.event_type == DurableEventType::MutationCommitted.as_str() {
            if let Ok(payload) = serde_json::from_value::<MutationCommitted>(event.payload.clone())
                && payload.workspace_id.as_deref().unwrap_or(workspace_id) == workspace_id
            {
                latest = latest.max(payload.workspace_revision);
            }
        } else if event.event_type == DurableEventType::MutationReconciled.as_str()
            && let Ok(payload) = serde_json::from_value::<MutationReconciled>(event.payload.clone())
            && payload.workspace_revision.is_some()
        {
            latest = latest.max(payload.workspace_revision.unwrap_or_default());
        } else if event.event_type == DurableEventType::MutationPrepared.as_str()
            && let Ok(payload) = serde_json::from_value::<MutationPrepared>(event.payload.clone())
            && payload.workspace_id == workspace_id
        {
            latest = latest.max(payload.base_workspace_revision);
        } else if event.event_type == DurableEventType::WorkspaceMutationDetected.as_str()
            && let Ok(payload) =
                serde_json::from_value::<WorkspaceMutationDetected>(event.payload.clone())
            && payload.workspace_id == workspace_id
        {
            latest = latest.max(payload.workspace_revision);
        }
    }
    Ok(latest)
}

fn single_subject_snapshot_id(
    workspace_id: &str,
    relative_path: &Path,
    file_type: FileType,
    observed_hash: Option<String>,
) -> Result<WorkspaceSnapshotId> {
    let state = match file_type {
        FileType::File if observed_hash.is_some() => SnapshotEntryState::Present,
        FileType::File => SnapshotEntryState::Missing,
        FileType::Directory if observed_hash.is_some() => SnapshotEntryState::Present,
        FileType::Directory => SnapshotEntryState::Missing,
        FileType::Symlink | FileType::Other => SnapshotEntryState::Unsupported,
    };
    let content_hash = if file_type == FileType::File {
        observed_hash
    } else {
        None
    };
    let manifest = WorkspaceSnapshotManifestV1 {
        workspace_id: workspace_id.to_owned(),
        scope_hash: mutation_scope_hash(relative_path),
        entries: vec![WorkspaceSnapshotEntry {
            normalized_path: relative_path.to_path_buf(),
            file_type,
            content_hash,
            mode: None,
            file_metadata: None,
            symlink_target: None,
            state,
        }],
    };
    manifest.workspace_snapshot_id()
}

fn mutation_scope_hash(relative_path: &Path) -> VerificationScopeHash {
    let digest = Sha256::digest(relative_path.to_string_lossy().as_bytes());
    format!("mutation-scope:sha256:{digest:x}")
}

fn normalize_relative_path(path: PathBuf) -> Result<PathBuf> {
    if path.is_absolute() {
        bail!(
            "mutation path must be workspace-relative: {}",
            path.display()
        );
    }
    if path.components().any(|component| {
        matches!(
            component,
            std::path::Component::ParentDir
                | std::path::Component::RootDir
                | std::path::Component::Prefix(_)
        )
    }) {
        bail!(
            "mutation path must not escape workspace: {}",
            path.display()
        );
    }
    Ok(path.components().collect())
}

fn ensure_absolute_path_matches_subject(
    workspace_root: &Path,
    relative_path: &Path,
    absolute_path: &Path,
) -> Result<()> {
    if !absolute_path.is_absolute() {
        bail!(
            "mutation absolute path must be absolute: {}",
            absolute_path.display()
        );
    }
    let expected = lexically_normalize_path(&workspace_root.join(relative_path))?;
    let actual = normalize_absolute_path_for_subject(absolute_path)?;
    if actual != expected {
        bail!(
            "mutation target {} does not match workspace subject {}",
            actual.display(),
            relative_path.display()
        );
    }
    Ok(())
}

fn normalize_absolute_path_for_subject(path: &Path) -> Result<PathBuf> {
    if !path.is_absolute() {
        bail!("path must be absolute: {}", path.display());
    }
    if let Ok(canonical) = fs::canonicalize(path) {
        return lexically_normalize_path(&canonical);
    }
    let mut missing_components = Vec::<OsString>::new();
    let mut cursor = path;
    loop {
        if let Ok(canonical) = fs::canonicalize(cursor) {
            let mut normalized = canonical;
            for component in missing_components.iter().rev() {
                normalized.push(component);
            }
            return lexically_normalize_path(&normalized);
        }
        let file_name = cursor
            .file_name()
            .ok_or_else(|| anyhow!("failed to resolve absolute path root: {}", path.display()))?;
        missing_components.push(file_name.to_os_string());
        cursor = cursor
            .parent()
            .ok_or_else(|| anyhow!("failed to resolve absolute path parent: {}", path.display()))?;
    }
}

fn lexically_normalize_path(path: &Path) -> Result<PathBuf> {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                if !normalized.pop() {
                    bail!("path normalization would escape root: {}", path.display());
                }
            }
            std::path::Component::Normal(part) => normalized.push(part),
            std::path::Component::RootDir | std::path::Component::Prefix(_) => {
                normalized.push(component.as_os_str());
            }
        }
    }
    Ok(normalized)
}

fn workspace_scan_from_snapshot(
    workspace_id: WorkspaceId,
    scope: VerificationScope,
    workspace_revision: WorkspaceRevision,
    snapshot: WorkspaceSnapshotBuild,
) -> WorkspaceMutationScan {
    WorkspaceMutationScan {
        workspace_id,
        scope_hash: scope.scope_hash.clone(),
        scope,
        workspace_revision,
        workspace_snapshot_id: snapshot.workspace_snapshot_id,
        workspace_knowledge: snapshot.workspace_knowledge,
    }
}

fn workspace_mutation_detection_reason(
    before: &WorkspaceMutationScan,
    after: &WorkspaceMutationScan,
) -> Option<WorkspaceMutationDetectionReason> {
    if before.workspace_knowledge.is_unknown_dirty() {
        return Some(WorkspaceMutationDetectionReason::SnapshotIncompleteBefore);
    }
    if after.workspace_knowledge.is_unknown_dirty() {
        return Some(WorkspaceMutationDetectionReason::SnapshotIncompleteAfter);
    }
    (before.workspace_snapshot_id != after.workspace_snapshot_id)
        .then_some(WorkspaceMutationDetectionReason::SnapshotChanged)
}

fn detection_already_covers_after_scan(
    detection: &WorkspaceMutationDetected,
    after: &WorkspaceMutationScan,
) -> bool {
    detection.to_workspace_snapshot_id == after.workspace_snapshot_id
        && detection.unknown_dirty == after.workspace_knowledge.is_unknown_dirty()
}

fn workspace_detection_operation_id(
    workspace_id: &str,
    tool_call_id: &str,
    before_snapshot_id: Option<&str>,
    after_snapshot_id: Option<&str>,
    reason: WorkspaceMutationDetectionReason,
) -> OperationId {
    stable_event_uuid(
        "sigil-workspace-mutation-detected",
        &format!(
            "{workspace_id}:{tool_call_id}:{before_snapshot_id:?}:{after_snapshot_id:?}:{reason:?}"
        ),
    )
}

#[cfg(test)]
#[path = "tests/mutation_tests.rs"]
mod tests;

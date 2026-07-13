use std::{collections::BTreeMap, path::PathBuf};

use serde::{Deserialize, Serialize};

use crate::{
    EventId, StoredEvent,
    verification::{
        FileType, ToolEffect, VerificationScope, VerificationScopeHash, WorkspaceId,
        WorkspaceKnowledge, WorkspaceRevision, WorkspaceSnapshotId,
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
    /// In-process apply failed and every already-attempted file was restored by CAS.
    RolledBack,
    /// At least one compensating restore failed; residual workspace changes may remain.
    RollbackFailed,
}

/// Durable start payload for one multi-subject mutation batch.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct MutationBatchStarted {
    pub batch_id: MutationBatchId,
    pub operation_id: OperationId,
    pub expected_subjects: Vec<MutationSubject>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prepared_digest: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approval_identity: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub policy_fingerprint: Option<String>,
}

/// Durable terminal payload for one multi-subject mutation batch.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct MutationBatchFinished {
    pub batch_id: MutationBatchId,
    pub status: MutationBatchStatus,
    pub committed_operations: Vec<OperationId>,
    pub failed_operations: Vec<OperationId>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub rollback_operations: Vec<OperationId>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub rollback_failed_operations: Vec<OperationId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prepared_digest: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approval_identity: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub policy_fingerprint: Option<String>,
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
    pub snapshot_coverage: SnapshotCoverage,
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

/// Why an exact checkpoint restore was rejected before its first workspace write.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CheckpointRestoreConflictReason {
    WorkspaceMismatch,
    CurrentHashMismatch,
    ArtifactUnavailable,
    SensitiveSnapshot,
    UnsupportedSnapshot,
    InvalidBinding,
}

/// Durable evidence that one explicit restore could not pass its exact preflight.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct CheckpointRestoreConflict {
    pub checkpoint_id: String,
    pub checkpoint_digest: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<PathBuf>,
    pub reason: CheckpointRestoreConflictReason,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_current_hash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actual_current_hash: Option<String>,
}

/// Result of one checkpoint restore mutation.
#[derive(Debug, Clone, PartialEq)]
pub struct RestoredFileMutation {
    pub committed: CommittedFileMutation,
    pub checkpoint_event: StoredEvent,
    pub restored_from: SnapshotCoverage,
}

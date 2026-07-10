//! RFC-0002 crash-consistent mutation protocol foundation.
//!
//! This module covers controlled file mutations and the first durable evidence path for
//! workspace mutations caused by unknown-effect executions. Full shell, MCP, plugin and
//! persistent terminal lifecycle coverage remains staged by RFC-0002.

mod artifacts;
mod coordinator;
mod events;
mod hash;
mod ops;
mod recorder;
mod retention;
mod scan;

use artifacts::{
    MutationArtifactGroup, default_mutation_artifact_root, locate_mutation_artifacts,
    read_mutation_artifact_content, scan_mutation_artifact_groups,
    snapshot_coverage_for_pre_mutation_content,
};
use hash::{
    artifact_blob_matches, atomic_replace, atomic_write_artifact, compare_current_directory_hash,
    compare_current_hash, directory_present_hash, directory_state_hash, ensure_empty_directory,
    ensure_observed_after_hash_matches_intent, file_modified_ms, harden_artifact_dir,
    harden_artifact_file, remove_file_if_exists, short_hash, sync_existing_dir, sync_parent,
    unix_time_ms,
};
use ops::{
    ensure_absolute_path_matches_subject, normalize_absolute_path_for_subject,
    normalize_relative_path, operation_id_for,
};
use scan::{latest_workspace_revision, single_subject_snapshot_id};

#[cfg(test)]
use hash::atomic_replace_error_message;

pub use artifacts::{MutationArtifactLifecycleRecorded, MutationArtifactLifecycleStatus};
pub use coordinator::MutationCoordinator;
pub use events::{
    CheckpointRestored, CommittedDirectoryMutation, CommittedFileMutation,
    ExecutionMutationProfile, MutationArtifactId, MutationBatchFinished, MutationBatchId,
    MutationBatchStarted, MutationBatchStatus, MutationCommitted, MutationObservedState,
    MutationPrepared, MutationReconciled, MutationResolution, MutationSubject, MutationSyncClass,
    OperationId, PreparedDirectoryMutation, PreparedFileMutation, RestoredFileMutation,
    SnapshotCoverage, ToolCallId, WorkspaceMutationDetected, WorkspaceMutationDetectionReason,
    WorkspaceMutationScan,
};
pub use hash::{bytes_hash, file_content_hash};
pub use ops::{
    create_directory_with_mutation, delete_directory_with_mutation, delete_file_with_mutation,
    delete_file_with_mutation_in_batch, restore_file_from_snapshot_with_mutation,
    write_file_with_mutation, write_file_with_mutation_in_batch,
};
pub use recorder::MutationEventRecorder;
pub use retention::{
    MutationArtifactCleanupRequested, MutationArtifactCleanupTarget, MutationArtifactInventoryItem,
    MutationArtifactRetentionPolicy, MutationArtifactRetentionReport,
};

#[cfg(test)]
#[path = "tests/mutation_tests.rs"]
mod tests;

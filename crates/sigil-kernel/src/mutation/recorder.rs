use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};
use serde_json::json;

use crate::{
    DurableEventType, EventClass, ExtensionProcessLifecycleAudit, JsonlSessionStore, StoredEvent,
    verification::stable_workspace_id,
};

use super::{
    CheckpointRestored, CommittedFileMutation, MutationArtifactCleanupRequested,
    MutationArtifactLifecycleRecorded, MutationBatchId, MutationBatchStatus, MutationCommitted,
    MutationCoordinator, MutationPrepared, MutationReconciled, MutationSubject, OperationId,
    ToolCallId, WorkspaceMutationDetected, default_mutation_artifact_root,
};

/// Store-backed durable mutation recorder.
#[derive(Debug, Clone)]
pub struct MutationEventRecorder {
    pub(super) store: JsonlSessionStore,
    pub(super) artifact_root: PathBuf,
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

    /// Appends a neutral extension-process lifecycle audit without marking the workspace dirty.
    ///
    /// This store-backed bridge is used by current extension launchers; RFC-0021 E21.3 will
    /// replace the wider recorder plumbing with a dedicated linear durable writer.
    ///
    /// # Errors
    ///
    /// Returns an error when the payload cannot be serialized or the session store cannot append
    /// and synchronize the recovery-critical event.
    pub fn append_extension_process_lifecycle(
        &self,
        payload: &ExtensionProcessLifecycleAudit,
    ) -> Result<StoredEvent> {
        self.store.append_event(
            DurableEventType::ExtensionProcessLifecycleRecorded,
            EventClass::Critical,
            serde_json::to_value(payload)
                .context("failed to encode extension process lifecycle payload")?,
        )
    }
}

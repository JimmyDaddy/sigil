#[cfg(feature = "test-support")]
use std::{
    collections::BTreeMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
};
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    thread,
    time::Duration,
};

use anyhow::{Context, Result, bail};
use fs2::FileExt;
use serde_json::json;

use crate::{
    DurableEventType, EventClass, ExtensionProcessLifecycleAudit, JsonlSessionStore, StoredEvent,
    verification::stable_workspace_id,
};

use super::{
    CheckpointRestored, CommittedFileMutation, MutationArtifactCleanupRequested,
    MutationArtifactLifecycleRecorded, MutationBatchFinished, MutationBatchId,
    MutationBatchStarted, MutationBatchStatus, MutationCommitted, MutationCoordinator,
    MutationPrepared, MutationReconciled, MutationSubject, OperationId, ToolCallId,
    WorkspaceMutationDetected, default_mutation_artifact_root, harden_artifact_dir,
    harden_artifact_file, latest_workspace_revision,
};

const WORKSPACE_MUTATION_LEASE_ATTEMPTS: usize = 500;
const WORKSPACE_MUTATION_LEASE_RETRY_DELAY: Duration = Duration::from_millis(10);

/// Cross-process exclusive lease covering one workspace controlled-mutation critical section.
#[derive(Debug)]
pub(super) struct WorkspaceMutationLease {
    file: File,
}

impl WorkspaceMutationLease {
    pub(super) fn epoch(&self) -> Result<u64> {
        let mut file = &self.file;
        file.seek(SeekFrom::Start(0))?;
        let mut value = String::new();
        file.read_to_string(&mut value)?;
        if value.trim().is_empty() {
            return Ok(0);
        }
        value
            .trim()
            .parse::<u64>()
            .context("workspace mutation lease contains an invalid epoch")
    }

    pub(super) fn advance_epoch(&self) -> Result<u64> {
        let next = self.epoch()?.saturating_add(1);
        let mut file = &self.file;
        file.set_len(0)?;
        file.seek(SeekFrom::Start(0))?;
        writeln!(file, "{next}")?;
        file.sync_all()?;
        Ok(next)
    }
}

impl Drop for WorkspaceMutationLease {
    fn drop(&mut self) {
        let _ = FileExt::unlock(&self.file);
    }
}

/// Store-backed durable mutation recorder.
#[derive(Debug, Clone)]
pub struct MutationEventRecorder {
    pub(super) store: JsonlSessionStore,
    pub(super) artifact_root: PathBuf,
    workspace_lease_root: PathBuf,
    #[cfg(feature = "test-support")]
    commit_write_faults: Arc<Mutex<BTreeMap<usize, bool>>>,
    #[cfg(feature = "test-support")]
    commit_write_calls: Arc<AtomicUsize>,
}

impl MutationEventRecorder {
    #[must_use]
    pub fn new(store: JsonlSessionStore) -> Self {
        let artifact_root = default_mutation_artifact_root(store.path());
        let workspace_lease_root = workspace_lease_root_for_artifacts(&artifact_root);
        Self {
            store,
            artifact_root,
            workspace_lease_root,
            #[cfg(feature = "test-support")]
            commit_write_faults: Arc::new(Mutex::new(BTreeMap::new())),
            #[cfg(feature = "test-support")]
            commit_write_calls: Arc::new(AtomicUsize::new(0)),
        }
    }

    /// Creates a recorder with an explicit mutation artifact root.
    ///
    /// This is primarily used by tests and entrypoints that already resolved the user state
    /// directory. The root must not point inside the workspace repository.
    #[must_use]
    pub fn with_artifact_root(store: JsonlSessionStore, artifact_root: impl Into<PathBuf>) -> Self {
        let artifact_root = artifact_root.into();
        let workspace_lease_root = workspace_lease_root_for_artifacts(&artifact_root);
        Self {
            store,
            artifact_root,
            workspace_lease_root,
            #[cfg(feature = "test-support")]
            commit_write_faults: Arc::new(Mutex::new(BTreeMap::new())),
            #[cfg(feature = "test-support")]
            commit_write_calls: Arc::new(AtomicUsize::new(0)),
        }
    }

    /// Installs a deterministic commit fault for cross-crate transaction tests.
    ///
    /// `call_index` is one-based. `after_write` fails after atomic replacement but before durable
    /// commit; `false` fails before touching the target.
    #[cfg(feature = "test-support")]
    #[doc(hidden)]
    pub fn inject_commit_write_fault_for_test(&self, call_index: usize, after_write: bool) {
        self.commit_write_faults
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(call_index, after_write);
    }

    #[cfg(feature = "test-support")]
    pub(super) fn begin_commit_write_for_test(&self) -> usize {
        self.commit_write_calls.fetch_add(1, Ordering::SeqCst) + 1
    }

    #[cfg(feature = "test-support")]
    pub(super) fn take_commit_write_fault_for_test(
        &self,
        call_index: usize,
        after_write: bool,
    ) -> bool {
        let mut faults = self
            .commit_write_faults
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if faults.get(&call_index).copied() == Some(after_write) {
            faults.remove(&call_index);
            true
        } else {
            false
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
            workspace_lease: None,
            not_sync: std::marker::PhantomData,
        })
    }

    /// Creates a coordinator that holds one exclusive workspace lease until it is dropped.
    pub fn coordinator_with_workspace_lease(
        &self,
        workspace_root: impl AsRef<Path>,
        tool_call_id: impl Into<ToolCallId>,
        batch_id: Option<MutationBatchId>,
    ) -> Result<MutationCoordinator> {
        let workspace_root = workspace_root.as_ref();
        let workspace_lease = self.acquire_workspace_mutation_lease(workspace_root)?;
        let mut coordinator = self.coordinator(workspace_root, tool_call_id, batch_id)?;
        coordinator.workspace_lease = Some(workspace_lease);
        Ok(coordinator)
    }

    pub(super) fn acquire_workspace_mutation_lease(
        &self,
        workspace_root: &Path,
    ) -> Result<WorkspaceMutationLease> {
        let workspace_id = stable_workspace_id(workspace_root)?;
        let lease_root = &self.workspace_lease_root;
        match fs::symlink_metadata(lease_root) {
            Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {}
            Ok(_) => bail!(
                "workspace mutation lease root is not a plain directory: {}",
                lease_root.display()
            ),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                fs::create_dir_all(lease_root)
                    .with_context(|| format!("failed to create {}", lease_root.display()))?;
                let metadata = fs::symlink_metadata(lease_root)
                    .with_context(|| format!("failed to inspect {}", lease_root.display()))?;
                if !metadata.is_dir() || metadata.file_type().is_symlink() {
                    bail!(
                        "workspace mutation lease root is not a plain directory: {}",
                        lease_root.display()
                    );
                }
            }
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("failed to inspect {}", lease_root.display()));
            }
        }
        harden_artifact_dir(lease_root)?;
        let lease_name = workspace_id.replace([':', '/', '\\'], "_");
        let lease_path = lease_root.join(format!("{lease_name}.lock"));
        match fs::symlink_metadata(&lease_path) {
            Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => {}
            Ok(_) => bail!(
                "workspace mutation lease is not a plain file: {}",
                lease_path.display()
            ),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("failed to inspect {}", lease_path.display()));
            }
        }
        let file = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(&lease_path)
            .with_context(|| format!("failed to open {}", lease_path.display()))?;
        harden_artifact_file(&lease_path)?;
        for attempt in 0..=WORKSPACE_MUTATION_LEASE_ATTEMPTS {
            match file.try_lock_exclusive() {
                Ok(()) => return Ok(WorkspaceMutationLease { file }),
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    if attempt < WORKSPACE_MUTATION_LEASE_ATTEMPTS {
                        thread::sleep(WORKSPACE_MUTATION_LEASE_RETRY_DELAY);
                    }
                }
                Err(error) => {
                    return Err(error)
                        .with_context(|| format!("failed to lock {}", lease_path.display()));
                }
            }
        }
        bail!(
            "timed out acquiring workspace mutation lease {}",
            lease_path.display()
        )
    }

    /// Returns the latest durable workspace mutation revision for a canonical workspace.
    pub fn current_workspace_revision(&self, workspace_root: impl AsRef<Path>) -> Result<u64> {
        let workspace_id = stable_workspace_id(workspace_root.as_ref())?;
        latest_workspace_revision(&self.store, &workspace_id)
    }

    /// Returns the cross-session controlled-mutation epoch for a workspace.
    pub fn current_workspace_mutation_epoch(
        &self,
        workspace_root: impl AsRef<Path>,
    ) -> Result<u64> {
        let lease = self.acquire_workspace_mutation_lease(workspace_root.as_ref())?;
        lease.epoch()
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
        self.append_bound_batch_started(batch_id, operation_id, expected_subjects, None, None, None)
    }

    pub fn append_bound_batch_started(
        &self,
        batch_id: &str,
        operation_id: &str,
        expected_subjects: &[MutationSubject],
        prepared_digest: Option<&str>,
        approval_identity: Option<&str>,
        policy_fingerprint: Option<&str>,
    ) -> Result<StoredEvent> {
        self.store.append_event(
            DurableEventType::MutationBatchStarted,
            EventClass::Critical,
            serde_json::to_value(MutationBatchStarted {
                batch_id: batch_id.to_owned(),
                operation_id: operation_id.to_owned(),
                expected_subjects: expected_subjects.to_vec(),
                prepared_digest: prepared_digest.map(str::to_owned),
                approval_identity: approval_identity.map(str::to_owned),
                policy_fingerprint: policy_fingerprint.map(str::to_owned),
            })
            .context("failed to encode mutation batch start payload")?,
        )
    }

    pub fn append_batch_finished(
        &self,
        batch_id: &str,
        status: MutationBatchStatus,
        committed_operations: &[OperationId],
        failed_operations: &[OperationId],
    ) -> Result<StoredEvent> {
        self.append_bound_batch_finished(
            batch_id,
            status,
            committed_operations,
            failed_operations,
            &[],
            &[],
            None,
            None,
            None,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn append_bound_batch_finished(
        &self,
        batch_id: &str,
        status: MutationBatchStatus,
        committed_operations: &[OperationId],
        failed_operations: &[OperationId],
        rollback_operations: &[OperationId],
        rollback_failed_operations: &[OperationId],
        prepared_digest: Option<&str>,
        approval_identity: Option<&str>,
        policy_fingerprint: Option<&str>,
    ) -> Result<StoredEvent> {
        self.store.append_event(
            DurableEventType::MutationBatchFinished,
            EventClass::Critical,
            serde_json::to_value(MutationBatchFinished {
                batch_id: batch_id.to_owned(),
                status,
                committed_operations: committed_operations.to_vec(),
                failed_operations: failed_operations.to_vec(),
                rollback_operations: rollback_operations.to_vec(),
                rollback_failed_operations: rollback_failed_operations.to_vec(),
                prepared_digest: prepared_digest.map(str::to_owned),
                approval_identity: approval_identity.map(str::to_owned),
                policy_fingerprint: policy_fingerprint.map(str::to_owned),
            })
            .context("failed to encode mutation batch terminal payload")?,
        )
    }

    /// Appends a neutral extension-process lifecycle audit without marking the workspace dirty.
    ///
    /// This store-backed bridge is used by current extension launchers and shares the session's
    /// linear writer owner with every cloned recorder.
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

fn workspace_lease_root_for_artifacts(artifact_root: &Path) -> PathBuf {
    artifact_root
        .parent()
        .unwrap_or(artifact_root)
        .join("workspace-mutation-leases")
}

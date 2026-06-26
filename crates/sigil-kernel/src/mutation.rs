//! RFC-0002 crash-consistent mutation protocol foundation.
//!
//! This module covers controlled file mutations. Shell, MCP, plugin, and external mutation
//! detection are intentionally handled by later RFC-0002 phases.

use std::{
    fs::{self, File},
    io::Write,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, anyhow, bail};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};

use crate::{
    DurableEventType, EventClass, EventId, JsonlSessionStore, StoredEvent, stable_event_uuid,
    verification::{
        FileType, SnapshotEntryState, VerificationScopeHash, WorkspaceId, WorkspaceRevision,
        WorkspaceSnapshotEntry, WorkspaceSnapshotId, WorkspaceSnapshotManifestV1,
        stable_workspace_id,
    },
};

pub type MutationBatchId = String;
pub type OperationId = String;
pub type ToolCallId = String;

/// Snapshot coverage captured before a controlled mutation.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SnapshotCoverage {
    Captured(String),
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

/// Store-backed durable mutation recorder.
#[derive(Debug, Clone)]
pub struct MutationEventRecorder {
    store: JsonlSessionStore,
}

impl MutationEventRecorder {
    #[must_use]
    pub fn new(store: JsonlSessionStore) -> Self {
        Self { store }
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
            let MutationSubject::File { path, .. } = &payload.subject else {
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
            };

            let absolute_path = workspace_root.join(path);
            let observed_hash = file_content_hash(&absolute_path)?;
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
                    let snapshot_id =
                        single_file_snapshot_id(&payload.workspace_id, path, observed_hash)?;
                    (
                        MutationObservedState::AppliedAsIntended,
                        MutationResolution::MarkCommitted,
                        Some(revision),
                        Some(snapshot_id),
                    )
                } else {
                    let revision = latest_revision.saturating_add(1);
                    latest_revision = revision;
                    let snapshot_id =
                        single_file_snapshot_id(&payload.workspace_id, path, observed_hash)?;
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
    pub fn prepare_file(
        &self,
        relative_path: impl Into<PathBuf>,
        absolute_path: impl Into<PathBuf>,
        intended_after_hash: Option<String>,
    ) -> Result<PreparedFileMutation> {
        let relative_path = normalize_relative_path(relative_path.into())?;
        let absolute_path = absolute_path.into();
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
            snapshot_coverage: if before_hash.is_some() {
                SnapshotCoverage::Unavailable
            } else {
                SnapshotCoverage::NoPriorContent
            },
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
        let workspace_revision = prepared.base_workspace_revision.saturating_add(1);
        let workspace_snapshot_id = single_file_snapshot_id(
            &prepared.workspace_id,
            &prepared.relative_path,
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
    let intended_after_hash = Some(bytes_hash(content));
    let prepared = coordinator.prepare_file(relative_path, absolute_path, intended_after_hash)?;
    coordinator.commit_write(&prepared, content).map(Some)
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

pub fn file_content_hash(path: &Path) -> Result<Option<String>> {
    match fs::read(path) {
        Ok(bytes) => Ok(Some(bytes_hash(&bytes))),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error).with_context(|| format!("failed to read {}", path.display())),
    }
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
        }
    }
    Ok(latest)
}

fn single_file_snapshot_id(
    workspace_id: &str,
    relative_path: &Path,
    content_hash: Option<String>,
) -> Result<WorkspaceSnapshotId> {
    let state = if content_hash.is_some() {
        SnapshotEntryState::Present
    } else {
        SnapshotEntryState::Missing
    };
    let manifest = WorkspaceSnapshotManifestV1 {
        workspace_id: workspace_id.to_owned(),
        scope_hash: mutation_scope_hash(relative_path),
        entries: vec![WorkspaceSnapshotEntry {
            normalized_path: relative_path.to_path_buf(),
            file_type: FileType::File,
            content_hash,
            mode: None,
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

#[cfg(test)]
#[path = "tests/mutation_tests.rs"]
mod tests;

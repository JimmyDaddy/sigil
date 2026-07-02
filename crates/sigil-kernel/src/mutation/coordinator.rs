use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};

use crate::verification::{FileType, WorkspaceId};

use super::{
    CommittedDirectoryMutation, CommittedFileMutation, MutationBatchId, MutationCommitted,
    MutationEventRecorder, MutationPrepared, MutationSubject, MutationSyncClass,
    PreparedDirectoryMutation, PreparedFileMutation, SnapshotCoverage, ToolCallId, atomic_replace,
    bytes_hash, compare_current_directory_hash, compare_current_hash, directory_present_hash,
    directory_state_hash, ensure_absolute_path_matches_subject,
    ensure_observed_after_hash_matches_intent, file_content_hash, latest_workspace_revision,
    normalize_absolute_path_for_subject, normalize_relative_path, operation_id_for,
    single_subject_snapshot_id, snapshot_coverage_for_pre_mutation_content, sync_parent,
};

/// Controlled mutation coordinator for one tool call.
#[derive(Debug, Clone)]
pub struct MutationCoordinator {
    pub(super) recorder: MutationEventRecorder,
    pub(super) workspace_root: PathBuf,
    pub(super) workspace_id: WorkspaceId,
    pub(super) tool_call_id: ToolCallId,
    pub(super) batch_id: Option<MutationBatchId>,
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

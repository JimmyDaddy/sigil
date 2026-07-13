use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};

use crate::{
    ControlledCheckpoint, ControlledCheckpointFile, ControlledCheckpointFileAvailability,
    ControlledCheckpointProjection, ControlledCheckpointRestoreKind,
    ControlledCheckpointRestoreOutput, ControlledCheckpointRestorePreview,
    ControlledCheckpointRestorePreviewFile, ControlledCheckpointRestoreRequest,
    SessionStreamRecord, stable_event_uuid, stable_workspace_id,
};

use super::{
    CheckpointRestoreConflict, CheckpointRestoreConflictReason, CheckpointRestored,
    MutationBatchStatus, MutationCoordinator, MutationEventRecorder, MutationSubject,
    RestoredFileMutation, SnapshotCoverage, bytes_hash, ensure_absolute_path_matches_subject,
    file_content_hash, normalize_relative_path, read_mutation_artifact_content,
};

#[derive(Debug)]
struct ResolvedRestoreFile {
    binding: ControlledCheckpointFile,
    absolute_path: PathBuf,
    restore_content: Option<Vec<u8>>,
}

#[derive(Debug)]
struct ResolvedRestore {
    checkpoint: ControlledCheckpoint,
    preview: ControlledCheckpointRestorePreview,
    files: Vec<ResolvedRestoreFile>,
}

/// Builds a read-only, exact checkpoint restore preview from the current durable stream and disk.
///
/// # Errors
///
/// Returns an error when the requested checkpoint id/digest is stale, the stream cannot be
/// projected, or the workspace root cannot be validated. Per-file conflicts are returned in the
/// preview with `ready = false` and do not mutate durable state.
pub fn preview_controlled_checkpoint_restore(
    recorder: &MutationEventRecorder,
    records: &[SessionStreamRecord],
    workspace_root: &Path,
    request: &ControlledCheckpointRestoreRequest,
) -> Result<ControlledCheckpointRestorePreview> {
    resolve_restore(recorder, records, workspace_root, request).map(|resolved| resolved.preview)
}

/// Restores every controlled ordinary file in an exact checkpoint after one full preflight.
///
/// The operation holds the workspace mutation lease across preflight and apply. It appends a
/// conflict before returning when preflight fails, and uses the existing mutation batch plus
/// per-file `CheckpointRestored` evidence for successful writes.
///
/// # Errors
///
/// Returns an error when the binding is stale, any file is not exactly restorable, durable event
/// writes fail, or a filesystem mutation cannot complete. A mid-batch error is recorded as failed
/// or partially applied rather than being described as an atomic rollback.
pub fn execute_controlled_checkpoint_restore(
    recorder: &MutationEventRecorder,
    records: &[SessionStreamRecord],
    workspace_root: &Path,
    request: &ControlledCheckpointRestoreRequest,
) -> Result<ControlledCheckpointRestoreOutput> {
    let batch_id = format!(
        "checkpoint-restore:{}",
        stable_event_uuid(
            "sigil-checkpoint-restore-batch",
            &format!("{}:{}", request.checkpoint_id, request.checkpoint_digest),
        )
    );
    let tool_call_id = format!("checkpoint-restore:{}", request.checkpoint_id);
    let coordinator = recorder.coordinator_with_workspace_lease(
        workspace_root,
        tool_call_id.clone(),
        Some(batch_id.clone()),
    )?;
    let resolved = match resolve_restore(recorder, records, workspace_root, request) {
        Ok(resolved) => resolved,
        Err(error) => {
            recorder.append_checkpoint_restore_conflict(&CheckpointRestoreConflict {
                checkpoint_id: request.checkpoint_id.clone(),
                checkpoint_digest: request.checkpoint_digest.clone(),
                path: None,
                reason: CheckpointRestoreConflictReason::InvalidBinding,
                expected_current_hash: None,
                actual_current_hash: None,
            })?;
            return Err(error.context("controlled checkpoint restore binding is stale or invalid"));
        }
    };
    if !resolved.preview.ready {
        append_restore_conflicts(recorder, &resolved.preview)?;
        bail!("controlled checkpoint restore preflight found conflicts");
    }

    let expected_subjects = resolved
        .files
        .iter()
        .map(|file| MutationSubject::File {
            path: file.binding.path.clone(),
            file_type: crate::FileType::File,
        })
        .collect::<Vec<_>>();
    recorder.append_batch_started(
        &batch_id,
        &format!("restore:{}", resolved.checkpoint.checkpoint_id),
        &expected_subjects,
    )?;

    let mut restored = Vec::with_capacity(resolved.files.len());
    let mut committed_operations = Vec::with_capacity(resolved.files.len());
    for file in &resolved.files {
        match restore_file(recorder, &coordinator, &tool_call_id, file) {
            Ok(output) => {
                committed_operations.push(output.committed.operation_id.clone());
                restored.push(output);
            }
            Err(error) => {
                let failed_operations = vec![file.binding.latest_operation_id.clone()];
                let status = if committed_operations.is_empty() {
                    MutationBatchStatus::Failed
                } else {
                    MutationBatchStatus::PartiallyApplied
                };
                recorder
                    .append_batch_finished(
                        &batch_id,
                        status,
                        &committed_operations,
                        &failed_operations,
                    )
                    .with_context(|| {
                        format!("failed to record checkpoint restore batch after error: {error:#}")
                    })?;
                return Err(error);
            }
        }
    }
    recorder.append_batch_finished(
        &batch_id,
        MutationBatchStatus::Applied,
        &committed_operations,
        &[],
    )?;
    Ok(ControlledCheckpointRestoreOutput {
        preview: resolved.preview,
        batch_id,
        restored,
    })
}

fn restore_file(
    recorder: &MutationEventRecorder,
    coordinator: &MutationCoordinator,
    tool_call_id: &str,
    file: &ResolvedRestoreFile,
) -> Result<RestoredFileMutation> {
    let intended_after_hash = file.restore_content.as_deref().map(bytes_hash);
    let prepared = coordinator.prepare_file_expected(
        file.binding.path.clone(),
        file.absolute_path.clone(),
        file.binding.expected_current_hash.clone(),
        intended_after_hash,
    )?;
    let committed = match file.restore_content.as_deref() {
        Some(content) => coordinator.commit_write(&prepared, content)?,
        None => coordinator.commit_delete(&prepared)?,
    };
    let checkpoint_payload = CheckpointRestored {
        operation_id: committed.operation_id.clone(),
        batch_id: committed.batch_id.clone(),
        tool_call_id: Some(tool_call_id.to_owned()),
        restored_subject: MutationSubject::File {
            path: file.binding.path.clone(),
            file_type: crate::FileType::File,
        },
        restored_from: file.binding.snapshot_coverage.clone(),
        mutation_committed_event_id: committed.committed_event.event_id.clone(),
        workspace_revision: committed.workspace_revision,
        workspace_snapshot_id: committed.workspace_snapshot_id.clone(),
    };
    let checkpoint_event = recorder.append_checkpoint_restored(&checkpoint_payload)?;
    Ok(RestoredFileMutation {
        committed,
        checkpoint_event,
        restored_from: file.binding.snapshot_coverage.clone(),
    })
}

fn resolve_restore(
    recorder: &MutationEventRecorder,
    records: &[SessionStreamRecord],
    workspace_root: &Path,
    request: &ControlledCheckpointRestoreRequest,
) -> Result<ResolvedRestore> {
    let workspace_root = fs::canonicalize(workspace_root).with_context(|| {
        format!(
            "failed to resolve checkpoint restore workspace {}",
            workspace_root.display()
        )
    })?;
    let projection = ControlledCheckpointProjection::from_records(records)?;
    let checkpoint = projection
        .checkpoint(&request.checkpoint_id)
        .cloned()
        .ok_or_else(|| anyhow!("controlled checkpoint is no longer available"))?;
    if checkpoint.checkpoint_digest != request.checkpoint_digest {
        bail!("controlled checkpoint changed since the action was rendered");
    }
    let workspace_id = stable_workspace_id(&workspace_root)?;
    let mut preview_files = Vec::with_capacity(checkpoint.files.len());
    let mut resolved_files = Vec::with_capacity(checkpoint.files.len());

    for binding in &checkpoint.files {
        let mut conflict_reason = availability_conflict(binding.availability);
        if binding.workspace_id != workspace_id {
            conflict_reason = Some(CheckpointRestoreConflictReason::WorkspaceMismatch);
        }
        let relative_path = match normalize_relative_path(binding.path.clone()) {
            Ok(path) => path,
            Err(_) => {
                conflict_reason = Some(CheckpointRestoreConflictReason::InvalidBinding);
                binding.path.clone()
            }
        };
        let absolute_path = workspace_root.join(&relative_path);
        if ensure_absolute_path_matches_subject(&workspace_root, &relative_path, &absolute_path)
            .is_err()
        {
            conflict_reason = Some(CheckpointRestoreConflictReason::InvalidBinding);
        }
        if binding.restore_kind == ControlledCheckpointRestoreKind::RestoreContent
            && absolute_path.parent().is_none_or(|parent| {
                !matches!(
                    fs::symlink_metadata(parent),
                    Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink()
                )
            })
        {
            conflict_reason = Some(CheckpointRestoreConflictReason::InvalidBinding);
        }
        let actual_current_hash = match file_content_hash(&absolute_path) {
            Ok(hash) => hash,
            Err(_) => {
                conflict_reason = Some(CheckpointRestoreConflictReason::InvalidBinding);
                None
            }
        };
        if actual_current_hash != binding.expected_current_hash {
            conflict_reason = Some(CheckpointRestoreConflictReason::CurrentHashMismatch);
        }

        let restore_content = match &binding.snapshot_coverage {
            SnapshotCoverage::Captured(artifact_id) if conflict_reason.is_none() => {
                match read_mutation_artifact_content(&recorder.artifact_root, artifact_id) {
                    Ok(content)
                        if binding.before_hash.as_deref()
                            == Some(bytes_hash(&content).as_str()) =>
                    {
                        Some(content)
                    }
                    Ok(_) => {
                        conflict_reason = Some(CheckpointRestoreConflictReason::InvalidBinding);
                        None
                    }
                    Err(_) => {
                        conflict_reason =
                            Some(CheckpointRestoreConflictReason::ArtifactUnavailable);
                        None
                    }
                }
            }
            SnapshotCoverage::Captured(_) => None,
            SnapshotCoverage::NoPriorContent if binding.before_hash.is_none() => None,
            SnapshotCoverage::NoPriorContent => {
                conflict_reason = Some(CheckpointRestoreConflictReason::InvalidBinding);
                None
            }
            SnapshotCoverage::SkippedSensitive => {
                conflict_reason = Some(CheckpointRestoreConflictReason::SensitiveSnapshot);
                None
            }
            SnapshotCoverage::Unsupported => {
                conflict_reason = Some(CheckpointRestoreConflictReason::UnsupportedSnapshot);
                None
            }
            SnapshotCoverage::Unavailable => {
                conflict_reason = Some(CheckpointRestoreConflictReason::ArtifactUnavailable);
                None
            }
        };
        preview_files.push(ControlledCheckpointRestorePreviewFile {
            path: binding.path.clone(),
            restore_kind: binding.restore_kind,
            expected_current_hash: binding.expected_current_hash.clone(),
            actual_current_hash,
            conflict_reason,
        });
        resolved_files.push(ResolvedRestoreFile {
            binding: binding.clone(),
            absolute_path,
            restore_content,
        });
    }
    let ready = !preview_files.is_empty()
        && preview_files
            .iter()
            .all(|file| file.conflict_reason.is_none());
    Ok(ResolvedRestore {
        preview: ControlledCheckpointRestorePreview {
            checkpoint_id: checkpoint.checkpoint_id.clone(),
            checkpoint_digest: checkpoint.checkpoint_digest.clone(),
            files: preview_files,
            unknown_mutation_count: checkpoint.unknown_mutation_count,
            ready,
        },
        checkpoint,
        files: resolved_files,
    })
}

fn availability_conflict(
    availability: ControlledCheckpointFileAvailability,
) -> Option<CheckpointRestoreConflictReason> {
    match availability {
        ControlledCheckpointFileAvailability::Restorable => None,
        ControlledCheckpointFileAvailability::Sensitive => {
            Some(CheckpointRestoreConflictReason::SensitiveSnapshot)
        }
        ControlledCheckpointFileAvailability::Unsupported => {
            Some(CheckpointRestoreConflictReason::UnsupportedSnapshot)
        }
        ControlledCheckpointFileAvailability::Unavailable => {
            Some(CheckpointRestoreConflictReason::ArtifactUnavailable)
        }
    }
}

fn append_restore_conflicts(
    recorder: &MutationEventRecorder,
    preview: &ControlledCheckpointRestorePreview,
) -> Result<()> {
    let conflicts = preview
        .files
        .iter()
        .filter_map(|file| file.conflict_reason.map(|reason| (file, reason)))
        .collect::<Vec<_>>();
    if conflicts.is_empty() {
        recorder.append_checkpoint_restore_conflict(&CheckpointRestoreConflict {
            checkpoint_id: preview.checkpoint_id.clone(),
            checkpoint_digest: preview.checkpoint_digest.clone(),
            path: None,
            reason: CheckpointRestoreConflictReason::InvalidBinding,
            expected_current_hash: None,
            actual_current_hash: None,
        })?;
        return Ok(());
    }
    for (file, reason) in conflicts {
        recorder.append_checkpoint_restore_conflict(&CheckpointRestoreConflict {
            checkpoint_id: preview.checkpoint_id.clone(),
            checkpoint_digest: preview.checkpoint_digest.clone(),
            path: Some(file.path.clone()),
            reason,
            expected_current_hash: file.expected_current_hash.clone(),
            actual_current_hash: file.actual_current_hash.clone(),
        })?;
    }
    Ok(())
}

#[cfg(test)]
#[path = "../tests/checkpoint_restore_tests.rs"]
mod tests;

use std::{
    ffi::OsString,
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Result, anyhow, bail};

use crate::{stable_event_uuid, verification::FileType};

use super::{
    CheckpointRestored, CommittedDirectoryMutation, CommittedFileMutation, MutationBatchId,
    MutationEventRecorder, MutationSubject, OperationId, RestoredFileMutation, SnapshotCoverage,
    bytes_hash, directory_present_hash, ensure_empty_directory, file_content_hash,
    read_mutation_artifact_content,
};

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

/// Writes a file with RFC-0002 mutation evidence.
pub fn write_file_with_mutation_in_batch(
    recorder: Option<&MutationEventRecorder>,
    workspace_root: &Path,
    tool_call_id: &str,
    batch_id: Option<MutationBatchId>,
    relative_path: impl Into<PathBuf>,
    absolute_path: impl Into<PathBuf>,
    content: &[u8],
) -> Result<Option<CommittedFileMutation>> {
    let recorder = require_mutation_recorder(recorder)?;
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
    let recorder = require_mutation_recorder(recorder)?;
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
    let recorder = require_mutation_recorder(recorder)?;
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

/// Deletes a file with RFC-0002 mutation evidence.
pub fn delete_file_with_mutation_in_batch(
    recorder: Option<&MutationEventRecorder>,
    workspace_root: &Path,
    tool_call_id: &str,
    batch_id: Option<MutationBatchId>,
    relative_path: impl Into<PathBuf>,
    absolute_path: impl Into<PathBuf>,
) -> Result<Option<CommittedFileMutation>> {
    let absolute_path = absolute_path.into();
    let recorder = require_mutation_recorder(recorder)?;
    let coordinator = recorder.coordinator(workspace_root, tool_call_id.to_owned(), batch_id)?;
    let prepared = coordinator.prepare_file(relative_path, &absolute_path, None)?;
    coordinator.commit_delete(&prepared).map(Some)
}

fn require_mutation_recorder(
    recorder: Option<&MutationEventRecorder>,
) -> Result<&MutationEventRecorder> {
    recorder.ok_or_else(|| anyhow!("mutation recorder is required for workspace mutations"))
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

pub(super) fn operation_id_for(
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

pub(super) fn normalize_relative_path(path: PathBuf) -> Result<PathBuf> {
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

pub(super) fn ensure_absolute_path_matches_subject(
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

pub(super) fn normalize_absolute_path_for_subject(path: &Path) -> Result<PathBuf> {
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

pub(super) fn lexically_normalize_path(path: &Path) -> Result<PathBuf> {
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

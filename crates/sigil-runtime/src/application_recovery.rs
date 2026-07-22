//! Shared application-facing projection and commands for durable conversation recovery.

use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use sigil_kernel::{
    ControlEntry, ControlledCheckpointFileAvailability, ControlledCheckpointProjection,
    ControlledCheckpointRestoreKind, ControlledCheckpointRestoreOutput,
    ControlledCheckpointRestorePreview, ControlledCheckpointRestoreRequest,
    ConversationForkProjection, JsonlSessionStore, MutationEventRecorder, RootConfig,
    SessionLogEntry, SessionStreamRecord, TypedDomainEvent, resolve_workspace_root,
};

/// Bounded path-safe summary of one controlled checkpoint file.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct ApplicationCheckpointFileView {
    pub path: PathBuf,
    pub restore_kind: ControlledCheckpointRestoreKind,
    pub availability: ControlledCheckpointFileAvailability,
}

/// Exact checkpoint binding rendered by an application surface.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct ApplicationCheckpointView {
    pub checkpoint_id: String,
    pub checkpoint_digest: String,
    pub turn_index: usize,
    pub prompt: Option<String>,
    pub files: Vec<ApplicationCheckpointFileView>,
    pub unknown_mutation_count: usize,
    pub fully_restorable: bool,
}

/// Exact finalized-turn binding rendered by an application surface.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct ApplicationConversationForkPointView {
    pub source_turn_index: usize,
    pub source_turn_digest: String,
    pub source_boundary_stream_sequence: u64,
    pub source_finalized_stream_sequence: u64,
}

/// Read-only durable recovery projection for one application session.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct ApplicationConversationRecoveryView {
    pub checkpoints: Vec<ApplicationCheckpointView>,
    pub fork_points: Vec<ApplicationConversationForkPointView>,
    pub through_stream_sequence: u64,
}

/// Resolves the exact workspace root used by application recovery commands.
///
/// # Errors
///
/// Returns an error when current configuration cannot be loaded.
pub fn application_recovery_workspace_root(
    config_path: &Path,
    launch_cwd: &Path,
) -> Result<PathBuf> {
    let root_config = RootConfig::load(config_path)?;
    Ok(resolve_workspace_root(
        config_path,
        launch_cwd,
        &root_config.workspace.root,
    ))
}

/// One bounded recorded diff reversed into the exact restore direction.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct ApplicationCheckpointReverseDiff {
    pub path: PathBuf,
    pub diff: String,
    pub truncated: bool,
    pub original_line_count: usize,
}

/// Exact restore preflight plus any durable reverse diff that can be rendered for review.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct ApplicationCheckpointRestoreReview {
    pub preview: ControlledCheckpointRestorePreview,
    pub reverse_diffs: Vec<ApplicationCheckpointReverseDiff>,
}

/// Projects checkpoint and finalized-turn recovery bindings from one exact durable scope.
///
/// # Errors
///
/// Returns an error when the stream cannot be verified, belongs to another session scope, or a
/// recovery-critical entry cannot be decoded.
pub fn application_conversation_recovery_view(
    session_path: &Path,
    expected_session_scope_id: &str,
) -> Result<ApplicationConversationRecoveryView> {
    let records = read_bound_records(session_path, expected_session_scope_id)?;
    let checkpoints = ControlledCheckpointProjection::from_records(&records)?
        .checkpoints
        .into_iter()
        .map(|checkpoint| {
            let fully_restorable = checkpoint.is_fully_restorable();
            ApplicationCheckpointView {
                checkpoint_id: checkpoint.checkpoint_id,
                checkpoint_digest: checkpoint.checkpoint_digest,
                turn_index: checkpoint.turn_index,
                prompt: checkpoint.prompt,
                files: checkpoint
                    .files
                    .into_iter()
                    .map(|file| ApplicationCheckpointFileView {
                        path: file.path,
                        restore_kind: file.restore_kind,
                        availability: file.availability,
                    })
                    .collect(),
                unknown_mutation_count: checkpoint.unknown_mutation_count,
                fully_restorable,
            }
        })
        .collect();
    let fork_points = ConversationForkProjection::from_records(&records)?
        .points
        .into_iter()
        .map(|point| ApplicationConversationForkPointView {
            source_turn_index: point.source_turn_index,
            source_turn_digest: point.source_turn_digest,
            source_boundary_stream_sequence: point.source_boundary_stream_sequence,
            source_finalized_stream_sequence: point.source_finalized_stream_sequence,
        })
        .collect();
    Ok(ApplicationConversationRecoveryView {
        checkpoints,
        fork_points,
        through_stream_sequence: records
            .last()
            .map_or(0, SessionStreamRecord::stream_sequence),
    })
}

/// Revalidates one exact checkpoint binding against current durable and workspace truth.
///
/// # Errors
///
/// Returns an error when the binding is stale, the durable scope changed, or the workspace root
/// cannot be validated. File-level conflicts remain represented by `ready = false`.
pub fn preview_application_checkpoint_restore(
    session_path: &Path,
    expected_session_scope_id: &str,
    workspace_root: &Path,
    request: &ControlledCheckpointRestoreRequest,
) -> Result<ApplicationCheckpointRestoreReview> {
    let records = read_bound_records(session_path, expected_session_scope_id)?;
    let store = JsonlSessionStore::new(session_path)?;
    let recorder = MutationEventRecorder::new(store);
    let preview = sigil_kernel::preview_controlled_checkpoint_restore(
        &recorder,
        &records,
        workspace_root,
        request,
    )?;
    let reverse_diffs = checkpoint_reverse_diffs(&records, &preview)?;
    Ok(ApplicationCheckpointRestoreReview {
        preview,
        reverse_diffs,
    })
}

/// Applies one exact controlled checkpoint restore after a full fresh preflight.
///
/// # Errors
///
/// Returns an error for stale bindings, conflicts, durable append failures, or filesystem errors.
pub fn restore_application_checkpoint(
    session_path: &Path,
    expected_session_scope_id: &str,
    workspace_root: &Path,
    request: &ControlledCheckpointRestoreRequest,
) -> Result<ControlledCheckpointRestoreOutput> {
    let records = read_bound_records(session_path, expected_session_scope_id)?;
    let store = JsonlSessionStore::new(session_path)?;
    let recorder = MutationEventRecorder::new(store);
    sigil_kernel::execute_controlled_checkpoint_restore(
        &recorder,
        &records,
        workspace_root,
        request,
    )
}

fn read_bound_records(
    session_path: &Path,
    expected_session_scope_id: &str,
) -> Result<Vec<SessionStreamRecord>> {
    if expected_session_scope_id.trim().is_empty() {
        bail!("expected recovery session scope must not be empty");
    }
    let records = JsonlSessionStore::read_event_records(session_path)
        .with_context(|| format!("failed to read recovery stream {}", session_path.display()))?;
    let Some(first) = records.first() else {
        bail!("conversation recovery requires a non-empty durable stream");
    };
    if first.session_id() != expected_session_scope_id
        || records
            .iter()
            .any(|record| record.session_id() != expected_session_scope_id)
    {
        bail!("conversation recovery session scope mismatch");
    }
    Ok(records)
}

fn checkpoint_reverse_diffs(
    records: &[SessionStreamRecord],
    preview: &ControlledCheckpointRestorePreview,
) -> Result<Vec<ApplicationCheckpointReverseDiff>> {
    let projection = ControlledCheckpointProjection::from_records(records)?;
    let checkpoint = projection
        .checkpoints
        .iter()
        .find(|checkpoint| {
            checkpoint.checkpoint_id == preview.checkpoint_id
                && checkpoint.checkpoint_digest == preview.checkpoint_digest
        })
        .context("checkpoint restore review binding changed")?;
    let turn_records = records
        .iter()
        .filter(|record| record.stream_sequence() > checkpoint.turn_boundary_stream_sequence)
        .take_while(|record| {
            !matches!(
                checkpoint_session_entry(record),
                Ok(Some(SessionLogEntry::User(_)))
            )
        })
        .collect::<Vec<_>>();
    let mut prepared_call_ids = BTreeMap::new();
    let mut committed_operation_ids = BTreeSet::new();
    for record in &turn_records {
        let Some(typed) = record.typed_domain_event_record()? else {
            continue;
        };
        match typed.event {
            TypedDomainEvent::MutationPrepared(prepared) => {
                if let Some(call_id) = prepared.tool_call_id {
                    prepared_call_ids.insert(prepared.operation_id, call_id);
                }
            }
            TypedDomainEvent::MutationCommitted(committed) => {
                committed_operation_ids.insert(committed.operation_id);
            }
            _ => {}
        }
    }
    let committed_call_ids = committed_operation_ids
        .iter()
        .filter_map(|operation_id| prepared_call_ids.get(operation_id))
        .collect::<BTreeSet<_>>();
    let mut reverse_diffs = Vec::new();
    for record in turn_records {
        let Some(SessionLogEntry::Control(ControlEntry::ToolPreviewCaptured(snapshot))) =
            checkpoint_session_entry(record)?
        else {
            continue;
        };
        if !committed_call_ids.contains(&snapshot.call_id) {
            continue;
        }
        for file in snapshot.file_diffs {
            let Some(restore_file) = preview
                .files
                .iter()
                .find(|preview_file| preview_file.path.to_string_lossy().as_ref() == file.path)
            else {
                continue;
            };
            if file.diff.trim().is_empty() {
                continue;
            }
            reverse_diffs.push(ApplicationCheckpointReverseDiff {
                path: restore_file.path.clone(),
                diff: reverse_recorded_diff(&file.diff, restore_file).join("\n"),
                truncated: file.truncated,
                original_line_count: file.original_line_count,
            });
        }
    }
    reverse_diffs.reverse();
    Ok(reverse_diffs)
}

fn checkpoint_session_entry(record: &SessionStreamRecord) -> Result<Option<SessionLogEntry>> {
    record.session_log_entry()
}

fn reverse_recorded_diff(
    diff: &str,
    restore_file: &sigil_kernel::ControlledCheckpointRestorePreviewFile,
) -> Vec<String> {
    let path = restore_file.path.display();
    let current_header = match restore_file.restore_kind {
        ControlledCheckpointRestoreKind::RestoreContent
            if restore_file.actual_current_hash.is_none() =>
        {
            "--- /dev/null".to_owned()
        }
        _ => format!("--- current/{path}"),
    };
    let restored_header = match restore_file.restore_kind {
        ControlledCheckpointRestoreKind::RestoreContent => format!("+++ restored/{path}"),
        ControlledCheckpointRestoreKind::RemoveCreatedFile => "+++ /dev/null".to_owned(),
    };
    let mut lines = vec![current_header, restored_header];
    for line in diff.lines() {
        if line.starts_with("--- ") || line.starts_with("+++ ") {
            continue;
        }
        if let Some(reversed) = reverse_unified_hunk_header(line) {
            lines.push(reversed);
        } else if let Some(removed) = line.strip_prefix('-') {
            lines.push(format!("+{removed}"));
        } else if let Some(added) = line.strip_prefix('+') {
            lines.push(format!("-{added}"));
        } else {
            lines.push(line.to_owned());
        }
    }
    lines
}

fn reverse_unified_hunk_header(line: &str) -> Option<String> {
    let body = line.strip_prefix("@@ -")?;
    let (old_range, rest) = body.split_once(" +")?;
    let (new_range, suffix) = rest.split_once(" @@")?;
    Some(format!("@@ -{new_range} +{old_range} @@{suffix}"))
}

#[cfg(test)]
#[path = "tests/application_recovery_tests.rs"]
mod tests;

use std::path::{Path, PathBuf};

use anyhow::Result;

use super::*;
use crate::{
    DurableEventType, EventClass, FileType, JsonlSessionStore, ModelMessage,
    MutationArtifactLifecycleRecorded, MutationArtifactLifecycleStatus, MutationCommitted,
    MutationPrepared, MutationSubject, MutationSyncClass, SessionLogEntry, SnapshotCoverage,
    ToolEffect, WorkspaceMutationDetected, WorkspaceMutationDetectionReason,
};

#[test]
fn checkpoint_projection_requires_committed_files_and_folds_same_path() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let store = JsonlSessionStore::new(temp.path().join("session.jsonl"))?;
    store.append(&SessionLogEntry::User(ModelMessage::user("edit note")))?;

    append_file_operation(
        &store,
        "op-1",
        "note.txt",
        Some("hash-before"),
        Some("hash-middle"),
        SnapshotCoverage::Captured("mutation-artifact:sha256:before".to_owned()),
        true,
    )?;
    append_file_operation(
        &store,
        "op-2",
        "note.txt",
        Some("hash-middle"),
        Some("hash-after"),
        SnapshotCoverage::Captured("mutation-artifact:sha256:middle".to_owned()),
        true,
    )?;
    append_file_operation(
        &store,
        "prepared-only",
        "ignored.txt",
        None,
        Some("ignored-after"),
        SnapshotCoverage::NoPriorContent,
        false,
    )?;

    let records = JsonlSessionStore::read_event_records(store.path())?;
    let projection = ControlledCheckpointProjection::from_records(&records)?;
    let checkpoint = projection.latest().expect("checkpoint");

    assert_eq!(checkpoint.turn_index, 1);
    assert_eq!(checkpoint.prompt.as_deref(), Some("edit note"));
    assert_eq!(checkpoint.files.len(), 1);
    let file = &checkpoint.files[0];
    assert_eq!(file.path, PathBuf::from("note.txt"));
    assert_eq!(file.first_operation_id, "op-1");
    assert_eq!(file.latest_operation_id, "op-2");
    assert_eq!(file.before_hash.as_deref(), Some("hash-before"));
    assert_eq!(file.expected_current_hash.as_deref(), Some("hash-after"));
    assert_eq!(
        file.snapshot_coverage,
        SnapshotCoverage::Captured("mutation-artifact:sha256:before".to_owned())
    );
    assert!(checkpoint.is_fully_restorable());
    assert_eq!(
        projection
            .checkpoint(&checkpoint.checkpoint_id)
            .map(|value| value.checkpoint_digest.as_str()),
        Some(checkpoint.checkpoint_digest.as_str())
    );
    assert_eq!(
        ControlledCheckpointProjection::from_records(&records)?
            .latest()
            .map(|value| value.checkpoint_digest.as_str()),
        Some(checkpoint.checkpoint_digest.as_str())
    );
    Ok(())
}

#[test]
fn checkpoint_projection_marks_lifecycle_and_unknown_side_effects_truthfully() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let store = JsonlSessionStore::new(temp.path().join("session.jsonl"))?;
    store.append(&SessionLogEntry::User(ModelMessage::user(
        "run mixed edits",
    )))?;
    let artifact_id = "mutation-artifact:sha256:expired";
    append_file_operation(
        &store,
        "op-expired",
        "note.txt",
        Some("hash-before"),
        Some("hash-after"),
        SnapshotCoverage::Captured(artifact_id.to_owned()),
        true,
    )?;
    store.append_event(
        DurableEventType::WorkspaceMutationDetected,
        EventClass::Critical,
        serde_json::to_value(WorkspaceMutationDetected {
            operation_id: "op-shell".to_owned(),
            tool_call_id: Some("call-shell".to_owned()),
            tool_name: "bash".to_owned(),
            tool_effect: ToolEffect::Unknown,
            workspace_id: "workspace-1".to_owned(),
            scope_hash: "scope-1".to_owned(),
            from_workspace_snapshot_id: Some("snapshot-before".to_owned()),
            to_workspace_snapshot_id: Some("snapshot-after".to_owned()),
            base_workspace_revision: 1,
            workspace_revision: 2,
            reason: WorkspaceMutationDetectionReason::SnapshotChanged,
            unknown_dirty: true,
            metadata: Default::default(),
        })?,
    )?;
    store.append_event(
        DurableEventType::MutationArtifactLifecycleRecorded,
        EventClass::Critical,
        serde_json::to_value(MutationArtifactLifecycleRecorded {
            artifact_id: artifact_id.to_owned(),
            status: MutationArtifactLifecycleStatus::Expired,
            reason: "retention policy".to_owned(),
            content_hash: None,
            size: None,
            operation_ids: vec!["op-expired".to_owned()],
            source_paths: vec![PathBuf::from("note.txt")],
        })?,
    )?;

    let projection = ControlledCheckpointProjection::from_records(
        &JsonlSessionStore::read_event_records(store.path())?,
    )?;
    let checkpoint = projection.latest().expect("checkpoint");

    assert_eq!(checkpoint.unknown_mutation_count, 1);
    assert_eq!(
        checkpoint.files[0].availability,
        ControlledCheckpointFileAvailability::Unavailable
    );
    assert!(!checkpoint.is_fully_restorable());
    Ok(())
}

#[test]
fn checkpoint_projection_distinguishes_created_sensitive_and_directory_mutations() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let store = JsonlSessionStore::new(temp.path().join("session.jsonl"))?;
    store.append(&SessionLogEntry::User(ModelMessage::user("create files")))?;
    append_file_operation(
        &store,
        "op-created",
        "created.txt",
        None,
        Some("hash-created"),
        SnapshotCoverage::NoPriorContent,
        true,
    )?;
    append_file_operation(
        &store,
        "op-secret",
        ".env",
        Some("hash-secret"),
        Some("hash-secret-after"),
        SnapshotCoverage::SkippedSensitive,
        true,
    )?;
    let directory_prepared = prepared(
        "op-dir",
        MutationSubject::Directory {
            path: PathBuf::from("generated"),
        },
        None,
        None,
        SnapshotCoverage::Unsupported,
    );
    append_payload(
        &store,
        DurableEventType::MutationPrepared,
        &directory_prepared,
    )?;
    append_payload(
        &store,
        DurableEventType::MutationCommitted,
        &MutationCommitted {
            operation_id: "op-dir".to_owned(),
            batch_id: None,
            workspace_id: Some("workspace-1".to_owned()),
            observed_after_hash: Some("directory:present".to_owned()),
            workspace_revision: 3,
            workspace_snapshot_id: "snapshot-dir".to_owned(),
            committed_subject: MutationSubject::Directory {
                path: PathBuf::from("generated"),
            },
        },
    )?;

    let projection = ControlledCheckpointProjection::from_records(
        &JsonlSessionStore::read_event_records(store.path())?,
    )?;
    let checkpoint = projection.latest().expect("checkpoint");

    assert_eq!(checkpoint.files.len(), 2);
    let created = checkpoint
        .files
        .iter()
        .find(|file| file.path == Path::new("created.txt"))
        .expect("created file");
    assert_eq!(
        created.restore_kind,
        ControlledCheckpointRestoreKind::RemoveCreatedFile
    );
    assert_eq!(
        created.availability,
        ControlledCheckpointFileAvailability::Restorable
    );
    let sensitive = checkpoint
        .files
        .iter()
        .find(|file| file.path == Path::new(".env"))
        .expect("sensitive file");
    assert_eq!(
        sensitive.availability,
        ControlledCheckpointFileAvailability::Sensitive
    );
    assert!(!checkpoint.is_fully_restorable());
    Ok(())
}

#[test]
fn checkpoint_projection_accepts_content_recaptured_after_old_lifecycle_event() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let store = JsonlSessionStore::new(temp.path().join("session.jsonl"))?;
    store.append(&SessionLogEntry::User(ModelMessage::user(
        "recreate content",
    )))?;
    let artifact_id = "mutation-artifact:sha256:reused";
    store.append_event(
        DurableEventType::MutationArtifactLifecycleRecorded,
        EventClass::Critical,
        serde_json::to_value(MutationArtifactLifecycleRecorded {
            artifact_id: artifact_id.to_owned(),
            status: MutationArtifactLifecycleStatus::Expired,
            reason: "old retention event".to_owned(),
            content_hash: None,
            size: None,
            operation_ids: vec!["old-operation".to_owned()],
            source_paths: vec![PathBuf::from("old.txt")],
        })?,
    )?;
    append_file_operation(
        &store,
        "op-recaptured",
        "note.txt",
        Some("hash-before"),
        Some("hash-after"),
        SnapshotCoverage::Captured(artifact_id.to_owned()),
        true,
    )?;

    let projection = ControlledCheckpointProjection::from_records(
        &JsonlSessionStore::read_event_records(store.path())?,
    )?;

    assert_eq!(
        projection.latest().expect("checkpoint").files[0].availability,
        ControlledCheckpointFileAvailability::Restorable
    );
    Ok(())
}

fn append_file_operation(
    store: &JsonlSessionStore,
    operation_id: &str,
    path: &str,
    before_hash: Option<&str>,
    after_hash: Option<&str>,
    snapshot_coverage: SnapshotCoverage,
    committed: bool,
) -> Result<()> {
    let subject = MutationSubject::File {
        path: PathBuf::from(path),
        file_type: FileType::File,
    };
    append_payload(
        store,
        DurableEventType::MutationPrepared,
        &prepared(
            operation_id,
            subject.clone(),
            before_hash,
            after_hash,
            snapshot_coverage,
        ),
    )?;
    if committed {
        append_payload(
            store,
            DurableEventType::MutationCommitted,
            &MutationCommitted {
                operation_id: operation_id.to_owned(),
                batch_id: None,
                workspace_id: Some("workspace-1".to_owned()),
                observed_after_hash: after_hash.map(str::to_owned),
                workspace_revision: 1,
                workspace_snapshot_id: format!("snapshot-{operation_id}"),
                committed_subject: subject,
            },
        )?;
    }
    Ok(())
}

fn prepared(
    operation_id: &str,
    subject: MutationSubject,
    before_hash: Option<&str>,
    intended_after_hash: Option<&str>,
    snapshot_coverage: SnapshotCoverage,
) -> MutationPrepared {
    MutationPrepared {
        operation_id: operation_id.to_owned(),
        batch_id: None,
        tool_call_id: Some(format!("call-{operation_id}")),
        causation_event_id: format!("cause-{operation_id}"),
        subject,
        before_hash: before_hash.map(str::to_owned),
        intended_after_hash: intended_after_hash.map(str::to_owned),
        snapshot_coverage,
        workspace_id: "workspace-1".to_owned(),
        base_workspace_revision: 0,
        sync_class: MutationSyncClass::RecoveryCritical,
    }
}

fn append_payload<T: serde::Serialize>(
    store: &JsonlSessionStore,
    event_type: DurableEventType,
    payload: &T,
) -> Result<()> {
    store.append_event(
        event_type,
        EventClass::Critical,
        serde_json::to_value(payload)?,
    )?;
    Ok(())
}

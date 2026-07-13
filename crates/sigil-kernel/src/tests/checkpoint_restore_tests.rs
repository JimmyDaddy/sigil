use std::fs;

use anyhow::Result;

use super::*;
use crate::{
    ControlledCheckpointProjection, ControlledCheckpointRestoreRequest, DomainEvent,
    JsonlSessionStore, ModelMessage, SessionLogEntry, SnapshotCoverage, write_file_with_mutation,
};

#[test]
fn exact_checkpoint_restore_previews_and_restores_update_and_create() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let workspace = temp.path().join("workspace");
    let artifact_root = temp.path().join("artifacts");
    fs::create_dir(&workspace)?;
    let note = workspace.join("note.txt");
    let created = workspace.join("created.txt");
    fs::write(&note, "before\n")?;
    let store = JsonlSessionStore::new(temp.path().join("session.jsonl"))?;
    store.append(&SessionLogEntry::User(ModelMessage::user("edit files")))?;
    let recorder = MutationEventRecorder::with_artifact_root(store.clone(), artifact_root);
    write_file_with_mutation(
        Some(&recorder),
        &workspace,
        "call-note",
        "note.txt",
        &note,
        b"after\n",
    )?;
    write_file_with_mutation(
        Some(&recorder),
        &workspace,
        "call-created",
        "created.txt",
        &created,
        b"created\n",
    )?;
    let records = JsonlSessionStore::read_event_records(store.path())?;
    let checkpoint = ControlledCheckpointProjection::from_records(&records)?
        .latest()
        .cloned()
        .expect("checkpoint");
    let request = ControlledCheckpointRestoreRequest {
        checkpoint_id: checkpoint.checkpoint_id.clone(),
        checkpoint_digest: checkpoint.checkpoint_digest.clone(),
    };

    let preview = preview_controlled_checkpoint_restore(&recorder, &records, &workspace, &request)?;
    assert!(preview.ready, "{preview:?}");
    assert_eq!(preview.files.len(), 2);

    let output = execute_controlled_checkpoint_restore(&recorder, &records, &workspace, &request)?;

    assert_eq!(fs::read_to_string(&note)?, "before\n");
    assert!(!created.exists());
    assert_eq!(output.restored.len(), 2);
    let restored_records = JsonlSessionStore::read_event_records(store.path())?;
    assert_eq!(
        restored_records
            .iter()
            .map(|record| record.domain_event_record())
            .collect::<Result<Vec<_>>>()?
            .into_iter()
            .flatten()
            .filter(|record| matches!(record.event, DomainEvent::CheckpointRestored(_)))
            .count(),
        2
    );
    let after_projection = ControlledCheckpointProjection::from_records(&restored_records)?;
    let after_checkpoint = after_projection
        .checkpoint(&checkpoint.checkpoint_id)
        .expect("source checkpoint remains projected");
    assert_eq!(
        after_checkpoint.checkpoint_digest,
        checkpoint.checkpoint_digest
    );
    Ok(())
}

#[test]
fn exact_checkpoint_restore_records_hash_conflict_before_any_restore_write() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let workspace = temp.path().join("workspace");
    let artifact_root = temp.path().join("artifacts");
    fs::create_dir(&workspace)?;
    let note = workspace.join("note.txt");
    fs::write(&note, "before\n")?;
    let store = JsonlSessionStore::new(temp.path().join("session.jsonl"))?;
    store.append(&SessionLogEntry::User(ModelMessage::user("edit note")))?;
    let recorder = MutationEventRecorder::with_artifact_root(store.clone(), artifact_root);
    write_file_with_mutation(
        Some(&recorder),
        &workspace,
        "call-note",
        "note.txt",
        &note,
        b"after\n",
    )?;
    let records = JsonlSessionStore::read_event_records(store.path())?;
    let checkpoint = ControlledCheckpointProjection::from_records(&records)?
        .latest()
        .cloned()
        .expect("checkpoint");
    let request = ControlledCheckpointRestoreRequest {
        checkpoint_id: checkpoint.checkpoint_id,
        checkpoint_digest: checkpoint.checkpoint_digest,
    };
    fs::write(&note, "external change\n")?;

    let preview = preview_controlled_checkpoint_restore(&recorder, &records, &workspace, &request)?;
    assert!(!preview.ready);
    assert_eq!(
        preview.files[0].conflict_reason,
        Some(CheckpointRestoreConflictReason::CurrentHashMismatch)
    );
    let error = execute_controlled_checkpoint_restore(&recorder, &records, &workspace, &request)
        .expect_err("drift must fail before restore");

    assert!(error.to_string().contains("preflight found conflicts"));
    assert_eq!(fs::read_to_string(&note)?, "external change\n");
    let events = JsonlSessionStore::read_event_records(store.path())?
        .into_iter()
        .map(|record| record.domain_event_record())
        .collect::<Result<Vec<_>>>()?
        .into_iter()
        .flatten()
        .map(|record| record.event)
        .collect::<Vec<_>>();
    assert!(
        events
            .iter()
            .any(|event| matches!(event, DomainEvent::CheckpointRestoreConflict(_)))
    );
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, DomainEvent::CheckpointRestored(_)))
    );
    Ok(())
}

#[test]
fn exact_checkpoint_restore_fails_closed_for_sensitive_and_expired_snapshots() -> Result<()> {
    for expire_artifact in [false, true] {
        let temp = tempfile::tempdir()?;
        let workspace = temp.path().join("workspace");
        let artifact_root = temp.path().join("artifacts");
        fs::create_dir(&workspace)?;
        let relative = if expire_artifact { "note.txt" } else { ".env" };
        let target = workspace.join(relative);
        fs::write(&target, "before\n")?;
        let store = JsonlSessionStore::new(temp.path().join("session.jsonl"))?;
        store.append(&SessionLogEntry::User(ModelMessage::user("edit file")))?;
        let recorder = MutationEventRecorder::with_artifact_root(store.clone(), artifact_root);
        write_file_with_mutation(
            Some(&recorder),
            &workspace,
            "call-file",
            relative,
            &target,
            b"after\n",
        )?;
        if expire_artifact {
            let records = JsonlSessionStore::read_event_records(store.path())?;
            let projection = ControlledCheckpointProjection::from_records(&records)?;
            let checkpoint = projection.latest().expect("checkpoint");
            let SnapshotCoverage::Captured(artifact_id) =
                checkpoint.files[0].snapshot_coverage.clone()
            else {
                panic!("expected captured artifact");
            };
            recorder.expire_mutation_artifact(&artifact_id, "test expiration")?;
        }
        let records = JsonlSessionStore::read_event_records(store.path())?;
        let checkpoint = ControlledCheckpointProjection::from_records(&records)?
            .latest()
            .cloned()
            .expect("checkpoint");
        let request = ControlledCheckpointRestoreRequest {
            checkpoint_id: checkpoint.checkpoint_id,
            checkpoint_digest: checkpoint.checkpoint_digest,
        };

        let preview =
            preview_controlled_checkpoint_restore(&recorder, &records, &workspace, &request)?;

        assert!(!preview.ready, "{preview:?}");
        assert!(
            matches!(
                preview.files[0].conflict_reason,
                Some(CheckpointRestoreConflictReason::SensitiveSnapshot)
                    | Some(CheckpointRestoreConflictReason::ArtifactUnavailable)
            ),
            "{preview:?}"
        );
        assert_eq!(fs::read_to_string(&target)?, "after\n");
    }
    Ok(())
}

#[test]
fn exact_checkpoint_restore_records_stale_digest_as_invalid_binding() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let workspace = temp.path().join("workspace");
    fs::create_dir(&workspace)?;
    let note = workspace.join("note.txt");
    fs::write(&note, "before\n")?;
    let store = JsonlSessionStore::new(temp.path().join("session.jsonl"))?;
    store.append(&SessionLogEntry::User(ModelMessage::user("edit")))?;
    let recorder =
        MutationEventRecorder::with_artifact_root(store.clone(), temp.path().join("artifacts"));
    write_file_with_mutation(
        Some(&recorder),
        &workspace,
        "call-edit",
        "note.txt",
        &note,
        b"after\n",
    )?;
    let records = JsonlSessionStore::read_event_records(store.path())?;
    let checkpoint = ControlledCheckpointProjection::from_records(&records)?
        .latest()
        .cloned()
        .expect("checkpoint");
    let request = ControlledCheckpointRestoreRequest {
        checkpoint_id: checkpoint.checkpoint_id,
        checkpoint_digest: "sha256:stale".to_owned(),
    };

    execute_controlled_checkpoint_restore(&recorder, &records, &workspace, &request)
        .expect_err("stale digest must fail closed");

    let conflicts = JsonlSessionStore::read_event_records(store.path())?
        .into_iter()
        .map(|record| record.domain_event_record())
        .collect::<Result<Vec<_>>>()?
        .into_iter()
        .flatten()
        .filter_map(|record| match record.event {
            DomainEvent::CheckpointRestoreConflict(payload) => {
                serde_json::from_value::<CheckpointRestoreConflict>(payload.payload).ok()
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(conflicts.len(), 1);
    assert_eq!(
        conflicts[0].reason,
        CheckpointRestoreConflictReason::InvalidBinding
    );
    assert_eq!(fs::read_to_string(note)?, "after\n");
    Ok(())
}

use std::{fs, path::Path};

use anyhow::Result;

use crate::{
    DurableEventType, EventClass, JsonlSessionStore, ModelMessage, MutationBatchStatus,
    MutationEventRecorder, MutationObservedState, MutationReconciled, MutationResolution,
    MutationSubject, SessionLogEntry, SessionStreamRecord, ToolEffect, VerificationScope,
    WorkspaceKnowledge, WorkspaceMutationDetected, WorkspaceMutationDetectionReason,
    WorkspaceMutationScan, bytes_hash, delete_file_with_mutation,
    delete_file_with_mutation_in_batch, file_content_hash, write_file_with_mutation,
    write_file_with_mutation_in_batch,
};

fn stored_event_types(store: &JsonlSessionStore) -> Result<Vec<String>> {
    let mut event_types = Vec::new();
    for record in JsonlSessionStore::read_event_records(store.path())? {
        if let SessionStreamRecord::Stored(event) = record {
            event_types.push(event.event_type);
        }
    }
    Ok(event_types)
}

#[test]
fn controlled_write_records_prepare_commit_and_write_events() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let workspace = temp.path().join("workspace");
    fs::create_dir(&workspace)?;
    let store = JsonlSessionStore::new(temp.path().join("session.jsonl"))?;
    let recorder = MutationEventRecorder::new(store.clone());

    let committed = write_file_with_mutation(
        Some(&recorder),
        &workspace,
        "tool-call-1",
        "src/lib.rs",
        workspace.join("src/lib.rs"),
        b"pub fn ok() {}\n",
    )?
    .expect("durable recorder should return committed mutation");

    assert_eq!(
        fs::read_to_string(workspace.join("src/lib.rs"))?,
        "pub fn ok() {}\n"
    );
    assert_eq!(
        committed.observed_after_hash.as_deref(),
        file_content_hash(&workspace.join("src/lib.rs"))?.as_deref()
    );
    assert_eq!(
        stored_event_types(&store)?,
        vec![
            DurableEventType::MutationPrepared.as_str(),
            DurableEventType::MutationCommitted.as_str(),
            DurableEventType::WriteCommitted.as_str(),
        ]
    );
    Ok(())
}

#[test]
fn no_recorder_paths_keep_legacy_write_and_delete_behavior() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let workspace = temp.path().join("workspace");
    fs::create_dir(&workspace)?;
    let target = workspace.join("nested/note.txt");

    let write = write_file_with_mutation(
        None,
        &workspace,
        "tool-call-legacy",
        "nested/note.txt",
        &target,
        b"legacy",
    )?;
    assert!(write.is_none());
    assert_eq!(fs::read_to_string(&target)?, "legacy");

    let delete = delete_file_with_mutation(
        None,
        &workspace,
        "tool-call-legacy",
        "nested/note.txt",
        &target,
    )?;
    assert!(delete.is_none());
    assert!(!target.exists());
    Ok(())
}

#[test]
fn controlled_delete_and_batch_events_are_recorded() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let workspace = temp.path().join("workspace");
    fs::create_dir(&workspace)?;
    let first = workspace.join("a.txt");
    let second = workspace.join("b.txt");
    fs::write(&first, "a")?;
    let store = JsonlSessionStore::new(temp.path().join("session.jsonl"))?;
    let recorder = MutationEventRecorder::new(store.clone());
    recorder.append_batch_started(
        "batch-1",
        "batch-operation",
        &[
            MutationSubject::File {
                path: "a.txt".into(),
                file_type: crate::FileType::File,
            },
            MutationSubject::File {
                path: "b.txt".into(),
                file_type: crate::FileType::File,
            },
        ],
    )?;
    let deleted = delete_file_with_mutation_in_batch(
        Some(&recorder),
        &workspace,
        "tool-call-batch",
        Some("batch-1".to_owned()),
        "a.txt",
        &first,
    )?
    .expect("delete should commit through recorder");
    let written = write_file_with_mutation_in_batch(
        Some(&recorder),
        &workspace,
        "tool-call-batch",
        Some("batch-1".to_owned()),
        "b.txt",
        &second,
        b"b",
    )?
    .expect("write should commit through recorder");
    recorder.append_batch_finished(
        "batch-1",
        MutationBatchStatus::Applied,
        &[deleted.operation_id.clone(), written.operation_id.clone()],
        &[],
    )?;

    assert!(!first.exists());
    assert_eq!(fs::read_to_string(&second)?, "b");
    assert_eq!(deleted.batch_id.as_deref(), Some("batch-1"));
    assert_eq!(written.batch_id.as_deref(), Some("batch-1"));
    let event_types = stored_event_types(&store)?;
    assert!(event_types.contains(&DurableEventType::MutationBatchStarted.as_str().to_owned()));
    assert!(event_types.contains(&DurableEventType::MutationBatchFinished.as_str().to_owned()));
    assert_eq!(
        event_types
            .iter()
            .filter(|event_type| event_type.as_str() == DurableEventType::WriteCommitted.as_str())
            .count(),
        2
    );
    Ok(())
}

#[test]
fn controlled_commit_fails_when_file_changes_after_prepare() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let workspace = temp.path().join("workspace");
    fs::create_dir(&workspace)?;
    let target = workspace.join("note.txt");
    fs::write(&target, "old")?;
    let store = JsonlSessionStore::new(temp.path().join("session.jsonl"))?;
    let recorder = MutationEventRecorder::new(store.clone());
    let coordinator = recorder.coordinator(&workspace, "tool-call-2", None)?;
    let prepared = coordinator.prepare_file("note.txt", &target, Some(bytes_hash(b"new")))?;

    fs::write(&target, "external")?;
    let error = coordinator
        .commit_write(&prepared, b"new")
        .expect_err("CAS should reject external edits after prepare");

    assert!(
        error
            .to_string()
            .contains("file changed before controlled mutation commit")
    );
    assert_eq!(fs::read_to_string(&target)?, "external");
    assert_eq!(
        stored_event_types(&store)?,
        vec![DurableEventType::MutationPrepared.as_str()]
    );
    Ok(())
}

#[test]
fn controlled_mutation_rejects_escape_paths_and_mismatched_hashes() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let workspace = temp.path().join("workspace");
    fs::create_dir(&workspace)?;
    let target = workspace.join("note.txt");
    fs::write(&target, "old")?;
    let store = JsonlSessionStore::new(temp.path().join("session.jsonl"))?;
    let recorder = MutationEventRecorder::new(store);
    let coordinator = recorder.coordinator(&workspace, "tool-call-escape", None)?;

    let absolute_error = coordinator
        .prepare_file("/tmp/escape.txt", &target, Some(bytes_hash(b"new")))
        .expect_err("absolute relative path should be rejected");
    assert!(absolute_error.to_string().contains("workspace-relative"));

    let parent_error = coordinator
        .prepare_file("../escape.txt", &target, Some(bytes_hash(b"new")))
        .expect_err("parent path should be rejected");
    assert!(parent_error.to_string().contains("must not escape"));

    let prepared = coordinator.prepare_file("note.txt", &target, Some(bytes_hash(b"new")))?;
    let mismatch = coordinator
        .commit_write(&prepared, b"different")
        .expect_err("content must match prepared intended hash");
    assert!(mismatch.to_string().contains("intended hash"));

    let wrong_delete = coordinator
        .commit_delete(&prepared)
        .expect_err("delete requires a no-after-hash prepare");
    assert!(wrong_delete.to_string().contains("delete mutation"));
    Ok(())
}

#[test]
fn reconciliation_covers_terminal_not_applied_intended_and_conflict_states() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let workspace = temp.path().join("workspace");
    fs::create_dir(&workspace)?;
    let store = JsonlSessionStore::new(temp.path().join("session.jsonl"))?;
    let recorder = MutationEventRecorder::new(store.clone());

    let terminal_path = workspace.join("terminal.txt");
    fs::write(&terminal_path, "old")?;
    let terminal = recorder.coordinator(&workspace, "terminal-call", None)?;
    let terminal_prepared =
        terminal.prepare_file("terminal.txt", &terminal_path, Some(bytes_hash(b"new")))?;
    terminal.commit_write(&terminal_prepared, b"new")?;

    let not_applied_path = workspace.join("not-applied.txt");
    fs::write(&not_applied_path, "same")?;
    let not_applied = recorder.coordinator(&workspace, "not-applied-call", None)?;
    not_applied.prepare_file(
        "not-applied.txt",
        &not_applied_path,
        Some(bytes_hash(b"new")),
    )?;

    let intended_path = workspace.join("intended.txt");
    fs::write(&intended_path, "old")?;
    let intended = recorder.coordinator(&workspace, "intended-call", None)?;
    intended.prepare_file("intended.txt", &intended_path, Some(bytes_hash(b"new")))?;
    fs::write(&intended_path, "new")?;

    let conflict_path = workspace.join("conflict.txt");
    fs::write(&conflict_path, "old")?;
    let conflict = recorder.coordinator(&workspace, "conflict-call", None)?;
    conflict.prepare_file("conflict.txt", &conflict_path, Some(bytes_hash(b"new")))?;
    fs::write(&conflict_path, "different")?;

    let reconciled_terminal_path = workspace.join("reconciled-terminal.txt");
    fs::write(&reconciled_terminal_path, "old")?;
    let reconciled_terminal = recorder.coordinator(&workspace, "reconciled-terminal-call", None)?;
    let reconciled_terminal_prepared = reconciled_terminal.prepare_file(
        "reconciled-terminal.txt",
        &reconciled_terminal_path,
        Some(bytes_hash(b"new")),
    )?;
    recorder.append_reconciled(&MutationReconciled {
        operation_id: reconciled_terminal_prepared.operation_id.clone(),
        batch_id: None,
        observed_state: MutationObservedState::AppliedAsIntended,
        resolution: MutationResolution::MarkCommitted,
        workspace_revision: Some(7),
        workspace_snapshot_id: Some("snapshot-reconciled-terminal".to_owned()),
    })?;

    let reconciled = recorder.reconcile_prepared_mutations(&workspace)?;
    let payloads = reconciled
        .iter()
        .map(|event| serde_json::from_value::<MutationReconciled>(event.payload.clone()))
        .collect::<std::result::Result<Vec<_>, _>>()?;

    assert_eq!(payloads.len(), 3);
    assert!(payloads.iter().any(|payload| {
        payload.observed_state == MutationObservedState::NotApplied
            && payload.resolution == MutationResolution::MarkNotApplied
            && payload.workspace_revision.is_none()
    }));
    assert!(payloads.iter().any(|payload| {
        payload.observed_state == MutationObservedState::AppliedAsIntended
            && payload.resolution == MutationResolution::MarkCommitted
            && payload.workspace_revision.is_some()
            && payload.workspace_snapshot_id.is_some()
    }));
    assert!(payloads.iter().any(|payload| {
        payload.observed_state == MutationObservedState::AppliedDifferently
            && payload.resolution == MutationResolution::MarkConflict
            && payload.workspace_revision.is_some()
            && payload.workspace_snapshot_id.is_some()
    }));
    assert!(
        !payloads
            .iter()
            .any(|payload| payload.operation_id == terminal_prepared.operation_id)
    );
    assert!(
        !payloads
            .iter()
            .any(|payload| { payload.operation_id == reconciled_terminal_prepared.operation_id })
    );
    Ok(())
}

#[test]
fn reconciliation_skips_legacy_records_and_marks_non_file_subject_unknown() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let workspace = temp.path().join("workspace");
    fs::create_dir(&workspace)?;
    let store_path = temp.path().join("session.jsonl");
    let legacy_entry = SessionLogEntry::User(ModelMessage::user("legacy entry"));
    fs::write(
        &store_path,
        format!("{}\n", serde_json::to_string(&legacy_entry)?),
    )?;
    let store = JsonlSessionStore::new(store_path)?;
    let recorder = MutationEventRecorder::new(store.clone());
    recorder.append_prepared(&crate::MutationPrepared {
        operation_id: "operation-directory".to_owned(),
        batch_id: None,
        tool_call_id: Some("tool-call".to_owned()),
        causation_event_id: "event-cause".to_owned(),
        subject: MutationSubject::Directory { path: "src".into() },
        before_hash: None,
        intended_after_hash: None,
        snapshot_coverage: crate::SnapshotCoverage::Unsupported,
        workspace_id: "workspace".to_owned(),
        base_workspace_revision: 0,
        sync_class: crate::MutationSyncClass::RecoveryCritical,
    })?;

    let reconciled = recorder.reconcile_prepared_mutations(&workspace)?;

    assert_eq!(reconciled.len(), 1);
    let payload: MutationReconciled = serde_json::from_value(reconciled[0].payload.clone())?;
    assert_eq!(payload.operation_id, "operation-directory");
    assert_eq!(payload.observed_state, MutationObservedState::Unknown);
    assert_eq!(payload.resolution, MutationResolution::MarkUnknownDirty);
    Ok(())
}

#[test]
fn file_hash_reports_unreadable_non_file_paths() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let error = file_content_hash(temp.path()).expect_err("directory is not readable as a file");

    assert!(error.to_string().contains("failed to read"));
    Ok(())
}

#[test]
fn defensive_helpers_and_latest_revision_cover_error_edges() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let store_path = temp.path().join("session.jsonl");
    let legacy_entry = SessionLogEntry::User(ModelMessage::user("legacy entry"));
    fs::write(
        &store_path,
        format!("{}\n", serde_json::to_string(&legacy_entry)?),
    )?;
    let store = JsonlSessionStore::new(store_path)?;
    store.append_event(
        DurableEventType::MutationReconciled,
        EventClass::Critical,
        serde_json::to_value(MutationReconciled {
            operation_id: "operation-reconciled".to_owned(),
            batch_id: None,
            observed_state: MutationObservedState::AppliedAsIntended,
            resolution: MutationResolution::MarkCommitted,
            workspace_revision: Some(9),
            workspace_snapshot_id: Some("snapshot-reconciled".to_owned()),
        })?,
    )?;

    assert_eq!(super::latest_workspace_revision(&store, "workspace")?, 9);

    let mismatch = super::ensure_observed_after_hash_matches_intent(
        &Some("sha256:actual".to_owned()),
        "sha256:intended",
    )
    .expect_err("defensive hash mismatch should be reported");
    assert!(
        mismatch
            .to_string()
            .contains("observed file hash does not match intended hash")
    );
    assert!(
        super::ensure_observed_after_hash_matches_intent(
            &Some("sha256:intended".to_owned()),
            "sha256:intended"
        )
        .is_ok()
    );
    assert!(
        super::atomic_replace_error_message(
            Path::new("target.txt"),
            Path::new(".target.txt.sigil-tmp")
        )
        .contains("failed to atomically replace target.txt with .target.txt.sigil-tmp")
    );
    Ok(())
}

#[test]
fn reconciliation_reports_bad_workspace_and_malformed_mutation_payloads() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let workspace = temp.path().join("workspace");
    fs::create_dir(&workspace)?;
    let missing_workspace = temp.path().join("missing-workspace");
    let store = JsonlSessionStore::new(temp.path().join("session.jsonl"))?;
    let recorder = MutationEventRecorder::new(store.clone());

    let error = recorder
        .reconcile_prepared_mutations(&missing_workspace)
        .expect_err("missing workspace root should report canonicalize failure");
    assert!(error.to_string().contains("failed to canonicalize"));

    for (event_type, expected) in [
        (
            DurableEventType::MutationPrepared,
            DurableEventType::MutationPrepared.as_str(),
        ),
        (
            DurableEventType::MutationCommitted,
            DurableEventType::MutationCommitted.as_str(),
        ),
        (
            DurableEventType::MutationReconciled,
            DurableEventType::MutationReconciled.as_str(),
        ),
    ] {
        let bad_store = JsonlSessionStore::new(
            temp.path()
                .join(format!("bad-{}.jsonl", event_type.as_str())),
        )?;
        bad_store.append_event(event_type, EventClass::Critical, serde_json::json!({}))?;
        let bad_recorder = MutationEventRecorder::new(bad_store);
        let error = bad_recorder
            .reconcile_prepared_mutations(&workspace)
            .expect_err("malformed payload should fail closed");
        assert!(error.to_string().contains(expected));
    }

    Ok(())
}

#[test]
fn reconciliation_marks_prepared_write_that_never_reached_disk_as_not_applied() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let workspace = temp.path().join("workspace");
    fs::create_dir(&workspace)?;
    let target = workspace.join("note.txt");
    fs::write(&target, "old")?;
    let store = JsonlSessionStore::new(temp.path().join("session.jsonl"))?;
    let recorder = MutationEventRecorder::new(store.clone());
    let coordinator = recorder.coordinator(&workspace, "tool-call-3", None)?;
    let prepared = coordinator.prepare_file("note.txt", &target, Some(bytes_hash(b"new")))?;

    let reconciled = recorder.reconcile_prepared_mutations(&workspace)?;

    assert_eq!(reconciled.len(), 1);
    let payload: MutationReconciled = serde_json::from_value(reconciled[0].payload.clone())?;
    assert_eq!(payload.operation_id, prepared.operation_id);
    assert_eq!(payload.observed_state, MutationObservedState::NotApplied);
    assert_eq!(payload.resolution, MutationResolution::MarkNotApplied);
    assert_eq!(
        stored_event_types(&store)?,
        vec![
            DurableEventType::MutationPrepared.as_str(),
            DurableEventType::MutationReconciled.as_str(),
        ]
    );
    Ok(())
}

#[test]
fn reconciliation_marks_prepared_write_found_on_disk_as_committed() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let workspace = temp.path().join("workspace");
    fs::create_dir(&workspace)?;
    let target = workspace.join("note.txt");
    fs::write(&target, "old")?;
    let store = JsonlSessionStore::new(temp.path().join("session.jsonl"))?;
    let recorder = MutationEventRecorder::new(store.clone());
    let coordinator = recorder.coordinator(&workspace, "tool-call-4", None)?;
    let prepared = coordinator.prepare_file("note.txt", &target, Some(bytes_hash(b"new")))?;

    fs::write(&target, "new")?;
    let reconciled = recorder.reconcile_prepared_mutations(&workspace)?;

    assert_eq!(reconciled.len(), 1);
    let payload: MutationReconciled = serde_json::from_value(reconciled[0].payload.clone())?;
    assert_eq!(payload.operation_id, prepared.operation_id);
    assert_eq!(
        payload.observed_state,
        MutationObservedState::AppliedAsIntended
    );
    assert_eq!(payload.resolution, MutationResolution::MarkCommitted);
    assert!(payload.workspace_revision.is_some());
    assert!(payload.workspace_snapshot_id.is_some());
    Ok(())
}

#[test]
fn reconciliation_marks_unexpected_disk_state_as_conflict() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let workspace = temp.path().join("workspace");
    fs::create_dir(&workspace)?;
    let target = workspace.join("note.txt");
    fs::write(&target, "old")?;
    let store = JsonlSessionStore::new(temp.path().join("session.jsonl"))?;
    let recorder = MutationEventRecorder::new(store);
    let coordinator = recorder.coordinator(&workspace, "tool-call-5", None)?;
    let prepared = coordinator.prepare_file("note.txt", &target, Some(bytes_hash(b"new")))?;

    fs::write(&target, "different")?;
    let reconciled = recorder.reconcile_prepared_mutations(&workspace)?;

    assert_eq!(reconciled.len(), 1);
    let payload: MutationReconciled = serde_json::from_value(reconciled[0].payload.clone())?;
    assert_eq!(payload.operation_id, prepared.operation_id);
    assert_eq!(
        payload.observed_state,
        MutationObservedState::AppliedDifferently
    );
    assert_eq!(payload.resolution, MutationResolution::MarkConflict);
    Ok(())
}

#[test]
fn workspace_mutation_scan_records_changed_snapshot() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let workspace = temp.path().join("workspace");
    fs::create_dir(&workspace)?;
    fs::write(workspace.join("note.txt"), "old")?;
    let store = JsonlSessionStore::new(temp.path().join("session.jsonl"))?;
    let recorder = MutationEventRecorder::new(store);
    let scope = VerificationScope::all_tracked("scope-main");
    let before = recorder.capture_workspace_scan(&workspace, &scope)?;

    fs::write(workspace.join("note.txt"), "new")?;
    let event = recorder
        .record_workspace_mutation_if_changed(
            &before,
            &workspace,
            "call-shell",
            "bash",
            ToolEffect::Unknown,
        )?
        .expect("changed workspace should record mutation");
    let payload: WorkspaceMutationDetected = serde_json::from_value(event.payload)?;

    assert_eq!(payload.tool_call_id.as_deref(), Some("call-shell"));
    assert_eq!(payload.tool_name, "bash");
    assert_eq!(payload.scope_hash, "scope-main");
    assert_eq!(
        payload.reason,
        WorkspaceMutationDetectionReason::SnapshotChanged
    );
    assert!(!payload.unknown_dirty);
    assert!(payload.from_workspace_snapshot_id.is_some());
    assert!(payload.to_workspace_snapshot_id.is_some());
    assert_ne!(
        payload.from_workspace_snapshot_id,
        payload.to_workspace_snapshot_id
    );
    assert_eq!(payload.base_workspace_revision, 0);
    assert_eq!(payload.workspace_revision, 1);

    let post_detection = recorder.capture_workspace_scan(&workspace, &scope)?;
    assert_eq!(post_detection.workspace_revision, 1);
    Ok(())
}

#[test]
fn workspace_mutation_scan_ignores_excluded_build_artifacts() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let workspace = temp.path().join("workspace");
    fs::create_dir(&workspace)?;
    fs::write(workspace.join("note.txt"), "same")?;
    let store = JsonlSessionStore::new(temp.path().join("session.jsonl"))?;
    let recorder = MutationEventRecorder::new(store);
    let scope = VerificationScope::all_tracked("scope-main");
    let before = recorder.capture_workspace_scan(&workspace, &scope)?;

    fs::create_dir_all(workspace.join("target/debug"))?;
    fs::write(workspace.join("target/debug/generated"), "artifact")?;
    let event = recorder.record_workspace_mutation_if_changed(
        &before,
        &workspace,
        "call-build",
        "bash",
        ToolEffect::Unknown,
    )?;

    assert!(event.is_none());
    Ok(())
}

#[test]
fn workspace_mutation_scan_records_incomplete_snapshot_as_unknown_dirty() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let workspace = temp.path().join("workspace");
    fs::create_dir(&workspace)?;
    let store = JsonlSessionStore::new(temp.path().join("session.jsonl"))?;
    let recorder = MutationEventRecorder::new(store);
    let scope = VerificationScope::all_tracked("scope-main");
    let before = WorkspaceMutationScan {
        workspace_id: "workspace-1".to_owned(),
        scope_hash: "scope-main".to_owned(),
        scope: scope.clone(),
        workspace_revision: 3,
        workspace_snapshot_id: None,
        workspace_knowledge: WorkspaceKnowledge::UnknownDirty,
    };
    let after = WorkspaceMutationScan {
        workspace_id: "workspace-1".to_owned(),
        scope_hash: "scope-main".to_owned(),
        scope,
        workspace_revision: 3,
        workspace_snapshot_id: Some("snapshot-after".to_owned()),
        workspace_knowledge: WorkspaceKnowledge::Clean(3),
    };

    let event = recorder
        .record_workspace_mutation_scan_result(
            &before,
            &after,
            "call-shell",
            "bash",
            ToolEffect::Unknown,
        )?
        .expect("incomplete snapshot should record unknown dirty");
    let payload: WorkspaceMutationDetected = serde_json::from_value(event.payload)?;

    assert_eq!(
        payload.reason,
        WorkspaceMutationDetectionReason::SnapshotIncompleteBefore
    );
    assert!(payload.unknown_dirty);
    assert_eq!(payload.workspace_revision, 4);
    Ok(())
}

#[test]
fn workspace_mutation_scan_records_after_incomplete_snapshot_as_unknown_dirty() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let workspace = temp.path().join("workspace");
    fs::create_dir(&workspace)?;
    let store = JsonlSessionStore::new(temp.path().join("session.jsonl"))?;
    let recorder = MutationEventRecorder::new(store);
    let scope = VerificationScope::all_tracked("scope-main");
    let before = WorkspaceMutationScan {
        workspace_id: "workspace-1".to_owned(),
        scope_hash: "scope-main".to_owned(),
        scope: scope.clone(),
        workspace_revision: 7,
        workspace_snapshot_id: Some("snapshot-before".to_owned()),
        workspace_knowledge: WorkspaceKnowledge::Clean(7),
    };
    let after = WorkspaceMutationScan {
        workspace_id: "workspace-1".to_owned(),
        scope_hash: "scope-main".to_owned(),
        scope,
        workspace_revision: 7,
        workspace_snapshot_id: None,
        workspace_knowledge: WorkspaceKnowledge::UnknownDirty,
    };

    let event = recorder
        .record_workspace_mutation_scan_result(
            &before,
            &after,
            "call-shell",
            "bash",
            ToolEffect::Unknown,
        )?
        .expect("after incomplete snapshot should record unknown dirty");
    let payload: WorkspaceMutationDetected = serde_json::from_value(event.payload)?;

    assert_eq!(
        payload.reason,
        WorkspaceMutationDetectionReason::SnapshotIncompleteAfter
    );
    assert!(payload.unknown_dirty);
    assert_eq!(payload.workspace_revision, 8);
    Ok(())
}

#[test]
fn workspace_mutation_scan_unavailable_records_unknown_dirty() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let workspace = temp.path().join("workspace");
    fs::create_dir(&workspace)?;
    let store = JsonlSessionStore::new(temp.path().join("session.jsonl"))?;
    let recorder = MutationEventRecorder::new(store);

    let event = recorder.record_workspace_scan_unavailable(
        &workspace,
        "call-shell",
        "bash",
        ToolEffect::Unknown,
    )?;
    let payload: WorkspaceMutationDetected = serde_json::from_value(event.payload)?;

    assert_eq!(
        payload.reason,
        WorkspaceMutationDetectionReason::ScanUnavailable
    );
    assert!(payload.unknown_dirty);
    assert!(payload.from_workspace_snapshot_id.is_none());
    assert!(payload.to_workspace_snapshot_id.is_none());
    Ok(())
}

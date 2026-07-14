use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::Result;

use crate::{
    CheckpointRestored, DurableEventType, EventClass, JsonlSessionStore, ModelMessage,
    MutationArtifactCleanupRequested, MutationArtifactCleanupTarget,
    MutationArtifactLifecycleRecorded, MutationArtifactLifecycleStatus,
    MutationArtifactRetentionPolicy, MutationBatchStatus, MutationEventRecorder,
    MutationObservedState, MutationPrepared, MutationReconciled, MutationResolution,
    MutationSubject, SessionLogEntry, SessionStreamRecord, SnapshotCoverage, ToolEffect,
    VerificationScope, WorkspaceKnowledge, WorkspaceMutationDetected,
    WorkspaceMutationDetectionReason, WorkspaceMutationScan, bytes_hash,
    create_directory_with_mutation, delete_directory_with_mutation, delete_file_with_mutation,
    delete_file_with_mutation_in_batch, file_content_hash,
    restore_file_from_snapshot_with_mutation, stable_workspace_id, write_file_with_mutation,
    write_file_with_mutation_in_batch,
};

fn stored_event_types(store: &JsonlSessionStore) -> Result<Vec<String>> {
    let mut event_types = Vec::new();
    for record in JsonlSessionStore::read_event_records(store.path())? {
        let event = record.into_stored_event();
        event_types.push(event.event_type);
    }
    Ok(event_types)
}

fn artifact_files(root: &Path) -> Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    if !root.exists() {
        return Ok(files);
    }
    let mut pending = vec![root.to_path_buf()];
    while let Some(path) = pending.pop() {
        for entry in fs::read_dir(&path)? {
            let entry = entry?;
            let path = entry.path();
            if path.is_dir() {
                pending.push(path);
            } else {
                files.push(path);
            }
        }
    }
    files.sort();
    Ok(files)
}

#[test]
fn workspace_mutation_lease_serializes_regular_controlled_writes() -> Result<()> {
    let workspace = tempfile::tempdir()?;
    let state = tempfile::tempdir()?;
    let store = JsonlSessionStore::new(state.path().join("session.jsonl"))?;
    let recorder =
        MutationEventRecorder::with_artifact_root(store, state.path().join("mutation-artifacts"));
    let target = workspace.path().join("target.txt");
    fs::write(&target, b"before")?;
    let leased = recorder.coordinator_with_workspace_lease(
        workspace.path(),
        "prepared-call",
        Some("prepared-batch".to_owned()),
    )?;
    assert_eq!(leased.workspace_mutation_epoch()?, 0);

    let (started_tx, started_rx) = std::sync::mpsc::channel();
    let (done_tx, done_rx) = std::sync::mpsc::channel();
    let worker_recorder = recorder.clone();
    let workspace_root = workspace.path().to_path_buf();
    let worker_target = target.clone();
    let worker = std::thread::spawn(move || {
        started_tx.send(()).expect("worker start should signal");
        let result = write_file_with_mutation(
            Some(&worker_recorder),
            &workspace_root,
            "concurrent-call",
            "target.txt",
            &worker_target,
            b"after",
        );
        done_tx.send(result).expect("worker result should signal");
    });
    started_rx.recv_timeout(std::time::Duration::from_secs(1))?;
    assert!(
        matches!(
            done_rx.recv_timeout(std::time::Duration::from_millis(100)),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout)
        ),
        "ordinary controlled write must wait for the prepared mutation lease"
    );
    assert_eq!(fs::read(&target)?, b"before");

    drop(leased);
    done_rx.recv_timeout(std::time::Duration::from_secs(5))??;
    worker.join().expect("worker should not panic");
    assert_eq!(fs::read(&target)?, b"after");
    assert_eq!(
        recorder.current_workspace_mutation_epoch(workspace.path())?,
        1
    );
    Ok(())
}

#[test]
fn workspace_mutation_lease_cross_process_child() -> Result<()> {
    let Ok(workspace_root) = std::env::var("SIGIL_MUTATION_LEASE_CHILD_WORKSPACE") else {
        return Ok(());
    };
    let state_root = PathBuf::from(std::env::var("SIGIL_MUTATION_LEASE_CHILD_STATE")?);
    let workspace_root = PathBuf::from(workspace_root);
    let recorder = MutationEventRecorder::new(JsonlSessionStore::new(
        state_root.join("sessions/child.jsonl"),
    )?);
    let result = write_file_with_mutation(
        Some(&recorder),
        &workspace_root,
        "child-call",
        "target.txt",
        workspace_root.join("target.txt"),
        b"child",
    );
    assert!(
        result
            .expect_err("child must not acquire the parent workspace lease")
            .to_string()
            .contains("timed out acquiring workspace mutation lease")
    );
    Ok(())
}

#[test]
fn workspace_mutation_lease_is_cross_process_and_independent_of_temp_dir() -> Result<()> {
    let workspace = tempfile::tempdir()?;
    let state = tempfile::tempdir()?;
    let alternate_temp = tempfile::tempdir()?;
    fs::create_dir_all(state.path().join("sessions"))?;
    let target = workspace.path().join("target.txt");
    fs::write(&target, b"parent")?;
    let recorder = MutationEventRecorder::new(JsonlSessionStore::new(
        state.path().join("sessions/parent.jsonl"),
    )?);
    let leased = recorder.coordinator_with_workspace_lease(
        workspace.path(),
        "parent-call",
        Some("parent-batch".to_owned()),
    )?;

    let output = std::process::Command::new(std::env::current_exe()?)
        .arg("--exact")
        .arg("mutation::tests::workspace_mutation_lease_cross_process_child")
        .arg("--nocapture")
        .env("SIGIL_MUTATION_LEASE_CHILD_WORKSPACE", workspace.path())
        .env("SIGIL_MUTATION_LEASE_CHILD_STATE", state.path())
        .env("TMPDIR", alternate_temp.path())
        .output()?;
    drop(leased);

    assert!(
        output.status.success(),
        "child mutation lease fixture failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(fs::read(&target)?, b"parent");
    Ok(())
}

#[test]
fn mutation_recovery_lease_cross_process_child() -> Result<()> {
    let Ok(workspace_root) = std::env::var("SIGIL_MUTATION_RECOVERY_CHILD_WORKSPACE") else {
        return Ok(());
    };
    let session_path = PathBuf::from(std::env::var("SIGIL_MUTATION_RECOVERY_CHILD_SESSION")?);
    let recorder = MutationEventRecorder::new(JsonlSessionStore::new(session_path)?);
    let result = recorder.reconcile_prepared_mutations(workspace_root);
    assert!(
        result
            .expect_err("recovery must not bypass the parent workspace lease")
            .to_string()
            .contains("timed out acquiring workspace mutation lease")
    );
    Ok(())
}

#[test]
fn prepared_recovery_is_serialized_with_cross_process_commits() -> Result<()> {
    let workspace = tempfile::tempdir()?;
    let state = tempfile::tempdir()?;
    let alternate_temp = tempfile::tempdir()?;
    fs::create_dir_all(state.path().join("sessions"))?;
    let target = workspace.path().join("target.txt");
    fs::write(&target, b"before")?;
    let recovery_session = state.path().join("sessions/recovery.jsonl");
    {
        let recovery_recorder =
            MutationEventRecorder::new(JsonlSessionStore::new(&recovery_session)?);
        recovery_recorder
            .coordinator(workspace.path(), "pending-call", None)?
            .prepare_file_expected(
                "target.txt",
                &target,
                Some(bytes_hash(b"before")),
                Some(bytes_hash(b"after")),
            )?;
    }
    let parent_recorder = MutationEventRecorder::new(JsonlSessionStore::new(
        state.path().join("sessions/parent-recovery-lock.jsonl"),
    )?);
    let leased = parent_recorder.coordinator_with_workspace_lease(
        workspace.path(),
        "parent-call",
        Some("parent-batch".to_owned()),
    )?;

    let output = std::process::Command::new(std::env::current_exe()?)
        .arg("--exact")
        .arg("mutation::tests::mutation_recovery_lease_cross_process_child")
        .arg("--nocapture")
        .env("SIGIL_MUTATION_RECOVERY_CHILD_WORKSPACE", workspace.path())
        .env("SIGIL_MUTATION_RECOVERY_CHILD_SESSION", &recovery_session)
        .env("TMPDIR", alternate_temp.path())
        .output()?;
    drop(leased);

    assert!(
        output.status.success(),
        "child recovery lease fixture failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(fs::read(&target)?, b"before");
    Ok(())
}

#[test]
fn leased_coordinator_rejects_prepared_mutation_from_another_workspace() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let workspace_a = temp.path().join("workspace-a");
    let workspace_b = temp.path().join("workspace-b");
    fs::create_dir(&workspace_a)?;
    fs::create_dir(&workspace_b)?;
    let target_b = workspace_b.join("target.txt");
    fs::write(&target_b, b"before")?;
    let store = JsonlSessionStore::new(temp.path().join("session.jsonl"))?;
    let recorder = MutationEventRecorder::new(store.clone());
    let prepared_b = recorder
        .coordinator(&workspace_b, "workspace-b-call", None)?
        .prepare_file("target.txt", &target_b, Some(bytes_hash(b"after")))?;
    let leased_a = recorder.coordinator_with_workspace_lease(
        &workspace_a,
        "workspace-a-call",
        Some("workspace-a-batch".to_owned()),
    )?;

    let reconcile_error = leased_a
        .reconcile_prepared_file_from_disk(&prepared_b)
        .expect_err("workspace A lease must not reconcile workspace B");
    assert!(
        reconcile_error
            .to_string()
            .contains("belongs to a different workspace")
    );
    let commit_error = leased_a
        .commit_write(&prepared_b, b"after")
        .expect_err("workspace A lease must not commit workspace B");
    assert!(
        commit_error
            .to_string()
            .contains("belongs to a different workspace")
    );
    assert_eq!(fs::read(&target_b)?, b"before");
    assert_eq!(leased_a.workspace_mutation_epoch()?, 0);
    assert_eq!(
        stored_event_types(&store)?,
        vec![DurableEventType::MutationPrepared.as_str()]
    );
    Ok(())
}

#[test]
fn prepared_recovery_only_reconciles_the_leased_workspace() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let workspace_a = temp.path().join("workspace-a");
    let workspace_b = temp.path().join("workspace-b");
    fs::create_dir(&workspace_a)?;
    fs::create_dir(&workspace_b)?;
    let target_a = workspace_a.join("target.txt");
    let target_b = workspace_b.join("target.txt");
    fs::write(&target_a, b"before-a")?;
    fs::write(&target_b, b"before-b")?;
    let store = JsonlSessionStore::new(temp.path().join("session.jsonl"))?;
    let recorder = MutationEventRecorder::new(store);
    let prepared_a = recorder
        .coordinator(&workspace_a, "workspace-a-call", None)?
        .prepare_file("target.txt", &target_a, Some(bytes_hash(b"after-a")))?;
    let prepared_b = recorder
        .coordinator(&workspace_b, "workspace-b-call", None)?
        .prepare_file("target.txt", &target_b, Some(bytes_hash(b"after-b")))?;

    let reconciled_a = recorder.reconcile_prepared_mutations(&workspace_a)?;
    assert_eq!(reconciled_a.len(), 1);
    let payload_a: MutationReconciled = serde_json::from_value(reconciled_a[0].payload.clone())?;
    assert_eq!(payload_a.operation_id, prepared_a.operation_id);

    let reconciled_b = recorder.reconcile_prepared_mutations(&workspace_b)?;
    assert_eq!(reconciled_b.len(), 1);
    let payload_b: MutationReconciled = serde_json::from_value(reconciled_b[0].payload.clone())?;
    assert_eq!(payload_b.operation_id, prepared_b.operation_id);
    Ok(())
}

fn first_prepared_payload(store: &JsonlSessionStore) -> Result<MutationPrepared> {
    for record in JsonlSessionStore::read_event_records(store.path())? {
        let event = record.into_stored_event();
        if event.event_type == DurableEventType::MutationPrepared.as_str() {
            return serde_json::from_value(event.payload)
                .map_err(|error| anyhow::anyhow!("failed to decode mutation prepared: {error}"));
        }
    }
    anyhow::bail!("missing mutation prepared event")
}

fn checkpoint_restored_payloads(store: &JsonlSessionStore) -> Result<Vec<CheckpointRestored>> {
    let mut payloads = Vec::new();
    for record in JsonlSessionStore::read_event_records(store.path())? {
        let event = record.into_stored_event();
        if event.event_type == DurableEventType::CheckpointRestored.as_str() {
            payloads.push(serde_json::from_value(event.payload)?);
        }
    }
    Ok(payloads)
}

fn artifact_lifecycle_payloads(
    store: &JsonlSessionStore,
) -> Result<Vec<MutationArtifactLifecycleRecorded>> {
    let mut payloads = Vec::new();
    for record in JsonlSessionStore::read_event_records(store.path())? {
        let event = record.into_stored_event();
        if event.event_type == DurableEventType::MutationArtifactLifecycleRecorded.as_str() {
            payloads.push(serde_json::from_value(event.payload)?);
        }
    }
    Ok(payloads)
}

fn artifact_cleanup_request_payloads(
    store: &JsonlSessionStore,
) -> Result<Vec<MutationArtifactCleanupRequested>> {
    let mut payloads = Vec::new();
    for record in JsonlSessionStore::read_event_records(store.path())? {
        let event = record.into_stored_event();
        if event.event_type == DurableEventType::MutationArtifactCleanupRequested.as_str() {
            payloads.push(serde_json::from_value(event.payload)?);
        }
    }
    Ok(payloads)
}

fn captured_artifact_id(
    recorder: &MutationEventRecorder,
    workspace: &Path,
    file_name: &str,
    old_content: &str,
    new_content: &str,
) -> Result<String> {
    let target = workspace.join(file_name);
    fs::write(&target, old_content)?;
    let prepared = recorder
        .coordinator(workspace, file_name, None)?
        .prepare_file(file_name, &target, Some(bytes_hash(new_content.as_bytes())))?;
    for record in JsonlSessionStore::read_event_records(recorder.store.path())? {
        let event = record.into_stored_event();
        if event.event_type == DurableEventType::MutationPrepared.as_str() {
            let payload = serde_json::from_value::<MutationPrepared>(event.payload)?;
            if payload.operation_id != prepared.operation_id {
                continue;
            }
            let SnapshotCoverage::Captured(artifact_id) = payload.snapshot_coverage else {
                anyhow::bail!("expected captured artifact coverage");
            };
            return Ok(artifact_id);
        }
    }
    anyhow::bail!("missing captured artifact payload")
}

fn set_artifact_created_at_ms(
    artifact_root: &Path,
    artifact_id: &str,
    created_at_ms: u64,
) -> Result<()> {
    for path in artifact_files(artifact_root)?
        .into_iter()
        .filter(|path| path.extension().and_then(|ext| ext.to_str()) == Some("json"))
    {
        let mut value = serde_json::from_slice::<serde_json::Value>(&fs::read(&path)?)?;
        if value.get("artifact_id").and_then(|value| value.as_str()) != Some(artifact_id) {
            continue;
        }
        value["created_at_ms"] = serde_json::json!(created_at_ms);
        fs::write(&path, serde_json::to_vec_pretty(&value)?)?;
    }
    Ok(())
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
            DurableEventType::MutationPrepared.as_str(),
            DurableEventType::MutationCommitted.as_str(),
            DurableEventType::WriteCommitted.as_str(),
        ]
    );
    let records = JsonlSessionStore::read_event_records(store.path())?;
    let directory_prepared = records.iter().find_map(|record| match record {
        SessionStreamRecord::Stored(event)
            if event.event_type == DurableEventType::MutationPrepared.as_str() =>
        {
            serde_json::from_value::<MutationPrepared>(event.payload.clone()).ok()
        }
        _ => None,
    });
    assert!(matches!(
        directory_prepared.map(|payload| payload.subject),
        Some(MutationSubject::Directory { path }) if path == Path::new("src")
    ));
    Ok(())
}

#[test]
fn controlled_mutation_rejects_subject_absolute_path_mismatch() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let workspace = temp.path().join("workspace");
    fs::create_dir(&workspace)?;
    let store = JsonlSessionStore::new(temp.path().join("session.jsonl"))?;
    let recorder = MutationEventRecorder::new(store);
    let coordinator = recorder.coordinator(&workspace, "tool-call-1", None)?;
    let outside = temp.path().join("outside.txt");

    let error = coordinator
        .prepare_file("note.txt", &outside, Some(bytes_hash(b"new\n")))
        .expect_err("absolute path must match the relative mutation subject");

    assert!(
        error
            .to_string()
            .contains("does not match workspace subject")
    );
    Ok(())
}

#[test]
fn controlled_parent_directory_creation_handles_empty_and_escape_targets() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let workspace = temp.path().join("workspace");
    fs::create_dir(&workspace)?;
    let store = JsonlSessionStore::new(temp.path().join("session.jsonl"))?;
    let recorder = MutationEventRecorder::new(store);
    let coordinator = recorder.coordinator(&workspace, "tool-call-parent", None)?;

    assert!(
        coordinator
            .create_missing_parent_directories(Path::new(""))?
            .is_empty()
    );

    let outside_target = temp.path().join("outside/file.txt");
    let error = coordinator
        .create_missing_parent_directories(&outside_target)
        .expect_err("parent outside workspace should fail before mkdir");
    assert!(error.to_string().contains("outside workspace"));
    Ok(())
}

#[test]
fn controlled_prepare_captures_existing_file_artifact() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let workspace = temp.path().join("workspace");
    let artifact_root = temp.path().join("artifacts");
    fs::create_dir(&workspace)?;
    let target = workspace.join("note.txt");
    fs::write(&target, "old content")?;
    let store = JsonlSessionStore::new(temp.path().join("session.jsonl"))?;
    let recorder = MutationEventRecorder::with_artifact_root(store.clone(), &artifact_root);
    let coordinator = recorder.coordinator(&workspace, "tool-call-artifact", None)?;

    coordinator.prepare_file("note.txt", &target, Some(bytes_hash(b"new content")))?;

    let prepared = first_prepared_payload(&store)?;
    let SnapshotCoverage::Captured(artifact_id) = prepared.snapshot_coverage else {
        panic!("expected captured artifact coverage");
    };
    assert!(artifact_id.starts_with("mutation-artifact:sha256:"));
    let files = artifact_files(&artifact_root)?;
    let blob = files
        .iter()
        .find(|path| {
            path.extension().and_then(|ext| ext.to_str()) == Some("blob")
                && fs::read(path).is_ok_and(|bytes| bytes == b"old content")
        })
        .expect("captured old content blob should exist");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        assert_eq!(fs::metadata(blob)?.permissions().mode() & 0o777, 0o600);
    }
    assert!(
        files
            .iter()
            .any(|path| path.extension().and_then(|ext| ext.to_str()) == Some("json"))
    );
    Ok(())
}

#[test]
fn controlled_prepare_skips_sensitive_file_artifact() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let workspace = temp.path().join("workspace");
    let artifact_root = temp.path().join("artifacts");
    fs::create_dir(&workspace)?;
    let target = workspace.join(".env");
    fs::write(&target, "API_KEY=secret")?;
    let store = JsonlSessionStore::new(temp.path().join("session.jsonl"))?;
    let recorder = MutationEventRecorder::with_artifact_root(store.clone(), &artifact_root);
    let coordinator = recorder.coordinator(&workspace, "tool-call-sensitive", None)?;

    coordinator.prepare_file(".env", &target, Some(bytes_hash(b"API_KEY=new")))?;

    let prepared = first_prepared_payload(&store)?;
    assert_eq!(
        prepared.snapshot_coverage,
        SnapshotCoverage::SkippedSensitive
    );
    assert!(artifact_files(&artifact_root)?.is_empty());
    Ok(())
}

#[test]
fn controlled_prepare_skips_common_secret_like_artifacts() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let workspace = temp.path().join("workspace");
    let artifact_root = temp.path().join("artifacts");
    fs::create_dir(&workspace)?;
    let store = JsonlSessionStore::new(temp.path().join("session.jsonl"))?;
    let recorder = MutationEventRecorder::with_artifact_root(store, &artifact_root);
    let coordinator = recorder.coordinator(&workspace, "tool-call-secret-like", None)?;
    for (file_name, old_content, new_content) in [
        (
            ".npmrc",
            "//registry/:_authToken=old",
            "//registry/:_authToken=new",
        ),
        ("api_token.txt", "old token", "new token"),
        (
            "service-account.json",
            "{\"private_key\":\"old\"}",
            "{\"private_key\":\"new\"}",
        ),
    ] {
        let target = workspace.join(file_name);
        fs::write(&target, old_content)?;
        let prepared = coordinator.prepare_file(
            file_name,
            &target,
            Some(bytes_hash(new_content.as_bytes())),
        )?;
        assert_eq!(
            prepared.before_hash,
            Some(bytes_hash(old_content.as_bytes()))
        );
    }

    let prepared_events = JsonlSessionStore::read_event_records(coordinator.recorder.store.path())?
        .into_iter()
        .filter_map(|record| match record {
            SessionStreamRecord::Stored(event)
                if event.event_type == DurableEventType::MutationPrepared.as_str() =>
            {
                serde_json::from_value::<MutationPrepared>(event.payload).ok()
            }
            _ => None,
        })
        .collect::<Vec<_>>();

    assert_eq!(prepared_events.len(), 3);
    assert!(
        prepared_events
            .iter()
            .all(|payload| payload.snapshot_coverage == SnapshotCoverage::SkippedSensitive)
    );
    assert!(artifact_files(&artifact_root)?.is_empty());
    Ok(())
}

#[test]
fn controlled_prepare_repairs_truncated_existing_artifact_blob() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let workspace = temp.path().join("workspace");
    let artifact_root = temp.path().join("artifacts");
    fs::create_dir(&workspace)?;
    let target = workspace.join("note.txt");
    fs::write(&target, "old content")?;
    let first_store = JsonlSessionStore::new(temp.path().join("first.jsonl"))?;
    let first = MutationEventRecorder::with_artifact_root(first_store, &artifact_root);
    first
        .coordinator(&workspace, "tool-call-artifact", None)?
        .prepare_file("note.txt", &target, Some(bytes_hash(b"new content")))?;
    let blob = artifact_files(&artifact_root)?
        .into_iter()
        .find(|path| path.extension().and_then(|ext| ext.to_str()) == Some("blob"))
        .expect("first prepare should write blob");
    fs::write(&blob, "truncated")?;

    let second_store = JsonlSessionStore::new(temp.path().join("second.jsonl"))?;
    let second = MutationEventRecorder::with_artifact_root(second_store, &artifact_root);
    second
        .coordinator(&workspace, "tool-call-artifact", None)?
        .prepare_file("note.txt", &target, Some(bytes_hash(b"new content")))?;

    assert_eq!(fs::read(&blob)?, b"old content");
    Ok(())
}

#[test]
fn mutation_artifact_expire_removes_content_and_records_lifecycle_event() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let workspace = temp.path().join("workspace");
    let artifact_root = temp.path().join("artifacts");
    fs::create_dir(&workspace)?;
    let target = workspace.join("note.txt");
    fs::write(&target, "old content")?;
    let store = JsonlSessionStore::new(temp.path().join("session.jsonl"))?;
    let recorder = MutationEventRecorder::with_artifact_root(store.clone(), &artifact_root);
    recorder
        .coordinator(&workspace, "tool-call-artifact", None)?
        .prepare_file("note.txt", &target, Some(bytes_hash(b"new content")))?;
    let prepared = first_prepared_payload(&store)?;
    let SnapshotCoverage::Captured(artifact_id) = prepared.snapshot_coverage else {
        panic!("expected captured artifact coverage");
    };
    assert_eq!(artifact_files(&artifact_root)?.len(), 2);

    recorder.expire_mutation_artifact(&artifact_id, "retention policy")?;

    assert!(artifact_files(&artifact_root)?.is_empty());
    let payloads = artifact_lifecycle_payloads(&store)?;
    assert_eq!(payloads.len(), 1);
    assert_eq!(payloads[0].artifact_id, artifact_id);
    assert_eq!(payloads[0].status, MutationArtifactLifecycleStatus::Expired);
    assert_eq!(payloads[0].reason, "retention policy");
    assert_eq!(payloads[0].content_hash, Some(bytes_hash(b"old content")));
    assert_eq!(payloads[0].size, Some("old content".len() as u64));
    assert_eq!(payloads[0].operation_ids, vec![prepared.operation_id]);
    assert_eq!(payloads[0].source_paths, vec![PathBuf::from("note.txt")]);
    Ok(())
}

#[test]
fn mutation_artifact_delete_records_unavailable_when_content_is_missing() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let store = JsonlSessionStore::new(temp.path().join("session.jsonl"))?;
    let recorder =
        MutationEventRecorder::with_artifact_root(store.clone(), temp.path().join("artifacts"));

    recorder
        .delete_mutation_artifact("mutation-artifact:sha256:missing-content", "user cleanup")?;

    let payloads = artifact_lifecycle_payloads(&store)?;
    assert_eq!(payloads.len(), 1);
    assert_eq!(
        payloads[0].artifact_id,
        "mutation-artifact:sha256:missing-content"
    );
    assert_eq!(
        payloads[0].status,
        MutationArtifactLifecycleStatus::Unavailable
    );
    assert_eq!(payloads[0].reason, "user cleanup");
    assert_eq!(payloads[0].content_hash, None);
    assert!(payloads[0].operation_ids.is_empty());
    assert!(payloads[0].source_paths.is_empty());

    let bad_id_error = recorder
        .delete_mutation_artifact("not-a-mutation-artifact", "user cleanup")
        .expect_err("artifact lifecycle should reject unsupported ids");
    assert!(
        bad_id_error
            .to_string()
            .contains("unsupported mutation artifact id")
    );
    Ok(())
}

#[test]
fn mutation_artifact_retention_preview_is_read_only() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let workspace = temp.path().join("workspace");
    let artifact_root = temp.path().join("artifacts");
    fs::create_dir(&workspace)?;
    let store = JsonlSessionStore::new(temp.path().join("session.jsonl"))?;
    let recorder = MutationEventRecorder::with_artifact_root(store.clone(), &artifact_root);
    let old_artifact =
        captured_artifact_id(&recorder, &workspace, "old.txt", "old-one", "new-one")?;
    let middle_artifact =
        captured_artifact_id(&recorder, &workspace, "middle.txt", "old-two", "new-two")?;
    let new_artifact =
        captured_artifact_id(&recorder, &workspace, "new.txt", "old-three", "new-three")?;
    set_artifact_created_at_ms(&artifact_root, &old_artifact, 1)?;
    set_artifact_created_at_ms(&artifact_root, &middle_artifact, 20)?;
    set_artifact_created_at_ms(&artifact_root, &new_artifact, 30)?;

    let report = recorder.preview_artifact_retention_at(
        &MutationArtifactRetentionPolicy {
            max_artifacts: Some(1),
            max_bytes: None,
            expire_older_than_ms: Some(10),
        },
        15,
    )?;

    assert_eq!(report.scanned_artifacts, 3);
    assert_eq!(report.expired_artifacts, 2);
    assert_eq!(report.unavailable_artifacts, 0);
    assert!(report.lifecycle_events.is_empty());
    assert!(artifact_lifecycle_payloads(&store)?.is_empty());
    assert!(artifact_cleanup_request_payloads(&store)?.is_empty());
    assert_eq!(artifact_files(&artifact_root)?.len(), 6);

    let bytes_report = recorder.preview_artifact_retention_at(
        &MutationArtifactRetentionPolicy {
            max_artifacts: None,
            max_bytes: Some(8),
            expire_older_than_ms: None,
        },
        30,
    )?;
    assert_eq!(bytes_report.scanned_artifacts, 3);
    assert_eq!(bytes_report.expired_artifacts, 3);
    assert_eq!(bytes_report.expired_bytes, bytes_report.scanned_bytes);
    Ok(())
}

#[test]
fn mutation_artifact_inventory_lists_metadata_without_mutation() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let workspace = temp.path().join("workspace");
    let artifact_root = temp.path().join("artifacts");
    fs::create_dir(&workspace)?;
    let store = JsonlSessionStore::new(temp.path().join("session.jsonl"))?;
    let recorder = MutationEventRecorder::with_artifact_root(store.clone(), &artifact_root);
    let artifact_id = captured_artifact_id(&recorder, &workspace, "note.txt", "old", "new")?;
    set_artifact_created_at_ms(&artifact_root, &artifact_id, 42)?;

    let inventory = recorder.list_mutation_artifacts()?;

    assert_eq!(inventory.len(), 1);
    assert_eq!(inventory[0].artifact_id, artifact_id);
    assert_eq!(inventory[0].size, 3);
    assert_eq!(inventory[0].created_at_ms, Some(42));
    assert!(inventory[0].blob_available);
    assert_eq!(inventory[0].operation_ids.len(), 1);
    assert_eq!(inventory[0].source_paths, vec![PathBuf::from("note.txt")]);
    assert!(artifact_lifecycle_payloads(&store)?.is_empty());
    assert_eq!(artifact_files(&artifact_root)?.len(), 2);
    Ok(())
}

#[test]
fn mutation_artifact_retention_public_wrappers_record_cleanup_request() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let workspace = temp.path().join("workspace");
    let artifact_root = temp.path().join("artifacts");
    fs::create_dir(&workspace)?;
    let store = JsonlSessionStore::new(temp.path().join("session.jsonl"))?;
    let recorder = MutationEventRecorder::with_artifact_root(store.clone(), &artifact_root);
    let artifact_id = captured_artifact_id(&recorder, &workspace, "note.txt", "old", "new")?;

    let preview =
        recorder.preview_artifact_retention(&MutationArtifactRetentionPolicy::default())?;

    assert_eq!(preview.scanned_artifacts, 1);
    assert_eq!(preview.expired_artifacts, 0);
    assert!(preview.lifecycle_events.is_empty());
    assert!(artifact_cleanup_request_payloads(&store)?.is_empty());
    assert_eq!(artifact_files(&artifact_root)?.len(), 2);

    let report = recorder.enforce_artifact_retention(&MutationArtifactRetentionPolicy {
        max_artifacts: Some(0),
        max_bytes: None,
        expire_older_than_ms: None,
    })?;

    assert_eq!(report.scanned_artifacts, 1);
    assert_eq!(report.expired_artifacts, 1);
    assert_eq!(report.lifecycle_events.len(), 1);
    assert!(artifact_files(&artifact_root)?.is_empty());
    let cleanup_requests = artifact_cleanup_request_payloads(&store)?;
    assert_eq!(cleanup_requests.len(), 1);
    assert_eq!(cleanup_requests[0].candidate_artifacts, 1);
    let lifecycle_payloads = artifact_lifecycle_payloads(&store)?;
    assert_eq!(lifecycle_payloads.len(), 1);
    assert_eq!(lifecycle_payloads[0].artifact_id, artifact_id);
    assert_eq!(
        lifecycle_payloads[0].status,
        MutationArtifactLifecycleStatus::Expired
    );
    Ok(())
}

#[test]
fn mutation_artifact_cleanup_expired_and_unavailable_targets_cover_selection_modes() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let workspace = temp.path().join("workspace");
    let artifact_root = temp.path().join("artifacts");
    fs::create_dir(&workspace)?;
    let store = JsonlSessionStore::new(temp.path().join("session.jsonl"))?;
    let recorder = MutationEventRecorder::with_artifact_root(store.clone(), &artifact_root);
    let old_artifact =
        captured_artifact_id(&recorder, &workspace, "old.txt", "old-one", "new-one")?;
    let fresh_artifact =
        captured_artifact_id(&recorder, &workspace, "fresh.txt", "old-two", "new-two")?;
    set_artifact_created_at_ms(&artifact_root, &old_artifact, 1)?;
    set_artifact_created_at_ms(&artifact_root, &fresh_artifact, 30)?;

    let expired_preview = recorder.preview_artifact_cleanup_at(
        &MutationArtifactCleanupTarget::Expired,
        &MutationArtifactRetentionPolicy {
            max_artifacts: None,
            max_bytes: None,
            expire_older_than_ms: Some(10),
        },
        20,
    )?;
    assert_eq!(expired_preview.scanned_artifacts, 2);
    assert_eq!(expired_preview.expired_artifacts, 1);

    let quota_preview = recorder.preview_artifact_cleanup_at(
        &MutationArtifactCleanupTarget::Expired,
        &MutationArtifactRetentionPolicy {
            max_artifacts: Some(0),
            max_bytes: None,
            expire_older_than_ms: None,
        },
        20,
    )?;
    assert_eq!(quota_preview.expired_artifacts, 2);

    let unavailable_artifact_root = temp.path().join("unavailable-artifacts");
    let unavailable_store = JsonlSessionStore::new(temp.path().join("unavailable.jsonl"))?;
    let unavailable_recorder = MutationEventRecorder::with_artifact_root(
        unavailable_store.clone(),
        &unavailable_artifact_root,
    );
    let unavailable_artifact = captured_artifact_id(
        &unavailable_recorder,
        &workspace,
        "missing.txt",
        "old",
        "new",
    )?;
    for path in artifact_files(&unavailable_artifact_root)?
        .into_iter()
        .filter(|path| path.extension().and_then(|ext| ext.to_str()) == Some("blob"))
    {
        fs::remove_file(path)?;
    }

    let unavailable_report = unavailable_recorder.enforce_artifact_cleanup_at(
        &MutationArtifactCleanupTarget::Unavailable,
        &MutationArtifactRetentionPolicy::default(),
        20,
    )?;

    assert_eq!(unavailable_report.unavailable_artifacts, 1);
    assert_eq!(unavailable_report.lifecycle_events.len(), 1);
    let lifecycle_payloads = artifact_lifecycle_payloads(&unavailable_store)?;
    assert_eq!(lifecycle_payloads.len(), 1);
    assert_eq!(lifecycle_payloads[0].artifact_id, unavailable_artifact);
    assert_eq!(
        lifecycle_payloads[0].status,
        MutationArtifactLifecycleStatus::Unavailable
    );
    Ok(())
}

#[test]
fn mutation_artifact_retention_scanner_applies_age_and_quota() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let workspace = temp.path().join("workspace");
    let artifact_root = temp.path().join("artifacts");
    fs::create_dir(&workspace)?;
    let store = JsonlSessionStore::new(temp.path().join("session.jsonl"))?;
    let recorder = MutationEventRecorder::with_artifact_root(store.clone(), &artifact_root);
    let old_artifact =
        captured_artifact_id(&recorder, &workspace, "old.txt", "old-one", "new-one")?;
    let middle_artifact =
        captured_artifact_id(&recorder, &workspace, "middle.txt", "old-two", "new-two")?;
    let new_artifact =
        captured_artifact_id(&recorder, &workspace, "new.txt", "old-three", "new-three")?;
    set_artifact_created_at_ms(&artifact_root, &old_artifact, 1)?;
    set_artifact_created_at_ms(&artifact_root, &middle_artifact, 20)?;
    set_artifact_created_at_ms(&artifact_root, &new_artifact, 30)?;

    let report = recorder.enforce_artifact_retention_at(
        &MutationArtifactRetentionPolicy {
            max_artifacts: Some(1),
            max_bytes: None,
            expire_older_than_ms: Some(10),
        },
        15,
    )?;

    assert_eq!(report.scanned_artifacts, 3);
    assert_eq!(report.expired_artifacts, 2);
    assert_eq!(report.unavailable_artifacts, 0);
    assert_eq!(report.lifecycle_events.len(), 2);
    let requests = artifact_cleanup_request_payloads(&store)?;
    assert_eq!(requests.len(), 1);
    assert_eq!(
        requests[0].target,
        MutationArtifactCleanupTarget::Recommended
    );
    assert_eq!(requests[0].scanned_artifacts, 3);
    assert_eq!(requests[0].candidate_artifacts, 2);
    let payloads = artifact_lifecycle_payloads(&store)?;
    assert_eq!(payloads.len(), 2);
    assert_eq!(payloads[0].artifact_id, old_artifact);
    assert_eq!(payloads[0].status, MutationArtifactLifecycleStatus::Expired);
    assert_eq!(payloads[0].reason, "retention age limit");
    assert_eq!(payloads[1].artifact_id, middle_artifact);
    assert_eq!(payloads[1].status, MutationArtifactLifecycleStatus::Expired);
    assert_eq!(payloads[1].reason, "retention quota limit");
    let new_digest = &new_artifact["mutation-artifact:sha256:".len()..];
    let remaining = artifact_files(&artifact_root)?;
    assert_eq!(remaining.len(), 2);
    assert!(remaining.iter().all(|path| {
        path.file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.contains(new_digest))
    }));
    Ok(())
}

#[test]
fn mutation_artifact_retention_scanner_records_unavailable_content() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let workspace = temp.path().join("workspace");
    let artifact_root = temp.path().join("artifacts");
    fs::create_dir(&workspace)?;
    let store = JsonlSessionStore::new(temp.path().join("session.jsonl"))?;
    let recorder = MutationEventRecorder::with_artifact_root(store.clone(), &artifact_root);
    let artifact_id = captured_artifact_id(
        &recorder,
        &workspace,
        "note.txt",
        "old content",
        "new content",
    )?;
    for path in artifact_files(&artifact_root)?
        .into_iter()
        .filter(|path| path.extension().and_then(|ext| ext.to_str()) == Some("blob"))
    {
        fs::remove_file(path)?;
    }

    let report =
        recorder.enforce_artifact_retention_at(&MutationArtifactRetentionPolicy::default(), 1)?;

    assert_eq!(report.scanned_artifacts, 1);
    assert_eq!(report.expired_artifacts, 0);
    assert_eq!(report.unavailable_artifacts, 1);
    assert_eq!(report.lifecycle_events.len(), 1);
    assert!(artifact_files(&artifact_root)?.is_empty());
    let payloads = artifact_lifecycle_payloads(&store)?;
    assert_eq!(payloads.len(), 1);
    assert_eq!(payloads[0].artifact_id, artifact_id);
    assert_eq!(
        payloads[0].status,
        MutationArtifactLifecycleStatus::Unavailable
    );
    assert_eq!(
        payloads[0].reason,
        "retention scan found unavailable content"
    );
    Ok(())
}

#[test]
fn mutation_artifact_cleanup_targets_select_coarse_groups() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let workspace = temp.path().join("workspace");
    let artifact_root = temp.path().join("artifacts");
    fs::create_dir(&workspace)?;
    let primary_store = JsonlSessionStore::new(temp.path().join("primary.jsonl"))?;
    let primary = MutationEventRecorder::with_artifact_root(primary_store.clone(), &artifact_root);
    let artifact_id = captured_artifact_id(&primary, &workspace, "note.txt", "old", "new")?;
    let workspace_id = stable_workspace_id(&workspace)?;

    let workspace_preview = primary.preview_artifact_cleanup(
        &MutationArtifactCleanupTarget::Workspace(workspace_id),
        &MutationArtifactRetentionPolicy::default(),
    )?;

    assert_eq!(workspace_preview.scanned_artifacts, 1);
    assert_eq!(workspace_preview.deleted_artifacts, 1);
    assert_eq!(workspace_preview.deleted_bytes, 3);
    assert_eq!(workspace_preview.expired_artifacts, 0);
    assert_eq!(workspace_preview.unavailable_artifacts, 0);
    assert!(workspace_preview.lifecycle_events.is_empty());
    assert!(artifact_lifecycle_payloads(&primary_store)?.is_empty());

    let secondary_store = JsonlSessionStore::new(temp.path().join("secondary.jsonl"))?;
    let secondary =
        MutationEventRecorder::with_artifact_root(secondary_store.clone(), &artifact_root);
    let unreferenced_preview = secondary.preview_artifact_cleanup(
        &MutationArtifactCleanupTarget::Unreferenced,
        &MutationArtifactRetentionPolicy::default(),
    )?;

    assert_eq!(unreferenced_preview.scanned_artifacts, 1);
    assert_eq!(unreferenced_preview.deleted_artifacts, 1);
    assert_eq!(unreferenced_preview.deleted_bytes, 3);

    let report = secondary.enforce_artifact_cleanup(
        &MutationArtifactCleanupTarget::Unreferenced,
        &MutationArtifactRetentionPolicy::default(),
    )?;

    assert_eq!(report.deleted_artifacts, 1);
    assert_eq!(report.deleted_bytes, 3);
    assert_eq!(report.lifecycle_events.len(), 1);
    assert!(artifact_files(&artifact_root)?.is_empty());
    let payloads = artifact_lifecycle_payloads(&secondary_store)?;
    assert_eq!(payloads.len(), 1);
    assert_eq!(payloads[0].artifact_id, artifact_id);
    assert_eq!(payloads[0].status, MutationArtifactLifecycleStatus::Deleted);
    assert_eq!(
        payloads[0].reason,
        "artifact metadata is not referenced by session events"
    );
    Ok(())
}

#[test]
fn controlled_directory_create_and_delete_record_directory_subjects() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let workspace = temp.path().join("workspace");
    fs::create_dir(&workspace)?;
    let target = workspace.join("empty-dir");
    let store = JsonlSessionStore::new(temp.path().join("session.jsonl"))?;
    let recorder = MutationEventRecorder::new(store.clone());

    let created = create_directory_with_mutation(
        Some(&recorder),
        &workspace,
        "tool-call-dir-create",
        "empty-dir",
        &target,
    )?
    .expect("directory create should produce mutation evidence");
    assert!(target.is_dir());
    assert!(created.observed_after_hash.is_some());

    let deleted = delete_directory_with_mutation(
        Some(&recorder),
        &workspace,
        "tool-call-dir-delete",
        "empty-dir",
        &target,
    )?
    .expect("directory delete should produce mutation evidence");
    assert!(!target.exists());
    assert!(deleted.observed_after_hash.is_none());

    let committed_subjects = JsonlSessionStore::read_event_records(store.path())?
        .into_iter()
        .filter_map(|record| match record {
            SessionStreamRecord::Stored(event)
                if event.event_type == DurableEventType::MutationCommitted.as_str() =>
            {
                serde_json::from_value::<crate::MutationCommitted>(event.payload)
                    .ok()
                    .map(|payload| payload.committed_subject)
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(
        committed_subjects,
        vec![
            MutationSubject::Directory {
                path: PathBuf::from("empty-dir")
            },
            MutationSubject::Directory {
                path: PathBuf::from("empty-dir")
            },
        ]
    );
    Ok(())
}

#[test]
fn controlled_directory_delete_rejects_non_empty_before_prepare() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let workspace = temp.path().join("workspace");
    fs::create_dir(&workspace)?;
    let target = workspace.join("non-empty-dir");
    fs::create_dir(&target)?;
    fs::write(target.join("child.txt"), "child\n")?;
    let store = JsonlSessionStore::new(temp.path().join("session.jsonl"))?;
    let recorder = MutationEventRecorder::new(store.clone());

    let error = delete_directory_with_mutation(
        Some(&recorder),
        &workspace,
        "tool-call-dir-delete",
        "non-empty-dir",
        &target,
    )
    .expect_err("recursive directory delete is outside controlled mutation MVP");

    assert!(error.to_string().contains("non-empty directory delete"));
    assert!(target.join("child.txt").exists());
    let prepared_events = JsonlSessionStore::read_event_records(store.path())?
        .into_iter()
        .filter(|record| match record {
            SessionStreamRecord::Stored(event) => {
                event.event_type == DurableEventType::MutationPrepared.as_str()
            }
        })
        .count();
    assert_eq!(prepared_events, 0);
    Ok(())
}

#[test]
fn checkpoint_restore_captured_artifact_records_new_mutation() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let workspace = temp.path().join("workspace");
    let artifact_root = temp.path().join("artifacts");
    fs::create_dir(&workspace)?;
    let target = workspace.join("note.txt");
    fs::write(&target, "old content")?;
    let store = JsonlSessionStore::new(temp.path().join("session.jsonl"))?;
    let recorder = MutationEventRecorder::with_artifact_root(store.clone(), &artifact_root);

    let changed = write_file_with_mutation(
        Some(&recorder),
        &workspace,
        "tool-call-write",
        "note.txt",
        &target,
        b"new content",
    )?
    .expect("write should produce mutation evidence");
    let coverage = first_prepared_payload(&store)?.snapshot_coverage;
    assert_eq!(fs::read_to_string(&target)?, "new content");

    let restored = restore_file_from_snapshot_with_mutation(
        &recorder,
        &workspace,
        "tool-call-restore",
        "note.txt",
        &target,
        coverage.clone(),
        changed.observed_after_hash.as_deref(),
    )?;

    assert_eq!(fs::read_to_string(&target)?, "old content");
    assert_eq!(restored.restored_from, coverage);
    assert_eq!(
        stored_event_types(&store)?,
        vec![
            DurableEventType::MutationPrepared.as_str(),
            DurableEventType::MutationCommitted.as_str(),
            DurableEventType::WriteCommitted.as_str(),
            DurableEventType::MutationPrepared.as_str(),
            DurableEventType::MutationCommitted.as_str(),
            DurableEventType::WriteCommitted.as_str(),
            DurableEventType::CheckpointRestored.as_str(),
        ]
    );
    let restored_payloads = checkpoint_restored_payloads(&store)?;
    assert_eq!(restored_payloads.len(), 1);
    assert_eq!(
        restored_payloads[0].mutation_committed_event_id,
        restored.committed.committed_event.event_id
    );
    assert_eq!(
        restored_payloads[0].workspace_snapshot_id,
        restored.committed.workspace_snapshot_id
    );
    Ok(())
}

#[test]
fn checkpoint_restore_skipped_sensitive_snapshot_fails_without_writing() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let workspace = temp.path().join("workspace");
    let artifact_root = temp.path().join("artifacts");
    fs::create_dir(&workspace)?;
    let target = workspace.join(".env");
    fs::write(&target, "API_KEY=old")?;
    let store = JsonlSessionStore::new(temp.path().join("session.jsonl"))?;
    let recorder = MutationEventRecorder::with_artifact_root(store.clone(), &artifact_root);
    let changed = write_file_with_mutation(
        Some(&recorder),
        &workspace,
        "tool-call-write",
        ".env",
        &target,
        b"API_KEY=new",
    )?
    .expect("write should produce mutation evidence");
    let coverage = first_prepared_payload(&store)?.snapshot_coverage;
    assert_eq!(coverage, SnapshotCoverage::SkippedSensitive);

    let error = restore_file_from_snapshot_with_mutation(
        &recorder,
        &workspace,
        "tool-call-restore",
        ".env",
        &target,
        coverage,
        changed.observed_after_hash.as_deref(),
    )
    .expect_err("sensitive snapshot should not be restorable");

    assert!(error.to_string().contains("skipped sensitive snapshot"));
    assert_eq!(fs::read_to_string(&target)?, "API_KEY=new");
    assert!(checkpoint_restored_payloads(&store)?.is_empty());
    Ok(())
}

#[test]
fn checkpoint_restore_no_prior_content_deletes_created_file() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let workspace = temp.path().join("workspace");
    let artifact_root = temp.path().join("artifacts");
    fs::create_dir(&workspace)?;
    let target = workspace.join("created.txt");
    let store = JsonlSessionStore::new(temp.path().join("session.jsonl"))?;
    let recorder = MutationEventRecorder::with_artifact_root(store.clone(), &artifact_root);

    let changed = write_file_with_mutation(
        Some(&recorder),
        &workspace,
        "tool-call-write",
        "created.txt",
        &target,
        b"created content",
    )?
    .expect("write should produce mutation evidence");
    let coverage = first_prepared_payload(&store)?.snapshot_coverage;
    assert_eq!(coverage, SnapshotCoverage::NoPriorContent);
    assert!(target.exists());

    let restored = restore_file_from_snapshot_with_mutation(
        &recorder,
        &workspace,
        "tool-call-restore",
        "created.txt",
        &target,
        coverage.clone(),
        changed.observed_after_hash.as_deref(),
    )?;

    assert!(!target.exists());
    assert_eq!(restored.restored_from, coverage);
    assert_eq!(checkpoint_restored_payloads(&store)?.len(), 1);

    let missing_error = restore_file_from_snapshot_with_mutation(
        &recorder,
        &workspace,
        "tool-call-restore-missing",
        "created.txt",
        &target,
        SnapshotCoverage::NoPriorContent,
        None,
    )
    .expect_err("absent target cannot be deleted again");
    assert!(
        missing_error
            .to_string()
            .contains("checkpoint restore target already absent")
    );
    Ok(())
}

#[test]
fn checkpoint_restore_rejects_unsupported_unavailable_and_corrupt_artifacts() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let workspace = temp.path().join("workspace");
    let artifact_root = temp.path().join("artifacts");
    fs::create_dir(&workspace)?;
    let target = workspace.join("note.txt");
    fs::write(&target, "old content")?;
    let store = JsonlSessionStore::new(temp.path().join("session.jsonl"))?;
    let recorder = MutationEventRecorder::with_artifact_root(store.clone(), &artifact_root);
    let changed = write_file_with_mutation(
        Some(&recorder),
        &workspace,
        "tool-call-write",
        "note.txt",
        &target,
        b"new content",
    )?
    .expect("write should produce mutation evidence");
    let coverage = first_prepared_payload(&store)?.snapshot_coverage;

    for unsupported in [SnapshotCoverage::Unsupported, SnapshotCoverage::Unavailable] {
        let error = restore_file_from_snapshot_with_mutation(
            &recorder,
            &workspace,
            "tool-call-restore",
            "note.txt",
            &target,
            unsupported,
            changed.observed_after_hash.as_deref(),
        )
        .expect_err("unsupported coverage should not restore");
        assert!(error.to_string().contains("checkpoint restore snapshot"));
    }

    let SnapshotCoverage::Captured(ref artifact_id) = coverage else {
        panic!("expected captured artifact coverage");
    };
    let digest = &artifact_id["mutation-artifact:sha256:".len()..];
    for path in artifact_files(&artifact_root)?.into_iter().filter(|path| {
        path.file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name == format!("{digest}.blob"))
    }) {
        fs::write(path, "corrupt artifact content")?;
    }
    let error = restore_file_from_snapshot_with_mutation(
        &recorder,
        &workspace,
        "tool-call-restore",
        "note.txt",
        &target,
        coverage,
        changed.observed_after_hash.as_deref(),
    )
    .expect_err("corrupt artifact content should fail checksum validation");
    assert!(error.to_string().contains("mutation artifact not found"));
    Ok(())
}

#[test]
fn checkpoint_restore_rejects_current_hash_conflict_before_prepare() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let workspace = temp.path().join("workspace");
    let artifact_root = temp.path().join("artifacts");
    fs::create_dir(&workspace)?;
    let target = workspace.join("note.txt");
    fs::write(&target, "old content")?;
    let store = JsonlSessionStore::new(temp.path().join("session.jsonl"))?;
    let recorder = MutationEventRecorder::with_artifact_root(store.clone(), &artifact_root);
    let changed = write_file_with_mutation(
        Some(&recorder),
        &workspace,
        "tool-call-write",
        "note.txt",
        &target,
        b"new content",
    )?
    .expect("write should produce mutation evidence");
    let coverage = first_prepared_payload(&store)?.snapshot_coverage;
    fs::write(&target, "external edit")?;

    let error = restore_file_from_snapshot_with_mutation(
        &recorder,
        &workspace,
        "tool-call-restore",
        "note.txt",
        &target,
        coverage,
        changed.observed_after_hash.as_deref(),
    )
    .expect_err("restore should reject stale current hash");

    assert!(
        error
            .to_string()
            .contains("file changed before checkpoint restore")
    );
    assert_eq!(fs::read_to_string(&target)?, "external edit");
    assert!(checkpoint_restored_payloads(&store)?.is_empty());
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
fn checkpoint_restore_rejects_subject_absolute_path_mismatch_before_read() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let workspace = temp.path().join("workspace");
    let artifact_root = temp.path().join("artifacts");
    fs::create_dir(&workspace)?;
    let outside = temp.path().join("outside.txt");
    let store = JsonlSessionStore::new(temp.path().join("session.jsonl"))?;
    let recorder = MutationEventRecorder::with_artifact_root(store.clone(), &artifact_root);

    let error = restore_file_from_snapshot_with_mutation(
        &recorder,
        &workspace,
        "tool-call-restore",
        "note.txt",
        &outside,
        SnapshotCoverage::NoPriorContent,
        None,
    )
    .expect_err("absolute path must match the relative restore subject");

    assert!(
        error
            .to_string()
            .contains("does not match workspace subject")
    );
    assert!(stored_event_types(&store)?.is_empty());
    Ok(())
}

#[test]
fn workspace_local_session_artifacts_default_outside_workspace_sigil_dir() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let workspace = temp.path().join("workspace");
    let workspace_local_session = workspace.join(".sigil/sessions/session.jsonl");

    let root = super::default_mutation_artifact_root(&workspace_local_session);

    assert!(!root.starts_with(workspace.join(".sigil")));
    assert!(root.ends_with(Path::new("artifacts").join("mutations")));
    Ok(())
}

#[test]
fn no_recorder_paths_reject_workspace_mutations() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let workspace = temp.path().join("workspace");
    fs::create_dir(&workspace)?;
    let target = workspace.join("nested/note.txt");

    let write = write_file_with_mutation(
        None,
        &workspace,
        "tool-call",
        "nested/note.txt",
        &target,
        b"content",
    )
    .expect_err("missing recorder should reject file writes");
    assert!(write.to_string().contains("mutation recorder is required"));
    assert!(!target.exists());

    fs::create_dir_all(target.parent().expect("nested parent"))?;
    fs::write(&target, "content")?;
    let delete =
        delete_file_with_mutation(None, &workspace, "tool-call", "nested/note.txt", &target)
            .expect_err("missing recorder should reject file deletes");
    assert!(delete.to_string().contains("mutation recorder is required"));
    assert!(target.exists());

    let dir = workspace.join("tracked-dir");
    let created_dir =
        create_directory_with_mutation(None, &workspace, "tool-call-dir", "tracked-dir", &dir)
            .expect_err("missing recorder should reject directory creates");
    assert!(
        created_dir
            .to_string()
            .contains("mutation recorder is required")
    );
    assert!(!dir.exists());

    fs::create_dir(&dir)?;
    let deleted_dir =
        delete_directory_with_mutation(None, &workspace, "tool-call-dir", "tracked-dir", &dir)
            .expect_err("missing recorder should reject directory deletes");
    assert!(
        deleted_dir
            .to_string()
            .contains("mutation recorder is required")
    );
    assert!(dir.exists());
    Ok(())
}

#[test]
fn controlled_directory_commits_reject_wrong_intent_and_blocking_parents() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let workspace = temp.path().join("workspace");
    fs::create_dir(&workspace)?;
    let store = JsonlSessionStore::new(temp.path().join("session.jsonl"))?;
    let recorder = MutationEventRecorder::new(store);
    let coordinator = recorder.coordinator(&workspace, "tool-call-dir-errors", None)?;

    let create = coordinator.prepare_directory(
        "dir-create",
        workspace.join("dir-create"),
        Some("sha256:not-directory-present".to_owned()),
    )?;
    let create_error = coordinator
        .commit_create_directory(&create)
        .expect_err("directory create should reject wrong intended hash");
    assert!(
        create_error
            .to_string()
            .contains("directory create mutation must intend")
    );
    let delete_error = coordinator
        .commit_delete_directory(&create)
        .expect_err("directory delete should reject an intended hash");
    assert!(
        delete_error
            .to_string()
            .contains("directory delete mutation must not have")
    );

    fs::write(workspace.join("blocked"), "not a directory")?;
    let parent_error = coordinator
        .create_missing_parent_directories(&workspace.join("blocked/child.txt"))
        .expect_err("file parent should block controlled mkdir");
    assert!(
        parent_error
            .to_string()
            .contains("parent path is not a directory")
    );

    let file_target = workspace.join("not-a-dir");
    fs::write(&file_target, "file")?;
    let prepare_error = coordinator
        .prepare_directory("not-a-dir", &file_target, None)
        .expect_err("directory prepare should reject file subjects");
    assert!(
        prepare_error
            .to_string()
            .contains("path is not a directory")
    );

    let race_target = workspace.join("race-dir");
    let race = coordinator.prepare_directory(
        "race-dir",
        &race_target,
        Some(super::directory_present_hash()),
    )?;
    fs::create_dir(&race_target)?;
    let race_error = coordinator
        .commit_create_directory(&race)
        .expect_err("external mkdir should trip directory CAS");
    assert!(
        race_error
            .to_string()
            .contains("directory changed before controlled mutation commit")
    );
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
fn reconciliation_marks_workspace_subject_unknown_without_prior_workspace_snapshot() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let workspace = temp.path().join("workspace");
    fs::create_dir(&workspace)?;
    let store = JsonlSessionStore::new(temp.path().join("session.jsonl"))?;
    let recorder = MutationEventRecorder::new(store.clone());
    recorder.append_prepared(&crate::MutationPrepared {
        operation_id: "operation-workspace".to_owned(),
        batch_id: None,
        tool_call_id: Some("tool-call".to_owned()),
        causation_event_id: "event-cause".to_owned(),
        subject: MutationSubject::Workspace {
            scope_hash: "scope-main".to_owned(),
        },
        before_hash: None,
        intended_after_hash: None,
        snapshot_coverage: crate::SnapshotCoverage::Unsupported,
        workspace_id: stable_workspace_id(&workspace)?,
        base_workspace_revision: 0,
        sync_class: crate::MutationSyncClass::RecoveryCritical,
    })?;

    let reconciled = recorder.reconcile_prepared_mutations(&workspace)?;

    assert_eq!(reconciled.len(), 1);
    let payload: MutationReconciled = serde_json::from_value(reconciled[0].payload.clone())?;
    assert_eq!(payload.operation_id, "operation-workspace");
    assert_eq!(payload.observed_state, MutationObservedState::Unknown);
    assert_eq!(payload.resolution, MutationResolution::MarkUnknownDirty);
    Ok(())
}

#[test]
fn reconciliation_classifies_directory_prepared_without_commit() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let workspace = temp.path().join("workspace");
    fs::create_dir(&workspace)?;
    let store = JsonlSessionStore::new(temp.path().join("session.jsonl"))?;
    let recorder = MutationEventRecorder::new(store.clone());
    let coordinator = recorder.coordinator(&workspace, "tool-call-dir", None)?;
    let prepared = coordinator.prepare_directory(
        "created-dir",
        workspace.join("created-dir"),
        Some(bytes_hash(b"sigil:directory:present:v1")),
    )?;
    fs::create_dir(workspace.join("created-dir"))?;

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
fn file_hash_reports_unreadable_non_file_paths() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let error = file_content_hash(temp.path()).expect_err("directory is not readable as a file");

    assert!(error.to_string().contains("failed to read"));
    Ok(())
}

#[test]
fn defensive_helpers_and_latest_revision_cover_error_edges() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let store = JsonlSessionStore::new(temp.path().join("session.jsonl"))?;
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
fn external_process_mutation_scan_result_records_changed_snapshot_without_tool_call() -> Result<()>
{
    let temp = tempfile::tempdir()?;
    let workspace = temp.path().join("workspace");
    fs::create_dir(&workspace)?;
    fs::write(workspace.join("note.txt"), "old")?;
    let store = JsonlSessionStore::new(temp.path().join("session.jsonl"))?;
    let recorder = MutationEventRecorder::new(store);
    let scope = VerificationScope::all_tracked("scope-main");
    let before = recorder.capture_workspace_scan(&workspace, &scope)?;

    assert!(
        recorder
            .record_external_process_mutation_scan_result(
                &before,
                &before,
                "mcp_server:clean",
                ToolEffect::Unknown,
                std::collections::BTreeMap::new(),
            )?
            .is_none(),
        "unchanged external process scan must not create mutation evidence"
    );

    fs::write(workspace.join("note.txt"), "new")?;
    let after = recorder.capture_workspace_scan(&workspace, &scope)?;
    let event = recorder
        .record_external_process_mutation_scan_result(
            &before,
            &after,
            "mcp_server:filesystem",
            ToolEffect::Unknown,
            std::collections::BTreeMap::from([(
                "mcp_startup_result".to_owned(),
                "startup_failed".to_owned(),
            )]),
        )?
        .expect("changed external process scan should record mutation");
    let payload: WorkspaceMutationDetected = serde_json::from_value(event.payload)?;

    assert_eq!(payload.tool_call_id, None);
    assert_eq!(payload.tool_name, "mcp_server:filesystem");
    assert_eq!(
        payload.reason,
        WorkspaceMutationDetectionReason::SnapshotChanged
    );
    assert!(!payload.unknown_dirty);
    assert_eq!(
        payload
            .metadata
            .get("mcp_startup_result")
            .map(String::as_str),
        Some("startup_failed")
    );
    Ok(())
}

#[test]
fn execution_mutation_profile_captures_pre_execution_snapshot() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let workspace = temp.path().join("workspace");
    fs::create_dir(&workspace)?;
    fs::write(workspace.join("note.txt"), "old")?;
    let store = JsonlSessionStore::new(temp.path().join("session.jsonl"))?;
    let recorder = MutationEventRecorder::new(store);
    let scope = VerificationScope::all_tracked("scope-main");

    let profile = recorder.execution_mutation_profile(
        &workspace,
        &scope,
        "call-shell",
        "bash",
        ToolEffect::Unknown,
    )?;

    assert_eq!(profile.tool_call_id, "call-shell");
    assert_eq!(profile.tool_name, "bash");
    assert_eq!(profile.effect, ToolEffect::Unknown);
    assert_eq!(profile.scan_scope_hash, "scope-main");
    assert_eq!(profile.pre_execution_workspace_revision, 0);
    assert!(profile.pre_execution_snapshot_id.is_some());
    assert_eq!(profile.workspace_knowledge, WorkspaceKnowledge::Clean(0));
    Ok(())
}

#[test]
fn execution_mutation_profile_reconcile_ignores_prior_session_entries_when_clean() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let workspace = temp.path().join("workspace");
    fs::create_dir(&workspace)?;
    fs::write(workspace.join("note.txt"), "same")?;
    let store = JsonlSessionStore::new(temp.path().join("session.jsonl"))?;
    store.append(&SessionLogEntry::User(ModelMessage::user(
        "raw before profile",
    )))?;
    let recorder = MutationEventRecorder::new(store);
    let scope = VerificationScope::all_tracked("scope-main");
    let profile = recorder.execution_mutation_profile(
        &workspace,
        &scope,
        "call-shell",
        "bash",
        ToolEffect::Unknown,
    )?;

    let event = recorder.reconcile_execution_mutation_profile(&workspace, &profile)?;

    assert!(event.is_none());
    Ok(())
}

#[test]
fn execution_mutation_profile_reconcile_records_later_change_after_existing_detection() -> Result<()>
{
    let temp = tempfile::tempdir()?;
    let workspace = temp.path().join("workspace");
    fs::create_dir(&workspace)?;
    fs::write(workspace.join("note.txt"), "old")?;
    let store = JsonlSessionStore::new(temp.path().join("session.jsonl"))?;
    let recorder = MutationEventRecorder::new(store.clone());
    let scope = VerificationScope::all_tracked("scope-main");
    let profile = recorder.execution_mutation_profile(
        &workspace,
        &scope,
        "call-terminal-start",
        "terminal_start",
        ToolEffect::Unknown,
    )?;
    let before = WorkspaceMutationScan {
        workspace_id: profile.workspace_id.clone(),
        scope_hash: profile.scan_scope_hash.clone(),
        scope: scope.clone(),
        workspace_revision: profile.pre_execution_workspace_revision,
        workspace_snapshot_id: profile.pre_execution_snapshot_id.clone(),
        workspace_knowledge: profile.workspace_knowledge.clone(),
    };

    fs::write(workspace.join("note.txt"), "early")?;
    let first = recorder
        .record_workspace_mutation_if_changed(
            &before,
            &workspace,
            "call-terminal-start",
            "terminal_start",
            ToolEffect::Unknown,
        )?
        .expect("first terminal mutation should be recorded");
    fs::write(workspace.join("note.txt"), "late")?;

    let second = recorder
        .reconcile_execution_mutation_profile(&workspace, &profile)?
        .expect("later terminal mutation should not be skipped by earlier detection");

    let first_payload: WorkspaceMutationDetected = serde_json::from_value(first.payload)?;
    let second_payload: WorkspaceMutationDetected = serde_json::from_value(second.payload)?;
    assert_eq!(first_payload.tool_call_id, second_payload.tool_call_id);
    assert_ne!(
        first_payload.to_workspace_snapshot_id,
        second_payload.to_workspace_snapshot_id
    );
    let detection_count = JsonlSessionStore::read_event_records(store.path())?
        .into_iter()
        .filter(|record| {
            matches!(
                record,
                SessionStreamRecord::Stored(event)
                    if event.event_type == DurableEventType::WorkspaceMutationDetected.as_str()
            )
        })
        .count();
    assert_eq!(detection_count, 2);
    Ok(())
}

#[test]
fn execution_mutation_profile_reconcile_records_scan_unavailable() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let workspace = temp.path().join("workspace");
    fs::create_dir(&workspace)?;
    fs::write(workspace.join("note.txt"), "old")?;
    let store = JsonlSessionStore::new(temp.path().join("session.jsonl"))?;
    let recorder = MutationEventRecorder::new(store);
    let scope = VerificationScope::all_tracked("scope-main");
    let profile = recorder.execution_mutation_profile(
        &workspace,
        &scope,
        "call-shell",
        "bash",
        ToolEffect::Unknown,
    )?;
    fs::remove_dir_all(&workspace)?;
    fs::write(&workspace, "not a directory")?;

    let event = recorder
        .reconcile_execution_mutation_profile(&workspace, &profile)?
        .expect("unavailable workspace scan should record unknown dirty mutation");
    let payload: WorkspaceMutationDetected = serde_json::from_value(event.payload)?;

    assert_eq!(
        payload.reason,
        WorkspaceMutationDetectionReason::ScanUnavailable
    );
    assert_eq!(payload.tool_call_id.as_deref(), Some("call-shell"));
    assert_eq!(payload.tool_name, "bash");
    assert!(payload.unknown_dirty);
    assert!(
        recorder
            .reconcile_execution_mutation_profile(&workspace, &profile)?
            .is_none(),
        "same scan-unavailable evidence should not be duplicated"
    );
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

#[test]
fn workspace_mutation_scan_unavailable_after_preserves_before_snapshot() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let store = JsonlSessionStore::new(temp.path().join("session.jsonl"))?;
    let recorder = MutationEventRecorder::new(store);
    let before = WorkspaceMutationScan {
        workspace_id: "workspace-1".to_owned(),
        scope_hash: "scope-main".to_owned(),
        scope: VerificationScope::all_tracked("scope-main"),
        workspace_revision: 7,
        workspace_snapshot_id: Some("snapshot-before".to_owned()),
        workspace_knowledge: WorkspaceKnowledge::Clean(7),
    };

    let event = recorder.record_workspace_scan_unavailable_after(
        &before,
        "call-shell",
        "bash",
        ToolEffect::Unknown,
    )?;
    let payload: WorkspaceMutationDetected = serde_json::from_value(event.payload)?;

    assert_eq!(
        payload.reason,
        WorkspaceMutationDetectionReason::ScanUnavailable
    );
    assert_eq!(
        payload.from_workspace_snapshot_id.as_deref(),
        Some("snapshot-before")
    );
    assert!(payload.to_workspace_snapshot_id.is_none());
    assert_eq!(payload.base_workspace_revision, 7);
    assert_eq!(payload.workspace_revision, 8);
    assert!(payload.unknown_dirty);
    Ok(())
}

#[test]
fn external_process_unknown_dirty_records_without_tool_call() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let workspace = temp.path().join("workspace");
    fs::create_dir(&workspace)?;
    let store = JsonlSessionStore::new(temp.path().join("session.jsonl"))?;
    let recorder = MutationEventRecorder::new(store);

    let event = recorder.record_external_process_unknown_dirty(
        &workspace,
        "mcp_server:docs",
        ToolEffect::Unknown,
    )?;
    let payload: WorkspaceMutationDetected = serde_json::from_value(event.payload)?;

    assert_eq!(payload.tool_call_id, None);
    assert_eq!(payload.tool_name, "mcp_server:docs");
    assert_eq!(
        payload.reason,
        WorkspaceMutationDetectionReason::DeclaredWriteEffect
    );
    assert!(payload.unknown_dirty);
    assert!(payload.from_workspace_snapshot_id.is_none());
    assert!(payload.to_workspace_snapshot_id.is_none());
    Ok(())
}

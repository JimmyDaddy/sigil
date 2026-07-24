use anyhow::Result;
use serde_json::json;

use crate::{
    ChangeSet, ChangeSetFile, ChangeSetFileAction, ChangeSetFileResultStatus, ChangeSetId,
    ChangeSetResultStatus, ChangeSetRisk, ControlEntry, DurableEventType, EventClass,
    IsolatedChangeSetProduced, IsolatedWorkspaceBackend, IsolatedWorkspaceCleanupRecorded,
    IsolatedWorkspaceCleanupStatus, IsolatedWorkspaceCreated, IsolatedWorkspacePrepared,
    JsonlSessionStore, MergeDecision, MergeReviewId, MergeReviewParentMutationRequest,
    MergeReviewRequested, MergeReviewResolved, MutationBatchStatus, MutationSubject,
    ParentChangeSetMutationRequest, Session, SessionLogEntry, StoredEvent, TypedDomainEvent,
    TypedStoredEventDecode, WriteIsolationMode, WriteIsolationProjection, WriteLeaseAcquired,
    WriteLeaseId, WriteLeaseReleaseStatus, WriteLeaseReleased, WriteLeaseScope,
    apply_parent_changeset_mutation_batch, bytes_hash, decode_typed_stored_event,
    resolve_merge_review_parent_mutation,
};

fn lease_id() -> WriteLeaseId {
    WriteLeaseId::new("lease-1").expect("valid lease id")
}

fn review_id() -> MergeReviewId {
    MergeReviewId::new("review-1").expect("valid review id")
}

fn change_set_id() -> ChangeSetId {
    ChangeSetId::new("change-1").expect("valid changeset id")
}

fn note_change_set(id: ChangeSetId) -> ChangeSet {
    ChangeSet {
        id,
        title: "Update note".to_owned(),
        summary: "Update note.txt".to_owned(),
        risk: ChangeSetRisk::Low,
        files: vec![ChangeSetFile {
            path: "note.txt".to_owned(),
            previous_path: None,
            action: ChangeSetFileAction::Update,
            risk: ChangeSetRisk::Low,
            before_hash: None,
            after_hash: None,
            diff_hash: None,
            additions: 1,
            deletions: 1,
            validations: Vec::new(),
        }],
        validations: Vec::new(),
    }
}

fn note_diff() -> String {
    "--- a/note.txt\n+++ b/note.txt\n@@ -1,1 +1,1 @@\n-old\n+new\n".to_owned()
}

fn stored_event_types(store: &JsonlSessionStore) -> Result<Vec<String>> {
    let mut event_types = Vec::new();
    for record in JsonlSessionStore::read_event_records(store.path())? {
        let event = record.into_stored_event();
        event_types.push(event.event_type);
    }
    Ok(event_types)
}

fn artifact_digest(content: &str) -> String {
    bytes_hash(content.as_bytes())
}

fn append_merge_review_request(
    session: &mut Session,
    change_set_id: ChangeSetId,
    workspace_root: &std::path::Path,
) -> Result<()> {
    session.append_control(ControlEntry::MergeReviewRequested(MergeReviewRequested {
        review_id: review_id(),
        changeset_id: change_set_id,
        parent_workspace_snapshot_id: super::parent_workspace_snapshot_id(workspace_root)?,
    }))
}

fn acquired_entry() -> WriteLeaseAcquired {
    WriteLeaseAcquired {
        lease_id: lease_id(),
        workspace_id: "workspace-parent".to_owned(),
        owner_agent_id: "agent-main".to_owned(),
        isolation_mode: WriteIsolationMode::SharedWorkspaceExclusive,
        scope: WriteLeaseScope::Subjects(vec![MutationSubject::Workspace {
            scope_hash: "scope-main".to_owned(),
        }]),
    }
}

fn stored_control_event(
    event_type: DurableEventType,
    control: ControlEntry,
    stream_sequence: u64,
) -> StoredEvent {
    StoredEvent::new(
        event_type,
        event_type
            .expected_event_class()
            .expect("write-isolation event type should have a class"),
        format!("event-{stream_sequence}"),
        "session-1".to_owned(),
        stream_sequence,
        json!({ "session_log_entry": SessionLogEntry::Control(control) }),
    )
    .expect("stored control event should build")
}

#[test]
fn write_isolation_modes_have_stable_labels() {
    assert_eq!(
        WriteIsolationMode::SharedWorkspaceExclusive.as_str(),
        "shared_workspace_exclusive"
    );
    assert_eq!(WriteIsolationMode::ChangesetOnly.as_str(), "changeset_only");
    assert_eq!(WriteIsolationMode::Worktree.as_str(), "worktree");
    assert_eq!(WriteLeaseReleaseStatus::Interrupted.as_str(), "interrupted");
    assert_eq!(
        IsolatedWorkspaceBackend::GitWorktree.as_str(),
        "git_worktree"
    );
    assert_eq!(
        IsolatedWorkspaceCleanupStatus::AlreadyMissing.as_str(),
        "already_missing"
    );
    assert!(IsolatedWorkspaceCleanupStatus::Removed.is_terminal());
    assert!(!IsolatedWorkspaceCleanupStatus::Failed.is_terminal());
    assert_eq!(MergeDecision::Accepted.as_str(), "accepted");
}

#[test]
fn write_isolation_stable_ids_reject_path_like_values() {
    assert!(WriteLeaseId::new("lease_1").is_ok());
    assert!(MergeReviewId::new("review.1").is_ok());
    assert!(WriteLeaseId::new("../lease").is_err());
    assert!(MergeReviewId::new("review/1").is_err());
}

#[test]
fn write_isolation_projection_tracks_lease_and_merge_review_state() {
    let acquired = acquired_entry();
    let release = WriteLeaseReleased {
        lease_id: acquired.lease_id.clone(),
        status: WriteLeaseReleaseStatus::Completed,
    };
    let isolated = IsolatedWorkspaceCreated {
        isolated_workspace_id: "workspace-child".to_owned(),
        parent_workspace_id: acquired.workspace_id.clone(),
        owner_agent_id: "agent-child".to_owned(),
        isolation_mode: WriteIsolationMode::Worktree,
        base_snapshot_id: "snapshot-base".to_owned(),
        backend: IsolatedWorkspaceBackend::GitWorktree,
        base_commit: None,
        overlay_digest: None,
        overlay_artifact_ref: None,
        overlay_content_artifact_refs: Vec::new(),
        overlay_entry_count: 0,
        materialized_snapshot_id: None,
    };
    let produced = IsolatedChangeSetProduced {
        changeset_id: change_set_id(),
        owner_agent_id: "agent-child".to_owned(),
        base_snapshot_id: "snapshot-base".to_owned(),
        child_snapshot_id: Some("snapshot-child".to_owned()),
        source_isolation: WriteIsolationMode::Worktree,
        artifact_ref: Some("artifact-change-1".to_owned()),
        touched_subjects: vec![MutationSubject::File {
            path: "src/lib.rs".into(),
            file_type: crate::FileType::File,
        }],
        integration_facts: crate::IntegrationProposalFacts::default(),
    };
    let requested = MergeReviewRequested {
        review_id: review_id(),
        changeset_id: produced.changeset_id.clone(),
        parent_workspace_snapshot_id: "snapshot-parent".to_owned(),
    };
    let resolved = MergeReviewResolved {
        review_id: requested.review_id.clone(),
        decision: MergeDecision::Accepted,
        reason: Some("looks good".to_owned()),
    };

    let entries = vec![
        SessionLogEntry::Control(ControlEntry::WriteLeaseAcquired(acquired.clone())),
        SessionLogEntry::Control(ControlEntry::IsolatedWorkspaceCreated(isolated.clone())),
        SessionLogEntry::Control(ControlEntry::IsolatedChangeSetProduced(produced.clone())),
        SessionLogEntry::Control(ControlEntry::MergeReviewRequested(requested.clone())),
        SessionLogEntry::Control(ControlEntry::MergeReviewResolved(resolved.clone())),
        SessionLogEntry::Control(ControlEntry::WriteLeaseReleased(release.clone())),
    ];

    let projection = WriteIsolationProjection::from_entries(&entries);

    let lease = projection
        .leases
        .get(&acquired.lease_id)
        .expect("lease state");
    assert!(!lease.is_active());
    assert_eq!(lease.acquired.as_ref(), Some(&acquired));
    assert_eq!(lease.released.as_ref(), Some(&release));
    assert!(
        projection
            .active_lease_for_workspace(&acquired.workspace_id)
            .is_none()
    );
    assert_eq!(
        projection
            .isolated_workspaces
            .get(&isolated.isolated_workspace_id),
        Some(&isolated)
    );
    assert_eq!(
        projection.isolated_changesets.get(&produced.changeset_id),
        Some(&produced)
    );
    let review = projection
        .merge_reviews
        .get(&requested.review_id)
        .expect("merge review state");
    assert!(!review.is_pending());
    assert_eq!(review.requested.as_ref(), Some(&requested));
    assert_eq!(review.resolved.as_ref(), Some(&resolved));
    assert_eq!(projection.replay_order.len(), 6);
}

#[test]
fn write_isolation_projection_tracks_active_workspace_lease() -> Result<()> {
    let acquired = acquired_entry();
    let projection = WriteIsolationProjection::from_entries(&[SessionLogEntry::Control(
        ControlEntry::WriteLeaseAcquired(acquired.clone()),
    )]);

    let lease = projection
        .active_lease_for_workspace(&acquired.workspace_id)
        .expect("active lease");
    assert!(lease.is_active());
    assert_eq!(lease.lease_id, acquired.lease_id);
    Ok(())
}

#[test]
fn isolated_workspace_projection_reconstructs_cleanup_inventory_across_crash_windows() {
    let prepared = IsolatedWorkspacePrepared {
        isolated_workspace_id: "workspace-child".to_owned(),
        parent_workspace_id: "workspace-parent".to_owned(),
        owner_agent_id: "agent-child".to_owned(),
        isolation_mode: WriteIsolationMode::Worktree,
        base_snapshot_id: "snapshot-base".to_owned(),
        backend: IsolatedWorkspaceBackend::GitWorktree,
        base_commit: Some("0123456789012345678901234567890123456789".to_owned()),
        overlay_digest: Some("sha256:overlay".to_owned()),
        overlay_artifact_ref: Some("mutation-artifact:sha256:manifest".to_owned()),
        overlay_content_artifact_refs: vec!["mutation-artifact:sha256:content".to_owned()],
        overlay_entry_count: 1,
    };
    let created = IsolatedWorkspaceCreated {
        isolated_workspace_id: prepared.isolated_workspace_id.clone(),
        parent_workspace_id: prepared.parent_workspace_id.clone(),
        owner_agent_id: prepared.owner_agent_id.clone(),
        isolation_mode: prepared.isolation_mode,
        base_snapshot_id: prepared.base_snapshot_id.clone(),
        backend: prepared.backend,
        base_commit: prepared.base_commit.clone(),
        overlay_digest: prepared.overlay_digest.clone(),
        overlay_artifact_ref: prepared.overlay_artifact_ref.clone(),
        overlay_content_artifact_refs: prepared.overlay_content_artifact_refs.clone(),
        overlay_entry_count: prepared.overlay_entry_count,
        materialized_snapshot_id: Some("snapshot-materialized".to_owned()),
    };
    let failed_cleanup = IsolatedWorkspaceCleanupRecorded {
        isolated_workspace_id: prepared.isolated_workspace_id.clone(),
        status: IsolatedWorkspaceCleanupStatus::Failed,
    };
    let removed_cleanup = IsolatedWorkspaceCleanupRecorded {
        isolated_workspace_id: prepared.isolated_workspace_id.clone(),
        status: IsolatedWorkspaceCleanupStatus::Removed,
    };

    let prepared_only = WriteIsolationProjection::from_entries(&[SessionLogEntry::Control(
        ControlEntry::IsolatedWorkspacePrepared(prepared.clone()),
    )]);
    assert_eq!(
        prepared_only.isolated_workspace_cleanup_inventory().len(),
        1
    );

    let failed = WriteIsolationProjection::from_entries(&[
        SessionLogEntry::Control(ControlEntry::IsolatedWorkspacePrepared(prepared.clone())),
        SessionLogEntry::Control(ControlEntry::IsolatedWorkspaceCreated(created.clone())),
        SessionLogEntry::Control(ControlEntry::IsolatedWorkspaceCleanupRecorded(
            failed_cleanup.clone(),
        )),
    ]);
    let state = failed
        .isolated_workspace_cleanup_inventory()
        .into_iter()
        .next()
        .expect("failed cleanup should stay in recovery inventory");
    assert_eq!(state.prepared.as_ref(), Some(&prepared));
    assert_eq!(state.created.as_ref(), Some(&created));
    assert_eq!(state.cleanup.as_ref(), Some(&failed_cleanup));

    let removed = WriteIsolationProjection::from_entries(&[
        SessionLogEntry::Control(ControlEntry::IsolatedWorkspacePrepared(prepared.clone())),
        SessionLogEntry::Control(ControlEntry::IsolatedWorkspaceCreated(created)),
        SessionLogEntry::Control(ControlEntry::IsolatedWorkspaceCleanupRecorded(
            failed_cleanup,
        )),
        SessionLogEntry::Control(ControlEntry::IsolatedWorkspaceCleanupRecorded(
            removed_cleanup.clone(),
        )),
    ]);
    assert!(removed.isolated_workspace_cleanup_inventory().is_empty());
    assert_eq!(
        removed
            .isolated_workspace_states
            .get(&prepared.isolated_workspace_id)
            .and_then(|state| state.cleanup.as_ref()),
        Some(&removed_cleanup)
    );
    assert_eq!(removed.replay_order.len(), 4);
}

#[test]
fn isolated_workspace_projection_marks_conflicting_materialization_binding() {
    let prepared = IsolatedWorkspacePrepared {
        isolated_workspace_id: "workspace-child".to_owned(),
        parent_workspace_id: "workspace-parent".to_owned(),
        owner_agent_id: "agent-child".to_owned(),
        isolation_mode: WriteIsolationMode::Worktree,
        base_snapshot_id: "snapshot-base".to_owned(),
        backend: IsolatedWorkspaceBackend::GitWorktree,
        base_commit: None,
        overlay_digest: None,
        overlay_artifact_ref: None,
        overlay_content_artifact_refs: Vec::new(),
        overlay_entry_count: 0,
    };
    let created = IsolatedWorkspaceCreated {
        isolated_workspace_id: prepared.isolated_workspace_id.clone(),
        parent_workspace_id: prepared.parent_workspace_id.clone(),
        owner_agent_id: "agent-other".to_owned(),
        isolation_mode: prepared.isolation_mode,
        base_snapshot_id: prepared.base_snapshot_id.clone(),
        backend: prepared.backend,
        base_commit: prepared.base_commit.clone(),
        overlay_digest: prepared.overlay_digest.clone(),
        overlay_artifact_ref: prepared.overlay_artifact_ref.clone(),
        overlay_content_artifact_refs: prepared.overlay_content_artifact_refs.clone(),
        overlay_entry_count: prepared.overlay_entry_count,
        materialized_snapshot_id: None,
    };

    let projection = WriteIsolationProjection::from_entries(&[
        SessionLogEntry::Control(ControlEntry::IsolatedWorkspacePrepared(prepared.clone())),
        SessionLogEntry::Control(ControlEntry::IsolatedWorkspaceCreated(created)),
    ]);
    let state = projection
        .isolated_workspace_states
        .get(&prepared.isolated_workspace_id)
        .expect("workspace state");

    assert!(!state.is_consistent());
    assert!(state.requires_cleanup());
    assert_eq!(projection.isolated_workspace_cleanup_inventory().len(), 1);
}

#[test]
fn write_lease_admission_rejects_second_active_workspace_writer() -> Result<()> {
    let acquired = acquired_entry();
    let projection = WriteIsolationProjection::from_entries(&[SessionLogEntry::Control(
        ControlEntry::WriteLeaseAcquired(acquired.clone()),
    )]);

    projection.validate_can_acquire_shared_workspace_lease(&acquired)?;

    let conflicting = WriteLeaseAcquired {
        lease_id: WriteLeaseId::new("lease-2")?,
        owner_agent_id: "agent-other".to_owned(),
        ..acquired
    };
    let error = projection
        .validate_can_acquire_shared_workspace_lease(&conflicting)
        .expect_err("second active writer should fail closed");

    assert!(error.to_string().contains("already has active write lease"));
    Ok(())
}

#[test]
fn write_lease_projection_builds_stale_release_records_for_recovery() {
    let acquired = acquired_entry();
    let projection = WriteIsolationProjection::from_entries(&[SessionLogEntry::Control(
        ControlEntry::WriteLeaseAcquired(acquired.clone()),
    )]);

    let releases = projection.stale_active_lease_releases();

    assert_eq!(releases.len(), 1);
    assert_eq!(releases[0].lease_id, acquired.lease_id);
    assert_eq!(releases[0].status, WriteLeaseReleaseStatus::Stale);
}

#[test]
fn typed_event_decode_covers_write_isolation_family() {
    let acquired = acquired_entry();
    let event = stored_control_event(
        DurableEventType::WriteLeaseAcquired,
        ControlEntry::WriteLeaseAcquired(acquired.clone()),
        1,
    );

    let TypedStoredEventDecode::Known(event) =
        decode_typed_stored_event(event).expect("write isolation event should decode")
    else {
        panic!("expected typed write isolation event");
    };
    assert!(matches!(
        *event,
        TypedDomainEvent::WriteIsolation(ControlEntry::WriteLeaseAcquired(entry))
            if entry == acquired
    ));

    let prepared = IsolatedWorkspacePrepared {
        isolated_workspace_id: "workspace-child".to_owned(),
        parent_workspace_id: "workspace-parent".to_owned(),
        owner_agent_id: "agent-child".to_owned(),
        isolation_mode: WriteIsolationMode::Worktree,
        base_snapshot_id: "snapshot-base".to_owned(),
        backend: IsolatedWorkspaceBackend::GitWorktree,
        base_commit: None,
        overlay_digest: None,
        overlay_artifact_ref: None,
        overlay_content_artifact_refs: Vec::new(),
        overlay_entry_count: 0,
    };
    let event = stored_control_event(
        DurableEventType::IsolatedWorkspacePrepared,
        ControlEntry::IsolatedWorkspacePrepared(prepared.clone()),
        2,
    );
    let TypedStoredEventDecode::Known(event) =
        decode_typed_stored_event(event).expect("prepared workspace event should decode")
    else {
        panic!("expected typed write isolation event");
    };
    assert!(matches!(
        *event,
        TypedDomainEvent::WriteIsolation(ControlEntry::IsolatedWorkspacePrepared(entry))
            if entry == prepared
    ));

    let bad_event = stored_control_event(
        DurableEventType::WriteLeaseReleased,
        ControlEntry::WriteLeaseAcquired(acquired),
        3,
    );
    let error = decode_typed_stored_event(bad_event)
        .expect_err("mismatched write isolation event should fail closed");
    assert!(
        error
            .to_string()
            .contains("non-write-isolation control payload")
    );
}

#[test]
fn write_isolation_projection_replays_durable_stream_records() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("session.jsonl");
    let store = JsonlSessionStore::new(&path)?;
    let acquired = acquired_entry();
    let review = MergeReviewRequested {
        review_id: review_id(),
        changeset_id: change_set_id(),
        parent_workspace_snapshot_id: "snapshot-parent".to_owned(),
    };
    let resolved = MergeReviewResolved {
        review_id: review.review_id.clone(),
        decision: MergeDecision::Rejected,
        reason: Some("conflicts with parent".to_owned()),
    };
    let prepared = IsolatedWorkspacePrepared {
        isolated_workspace_id: "workspace-child".to_owned(),
        parent_workspace_id: acquired.workspace_id.clone(),
        owner_agent_id: "agent-child".to_owned(),
        isolation_mode: WriteIsolationMode::Worktree,
        base_snapshot_id: "snapshot-base".to_owned(),
        backend: IsolatedWorkspaceBackend::GitWorktree,
        base_commit: None,
        overlay_digest: None,
        overlay_artifact_ref: None,
        overlay_content_artifact_refs: Vec::new(),
        overlay_entry_count: 0,
    };
    let cleanup = IsolatedWorkspaceCleanupRecorded {
        isolated_workspace_id: prepared.isolated_workspace_id.clone(),
        status: IsolatedWorkspaceCleanupStatus::Failed,
    };
    store.append_session_entry_event(&SessionLogEntry::Control(
        ControlEntry::WriteLeaseAcquired(acquired.clone()),
    ))?;
    store.append_session_entry_event(&SessionLogEntry::Control(
        ControlEntry::IsolatedWorkspacePrepared(prepared.clone()),
    ))?;
    store.append_session_entry_event(&SessionLogEntry::Control(
        ControlEntry::IsolatedWorkspaceCleanupRecorded(cleanup.clone()),
    ))?;
    store.append_session_entry_event(&SessionLogEntry::Control(
        ControlEntry::MergeReviewRequested(review.clone()),
    ))?;
    store.append_session_entry_event(&SessionLogEntry::Control(
        ControlEntry::MergeReviewResolved(resolved.clone()),
    ))?;
    let session = Session::new("deepseek", "deepseek-v4-flash").with_store(store);

    let projection = session
        .try_write_isolation_projection_from_durable()?
        .expect("durable session should replay write isolation projection");

    assert!(
        projection
            .active_lease_for_workspace(&acquired.workspace_id)
            .is_some()
    );
    let workspace = projection
        .isolated_workspace_cleanup_inventory()
        .into_iter()
        .next()
        .expect("failed cleanup should survive durable replay");
    assert_eq!(workspace.prepared.as_ref(), Some(&prepared));
    assert_eq!(workspace.cleanup.as_ref(), Some(&cleanup));
    let review_state = projection
        .merge_reviews
        .get(&review.review_id)
        .expect("merge review state");
    assert_eq!(review_state.requested.as_ref(), Some(&review));
    assert_eq!(review_state.resolved.as_ref(), Some(&resolved));
    Ok(())
}

#[test]
fn write_isolation_projection_rejects_unknown_critical_stream_event() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("session.jsonl");
    let event = StoredEvent::new_raw(
        "future_write_isolation_event",
        EventClass::Critical,
        "event-future-write-isolation".to_owned(),
        "session-1".to_owned(),
        1,
        json!({"lease_id": "lease-1"}),
    )?;
    std::fs::write(&path, event.to_json_line()?)?;
    let store = JsonlSessionStore::new(&path)?;

    let error = store
        .read_event_records_writer()
        .expect_err("unknown critical write-isolation event should fail closed");

    assert!(format!("{error:#}").contains("unknown critical event future_write_isolation_event"));
    Ok(())
}

#[test]
fn verification_merge_accepted_review_applies_parent_mutation_batch() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let workspace_root = temp.path().join("workspace");
    std::fs::create_dir(&workspace_root)?;
    std::fs::write(workspace_root.join("note.txt"), b"old\n")?;
    let store = JsonlSessionStore::new(temp.path().join("session.jsonl"))?;
    let mut session = Session::new("deepseek", "model").with_store(store.clone());
    let change_set = note_change_set(change_set_id());
    session.append_control(ControlEntry::ChangeSetProposed(change_set.clone()))?;
    append_merge_review_request(&mut session, change_set.id.clone(), &workspace_root)?;

    let outcome = resolve_merge_review_parent_mutation(
        &mut session,
        MergeReviewParentMutationRequest {
            review_id: review_id(),
            decision: MergeDecision::Accepted,
            reason: Some("approved".to_owned()),
            change_set: change_set.clone(),
            artifact_content: note_diff(),
            workspace_root: workspace_root.clone(),
            tool_call_id: "merge-review-call".to_owned(),
        },
    )?;

    assert_eq!(
        std::fs::read_to_string(workspace_root.join("note.txt"))?,
        "new\n"
    );
    assert_eq!(outcome.batch_status, Some(MutationBatchStatus::Applied));
    let result = outcome.change_set_result.expect("changeset result");
    assert_eq!(result.status, ChangeSetResultStatus::Applied);
    assert_eq!(result.file_results.len(), 1);
    assert_eq!(
        result.file_results[0].status,
        ChangeSetFileResultStatus::Applied
    );
    let projection = session.write_isolation_projection();
    let review = projection
        .merge_reviews
        .get(&review_id())
        .expect("review state");
    assert_eq!(
        review.resolved.as_ref().map(|resolved| resolved.decision),
        Some(MergeDecision::Accepted)
    );

    let event_types = stored_event_types(&store)?;
    assert!(event_types.contains(&DurableEventType::MutationBatchStarted.as_str().to_owned()));
    assert!(event_types.contains(&DurableEventType::MutationPrepared.as_str().to_owned()));
    assert!(event_types.contains(&DurableEventType::MutationCommitted.as_str().to_owned()));
    assert!(event_types.contains(&DurableEventType::WriteCommitted.as_str().to_owned()));
    assert!(event_types.contains(&DurableEventType::MutationBatchFinished.as_str().to_owned()));
    assert!(event_types.contains(&DurableEventType::ChildChangesetMerged.as_str().to_owned()));
    Ok(())
}

#[test]
fn parent_changeset_mutation_preflights_and_applies_one_aggregate_batch() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let workspace_root = temp.path().join("workspace");
    std::fs::create_dir(&workspace_root)?;
    std::fs::write(workspace_root.join("note.txt"), b"old\n")?;
    std::fs::write(workspace_root.join("other.txt"), b"before\n")?;
    let store = JsonlSessionStore::new(temp.path().join("session.jsonl"))?;
    let session = Session::new("mock", "model").with_store(store.clone());
    let recorder = session
        .mutation_event_recorder()
        .expect("durable session recorder");
    let mut change_set = note_change_set(ChangeSetId::new("aggregate-change")?);
    change_set.files.push(ChangeSetFile {
        path: "other.txt".to_owned(),
        previous_path: None,
        action: ChangeSetFileAction::Update,
        risk: ChangeSetRisk::Low,
        before_hash: Some(
            bytes_hash(b"before\n")
                .trim_start_matches("sha256:")
                .to_owned(),
        ),
        after_hash: Some(
            bytes_hash(b"after\n")
                .trim_start_matches("sha256:")
                .to_owned(),
        ),
        diff_hash: None,
        additions: 1,
        deletions: 1,
        validations: Vec::new(),
    });
    let artifact = format!(
        "diff --git a/note.txt b/note.txt\nindex 1111111..2222222 100644\n{}\
diff --git a/other.txt b/other.txt\nindex 3333333..4444444 100644\n\
--- a/other.txt\n+++ b/other.txt\n@@ -1,1 +1,1 @@\n-before\n+after\n",
        note_diff()
    );
    let expected_snapshot_id = super::parent_workspace_snapshot_id(&workspace_root)?;

    let outcome = apply_parent_changeset_mutation_batch(
        &recorder,
        ParentChangeSetMutationRequest {
            operation_key: "promotion-attempt-1".to_owned(),
            expected_workspace_snapshot_id: expected_snapshot_id.clone(),
            change_set,
            artifact_digest: artifact_digest(&artifact),
            artifact_content: artifact,
            workspace_root: workspace_root.clone(),
            tool_call_id: "promotion-call-1".to_owned(),
        },
    )?;

    assert!(outcome.is_applied());
    assert_eq!(
        outcome.observed_workspace_snapshot_before_id,
        expected_snapshot_id
    );
    assert_ne!(
        outcome
            .observed_workspace_snapshot_after_id
            .as_deref()
            .expect("post-apply snapshot"),
        outcome.observed_workspace_snapshot_before_id
    );
    assert_eq!(
        std::fs::read_to_string(workspace_root.join("note.txt"))?,
        "new\n"
    );
    assert_eq!(
        std::fs::read_to_string(workspace_root.join("other.txt"))?,
        "after\n"
    );
    assert_eq!(outcome.committed_operations.len(), 2);
    assert!(outcome.failed_operations.is_empty());
    let event_types = stored_event_types(&store)?;
    assert!(event_types.contains(&DurableEventType::MutationBatchStarted.as_str().to_owned()));
    assert!(event_types.contains(&DurableEventType::MutationBatchFinished.as_str().to_owned()));
    assert!(!event_types.contains(&DurableEventType::ChildChangesetMerged.as_str().to_owned()));
    Ok(())
}

#[test]
fn parent_changeset_mutation_snapshot_drift_has_zero_effect() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let workspace_root = temp.path().join("workspace");
    std::fs::create_dir(&workspace_root)?;
    std::fs::write(workspace_root.join("note.txt"), b"old\n")?;
    let expected_snapshot_id = super::parent_workspace_snapshot_id(&workspace_root)?;
    std::fs::write(workspace_root.join("unrelated.txt"), b"user drift\n")?;
    let store = JsonlSessionStore::new(temp.path().join("session.jsonl"))?;
    let session = Session::new("mock", "model").with_store(store.clone());
    let recorder = session
        .mutation_event_recorder()
        .expect("durable session recorder");
    let artifact = note_diff();

    let outcome = apply_parent_changeset_mutation_batch(
        &recorder,
        ParentChangeSetMutationRequest {
            operation_key: "promotion-attempt-stale".to_owned(),
            expected_workspace_snapshot_id: expected_snapshot_id,
            change_set: note_change_set(ChangeSetId::new("aggregate-stale")?),
            artifact_digest: artifact_digest(&artifact),
            artifact_content: artifact,
            workspace_root: workspace_root.clone(),
            tool_call_id: "promotion-call-stale".to_owned(),
        },
    )?;

    assert!(outcome.conflict_reason.is_some());
    assert!(outcome.batch_id.is_none());
    assert!(outcome.committed_operations.is_empty());
    assert_eq!(
        std::fs::read_to_string(workspace_root.join("note.txt"))?,
        "old\n"
    );
    assert!(
        !stored_event_types(&store)?
            .contains(&DurableEventType::MutationBatchStarted.as_str().to_owned())
    );
    Ok(())
}

#[test]
fn parent_changeset_mutation_digest_drift_has_zero_effect() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let workspace_root = temp.path().join("workspace");
    std::fs::create_dir(&workspace_root)?;
    std::fs::write(workspace_root.join("note.txt"), b"old\n")?;
    let expected_snapshot_id = super::parent_workspace_snapshot_id(&workspace_root)?;
    let store = JsonlSessionStore::new(temp.path().join("session.jsonl"))?;
    let session = Session::new("mock", "model").with_store(store.clone());
    let recorder = session
        .mutation_event_recorder()
        .expect("durable session recorder");

    let outcome = apply_parent_changeset_mutation_batch(
        &recorder,
        ParentChangeSetMutationRequest {
            operation_key: "promotion-attempt-digest".to_owned(),
            expected_workspace_snapshot_id: expected_snapshot_id,
            change_set: note_change_set(ChangeSetId::new("aggregate-digest")?),
            artifact_content: note_diff(),
            artifact_digest: format!("sha256:{}", "0".repeat(64)),
            workspace_root: workspace_root.clone(),
            tool_call_id: "promotion-call-digest".to_owned(),
        },
    )?;

    assert!(
        outcome
            .conflict_reason
            .as_deref()
            .is_some_and(|reason| reason.contains("artifact digest mismatch"))
    );
    assert!(outcome.batch_id.is_none());
    assert_eq!(
        std::fs::read_to_string(workspace_root.join("note.txt"))?,
        "old\n"
    );
    assert!(
        !stored_event_types(&store)?
            .contains(&DurableEventType::MutationBatchStarted.as_str().to_owned())
    );
    Ok(())
}

#[test]
fn parent_changeset_mutation_any_file_preflight_conflict_has_zero_effect() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let workspace_root = temp.path().join("workspace");
    std::fs::create_dir(&workspace_root)?;
    std::fs::write(workspace_root.join("note.txt"), b"old\n")?;
    std::fs::write(workspace_root.join("conflict.txt"), b"actual\n")?;
    let expected_snapshot_id = super::parent_workspace_snapshot_id(&workspace_root)?;
    let store = JsonlSessionStore::new(temp.path().join("session.jsonl"))?;
    let session = Session::new("mock", "model").with_store(store.clone());
    let recorder = session
        .mutation_event_recorder()
        .expect("durable session recorder");
    let mut change_set = note_change_set(ChangeSetId::new("aggregate-conflict")?);
    change_set.files.push(ChangeSetFile {
        path: "conflict.txt".to_owned(),
        previous_path: None,
        action: ChangeSetFileAction::Update,
        risk: ChangeSetRisk::Low,
        before_hash: Some(bytes_hash(b"expected\n")),
        after_hash: Some(bytes_hash(b"changed\n")),
        diff_hash: None,
        additions: 1,
        deletions: 1,
        validations: Vec::new(),
    });
    let artifact = format!(
        "{}{}",
        note_diff(),
        "--- a/conflict.txt\n+++ b/conflict.txt\n@@ -1,1 +1,1 @@\n-expected\n+changed\n"
    );

    let outcome = apply_parent_changeset_mutation_batch(
        &recorder,
        ParentChangeSetMutationRequest {
            operation_key: "promotion-attempt-conflict".to_owned(),
            expected_workspace_snapshot_id: expected_snapshot_id,
            change_set,
            artifact_digest: artifact_digest(&artifact),
            artifact_content: artifact,
            workspace_root: workspace_root.clone(),
            tool_call_id: "promotion-call-conflict".to_owned(),
        },
    )?;

    assert!(
        outcome
            .conflict_reason
            .as_deref()
            .is_some_and(|reason| reason.contains("preflight conflict"))
    );
    assert!(outcome.batch_id.is_none());
    assert_eq!(
        std::fs::read_to_string(workspace_root.join("note.txt"))?,
        "old\n"
    );
    assert_eq!(
        std::fs::read_to_string(workspace_root.join("conflict.txt"))?,
        "actual\n"
    );
    assert!(
        !stored_event_types(&store)?
            .contains(&DurableEventType::MutationBatchStarted.as_str().to_owned())
    );
    Ok(())
}

#[test]
fn rejected_merge_review_does_not_mutate_parent_or_emit_batch() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let workspace_root = temp.path().join("workspace");
    std::fs::create_dir(&workspace_root)?;
    std::fs::write(workspace_root.join("note.txt"), b"old\n")?;
    let store = JsonlSessionStore::new(temp.path().join("session.jsonl"))?;
    let mut session = Session::new("deepseek", "model").with_store(store.clone());
    let change_set = note_change_set(change_set_id());
    append_merge_review_request(&mut session, change_set.id.clone(), &workspace_root)?;

    let outcome = resolve_merge_review_parent_mutation(
        &mut session,
        MergeReviewParentMutationRequest {
            review_id: review_id(),
            decision: MergeDecision::Rejected,
            reason: Some("not needed".to_owned()),
            change_set,
            artifact_content: note_diff(),
            workspace_root: workspace_root.clone(),
            tool_call_id: "merge-review-call".to_owned(),
        },
    )?;

    assert_eq!(
        std::fs::read_to_string(workspace_root.join("note.txt"))?,
        "old\n"
    );
    assert!(outcome.change_set_result.is_none());
    let event_types = stored_event_types(&store)?;
    assert!(!event_types.contains(&DurableEventType::MutationBatchStarted.as_str().to_owned()));
    assert!(!event_types.contains(&DurableEventType::MutationCommitted.as_str().to_owned()));
    assert!(!event_types.contains(&DurableEventType::WriteCommitted.as_str().to_owned()));
    assert!(!event_types.contains(&DurableEventType::ChildChangesetMerged.as_str().to_owned()));
    let projection = session.write_isolation_projection();
    let review = projection
        .merge_reviews
        .get(&review_id())
        .expect("review state");
    assert_eq!(
        review.resolved.as_ref().map(|resolved| resolved.decision),
        Some(MergeDecision::Rejected)
    );
    Ok(())
}

#[test]
fn accepted_merge_review_stale_parent_snapshot_is_zero_mutation() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let workspace_root = temp.path().join("workspace");
    std::fs::create_dir(&workspace_root)?;
    std::fs::write(workspace_root.join("note.txt"), b"old\n")?;
    let store = JsonlSessionStore::new(temp.path().join("session.jsonl"))?;
    let mut session = Session::new("deepseek", "model").with_store(store.clone());
    let change_set = note_change_set(change_set_id());
    session.append_control(ControlEntry::ChangeSetProposed(change_set.clone()))?;
    append_merge_review_request(&mut session, change_set.id.clone(), &workspace_root)?;
    std::fs::write(workspace_root.join("unrelated.txt"), b"user drift\n")?;

    let outcome = resolve_merge_review_parent_mutation(
        &mut session,
        MergeReviewParentMutationRequest {
            review_id: review_id(),
            decision: MergeDecision::Accepted,
            reason: Some("approved before parent drift".to_owned()),
            change_set,
            artifact_content: note_diff(),
            workspace_root: workspace_root.clone(),
            tool_call_id: "merge-review-call".to_owned(),
        },
    )?;

    assert_eq!(outcome.decision, MergeDecision::Conflict);
    assert_eq!(outcome.batch_status, None);
    assert!(outcome.change_set_result.is_none());
    assert!(outcome.committed_operations.is_empty());
    assert!(outcome.failed_operations.is_empty());
    assert_eq!(
        std::fs::read_to_string(workspace_root.join("note.txt"))?,
        "old\n"
    );
    assert_eq!(
        std::fs::read_to_string(workspace_root.join("unrelated.txt"))?,
        "user drift\n"
    );
    let event_types = stored_event_types(&store)?;
    assert!(!event_types.contains(&DurableEventType::MutationBatchStarted.as_str().to_owned()));
    assert!(!event_types.contains(&DurableEventType::ChildChangesetMerged.as_str().to_owned()));
    Ok(())
}

#[test]
fn accepted_merge_review_preflight_conflict_is_zero_mutation() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let workspace_root = temp.path().join("workspace");
    std::fs::create_dir(&workspace_root)?;
    std::fs::write(workspace_root.join("note.txt"), b"old\n")?;
    std::fs::write(workspace_root.join("conflict.txt"), b"actual\n")?;
    let store = JsonlSessionStore::new(temp.path().join("session.jsonl"))?;
    let mut session = Session::new("deepseek", "model").with_store(store.clone());
    let mut change_set = note_change_set(change_set_id());
    change_set.files.push(ChangeSetFile {
        path: "conflict.txt".to_owned(),
        previous_path: None,
        action: ChangeSetFileAction::Update,
        risk: ChangeSetRisk::Low,
        before_hash: Some(bytes_hash(b"expected\n")),
        after_hash: None,
        diff_hash: None,
        additions: 1,
        deletions: 1,
        validations: Vec::new(),
    });
    append_merge_review_request(&mut session, change_set.id.clone(), &workspace_root)?;
    let artifact = format!(
        "{}{}",
        note_diff(),
        "--- a/conflict.txt\n+++ b/conflict.txt\n@@ -1,1 +1,1 @@\n-expected\n+changed\n"
    );

    let outcome = resolve_merge_review_parent_mutation(
        &mut session,
        MergeReviewParentMutationRequest {
            review_id: review_id(),
            decision: MergeDecision::Accepted,
            reason: Some("approved with one conflict".to_owned()),
            change_set,
            artifact_content: artifact,
            workspace_root: workspace_root.clone(),
            tool_call_id: "merge-review-call".to_owned(),
        },
    )?;

    assert_eq!(
        std::fs::read_to_string(workspace_root.join("note.txt"))?,
        "old\n"
    );
    assert_eq!(
        std::fs::read_to_string(workspace_root.join("conflict.txt"))?,
        "actual\n"
    );
    assert_eq!(outcome.decision, MergeDecision::Conflict);
    assert_eq!(outcome.batch_status, None);
    assert!(outcome.change_set_result.is_none());
    assert!(outcome.committed_operations.is_empty());
    assert!(outcome.failed_operations.is_empty());
    let event_types = stored_event_types(&store)?;
    assert!(!event_types.contains(&DurableEventType::MutationBatchStarted.as_str().to_owned()));
    assert!(!event_types.contains(&DurableEventType::ChildChangesetMerged.as_str().to_owned()));
    Ok(())
}

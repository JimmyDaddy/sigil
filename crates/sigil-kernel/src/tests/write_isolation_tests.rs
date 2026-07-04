use anyhow::Result;
use serde_json::json;

use crate::{
    ChangeSet, ChangeSetFile, ChangeSetFileAction, ChangeSetFileResultStatus, ChangeSetId,
    ChangeSetResultStatus, ChangeSetRisk, ControlEntry, DurableEventType, EventClass,
    IsolatedChangeSetProduced, IsolatedWorkspaceBackend, IsolatedWorkspaceCreated,
    JsonlSessionStore, MergeDecision, MergeReviewId, MergeReviewParentMutationRequest,
    MergeReviewRequested, MergeReviewResolved, MutationBatchStatus, MutationSubject, Session,
    SessionLogEntry, SessionStreamRecord, StoredEvent, TypedDomainEvent, TypedStoredEventDecode,
    WriteIsolationMode, WriteIsolationProjection, WriteLeaseAcquired, WriteLeaseId,
    WriteLeaseReleaseStatus, WriteLeaseReleased, WriteLeaseScope, bytes_hash,
    decode_typed_stored_event, resolve_merge_review_parent_mutation,
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
        let SessionStreamRecord::Stored(event) = record;
        event_types.push(event.event_type);
    }
    Ok(event_types)
}

fn append_merge_review_request(session: &mut Session, change_set_id: ChangeSetId) -> Result<()> {
    session.append_control(ControlEntry::MergeReviewRequested(MergeReviewRequested {
        review_id: review_id(),
        changeset_id: change_set_id,
        parent_workspace_snapshot_id: "snapshot-parent-before".to_owned(),
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

    let bad_event = stored_control_event(
        DurableEventType::WriteLeaseReleased,
        ControlEntry::WriteLeaseAcquired(acquired),
        2,
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
    store.append_session_entry_event(&SessionLogEntry::Control(
        ControlEntry::WriteLeaseAcquired(acquired.clone()),
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
    std::fs::write(temp.path().join("note.txt"), b"old\n")?;
    let store = JsonlSessionStore::new(temp.path().join("session.jsonl"))?;
    let mut session = Session::new("deepseek", "model").with_store(store.clone());
    let change_set = note_change_set(change_set_id());
    session.append_control(ControlEntry::ChangeSetProposed(change_set.clone()))?;
    append_merge_review_request(&mut session, change_set.id.clone())?;

    let outcome = resolve_merge_review_parent_mutation(
        &mut session,
        MergeReviewParentMutationRequest {
            review_id: review_id(),
            decision: MergeDecision::Accepted,
            reason: Some("approved".to_owned()),
            change_set: change_set.clone(),
            artifact_content: note_diff(),
            workspace_root: temp.path().to_path_buf(),
            tool_call_id: "merge-review-call".to_owned(),
        },
    )?;

    assert_eq!(
        std::fs::read_to_string(temp.path().join("note.txt"))?,
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
fn rejected_merge_review_does_not_mutate_parent_or_emit_batch() -> Result<()> {
    let temp = tempfile::tempdir()?;
    std::fs::write(temp.path().join("note.txt"), b"old\n")?;
    let store = JsonlSessionStore::new(temp.path().join("session.jsonl"))?;
    let mut session = Session::new("deepseek", "model").with_store(store.clone());
    let change_set = note_change_set(change_set_id());
    append_merge_review_request(&mut session, change_set.id.clone())?;

    let outcome = resolve_merge_review_parent_mutation(
        &mut session,
        MergeReviewParentMutationRequest {
            review_id: review_id(),
            decision: MergeDecision::Rejected,
            reason: Some("not needed".to_owned()),
            change_set,
            artifact_content: note_diff(),
            workspace_root: temp.path().to_path_buf(),
            tool_call_id: "merge-review-call".to_owned(),
        },
    )?;

    assert_eq!(
        std::fs::read_to_string(temp.path().join("note.txt"))?,
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
fn accepted_merge_review_records_partial_batch_status() -> Result<()> {
    let temp = tempfile::tempdir()?;
    std::fs::write(temp.path().join("note.txt"), b"old\n")?;
    std::fs::write(temp.path().join("conflict.txt"), b"actual\n")?;
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
    append_merge_review_request(&mut session, change_set.id.clone())?;
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
            workspace_root: temp.path().to_path_buf(),
            tool_call_id: "merge-review-call".to_owned(),
        },
    )?;

    assert_eq!(
        std::fs::read_to_string(temp.path().join("note.txt"))?,
        "new\n"
    );
    assert_eq!(
        std::fs::read_to_string(temp.path().join("conflict.txt"))?,
        "actual\n"
    );
    assert_eq!(
        outcome.batch_status,
        Some(MutationBatchStatus::PartiallyApplied)
    );
    let result = outcome.change_set_result.expect("changeset result");
    assert_eq!(result.status, ChangeSetResultStatus::PartiallyApplied);
    assert_eq!(result.file_results.len(), 2);
    assert_eq!(
        result.file_results[0].status,
        ChangeSetFileResultStatus::Applied
    );
    assert_eq!(
        result.file_results[1].status,
        ChangeSetFileResultStatus::Failed
    );
    assert_eq!(outcome.committed_operations.len(), 1);
    assert_eq!(outcome.failed_operations.len(), 1);
    let event_types = stored_event_types(&store)?;
    assert!(event_types.contains(&DurableEventType::MutationBatchFinished.as_str().to_owned()));
    assert!(event_types.contains(&DurableEventType::ChildChangesetMerged.as_str().to_owned()));
    Ok(())
}

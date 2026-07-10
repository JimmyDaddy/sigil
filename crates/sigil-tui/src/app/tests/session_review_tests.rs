use std::path::PathBuf;

use super::*;
use sigil_kernel::DurableEventType;

#[test]
fn session_review_projects_legacy_stream_entries() {
    let records = vec![sigil_kernel::SessionStreamRecord::Legacy {
        event: sigil_kernel::LegacyEvent {
            event_id: "legacy-review-1".to_owned(),
            session_id: "legacy-session".to_owned(),
            stream_sequence: 1,
            raw_line_hash: "sha256:legacy-review".to_owned(),
            payload: serde_json::Value::Null,
        },
        entry: Box::new(SessionLogEntry::User(ModelMessage::user(
            "Review legacy session",
        ))),
    }];

    let review =
        super::super::session_review::session_review_sidebar_lines_from_records(&records, &[])
            .join("\n");
    assert!(review.contains("review: turn 1/1"));
    assert!(review.contains("Review legacy session"));
}

#[test]
fn session_review_reads_v2_mutation_and_readiness_evidence() -> Result<()> {
    let temp = tempdir()?;
    let config = RootConfig {
        workspace: WorkspaceConfig {
            root: temp.path().display().to_string(),
        },
        ..test_config()
    };
    let mut app = AppState::from_root_config(temp.path().join("sigil.toml").as_path(), &config);
    let session_path = temp.path().join("session-review.jsonl");
    let store = JsonlSessionStore::new(&session_path)?;

    store.append(&SessionLogEntry::User(ModelMessage::user(
        "Fix typo in note.txt",
    )))?;
    let prepared = sigil_kernel::MutationPrepared {
        operation_id: "op-review-1".to_owned(),
        batch_id: None,
        tool_call_id: Some("call-edit".to_owned()),
        causation_event_id: "cause-review".to_owned(),
        subject: sigil_kernel::MutationSubject::File {
            path: PathBuf::from("note.txt"),
            file_type: sigil_kernel::FileType::File,
        },
        before_hash: Some("before".to_owned()),
        intended_after_hash: Some("after".to_owned()),
        snapshot_coverage: sigil_kernel::SnapshotCoverage::Captured("artifact-note".to_owned()),
        workspace_id: "workspace-review".to_owned(),
        base_workspace_revision: 0,
        sync_class: sigil_kernel::MutationSyncClass::RecoveryCritical,
    };
    store.append_event(
        DurableEventType::MutationPrepared,
        sigil_kernel::EventClass::Critical,
        serde_json::to_value(prepared)?,
    )?;
    let committed = sigil_kernel::MutationCommitted {
        operation_id: "op-review-1".to_owned(),
        batch_id: None,
        workspace_id: Some("workspace-review".to_owned()),
        observed_after_hash: Some("after".to_owned()),
        workspace_revision: 1,
        workspace_snapshot_id: "snapshot-review-1".to_owned(),
        committed_subject: sigil_kernel::MutationSubject::File {
            path: PathBuf::from("note.txt"),
            file_type: sigil_kernel::FileType::File,
        },
    };
    store.append_event(
        DurableEventType::MutationCommitted,
        sigil_kernel::EventClass::Critical,
        serde_json::to_value(committed)?,
    )?;
    store.append(&SessionLogEntry::Control(ControlEntry::ToolExecution(
        Box::new(ToolExecutionEntry {
            call_id: "call-edit".to_owned(),
            tool_name: "edit_file".to_owned(),
            status: ToolExecutionStatus::Completed,
            duration_ms: Some(8),
            subjects: Vec::new(),
            changed_files: vec!["note.txt".to_owned()],
            metadata: ToolResultMeta::default(),
            error: None,
            model_content_hash: None,
        }),
    )))?;
    store.append(&SessionLogEntry::Control(ControlEntry::ReadinessEvaluated(
        sigil_kernel::ReadinessEvaluatedEntry {
            scope: sigil_kernel::EvidenceScope::Run("run-review".to_owned()),
            evaluation: sigil_kernel::ReadinessEvaluation {
                run_status: sigil_kernel::RunStatus::Completed,
                verification_verdict: sigil_kernel::VerificationVerdict::Missing,
                visible_state: sigil_kernel::VisibleCompletionState::CompletedUnverified,
                reasons: Vec::new(),
                required_actions: Vec::new(),
            },
            policy_hash: None,
            workspace_snapshot_id: Some("snapshot-review-1".to_owned()),
        },
    )))?;

    app.session_log_path = session_path.clone();
    app.sync_current_session_state(JsonlSessionStore::read_entries(&session_path)?);

    let review = app.session_review_sidebar_lines().join("\n");
    assert!(review.contains("review: turn 1/1"));
    assert!(review.contains("Fix typo in note.txt"));
    assert!(review.contains("changes: note.txt · tools 1 · writes 1"));
    assert!(review.contains("verification: run completed · missing"));
    assert!(review.contains("rewind: controlled checkpoint available"));

    app.toggle_info_rail_detail();
    let view = crate::view_model::UiViewModel::from_app(&app);
    assert!(
        view.info_rail
            .session_lines
            .iter()
            .any(|line| line.contains("review: turn 1/1"))
    );
    Ok(())
}

#[test]
fn session_review_warns_for_unknown_mutation_without_precise_rewind() -> Result<()> {
    let temp = tempdir()?;
    let config = RootConfig {
        workspace: WorkspaceConfig {
            root: temp.path().display().to_string(),
        },
        ..test_config()
    };
    let mut app = AppState::from_root_config(temp.path().join("sigil.toml").as_path(), &config);
    let session_path = temp.path().join("session-review-unknown.jsonl");
    let store = JsonlSessionStore::new(&session_path)?;

    store.append(&SessionLogEntry::User(ModelMessage::user("Run formatter")))?;
    let detected = sigil_kernel::WorkspaceMutationDetected {
        operation_id: "op-unknown".to_owned(),
        tool_call_id: Some("call-bash".to_owned()),
        tool_name: "bash".to_owned(),
        tool_effect: sigil_kernel::ToolEffect::Unknown,
        workspace_id: "workspace-review".to_owned(),
        scope_hash: sigil_kernel::DEFAULT_TASK_VERIFICATION_SCOPE_HASH.to_owned(),
        from_workspace_snapshot_id: Some("snapshot-before".to_owned()),
        to_workspace_snapshot_id: Some("snapshot-after".to_owned()),
        base_workspace_revision: 1,
        workspace_revision: 2,
        reason: sigil_kernel::WorkspaceMutationDetectionReason::SnapshotChanged,
        unknown_dirty: true,
        metadata: Default::default(),
    };
    store.append_event(
        DurableEventType::WorkspaceMutationDetected,
        sigil_kernel::EventClass::Critical,
        serde_json::to_value(detected)?,
    )?;

    app.session_log_path = session_path.clone();
    app.sync_current_session_state(JsonlSessionStore::read_entries(&session_path)?);

    let review = app.session_review_sidebar_lines().join("\n");
    assert!(review.contains("review: turn 1/1"));
    assert!(review.contains("writes 0"));
    assert!(review.contains("rewind: unknown write need git/manual restore"));
    Ok(())
}

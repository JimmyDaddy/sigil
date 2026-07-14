use std::path::PathBuf;

use super::*;
use sigil_kernel::{DurableEventType, EventClass, StoredEvent};

#[test]
fn session_review_projects_v2_stream_entries() {
    let entry = SessionLogEntry::User(ModelMessage::user("Review v2 session"));
    let records = vec![sigil_kernel::SessionStreamRecord::Stored(
        StoredEvent::new(
            DurableEventType::UserMessageRecorded,
            EventClass::Critical,
            "review-v2-1".to_owned(),
            "session-review".to_owned(),
            1,
            serde_json::json!({ "session_log_entry": entry }),
        )
        .expect("v2 record should build"),
    )];

    let review =
        super::super::session_review::session_review_sidebar_lines_from_records(&records, &[])
            .join("\n");
    assert!(review.contains("review: turn 1/1"));
    assert!(review.contains("Review v2 session"));
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

#[test]
fn checkpoint_restore_modal_loads_diff_and_owns_action_keys() -> Result<()> {
    let temp = tempdir()?;
    let workspace = temp.path().join("workspace");
    std::fs::create_dir(&workspace)?;
    let note = workspace.join("note.txt");
    std::fs::write(&note, "before\n")?;
    let config = RootConfig {
        workspace: WorkspaceConfig {
            root: workspace.display().to_string(),
        },
        ..test_config()
    };
    let mut app = AppState::from_root_config(temp.path().join("sigil.toml").as_path(), &config);
    let session_path = temp.path().join("session-checkpoint-actions.jsonl");
    let store = JsonlSessionStore::new(&session_path)?;
    store.append(&SessionLogEntry::User(ModelMessage::user("edit note")))?;
    store.append(&SessionLogEntry::Control(
        ControlEntry::ToolPreviewCaptured(ToolPreviewSnapshot::from_preview(
            "call-edit",
            "edit_file",
            &sample_approval_preview(),
            Default::default(),
            Some("preview-hash".to_owned()),
        )),
    ))?;
    let mut uncommitted_preview = sample_approval_preview();
    uncommitted_preview.file_diffs[0].diff =
        "--- current/note.txt\n+++ proposed/note.txt\n@@ -1 +1 @@\n-before\n+uncommitted-only"
            .to_owned();
    store.append(&SessionLogEntry::Control(
        ControlEntry::ToolPreviewCaptured(ToolPreviewSnapshot::from_preview(
            "call-not-committed",
            "edit_file",
            &uncommitted_preview,
            Default::default(),
            Some("unused-preview-hash".to_owned()),
        )),
    ))?;
    let recorder = sigil_kernel::MutationEventRecorder::new(store.clone());
    sigil_kernel::write_file_with_mutation(
        Some(&recorder),
        &workspace,
        "call-edit",
        "note.txt",
        &note,
        b"after\n",
    )?;
    app.session_log_path = session_path.clone();
    app.sync_current_session_state(JsonlSessionStore::read_entries(&session_path)?);
    app.verification_card_focused = true;
    app.composer.input = "keep this draft".to_owned();
    app.composer.input_cursor = app.composer.input.chars().count();
    app.set_terminal_size(120, 36);

    let open = app
        .handle_key_event(crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Char('r'),
            crossterm::event::KeyModifiers::CONTROL,
        ))?
        .expect("Ctrl-R should request an exact preview immediately");
    assert!(!app.verification_card_focused);
    assert!(app.checkpoint_restore_modal_open());
    assert_eq!(app.composer.input, "keep this draft");
    assert_eq!(
        app.checkpoint_restore_modal_view().map(|view| view.phase),
        Some(super::super::checkpoint_flow::CheckpointRestoreModalPhase::Loading)
    );
    let AppAction::PreviewCheckpointRestore {
        request_id,
        request,
    } = open
    else {
        panic!("Ctrl-R must preview");
    };
    assert!(
        app.handle_key_event(crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Char('i'),
            crossterm::event::KeyModifiers::NONE,
        ))?
        .is_none()
    );
    assert!(
        app.handle_key_event(crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Char('f'),
            crossterm::event::KeyModifiers::NONE,
        ))?
        .is_none()
    );
    assert_eq!(app.composer.input, "keep this draft");
    let records = JsonlSessionStore::read_event_records(&session_path)?;
    let preview = sigil_kernel::preview_controlled_checkpoint_restore(
        &recorder, &records, &workspace, &request,
    )?;
    let preview_for_completion = preview.clone();
    let timeline_len_before_preview = app.timeline.len();
    app.apply_checkpoint_restore_preview(request_id, preview);
    let ready = app
        .checkpoint_restore_modal_view()
        .expect("ready restore modal");
    assert_eq!(
        ready.phase,
        super::super::checkpoint_flow::CheckpointRestoreModalPhase::Ready
    );
    assert!(ready.can_restore);
    assert!(ready.can_fork);
    assert!(ready.body_is_diff);
    assert!(
        ready
            .summary_lines
            .iter()
            .any(|line| line == "Prompt: edit note")
    );
    assert!(
        ready
            .summary_lines
            .iter()
            .any(|line| line.starts_with("Controlled files: 1 · 1 ready"))
    );
    assert!(
        ready
            .summary_lines
            .iter()
            .any(|line| line.starts_with("Boundary: shell and remote side effects"))
    );
    assert!(ready.body_lines.iter().any(|line| line == "-gamma"));
    assert!(ready.body_lines.iter().any(|line| line == "+beta"));
    assert!(
        !ready
            .body_lines
            .iter()
            .any(|line| line.contains("uncommitted-only"))
    );
    assert_eq!(app.timeline.len(), timeline_len_before_preview);
    let backend = ratatui::backend::TestBackend::new(120, 36);
    let mut terminal = ratatui::Terminal::new(backend)?;
    terminal.draw(|frame| crate::ui::render(frame, &app))?;
    let rendered = terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .map(|cell| cell.symbol())
        .collect::<String>();
    assert!(rendered.contains("Restore Checkpoint"));
    assert!(rendered.contains("Reverse diff"));
    assert!(rendered.contains("restore"));
    assert!(rendered.contains("fork (files unchanged)"));

    app.set_terminal_size(64, 24);
    let narrow_backend = ratatui::backend::TestBackend::new(64, 24);
    let mut narrow_terminal = ratatui::Terminal::new(narrow_backend)?;
    narrow_terminal.draw(|frame| crate::ui::render(frame, &app))?;
    let narrow_rendered = narrow_terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .map(|cell| cell.symbol())
        .collect::<String>();
    assert!(narrow_rendered.contains("Enter"));
    assert!(narrow_rendered.contains("F"));
    assert!(narrow_rendered.contains("Esc close"));
    let narrow_view = app
        .checkpoint_restore_modal_view()
        .expect("narrow restore modal");
    let max_scroll = crate::ui::checkpoint_restore_max_scroll(64, 24, &narrow_view);
    assert!(max_scroll > 0);
    app.handle_key_event(crossterm::event::KeyEvent::new(
        crossterm::event::KeyCode::End,
        crossterm::event::KeyModifiers::NONE,
    ))?;
    assert_eq!(
        app.checkpoint_restore_modal_view()
            .expect("scrolled restore modal")
            .scroll,
        max_scroll as u16
    );
    app.handle_key_event(crossterm::event::KeyEvent::new(
        crossterm::event::KeyCode::Up,
        crossterm::event::KeyModifiers::NONE,
    ))?;
    assert_eq!(
        app.checkpoint_restore_modal_view()
            .expect("restore modal scrolled up")
            .scroll,
        max_scroll.saturating_sub(1) as u16
    );
    app.set_terminal_size(120, 36);

    let mut blocked_preview = preview_for_completion.clone();
    blocked_preview.ready = false;
    blocked_preview.files[0].conflict_reason =
        Some(sigil_kernel::CheckpointRestoreConflictReason::CurrentHashMismatch);
    app.checkpoint_restore_preview = Some(blocked_preview);
    let blocked = app
        .checkpoint_restore_modal_view()
        .expect("blocked restore modal");
    assert!(
        blocked
            .body_notice_lines
            .iter()
            .any(|line| line.contains("note.txt · current file changed"))
    );
    app.checkpoint_restore_preview = Some(preview_for_completion.clone());

    let second_enter = app
        .handle_key_event(crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Enter,
            crossterm::event::KeyModifiers::NONE,
        ))?
        .expect("execute action");
    let AppAction::ExecuteCheckpointRestore {
        request_id: execute_request_id,
        ..
    } = second_enter
    else {
        panic!("Enter must execute restore");
    };
    assert!(
        app.handle_key_event(crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Enter,
            crossterm::event::KeyModifiers::NONE,
        ))?
        .is_none()
    );
    assert!(app.apply_checkpoint_restore_completed(execute_request_id, &preview_for_completion));
    let restored_payload: serde_json::Value =
        serde_json::from_str(&app.timeline.last().expect("restored card").text)?;
    assert_eq!(
        restored_payload["metadata"]["details"]["action"],
        "restored"
    );
    assert_eq!(app.timeline.len(), timeline_len_before_preview + 1);

    let reopen = app
        .handle_key_event(crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Char('r'),
            crossterm::event::KeyModifiers::CONTROL,
        ))?
        .expect("reopen preview action");
    let AppAction::PreviewCheckpointRestore {
        request_id: reopen_request_id,
        request: reopen_request,
    } = reopen
    else {
        panic!("reopen must preview");
    };
    let reopen_preview = sigil_kernel::preview_controlled_checkpoint_restore(
        &recorder,
        &JsonlSessionStore::read_event_records(&session_path)?,
        &workspace,
        &reopen_request,
    )?;
    app.apply_checkpoint_restore_preview(reopen_request_id, reopen_preview);
    let fork = app
        .handle_key_event(crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Char('f'),
            crossterm::event::KeyModifiers::NONE,
        ))?
        .expect("fork action");
    assert!(matches!(
        fork,
        AppAction::ForkConversationAtCheckpoint { .. }
    ));
    assert_eq!(app.composer.input, "keep this draft");
    app.clear_checkpoint_interaction();
    assert!(!app.has_modal());

    let latest = app
        .handle_key_event(crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Char('r'),
            crossterm::event::KeyModifiers::CONTROL,
        ))?
        .expect("latest preview action");
    let AppAction::PreviewCheckpointRestore {
        request_id: latest_request_id,
        ..
    } = latest
    else {
        panic!("latest action must preview");
    };
    app.apply_checkpoint_restore_preview(reopen_request_id, preview_for_completion);
    assert_eq!(app.checkpoint_request_id, Some(latest_request_id));
    assert!(app.checkpoint_action_pending);
    assert_eq!(
        app.checkpoint_restore_modal_view().map(|view| view.phase),
        Some(super::super::checkpoint_flow::CheckpointRestoreModalPhase::Loading)
    );
    assert!(app.apply_checkpoint_operation_failed(latest_request_id, "preview failed"));
    let failed = app
        .checkpoint_restore_modal_view()
        .expect("failed preview should stay in the modal");
    assert_eq!(
        failed.phase,
        super::super::checkpoint_flow::CheckpointRestoreModalPhase::Unavailable
    );
    assert_eq!(failed.error.as_deref(), Some("preview failed"));
    assert_eq!(app.composer.input, "keep this draft");
    app.clear_checkpoint_interaction();
    Ok(())
}

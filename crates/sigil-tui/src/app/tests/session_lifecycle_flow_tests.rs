use super::*;

use std::path::{Path, PathBuf};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::Rect;
use sigil_kernel::SessionRef;
use sigil_runtime::{
    LocalSessionCatalogEntry, LocalSessionCatalogState, SessionDeletePreview,
    SessionRetentionCandidate, SessionRetentionPolicy, SessionRetentionPreview,
    SessionRetentionReason,
};

use crate::{
    app::session_lifecycle_flow::SessionModalAction,
    mouse::{AppMouseOutcome, MouseInput, MouseInputKind},
    ui::LayoutSnapshot,
};

fn ready_entry(path: &Path, pinned: bool, finalized_turn_count: usize) -> LocalSessionCatalogEntry {
    LocalSessionCatalogEntry {
        session_ref: SessionRef::new_relative(path.file_name().expect("session file name"))
            .expect("relative session reference"),
        path: path.to_path_buf(),
        state: LocalSessionCatalogState::Ready,
        bytes: 128,
        modified_at_unix_ms: 42,
        session_id: Some("session-target".to_owned()),
        provider_name: Some("deepseek".to_owned()),
        model_name: Some("deepseek-v4-flash".to_owned()),
        title: Some("target session".to_owned()),
        transcript_message_count: 2,
        finalized_turn_count,
        pinned,
    }
}

fn delete_preview(path: &Path) -> SessionDeletePreview {
    SessionDeletePreview {
        source_path: path.to_path_buf(),
        source_session_ref: SessionRef::new_relative(path.file_name().expect("session file name"))
            .expect("relative session reference"),
        source_session_id: "session-target".to_owned(),
        source_content_sha256: "sha256:content".to_owned(),
        source_bytes: 128,
        source_modified_at_unix_ms: 42,
        preview_digest: "sha256:delete-preview".to_owned(),
    }
}

fn app_with_resume_target() -> Result<(tempfile::TempDir, AppState, PathBuf)> {
    let temp = tempdir()?;
    let config = RootConfig {
        workspace: WorkspaceConfig {
            root: temp.path().display().to_string(),
        },
        ..test_config()
    };
    let session_dir = resolved_session_log_dir(&config, temp.path());
    std::fs::create_dir_all(&session_dir)?;
    let target = session_dir.join("session-target.jsonl");
    write_session_log(&target, &restored_entries("deepseek", "deepseek-v4-flash"))?;
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &config);
    app.composer.input = "/resume".to_owned();
    Ok((temp, app, target))
}

#[test]
fn ctrl_o_opens_exclusive_session_actions_and_preserves_draft() -> Result<()> {
    let (_temp, mut app, target) = app_with_resume_target()?;

    let action = app.handle_key_event(KeyEvent::new(KeyCode::Char('o'), KeyModifiers::CONTROL))?;
    let request_id = match action {
        Some(AppAction::InspectLocalSession {
            request_id,
            source_path,
        }) => {
            assert_eq!(source_path, target);
            request_id
        }
        other => panic!("expected inspect action, got {other:?}"),
    };
    assert_eq!(app.composer.input, "/resume");
    assert_eq!(app.modal_title(), Some("Session Actions"));

    app.handle_worker_message(WorkerMessage::LocalSessionInspected {
        request_id,
        entry: ready_entry(&target, false, 1),
    })?;
    assert!(
        app.modal_lines()
            .iter()
            .any(|line| line == "title: target session")
    );

    let fork = app.handle_key_event(KeyEvent::new(KeyCode::Char('f'), KeyModifiers::NONE))?;
    assert!(matches!(
        fork,
        Some(AppAction::ForkLocalSession { source_path, .. }) if source_path == target
    ));
    assert_eq!(app.composer.input, "/resume");
    Ok(())
}

#[test]
fn delete_requires_exact_preview_and_stale_responses_are_ignored() -> Result<()> {
    let (_temp, mut app, target) = app_with_resume_target()?;
    let inspect_request_id =
        match app.handle_key_event(KeyEvent::new(KeyCode::Char('o'), KeyModifiers::CONTROL))? {
            Some(AppAction::InspectLocalSession { request_id, .. }) => request_id,
            other => panic!("expected inspect action, got {other:?}"),
        };
    app.handle_worker_message(WorkerMessage::LocalSessionInspected {
        request_id: inspect_request_id,
        entry: ready_entry(&target, false, 1),
    })?;

    let preview_request_id =
        match app.handle_key_event(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::NONE))? {
            Some(AppAction::PreviewLocalSessionDelete {
                request_id,
                source_path,
            }) => {
                assert_eq!(source_path, target);
                request_id
            }
            other => panic!("expected delete preview action, got {other:?}"),
        };
    let preview = delete_preview(&target);
    app.handle_worker_message(WorkerMessage::LocalSessionDeletePreviewed {
        request_id: preview_request_id,
        preview: preview.clone(),
    })?;
    assert!(
        app.modal_lines()
            .iter()
            .any(|line| line == "content sha256: sha256:content")
    );

    let apply_request_id =
        match app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))? {
            Some(AppAction::ApplyLocalSessionDelete {
                request_id,
                preview: applied,
            }) => {
                assert_eq!(applied, preview);
                request_id
            }
            other => panic!("expected exact delete apply, got {other:?}"),
        };
    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE))?;
    app.handle_worker_message(WorkerMessage::LocalSessionLifecycleFailed {
        request_id: apply_request_id,
        error: "late failure".to_owned(),
    })?;
    assert!(!app.has_modal());
    assert!(app.events.iter().any(|event| {
        event.label == "session:lifecycle"
            && event.detail.contains("ignored stale failure response")
    }));
    Ok(())
}

#[test]
fn retention_footer_uses_worker_preview_and_replays_exact_batch() -> Result<()> {
    let temp = tempdir()?;
    let config = RootConfig {
        workspace: WorkspaceConfig {
            root: temp.path().display().to_string(),
        },
        ..test_config()
    };
    let target = resolved_session_log_dir(&config, temp.path()).join("old.jsonl");
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &config);
    app.open_config_panel();
    let request_id = app
        .drain_pending_worker_commands()
        .into_iter()
        .find_map(|command| match command {
            WorkerCommand::PreviewSessionRetention { request_id, .. } => Some(request_id),
            _ => None,
        })
        .expect("retention preview request");
    let preview = SessionRetentionPreview {
        policy: SessionRetentionPolicy {
            max_sessions: Some(500),
            max_bytes: Some(2 * 1024 * 1024 * 1024),
            expire_older_than_ms: Some(180 * 24 * 60 * 60 * 1000),
        },
        generated_at_unix_ms: 100,
        total_ready_sessions: 2,
        total_ready_bytes: 256,
        protected_sessions: 1,
        pinned_sessions: 0,
        ineligible_sessions: 0,
        selected_bytes: 128,
        constraints_satisfied: true,
        candidates: vec![SessionRetentionCandidate {
            delete_preview: delete_preview(&target),
            reasons: vec![SessionRetentionReason::Age],
        }],
        preview_digest: "sha256:retention-preview".to_owned(),
    };
    app.handle_worker_message(WorkerMessage::SessionRetentionPreviewed {
        request_id,
        preview: preview.clone(),
    })?;
    app.open_session_retention_modal();
    assert_eq!(app.modal_title(), Some("Storage Maintenance"));

    let action = app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))?;
    assert!(matches!(
        action,
        Some(AppAction::ApplySessionRetention { preview: applied, .. }) if applied == preview
    ));
    Ok(())
}

#[test]
fn right_click_opens_actions_and_modal_rows_are_clickable() -> Result<()> {
    let (_temp, mut app, target) = app_with_resume_target()?;
    app.set_terminal_size(120, 24);
    let screen = Rect::new(0, 0, 120, 24);
    let layout = LayoutSnapshot::from_app(screen, &app);
    let slash = layout.slash_overlay.expect("resume selector overlay");
    let input = MouseInput {
        column: slash.content.x,
        row: slash.content.y.saturating_add(slash.title_rows),
        kind: MouseInputKind::RightDown,
        modifiers: KeyModifiers::NONE,
    };
    let request_id = match app.handle_mouse_event(input, &layout)? {
        AppMouseOutcome::Action(AppAction::InspectLocalSession { request_id, .. }) => request_id,
        other => panic!("expected inspect action, got {other:?}"),
    };
    app.handle_worker_message(WorkerMessage::LocalSessionInspected {
        request_id,
        entry: ready_entry(&target, false, 1),
    })?;

    let layout = LayoutSnapshot::from_app(screen, &app);
    let export = layout
        .session_modal_hit_areas
        .as_ref()
        .expect("session modal hit areas")
        .actions
        .iter()
        .find(|area| area.action == SessionModalAction::Export)
        .expect("export action row");
    let input = MouseInput {
        column: export.area.x,
        row: export.area.y,
        kind: MouseInputKind::LeftDown,
        modifiers: KeyModifiers::NONE,
    };
    assert!(matches!(
        app.handle_mouse_event(input, &layout)?,
        AppMouseOutcome::Action(AppAction::ExportLocalSession { source_path, .. })
            if source_path == target
    ));
    assert_eq!(app.composer.input, "/resume");
    Ok(())
}

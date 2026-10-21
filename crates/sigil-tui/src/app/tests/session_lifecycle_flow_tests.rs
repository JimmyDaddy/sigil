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
        resource_bytes: 0,
        resource_tree_sha256: "sha256:empty-resource-tree".to_owned(),
        source_modified_at_unix_ms: 42,
        preview_digest: "sha256:delete-preview".to_owned(),
    }
}

fn app_with_resume_target() -> Result<(tempfile::TempDir, AppState, PathBuf)> {
    let temp = tempdir()?;
    let config = RootConfig {
        config_version: 2,
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
    crate::app::tests::common::settle_session_auxiliary(&mut app);
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
fn branch_knowledge_session_actions_open_selected_source_without_switching_session() -> Result<()> {
    let (_temp, mut app, source) = app_with_resume_target()?;
    let target_id = app.session_id.clone();
    let Some(AppAction::InspectLocalSession { request_id, .. }) =
        app.handle_key_event(KeyEvent::new(KeyCode::Char('o'), KeyModifiers::CONTROL))?
    else {
        anyhow::bail!("expected session action inspection")
    };
    app.handle_worker_message(WorkerMessage::LocalSessionInspected {
        request_id,
        entry: ready_entry(&source, false, 1),
    })?;
    let action = app.handle_key_event(KeyEvent::new(KeyCode::Char('k'), KeyModifiers::NONE))?;
    assert!(
        matches!(action, Some(AppAction::LoadBranchKnowledge { target_session_id, source_session_ref, source_session_id, .. })
        if target_session_id == target_id && source_session_id == "session-target" && source_session_ref.as_path() == Path::new("session-target.jsonl"))
    );
    assert_eq!(app.session_id, target_id);
    assert_eq!(app.composer.input, "/resume");
    assert_eq!(app.modal_title(), Some("Bring Back a Conclusion"));
    assert!(!app.runtime.is_busy);
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
        config_version: 2,
        workspace: WorkspaceConfig {
            root: temp.path().display().to_string(),
        },
        ..test_config()
    };
    let target = resolved_session_log_dir(&config, temp.path()).join("old.jsonl");
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &config);
    app.open_config_panel();
    let request_id = app
        .open_session_retention_modal()
        .and_then(|action| match action {
            AppAction::PreviewSessionRetention { request_id, .. } => Some(request_id),
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

#[test]
fn resume_picker_moves_action_help_out_of_rows_and_does_not_offer_missing_target() -> Result<()> {
    let (_temp, mut app, _) = app_with_resume_target()?;
    assert!(
        app.slash_selector_rows()
            .iter()
            .all(|(_, text)| !text.contains("Ctrl-O") && !text.contains("right-click"))
    );
    app.set_input_and_cursor("/resume NO_SUCH_SESSION".to_owned());
    assert!(app.slash_selector_rows().is_empty());
    let hints = crate::view_model::UiViewModel::from_app(&app).footer.hints;
    assert!(!hints.contains("Enter restore"));
    assert!(hints.contains("UUID"));
    Ok(())
}

#[test]
fn session_details_preserve_phase_draft_and_reject_business_keys_and_old_mouse_frame() -> Result<()>
{
    let (_temp, mut app, target) = app_with_resume_target()?;
    app.set_terminal_size(80, 24);
    let Some(AppAction::InspectLocalSession { request_id, .. }) =
        app.handle_key_event(KeyEvent::new(KeyCode::Char('o'), KeyModifiers::CONTROL))?
    else {
        panic!("inspection");
    };
    let mut entry = ready_entry(&target, true, 3);
    entry.title = Some(format!("首部{}尾部", "长中文e\u{301}🙂".repeat(320)));
    app.handle_worker_message(WorkerMessage::LocalSessionInspected { request_id, entry })?;
    let screen = Rect::new(0, 0, 80, 24);
    let old = LayoutSnapshot::from_app(screen, &app);
    let resume_hit = *old
        .session_modal_hit_areas
        .as_ref()
        .expect("actions")
        .actions
        .iter()
        .find(|hit| hit.action == SessionModalAction::Resume)
        .expect("resume");
    let lines = app.modal_lines();
    assert!(lines[0].starts_with("title: 首部"));
    assert!(lines[1].contains("3 turns") && lines[1].contains("pinned"));
    assert!(
        !lines
            .iter()
            .any(|line| line.starts_with("file:") || line.starts_with("UUID:"))
    );
    app.handle_key_event(KeyEvent::new(KeyCode::Char('i'), KeyModifiers::NONE))?;
    assert!(app.session_actions_view().expect("details").details_open);
    assert!(app.modal_lines().iter().any(|line| line.contains("尾部")));
    for code in [
        KeyCode::Enter,
        KeyCode::Char('f'),
        KeyCode::Char('e'),
        KeyCode::Char('d'),
        KeyCode::F(4),
    ] {
        assert!(
            app.handle_key_event(KeyEvent::new(code, KeyModifiers::NONE))?
                .is_none()
        );
        assert!(
            app.session_actions_view()
                .expect("details retained")
                .details_open
        );
    }
    assert!(matches!(
        app.handle_mouse_event(
            MouseInput {
                kind: MouseInputKind::LeftDown,
                column: resume_hit.area.x,
                row: resume_hit.area.y,
                modifiers: KeyModifiers::NONE,
            },
            &old
        )?,
        AppMouseOutcome::Noop
    ));
    let details_frame = LayoutSnapshot::from_app(screen, &app);
    let area = details_frame
        .session_modal_hit_areas
        .as_ref()
        .expect("details hit areas")
        .modal;
    let details = app.session_actions_view().expect("details before wheel");
    let (max_scroll, viewport_rows) =
        crate::ui::session_actions_scroll_bounds(80, 24, &app.modal_lines(), &details);
    assert!(
        max_scroll > viewport_rows,
        "wheel fixture must exceed one full page"
    );
    assert_eq!(details.scroll_rows, 0);
    assert!(matches!(
        app.handle_mouse_event(
            MouseInput {
                kind: MouseInputKind::ScrollDown,
                column: area.x + 1,
                row: area.y + 1,
                modifiers: KeyModifiers::NONE,
            },
            &details_frame
        )?,
        AppMouseOutcome::Redraw
    ));
    assert!(
        app.session_actions_view()
            .expect("wheel scrolled")
            .scroll_rows
            > 0
    );
    app.handle_key_event(KeyEvent::new(KeyCode::End, KeyModifiers::NONE))?;
    let end = app.session_actions_view().expect("details").scroll_rows;
    assert!(end > 0);
    app.handle_key_event(KeyEvent::new(KeyCode::PageDown, KeyModifiers::NONE))?;
    assert_eq!(
        app.session_actions_view().expect("bounded").scroll_rows,
        end
    );
    app.handle_key_event(KeyEvent::new(KeyCode::Home, KeyModifiers::NONE))?;
    assert_eq!(app.session_actions_view().expect("top").scroll_rows, 0);
    app.handle_key_event(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE))?;
    assert!(
        !app.session_actions_view()
            .expect("actions retained")
            .details_open
    );
    assert_eq!(app.composer.input, "/resume");
    assert!(
        matches!(app.handle_key_event(KeyEvent::new(KeyCode::Char('f'), KeyModifiers::NONE))?,
        Some(AppAction::ForkLocalSession { source_path, .. }) if source_path == target)
    );
    Ok(())
}

#[test]
fn session_actions_production_surface_prioritizes_identity_and_scrolls_delete_evidence()
-> Result<()> {
    let (_temp, mut app, target) = app_with_resume_target()?;
    app.set_terminal_size(80, 24);
    let Some(AppAction::InspectLocalSession { request_id, .. }) =
        app.handle_key_event(KeyEvent::new(KeyCode::Char('o'), KeyModifiers::CONTROL))?
    else {
        panic!("inspection");
    };
    app.handle_worker_message(WorkerMessage::LocalSessionInspected {
        request_id,
        entry: ready_entry(&target, false, 1),
    })?;
    let screen = Rect::new(0, 0, 80, 24);
    let surface = crate::surface_adapter::build_surface_model(
        screen,
        &app,
        crate::surface::SurfaceState {
            frame_generation: 1,
            terminal_epoch: 1,
        },
    );
    let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, 24))?;
    terminal.draw(|frame| crate::ui::render_surface(frame, &surface))?;
    let text = terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .map(|cell| cell.symbol())
        .collect::<String>();
    assert!(
        text.find("title: target session").expect("identity")
            < text.find("[Enter resume]").expect("action")
    );
    assert!(!text.contains("UUID:") && !text.contains("file:") && !text.contains("bytes"));
    for action in &surface
        .layout
        .session_modal_hit_areas
        .as_ref()
        .expect("hits")
        .actions
    {
        let row = (action.area.x..action.area.right())
            .map(|x| {
                terminal
                    .backend()
                    .buffer()
                    .cell((x, action.area.y))
                    .expect("cell")
                    .symbol()
            })
            .collect::<String>();
        assert!(!row.trim().is_empty(), "hit area must have rendered text");
    }
    let Some(AppAction::PreviewLocalSessionDelete { request_id, .. }) =
        app.handle_key_event(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::NONE))?
    else {
        panic!("delete preview");
    };
    let mut preview = delete_preview(&target);
    preview.source_content_sha256 = format!("sha256:{}", "ab".repeat(32));
    preview.resource_tree_sha256 = format!("sha256:{}", "cd".repeat(32));
    assert!(app.apply_local_session_delete_preview(request_id, preview.clone()));
    let evidence = app.modal_lines().join("\n");
    assert!(evidence.contains(&target.display().to_string()));
    assert!(evidence.contains(&preview.source_content_sha256));
    assert!(evidence.contains(&preview.resource_tree_sha256));
    assert!(evidence.contains(&preview.source_session_id));
    assert!(evidence.contains("delete bytes: 128"));
    app.handle_key_event(KeyEvent::new(KeyCode::End, KeyModifiers::NONE))?;
    let surface = crate::surface_adapter::build_surface_model(
        screen,
        &app,
        crate::surface::SurfaceState {
            frame_generation: 2,
            terminal_epoch: 1,
        },
    );
    terminal.draw(|frame| crate::ui::render_surface(frame, &surface))?;
    let text = terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .map(|cell| cell.symbol())
        .collect::<String>();
    assert!(text.contains("[Enter confirm delete]"));
    assert!(
        surface
            .layout
            .session_modal_hit_areas
            .as_ref()
            .expect("hits")
            .actions
            .iter()
            .any(|hit| hit.action == SessionModalAction::ConfirmDelete)
    );
    let stale = surface.layout.clone();
    app.handle_key_event(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE))?;
    let confirm = stale
        .session_modal_hit_areas
        .as_ref()
        .expect("old hits")
        .actions
        .iter()
        .find(|hit| hit.action == SessionModalAction::ConfirmDelete)
        .expect("old confirm");
    assert!(matches!(
        app.handle_mouse_event(
            MouseInput {
                kind: MouseInputKind::LeftDown,
                column: confirm.area.x,
                row: confirm.area.y,
                modifiers: KeyModifiers::NONE
            },
            &stale
        )?,
        AppMouseOutcome::Noop
    ));
    assert!(target.exists(), "no deletion from stale preview");
    Ok(())
}

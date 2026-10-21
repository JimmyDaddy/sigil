use anyhow::Result;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{Terminal, backend::TestBackend, layout::Rect};
use std::sync::Arc;

use crate::{
    app::{
        AppAction, AppState, TimelineRole, UserInputDraftValue,
        tests::{
            common::{
                inject_write_file_approval, sample_approval_preview, test_approval_identity,
                test_config,
            },
            worker_bridge_tests::pending_text_user_input_request,
        },
    },
    mouse::{AppMouseOutcome, HitTarget, MouseInput, MouseInputKind},
    presentation::{CommittedPresentation, PresentationSession},
    runner::WorkerMessage,
    surface::{SurfaceModel, SurfaceState},
    ui::LayoutSnapshot,
};

fn app_fixture() -> Result<(tempfile::TempDir, AppState)> {
    let root = tempfile::tempdir()?;
    let workspace = root.path().join("项目  alpha").join("workspace-details");
    std::fs::create_dir_all(&workspace)?;
    let mut config = test_config();
    config.workspace.root = workspace.display().to_string();
    let app = AppState::from_root_config(&root.path().join("sigil.toml"), &config);
    Ok((root, app))
}

fn press(app: &mut AppState, code: KeyCode) -> Result<Option<AppAction>> {
    app.handle_key_event(KeyEvent::new(code, KeyModifiers::NONE))
}

fn details_key() -> KeyEvent {
    KeyEvent::new(KeyCode::Char('i'), KeyModifiers::ALT)
}

#[test]
fn info_details_aliases_open_and_close_the_same_modal_without_a_sidebar() -> Result<()> {
    let (_root, mut app) = app_fixture()?;
    app.set_terminal_size(80, 24);
    assert_eq!(
        LayoutSnapshot::from_app(Rect::new(0, 0, 80, 24), &app)
            .info_rail
            .width,
        0
    );
    for key in [
        details_key(),
        KeyEvent::new(KeyCode::F(2), KeyModifiers::SHIFT),
    ] {
        assert!(app.handle_key_event(key)?.is_none());
        assert_eq!(app.modal_title(), Some("Details"));
        assert!(app.info_rail_detail_enabled());
        assert!(app.handle_key_event(key)?.is_none());
        assert!(!app.has_modal());
    }
    assert!(app.handle_key_event(details_key())?.is_none());
    assert!(
        app.modal_lines()
            .iter()
            .any(|line| line == &app.workspace_root.display().to_string())
    );
    assert!(app.modal_lines().iter().any(|line| line == "Usage"));
    assert!(
        app.events
            .iter()
            .any(|event| event.detail == "details opened")
    );
    assert!(
        app.events
            .iter()
            .any(|event| event.detail == "details closed")
    );
    Ok(())
}

#[test]
fn info_details_visual_row_scroll_and_resize_keep_the_entire_document_reachable() -> Result<()> {
    let (_root, mut app) = app_fixture()?;
    app.set_terminal_size(80, 24);
    app.handle_key_event(details_key())?;
    let (max_scroll, page_rows) = crate::ui::info_details_scroll_bounds(80, 24, &app.modal_lines());
    assert!(max_scroll > 0 && page_rows > 0);
    press(&mut app, KeyCode::Down)?;
    assert_eq!(app.info_details_view().expect("details").scroll_rows, 1);
    press(&mut app, KeyCode::PageDown)?;
    assert_eq!(
        app.info_details_view().expect("details").scroll_rows,
        (1 + page_rows).min(max_scroll)
    );
    press(&mut app, KeyCode::End)?;
    press(&mut app, KeyCode::Down)?;
    assert_eq!(
        app.info_details_view().expect("details").scroll_rows,
        max_scroll
    );
    app.set_terminal_size(124, 32);
    let (wide_max, wide_page) = crate::ui::info_details_scroll_bounds(124, 32, &app.modal_lines());
    assert!(wide_max < max_scroll);
    assert_eq!(
        app.info_details_view().expect("details").scroll_rows,
        wide_max
    );
    press(&mut app, KeyCode::PageUp)?;
    assert_eq!(
        app.info_details_view().expect("details").scroll_rows,
        wide_max.saturating_sub(wide_page)
    );
    press(&mut app, KeyCode::Home)?;
    press(&mut app, KeyCode::Up)?;
    assert_eq!(app.info_details_view().expect("details").scroll_rows, 0);
    Ok(())
}

#[test]
fn info_details_close_and_paste_preserve_draft_running_state_and_history() -> Result<()> {
    for close in [KeyCode::Esc, KeyCode::Enter] {
        let (_root, mut app) = app_fixture()?;
        app.set_terminal_size(80, 24);
        for index in 0..40 {
            app.push_timeline(TimelineRole::Assistant, format!("Reply {index}"));
        }
        app.timeline_scroll_back = 7;
        app.runtime.is_busy = true;
        app.composer.input = "unsent 中文  follow-up".to_owned();
        app.composer.input_cursor = 3;
        let permission = app.runtime.permission_mode.clone();
        let session = app.session_id.clone();
        let timeline_len = app.timeline.len();
        assert!(app.handle_key_event(details_key())?.is_none());
        app.handle_paste_text("must not enter draft");
        for code in [KeyCode::PageDown, KeyCode::End, KeyCode::Home] {
            assert!(press(&mut app, code)?.is_none());
        }
        assert!(press(&mut app, close)?.is_none());
        assert!(!app.has_modal());
        assert!(app.runtime.is_busy);
        assert!(!app.should_quit);
        assert_eq!(app.composer.input, "unsent 中文  follow-up");
        assert_eq!(app.composer.input_cursor, 3);
        assert_eq!(app.timeline_scroll_back, 7);
        assert_eq!(app.runtime.permission_mode, permission);
        assert_eq!(app.session_id, session);
        assert_eq!(app.timeline.len(), timeline_len);
    }
    Ok(())
}

#[test]
fn info_details_help_entry_uses_the_same_action_and_keeps_other_modal_inputs() -> Result<()> {
    let (_root, mut app) = app_fixture()?;
    app.set_terminal_size(80, 24);
    press(&mut app, KeyCode::F(1))?;
    assert!(
        app.modal_lines()
            .iter()
            .any(|line| line == "Alt-I: Workspace and session details.")
    );
    app.handle_key_event(details_key())?;
    assert_eq!(app.modal_title(), Some("Details"));
    press(&mut app, KeyCode::Esc)?;
    app.open_secret_input(super::SecretInputTarget::SetupApiKey, "test".to_owned());
    app.toggle_info_rail_detail();
    assert_eq!(app.modal_title(), Some("API Key"));
    Ok(())
}

#[test]
fn info_details_control_c_retains_existing_active_cancel_and_idle_quit() -> Result<()> {
    let (_busy_root, mut busy) = app_fixture()?;
    busy.runtime.is_busy = true;
    busy.handle_key_event(details_key())?;
    let lines = busy.modal_lines();
    assert!(lines.iter().any(|line| line == "Ctrl-C: cancel work"));
    assert!(lines.iter().all(|line| line != "Esc: interrupt"));
    assert!(matches!(
        busy.handle_key_event(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL))?,
        Some(AppAction::CancelRun)
    ));
    assert!(!busy.has_modal());
    assert!(!busy.should_quit);
    let (_idle_root, mut idle) = app_fixture()?;
    idle.handle_key_event(details_key())?;
    idle.handle_key_event(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL))?;
    assert!(idle.should_quit);
    Ok(())
}

#[test]
fn info_details_mouse_entry_and_wheel_keep_background_input_untouched() -> Result<()> {
    let (_root, mut app) = app_fixture()?;
    app.set_terminal_size(124, 32);
    app.composer.input = "preserve  draft".to_owned();
    app.composer.input_cursor = 4;
    let layout = LayoutSnapshot::from_app(Rect::new(0, 0, 124, 32), &app);
    let details = layout.info_rail_details.expect("visible details entry");
    assert_eq!(
        layout.hit_target(details.x, details.y),
        HitTarget::InfoRailDetails
    );
    let input = MouseInput {
        column: details.x,
        row: details.y,
        kind: MouseInputKind::LeftDown,
        modifiers: KeyModifiers::NONE,
    };
    assert!(matches!(
        app.handle_mouse_event(input, &layout)?,
        AppMouseOutcome::Redraw
    ));
    assert_eq!(app.modal_title(), Some("Details"));
    let before_scroll = app.timeline_scroll_back;
    app.handle_mouse_event(
        MouseInput {
            kind: MouseInputKind::ScrollDown,
            ..input
        },
        &layout,
    )?;
    assert_eq!(app.info_details_view().expect("details").scroll_rows, 3);
    app.handle_mouse_event(
        MouseInput {
            column: layout.composer.x + 1,
            row: layout.composer.y + 1,
            ..input
        },
        &layout,
    )?;
    assert!(app.has_modal());
    assert_eq!(app.composer.input, "preserve  draft");
    assert_eq!(app.composer.input_cursor, 4);
    assert_eq!(app.timeline_scroll_back, before_scroll);
    Ok(())
}

fn read_only_keys() -> [KeyEvent; 3] {
    [
        details_key(),
        KeyEvent::new(KeyCode::F(2), KeyModifiers::SHIFT),
        KeyEvent::new(KeyCode::F(1), KeyModifiers::NONE),
    ]
}

fn present_attention(app: &AppState) -> Result<(SurfaceModel, Arc<CommittedPresentation>)> {
    let area = Rect::new(0, 0, 124, 40);
    let mut session = PresentationSession::new();
    let generation = session.begin_prepare()?;
    let surface = crate::surface_adapter::build_surface_model(
        area,
        app,
        SurfaceState {
            frame_generation: generation.value(),
            terminal_epoch: session.terminal_epoch().value(),
        },
    );
    let attempt = session.begin_present(generation)?;
    let mut terminal = Terminal::new(TestBackend::new(area.width, area.height))?;
    terminal.draw(|frame| crate::ui::render_surface(frame, &surface))?;
    session.finish_draw(
        generation,
        attempt,
        area,
        terminal.backend().buffer().clone(),
        Arc::new(surface.layout.clone()),
    )?;
    Ok((
        surface,
        session.current().cloned().expect("committed frame"),
    ))
}

fn assert_no_read_only_overlay(surface: &SurfaceModel) {
    assert!(!surface.modal.visible);
    assert!(surface.modal.info_details.is_none());
    assert!(surface.modal.keyboard_help.is_none());
}

fn assert_approval_identity(app: &AppState) {
    let expected = test_approval_identity("call-1");
    let pending = app
        .approval
        .pending
        .as_ref()
        .expect("approval still pending");
    assert!(pending.actions_available());
    assert_eq!(pending.approval_request_id, expected.approval_request_id);
    assert_eq!(pending.call.id, expected.call_id);
    assert_eq!(pending.call.name, "write_file");
}

fn click_committed_details(
    app: &mut AppState,
    presentation: &CommittedPresentation,
) -> Result<AppMouseOutcome> {
    let entry = presentation
        .layout()
        .info_rail_details
        .expect("details entry");
    let column = entry.x + entry.width / 2;
    assert_eq!(
        presentation.layout().hit_target(column, entry.y),
        HitTarget::InfoRailDetails
    );
    app.handle_committed_mouse_event(
        MouseInput {
            column,
            row: entry.y,
            kind: MouseInputKind::LeftDown,
            modifiers: KeyModifiers::NONE,
        },
        presentation,
    )
}

#[test]
fn info_details_attention_approval_arrival_dismisses_details_and_help_without_deciding()
-> Result<()> {
    for key in [
        details_key(),
        KeyEvent::new(KeyCode::F(1), KeyModifiers::NONE),
    ] {
        let (_root, mut app) = app_fixture()?;
        app.set_terminal_size(124, 40);
        app.runtime.is_busy = true;
        app.composer.input = "unsent  draft".to_owned();
        assert!(app.handle_key_event(key)?.is_none());
        assert!(app.has_modal());
        inject_write_file_approval(&mut app, sample_approval_preview())?;
        assert!(!app.has_modal());
        assert_approval_identity(&app);
        let (surface, presentation) = present_attention(&app)?;
        assert_no_read_only_overlay(&surface);
        assert!(surface.approval.is_some());
        let pixels = presentation
            .frame()
            .surface()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(pixels.contains("Review file changes"));
        assert!(pixels.contains("note.txt"));
        assert_eq!(app.composer.input, "unsent  draft");
        assert!(app.runtime.is_busy);
        assert!(!app.should_quit);
        let expected = test_approval_identity("call-1");
        assert!(matches!(
            press(&mut app, KeyCode::Char('n'))?,
            Some(AppAction::ApprovalDecision {
                call_id,
                approval_request_id,
                approved: false,
            }) if call_id == expected.call_id
                && approval_request_id == expected.approval_request_id
        ));
    }
    Ok(())
}

#[test]
fn info_details_attention_pending_approval_blocks_shortcuts_and_committed_mouse_entry() -> Result<()>
{
    let (_root, mut app) = app_fixture()?;
    app.set_terminal_size(124, 40);
    app.runtime.is_busy = true;
    inject_write_file_approval(&mut app, sample_approval_preview())?;
    for key in read_only_keys() {
        assert!(
            app.handle_key_event(key)?.is_none(),
            "a read-only shortcut must not decide approval"
        );
        assert!(!app.has_modal());
        assert_approval_identity(&app);
    }
    let (surface, presentation) = present_attention(&app)?;
    assert_no_read_only_overlay(&surface);
    assert!(matches!(
        click_committed_details(&mut app, &presentation)?,
        AppMouseOutcome::Noop
    ));
    assert!(!app.has_modal());
    assert_approval_identity(&app);
    assert!(app.runtime.is_busy);
    Ok(())
}

#[test]
fn info_details_attention_user_request_dismisses_details_and_help_and_receives_text_enter()
-> Result<()> {
    for key in [
        details_key(),
        KeyEvent::new(KeyCode::F(1), KeyModifiers::NONE),
    ] {
        let (_root, mut app) = app_fixture()?;
        app.set_terminal_size(124, 40);
        app.runtime.is_busy = true;
        app.composer.input = "unsent  composer".to_owned();
        assert!(app.handle_key_event(key)?.is_none());
        assert!(app.has_modal());
        let request = pending_text_user_input_request()?;
        app.handle_worker_message(WorkerMessage::UserInputRequested {
            request: request.clone(),
            entries: Vec::new(),
        })?;
        assert!(!app.has_modal());
        let pending = app.pending_user_input().expect("input request");
        assert!(pending.open);
        assert_eq!(
            pending.request.as_ref().expect("typed request").identity,
            request.identity
        );
        let (surface, presentation) = present_attention(&app)?;
        assert_no_read_only_overlay(&surface);
        assert!(surface.user_input_open);
        let pixels = presentation
            .frame()
            .surface()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(pixels.contains("Input required"));
        assert!(pixels.contains("Which scope should be used?"));
        for character in "repo  scope".chars() {
            assert!(press(&mut app, KeyCode::Char(character))?.is_none());
        }
        assert_eq!(
            app.pending_user_input().expect("form").drafts,
            [UserInputDraftValue::Text("repo  scope".to_owned())]
        );
        assert!(press(&mut app, KeyCode::Enter)?.is_none());
        assert!(
            matches!(press(&mut app, KeyCode::Enter)?, Some(AppAction::SubmitUserInputDecision {
                command_id: None, request_id, generation, expected_request_hash,
                decision: sigil_kernel::UserInputDecisionV1::Submitted { answers },
            }) if request_id == request.identity.request_id.as_str()
                && generation == request.identity.generation
                && expected_request_hash == request.request_hash
                && answers == vec![sigil_kernel::UserInputAnswerV1 {
                    question_id: "scope".to_owned(),
                    value: sigil_kernel::UserInputAnswerValueV1::Text { value: "repo  scope".to_owned() },
                }])
        );
        assert_eq!(app.composer.input, "unsent  composer");
    }
    Ok(())
}

#[test]
fn info_details_attention_open_form_blocks_shortcuts_and_committed_mouse_entry() -> Result<()> {
    let (_root, mut app) = app_fixture()?;
    app.set_terminal_size(124, 40);
    app.runtime.is_busy = true;
    app.composer.input = "unsent  composer".to_owned();
    let request = pending_text_user_input_request()?;
    app.set_pending_user_input(request.clone());
    for key in read_only_keys() {
        assert!(app.handle_key_event(key)?.is_none());
        assert!(!app.has_modal());
        let form = app.pending_user_input().expect("form");
        assert!(form.open);
        assert_eq!(
            form.request.as_ref().expect("typed request").identity,
            request.identity
        );
    }
    let (surface, presentation) = present_attention(&app)?;
    assert_no_read_only_overlay(&surface);
    assert!(surface.user_input_open);
    assert!(matches!(
        click_committed_details(&mut app, &presentation)?,
        AppMouseOutcome::Redraw
    ));
    assert!(!app.has_modal());
    assert!(app.pending_user_input().expect("form").open);
    assert_eq!(
        app.pending_user_input()
            .expect("form")
            .request
            .as_ref()
            .expect("typed request")
            .identity,
        request.identity
    );
    assert!(press(&mut app, KeyCode::Char('a'))?.is_none());
    assert_eq!(
        app.pending_user_input().expect("form").drafts,
        [UserInputDraftValue::Text("a".to_owned())]
    );
    assert_eq!(app.composer.input, "unsent  composer");
    Ok(())
}

#[test]
fn info_details_attention_keeps_stateful_secret_modal_when_typed_requests_arrive() -> Result<()> {
    for approval in [true, false] {
        let (_root, mut app) = app_fixture()?;
        app.open_secret_input(
            super::SecretInputTarget::SetupApiKey,
            "staged-key".to_owned(),
        );
        let lines = app.modal_lines();
        let cursor = app.modal_input_cursor();
        if approval {
            inject_write_file_approval(&mut app, sample_approval_preview())?;
            assert_approval_identity(&app);
        } else {
            let request = pending_text_user_input_request()?;
            app.set_pending_user_input(request.clone());
            assert_eq!(
                app.pending_user_input()
                    .expect("form")
                    .request
                    .as_ref()
                    .expect("typed request")
                    .identity,
                request.identity
            );
        }
        assert_eq!(app.modal_title(), Some("API Key"));
        assert_eq!(app.modal_lines(), lines);
        assert_eq!(app.modal_input_cursor(), cursor);
    }
    Ok(())
}

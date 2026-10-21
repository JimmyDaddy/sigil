use super::*;
use std::sync::Arc;

use crate::{
    app::ApprovalDiffMode,
    mouse::{AppMouseOutcome, HitTarget, MouseInput, MouseInputKind},
    presentation::{CommittedPresentation, PresentationSession},
    surface::{SurfaceModel, SurfaceState},
};
use ratatui::{Terminal, backend::TestBackend, layout::Rect};

fn fixture(width: u16, height: u16) -> Result<(tempfile::TempDir, AppState)> {
    let root = tempfile::tempdir()?;
    let workspace = root.path().join("review-workspace");
    std::fs::create_dir_all(&workspace)?;
    let mut config = test_config();
    config.workspace.root = workspace.display().to_string();
    let mut app = AppState::from_root_config(&root.path().join("sigil.toml"), &config);
    app.set_terminal_size(width, height);
    app.runtime.is_busy = true;
    inject_write_file_approval(&mut app, sample_approval_preview())?;
    Ok((root, app))
}

fn press(app: &mut AppState, code: KeyCode) -> Result<Option<AppAction>> {
    app.handle_key_event(KeyEvent::new(code, KeyModifiers::NONE))
}

fn present(
    app: &AppState,
    width: u16,
    height: u16,
) -> Result<(SurfaceModel, Arc<CommittedPresentation>)> {
    let area = Rect::new(0, 0, width, height);
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
    let mut terminal = Terminal::new(TestBackend::new(width, height))?;
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

fn mouse(kind: MouseInputKind, area: Rect) -> MouseInput {
    MouseInput {
        kind,
        column: area.x + area.width / 2,
        row: area.y + area.height / 2,
        modifiers: KeyModifiers::NONE,
    }
}

fn assert_pending(app: &AppState, call_id: &str) {
    let expected = test_approval_identity(call_id);
    let pending = app.approval.pending.as_ref().expect("pending request");
    assert!(pending.actions_available());
    assert_eq!(pending.call.id, expected.call_id);
    assert_eq!(pending.approval_request_id, expected.approval_request_id);
}

#[test]
fn approval_scroll_end_pageup_pagedown_mouse_and_resize_render_tail_without_losing_guard()
-> Result<()> {
    let (_root, mut app) = fixture(80, 24)?;
    let diff = format!(
        "--- current/note.txt\n+++ proposed/note.txt\n@@ -1 +1,224 @@\n-old\n{}\n+REVIEWTAIL",
        (0..223)
            .map(|index| format!("+{index} {}", "完整中文代码 🌿 保留空白。 ".repeat(5)))
            .collect::<Vec<_>>()
            .join("\n")
    );
    let preview = app
        .approval
        .pending
        .as_mut()
        .and_then(|pending| pending.preview.as_mut())
        .expect("preview");
    preview.body = diff.clone();
    preview.file_diffs[0].diff = diff;
    let frozen_preview =
        serde_json::to_value(&app.approval.pending.as_ref().expect("pending").preview)?;
    let content = |committed: &CommittedPresentation| {
        committed
            .frame()
            .surface()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>()
    };
    for (width, height) in [(80, 24), (166, 48), (80, 24)] {
        app.set_terminal_size(width, height);
        let view = app.approval_modal_view().expect("review open");
        let current_theme = crate::ui::theme::resolve_for_app(&app);
        let max_scroll =
            crate::ui::approval_max_scroll(Rect::new(0, 0, width, height), &view, &current_theme);
        assert!(app.approval.scroll_back <= max_scroll);
        assert!(press(&mut app, KeyCode::End)?.is_none());
        let end = app.approval.scroll_back;
        assert!(end > 8);
        let (surface, committed) = present(&app, width, height)?;
        assert_eq!(surface.approval_scroll_back, end);
        assert!(content(&committed).contains("REVIEWTAIL"));
        assert!(press(&mut app, KeyCode::PageUp)?.is_none());
        assert_eq!(app.approval.scroll_back, end - 8);
        let (_, previous) = present(&app, width, height)?;
        assert_ne!(content(&previous), content(&committed));
        for _ in 0..end.div_ceil(8).saturating_add(2) {
            assert!(press(&mut app, KeyCode::PageDown)?.is_none());
        }
        assert_eq!(app.approval.scroll_back, end);
        let (_, committed) = present(&app, width, height)?;
        let body = committed
            .layout()
            .approval_modal_hit_areas
            .as_ref()
            .expect("approval geometry")
            .diff_area;
        for _ in 0..5 {
            assert!(matches!(
                app.handle_committed_mouse_event(
                    mouse(MouseInputKind::ScrollDown, body),
                    &committed
                )?,
                AppMouseOutcome::Redraw
            ));
            assert_eq!(app.approval.scroll_back, end);
        }
        let (_, committed) = present(&app, width, height)?;
        assert!(content(&committed).contains("REVIEWTAIL"));
        assert!(press(&mut app, KeyCode::Home)?.is_none());
        assert_eq!(app.approval.scroll_back, 0);
        let (_, committed) = present(&app, width, height)?;
        assert!(content(&committed).contains("current/note.txt"));
        assert_pending(&app, "call-1");
        assert!(press(&mut app, KeyCode::End)?.is_none());
    }
    assert_eq!(
        serde_json::to_value(&app.approval.pending.as_ref().expect("pending").preview)?,
        frozen_preview
    );
    Ok(())
}

#[test]
fn approval_review_escape_preserves_draft_identity_and_diff_state() -> Result<()> {
    let (_root, mut app) = fixture(124, 40)?;
    app.composer.input = "保留  draft".to_owned();
    app.composer.input_cursor = 3;
    app.approval.scroll_back = 3;
    app.approval.metadata_collapsed = false;
    app.approval.diff_mode = ApprovalDiffMode::ChangedOnly;
    app.approval.selected_action = ApprovalAction::AllowOnce;
    let before = serde_json::to_value(&app.approval.pending.as_ref().expect("pending").preview)?;
    assert!(app.approval_modal_view().is_some());
    assert!(press(&mut app, KeyCode::Esc)?.is_none());
    assert_eq!(app.active_pane, PaneFocus::Composer);
    assert!(app.approval_modal_view().is_none());
    assert_pending(&app, "call-1");
    assert_eq!(app.composer.input, "保留  draft");
    assert_eq!(app.composer.input_cursor, 3);
    assert_eq!(app.approval.scroll_back, 3);
    assert!(!app.approval.metadata_collapsed);
    assert_eq!(app.approval.diff_mode, ApprovalDiffMode::ChangedOnly);
    assert_eq!(app.approval.selected_action, ApprovalAction::AllowOnce);
    assert_eq!(
        serde_json::to_value(&app.approval.pending.as_ref().expect("pending").preview)?,
        before
    );
    assert!(app.runtime.is_busy);
    Ok(())
}

#[test]
fn approval_review_hidden_letters_paste_and_plain_enter_only_queue_followup() -> Result<()> {
    let (_root, mut app) = fixture(80, 24)?;
    app.approval.selected_action = ApprovalAction::AllowOnce;
    assert!(press(&mut app, KeyCode::Esc)?.is_none());
    for character in "yYnNfFbB 中文 ".chars() {
        assert!(press(&mut app, KeyCode::Char(character))?.is_none());
    }
    app.handle_paste_text("真实粘贴\nread note.txt");
    let expected = "yYnNfFbB 中文 真实粘贴\nread note.txt";
    assert_eq!(app.composer.input, expected);
    assert_pending(&app, "call-1");
    assert!(
        matches!(press(&mut app, KeyCode::Enter)?, Some(AppAction::QueueConversationInput { prompt, .. }) if prompt == expected)
    );
    assert!(app.composer.input.is_empty());
    assert_pending(&app, "call-1");
    assert!(!app.approval_review_open());
    assert!(app.runtime.is_busy);
    Ok(())
}

#[test]
fn approval_review_shift_tab_reopens_from_queue_and_enter_decides_exact_request() -> Result<()> {
    let (_root, mut app) = fixture(124, 40)?;
    app.approval.selected_action = ApprovalAction::AllowOnce;
    press(&mut app, KeyCode::Esc)?;
    app.handle_paste_text("read after cancellation");
    assert!(matches!(
        press(&mut app, KeyCode::Enter)?,
        Some(AppAction::QueueConversationInput { .. })
    ));
    press(&mut app, KeyCode::Tab)?;
    assert!(app.is_composer_queue_panel_focused());
    assert!(
        app.handle_key_event(KeyEvent::new(KeyCode::BackTab, KeyModifiers::SHIFT))?
            .is_none()
    );
    assert!(!app.is_composer_queue_panel_focused());
    assert!(app.approval_review_open());
    assert_eq!(app.approval.selected_action, ApprovalAction::AllowOnce);
    let expected = test_approval_identity("call-1");
    assert!(
        matches!(press(&mut app, KeyCode::Enter)?, Some(AppAction::ApprovalDecision {
        call_id, approval_request_id, approved: true,
    }) if call_id == expected.call_id && approval_request_id == expected.approval_request_id)
    );
    Ok(())
}

#[test]
fn approval_review_stale_committed_mouse_cannot_decide_or_mutate_hidden_review() -> Result<()> {
    let (_root, mut app) = fixture(124, 40)?;
    let (_surface, previous) = present(&app, 124, 40)?;
    let actions = previous
        .layout()
        .approval_modal_hit_areas
        .as_ref()
        .expect("approval geometry");
    let allow = actions.allow_once_action;
    assert_eq!(
        previous
            .layout()
            .hit_target(allow.x + allow.width / 2, allow.y + allow.height / 2),
        HitTarget::ApprovalAction {
            action: ApprovalAction::AllowOnce
        }
    );
    press(&mut app, KeyCode::Esc)?;
    let scroll = app.approval.scroll_back;
    for kind in [
        MouseInputKind::LeftDown,
        MouseInputKind::LeftUp,
        MouseInputKind::ScrollDown,
    ] {
        assert!(matches!(
            app.handle_committed_mouse_event(mouse(kind, allow), &previous)?,
            AppMouseOutcome::Noop
        ));
    }
    assert_pending(&app, "call-1");
    assert_eq!(app.active_pane, PaneFocus::Composer);
    assert_eq!(app.approval.scroll_back, scroll);
    Ok(())
}

#[test]
fn approval_review_hidden_fresh_mouse_can_focus_composer_and_queue() -> Result<()> {
    let (_root, mut app) = fixture(124, 40)?;
    press(&mut app, KeyCode::Esc)?;
    let (surface, current) = present(&app, 124, 40)?;
    assert!(surface.approval.is_none());
    assert!(current.layout().approval_modal_hit_areas.is_none());
    let composer = current.layout().composer_input;
    assert_eq!(
        current
            .layout()
            .hit_target(composer.x + composer.width / 2, composer.y),
        HitTarget::Composer
    );
    assert!(matches!(
        app.handle_committed_mouse_event(mouse(MouseInputKind::LeftDown, composer), &current)?,
        AppMouseOutcome::Redraw
    ));
    app.handle_paste_text("queued read");
    assert!(
        matches!(press(&mut app, KeyCode::Enter)?, Some(AppAction::QueueConversationInput { prompt, .. }) if prompt == "queued read")
    );
    assert_pending(&app, "call-1");
    Ok(())
}

fn request_next(app: &mut AppState, call_id: &str, request_id: &str) -> Result<()> {
    let previous = app.approval.pending.as_ref().expect("previous request");
    let mut call = previous.call.clone();
    call.id = call_id.to_owned();
    let event = RunEvent::ToolApprovalRequested {
        display_call_id: None,
        approval_identity: {
            let mut identity = test_approval_identity(call_id);
            identity.approval_request_id = request_id.to_owned();
            identity
        },
        effects: previous.effects.clone(),
        analysis: previous.analysis.clone(),
        containment: previous.containment.clone(),
        safe_summary: previous.safe_summary.clone(),
        decision_reasons: previous.decision_reasons.clone(),
        session_grant_available: previous.session_grant_available,
        session_grant_unavailable_reason: previous.session_grant_unavailable_reason,
        call,
        spec: previous.spec.clone(),
        subjects: previous.subjects.clone(),
        network_effect: previous.network_effect,
        local_policy_decision: previous.local_policy_decision,
        network_policy_decision: previous.network_policy_decision,
        source_policy_decision: previous.source_policy_decision,
        operation: previous.operation,
        risk: previous.risk,
        subject_zones: previous.subject_zones.clone(),
        confirmation: previous.confirmation.clone(),
        snapshot_required: previous.snapshot_required,
        command_permission_matches: previous.command_permission_matches.clone(),
        preview: previous.preview.clone(),
    };
    app.handle(event)?;
    Ok(())
}

#[test]
fn approval_review_new_typed_request_automatically_reopens_review() -> Result<()> {
    let (_root, mut app) = fixture(124, 40)?;
    press(&mut app, KeyCode::Esc)?;
    request_next(&mut app, "call-next", "approval-call-next")?;
    assert_pending(&app, "call-next");
    assert_eq!(app.active_pane, PaneFocus::Activity);
    assert!(app.approval_modal_view().is_some());
    Ok(())
}

#[test]
fn approval_review_control_c_keeps_cancel_semantics_open_and_hidden() -> Result<()> {
    for hide in [false, true] {
        let (_root, mut app) = fixture(80, 24)?;
        if hide {
            press(&mut app, KeyCode::Esc)?;
        }
        assert!(matches!(
            app.handle_key_event(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL))?,
            Some(AppAction::CancelRun)
        ));
        assert!(!app.should_quit);
        assert_pending(&app, "call-1");
    }
    Ok(())
}

#[test]
fn approval_review_narrow_surface_keeps_escape_and_pending_queue_affordances_visible() -> Result<()>
{
    for (width, height) in [(80, 24), (124, 40)] {
        let (_root, mut app) = fixture(width, height)?;
        let (surface, current) = present(&app, width, height)?;
        assert!(surface.approval.is_some());
        let pixels = current
            .frame()
            .surface()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(
            pixels.contains("Esc input"),
            "{width}x{height} review must show escape action"
        );
        press(&mut app, KeyCode::Esc)?;
        let (surface, current) = present(&app, width, height)?;
        assert!(surface.approval.is_none());
        assert!(surface.layout.approval_modal.is_none());
        let pixels = current
            .frame()
            .surface()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        for hint in ["Approval pending", "Shift-Tab review", "Enter queue"] {
            assert!(pixels.contains(hint), "{width}x{height} missing {hint}");
        }
    }
    Ok(())
}

#[test]
fn approval_review_control_and_alt_letters_do_not_decide_requests() -> Result<()> {
    let (_root, mut app) = fixture(124, 40)?;
    for modifiers in [KeyModifiers::CONTROL, KeyModifiers::ALT] {
        for character in ['y', 'n', 'b', 'f'] {
            let action =
                app.handle_key_event(KeyEvent::new(KeyCode::Char(character), modifiers))?;
            assert!(!matches!(
                action,
                Some(
                    AppAction::ApprovalDecision { .. }
                        | AppAction::ApprovalSessionDecision { .. }
                        | AppAction::ApprovalFamilyDecision { .. }
                        | AppAction::ApprovalDecisionWithArgs { .. }
                )
            ));
            assert_pending(&app, "call-1");
        }
    }
    Ok(())
}

#[test]
fn approval_review_plain_enter_with_empty_hidden_draft_does_not_decide() -> Result<()> {
    let (_root, mut app) = fixture(80, 24)?;
    app.approval.selected_action = ApprovalAction::AllowOnce;
    press(&mut app, KeyCode::Esc)?;
    assert!(press(&mut app, KeyCode::Enter)?.is_none());
    assert_pending(&app, "call-1");
    assert!(!app.approval_review_open());
    Ok(())
}

#[test]
fn approval_review_committed_request_identity_rejects_old_frame_and_accepts_new_frame() -> Result<()>
{
    for (call_id, request_id) in [
        ("call-next", "approval-call-next"),
        ("call-1", "approval-call-1-next"),
    ] {
        let (_root, mut app) = fixture(124, 40)?;
        let (_, old_frame) = present(&app, 124, 40)?;
        let old_allow = old_frame
            .layout()
            .approval_modal_hit_areas
            .as_ref()
            .expect("old review")
            .allow_once_action;
        press(&mut app, KeyCode::Esc)?;
        request_next(&mut app, call_id, request_id)?;
        assert!(app.approval_review_open());
        assert!(matches!(
            app.handle_committed_mouse_event(
                mouse(MouseInputKind::LeftDown, old_allow),
                &old_frame
            )?,
            AppMouseOutcome::Noop
        ));
        let pending = app.approval.pending.as_ref().expect("new request");
        assert_eq!(pending.call.id, call_id);
        assert_eq!(pending.approval_request_id, request_id);
        assert!(pending.actions_available());
        let (_, new_frame) = present(&app, 124, 40)?;
        assert_eq!(
            new_frame.layout().approval_request_identity,
            Some((call_id.to_owned(), request_id.to_owned()))
        );
        let allow = new_frame
            .layout()
            .approval_modal_hit_areas
            .as_ref()
            .expect("new review")
            .allow_once_action;
        assert!(
            matches!(app.handle_committed_mouse_event(mouse(MouseInputKind::LeftDown, allow), &new_frame)?, AppMouseOutcome::Action(AppAction::ApprovalDecision {
            call_id: actual_call, approval_request_id: actual_request, approved: true,
        }) if actual_call == call_id && actual_request == request_id)
        );
    }
    Ok(())
}

#[test]
fn approval_review_mcp_input_focus_does_not_reopen_hidden_review() -> Result<()> {
    let (_root, mut app) = fixture(124, 40)?;
    press(&mut app, KeyCode::Esc)?;
    let (response_tx, _response_rx) = tokio::sync::oneshot::channel();
    app.handle_worker_message(WorkerMessage::McpElicitationRequest {
        request: McpElicitationRequest {
            server_name: "filesystem".to_owned(), message: "Need path".to_owned(),
            requested_schema: json!({ "type": "object", "properties": { "path": { "type": "string" } }, "required": ["path"] }),
        }, response_tx,
    })?;
    assert!(app.pending_user_input().is_some_and(|form| form.open));
    assert_eq!(app.active_pane, PaneFocus::Activity);
    assert!(!app.approval_review_open());
    assert!(press(&mut app, KeyCode::Esc)?.is_none());
    assert!(app.pending_user_input().is_some_and(|form| !form.open));
    assert!(!app.approval_review_open());
    assert!(press(&mut app, KeyCode::Char('y'))?.is_none());
    assert!(press(&mut app, KeyCode::Enter)?.is_none());
    assert_eq!(app.composer.input, "y");
    assert_pending(&app, "call-1");
    Ok(())
}

#[test]
fn approval_review_late_compaction_preview_does_not_reopen_hidden_review() -> Result<()> {
    let (root, mut app) = fixture(124, 40)?;
    press(&mut app, KeyCode::Esc)?;
    let store = JsonlSessionStore::new(root.path().join("preview.jsonl"))?;
    store.append(&SessionLogEntry::User(ModelMessage::user("old request")))?;
    store.append(&SessionLogEntry::Assistant(ModelMessage::assistant(
        Some("old response".to_owned()),
        Vec::new(),
    )))?;
    store.append(&SessionLogEntry::User(ModelMessage::user("latest request")))?;
    let preview = adaptive_test_compaction_preview(&store)?;
    app.handle_worker_message(WorkerMessage::V2CompactionPreviewed {
        state: crate::runner::V2CompactionPreviewState::Review(Box::new(
            crate::runner::V2CompactionReview {
                request_id: 91,
                strategy: sigil_kernel::CompactionStrategy::CacheAwareV3,
                preview,
                admission: crate::runner::V2CompactionAdmission::Unavailable {
                    reason: "test has no admitted target".to_owned(),
                },
                tool_output_shrink_candidates: Vec::new(),
                continuity: None,
                native_carrier_requested: false,
            },
        )),
    })?;
    assert_eq!(app.modal_title(), Some("Context Compaction"));
    assert_eq!(app.active_pane, PaneFocus::Activity);
    assert!(!app.approval_review_open());
    assert!(matches!(
        press(&mut app, KeyCode::Esc)?,
        Some(AppAction::CancelV2CompactionReview { request_id: 91 })
    ));
    assert!(!app.has_modal());
    assert!(!app.approval_review_open());
    assert!(press(&mut app, KeyCode::Char('n'))?.is_none());
    assert!(
        matches!(press(&mut app, KeyCode::Enter)?, Some(AppAction::QueueConversationInput { prompt, .. }) if prompt == "n")
    );
    assert_pending(&app, "call-1");
    Ok(())
}

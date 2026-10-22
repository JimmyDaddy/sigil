use anyhow::Result;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::*;
use crate::app::{AppAction, PaneFocus, tests::common::test_config};

fn fixture() -> Result<(tempfile::TempDir, AppState)> {
    let root = tempfile::tempdir()?;
    let mut config = test_config();
    config.workspace.root = root.path().display().to_string();
    let mut app = AppState::from_root_config(&root.path().join("sigil.toml"), &config);
    app.active_pane = PaneFocus::Composer;
    app.set_terminal_size(80, 24);
    Ok((root, app))
}

fn key(app: &mut AppState, code: KeyCode, modifiers: KeyModifiers) -> Result<()> {
    app.handle_key_event(KeyEvent::new(code, modifiers))?;
    Ok(())
}

fn snapshot(input: String) -> ComposerEditSnapshot {
    ComposerEditSnapshot {
        cursor: input.chars().count(),
        input,
        selection_anchor: None,
        paste_spans: Vec::new(),
    }
}

#[test]
fn composer_edit_history_limits_steps_and_drops_redo_on_new_edits() {
    let mut history = ComposerEditHistory::default();
    for index in 0..MAX_EDIT_STEPS + 3 {
        history.record(snapshot(format!("draft {index}")));
    }
    assert_eq!(history.undo.len(), MAX_EDIT_STEPS);
    assert_eq!(
        history.undo.front().map(|entry| entry.input.as_str()),
        Some("draft 3")
    );
    history.redo.push_back(snapshot("old future".to_owned()));
    history.record(snapshot("new edit".to_owned()));
    assert!(history.redo.is_empty());
    assert_eq!(history.undo.len(), MAX_EDIT_STEPS);
}

#[test]
fn composer_edit_history_limits_retained_bytes_including_paste_metadata() {
    let mut history = ComposerEditHistory::default();
    for _ in 0..4 {
        let mut value = snapshot("a".repeat(MAX_EDIT_BYTES / 2));
        value.paste_spans.push(ComposerPasteSpan {
            start: 0,
            end: value.input.len(),
            char_count: value.input.len(),
            line_count: 1,
        });
        history.record(value);
    }
    assert_eq!(history.undo.len(), 1);
    assert!(
        history
            .undo
            .iter()
            .chain(&history.redo)
            .map(ComposerEditSnapshot::retained_bytes)
            .sum::<usize>()
            <= MAX_EDIT_BYTES
    );
}

#[test]
fn composer_edit_history_undo_redo_restores_unicode_selection_and_exact_cursor() -> Result<()> {
    let (_root, mut app) = fixture()?;
    let family = "👨‍👩‍👧‍👦";
    app.handle_paste_text(&format!("前e\u{301}{family}后"));
    key(&mut app, KeyCode::Left, KeyModifiers::NONE)?;
    key(&mut app, KeyCode::Left, KeyModifiers::SHIFT)?;
    let original = app.composer.input.clone();
    let cursor = app.composer.input_cursor;
    let anchor = app.composer.input_selection_anchor;
    app.handle_paste_text("替\n换");
    assert_eq!(app.composer.input, "前e\u{301}替\n换后");
    key(&mut app, KeyCode::Char('z'), KeyModifiers::CONTROL)?;
    assert_eq!(app.composer.input, original);
    assert_eq!(app.composer.input_cursor, cursor);
    assert_eq!(app.composer.input_selection_anchor, anchor);
    key(&mut app, KeyCode::Char('y'), KeyModifiers::CONTROL)?;
    assert_eq!(app.composer.input, "前e\u{301}替\n换后");
    key(&mut app, KeyCode::Char('z'), KeyModifiers::CONTROL)?;
    key(
        &mut app,
        KeyCode::Char('Z'),
        KeyModifiers::CONTROL | KeyModifiers::SHIFT,
    )?;
    assert_eq!(app.composer.input, "前e\u{301}替\n换后");
    Ok(())
}

#[test]
fn composer_edit_history_movement_and_noop_deletes_do_not_create_edits() -> Result<()> {
    let (_root, mut app) = fixture()?;
    key(&mut app, KeyCode::Backspace, KeyModifiers::NONE)?;
    key(&mut app, KeyCode::Char('甲'), KeyModifiers::NONE)?;
    key(&mut app, KeyCode::Left, KeyModifiers::NONE)?;
    key(&mut app, KeyCode::Backspace, KeyModifiers::NONE)?;
    key(&mut app, KeyCode::Right, KeyModifiers::NONE)?;
    key(&mut app, KeyCode::Char('z'), KeyModifiers::CONTROL)?;
    assert!(app.composer.input.is_empty());
    key(&mut app, KeyCode::Char('y'), KeyModifiers::CONTROL)?;
    assert_eq!(app.composer.input, "甲");
    key(&mut app, KeyCode::Char('z'), KeyModifiers::CONTROL)?;
    key(&mut app, KeyCode::Char('乙'), KeyModifiers::NONE)?;
    key(
        &mut app,
        KeyCode::Char('Z'),
        KeyModifiers::CONTROL | KeyModifiers::SHIFT,
    )?;
    assert_eq!(app.composer.input, "乙");
    Ok(())
}

#[test]
fn composer_edit_history_restores_collapsed_paste_spans_and_multiline_redo() -> Result<()> {
    let (_root, mut app) = fixture()?;
    let large = "甲\n".repeat(5_000);
    app.handle_paste_text(&large);
    let spans = app.composer.input_paste_spans.clone();
    assert_eq!(spans.len(), 1);
    app.handle_paste_text("tail\n尾部");
    key(&mut app, KeyCode::Char('z'), KeyModifiers::CONTROL)?;
    assert_eq!(app.composer.input, large);
    assert_eq!(app.composer.input_paste_spans, spans);
    assert!(app.composer_display_input().contains("[Pasted text #1:"));
    key(&mut app, KeyCode::Char('z'), KeyModifiers::CONTROL)?;
    assert!(app.composer.input.is_empty());
    key(&mut app, KeyCode::Char('y'), KeyModifiers::CONTROL)?;
    assert_eq!(app.composer.input, large);
    assert_eq!(app.composer.input_paste_spans, spans);
    key(
        &mut app,
        KeyCode::Char('Z'),
        KeyModifiers::CONTROL | KeyModifiers::SHIFT,
    )?;
    assert_eq!(app.composer.input, format!("{large}tail\n尾部"));
    Ok(())
}

#[test]
fn composer_edit_history_undo_esc_and_keep_kill_yank_shortcuts() -> Result<()> {
    let (_root, mut app) = fixture()?;
    app.set_input_and_cursor("甲🙂乙".to_owned());
    key(&mut app, KeyCode::Esc, KeyModifiers::NONE)?;
    key(&mut app, KeyCode::Char('z'), KeyModifiers::CONTROL)?;
    assert_eq!(app.composer.input, "甲🙂乙");
    assert_eq!(app.last_notice(), Some("draft restored"));
    key(&mut app, KeyCode::Char('y'), KeyModifiers::CONTROL)?;
    assert!(app.composer.input.is_empty());
    key(&mut app, KeyCode::Char('z'), KeyModifiers::CONTROL)?;
    key(&mut app, KeyCode::Char('a'), KeyModifiers::CONTROL)?;
    key(&mut app, KeyCode::Char('k'), KeyModifiers::CONTROL)?;
    assert_eq!(app.composer.input_kill_buffer.as_deref(), Some("甲🙂乙"));
    key(&mut app, KeyCode::Char('y'), KeyModifiers::CONTROL)?;
    assert_eq!(app.composer.input, "甲🙂乙");
    key(&mut app, KeyCode::Char('z'), KeyModifiers::CONTROL)?;
    key(&mut app, KeyCode::Char('y'), KeyModifiers::ALT)?;
    assert_eq!(app.composer.input, "甲🙂乙");
    key(
        &mut app,
        KeyCode::Char('Z'),
        KeyModifiers::CONTROL | KeyModifiers::SHIFT,
    )?;
    assert_eq!(app.composer.input, "甲🙂乙");
    Ok(())
}

#[test]
fn composer_edit_history_backspace_delete_newline_and_slash_completion_are_undoable() -> Result<()>
{
    let (_root, mut app) = fixture()?;
    app.set_input_and_cursor("前👨‍👩‍👧‍👦后".to_owned());
    key(&mut app, KeyCode::Left, KeyModifiers::NONE)?;
    let original = app.composer.input.clone();
    key(&mut app, KeyCode::Backspace, KeyModifiers::NONE)?;
    assert_eq!(app.composer.input, "前后");
    key(&mut app, KeyCode::Char('z'), KeyModifiers::CONTROL)?;
    assert_eq!(app.composer.input, original);
    key(&mut app, KeyCode::Delete, KeyModifiers::NONE)?;
    assert_eq!(app.composer.input, "前👨‍👩‍👧‍👦");
    key(&mut app, KeyCode::Char('z'), KeyModifiers::CONTROL)?;
    key(&mut app, KeyCode::Char('j'), KeyModifiers::CONTROL)?;
    assert_eq!(app.composer.input, "前👨‍👩‍👧‍👦\n后");
    key(&mut app, KeyCode::Char('z'), KeyModifiers::CONTROL)?;
    assert_eq!(app.composer.input, original);
    app.reset_composer_edit_context();
    app.set_input_and_cursor("/mod".to_owned());
    key(&mut app, KeyCode::Tab, KeyModifiers::NONE)?;
    assert!(app.composer.input.starts_with("/model"));
    key(&mut app, KeyCode::Char('z'), KeyModifiers::CONTROL)?;
    assert_eq!(app.composer.input, "/mod");
    Ok(())
}

#[test]
fn composer_edit_history_prompt_navigation_preserves_original_edit_context() -> Result<()> {
    let (_root, mut app) = fixture()?;
    app.composer.input_history = vec!["prior prompt".to_owned()];
    app.handle_paste_text("draft");
    key(&mut app, KeyCode::Char('!'), KeyModifiers::NONE)?;
    app.composer.input_cursor = 2;
    app.composer.input_selection_anchor = Some(4);
    app.navigate_input_history(true);
    assert_eq!(app.composer.input, "prior prompt");
    assert!(!app.undo_composer_edit());
    app.navigate_input_history(false);
    assert_eq!(app.composer.input, "draft!");
    assert_eq!(app.composer.input_cursor, 2);
    assert_eq!(app.composer.input_selection_anchor, Some(4));
    assert!(app.undo_composer_edit());
    assert_eq!(app.composer.input, "draft");
    Ok(())
}

#[test]
fn composer_edit_history_submission_clears_history_and_rejected_admission_restores_it() -> Result<()>
{
    let (_root, mut app) = fixture()?;
    app.handle_paste_text("draft");
    key(&mut app, KeyCode::Char('!'), KeyModifiers::NONE)?;
    let action = app.submit_input()?.expect("submission");
    assert!(matches!(&action, AppAction::SubmitPrompt(prompt) if prompt == "draft!"));
    assert!(!app.undo_composer_edit());
    assert!(!app.redo_composer_edit());
    app.restore_unadmitted_run_input(&action);
    assert_eq!(app.composer.input, "draft!");
    assert!(app.undo_composer_edit());
    assert_eq!(app.composer.input, "draft");
    Ok(())
}

#[test]
fn composer_edit_history_late_rejection_does_not_replace_new_draft_history() -> Result<()> {
    let (_root, mut app) = fixture()?;
    app.handle_paste_text("submitted");
    let action = app.submit_input()?.expect("submission");
    app.handle_paste_text("new draft");
    app.restore_unadmitted_run_input(&action);
    assert_eq!(app.composer.input, "new draft");
    assert!(app.undo_composer_edit());
    assert!(app.composer.input.is_empty());
    Ok(())
}

#[test]
fn composer_edit_history_session_switch_discards_old_undo_redo_and_kill_context() -> Result<()> {
    let (root, mut app) = fixture()?;
    app.handle_paste_text("old draft");
    app.composer.input_kill_buffer = Some("old kill".to_owned());
    app.restore_session_view(
        "other-session".to_owned(),
        root.path().join("other.jsonl"),
        "Fixture".to_owned(),
        "fixture-model".to_owned(),
        Vec::new(),
        "switched",
    );
    assert!(!app.undo_composer_edit());
    assert!(!app.redo_composer_edit());
    assert!(app.composer.input_kill_buffer.is_none());
    Ok(())
}

#[test]
fn composer_edit_history_queue_cancel_and_success_restore_original_editor_history() -> Result<()> {
    use crate::runner::WorkerMessage;
    use sigil_kernel::{
        ControlEntry, ConversationInputKind, ConversationInputQueueId,
        ConversationInputQueuedEntry, ConversationInputTarget, SessionLogEntry,
    };
    let (_root, mut app) = fixture()?;
    app.sync_current_session_state(vec![SessionLogEntry::Control(
        ControlEntry::ConversationInputQueued(ConversationInputQueuedEntry {
            queue_id: ConversationInputQueueId::new("undo-queue")?,
            target: ConversationInputTarget::MainThread,
            kind: ConversationInputKind::Chat,
            prompt: "queued prompt".to_owned(),
            prompt_hash: "sha256:undo-queue".to_owned(),
            reasoning_effort: None,
            created_at_ms: None,
        }),
    )]);
    app.handle_paste_text("original");
    key(&mut app, KeyCode::Char('!'), KeyModifiers::NONE)?;
    app.composer.input_kill_buffer = Some("original kill".to_owned());
    assert!(app.focus_composer_queue_panel());
    assert!(app.begin_edit_selected_queue_item());
    assert!(!app.undo_composer_edit());
    assert!(app.composer.input_kill_buffer.is_none());
    key(&mut app, KeyCode::Char('?'), KeyModifiers::NONE)?;
    assert!(app.undo_composer_edit());
    assert_eq!(app.composer.input, "queued prompt");
    key(&mut app, KeyCode::Esc, KeyModifiers::NONE)?;
    assert_eq!(app.composer.input, "original!");
    assert_eq!(
        app.composer.input_kill_buffer.as_deref(),
        Some("original kill")
    );
    assert!(app.undo_composer_edit());
    assert_eq!(app.composer.input, "original");
    assert!(app.redo_composer_edit());
    assert!(app.begin_edit_selected_queue_item());
    app.handle_paste_text(" updated");
    let action = app.submit_input()?.expect("queue edit submission");
    let AppAction::EditQueuedConversationInput { queue_id, .. } = action else {
        anyhow::bail!("unexpected queue action");
    };
    let operation = app
        .composer
        .pending_queue_operations
        .get(&queue_id)
        .cloned()
        .expect("pending edit");
    app.handle_worker_message(WorkerMessage::ConversationQueueOperationCompleted {
        session_log_path: app.session_log_path.clone(),
        operation,
        result: Ok(()),
    })?;
    assert_eq!(app.composer.input, "original!");
    assert_eq!(
        app.composer.input_kill_buffer.as_deref(),
        Some("original kill")
    );
    assert!(app.undo_composer_edit());
    assert_eq!(app.composer.input, "original");
    Ok(())
}

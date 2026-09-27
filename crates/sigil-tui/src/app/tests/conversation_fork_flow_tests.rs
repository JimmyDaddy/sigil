use super::*;
use sigil_runtime::application_recovery::ApplicationConversationForkPointView;

fn point(index: usize) -> ApplicationConversationForkPointView {
    ApplicationConversationForkPointView {
        source_turn_index: index,
        prompt_preview: Some(format!("Prompt {index}")),
        source_turn_digest: format!("digest-{index}"),
        source_boundary_stream_sequence: index as u64 * 3 + 1,
        source_finalized_stream_sequence: index as u64 * 3 + 3,
    }
}

fn open_points(app: &mut AppState) -> Result<u64> {
    let Some(AppAction::LoadConversationForkPoints {
        request_id,
        source_session_id,
    }) = app.open_conversation_fork_modal()
    else {
        anyhow::bail!("expected fork source load")
    };
    app.handle_worker_message(WorkerMessage::ConversationForkPointsLoaded {
        request_id,
        source_session_id,
        points: vec![point(0), point(1)],
    })?;
    Ok(request_id)
}

#[test]
fn conversation_fork_selector_uses_exact_turn_preserves_draft_and_never_submits() -> Result<()> {
    let config = test_config();
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &config);
    app.composer.input = "editable next direction".to_owned();
    open_points(&mut app)?;
    assert_eq!(app.modal_title(), Some("Branch Conversation"));
    assert!(
        app.modal_lines()
            .iter()
            .any(|line| line.contains("Prompt 1"))
    );
    app.handle_key_event(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE))?;
    let Some(AppAction::ForkConversation {
        request_id,
        source_session_id,
        source_turn_digest,
        ..
    }) = app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))?
    else {
        anyhow::bail!("expected exact fork without a submit")
    };
    assert_eq!(source_session_id, app.session_id);
    assert_eq!(source_turn_digest, "digest-0");
    assert_eq!(app.composer.input, "editable next direction");
    assert!(app.conversation_fork_applying());
    assert!(
        app.handle_key_event(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE))?
            .is_none()
    );
    assert!(app.conversation_fork_modal_open());
    app.handle_worker_message(WorkerMessage::LocalSessionForked {
        session_id: "new-branch".to_owned(),
        request_id,
        session_log_path: Path::new("new-branch.jsonl").to_path_buf(),
        provider_name: "deepseek".to_owned(),
        model_name: "deepseek-v4-flash".to_owned(),
        copied_message_count: 2,
        entries: restored_entries("deepseek", "deepseek-v4-flash"),
    })?;
    assert_eq!(app.session_id, "new-branch");
    assert_eq!(app.active_pane, PaneFocus::Composer);
    assert_eq!(app.composer.input, "editable next direction");
    assert!(!app.runtime.is_busy);
    assert!(app.runtime.worker_rebind_required);
    assert!(!app.conversation_fork_modal_open());
    Ok(())
}

#[test]
fn conversation_fork_selector_ignores_stale_scope_and_retains_failed_draft() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.composer.input = "draft".to_owned();
    let old_request = open_points(&mut app)?;
    app.handle_key_event(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE))?;
    let current_request = open_points(&mut app)?;
    app.handle_worker_message(WorkerMessage::ConversationForkPointsLoaded {
        request_id: current_request,
        source_session_id: "another-session".to_owned(),
        points: vec![point(90)],
    })?;
    app.handle_worker_message(WorkerMessage::LocalSessionLifecycleFailed {
        request_id: old_request,
        error: "old failure".to_owned(),
    })?;
    assert!(
        !app.modal_lines()
            .iter()
            .any(|line| line.contains("old failure") || line.contains("Prompt 90"))
    );
    app.handle_worker_message(WorkerMessage::LocalSessionLifecycleFailed {
        request_id: current_request,
        error: "stale source".to_owned(),
    })?;
    assert!(
        app.modal_lines()
            .iter()
            .any(|line| line.contains("stale source"))
    );
    assert_eq!(app.composer.input, "draft");
    assert!(!app.conversation_fork_applying());
    Ok(())
}

#[test]
fn conversation_fork_shortcut_keeps_word_navigation_while_editing() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.active_pane = PaneFocus::Composer;
    app.set_input_and_cursor("keep this draft".to_owned());
    assert!(
        app.handle_key_event(KeyEvent::new(KeyCode::Char('b'), KeyModifiers::ALT))?
            .is_none()
    );
    assert!(!app.conversation_fork_modal_open());
    assert!(app.composer.input_cursor < "keep this draft".chars().count());
    app.set_input_and_cursor(String::new());
    assert!(matches!(
        app.handle_key_event(KeyEvent::new(KeyCode::Char('b'), KeyModifiers::ALT))?,
        Some(AppAction::LoadConversationForkPoints { .. })
    ));
    Ok(())
}

use super::*;
use crate::{app::ComposerQueueAction, runner::QueueMoveDirection};

fn task_run_entry(status: sigil_kernel::TaskRunStatus) -> Result<SessionLogEntry> {
    Ok(SessionLogEntry::Control(ControlEntry::TaskRun(
        sigil_kernel::TaskRunEntry {
            task_id: sigil_kernel::TaskId::new("task_1")?,
            parent_session_ref: sigil_kernel::SessionRef::new_relative("parent.jsonl")?,
            objective: "review workspace".to_owned(),
            status,
            reason: None,
        },
    )))
}

fn queued_conversation_input_entry(id: &str, prompt: &str) -> Result<SessionLogEntry> {
    Ok(SessionLogEntry::Control(
        ControlEntry::ConversationInputQueued(sigil_kernel::ConversationInputQueuedEntry {
            queue_id: sigil_kernel::ConversationInputQueueId::new(id)?,
            target: sigil_kernel::ConversationInputTarget::MainThread,
            kind: sigil_kernel::ConversationInputKind::Chat,
            prompt_hash: format!("sha256:{id}"),
            prompt: prompt.to_owned(),
            reasoning_effort: Some(ReasoningEffort::Max),
            created_at_ms: None,
        }),
    ))
}

fn sync_child_agent(app: &mut AppState) -> Result<()> {
    let task_id = sigil_kernel::TaskId::new("task_1")?;
    let step_id = sigil_kernel::TaskStepId::new("step_1")?;
    app.sync_current_session_state(vec![
        SessionLogEntry::Control(ControlEntry::TaskRun(sigil_kernel::TaskRunEntry {
            task_id: task_id.clone(),
            parent_session_ref: sigil_kernel::SessionRef::new_relative("parent.jsonl")?,
            objective: "review workspace".to_owned(),
            status: sigil_kernel::TaskRunStatus::Running,
            reason: None,
        })),
        SessionLogEntry::Control(ControlEntry::TaskPlan(sigil_kernel::TaskPlanEntry {
            task_id: task_id.clone(),
            plan_version: 1,
            status: sigil_kernel::TaskPlanStatus::Accepted,
            steps: vec![sigil_kernel::TaskStepSpec {
                step_id: step_id.clone(),
                title: "让子 agent 检查仓库".to_owned(),
                display_name: Some("仓库审查".to_owned()),
                detail: None,
                role: sigil_kernel::AgentRole::SubagentRead,
                depends_on: Vec::new(),
                mode: None,
                isolation: None,
            }],
            reason: None,
        })),
        SessionLogEntry::Control(ControlEntry::TaskChildSession(
            sigil_kernel::TaskChildSessionEntry {
                task_id,
                plan_version: 1,
                step_id,
                child_task_id: sigil_kernel::TaskId::new("child_1")?,
                child_session_ref: sigil_kernel::SessionRef::new_relative(
                    "children/task_1/step_1-child_1.jsonl",
                )?,
                role: sigil_kernel::AgentRole::SubagentRead,
                status: sigil_kernel::TaskChildSessionStatus::Completed,
                summary_hash: None,
            },
        )),
    ]);
    Ok(())
}

#[test]
fn cjk_input_cursor_visual_position_uses_display_width() {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.set_terminal_size(40, 12);
    app.set_input_and_cursor("你好".to_owned());

    assert_eq!(app.input_cursor_visual_position(), (4, 0));
}

#[test]
fn bootstrap_creates_scratch_dir() {
    let temp = tempfile::tempdir().expect("workspace tempdir should create");
    let mut config = test_config();
    config.workspace.root = temp.path().display().to_string();

    let app = AppState::from_root_config(Path::new("sigil.toml"), &config);

    assert!(app.sigil_paths.scratch_root.is_dir());
    assert!(
        app.events
            .iter()
            .any(|event| { event.label == "scratch" && event.detail == "cache/tmp" })
    );
}

#[test]
fn bootstrap_reports_scratch_dir_creation_failure() {
    let temp = tempfile::tempdir().expect("workspace tempdir should create");
    let mut config = test_config();
    config.workspace.root = temp.path().display().to_string();

    let scratch_root =
        sigil_runtime::resolve_sigil_paths(&config.storage, &config.session, temp.path())
            .scratch_root;
    let scratch_parent = scratch_root
        .parent()
        .expect("scratch root should have a parent directory");
    std::fs::create_dir_all(scratch_parent).expect("scratch parent should create");
    std::fs::write(&scratch_root, "block").expect("scratch blocker file should write");

    let app = AppState::from_root_config(Path::new("sigil.toml"), &config);

    assert!(
        app.events.iter().any(|event| {
            event.label == "scratch" && event.detail.starts_with("failed to create")
        })
    );
}

#[test]
fn shift_enter_inserts_newline_without_submitting() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.composer.input = "hello".to_owned();
    app.composer.input_cursor = app.composer.input.chars().count();
    let timeline_len = app.timeline.len();

    let action = app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::SHIFT))?;

    assert!(action.is_none());
    assert_eq!(app.composer.input, "hello\n");
    assert_eq!(app.timeline.len(), timeline_len);
    assert_eq!(app.composer_input_rows(), 2);
    Ok(())
}

#[test]
fn shifted_line_feed_key_inserts_newline_without_submitting() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.composer.input = "hello".to_owned();
    app.composer.input_cursor = app.composer.input.chars().count();
    let timeline_len = app.timeline.len();

    let action = app.handle_key_event(KeyEvent::new(KeyCode::Char('\n'), KeyModifiers::SHIFT))?;

    assert!(action.is_none());
    assert_eq!(app.composer.input, "hello\n");
    assert_eq!(app.timeline.len(), timeline_len);
    assert_eq!(app.composer_input_rows(), 2);
    Ok(())
}

#[test]
fn shifted_carriage_return_key_normalizes_to_newline() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.composer.input = "hello".to_owned();
    app.composer.input_cursor = app.composer.input.chars().count();

    let action = app.handle_key_event(KeyEvent::new(KeyCode::Char('\r'), KeyModifiers::SHIFT))?;

    assert!(action.is_none());
    assert_eq!(app.composer.input, "hello\n");
    assert_eq!(app.composer_input_rows(), 2);
    Ok(())
}

#[test]
fn composer_ignores_non_printing_control_characters() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    let action =
        app.handle_key_event(KeyEvent::new(KeyCode::Char('\u{1b}'), KeyModifiers::NONE))?;

    assert!(action.is_none());
    assert!(app.composer.input.is_empty());
    assert_eq!(app.input_cursor_visual_position(), (0, 0));
    Ok(())
}

#[test]
fn carriage_return_key_submits_instead_of_entering_invisible_text() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.composer.input = "hello".to_owned();
    app.composer.input_cursor = app.composer.input.chars().count();

    let action = app.handle_key_event(KeyEvent::new(KeyCode::Char('\r'), KeyModifiers::NONE))?;

    assert!(matches!(
        action,
        Some(AppAction::SubmitPrompt(prompt)) if prompt == "hello"
    ));
    Ok(())
}

#[test]
fn composer_readline_line_and_character_shortcuts_move_cursor() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.set_input_and_cursor("first line\nsecond line".to_owned());
    app.composer.input_cursor = "first line\nsecond".chars().count();

    app.handle_key_event(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::CONTROL))?;
    assert_eq!(app.composer.input_cursor, "first line\n".chars().count());

    app.handle_key_event(KeyEvent::new(KeyCode::Char('e'), KeyModifiers::CONTROL))?;
    assert_eq!(app.composer.input_cursor, app.input_char_len());

    app.handle_key_event(KeyEvent::new(KeyCode::Char('b'), KeyModifiers::CONTROL))?;
    assert_eq!(app.composer.input_cursor, app.input_char_len() - 1);

    app.handle_key_event(KeyEvent::new(KeyCode::Char('f'), KeyModifiers::CONTROL))?;
    assert_eq!(app.composer.input_cursor, app.input_char_len());
    Ok(())
}

#[test]
fn composer_delete_shortcuts_handle_characters_and_unicode() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.set_input_and_cursor("a你b".to_owned());
    app.composer.input_cursor = 1;

    app.handle_key_event(KeyEvent::new(KeyCode::Delete, KeyModifiers::NONE))?;

    assert_eq!(app.composer.input, "ab");
    assert_eq!(app.composer.input_cursor, 1);

    app.handle_key_event(KeyEvent::new(KeyCode::Char('h'), KeyModifiers::CONTROL))?;

    assert_eq!(app.composer.input, "b");
    assert_eq!(app.composer.input_cursor, 0);
    Ok(())
}

#[test]
fn composer_word_shortcuts_move_delete_and_yank() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.set_input_and_cursor("alpha beta gamma".to_owned());

    app.handle_key_event(KeyEvent::new(KeyCode::Char('b'), KeyModifiers::ALT))?;
    assert_eq!(app.composer.input_cursor, "alpha beta ".chars().count());

    app.handle_key_event(KeyEvent::new(KeyCode::Char('f'), KeyModifiers::ALT))?;
    assert_eq!(app.composer.input_cursor, app.input_char_len());

    app.handle_key_event(KeyEvent::new(KeyCode::Char('w'), KeyModifiers::CONTROL))?;
    assert_eq!(app.composer.input, "alpha beta ");
    assert_eq!(app.composer.input_cursor, "alpha beta ".chars().count());

    app.handle_key_event(KeyEvent::new(KeyCode::Char('y'), KeyModifiers::CONTROL))?;
    assert_eq!(app.composer.input, "alpha beta gamma");
    assert_eq!(app.composer.input_cursor, app.input_char_len());

    app.composer.input_cursor = 0;
    app.handle_key_event(KeyEvent::new(KeyCode::Right, KeyModifiers::CONTROL))?;
    assert_eq!(app.composer.input_cursor, "alpha".chars().count());

    app.handle_key_event(KeyEvent::new(KeyCode::Right, KeyModifiers::ALT))?;
    assert_eq!(app.composer.input_cursor, "alpha beta".chars().count());

    app.handle_key_event(KeyEvent::new(KeyCode::Backspace, KeyModifiers::CONTROL))?;
    assert_eq!(app.composer.input, "alpha  gamma");
    assert_eq!(app.composer.input_cursor, "alpha ".chars().count());

    app.handle_key_event(KeyEvent::new(KeyCode::Delete, KeyModifiers::ALT))?;
    assert_eq!(app.composer.input, "alpha ");
    assert_eq!(app.composer.input_cursor, "alpha ".chars().count());
    Ok(())
}

#[test]
fn composer_ctrl_k_kills_to_line_end_and_ctrl_y_yanks() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.set_input_and_cursor("one\ntwo\nthree".to_owned());
    app.composer.input_cursor = "one\n".chars().count();

    app.handle_key_event(KeyEvent::new(KeyCode::Char('k'), KeyModifiers::CONTROL))?;
    assert_eq!(app.composer.input, "one\n\nthree");
    assert_eq!(app.composer.input_cursor, "one\n".chars().count());

    app.handle_key_event(KeyEvent::new(KeyCode::Char('y'), KeyModifiers::CONTROL))?;
    assert_eq!(app.composer.input, "one\ntwo\nthree");
    assert_eq!(app.composer.input_cursor, "one\ntwo".chars().count());

    app.handle_key_event(KeyEvent::new(KeyCode::Char('k'), KeyModifiers::CONTROL))?;
    assert_eq!(app.composer.input, "one\ntwothree");

    app.handle_key_event(KeyEvent::new(KeyCode::Char('k'), KeyModifiers::CONTROL))?;
    assert_eq!(app.composer.input, "one\ntwo");
    Ok(())
}

#[test]
fn composer_ctrl_j_and_alt_enter_insert_newlines() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.set_input_and_cursor("hello".to_owned());
    let timeline_len = app.timeline.len();

    app.handle_key_event(KeyEvent::new(KeyCode::Char('j'), KeyModifiers::CONTROL))?;
    app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::ALT))?;

    assert_eq!(app.composer.input, "hello\n\n");
    assert_eq!(app.timeline.len(), timeline_len);
    assert_eq!(app.composer_input_rows(), 3);
    Ok(())
}

#[test]
fn composer_paste_inserts_multiline_text_without_submitting() {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.set_input_and_cursor("prefix ".to_owned());
    let timeline_len = app.timeline.len();

    app.handle_paste_text("one\r\ntwo\rthree");

    assert_eq!(app.composer.input, "prefix one\ntwo\nthree");
    assert_eq!(app.composer.input_cursor, app.input_char_len());
    assert_eq!(app.timeline.len(), timeline_len);
    assert_eq!(app.composer_input_rows(), 3);
}

#[test]
fn large_composer_paste_collapses_display_but_submits_full_text() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.set_terminal_size(180, 24);
    let pasted = format!("{}\n{}", "x".repeat(10_000), "tail");

    app.handle_paste_text(&pasted);

    let view_model = crate::view_model::UiViewModel::from_app(&app);
    assert_eq!(app.composer.input, pasted);
    assert!(view_model.composer.input.contains("[Pasted text #1:"));
    assert!(!view_model.composer.input.contains(&"x".repeat(80)));
    assert!(view_model.composer.input_rows < 3);

    let action = app.submit_input()?;

    assert!(matches!(
        action,
        Some(AppAction::SubmitPrompt(prompt)) if prompt == pasted
    ));
    Ok(())
}

#[test]
fn editing_after_large_paste_restores_full_display_mapping() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    let pasted = "x".repeat(10_000);
    app.handle_paste_text(&pasted);
    assert!(
        crate::view_model::UiViewModel::from_app(&app)
            .composer
            .input
            .contains("[Pasted text #1:")
    );

    app.handle_key_event(KeyEvent::new(KeyCode::Left, KeyModifiers::NONE))?;

    assert_eq!(
        crate::view_model::UiViewModel::from_app(&app)
            .composer
            .input,
        pasted
    );
    Ok(())
}

#[test]
fn modal_paste_does_not_submit_text_input() {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.open_text_input(
        super::super::modal_flow::TextInputTarget::SkillArguments,
        "pre",
    );

    app.handle_paste_text(" one\ntwo");

    let Some(ModalState::TextInput(state)) = &app.modal_state else {
        panic!("text input modal should stay open after paste");
    };
    assert_eq!(state.buffer, "pre onetwo");
}

#[test]
fn setup_field_paste_updates_selected_text_without_saving() {
    let mut app = AppState::from_setup(
        Path::new("sigil.toml").to_path_buf(),
        Path::new(".").to_path_buf(),
        None,
    );
    app.setup_state
        .as_mut()
        .expect("setup state exists")
        .selected_field = SetupField::ApiKey;

    app.handle_paste_text("sk-test\n");

    let state = app.setup_state.as_ref().expect("setup state remains open");
    assert_eq!(state.api_key, "sk-test");
    assert!(!matches!(app.last_notice(), Some(notice) if notice.contains("saved config")));
}

#[test]
fn config_field_paste_updates_selected_text_without_submitting() {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.open_config_panel();
    app.config_state
        .as_mut()
        .expect("config state exists")
        .selected_field = Some(ConfigField::ProviderModel);

    app.handle_paste_text("custom\nmodel");

    let state = app
        .config_state
        .as_ref()
        .expect("config state remains open");
    assert_eq!(state.draft.provider_model, "custommodel");
    assert!(state.dirty);
    assert_eq!(app.last_notice(), Some("updated model"));

    app.config_state
        .as_mut()
        .expect("config state exists")
        .selected_field = Some(ConfigField::ProviderApiKey);
    app.handle_paste_text("sk-test\nwith-control\u{0007}");
    let state = app
        .config_state
        .as_ref()
        .expect("config state remains open");
    assert_eq!(state.draft.provider_api_key, "sk-testwith-control");
    assert_eq!(app.last_notice(), Some("updated api_key"));

    app.config_state
        .as_mut()
        .expect("config state exists")
        .footer_selected = true;
    app.handle_paste_text("ignored");
    assert_eq!(app.last_notice(), Some("updated api_key"));

    app.config_state
        .as_mut()
        .expect("config state exists")
        .footer_selected = false;
    app.config_state
        .as_mut()
        .expect("config state exists")
        .selected_field = None;
    app.handle_paste_text("ignored");
    assert_eq!(app.last_notice(), Some("updated api_key"));
}

#[test]
fn paste_empty_pending_and_collapsed_cursor_edges_are_noops_or_bounded() {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.handle_paste_text("");
    assert_eq!(app.composer.input, "");

    inject_write_file_approval(&mut app, sample_approval_preview())
        .expect("approval should inject");
    app.handle_paste_text("ignored");
    assert_eq!(app.composer.input, "");
    app.approval.pending = None;

    let pasted = "x".repeat(10_000);
    app.handle_paste_text(&pasted);
    app.composer.input_cursor = 1;
    let display = app.composer_display_input();
    assert!(display.contains("[Pasted text #1:"));
    assert!(app.input_cursor_visual_position().0 > 0);

    app.composer.input_paste_spans[0].end = pasted.len() + 1;
    let display = app.composer_display_input();
    assert_eq!(display, pasted);
}

#[test]
fn composer_ctrl_z_restores_last_esc_cleared_draft_once() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.set_input_and_cursor("draft text".to_owned());

    app.handle_key_event(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE))?;

    assert!(app.composer.input.is_empty());
    assert_eq!(app.composer.input_cursor, 0);

    app.handle_key_event(KeyEvent::new(KeyCode::Char('z'), KeyModifiers::CONTROL))?;

    assert_eq!(app.composer.input, "draft text");
    assert_eq!(app.composer.input_cursor, app.input_char_len());
    assert_eq!(app.last_notice(), Some("draft restored"));

    app.handle_key_event(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE))?;
    app.handle_key_event(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE))?;
    app.composer.input.clear();
    app.composer.input_cursor = 0;
    app.handle_key_event(KeyEvent::new(KeyCode::Char('z'), KeyModifiers::CONTROL))?;

    assert!(app.composer.input.is_empty());
    Ok(())
}

#[test]
fn composer_input_edges_ignore_empty_and_boundary_operations() {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());

    app.insert_input_text("");
    app.remove_input_character_before_cursor();
    app.remove_input_character_at_cursor();
    app.remove_input_word_before_cursor();
    app.remove_input_word_after_cursor();
    app.kill_input_to_line_end();
    app.yank_input_kill_buffer();
    assert!(app.composer.input.is_empty());
    assert_eq!(app.composer.input_cursor, 0);

    app.set_input_and_cursor("alpha".to_owned());
    app.restore_cleared_input_draft();
    assert_eq!(app.composer.input, "alpha");
    app.composer.input_cursor = 0;
    app.remove_input_word_before_cursor();
    assert_eq!(app.composer.input, "alpha");
    app.composer.input_cursor = app.input_char_len();
    app.remove_input_word_after_cursor();
    assert_eq!(app.composer.input, "alpha");
}

#[test]
fn input_history_persists_and_loads_when_test_flag_is_enabled() -> Result<()> {
    struct EnvGuard;

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            unsafe {
                std::env::remove_var("SIGIL_TUI_TEST_PERSIST_INPUT_HISTORY");
            }
            let _ = std::fs::remove_file(".sigil/input-history.jsonl");
            let _ = std::fs::remove_file("crates/sigil-tui/.sigil/input-history.jsonl");
        }
    }

    let _ = std::fs::remove_file(".sigil/input-history.jsonl");
    let _ = std::fs::remove_file("crates/sigil-tui/.sigil/input-history.jsonl");
    unsafe {
        std::env::set_var("SIGIL_TUI_TEST_PERSIST_INPUT_HISTORY", "1");
    }
    let _guard = EnvGuard;
    let temp = tempdir()?;
    let mut config = test_config();
    config.workspace.root = temp.path().display().to_string();
    config.session.log_dir = Some(".sigil/sessions".to_owned());

    let mut app = AppState::from_root_config(&temp.path().join("sigil.toml"), &config);
    app.record_input_history("first prompt".to_owned());
    app.record_input_history("second prompt".to_owned());

    let mut restored = AppState::from_root_config(&temp.path().join("sigil.toml"), &config);
    restored.load_input_history();

    assert_eq!(
        restored.composer.input_history,
        vec!["first prompt".to_owned(), "second prompt".to_owned()]
    );
    Ok(())
}

#[test]
fn composer_alt_modified_non_ascii_text_still_inserts() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());

    app.handle_key_event(KeyEvent::new(KeyCode::Char('æ'), KeyModifiers::ALT))?;

    assert_eq!(app.composer.input, "æ");
    assert_eq!(app.composer.input_cursor, 1);
    Ok(())
}

#[test]
fn plain_prompt_after_final_task_starts_new_conversation() -> Result<()> {
    for status in [
        sigil_kernel::TaskRunStatus::Completed,
        sigil_kernel::TaskRunStatus::Cancelled,
    ] {
        let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
        app.sync_current_session_state(vec![task_run_entry(status)?]);
        app.composer.input = "new question".to_owned();
        app.composer.input_cursor = app.composer.input.chars().count();

        let action = app.submit_input()?;

        assert!(matches!(
            action,
            Some(AppAction::SubmitPrompt(prompt)) if prompt == "new question"
        ));
        assert_eq!(app.last_notice(), Some("thinking"));
    }
    Ok(())
}

#[test]
fn busy_plain_prompt_queues_without_persisting_user_timeline() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.runtime.is_busy = true;
    app.composer.input = "follow up after this finishes".to_owned();
    app.composer.input_cursor = app.composer.input.chars().count();

    let action = app.submit_input()?;

    assert!(matches!(
        action,
        Some(AppAction::QueueConversationInput {
            prompt,
            kind: sigil_kernel::ConversationInputKind::Chat,
            target: sigil_kernel::ConversationInputTarget::MainThread,
        }) if prompt == "follow up after this finishes"
    ));
    assert!(app.composer.input.is_empty());
    assert_eq!(app.last_notice(), Some("queued for next turn"));
    assert!(
        !app.timeline
            .iter()
            .any(|entry| entry.role == TimelineRole::User
                && entry.text == "follow up after this finishes")
    );
    Ok(())
}

#[test]
fn composer_down_focuses_queue_panel_and_enter_runs_visible_queue_action() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.sync_current_session_state(vec![
        queued_conversation_input_entry("queue_1", "first queued prompt")?,
        queued_conversation_input_entry("queue_2", "second queued prompt")?,
    ]);

    let focus_action = app.handle_key_event(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE))?;

    assert!(focus_action.is_none());
    assert!(app.is_composer_queue_panel_focused());
    assert!(app.composer_queue_rows()[0].selected);

    let move_action = app.handle_key_event(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE))?;
    assert!(move_action.is_none());
    assert!(app.composer_queue_rows()[1].selected);

    let action = app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))?;
    assert!(matches!(
        action,
        Some(AppAction::SendQueuedConversationInputNow { ref queue_id })
            if queue_id.as_str() == "queue_2"
    ));
    assert_eq!(app.last_notice(), Some("queued input sending now"));

    let tab = app.handle_key_event(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE))?;
    assert!(tab.is_none());
    let keep_next = app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))?;
    assert!(matches!(
        keep_next,
        Some(AppAction::PromoteQueuedConversationInput { ref queue_id })
            if queue_id.as_str() == "queue_2"
    ));
    assert_eq!(app.last_notice(), Some("queued input moved to next turn"));
    Ok(())
}

#[test]
fn queue_panel_keyboard_actions_cover_navigation_reorder_and_adjacent_focus() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.sync_current_session_state(vec![
        queued_conversation_input_entry("queue_1", "first queued prompt")?,
        queued_conversation_input_entry("queue_2", "second queued prompt")?,
    ]);

    app.handle_key_event(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE))?;
    assert!(app.is_composer_queue_panel_focused());
    assert_eq!(
        app.selected_composer_queue_action(),
        ComposerQueueAction::SendNow
    );

    app.handle_key_event(KeyEvent::new(KeyCode::BackTab, KeyModifiers::NONE))?;
    assert_eq!(
        app.selected_composer_queue_action(),
        ComposerQueueAction::Delete
    );
    app.handle_key_event(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE))?;
    assert_eq!(
        app.selected_composer_queue_action(),
        ComposerQueueAction::SendNow
    );
    app.handle_key_event(KeyEvent::new(KeyCode::Left, KeyModifiers::NONE))?;
    assert_eq!(
        app.selected_composer_queue_action(),
        ComposerQueueAction::Delete
    );

    let move_first_up = app.handle_key_event(KeyEvent::new(KeyCode::Up, KeyModifiers::ALT))?;
    assert!(matches!(
        move_first_up,
        Some(AppAction::MoveQueuedConversationInput {
            ref queue_id,
            direction: QueueMoveDirection::Up,
        }) if queue_id.as_str() == "queue_1"
    ));
    let move_first_down = app.handle_key_event(KeyEvent::new(KeyCode::Down, KeyModifiers::ALT))?;
    assert!(matches!(
        move_first_down,
        Some(AppAction::MoveQueuedConversationInput {
            ref queue_id,
            direction: QueueMoveDirection::Down,
        }) if queue_id.as_str() == "queue_1"
    ));

    app.handle_key_event(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE))?;
    let move_last_down = app.handle_key_event(KeyEvent::new(KeyCode::Down, KeyModifiers::ALT))?;
    assert!(matches!(
        move_last_down,
        Some(AppAction::MoveQueuedConversationInput {
            ref queue_id,
            direction: QueueMoveDirection::Down,
        }) if queue_id.as_str() == "queue_2"
    ));
    let move_last_up = app.handle_key_event(KeyEvent::new(KeyCode::Up, KeyModifiers::ALT))?;
    assert!(matches!(
        move_last_up,
        Some(AppAction::MoveQueuedConversationInput {
            ref queue_id,
            direction: QueueMoveDirection::Up,
        }) if queue_id.as_str() == "queue_2"
    ));
    app.handle_key_event(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE))?;
    assert!(app.composer_queue_rows()[0].selected);
    let delete = app.handle_key_event(KeyEvent::new(KeyCode::Delete, KeyModifiers::NONE))?;
    assert!(matches!(
        delete,
        Some(AppAction::CancelQueuedConversationInput { ref queue_id })
            if queue_id.as_str() == "queue_1"
    ));
    app.handle_key_event(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE))?;
    assert!(!app.is_composer_queue_panel_focused());

    let mut blur_app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    blur_app.sync_current_session_state(vec![queued_conversation_input_entry(
        "queue_1",
        "first queued prompt",
    )?]);
    blur_app.handle_key_event(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE))?;
    blur_app.handle_key_event(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE))?;
    assert!(!blur_app.is_composer_queue_panel_focused());

    let mut adjacent_app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    adjacent_app.sync_current_session_state(vec![queued_conversation_input_entry(
        "queue_1",
        "first queued prompt",
    )?]);
    sync_child_agent(&mut adjacent_app)?;
    adjacent_app.handle_key_event(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE))?;
    adjacent_app.handle_key_event(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE))?;
    assert!(!adjacent_app.is_composer_queue_panel_focused());
    assert!(adjacent_app.is_composer_agent_panel_focused());
    Ok(())
}

#[test]
fn queue_flow_empty_and_direct_actions_cover_boundaries() -> Result<()> {
    let mut empty_app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    assert_eq!(empty_app.queue_strip_rows(), 0);
    empty_app.composer.queue_panel_focused = true;
    assert!(!empty_app.move_composer_queue_selection(true));
    assert!(!empty_app.is_composer_queue_panel_focused());
    assert!(empty_app.execute_queue_slash_command("")?.is_none());
    assert_eq!(empty_app.last_notice(), Some("queue empty"));
    assert!(empty_app.execute_queue_slash_command("edit 1")?.is_none());
    assert_eq!(empty_app.last_notice(), Some("queue item not found"));
    assert!(!empty_app.begin_edit_selected_queue_item());

    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.sync_current_session_state(vec![
        queued_conversation_input_entry("queue_1", "\nfirst queued prompt")?,
        queued_conversation_input_entry("queue_2", "second queued prompt")?,
        queued_conversation_input_entry("queue_3", "third queued prompt")?,
    ]);
    assert_eq!(app.queue_strip_rows(), 5);
    assert!(app.focus_composer_queue_panel());
    app.composer.queue_selected = 1;
    assert!(app.move_composer_queue_selection(false));
    assert_eq!(app.composer.queue_selected, 0);

    app.composer.queue_action_selected = ComposerQueueAction::Edit;
    assert!(app.execute_selected_queue_action().is_none());
    assert_eq!(
        app.composer
            .queue_edit_target
            .as_ref()
            .map(|id| id.as_str()),
        Some("queue_1")
    );
    assert_eq!(app.composer.input, "\nfirst queued prompt");
    assert!(app.cancel_queue_edit());

    app.composer.queue_panel_focused = true;
    app.composer.queue_action_selected = ComposerQueueAction::Delete;
    let delete = app.execute_selected_queue_action();
    assert!(matches!(
        delete,
        Some(AppAction::CancelQueuedConversationInput { ref queue_id })
            if queue_id.as_str() == "queue_1"
    ));

    app.composer.queue_edit_target = Some(sigil_kernel::ConversationInputQueueId::new("missing")?);
    app.refresh_conversation_queue_selection();
    assert!(app.composer.queue_edit_target.is_none());
    Ok(())
}

#[test]
fn queue_slash_commands_map_to_explicit_queue_actions() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.sync_current_session_state(vec![
        queued_conversation_input_entry("queue_1", "first queued prompt")?,
        queued_conversation_input_entry("queue_2", "second queued prompt")?,
    ]);

    app.composer.input = "/queue pause".to_owned();
    app.composer.input_cursor = app.composer.input.chars().count();
    let pause = app.submit_input()?;
    assert!(matches!(
        pause,
        Some(AppAction::SetConversationQueuePaused { paused: true })
    ));

    app.composer.input = "/queue show".to_owned();
    app.composer.input_cursor = app.composer.input.chars().count();
    assert!(app.submit_input()?.is_none());
    assert!(app.is_composer_queue_panel_focused());

    app.composer.input = "/queue next 2".to_owned();
    app.composer.input_cursor = app.composer.input.chars().count();
    let next = app.submit_input()?;
    assert!(matches!(
        next,
        Some(AppAction::PromoteQueuedConversationInput { ref queue_id })
            if queue_id.as_str() == "queue_2"
    ));

    app.composer.input = "/queue now 2".to_owned();
    app.composer.input_cursor = app.composer.input.chars().count();
    let now = app.submit_input()?;
    assert!(matches!(
        now,
        Some(AppAction::SendQueuedConversationInputNow { ref queue_id })
            if queue_id.as_str() == "queue_2"
    ));

    app.composer.input = "/queue delete second".to_owned();
    app.composer.input_cursor = app.composer.input.chars().count();
    let delete = app.submit_input()?;
    assert!(matches!(
        delete,
        Some(AppAction::CancelQueuedConversationInput { ref queue_id })
            if queue_id.as_str() == "queue_2"
    ));

    app.composer.input = "/queue edit 2".to_owned();
    app.composer.input_cursor = app.composer.input.chars().count();
    assert!(app.submit_input()?.is_none());
    assert_eq!(app.composer.input, "second queued prompt");

    app.composer.input = "updated queued prompt".to_owned();
    app.composer.input_cursor = app.composer.input.chars().count();
    let edit = app.submit_input()?;
    assert!(matches!(
        edit,
        Some(AppAction::EditQueuedConversationInput { ref queue_id, ref prompt })
            if queue_id.as_str() == "queue_2" && prompt == "updated queued prompt"
    ));

    app.sync_current_session_state(vec![
        queued_conversation_input_entry("queue_1", "first queued prompt")?,
        queued_conversation_input_entry("queue_2", "second queued prompt")?,
    ]);
    for command in [
        "/queue resume",
        "/queue next",
        "/queue send 1",
        "/queue send-now 1",
    ] {
        app.composer.input = command.to_owned();
        app.composer.input_cursor = app.composer.input.chars().count();
        assert!(app.submit_input()?.is_some());
    }
    for command in [
        "/queue cancel 2",
        "/queue remove 2",
        "/queue up 2",
        "/queue down 2",
    ] {
        app.composer.input = command.to_owned();
        app.composer.input_cursor = app.composer.input.chars().count();
        assert!(app.submit_input()?.is_some());
    }
    app.composer.input = "/queue now missing".to_owned();
    app.composer.input_cursor = app.composer.input.chars().count();
    assert!(app.submit_input()?.is_none());
    assert_eq!(app.last_notice(), Some("queue item not found"));
    app.composer.input = "/queue nonsense".to_owned();
    app.composer.input_cursor = app.composer.input.chars().count();
    assert!(app.submit_input()?.is_none());
    assert_eq!(
        app.last_notice(),
        Some("usage: /queue <show|next|now|edit|delete>")
    );
    Ok(())
}

#[test]
fn queue_edit_escape_cancels_without_submitting() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.sync_current_session_state(vec![queued_conversation_input_entry(
        "queue_1",
        "queued prompt",
    )?]);
    app.composer.input = "/queue edit 1".to_owned();
    app.composer.input_cursor = app.composer.input.chars().count();
    assert!(app.submit_input()?.is_none());
    assert_eq!(
        app.composer
            .queue_edit_target
            .as_ref()
            .map(|id| id.as_str()),
        Some("queue_1")
    );

    let action = app.handle_key_event(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE))?;

    assert!(action.is_none());
    assert!(app.composer.queue_edit_target.is_none());
    assert!(app.composer.input.is_empty());
    assert_eq!(app.active_pane, PaneFocus::Composer);
    assert_eq!(app.last_notice(), Some("queue edit cancelled"));
    Ok(())
}

#[test]
fn agent_message_command_reports_unavailable_child_view_without_thread_id() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.active_agent_view = super::super::AgentView::Child {
        child_task_id: "orphan_child".to_owned(),
        child_session_ref: sigil_kernel::SessionRef::new_relative("children/orphan.jsonl")?,
    };
    app.composer.input = "/agent message current hello".to_owned();
    app.composer.input_cursor = app.composer.input.chars().count();

    let action = app.submit_input()?;

    assert!(action.is_none());
    assert_eq!(
        app.last_notice(),
        Some("agent message unavailable: current")
    );
    Ok(())
}

#[test]
fn composer_agent_panel_missing_selection_rejects_message_and_close() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    sync_child_agent(&mut app)?;
    app.composer.agent_panel_focused = true;
    app.sidebar_agent_selected = usize::MAX;

    let close = app.handle_key_event(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::NONE))?;
    assert!(close.is_none());
    assert_eq!(app.last_notice(), Some("no agent selected"));

    let message = app.handle_key_event(KeyEvent::new(KeyCode::Char('m'), KeyModifiers::NONE))?;
    assert!(message.is_none());
    assert_eq!(app.last_notice(), Some("no agent selected"));
    Ok(())
}

#[test]
fn agent_message_command_rejects_empty_prompt() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    sync_child_agent(&mut app)?;
    app.composer.input = "/agent message child_1    ".to_owned();
    app.composer.input_cursor = app.composer.input.chars().count();

    let action = app.submit_input()?;

    assert!(action.is_none());
    assert_eq!(
        app.last_notice(),
        Some("usage: /agent message <agent|current> <prompt>")
    );
    Ok(())
}

#[test]
fn queue_slash_selector_exposes_next_turn_language() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.sync_current_session_state(vec![queued_conversation_input_entry(
        "queue_1",
        "first queued prompt",
    )?]);
    app.composer.input = "/queue ".to_owned();
    app.composer.input_cursor = app.composer.input.chars().count();

    let rows = app.slash_selector_rows();

    let labels = rows.iter().map(|row| row.0.as_str()).collect::<Vec<_>>();
    assert_eq!(labels, vec!["show", "next", "now", "edit", "delete"]);
    assert!(
        rows.iter()
            .any(|row| row.1 == "run selected after current turn")
    );
    assert!(
        rows.iter()
            .any(|row| row.1 == "interrupt current turn and run selected")
    );
    Ok(())
}

#[test]
fn plain_prompt_with_unfinished_task_starts_new_chat() -> Result<()> {
    for status in [
        sigil_kernel::TaskRunStatus::Started,
        sigil_kernel::TaskRunStatus::Running,
        sigil_kernel::TaskRunStatus::Paused,
        sigil_kernel::TaskRunStatus::Failed,
        sigil_kernel::TaskRunStatus::Interrupted,
    ] {
        let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
        app.sync_current_session_state(vec![task_run_entry(status)?]);
        app.composer.input = "continue with the review".to_owned();
        app.composer.input_cursor = app.composer.input.chars().count();

        let action = app.submit_input()?;

        assert!(matches!(
            action,
            Some(AppAction::SubmitPrompt(prompt)) if prompt == "continue with the review"
        ));
        assert_eq!(app.last_notice(), Some("thinking"));
    }
    Ok(())
}

#[test]
fn composer_up_down_navigates_history_when_input_is_empty_without_scrolling() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.set_terminal_size(80, 12);
    for index in 0..8 {
        app.push_timeline(TimelineRole::Assistant, format!("message {index}"));
    }
    app.composer.input_history = vec!["first".to_owned(), "second".to_owned()];
    app.composer.input.clear();
    app.composer.input_cursor = 0;

    app.handle_key_event(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE))?;
    assert_eq!(app.composer.input, "second");
    assert_eq!(app.timeline_scroll_back, 0);

    app.handle_key_event(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE))?;
    assert!(app.composer.input.is_empty());
    assert_eq!(app.timeline_scroll_back, 0);
    Ok(())
}

#[test]
fn composer_up_down_without_history_do_not_scroll_transcript() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.set_terminal_size(80, 12);
    for index in 0..8 {
        app.push_timeline(TimelineRole::Assistant, format!("message {index}"));
    }
    app.composer.input.clear();
    app.composer.input_cursor = 0;

    app.handle_key_event(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE))?;
    app.handle_key_event(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE))?;

    assert!(app.composer.input.is_empty());
    assert_eq!(app.timeline_scroll_back, 0);
    Ok(())
}

#[test]
fn ctrl_p_and_ctrl_n_navigate_prompt_history() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.composer.input_history = vec!["first".to_owned(), "second".to_owned()];
    app.composer.input.clear();
    app.composer.input_cursor = 0;

    app.handle_key_event(KeyEvent::new(KeyCode::Char('p'), KeyModifiers::CONTROL))?;
    assert_eq!(app.composer.input, "second");

    app.handle_key_event(KeyEvent::new(KeyCode::Char('n'), KeyModifiers::CONTROL))?;
    assert!(app.composer.input.is_empty());
    Ok(())
}

#[test]
fn composer_up_down_navigates_input_history() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.composer.input = "first".to_owned();
    assert!(matches!(
        app.submit_input()?,
        Some(AppAction::SubmitPrompt(prompt)) if prompt == "first"
    ));
    app.runtime.is_busy = false;

    app.composer.input = "second".to_owned();
    assert!(matches!(
        app.submit_input()?,
        Some(AppAction::SubmitPrompt(prompt)) if prompt == "second"
    ));
    app.runtime.is_busy = false;

    app.composer.input = "draft".to_owned();
    app.active_pane = PaneFocus::Composer;
    app.handle_key_event(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE))?;
    assert_eq!(app.composer.input, "second");
    app.handle_key_event(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE))?;
    assert_eq!(app.composer.input, "first");
    app.handle_key_event(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE))?;
    assert_eq!(app.composer.input, "second");
    app.handle_key_event(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE))?;
    assert_eq!(app.composer.input, "draft");
    Ok(())
}

#[test]
fn composer_history_navigation_continues_past_slash_entries() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.composer.input_history = vec![
        "earlier prompt".to_owned(),
        "/quit".to_owned(),
        "latest prompt".to_owned(),
    ];
    app.active_pane = PaneFocus::Composer;
    app.set_input_and_cursor(String::new());

    app.handle_key_event(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE))?;
    assert_eq!(app.composer.input, "latest prompt");

    app.handle_key_event(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE))?;
    assert_eq!(app.composer.input, "/quit");

    app.handle_key_event(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE))?;
    assert_eq!(app.composer.input, "earlier prompt");

    app.handle_key_event(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE))?;
    assert_eq!(app.composer.input, "/quit");

    app.handle_key_event(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE))?;
    assert_eq!(app.composer.input, "latest prompt");
    Ok(())
}

#[test]
fn input_history_does_not_record_session_control_commands() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());

    app.composer.input = "/quit".to_owned();
    assert!(app.submit_input()?.is_none());
    app.should_quit = false;

    app.composer.input = "/new".to_owned();
    assert!(matches!(
        app.submit_input()?,
        Some(AppAction::StartNewSession { .. })
    ));

    app.composer.input = "normal prompt".to_owned();
    assert!(matches!(
        app.submit_input()?,
        Some(AppAction::SubmitPrompt(prompt)) if prompt == "normal prompt"
    ));

    assert_eq!(app.composer.input_history, vec!["normal prompt".to_owned()]);
    Ok(())
}

#[test]
fn composer_up_inside_wrapped_input_moves_cursor_before_history() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.composer.input = "first".to_owned();
    assert!(matches!(
        app.submit_input()?,
        Some(AppAction::SubmitPrompt(prompt)) if prompt == "first"
    ));
    app.runtime.is_busy = false;

    app.active_pane = PaneFocus::Composer;
    app.set_terminal_size(96, 20);
    app.composer.input = "draft".repeat(20);
    app.composer.input_cursor = 70;
    assert!(app.input_cursor_visual_position().1 > 0);

    app.handle_key_event(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE))?;

    assert_eq!(app.composer.input, "draft".repeat(20));
    assert_eq!(app.input_cursor_visual_position().1, 0);
    assert_eq!(app.composer.input_history_index, None);
    Ok(())
}

#[test]
fn composer_down_at_bottom_row_navigates_history() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.composer.input = "first".to_owned();
    assert!(matches!(
        app.submit_input()?,
        Some(AppAction::SubmitPrompt(prompt)) if prompt == "first"
    ));
    app.runtime.is_busy = false;

    app.composer.input = "second".to_owned();
    assert!(matches!(
        app.submit_input()?,
        Some(AppAction::SubmitPrompt(prompt)) if prompt == "second"
    ));
    app.runtime.is_busy = false;

    app.active_pane = PaneFocus::Composer;
    app.set_terminal_size(6, 20);
    app.composer.input = "draft123".to_owned();
    app.composer.input_cursor = 1;

    app.handle_key_event(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE))?;
    assert_eq!(app.composer.input, "second");
    app.composer.input_cursor = app.composer.input.chars().count();

    app.handle_key_event(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE))?;
    assert_eq!(app.composer.input, "draft123");
    Ok(())
}

#[test]
fn composer_down_prefers_history_navigation_before_agent_panel_focus() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    sync_child_agent(&mut app)?;
    app.active_pane = PaneFocus::Composer;
    app.composer.input_history = vec!["first".to_owned(), "second".to_owned()];
    app.set_input_and_cursor("draft".to_owned());

    app.handle_key_event(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE))?;
    assert_eq!(app.composer.input, "second");
    assert!(!app.is_composer_agent_panel_focused());

    app.handle_key_event(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE))?;
    assert_eq!(app.composer.input, "draft");
    assert!(!app.is_composer_agent_panel_focused());

    app.handle_key_event(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE))?;
    assert!(app.is_composer_agent_panel_focused());
    Ok(())
}

#[test]
fn composer_down_focuses_agent_panel_and_enter_switches_agent() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    sync_child_agent(&mut app)?;
    app.active_pane = PaneFocus::Composer;
    app.composer.input.clear();
    app.composer.input_cursor = 0;

    app.handle_key_event(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE))?;

    assert!(app.is_composer_agent_panel_focused());
    assert_eq!(app.sidebar_agent_selected, 0);
    let view_model = crate::view_model::UiViewModel::from_app(&app);
    assert!(view_model.footer.hints.contains("Enter switch"));
    assert!(view_model.composer.agent_panel_focused);

    app.handle_key_event(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE))?;
    assert_eq!(app.sidebar_agent_selected, 1);

    app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))?;

    assert_eq!(app.active_agent_label(), "仓库审查");
    assert!(app.is_composer_agent_panel_focused());
    Ok(())
}

#[test]
fn composer_agent_panel_message_key_prefills_agent_message_command() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    sync_child_agent(&mut app)?;
    app.active_pane = PaneFocus::Composer;
    app.composer.input.clear();
    app.composer.input_cursor = 0;

    app.handle_key_event(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE))?;
    app.handle_key_event(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE))?;
    assert_eq!(app.sidebar_agent_selected, 1);

    let action = app.handle_key_event(KeyEvent::new(KeyCode::Char('m'), KeyModifiers::NONE))?;

    assert!(action.is_none());
    assert_eq!(app.composer.input, "/agent message child_1 ");
    assert_eq!(
        app.composer.input_cursor,
        app.composer.input.chars().count()
    );
    assert!(!app.is_composer_agent_panel_focused());
    assert_eq!(app.last_notice(), Some("compose agent message: child_1"));
    Ok(())
}

#[test]
fn composer_agent_panel_main_row_rejects_close_and_message_actions() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    sync_child_agent(&mut app)?;
    app.active_pane = PaneFocus::Composer;
    app.composer.input.clear();
    app.composer.input_cursor = 0;

    app.handle_key_event(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE))?;
    assert_eq!(app.sidebar_agent_selected, 0);

    let close = app.handle_key_event(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::NONE))?;
    assert!(close.is_none());
    assert_eq!(app.last_notice(), Some("agent close unavailable for main"));

    let message = app.handle_key_event(KeyEvent::new(KeyCode::Char('m'), KeyModifiers::NONE))?;
    assert!(message.is_none());
    assert_eq!(
        app.last_notice(),
        Some("agent message unavailable for main")
    );
    assert!(app.composer.input.is_empty());
    assert!(app.is_composer_agent_panel_focused());
    Ok(())
}

#[test]
fn composer_agent_panel_close_key_requests_selected_terminal_agent_close() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    sync_child_agent(&mut app)?;
    app.active_pane = PaneFocus::Composer;
    app.composer.input.clear();
    app.composer.input_cursor = 0;

    app.handle_key_event(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE))?;
    app.handle_key_event(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE))?;

    let action = app.handle_key_event(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::NONE))?;

    assert!(matches!(
        action,
        Some(AppAction::CloseAgent {
            ref thread_id,
            reason: Some(ref reason),
            ..
        }) if thread_id.as_str() == "legacy_task_1_v1_step_1_child_1"
            && reason == "closed from TUI /agent"
    ));
    assert_eq!(
        app.last_notice(),
        Some("agent close requested: legacy_task_1_v1_step_1_child_1")
    );
    assert!(app.is_composer_agent_panel_focused());
    Ok(())
}

#[test]
fn composer_agent_panel_down_wraps_from_last_agent_to_first() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    sync_child_agent(&mut app)?;
    app.active_pane = PaneFocus::Composer;

    app.handle_key_event(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE))?;
    app.handle_key_event(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE))?;
    assert_eq!(app.sidebar_agent_selected, 1);

    app.handle_key_event(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE))?;

    assert_eq!(app.sidebar_agent_selected, 0);
    assert!(app.is_composer_agent_panel_focused());
    Ok(())
}

#[test]
fn composer_down_moves_wrapped_input_before_agent_panel_focus() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    sync_child_agent(&mut app)?;
    app.active_pane = PaneFocus::Composer;
    app.set_terminal_size(96, 20);
    app.set_input_and_cursor("draft".repeat(20));
    app.composer.input_cursor = 0;

    app.handle_key_event(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE))?;

    assert!(app.input_cursor_visual_position().1 > 0);
    assert!(!app.is_composer_agent_panel_focused());
    Ok(())
}

#[test]
fn composer_agent_panel_up_and_escape_return_to_input() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    sync_child_agent(&mut app)?;
    app.handle_key_event(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE))?;
    assert!(app.is_composer_agent_panel_focused());

    app.handle_key_event(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE))?;
    assert!(!app.is_composer_agent_panel_focused());

    app.handle_key_event(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE))?;
    app.handle_key_event(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE))?;
    assert!(!app.is_composer_agent_panel_focused());
    assert!(app.composer.input.is_empty());
    Ok(())
}

#[test]
fn busy_submit_keeps_existing_input_and_emits_notice() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.runtime.is_busy = true;
    app.composer.input = "queued".to_owned();

    let action = app.submit_input()?;

    assert!(matches!(
        action,
        Some(AppAction::QueueConversationInput {
            prompt,
            kind: sigil_kernel::ConversationInputKind::Chat,
            target: sigil_kernel::ConversationInputTarget::MainThread,
        }) if prompt == "queued"
    ));
    assert!(app.composer.input.is_empty());
    assert!(
        app.timeline
            .iter()
            .any(|entry| entry.role == TimelineRole::Notice && entry.text == "queued for next turn")
    );
    assert!(
        app.events
            .iter()
            .any(|event| event.label == "queue" && event.detail == "queued busy input queued")
    );
    Ok(())
}

#[test]
fn input_history_is_capped_at_one_hundred_entries() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());

    for index in 0..101 {
        app.composer.input = format!("prompt {index}");
        assert!(matches!(
            app.submit_input()?,
            Some(AppAction::SubmitPrompt(prompt)) if prompt == format!("prompt {index}")
        ));
        app.runtime.is_busy = false;
    }

    assert_eq!(app.composer.input_history.len(), 100);
    assert_eq!(
        app.composer.input_history.first().map(String::as_str),
        Some("prompt 1")
    );
    assert_eq!(
        app.composer.input_history.last().map(String::as_str),
        Some("prompt 100")
    );
    Ok(())
}

#[test]
fn input_helpers_edit_and_navigate_multiline_text() {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.set_terminal_size(80, 12);
    app.set_input_and_cursor("ab\ncd".to_owned());

    assert_eq!(app.input_char_len(), 5);
    assert_eq!(app.composer_input_rows(), 2);
    assert_eq!(app.composer_height(), 6);
    assert_eq!(app.visual_position_for_cursor(5, 4), (1, 2));
    assert_eq!(app.cursor_for_visual_position(1, 1, 4), 4);

    app.composer.input_cursor = usize::MAX;
    app.clamp_input_cursor();
    assert_eq!(app.composer.input_cursor, 5);

    app.move_input_cursor_home();
    assert_eq!(app.composer.input_cursor, 0);
    assert!(!app.move_input_cursor_vertical(true));

    app.remove_input_character_before_cursor();
    assert_eq!(app.composer.input, "ab\ncd");

    app.move_input_cursor_right();
    app.insert_input_character('X');
    assert_eq!(app.composer.input, "aXb\ncd");
    assert_eq!(app.composer.input_cursor, 2);

    app.remove_input_character_before_cursor();
    assert_eq!(app.composer.input, "ab\ncd");

    app.move_input_cursor_end();
    assert_eq!(app.input_cursor_visual_row(), 1);
    assert!(app.move_input_cursor_vertical(true));
    assert_eq!(app.composer.input_cursor, 2);
    assert!(app.move_input_cursor_vertical(false));
    assert_eq!(app.composer.input_cursor, 5);
    assert!(!app.move_input_cursor_vertical(false));

    app.move_input_cursor_left();
    app.move_input_cursor_left();
    assert_eq!(app.composer.input_cursor, 3);
    app.move_input_cursor_home();
    app.move_input_cursor_left();
    assert_eq!(app.composer.input_cursor, 0);
    app.move_input_cursor_end();
    app.move_input_cursor_right();
    assert_eq!(app.composer.input_cursor, app.input_char_len());
}

#[test]
fn input_history_recording_deduplicates_caps_and_restores_draft() {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    for index in 0..=100 {
        app.record_input_history(format!("prompt-{index}"));
    }
    assert_eq!(app.composer.input_history.len(), 100);
    assert_eq!(
        app.composer.input_history.first().map(String::as_str),
        Some("prompt-1")
    );

    app.record_input_history("prompt-100".to_owned());
    assert_eq!(app.composer.input_history.len(), 100);

    app.composer.input = "draft".to_owned();
    app.navigate_input_history(true);
    assert_eq!(app.composer.input, "prompt-100");

    for _ in 0..200 {
        app.navigate_input_history(true);
    }
    assert_eq!(app.composer.input, "prompt-1");
    assert_eq!(app.composer.input_history_index, Some(0));

    app.navigate_input_history(true);
    assert_eq!(app.composer.input, "prompt-1");

    for _ in 0..200 {
        app.navigate_input_history(false);
    }
    assert_eq!(app.composer.input, "draft");
    assert_eq!(app.composer.input_history_index, None);
    assert_eq!(app.composer.input_history_draft, None);
}

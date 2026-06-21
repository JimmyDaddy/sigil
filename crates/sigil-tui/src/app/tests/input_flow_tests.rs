use super::*;

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
fn shift_enter_inserts_newline_without_submitting() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.input = "hello".to_owned();
    app.input_cursor = app.input.chars().count();
    let timeline_len = app.timeline.len();

    let action = app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::SHIFT))?;

    assert!(action.is_none());
    assert_eq!(app.input, "hello\n");
    assert_eq!(app.timeline.len(), timeline_len);
    assert_eq!(app.composer_input_rows(), 2);
    Ok(())
}

#[test]
fn shifted_line_feed_key_inserts_newline_without_submitting() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.input = "hello".to_owned();
    app.input_cursor = app.input.chars().count();
    let timeline_len = app.timeline.len();

    let action = app.handle_key_event(KeyEvent::new(KeyCode::Char('\n'), KeyModifiers::SHIFT))?;

    assert!(action.is_none());
    assert_eq!(app.input, "hello\n");
    assert_eq!(app.timeline.len(), timeline_len);
    assert_eq!(app.composer_input_rows(), 2);
    Ok(())
}

#[test]
fn shifted_carriage_return_key_normalizes_to_newline() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.input = "hello".to_owned();
    app.input_cursor = app.input.chars().count();

    let action = app.handle_key_event(KeyEvent::new(KeyCode::Char('\r'), KeyModifiers::SHIFT))?;

    assert!(action.is_none());
    assert_eq!(app.input, "hello\n");
    assert_eq!(app.composer_input_rows(), 2);
    Ok(())
}

#[test]
fn composer_ignores_non_printing_control_characters() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    let action =
        app.handle_key_event(KeyEvent::new(KeyCode::Char('\u{1b}'), KeyModifiers::NONE))?;

    assert!(action.is_none());
    assert!(app.input.is_empty());
    assert_eq!(app.input_cursor_visual_position(), (0, 0));
    Ok(())
}

#[test]
fn carriage_return_key_submits_instead_of_entering_invisible_text() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.input = "hello".to_owned();
    app.input_cursor = app.input.chars().count();

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
    app.input_cursor = "first line\nsecond".chars().count();

    app.handle_key_event(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::CONTROL))?;
    assert_eq!(app.input_cursor, "first line\n".chars().count());

    app.handle_key_event(KeyEvent::new(KeyCode::Char('e'), KeyModifiers::CONTROL))?;
    assert_eq!(app.input_cursor, app.input_char_len());

    app.handle_key_event(KeyEvent::new(KeyCode::Char('b'), KeyModifiers::CONTROL))?;
    assert_eq!(app.input_cursor, app.input_char_len() - 1);

    app.handle_key_event(KeyEvent::new(KeyCode::Char('f'), KeyModifiers::CONTROL))?;
    assert_eq!(app.input_cursor, app.input_char_len());
    Ok(())
}

#[test]
fn composer_delete_shortcuts_handle_characters_and_unicode() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.set_input_and_cursor("a你b".to_owned());
    app.input_cursor = 1;

    app.handle_key_event(KeyEvent::new(KeyCode::Delete, KeyModifiers::NONE))?;

    assert_eq!(app.input, "ab");
    assert_eq!(app.input_cursor, 1);

    app.handle_key_event(KeyEvent::new(KeyCode::Char('h'), KeyModifiers::CONTROL))?;

    assert_eq!(app.input, "b");
    assert_eq!(app.input_cursor, 0);
    Ok(())
}

#[test]
fn composer_word_shortcuts_move_delete_and_yank() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.set_input_and_cursor("alpha beta gamma".to_owned());

    app.handle_key_event(KeyEvent::new(KeyCode::Char('b'), KeyModifiers::ALT))?;
    assert_eq!(app.input_cursor, "alpha beta ".chars().count());

    app.handle_key_event(KeyEvent::new(KeyCode::Char('f'), KeyModifiers::ALT))?;
    assert_eq!(app.input_cursor, app.input_char_len());

    app.handle_key_event(KeyEvent::new(KeyCode::Char('w'), KeyModifiers::CONTROL))?;
    assert_eq!(app.input, "alpha beta ");
    assert_eq!(app.input_cursor, "alpha beta ".chars().count());

    app.handle_key_event(KeyEvent::new(KeyCode::Char('y'), KeyModifiers::CONTROL))?;
    assert_eq!(app.input, "alpha beta gamma");
    assert_eq!(app.input_cursor, app.input_char_len());

    app.input_cursor = 0;
    app.handle_key_event(KeyEvent::new(KeyCode::Right, KeyModifiers::CONTROL))?;
    assert_eq!(app.input_cursor, "alpha".chars().count());

    app.handle_key_event(KeyEvent::new(KeyCode::Right, KeyModifiers::ALT))?;
    assert_eq!(app.input_cursor, "alpha beta".chars().count());

    app.handle_key_event(KeyEvent::new(KeyCode::Backspace, KeyModifiers::CONTROL))?;
    assert_eq!(app.input, "alpha  gamma");
    assert_eq!(app.input_cursor, "alpha ".chars().count());

    app.handle_key_event(KeyEvent::new(KeyCode::Delete, KeyModifiers::ALT))?;
    assert_eq!(app.input, "alpha ");
    assert_eq!(app.input_cursor, "alpha ".chars().count());
    Ok(())
}

#[test]
fn composer_ctrl_k_kills_to_line_end_and_ctrl_y_yanks() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.set_input_and_cursor("one\ntwo\nthree".to_owned());
    app.input_cursor = "one\n".chars().count();

    app.handle_key_event(KeyEvent::new(KeyCode::Char('k'), KeyModifiers::CONTROL))?;
    assert_eq!(app.input, "one\n\nthree");
    assert_eq!(app.input_cursor, "one\n".chars().count());

    app.handle_key_event(KeyEvent::new(KeyCode::Char('y'), KeyModifiers::CONTROL))?;
    assert_eq!(app.input, "one\ntwo\nthree");
    assert_eq!(app.input_cursor, "one\ntwo".chars().count());

    app.handle_key_event(KeyEvent::new(KeyCode::Char('k'), KeyModifiers::CONTROL))?;
    assert_eq!(app.input, "one\ntwothree");

    app.handle_key_event(KeyEvent::new(KeyCode::Char('k'), KeyModifiers::CONTROL))?;
    assert_eq!(app.input, "one\ntwo");
    Ok(())
}

#[test]
fn composer_ctrl_j_and_alt_enter_insert_newlines() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.set_input_and_cursor("hello".to_owned());
    let timeline_len = app.timeline.len();

    app.handle_key_event(KeyEvent::new(KeyCode::Char('j'), KeyModifiers::CONTROL))?;
    app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::ALT))?;

    assert_eq!(app.input, "hello\n\n");
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

    assert_eq!(app.input, "prefix one\ntwo\nthree");
    assert_eq!(app.input_cursor, app.input_char_len());
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
    assert_eq!(app.input, pasted);
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
    assert_eq!(app.input, "");

    inject_write_file_approval(&mut app, sample_approval_preview())
        .expect("approval should inject");
    app.handle_paste_text("ignored");
    assert_eq!(app.input, "");
    app.pending_approval = None;

    let pasted = "x".repeat(10_000);
    app.handle_paste_text(&pasted);
    app.input_cursor = 1;
    let display = app.composer_display_input();
    assert!(display.contains("[Pasted text #1:"));
    assert!(app.input_cursor_visual_position().0 > 0);

    app.input_paste_spans[0].end = pasted.len() + 1;
    let display = app.composer_display_input();
    assert_eq!(display, pasted);
}

#[test]
fn composer_ctrl_z_restores_last_esc_cleared_draft_once() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.set_input_and_cursor("draft text".to_owned());

    app.handle_key_event(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE))?;

    assert!(app.input.is_empty());
    assert_eq!(app.input_cursor, 0);

    app.handle_key_event(KeyEvent::new(KeyCode::Char('z'), KeyModifiers::CONTROL))?;

    assert_eq!(app.input, "draft text");
    assert_eq!(app.input_cursor, app.input_char_len());
    assert_eq!(app.last_notice(), Some("draft restored"));

    app.handle_key_event(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE))?;
    app.handle_key_event(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE))?;
    app.input.clear();
    app.input_cursor = 0;
    app.handle_key_event(KeyEvent::new(KeyCode::Char('z'), KeyModifiers::CONTROL))?;

    assert!(app.input.is_empty());
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
    assert!(app.input.is_empty());
    assert_eq!(app.input_cursor, 0);

    app.set_input_and_cursor("alpha".to_owned());
    app.restore_cleared_input_draft();
    assert_eq!(app.input, "alpha");
    app.input_cursor = 0;
    app.remove_input_word_before_cursor();
    assert_eq!(app.input, "alpha");
    app.input_cursor = app.input_char_len();
    app.remove_input_word_after_cursor();
    assert_eq!(app.input, "alpha");
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
    config.session.log_dir = ".sigil/sessions".to_owned();

    let mut app = AppState::from_root_config(&temp.path().join("sigil.toml"), &config);
    app.record_input_history("first prompt".to_owned());
    app.record_input_history("second prompt".to_owned());

    let mut restored = AppState::from_root_config(&temp.path().join("sigil.toml"), &config);
    restored.load_input_history();

    assert_eq!(
        restored.input_history,
        vec!["first prompt".to_owned(), "second prompt".to_owned()]
    );
    Ok(())
}

#[test]
fn composer_alt_modified_non_ascii_text_still_inserts() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());

    app.handle_key_event(KeyEvent::new(KeyCode::Char('æ'), KeyModifiers::ALT))?;

    assert_eq!(app.input, "æ");
    assert_eq!(app.input_cursor, 1);
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
        app.input = "new question".to_owned();
        app.input_cursor = app.input.chars().count();

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
        app.input = "continue with the review".to_owned();
        app.input_cursor = app.input.chars().count();

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
    app.input_history = vec!["first".to_owned(), "second".to_owned()];
    app.input.clear();
    app.input_cursor = 0;

    app.handle_key_event(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE))?;
    assert_eq!(app.input, "second");
    assert_eq!(app.timeline_scroll_back, 0);

    app.handle_key_event(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE))?;
    assert!(app.input.is_empty());
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
    app.input.clear();
    app.input_cursor = 0;

    app.handle_key_event(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE))?;
    app.handle_key_event(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE))?;

    assert!(app.input.is_empty());
    assert_eq!(app.timeline_scroll_back, 0);
    Ok(())
}

#[test]
fn ctrl_p_and_ctrl_n_navigate_prompt_history() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.input_history = vec!["first".to_owned(), "second".to_owned()];
    app.input.clear();
    app.input_cursor = 0;

    app.handle_key_event(KeyEvent::new(KeyCode::Char('p'), KeyModifiers::CONTROL))?;
    assert_eq!(app.input, "second");

    app.handle_key_event(KeyEvent::new(KeyCode::Char('n'), KeyModifiers::CONTROL))?;
    assert!(app.input.is_empty());
    Ok(())
}

#[test]
fn composer_up_down_navigates_input_history() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.input = "first".to_owned();
    assert!(matches!(
        app.submit_input()?,
        Some(AppAction::SubmitPrompt(prompt)) if prompt == "first"
    ));
    app.is_busy = false;

    app.input = "second".to_owned();
    assert!(matches!(
        app.submit_input()?,
        Some(AppAction::SubmitPrompt(prompt)) if prompt == "second"
    ));
    app.is_busy = false;

    app.input = "draft".to_owned();
    app.active_pane = PaneFocus::Composer;
    app.handle_key_event(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE))?;
    assert_eq!(app.input, "second");
    app.handle_key_event(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE))?;
    assert_eq!(app.input, "first");
    app.handle_key_event(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE))?;
    assert_eq!(app.input, "second");
    app.handle_key_event(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE))?;
    assert_eq!(app.input, "draft");
    Ok(())
}

#[test]
fn composer_history_navigation_continues_past_slash_entries() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.input_history = vec![
        "earlier prompt".to_owned(),
        "/quit".to_owned(),
        "latest prompt".to_owned(),
    ];
    app.active_pane = PaneFocus::Composer;
    app.set_input_and_cursor(String::new());

    app.handle_key_event(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE))?;
    assert_eq!(app.input, "latest prompt");

    app.handle_key_event(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE))?;
    assert_eq!(app.input, "/quit");

    app.handle_key_event(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE))?;
    assert_eq!(app.input, "earlier prompt");

    app.handle_key_event(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE))?;
    assert_eq!(app.input, "/quit");

    app.handle_key_event(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE))?;
    assert_eq!(app.input, "latest prompt");
    Ok(())
}

#[test]
fn input_history_does_not_record_session_control_commands() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());

    app.input = "/quit".to_owned();
    assert!(app.submit_input()?.is_none());
    app.should_quit = false;

    app.input = "/new".to_owned();
    assert!(matches!(
        app.submit_input()?,
        Some(AppAction::StartNewSession { .. })
    ));

    app.input = "normal prompt".to_owned();
    assert!(matches!(
        app.submit_input()?,
        Some(AppAction::SubmitPrompt(prompt)) if prompt == "normal prompt"
    ));

    assert_eq!(app.input_history, vec!["normal prompt".to_owned()]);
    Ok(())
}

#[test]
fn composer_up_inside_wrapped_input_moves_cursor_before_history() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.input = "first".to_owned();
    assert!(matches!(
        app.submit_input()?,
        Some(AppAction::SubmitPrompt(prompt)) if prompt == "first"
    ));
    app.is_busy = false;

    app.active_pane = PaneFocus::Composer;
    app.set_terminal_size(96, 20);
    app.input = "draft".repeat(20);
    app.input_cursor = 70;
    assert!(app.input_cursor_visual_position().1 > 0);

    app.handle_key_event(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE))?;

    assert_eq!(app.input, "draft".repeat(20));
    assert_eq!(app.input_cursor_visual_position().1, 0);
    assert_eq!(app.input_history_index, None);
    Ok(())
}

#[test]
fn composer_down_at_bottom_row_navigates_history() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.input = "first".to_owned();
    assert!(matches!(
        app.submit_input()?,
        Some(AppAction::SubmitPrompt(prompt)) if prompt == "first"
    ));
    app.is_busy = false;

    app.input = "second".to_owned();
    assert!(matches!(
        app.submit_input()?,
        Some(AppAction::SubmitPrompt(prompt)) if prompt == "second"
    ));
    app.is_busy = false;

    app.active_pane = PaneFocus::Composer;
    app.set_terminal_size(6, 20);
    app.input = "draft123".to_owned();
    app.input_cursor = 1;

    app.handle_key_event(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE))?;
    assert_eq!(app.input, "second");
    app.input_cursor = app.input.chars().count();

    app.handle_key_event(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE))?;
    assert_eq!(app.input, "draft123");
    Ok(())
}

#[test]
fn composer_down_prefers_history_navigation_before_agent_panel_focus() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    sync_child_agent(&mut app)?;
    app.active_pane = PaneFocus::Composer;
    app.input_history = vec!["first".to_owned(), "second".to_owned()];
    app.set_input_and_cursor("draft".to_owned());

    app.handle_key_event(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE))?;
    assert_eq!(app.input, "second");
    assert!(!app.is_composer_agent_panel_focused());

    app.handle_key_event(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE))?;
    assert_eq!(app.input, "draft");
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
    app.input.clear();
    app.input_cursor = 0;

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
    assert!(!app.is_composer_agent_panel_focused());
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
    app.input_cursor = 0;

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
    assert!(app.input.is_empty());
    Ok(())
}

#[test]
fn busy_submit_keeps_existing_input_and_emits_notice() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.is_busy = true;
    app.input = "queued".to_owned();

    let action = app.submit_input()?;

    assert!(action.is_none());
    assert_eq!(app.input, "queued");
    assert!(
        app.timeline
            .iter()
            .any(|entry| entry.role == TimelineRole::Notice && entry.text == "busy; submit later")
    );
    assert!(
        app.events
            .iter()
            .any(|event| event.label == "notice" && event.detail == "submit ignored while busy")
    );
    Ok(())
}

#[test]
fn input_history_is_capped_at_one_hundred_entries() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());

    for index in 0..101 {
        app.input = format!("prompt {index}");
        assert!(matches!(
            app.submit_input()?,
            Some(AppAction::SubmitPrompt(prompt)) if prompt == format!("prompt {index}")
        ));
        app.is_busy = false;
    }

    assert_eq!(app.input_history.len(), 100);
    assert_eq!(
        app.input_history.first().map(String::as_str),
        Some("prompt 1")
    );
    assert_eq!(
        app.input_history.last().map(String::as_str),
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

    app.input_cursor = usize::MAX;
    app.clamp_input_cursor();
    assert_eq!(app.input_cursor, 5);

    app.move_input_cursor_home();
    assert_eq!(app.input_cursor, 0);
    assert!(!app.move_input_cursor_vertical(true));

    app.remove_input_character_before_cursor();
    assert_eq!(app.input, "ab\ncd");

    app.move_input_cursor_right();
    app.insert_input_character('X');
    assert_eq!(app.input, "aXb\ncd");
    assert_eq!(app.input_cursor, 2);

    app.remove_input_character_before_cursor();
    assert_eq!(app.input, "ab\ncd");

    app.move_input_cursor_end();
    assert_eq!(app.input_cursor_visual_row(), 1);
    assert!(app.move_input_cursor_vertical(true));
    assert_eq!(app.input_cursor, 2);
    assert!(app.move_input_cursor_vertical(false));
    assert_eq!(app.input_cursor, 5);
    assert!(!app.move_input_cursor_vertical(false));

    app.move_input_cursor_left();
    app.move_input_cursor_left();
    assert_eq!(app.input_cursor, 3);
    app.move_input_cursor_home();
    app.move_input_cursor_left();
    assert_eq!(app.input_cursor, 0);
    app.move_input_cursor_end();
    app.move_input_cursor_right();
    assert_eq!(app.input_cursor, app.input_char_len());
}

#[test]
fn input_history_recording_deduplicates_caps_and_restores_draft() {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    for index in 0..=100 {
        app.record_input_history(format!("prompt-{index}"));
    }
    assert_eq!(app.input_history.len(), 100);
    assert_eq!(
        app.input_history.first().map(String::as_str),
        Some("prompt-1")
    );

    app.record_input_history("prompt-100".to_owned());
    assert_eq!(app.input_history.len(), 100);

    app.input = "draft".to_owned();
    app.navigate_input_history(true);
    assert_eq!(app.input, "prompt-100");

    for _ in 0..200 {
        app.navigate_input_history(true);
    }
    assert_eq!(app.input, "prompt-1");
    assert_eq!(app.input_history_index, Some(0));

    app.navigate_input_history(true);
    assert_eq!(app.input, "prompt-1");

    for _ in 0..200 {
        app.navigate_input_history(false);
    }
    assert_eq!(app.input, "draft");
    assert_eq!(app.input_history_index, None);
    assert_eq!(app.input_history_draft, None);
}

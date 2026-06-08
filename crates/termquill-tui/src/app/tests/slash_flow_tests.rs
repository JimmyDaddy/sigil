use super::*;

#[test]
fn compact_command_dispatches_worker_action_when_idle() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("termquill.toml"), &test_config());
    app.input = "/compact".to_owned();

    let action = app.submit_input()?;

    assert!(matches!(action, Some(AppAction::CompactNow)));
    Ok(())
}

#[test]
fn compact_command_prefix_is_resolved_to_exact_command() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("termquill.toml"), &test_config());
    app.input = "/comp".to_owned();

    let action = app.submit_input()?;

    assert!(matches!(action, Some(AppAction::CompactNow)));
    Ok(())
}

#[test]
fn effort_command_updates_runtime_effort_and_worker_submit_uses_it() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("termquill.toml"), &test_config());
    app.input = "/effort high".to_owned();

    assert!(app.submit_input()?.is_none());
    assert_eq!(app.reasoning_effort.as_str(), "high");

    let command = app.into_worker_command(AppAction::SubmitPrompt("hello".to_owned()));
    assert!(matches!(
        command,
        WorkerCommand::SubmitPrompt {
            prompt,
            reasoning_effort: ReasoningEffort::High,
        } if prompt == "hello"
    ));
    Ok(())
}

#[test]
fn model_command_switches_runtime_model_and_starts_fresh_session() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("termquill.toml"), &test_config());
    let previous_session_id = app.session_id.clone();
    app.push_timeline(TimelineRole::Assistant, "old context");
    app.input = "/model pro".to_owned();

    let action = app.submit_input()?;

    assert!(matches!(
        action,
        Some(AppAction::RuntimeConfigUpdated { .. })
    ));
    assert_eq!(app.model_name, "deepseek-v4-pro");
    assert_ne!(app.session_id, previous_session_id);
    assert!(
        app.timeline
            .iter()
            .any(|entry| entry.text.contains("model -> deepseek-v4-pro"))
    );
    Ok(())
}

#[test]
fn slash_command_hints_include_prefix_matches() {
    let mut app = AppState::from_root_config(Path::new("termquill.toml"), &test_config());
    app.input = "/res".to_owned();
    let hints = app.slash_command_hints();
    assert!(hints.iter().any(|hint| hint.contains("/resume")));
}

#[test]
fn slash_command_hints_handles_leading_space() {
    let mut app = AppState::from_root_config(Path::new("termquill.toml"), &test_config());
    app.input = " /compact".to_owned();
    let hints = app.slash_command_hints();
    assert!(hints.iter().any(|hint| hint.contains("/compact")));
}

#[test]
fn slash_command_input_starts_in_activity_mode() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("termquill.toml"), &test_config());
    app.active_pane = PaneFocus::Activity;
    app.input.clear();

    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Char('/'), KeyModifiers::NONE))?;
    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::NONE))?;

    assert_eq!(app.active_pane, PaneFocus::Composer);
    assert_eq!(app.input, "/c".to_owned());
    assert!(
        app.slash_command_hints()
            .iter()
            .any(|hint| hint.contains("/compact"))
    );
    Ok(())
}

#[test]
fn ideographic_comma_starts_command_palette() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("termquill.toml"), &test_config());
    app.input.clear();

    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Char('、'), KeyModifiers::NONE))?;
    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::NONE))?;

    assert_eq!(app.input, "/c");
    assert!(
        app.slash_command_hints()
            .iter()
            .any(|hint| hint.contains("/compact"))
    );
    Ok(())
}

#[test]
fn slash_selector_shows_all_commands_for_root_slash() {
    let mut app = AppState::from_root_config(Path::new("termquill.toml"), &test_config());
    app.input = "/".to_owned();

    let rows = app.slash_selector_rows();

    assert_eq!(rows.len(), super::SLASH_COMMANDS.len());
    assert_eq!(app.slash_selector_selected_index(), Some(0));
}

#[test]
fn slash_selector_does_not_register_tool_commands() {
    let mut app = AppState::from_root_config(Path::new("termquill.toml"), &test_config());
    app.input = "/".to_owned();

    let rows = app.slash_selector_rows();
    assert!(!rows.iter().any(|(label, _)| label == "/tool"));
    assert!(!rows.iter().any(|(label, _)| label == "/tools"));

    app.input = "/tools".to_owned();
    assert!(app.slash_selector_rows().is_empty());
    assert_eq!(app.slash_selector_empty_message(), Some("no slash match"));

    app.input = "/tool".to_owned();
    assert!(app.slash_selector_rows().is_empty());
    assert_eq!(app.slash_selector_empty_message(), Some("no slash match"));

    assert!(app.resolve_slash_command("/tool latest").is_none());
    assert!(app.resolve_slash_command("/tools full").is_none());
}

#[test]
fn slash_selector_navigation_and_tab_completion_work() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("termquill.toml"), &test_config());
    app.input = "/".to_owned();

    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE))?;
    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE))?;

    assert_eq!(app.input, "/config".to_owned());
    assert_eq!(app.slash_selector_selected_index(), Some(0));
    Ok(())
}

#[test]
fn slash_selector_offers_model_candidates_and_completes_argument() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("termquill.toml"), &test_config());
    app.input = "/model p".to_owned();

    let rows = app.slash_selector_rows();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].0, "pro");

    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE))?;
    assert_eq!(app.input, "/model deepseek-v4-pro");
    Ok(())
}

#[test]
fn slash_selector_executes_selected_model_candidate() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("termquill.toml"), &test_config());
    let previous_session_id = app.session_id.clone();
    app.input = "/model p".to_owned();

    let action = app.submit_input()?;

    assert!(matches!(
        action,
        Some(AppAction::RuntimeConfigUpdated { .. })
    ));
    assert_eq!(app.model_name, "deepseek-v4-pro");
    assert_ne!(app.session_id, previous_session_id);
    Ok(())
}

#[test]
fn enter_on_root_slash_model_completes_into_second_stage_selector() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("termquill.toml"), &test_config());
    app.input = "/".to_owned();

    select_root_slash_command(&mut app, "/model")?;
    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))?;

    assert_eq!(app.input, "/model ");
    let rows = app.slash_selector_rows();
    assert!(rows.iter().any(|(label, _)| label == "flash"));
    assert!(rows.iter().any(|(label, _)| label == "pro"));
    Ok(())
}

#[test]
fn enter_on_root_slash_effort_completes_into_second_stage_selector() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("termquill.toml"), &test_config());
    app.input = "/".to_owned();

    select_root_slash_command(&mut app, "/effort")?;
    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))?;

    assert_eq!(app.input, "/effort ");
    let rows = app.slash_selector_rows();
    assert!(rows.iter().any(|(label, _)| label == "low"));
    assert!(rows.iter().any(|(label, _)| label == "max"));
    Ok(())
}

#[test]
fn model_command_is_noop_when_selected_model_is_already_active() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("termquill.toml"), &test_config());
    let previous_session_id = app.session_id.clone();
    app.input = "/model".to_owned();

    let action = app.submit_input()?;

    assert!(action.is_none());
    assert_eq!(app.model_name, "deepseek-v4-flash");
    assert_eq!(app.session_id, previous_session_id);
    assert_eq!(
        app.last_notice(),
        Some("model already active = deepseek-v4-flash")
    );
    Ok(())
}

#[test]
fn slash_selector_orders_effort_candidates_by_current_value() {
    let mut app = AppState::from_root_config(Path::new("termquill.toml"), &test_config());
    app.reasoning_effort = ReasoningEffort::High;
    app.input = "/effort".to_owned();

    let rows = app.slash_selector_rows();

    assert_eq!(rows.first().map(|row| row.0.as_str()), Some("high"));
}

#[test]
fn slash_selector_executes_selected_effort_candidate() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("termquill.toml"), &test_config());
    app.input = "/effort h".to_owned();

    assert!(app.submit_input()?.is_none());
    assert_eq!(app.reasoning_effort.as_str(), "high");
    Ok(())
}

#[test]
fn slash_selector_preserves_custom_model_ids() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("termquill.toml"), &test_config());
    app.input = "/model ds-custom".to_owned();

    let rows = app.slash_selector_rows();
    assert_eq!(rows.first().map(|row| row.0.as_str()), Some("custom"));

    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE))?;
    assert_eq!(app.input, "/model ds-custom");
    Ok(())
}

#[test]
fn slash_command_does_not_pollute_timeline_as_user_message() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("termquill.toml"), &test_config());
    app.input = "/config".to_owned();

    let action = app.submit_input()?;

    assert!(action.is_none());
    assert!(
        !app.timeline
            .iter()
            .any(|entry| entry.role == TimelineRole::User && entry.text == "/config")
    );
    assert!(
        app.events
            .iter()
            .any(|event| event.label == "slash" && event.detail == "/config")
    );
    Ok(())
}

#[test]
fn submit_root_slash_executes_selected_command() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("termquill.toml"), &test_config());
    app.input = "/".to_owned();

    let action = app.submit_input()?;

    assert!(matches!(action, Some(AppAction::CompactNow)));
    Ok(())
}

#[test]
fn unknown_slash_command_does_not_become_normal_prompt() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("termquill.toml"), &test_config());
    app.input = "/unknown".to_owned();

    let action = app.submit_input()?;

    assert!(action.is_none());
    assert!(!app.is_busy);
    assert!(
        app.timeline
            .iter()
            .any(|entry| entry.text.contains("unknown slash command"))
    );
    Ok(())
}

#[test]
fn exit_alias_quits_tui() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("termquill.toml"), &test_config());
    app.input = "/exit".to_owned();

    let action = app.submit_input()?;

    assert!(action.is_none());
    assert!(app.should_quit);
    assert!(
        app.timeline
            .iter()
            .any(|entry| entry.role == TimelineRole::Notice && entry.text == "quitting")
    );
    Ok(())
}

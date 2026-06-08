use super::*;

#[test]
fn latest_session_can_be_restored_on_launch() -> Result<()> {
    let temp = tempdir()?;
    let config = RootConfig {
        workspace: WorkspaceConfig {
            root: temp.path().display().to_string(),
        },
        ..test_config()
    };
    let session_dir = temp.path().join(".termquill/sessions");
    let restored_path = session_dir.join("session-restored.jsonl");
    write_session_log(
        &restored_path,
        &restored_entries("restored-provider", "restored-model"),
    )?;

    let mut app = AppState::from_root_config(temp.path().join("termquill.toml").as_path(), &config);

    assert!(app.restore_latest_session_from_disk(&config));
    assert_eq!(app.session_log_path, restored_path);
    assert_eq!(app.provider_name, "restored-provider");
    assert_eq!(app.model_name, "restored-model");
    assert!(
        app.timeline
            .iter()
            .any(|entry| entry.text == "restored assistant answer")
    );
    assert_eq!(app.last_notice(), Some("restored latest session"));
    Ok(())
}

#[test]
fn session_sidebar_lines_include_model_and_phase() {
    let mut app = AppState::from_root_config(Path::new("termquill.toml"), &test_config());
    app.run_phase = RunPhase::Thinking;

    let lines = app.session_sidebar_lines();

    assert!(lines.iter().any(|line| line == "provider: deepseek"));
    assert!(lines.iter().any(|line| line == "model: deepseek-v4-flash"));
    assert!(lines.iter().any(|line| line == "effort: max"));
    assert!(lines.iter().any(|line| line == "phase: thinking"));
}

#[test]
fn session_display_title_uses_first_user_prompt() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("termquill.toml"), &test_config());
    app.input = "Summarize the codebase architecture".to_owned();

    let action = app.submit_input()?;

    assert!(matches!(action, Some(AppAction::SubmitPrompt(_))));
    assert_eq!(
        app.session_display_title(),
        "Summarize the codebase architecture".to_owned()
    );
    Ok(())
}

#[test]
fn latest_user_prompt_preview_reflects_recent_submission() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("termquill.toml"), &test_config());
    app.input = "hello from user".to_owned();

    let action = app.submit_input()?;

    assert!(matches!(action, Some(AppAction::SubmitPrompt(_))));
    assert_eq!(
        app.latest_user_prompt_preview(),
        Some("hello from user".to_owned())
    );
    Ok(())
}

#[test]
fn restored_session_view_shows_compaction_block_and_restored_prompt_pressure() -> Result<()> {
    let mut config = test_config();
    config.compaction.context_window_tokens = Some(100);
    config.compaction.soft_threshold_ratio = 0.5;
    config.compaction.hard_threshold_ratio = 0.8;
    let mut app = AppState::from_root_config(Path::new("termquill.toml"), &config);
    let session_log_path = app.session_log_path.clone();
    let entries = vec![
        SessionLogEntry::Control(ControlEntry::SessionIdentity {
            provider_name: "deepseek".to_owned(),
            model_name: "deepseek-v4-flash".to_owned(),
        }),
        SessionLogEntry::Control(ControlEntry::UsageSnapshot(UsageStats {
            prompt_tokens: 65,
            completion_tokens: 8,
            cache_hit_tokens: 45,
            cache_miss_tokens: 20,
            input_cost: 0.0,
            output_cost: 0.0,
            cache_savings: 0.0,
            system_fingerprint: None,
        })),
        SessionLogEntry::Control(ControlEntry::CompactionApplied(CompactionRecord {
            summary: "Compacted 2 earlier messages into a stable local summary.\n01. user hello\n02. assistant world".to_owned(),
            compacted_message_count: 2,
            retained_tail_message_count: 3,
        })),
        SessionLogEntry::User(ModelMessage::user("latest prompt")),
    ];

    app.handle_worker_message(WorkerMessage::SessionSwitched {
        session_log_path,
        provider_name: "deepseek".to_owned(),
        model_name: "deepseek-v4-flash".to_owned(),
        entries,
    })?;

    let lines = app.approval_preview_lines();
    assert_eq!(app.compaction_status, "ready");
    assert!(lines.iter().any(|line| line.contains("prompt=0")));
    assert!(
        lines
            .iter()
            .any(|line| line.contains("summary: compacted=2 tail=3"))
    );
    assert!(
        lines
            .iter()
            .any(|line| line.contains("[assistant] Compacted 2 earlier messages"))
    );
    assert!(lines.iter().any(|line| line.contains("/compact preview")));
    Ok(())
}

#[test]
fn session_view_mode_toggle_switches_between_provider_and_audit() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("termquill.toml"), &test_config());
    app.handle_worker_message(WorkerMessage::SessionSwitched {
        session_log_path: app.session_log_path.clone(),
        provider_name: app.provider_name.clone(),
        model_name: app.model_name.clone(),
        entries: vec![
            SessionLogEntry::Control(ControlEntry::SessionIdentity {
                provider_name: "deepseek".to_owned(),
                model_name: "deepseek-v4-flash".to_owned(),
            }),
            SessionLogEntry::Control(ControlEntry::CompactionApplied(CompactionRecord {
                summary: "Compacted 1 earlier messages into a stable local summary.".to_owned(),
                compacted_message_count: 1,
                retained_tail_message_count: 1,
            })),
            SessionLogEntry::User(ModelMessage::user("latest prompt")),
        ],
    })?;

    let provider_lines = app.approval_preview_lines().join("\n");
    assert!(provider_lines.contains("provider view"));
    assert!(provider_lines.contains("Provider:"));

    app.session_view_mode = super::SessionViewMode::Audit;
    let audit_lines = app.approval_preview_lines().join("\n");
    assert!(audit_lines.contains("audit view"));
    assert!(audit_lines.contains("Audit:"));
    assert!(audit_lines.contains("[ctl] compacted=1 tail=1"));
    Ok(())
}

#[test]
fn sessions_filter_narrows_sidebar_results() -> Result<()> {
    let temp = tempdir()?;
    let config = RootConfig {
        workspace: WorkspaceConfig {
            root: temp.path().display().to_string(),
        },
        ..test_config()
    };
    let session_dir = temp.path().join(".termquill/sessions");
    std::fs::create_dir_all(&session_dir)?;
    std::fs::write(session_dir.join("session-alpha.jsonl"), "")?;
    std::fs::write(session_dir.join("session-beta.jsonl"), "")?;

    let mut app = AppState::from_root_config(Path::new("termquill.toml"), &config);
    app.refresh_session_history();
    app.session_history_filter = "b".to_owned();
    let lines = app.recent_session_lines().join("\n");
    assert!(lines.contains("beta"));
    assert!(!lines.contains("alpha"));
    Ok(())
}

#[test]
fn session_rows_mark_selected_and_current_entry() -> Result<()> {
    let temp = tempdir()?;
    let config = RootConfig {
        workspace: WorkspaceConfig {
            root: temp.path().display().to_string(),
        },
        ..test_config()
    };
    let session_dir = temp.path().join(".termquill/sessions");
    std::fs::create_dir_all(&session_dir)?;
    let alpha = session_dir.join("session-alpha.jsonl");
    let beta = session_dir.join("session-beta.jsonl");
    std::fs::write(&alpha, "")?;
    std::fs::write(&beta, "")?;

    let mut app = AppState::from_root_config(Path::new("termquill.toml"), &config);
    app.session_log_path = beta.clone();
    app.refresh_session_history();

    let rows = app.recent_session_rows();
    assert!(rows.iter().any(|row| {
        matches!(
            row,
            super::SessionHistoryRow::SessionItem {
                label,
                current: true,
                selected: true,
                ..
            } if label.contains("beta")
        )
    }));
    Ok(())
}

#[test]
fn session_history_uses_first_user_prompt_as_display_title() -> Result<()> {
    let temp = tempdir()?;
    let config = RootConfig {
        workspace: WorkspaceConfig {
            root: temp.path().display().to_string(),
        },
        ..test_config()
    };
    let session_dir = temp.path().join(".termquill/sessions");
    std::fs::create_dir_all(&session_dir)?;
    let session_path = session_dir.join("session-title.jsonl");
    write_session_log(
        &session_path,
        &[
            SessionLogEntry::Control(ControlEntry::SessionIdentity {
                provider_name: "deepseek".to_owned(),
                model_name: "deepseek-v4-pro".to_owned(),
            }),
            SessionLogEntry::User(ModelMessage::user("Investigate selector title display")),
        ],
    )?;

    let mut app = AppState::from_root_config(Path::new("termquill.toml"), &config);
    app.refresh_session_history();

    assert_eq!(
        app.session_history
            .iter()
            .find(|entry| entry.path == session_path)
            .and_then(|entry| entry.title.as_deref()),
        Some("Investigate selector title display")
    );

    app.input = "/resume".to_owned();
    assert!(
        app.slash_selector_rows()
            .iter()
            .any(|(_, description)| { description.contains("Investigate selector title display") })
    );
    Ok(())
}

#[test]
fn resume_command_shows_session_selector_and_enter_switches_selected_session() -> Result<()> {
    let temp = tempdir()?;
    let config = RootConfig {
        workspace: WorkspaceConfig {
            root: temp.path().display().to_string(),
        },
        ..test_config()
    };
    let session_dir = temp.path().join(".termquill/sessions");
    std::fs::create_dir_all(&session_dir)?;
    let restored_path = session_dir.join("session-restored.jsonl");
    let restored = restored_entries("restored-provider", "restored-model");
    write_session_log(&restored_path, &restored)?;

    let mut app = AppState::from_root_config(Path::new("termquill.toml"), &config);
    app.input = "/resume".to_owned();

    let selector_rows = app.slash_selector_rows();
    assert_eq!(app.slash_selector_title(), Some("Resume session"));
    assert_eq!(app.slash_selector_visible_rows(), 2);
    assert!(
        selector_rows
            .iter()
            .any(|(_, description)| description.contains("restored"))
    );

    let action = app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))?;
    assert!(matches!(
        action,
        Some(AppAction::SwitchSession { session_log_path }) if session_log_path == restored_path
    ));
    Ok(())
}

#[test]
fn resume_command_then_session_switch_restores_durable_view() -> Result<()> {
    let temp = tempdir()?;
    let config = RootConfig {
        workspace: WorkspaceConfig {
            root: temp.path().display().to_string(),
        },
        ..test_config()
    };
    let session_dir = temp.path().join(".termquill/sessions");
    std::fs::create_dir_all(&session_dir)?;
    let restored_path = session_dir.join("session-restored.jsonl");
    let restored = restored_entries("restored-provider", "restored-model");
    write_session_log(&restored_path, &restored)?;

    let mut app = AppState::from_root_config(Path::new("termquill.toml"), &config);
    app.input = "/resume 1".to_owned();
    let action = app.submit_input()?;
    assert!(matches!(
        action,
        Some(AppAction::SwitchSession { session_log_path }) if session_log_path == restored_path
    ));

    let entries = JsonlSessionStore::read_entries(&restored_path)?;
    app.handle_worker_message(WorkerMessage::SessionSwitched {
        session_log_path: restored_path.clone(),
        provider_name: "restored-provider".to_owned(),
        model_name: "restored-model".to_owned(),
        entries,
    })?;

    assert_eq!(app.provider_name, "restored-provider");
    assert_eq!(app.model_name, "restored-model");
    assert_eq!(app.session_id, "restored");
    assert_eq!(app.session_log_path, restored_path);
    assert!(
        app.timeline
            .iter()
            .any(|entry| entry.text.contains("restored from disk"))
    );
    assert!(
        app.timeline
            .iter()
            .any(|entry| entry.text == "restored user prompt")
    );
    assert!(
        app.timeline
            .iter()
            .any(|entry| entry.text.contains("restored tool output"))
    );
    assert!(
        app.timeline
            .iter()
            .any(|entry| entry.text == "restored assistant answer")
    );
    assert!(
        app.events
            .iter()
            .any(|event| event.label == "model"
                && event.detail == "restored-provider/restored-model")
    );
    assert!(
        app.events
            .iter()
            .any(|event| event.label == "restore" && event.detail == "entries=4")
    );
    Ok(())
}

use super::*;

#[test]
fn normal_input_creates_user_and_running_state() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("termquill.toml"), &test_config());
    app.input = "hello".to_owned();
    let action = app.submit_input()?;
    assert!(
        app.timeline
            .iter()
            .any(|entry| { entry.role == TimelineRole::User && entry.text == "hello" })
    );
    assert!(matches!(action, Some(AppAction::SubmitPrompt(prompt)) if prompt == "hello"));
    assert!(app.is_busy);
    assert_eq!(app.active_pane, PaneFocus::Composer);
    assert_eq!(app.composer_height(), 5);
    assert!(
        !app.timeline
            .iter()
            .any(|entry| entry.role == TimelineRole::Phase)
    );
    assert!(
        app.events.iter().any(|event| {
            event.label == "phase" && event.detail == "thinking|deepseek-v4-flash"
        })
    );
    assert_eq!(app.run_phase(), RunPhase::Thinking);
    assert_eq!(app.last_notice(), Some("thinking"));
    Ok(())
}

#[test]
fn activate_lazy_mcp_action_maps_to_worker_command() {
    let app = AppState::from_root_config(Path::new("termquill.toml"), &test_config());

    let command = app.into_worker_command(AppAction::ActivateLazyMcp {
        server_name: Some("filesystem".to_owned()),
    });

    assert!(matches!(
        command,
        WorkerCommand::ActivateLazyMcp {
            server_name: Some(ref server_name)
        } if server_name == "filesystem"
    ));
}

#[test]
fn run_failed_surfaces_root_cause_summary_in_notice() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("termquill.toml"), &test_config());

    app.handle_worker_message(WorkerMessage::RunFailed(
        "deepseek request failed\n\nCaused by:\n    0: failed to send DeepSeek request\n    1: error sending request for url (https://api.example.com)"
            .to_owned(),
    ))?;

    assert_eq!(
        app.last_notice(),
        Some("error sending request for url (https://api.example.com)")
    );
    assert!(app.timeline.iter().any(|entry| {
        entry
            .text
            .contains("error sending request for url (https://api.example.com)")
    }));
    assert!(app.events.iter().any(
        |event| event.label == "run:error" && event.detail.contains("deepseek request failed")
    ));
    Ok(())
}

#[test]
fn automatic_compaction_message_resets_status_and_emits_notice() -> Result<()> {
    let mut config = test_config();
    config.agent.provider = "planned".to_owned();
    config.agent.model = "planned-model".to_owned();
    config.compaction.context_window_tokens = Some(100);
    config.compaction.soft_threshold_ratio = 0.5;
    config.compaction.hard_threshold_ratio = 0.8;
    let mut app = AppState::from_root_config(Path::new("termquill.toml"), &config);
    let session_log_path = app.session_log_path.clone();

    app.handle(RunEvent::Usage(UsageStats {
        prompt_tokens: 90,
        completion_tokens: 0,
        cache_hit_tokens: 0,
        cache_miss_tokens: 90,
        input_cost: 0.0,
        output_cost: 0.0,
        cache_savings: 0.0,
        system_fingerprint: None,
    }))?;
    assert_eq!(app.compaction_status, "hard");

    app.handle_worker_message(WorkerMessage::SessionCompacted {
        session_log_path,
        provider_name: app.provider_name.clone(),
        model_name: app.model_name.clone(),
        record: CompactionRecord {
            summary: "summary".to_owned(),
            compacted_message_count: 3,
            retained_tail_message_count: 2,
        },
        trigger: CompactionTrigger::AutomaticHardThreshold,
        entries: Vec::new(),
    })?;

    assert_eq!(app.compaction_status, "ready");
    assert_eq!(app.stats.last_prompt_tokens, 0);
    assert!(app.timeline.iter().any(|entry| {
        entry.role == TimelineRole::Notice && entry.text.contains("Auto-compacted")
    }));
    Ok(())
}

#[test]
fn ctrl_c_then_run_cancelled_restores_durable_session_view() -> Result<()> {
    let temp = tempdir()?;
    let config = RootConfig {
        workspace: WorkspaceConfig {
            root: temp.path().display().to_string(),
        },
        ..test_config()
    };
    let session_dir = temp.path().join(".termquill/sessions");
    std::fs::create_dir_all(&session_dir)?;
    let restored_path = session_dir.join("session-cancelled.jsonl");
    let restored = restored_entries("cancel-provider", "cancel-model");
    write_session_log(&restored_path, &restored)?;

    let mut app = AppState::from_root_config(Path::new("termquill.toml"), &config);
    app.input = "volatile prompt".to_owned();
    assert!(matches!(
        app.submit_input()?,
        Some(AppAction::SubmitPrompt(prompt)) if prompt == "volatile prompt"
    ));
    assert!(app.is_busy);

    let cancel_action =
        app.handle_key_event(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL))?;
    assert!(matches!(cancel_action, Some(AppAction::CancelRun)));
    assert!(
        app.timeline
            .iter()
            .any(|entry| entry.text.contains("cancel requested"))
    );

    let entries = JsonlSessionStore::read_entries(&restored_path)?;
    app.handle_worker_message(WorkerMessage::RunCancelled {
        session_log_path: restored_path.clone(),
        provider_name: "cancel-provider".to_owned(),
        model_name: "cancel-model".to_owned(),
        entries,
    })?;

    assert!(!app.is_busy);
    assert!(app.pending_approval.is_none());
    assert_eq!(app.provider_name, "cancel-provider");
    assert_eq!(app.model_name, "cancel-model");
    assert_eq!(app.session_id, "cancelled");
    assert_eq!(app.session_log_path, restored_path);
    assert!(
        app.timeline
            .iter()
            .any(|entry| { entry.text.contains("run cancelled; restored") })
    );
    assert!(
        !app.timeline
            .iter()
            .any(|entry| entry.text == "volatile prompt")
    );
    assert!(
        app.timeline
            .iter()
            .any(|entry| entry.text == "restored assistant answer")
    );
    assert!(
        app.events
            .iter()
            .any(|event| event.label == "restore" && event.detail == "entries=4")
    );
    assert!(
        app.events
            .iter()
            .any(|event| event.label == "model" && event.detail == "cancel-provider/cancel-model")
    );
    Ok(())
}

#[test]
fn esc_interrupts_active_run() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("termquill.toml"), &test_config());
    app.input = "long task".to_owned();
    assert!(matches!(
        app.submit_input()?,
        Some(AppAction::SubmitPrompt(prompt)) if prompt == "long task"
    ));
    assert!(app.is_busy);

    let cancel_action = app.handle_key_event(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE))?;

    assert!(matches!(cancel_action, Some(AppAction::CancelRun)));
    assert_eq!(app.last_notice(), Some("cancellation requested"));
    assert!(
        app.timeline
            .iter()
            .any(|entry| entry.text.contains("cancel requested"))
    );
    Ok(())
}

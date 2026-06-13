use super::*;
use crate::approval::PendingApproval;

#[test]
fn normal_input_creates_user_and_running_state() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
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
    let app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());

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
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());

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
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &config);
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
    let session_dir = temp.path().join(".sigil/sessions");
    std::fs::create_dir_all(&session_dir)?;
    let restored_path = session_dir.join("session-cancelled.jsonl");
    let restored = restored_entries("cancel-provider", "cancel-model");
    write_session_log(&restored_path, &restored)?;

    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &config);
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
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
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

#[test]
fn worker_messages_apply_balance_and_model_refresh() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.open_model_picker(ModelPickerTarget::Provider, "custom-model");
    let model_request_id = app
        .active_model_picker_refresh
        .as_ref()
        .expect("model picker refresh should be active")
        .request_id;

    let balance_request_id = app.next_background_request_id();
    app.active_balance_refresh_id = Some(balance_request_id);
    app.handle_worker_message(WorkerMessage::ProviderBalanceRefreshed {
        request_id: balance_request_id,
        snapshot: crate::provider_status::BalanceSnapshot {
            total: Some(2.0),
            currency: Some("USD".to_owned()),
            available: true,
            status: "USD 2.00".to_owned(),
        },
    })?;
    app.handle_worker_message(WorkerMessage::ProviderModelsRefreshed {
        request_id: model_request_id,
        base_url: "https://example.com".to_owned(),
        result: Ok(vec!["remote-model".to_owned()]),
    })?;

    assert_eq!(app.balance_snapshot.status, "USD 2.00");
    assert!(app.active_balance_refresh_id.is_none());
    assert!(app.active_model_picker_refresh.is_none());
    assert_eq!(
        app.last_notice(),
        Some("loaded provider model list (https://example.com)")
    );
    assert!(app.modal_lines().join("\n").contains("remote-model"));
    assert!(
        app.events
            .iter()
            .any(|event| event.label == "balance" && event.detail == "USD 2.00")
    );
    Ok(())
}

#[test]
fn pending_worker_commands_and_stale_provider_refreshes_are_noops() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    assert!(!app.poll_background_tasks());
    assert!(!app.has_pending_worker_commands());

    app.open_model_picker(ModelPickerTarget::Provider, "custom-model");
    let model_request_id = app
        .active_model_picker_refresh
        .as_ref()
        .expect("model picker refresh should be active")
        .request_id;
    assert!(app.has_pending_worker_commands());

    let commands = app.drain_pending_worker_commands();
    assert!(matches!(
        commands.as_slice(),
        [WorkerCommand::RefreshProviderModels { request_id, .. }] if *request_id == model_request_id
    ));
    assert!(!app.has_pending_worker_commands());
    assert!(app.drain_pending_worker_commands().is_empty());

    app.handle_worker_message(WorkerMessage::ProviderModelsRefreshed {
        request_id: model_request_id + 1,
        base_url: "https://stale.example".to_owned(),
        result: Ok(vec!["stale".to_owned()]),
    })?;
    assert!(app.active_model_picker_refresh.is_some());

    app.active_model_picker_refresh = None;
    app.handle_worker_message(WorkerMessage::ProviderModelsRefreshed {
        request_id: model_request_id,
        base_url: "https://none.example".to_owned(),
        result: Ok(vec!["ignored".to_owned()]),
    })?;

    app.active_balance_refresh_id = Some(7);
    let previous_status = app.balance_snapshot.status.clone();
    app.handle_worker_message(WorkerMessage::ProviderBalanceRefreshed {
        request_id: 8,
        snapshot: crate::provider_status::BalanceSnapshot {
            total: Some(1.0),
            currency: Some("USD".to_owned()),
            available: true,
            status: "USD 1.00".to_owned(),
        },
    })?;
    assert_eq!(app.active_balance_refresh_id, Some(7));
    assert_eq!(app.balance_snapshot.status, previous_status);
    Ok(())
}

#[test]
fn schedule_balance_refresh_handles_missing_config_and_auth() {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());

    app.config_snapshot = None;
    app.schedule_balance_refresh();
    assert_eq!(app.balance_snapshot.status, "n/a");
    assert!(app.active_balance_refresh_id.is_none());

    app.apply_runtime_config_snapshot(&test_config());
    app.schedule_balance_refresh();
    assert_eq!(app.balance_snapshot.status, "missing auth");
    assert!(app.active_balance_refresh_id.is_none());

    let temp = tempdir().expect("tempdir should be created");
    let mut setup_app = AppState::from_setup(
        temp.path().join("sigil.toml"),
        temp.path().join("workspace"),
        None,
    );
    setup_app.schedule_balance_refresh();
    assert!(setup_app.active_balance_refresh_id.is_none());
}

#[test]
fn code_intelligence_results_update_status_lines_and_diagnostics() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());

    app.handle(RunEvent::ToolResult(ToolResult::ok(
        "call-code-status",
        "code_status",
        "{}".to_owned(),
        ToolResultMeta {
            details: json!({
                "code_intelligence": {
                    "servers": [
                        { "server": "rust-analyzer", "status": "ready", "languages": ["rust"] },
                        { "server": "pyright", "status": "fallback", "languages": ["python"] }
                    ]
                }
            }),
            ..ToolResultMeta::default()
        },
    )))?;

    assert_eq!(app.code_intelligence_status, "ready");
    assert_eq!(
        app.code_intelligence_server_lines.get("rust-analyzer"),
        Some(&"rust: ready rust-analyzer".to_owned())
    );
    assert_eq!(
        app.code_intelligence_server_lines.get("pyright"),
        Some(&"python: fallback pyright".to_owned())
    );

    app.handle(RunEvent::ToolResult(ToolResult::ok(
        "call-code-diag",
        "code_diagnostics",
        json!({
            "query": { "paths": ["./src/main.rs", "src/lib.rs"] },
            "diagnostics": [
                { "path": "./src/main.rs", "severity": "error" },
                { "path": "src/main.rs", "severity": "warning" }
            ]
        })
        .to_string(),
        ToolResultMeta::default(),
    )))?;

    assert_eq!(
        app.code_intelligence_status,
        "diagnostics 1 errors 1 warnings"
    );
    assert_eq!(
        app.code_intelligence_diagnostics_line.as_deref(),
        Some("diagnostics: 1 errors 1 warnings")
    );
    assert_eq!(
        app.code_intelligence_diagnostics_by_path.get("src/main.rs"),
        Some(&ApprovalDiagnosticSummary {
            errors: 1,
            warnings: 1,
        })
    );
    assert_eq!(
        app.code_intelligence_diagnostics_by_path.get("src/lib.rs"),
        Some(&ApprovalDiagnosticSummary::default())
    );

    app.handle(RunEvent::ToolResult(ToolResult::error(
        "call-code-error",
        "code_search",
        ToolErrorKind::Protocol,
        "bad response",
    )))?;
    assert_eq!(app.code_intelligence_status, "degraded tool error");
    assert_eq!(
        app.code_intelligence_server_lines.get("status"),
        Some(&"status: degraded tool error".to_owned())
    );
    Ok(())
}

#[test]
fn worker_messages_cover_run_start_notice_and_manual_compaction_restore() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());

    app.handle_worker_message(WorkerMessage::RunStarted {
        prompt: "draft plan".to_owned(),
    })?;
    assert_eq!(app.run_phase(), RunPhase::Thinking);
    assert_eq!(app.last_notice(), Some("thinking"));
    assert!(
        app.events
            .iter()
            .any(|event| event.label == "run:start" && event.detail == "draft plan")
    );

    app.handle_worker_message(WorkerMessage::McpActivationStatus {
        server_name: None,
        status: McpActivationStatus::Deferred,
    })?;
    assert!(
        app.events
            .iter()
            .any(|event| event.label == "mcp" && event.detail == "deferred")
    );

    let temp = tempdir()?;
    let session_log_path = temp.path().join("session-restored.jsonl");
    let entries = restored_entries("restored-provider", "restored-model");
    app.handle_worker_message(WorkerMessage::SessionCompacted {
        session_log_path: session_log_path.clone(),
        provider_name: "restored-provider".to_owned(),
        model_name: "restored-model".to_owned(),
        record: CompactionRecord {
            summary: "summary".to_owned(),
            compacted_message_count: 2,
            retained_tail_message_count: 1,
        },
        trigger: CompactionTrigger::Manual,
        entries,
    })?;

    assert_eq!(app.session_log_path, session_log_path);
    assert_eq!(app.last_notice(), Some("Session compacted."));
    assert!(
        app.timeline
            .iter()
            .any(|entry| entry.text.contains("Session compacted."))
    );
    Ok(())
}

#[test]
fn worker_messages_cover_run_finished_notice_session_switch_and_failure_reset() -> Result<()> {
    let temp = tempdir()?;
    let config = RootConfig {
        workspace: WorkspaceConfig {
            root: temp.path().display().to_string(),
        },
        ..test_config()
    };
    let restored_path = temp.path().join("session-restored.jsonl");
    let entries = restored_entries("restored-provider", "restored-model");

    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &config);
    app.is_busy = true;
    app.pending_approval = Some(PendingApproval {
        call: ToolCall {
            id: "call-1".to_owned(),
            name: "write_file".to_owned(),
            args_json: "{}".to_owned(),
        },
        spec: ToolSpec {
            name: "write_file".to_owned(),
            description: "Write a file".to_owned(),
            input_schema: json!({"type": "object"}),
            category: ToolCategory::File,
            access: ToolAccess::Write,
            preview: ToolPreviewCapability::Required,
        },
        subjects: Vec::new(),
        preview: None,
    });
    app.modal_state = Some(ModalState::KeyboardHelp);
    app.streaming_reasoning_index = Some(0);

    app.handle_worker_message(WorkerMessage::RunFinished {
        result: sigil_kernel::AgentRunResult {
            final_text: "done".to_owned(),
            tool_calls: 2,
        },
        entries: entries.clone(),
    })?;

    assert!(!app.is_busy);
    assert!(app.pending_approval.is_none());
    assert!(app.modal_state.is_none());
    assert_eq!(app.run_phase(), RunPhase::Idle);
    assert_eq!(app.last_notice(), Some("agent idle"));
    assert!(app.events.iter().any(|event| {
        event.label == "run:finish" && event.detail == "tool_calls=2 final_text_bytes=4"
    }));

    app.handle_worker_message(WorkerMessage::Notice("worker note".to_owned()))?;
    assert_eq!(app.last_notice(), Some("worker note"));
    assert!(
        app.timeline
            .iter()
            .any(|entry| entry.role == TimelineRole::Notice && entry.text == "worker note")
    );
    assert!(
        app.events
            .iter()
            .any(|event| event.label == "worker" && event.detail == "worker note")
    );

    app.handle_worker_message(WorkerMessage::SessionSwitched {
        session_log_path: restored_path.clone(),
        provider_name: "restored-provider".to_owned(),
        model_name: "restored-model".to_owned(),
        entries: entries.clone(),
    })?;
    assert_eq!(app.session_log_path, restored_path);
    assert_eq!(app.provider_name, "restored-provider");
    assert_eq!(app.model_name, "restored-model");
    assert_eq!(app.last_notice(), Some("restored from disk"));

    app.is_busy = true;
    app.modal_state = Some(ModalState::KeyboardHelp);
    app.handle_worker_message(WorkerMessage::RunFailed(
        "request failed\n\nCaused by:\n  0: timeout".to_owned(),
    ))?;
    assert!(!app.is_busy);
    assert!(app.modal_state.is_none());
    assert_eq!(app.run_phase(), RunPhase::Idle);
    assert_eq!(app.last_notice(), Some("timeout"));
    Ok(())
}

#[test]
fn worker_events_cover_completion_continuation_and_duplicate_assistant_messages() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());

    app.handle(RunEvent::ToolCallArgsDelta {
        id: "call-1".to_owned(),
        delta: "{}".to_owned(),
    })?;
    assert_eq!(app.run_phase(), RunPhase::Tool("tool".to_owned()));

    app.handle(RunEvent::ToolCallCompleted(ToolCall {
        id: "call-1".to_owned(),
        name: "read_file".to_owned(),
        args_json: "{}".to_owned(),
    }))?;
    assert_eq!(app.run_phase(), RunPhase::Tool("read_file".to_owned()));
    assert!(
        app.events
            .iter()
            .any(|event| event.label == "tool:complete" && event.detail == "read_file call-1")
    );

    app.handle(RunEvent::Control(ControlEntry::Note {
        kind: "custom".to_owned(),
        data: json!({ "value": 1 }),
    }))?;
    assert!(
        app.events
            .iter()
            .any(|event| event.label == "control" && event.detail.contains("custom"))
    );

    app.handle(RunEvent::ContinuationState(
        sigil_kernel::ProviderContinuationState {
            provider_name: "deepseek".to_owned(),
            state_kind: "resume".to_owned(),
            message_id: Some("msg-1".to_owned()),
            opaque_blob: json!({ "cursor": 1 }),
        },
    ))?;
    assert!(
        app.events
            .iter()
            .any(|event| event.label == "continuation" && event.detail == "resume")
    );

    app.handle(RunEvent::AssistantMessage(ModelMessage::assistant(
        Some("same answer".to_owned()),
        Vec::new(),
    )))?;
    app.handle(RunEvent::AssistantMessage(ModelMessage::assistant(
        Some("same answer".to_owned()),
        Vec::new(),
    )))?;
    app.handle(RunEvent::AssistantMessage(ModelMessage::assistant(
        Some(String::new()),
        Vec::new(),
    )))?;

    let matching = app
        .timeline
        .iter()
        .filter(|entry| entry.role == TimelineRole::Assistant && entry.text == "same answer")
        .count();
    assert_eq!(matching, 1);
    Ok(())
}

#[test]
fn worker_command_conversion_covers_remaining_variants_and_panics_for_config_updates() {
    let app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());

    assert!(matches!(
        app.into_worker_command(AppAction::SubmitPrompt("draft".to_owned())),
        WorkerCommand::SubmitPrompt { prompt, .. } if prompt == "draft"
    ));
    assert!(matches!(
        app.into_worker_command(AppAction::ApprovalDecision {
            call_id: "call-1".to_owned(),
            approved: true,
        }),
        WorkerCommand::ApprovalDecision { call_id, approved }
            if call_id == "call-1" && approved
    ));
    assert!(matches!(
        app.into_worker_command(AppAction::CancelRun),
        WorkerCommand::CancelRun
    ));
    assert!(matches!(
        app.into_worker_command(AppAction::CompactNow),
        WorkerCommand::CompactNow
    ));
    assert!(matches!(
        app.into_worker_command(AppAction::CheckChangedFilesDiagnostics),
        WorkerCommand::CheckChangedFilesDiagnostics
    ));
    assert!(matches!(
        app.into_worker_command(AppAction::SwitchSession {
            session_log_path: std::path::PathBuf::from("session.jsonl"),
        }),
        WorkerCommand::SwitchSession { session_log_path }
            if session_log_path == std::path::Path::new("session.jsonl")
    ));
    assert!(matches!(
        AppState::shutdown_command(),
        WorkerCommand::Shutdown
    ));

    let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        app.into_worker_command(AppAction::ConfigSaved {
            root_config: Box::new(test_config()),
        })
    }));
    assert!(panic.is_err());
}

#[test]
fn manual_compaction_restores_session_view_and_notice() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.input = "compact this".to_owned();
    assert!(matches!(
        app.submit_input()?,
        Some(AppAction::SubmitPrompt(prompt)) if prompt == "compact this"
    ));
    assert!(app.is_busy);

    let session_log_path = app.session_log_path.clone();
    let entries = restored_entries("compacted-provider", "compacted-model");
    app.handle_worker_message(WorkerMessage::SessionCompacted {
        session_log_path: session_log_path.clone(),
        provider_name: "compacted-provider".to_owned(),
        model_name: "compacted-model".to_owned(),
        record: CompactionRecord {
            summary: "summary".to_owned(),
            compacted_message_count: 2,
            retained_tail_message_count: 1,
        },
        trigger: CompactionTrigger::Manual,
        entries: entries.clone(),
    })?;

    assert!(!app.is_busy);
    assert_eq!(app.provider_name, "compacted-provider");
    assert_eq!(app.model_name, "compacted-model");
    assert_eq!(app.session_log_path, session_log_path);
    assert_eq!(app.last_notice(), Some("Session compacted."));
    assert!(
        app.timeline
            .iter()
            .any(|entry| entry.text.contains("Session compacted."))
    );
    assert!(
        app.events.iter().any(|event| event.label == "restore"
            && event.detail == format!("entries={}", entries.len()))
    );
    Ok(())
}

#[test]
fn mcp_activation_status_without_server_name_only_emits_event() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    let before = app.mcp_server_statuses.clone();

    app.handle_worker_message(WorkerMessage::McpActivationStatus {
        server_name: None,
        status: McpActivationStatus::Failed {
            error: "MCP server filesystem tools/list failed: bad response".to_owned(),
        },
    })?;

    assert_eq!(app.mcp_server_statuses, before);
    assert!(app.mcp_server_runtime_status_label("filesystem").is_none());
    assert!(app.events.iter().any(|event| {
        event.label == "mcp"
            && event.detail.contains("failed")
            && event.detail.contains("bad response")
    }));
    Ok(())
}

#[test]
fn mcp_activate_server_tool_result_marks_lazy_server_ready() -> Result<()> {
    let mut config = test_config();
    config.mcp_servers.push(sigil_kernel::McpServerConfig {
        name: "filesystem".to_owned(),
        startup: sigil_kernel::McpServerStartup::Lazy,
        ..sigil_kernel::McpServerConfig::default()
    });
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &config);

    app.handle(RunEvent::ToolResult(ToolResult::ok(
        "activate-filesystem",
        "mcp_activate_server",
        serde_json::json!({
            "server_name": "filesystem",
            "status": "ready",
            "matched_servers": 1,
            "added_tools": 2
        })
        .to_string(),
        Default::default(),
    )))?;

    assert_eq!(
        app.mcp_server_runtime_status_label("filesystem").as_deref(),
        Some("ready 2 tools")
    );
    assert!(app.events.iter().any(|event| {
        event.label == "mcp" && event.detail == "server=filesystem ready tools=2"
    }));
    Ok(())
}

#[test]
fn mcp_runtime_progress_updates_live_activity_without_timeline_notice() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.is_busy = true;
    app.run_phase = RunPhase::Tool("mcp__filesystem__scan".to_owned());
    let before_timeline_len = app.timeline.len();

    app.handle_worker_message(WorkerMessage::McpProgress {
        notification: sigil_runtime::McpProgressNotification {
            server_name: "filesystem".to_owned(),
            progress_token: "scan".to_owned(),
            progress: Some(1.0),
            total: Some(4.0),
            message: Some("Scanning".to_owned()),
        },
    })?;

    let summary = app.live_activity_summary().expect("expected mcp progress");
    assert_eq!(summary.label, "mcp");
    assert_eq!(summary.detail, "filesystem: Scanning 25%");
    assert_eq!(app.timeline.len(), before_timeline_len);

    app.handle_worker_message(WorkerMessage::McpProgress {
        notification: sigil_runtime::McpProgressNotification {
            server_name: "filesystem".to_owned(),
            progress_token: "scan".to_owned(),
            progress: Some(7.0),
            total: None,
            message: Some(" ".to_owned()),
        },
    })?;
    let summary = app
        .live_activity_summary()
        .expect("expected progress-only mcp summary");
    assert_eq!(summary.detail, "filesystem: working 7");

    app.handle_worker_message(WorkerMessage::McpProgress {
        notification: sigil_runtime::McpProgressNotification {
            server_name: "filesystem".to_owned(),
            progress_token: "scan".to_owned(),
            progress: None,
            total: None,
            message: None,
        },
    })?;
    let summary = app
        .live_activity_summary()
        .expect("expected message-only mcp summary");
    assert_eq!(summary.detail, "filesystem: working");
    Ok(())
}

#[test]
fn mcp_list_changed_marks_server_stale_until_refresh_status_arrives() -> Result<()> {
    let mut config = test_config();
    config.mcp_servers.push(sigil_kernel::McpServerConfig {
        name: "filesystem".to_owned(),
        startup: sigil_kernel::McpServerStartup::Eager,
        ..sigil_kernel::McpServerConfig::default()
    });
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &config);

    app.handle_worker_message(WorkerMessage::McpListChanged {
        notification: sigil_runtime::McpListChangedNotification {
            server_name: "filesystem".to_owned(),
            kind: sigil_runtime::McpListChangedKind::Prompts,
        },
    })?;

    assert_eq!(
        app.mcp_server_runtime_status_label("filesystem").as_deref(),
        Some("stale prompts")
    );
    app.handle_worker_message(WorkerMessage::McpActivationStatus {
        server_name: Some("filesystem".to_owned()),
        status: McpActivationStatus::Refreshing,
    })?;
    assert_eq!(
        app.mcp_server_runtime_status_label("filesystem").as_deref(),
        Some("refreshing")
    );
    Ok(())
}

#[test]
fn run_finished_clears_modal_pending_approval_and_busy_state() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.input = "work".to_owned();
    assert!(matches!(
        app.submit_input()?,
        Some(AppAction::SubmitPrompt(prompt)) if prompt == "work"
    ));
    inject_write_file_approval(&mut app, sample_approval_preview())?;
    let _ = app.handle_key_event(KeyEvent::new(KeyCode::F(1), KeyModifiers::NONE))?;
    assert!(app.has_modal());
    assert!(app.pending_approval.is_some());

    app.handle_worker_message(WorkerMessage::RunFinished {
        result: sigil_kernel::AgentRunResult {
            final_text: "done".to_owned(),
            tool_calls: 1,
        },
        entries: restored_entries("deepseek", "deepseek-v4-flash"),
    })?;

    assert!(!app.is_busy);
    assert_eq!(app.run_phase(), RunPhase::Idle);
    assert!(!app.has_modal());
    assert!(app.pending_approval.is_none());
    assert_eq!(app.last_notice(), Some("agent idle"));
    assert!(
        app.events
            .iter()
            .any(|event| event.label == "run:finish" && event.detail.contains("tool_calls=1"))
    );
    Ok(())
}

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
fn run_notice_filters_status_noise_but_keeps_errors() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());

    app.handle(RunEvent::Notice("agent agent_chat_1 finished".to_owned()))?;
    assert_eq!(app.last_notice(), Some("agent agent_chat_1 finished"));
    assert!(
        !app.timeline
            .iter()
            .any(|entry| entry.role == TimelineRole::Notice)
    );
    assert!(
        app.events
            .iter()
            .any(|event| event.label == "notice" && event.detail == "agent agent_chat_1 finished")
    );

    app.handle(RunEvent::Notice(
        "permission wait_agent subject=agent:agent_chat_1 mode=allow".to_owned(),
    ))?;
    assert_eq!(
        app.last_notice(),
        Some("permission wait_agent subject=agent:agent_chat_1 mode=allow")
    );
    assert!(!app.timeline.iter().any(|entry| {
        entry.role == TimelineRole::Notice && entry.text.contains("permission wait_agent")
    }));

    app.handle(RunEvent::Notice(
        "agent budget warning after child completion: max exceeded".to_owned(),
    ))?;
    assert!(app.timeline.iter().any(|entry| {
        entry.role == TimelineRole::Notice
            && entry
                .text
                .contains("agent budget warning after child completion")
    }));
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
fn plan_actions_map_to_worker_commands() {
    let app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());

    let submit = app.into_worker_command(AppAction::SubmitTask("ship task".to_owned()));
    assert!(matches!(
        submit,
        WorkerCommand::SubmitTask { ref prompt } if prompt == "ship task"
    ));

    let plan_prompt = app.into_worker_command(AppAction::SubmitPlanPrompt(
        "inspect before editing".to_owned(),
    ));
    assert!(matches!(
        plan_prompt,
        WorkerCommand::SubmitPlanPrompt { ref prompt, .. }
            if prompt == "inspect before editing"
    ));

    let approve_plan = app.into_worker_command(AppAction::ApprovePlan {
        plan_text: "do the safe thing".to_owned(),
        permission: sigil_kernel::PlanApprovalPermission::WorkspaceEdits,
        scope_summary: "safe thing".to_owned(),
        clear_planning_context: true,
    });
    assert!(matches!(
        approve_plan,
        WorkerCommand::ApprovePlan {
            ref plan_text,
            permission: sigil_kernel::PlanApprovalPermission::WorkspaceEdits,
            ref scope_summary,
            clear_planning_context: true,
        } if plan_text == "do the safe thing" && scope_summary == "safe thing"
    ));

    let continue_task = app.into_worker_command(AppAction::ContinueTask {
        task_id: Some("task_1".to_owned()),
        guidance: Some("focus runtime".to_owned()),
    });
    assert!(matches!(
        continue_task,
        WorkerCommand::ContinueTask {
            task_id: Some(ref task_id),
            guidance: Some(ref guidance)
        } if task_id == "task_1" && guidance == "focus runtime"
    ));
}

#[test]
fn plan_run_finished_surfaces_pending_plan_approval_and_key_actions() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());

    app.handle_worker_message(WorkerMessage::PlanRunFinished {
        result: sigil_kernel::AgentRunResult {
            final_text: "1. inspect\n2. edit with preview".to_owned(),
            tool_calls: 0,
            final_message_id: None,
        },
        entries: Vec::new(),
    })?;

    let pending = app
        .pending_plan_approval()
        .expect("plan output should create a pending approval");
    assert!(pending.plan_hash.starts_with("sha256:"));
    assert_eq!(pending.scope_summary, "1. inspect");
    assert_eq!(app.last_notice(), Some("plan ready"));

    let action = app.handle_key_event(KeyEvent::new(KeyCode::Char('w'), KeyModifiers::NONE))?;
    assert!(app.pending_plan_approval().is_none());
    assert!(matches!(
        action,
        Some(AppAction::ApprovePlan {
            permission: sigil_kernel::PlanApprovalPermission::WorkspaceEdits,
            clear_planning_context: true,
            ..
        })
    ));

    app.handle_worker_message(WorkerMessage::PlanRunFinished {
        result: sigil_kernel::AgentRunResult {
            final_text: "   ".to_owned(),
            tool_calls: 1,
            final_message_id: None,
        },
        entries: Vec::new(),
    })?;
    assert!(app.pending_plan_approval().is_none());
    assert_eq!(app.last_notice(), Some("plan finished"));
    Ok(())
}

#[test]
fn pending_plan_approval_continue_returns_to_plan_composer_without_worker_action() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.handle_worker_message(WorkerMessage::PlanRunFinished {
        result: sigil_kernel::AgentRunResult {
            final_text: "1. inspect\n2. revise plan".to_owned(),
            tool_calls: 0,
            final_message_id: None,
        },
        entries: Vec::new(),
    })?;

    let action = app.handle_key_event(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::NONE))?;

    assert!(action.is_none());
    assert!(app.pending_plan_approval().is_none());
    assert_eq!(app.composer_mode_label(), "Plan");
    assert_eq!(app.last_notice(), Some("continue planning"));
    assert!(
        app.events
            .iter()
            .any(|event| { event.label == "plan" && event.detail == "continue" })
    );
    Ok(())
}

#[test]
fn pending_plan_approval_discard_clears_surface_without_worker_action() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.handle_worker_message(WorkerMessage::PlanRunFinished {
        result: sigil_kernel::AgentRunResult {
            final_text: "1. inspect\n2. revise plan".to_owned(),
            tool_calls: 0,
            final_message_id: None,
        },
        entries: Vec::new(),
    })?;

    let action = app.handle_key_event(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE))?;

    assert!(action.is_none());
    assert!(app.pending_plan_approval().is_none());
    assert_eq!(app.composer_mode_label(), "Build");
    assert_eq!(app.last_notice(), Some("plan approval dismissed"));
    assert!(
        app.events
            .iter()
            .any(|event| { event.label == "plan" && event.detail == "dismissed" })
    );
    Ok(())
}

#[test]
fn plan_approved_message_syncs_session_and_clears_pending_surface() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.set_pending_plan_approval_from_text("approved plan");
    let entry = sigil_kernel::PlanApprovedEntry {
        plan_version: 1,
        plan_hash: sigil_kernel::plan_text_hash("approved plan"),
        approved_at_ms: 42,
        permission: sigil_kernel::PlanApprovalPermission::Ask,
        scope: sigil_kernel::PlanApprovalScope {
            summary: "approved plan".to_owned(),
            workspace_paths: Vec::new(),
        },
        expires: sigil_kernel::PlanApprovalExpiry::NextUserPrompt,
        clear_planning_context: true,
    };

    app.handle_worker_message(WorkerMessage::PlanApproved {
        entry: entry.clone(),
        entries: vec![SessionLogEntry::Control(ControlEntry::PlanApproved(
            entry.clone(),
        ))],
    })?;

    assert!(app.pending_plan_approval().is_none());
    assert_eq!(app.last_notice(), Some("plan approved: ask"));
    assert_eq!(
        sigil_kernel::PlanApprovalProjection::from_entries(&app.current_session_entries)
            .latest_approval,
        Some(entry)
    );
    Ok(())
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
fn schedule_balance_refresh_skips_non_deepseek_provider() {
    let mut config = test_config();
    config.agent.provider = "openai_compat".to_owned();
    config.agent.model = "gpt-test".to_owned();
    config.providers.insert(
        "openai_compat".to_owned(),
        json!({
            "base_url": "https://openai.example.com/v1",
            "model": "gpt-test",
            "api_key": "openai-key"
        }),
    );
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &config);

    app.schedule_balance_refresh();

    assert_eq!(app.balance_snapshot.status, "n/a");
    assert!(app.active_balance_refresh_id.is_none());
    assert!(
        !app.drain_pending_worker_commands()
            .iter()
            .any(|command| matches!(command, WorkerCommand::RefreshProviderBalance { .. }))
    );
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
    app.handle_worker_message(WorkerMessage::PlanRunStarted {
        prompt: "inspect before editing".to_owned(),
    })?;
    assert_eq!(app.run_phase(), RunPhase::Thinking);
    assert_eq!(app.last_notice(), Some("planning"));
    assert!(
        app.events
            .iter()
            .any(|event| event.label == "plan:start" && event.detail == "inspect before editing")
    );
    app.handle_worker_message(WorkerMessage::AgentRunStarted {
        profile_id: "review".to_owned(),
        prompt: "inspect kernel".to_owned(),
    })?;
    assert_eq!(app.run_phase(), RunPhase::Agent("review".to_owned()));
    assert_eq!(app.last_notice(), Some("waiting for agent @review"));
    assert_eq!(
        app.live_activity_summary()
            .expect("agent run should expose live activity")
            .detail,
        "waiting for @review result"
    );
    assert!(!app.timeline.iter().any(|entry| {
        entry.role == TimelineRole::Notice
            && entry.text == "agent @review started; waiting for result"
    }));
    assert!(
        app.events
            .iter()
            .any(|event| event.label == "agent:start" && event.detail == "inspect kernel")
    );
    app.handle_worker_message(WorkerMessage::AgentResultContinuationStarted {
        thread_ids: vec![sigil_kernel::AgentThreadId::new("agent_chat_done")?],
    })?;
    assert!(app.is_busy);
    assert_eq!(app.run_phase(), RunPhase::Thinking);
    assert_eq!(app.last_notice(), Some("agent result ready; resuming main"));
    assert!(!app.timeline.iter().any(|entry| {
        entry.role == TimelineRole::Notice
            && entry
                .text
                .contains("agent result ready; resuming main for agent_chat_done")
    }));
    assert!(
        app.events
            .iter()
            .any(|event| event.label == "agent:resume" && event.detail == "agent_chat_done")
    );

    app.handle_worker_message(WorkerMessage::AgentRunFinished {
        profile_id: "review".to_owned(),
        result: sigil_kernel::AgentRunResult {
            final_text: "kernel review complete".to_owned(),
            tool_calls: 0,
            final_message_id: None,
        },
        entries: restored_entries("restored-provider", "restored-model"),
    })?;
    assert!(!app.is_busy);
    assert_eq!(app.run_phase(), RunPhase::Idle);
    assert_eq!(app.last_notice(), Some("agent @review finished"));
    assert!(!app.timeline.iter().any(|entry| {
        entry.role == TimelineRole::Notice && entry.text == "agent @review finished"
    }));
    assert!(app.timeline.iter().any(|entry| {
        entry.role == TimelineRole::Assistant && entry.text == "kernel review complete"
    }));
    assert!(app.events.iter().any(|event| {
        event.label == "agent:finish"
            && event
                .detail
                .contains("review tool_calls=0 final_text_bytes=22")
    }));

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
fn worker_messages_cover_task_start_and_all_finish_status_labels() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());

    app.handle_worker_message(WorkerMessage::TaskRunStarted {
        task_id: "task_1".to_owned(),
        objective: "ship task".to_owned(),
    })?;

    assert_eq!(app.run_phase(), RunPhase::Thinking);
    assert_eq!(app.last_notice(), Some("planning task task_1"));
    assert!(
        app.events
            .iter()
            .any(|event| { event.label == "task:start" && event.detail == "task_1 ship task" })
    );

    for (status, label) in [
        (sigil_kernel::TaskRunStatus::Started, "started"),
        (sigil_kernel::TaskRunStatus::Running, "running"),
        (sigil_kernel::TaskRunStatus::Paused, "paused"),
        (sigil_kernel::TaskRunStatus::Completed, "completed"),
        (sigil_kernel::TaskRunStatus::Failed, "failed"),
        (sigil_kernel::TaskRunStatus::Cancelled, "cancelled"),
        (sigil_kernel::TaskRunStatus::Interrupted, "interrupted"),
    ] {
        app.is_busy = true;
        app.handle_worker_message(WorkerMessage::TaskRunFinished {
            task_id: "task_1".to_owned(),
            status,
            entries: Vec::new(),
        })?;

        assert!(!app.is_busy);
        assert_eq!(app.run_phase(), RunPhase::Idle);
        let expected_notice = format!("task task_1 {label}");
        assert_eq!(app.last_notice(), Some(expected_notice.as_str()));
        assert!(app.events.iter().any(|event| {
            event.label == "task:finish" && event.detail == format!("task_1 status={label}")
        }));
    }

    app.is_busy = true;
    app.handle_worker_message(WorkerMessage::TaskRunFinished {
        task_id: "task_1".to_owned(),
        status: sigil_kernel::TaskRunStatus::Failed,
        entries: vec![sigil_kernel::SessionLogEntry::Control(
            sigil_kernel::ControlEntry::TaskRun(sigil_kernel::TaskRunEntry {
                task_id: sigil_kernel::TaskId::new("task_1")?,
                parent_session_ref: sigil_kernel::SessionRef::new_relative("parent.jsonl")?,
                objective: "ship task".to_owned(),
                status: sigil_kernel::TaskRunStatus::Failed,
                reason: Some("step gate_check failed".to_owned()),
            }),
        )],
    })?;
    assert!(!app.is_busy);
    assert_eq!(
        app.last_notice(),
        Some("task task_1 failed: step gate_check failed")
    );
    Ok(())
}

#[test]
fn worker_control_events_update_task_sidebar_immediately() -> Result<()> {
    let task_id = sigil_kernel::TaskId::new("task_1")?;
    let step_id = sigil_kernel::TaskStepId::new("overview")?;
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());

    app.handle(RunEvent::Control(ControlEntry::TaskRun(
        sigil_kernel::TaskRunEntry {
            task_id: task_id.clone(),
            parent_session_ref: sigil_kernel::SessionRef::new_relative("parent.jsonl")?,
            objective: "review workspace".to_owned(),
            status: sigil_kernel::TaskRunStatus::Running,
            reason: Some("continuing plan v1".to_owned()),
        },
    )))?;
    app.handle(RunEvent::Control(ControlEntry::TaskPlan(
        sigil_kernel::TaskPlanEntry {
            task_id: task_id.clone(),
            plan_version: 1,
            status: sigil_kernel::TaskPlanStatus::Accepted,
            steps: vec![sigil_kernel::TaskStepSpec {
                step_id: step_id.clone(),
                title: "scan workspace".to_owned(),
                display_name: None,
                detail: None,
                role: sigil_kernel::AgentRole::Executor,
            }],
            reason: None,
        },
    )))?;
    app.handle(RunEvent::Control(ControlEntry::TaskStep(
        sigil_kernel::TaskStepEntry {
            task_id,
            plan_version: 1,
            step_id,
            role: sigil_kernel::AgentRole::Executor,
            status: sigil_kernel::TaskStepStatus::Running,
            title: Some("scan workspace".to_owned()),
            summary: None,
            reason: None,
        },
    )))?;

    let lines = app.task_sidebar_lines();

    assert!(lines.contains(&"status: running".to_owned()));
    assert!(lines.contains(&"current: v1:overview running".to_owned()));
    assert!(lines.contains(&"◐ 1. running overview · scan workspace".to_owned()));
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
            final_message_id: None,
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
        !app.timeline
            .iter()
            .any(|entry| entry.role == TimelineRole::Notice && entry.text == "worker note")
    );
    assert!(
        app.events
            .iter()
            .any(|event| event.label == "worker" && event.detail == "worker note")
    );
    app.handle_worker_message(WorkerMessage::Notice("worker failed hard".to_owned()))?;
    assert!(
        app.timeline
            .iter()
            .any(|entry| entry.role == TimelineRole::Notice && entry.text == "worker failed hard")
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
    app.handle(RunEvent::ToolCallCompleted(ToolCall {
        id: "call-agent".to_owned(),
        name: "wait_agent".to_owned(),
        args_json: "{}".to_owned(),
    }))?;
    assert_eq!(app.run_phase(), RunPhase::Tool("wait_agent".to_owned()));
    assert!(
        app.events
            .iter()
            .any(|event| event.label == "tool:complete" && event.detail == "wait_agent call-agent")
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
fn agent_thread_event_updates_only_focused_child_transcript() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    let thread_id = sigil_kernel::AgentThreadId::new("agent_chat_live")?;

    app.handle_worker_message(WorkerMessage::AgentThreadEvent {
        thread_id: thread_id.clone(),
        event: Box::new(RunEvent::TextDelta("main ignored".to_owned())),
    })?;
    assert!(app.active_agent_child_transcript.is_none());

    app.active_agent_view = super::super::AgentView::Child {
        child_task_id: thread_id.as_str().to_owned(),
        child_session_ref: sigil_kernel::SessionRef::new_relative(
            "children/agent_chat_live.jsonl",
        )?,
    };
    app.active_agent_child_transcript = Some(super::super::ActiveAgentChildTranscript {
        path: Path::new("children/agent_chat_live.jsonl").to_path_buf(),
        file_signature: super::super::ChildTranscriptFileSignature::empty(),
        timeline_entries: Vec::new(),
        rendered_body_lines: Vec::new(),
        total_timeline_entries: 0,
        transcript_truncated: false,
        load_error: Some("not written yet".to_owned()),
    });

    app.handle_worker_message(WorkerMessage::AgentThreadEvent {
        thread_id: thread_id.clone(),
        event: Box::new(RunEvent::TextDelta("hel".to_owned())),
    })?;
    app.handle_worker_message(WorkerMessage::AgentThreadEvent {
        thread_id,
        event: Box::new(RunEvent::TextDelta("lo".to_owned())),
    })?;

    let transcript = app
        .active_agent_child_transcript
        .as_ref()
        .expect("focused child transcript should exist");
    assert_eq!(transcript.timeline_entries.len(), 1);
    assert_eq!(transcript.timeline_entries[0].role, TimelineRole::Assistant);
    assert_eq!(transcript.timeline_entries[0].text, "hello");
    assert!(transcript.load_error.is_none());
    assert!(!transcript.rendered_body_lines.is_empty());

    app.handle_worker_message(WorkerMessage::AgentThreadEvent {
        thread_id: sigil_kernel::AgentThreadId::new("agent_chat_other")?,
        event: Box::new(RunEvent::Notice("ignore me".to_owned())),
    })?;
    let transcript = app
        .active_agent_child_transcript
        .as_ref()
        .expect("focused child transcript should remain loaded");
    assert_eq!(transcript.timeline_entries.len(), 1);
    assert!(
        !transcript
            .timeline_entries
            .iter()
            .any(|entry| entry.text.contains("ignore me"))
    );
    Ok(())
}

#[test]
fn agent_thread_event_projects_live_child_event_variants() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    let thread_id = sigil_kernel::AgentThreadId::new("agent_chat_events")?;
    app.active_agent_view = super::super::AgentView::Child {
        child_task_id: thread_id.as_str().to_owned(),
        child_session_ref: sigil_kernel::SessionRef::new_relative(
            "children/agent_chat_events.jsonl",
        )?,
    };

    app.handle_worker_message(WorkerMessage::AgentThreadEvent {
        thread_id: thread_id.clone(),
        event: Box::new(RunEvent::ReasoningDelta("think".to_owned())),
    })?;
    app.handle_worker_message(WorkerMessage::AgentThreadEvent {
        thread_id: thread_id.clone(),
        event: Box::new(RunEvent::ToolCallStarted(ToolCall {
            id: "call-read".to_owned(),
            name: "read_file".to_owned(),
            args_json: "{}".to_owned(),
        })),
    })?;
    app.handle_worker_message(WorkerMessage::AgentThreadEvent {
        thread_id: thread_id.clone(),
        event: Box::new(RunEvent::Notice("after start".to_owned())),
    })?;
    app.handle_worker_message(WorkerMessage::AgentThreadEvent {
        thread_id: thread_id.clone(),
        event: Box::new(RunEvent::ToolCallCompleted(ToolCall {
            id: "call-read".to_owned(),
            name: "read_file".to_owned(),
            args_json: "{}".to_owned(),
        })),
    })?;
    app.handle_worker_message(WorkerMessage::AgentThreadEvent {
        thread_id: thread_id.clone(),
        event: Box::new(RunEvent::Notice("after complete".to_owned())),
    })?;
    app.handle_worker_message(WorkerMessage::AgentThreadEvent {
        thread_id: thread_id.clone(),
        event: Box::new(RunEvent::ToolResult(ToolResult::ok(
            "call-read".to_owned(),
            "read_file".to_owned(),
            "file contents".to_owned(),
            sigil_kernel::ToolResultMeta::default(),
        ))),
    })?;
    app.handle_worker_message(WorkerMessage::AgentThreadEvent {
        thread_id: thread_id.clone(),
        event: Box::new(RunEvent::AssistantMessage(ModelMessage::assistant(
            Some("draft".to_owned()),
            Vec::new(),
        ))),
    })?;
    app.handle_worker_message(WorkerMessage::AgentThreadEvent {
        thread_id: thread_id.clone(),
        event: Box::new(RunEvent::AssistantMessage(ModelMessage::assistant(
            Some("final".to_owned()),
            Vec::new(),
        ))),
    })?;
    app.handle_worker_message(WorkerMessage::AgentThreadEvent {
        thread_id: thread_id.clone(),
        event: Box::new(RunEvent::AssistantMessage(ModelMessage::assistant(
            Some(String::new()),
            Vec::new(),
        ))),
    })?;
    app.handle_worker_message(WorkerMessage::AgentThreadEvent {
        thread_id: thread_id.clone(),
        event: Box::new(RunEvent::Notice("child notice".to_owned())),
    })?;
    app.handle_worker_message(WorkerMessage::AgentThreadEvent {
        thread_id: thread_id.clone(),
        event: Box::new(RunEvent::Notice("child failed hard".to_owned())),
    })?;
    app.handle_worker_message(WorkerMessage::AgentThreadEvent {
        thread_id: thread_id.clone(),
        event: Box::new(RunEvent::TextDelta("after notice".to_owned())),
    })?;
    app.handle_worker_message(WorkerMessage::AgentThreadEvent {
        thread_id: thread_id.clone(),
        event: Box::new(RunEvent::ToolApprovalRequested {
            call: ToolCall {
                id: "call-write".to_owned(),
                name: "write_file".to_owned(),
                args_json: "{}".to_owned(),
            },
            spec: ToolSpec {
                name: "write_file".to_owned(),
                description: "Write".to_owned(),
                input_schema: json!({"type":"object"}),
                category: ToolCategory::File,
                access: ToolAccess::Write,
                preview: ToolPreviewCapability::Required,
            },
            subjects: Vec::new(),
            preview: None,
        }),
    })?;
    app.handle_worker_message(WorkerMessage::AgentThreadEvent {
        thread_id: thread_id.clone(),
        event: Box::new(RunEvent::TextDelta(" after approval".to_owned())),
    })?;
    app.handle_worker_message(WorkerMessage::AgentThreadEvent {
        thread_id: thread_id.clone(),
        event: Box::new(RunEvent::ToolApprovalResolved {
            call_id: "call-write".to_owned(),
            approved: false,
            reason: Some("scope".to_owned()),
        }),
    })?;

    let transcript = app
        .active_agent_child_transcript
        .as_ref()
        .expect("live event should initialize child transcript");
    assert!(transcript.load_error.is_none());
    let entries = transcript
        .timeline_entries
        .iter()
        .map(|entry| (entry.role, entry.text.as_str()))
        .collect::<Vec<_>>();
    assert!(entries.contains(&(TimelineRole::Thinking, "think")));
    assert!(entries.contains(&(TimelineRole::Tool, "Started read_file")));
    assert!(entries.contains(&(TimelineRole::Tool, "Completed read_file")));
    assert!(entries.contains(&(TimelineRole::Tool, "file contents")));
    assert!(
        entries
            .iter()
            .any(|(role, text)| *role == TimelineRole::Assistant && text.starts_with("final"))
    );
    assert!(!entries.contains(&(TimelineRole::Assistant, "draft")));
    assert!(!entries.contains(&(TimelineRole::Notice, "after start")));
    assert!(!entries.contains(&(TimelineRole::Notice, "after complete")));
    assert!(!entries.contains(&(TimelineRole::Notice, "child notice")));
    assert!(entries.contains(&(TimelineRole::Notice, "child failed hard")));
    assert!(entries.contains(&(TimelineRole::Notice, "Approve write_file in child agent")));
    assert!(entries.contains(&(TimelineRole::Notice, "Approval denied for call-write")));
    let entry_count = transcript.timeline_entries.len();

    app.handle_worker_message(WorkerMessage::AgentThreadEvent {
        thread_id: thread_id.clone(),
        event: Box::new(RunEvent::ToolCallArgsDelta {
            id: "call-read".to_owned(),
            delta: "{}".to_owned(),
        }),
    })?;
    app.handle_worker_message(WorkerMessage::AgentThreadEvent {
        thread_id: thread_id.clone(),
        event: Box::new(RunEvent::Usage(UsageStats::default())),
    })?;
    app.handle_worker_message(WorkerMessage::AgentThreadEvent {
        thread_id: thread_id.clone(),
        event: Box::new(RunEvent::ContinuationState(
            sigil_kernel::ProviderContinuationState {
                provider_name: "deepseek".to_owned(),
                state_kind: "reasoning".to_owned(),
                message_id: None,
                opaque_blob: json!({}),
            },
        )),
    })?;
    app.handle_worker_message(WorkerMessage::AgentThreadEvent {
        thread_id,
        event: Box::new(RunEvent::Control(ControlEntry::SessionIdentity {
            provider_name: "deepseek".to_owned(),
            model_name: "deepseek-v4-pro".to_owned(),
        })),
    })?;

    assert_eq!(
        app.active_agent_child_transcript
            .as_ref()
            .expect("transcript should remain loaded")
            .timeline_entries
            .len(),
        entry_count
    );
    Ok(())
}

#[test]
fn worker_queue_status_summarizes_long_prompt() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    let long_prompt = "please inspect ".repeat(8);

    app.handle_worker_message(WorkerMessage::ConversationQueueUpdated {
        items: vec![sigil_kernel::ConversationQueueItemProjection {
            queued: sigil_kernel::ConversationInputQueuedEntry {
                queue_id: sigil_kernel::ConversationInputQueueId::new("queue_long")?,
                target: sigil_kernel::ConversationInputTarget::MainThread,
                kind: sigil_kernel::ConversationInputKind::Chat,
                prompt_hash: "sha256:long".to_owned(),
                prompt: long_prompt,
                reasoning_effort: None,
                created_at_ms: None,
            },
            status: sigil_kernel::ConversationInputStatus::Queued,
            reason: None,
        }],
        paused: false,
        entries: Vec::new(),
    })?;

    let notice = app.last_notice().expect("queue notice should be set");
    assert!(notice.starts_with("queued 1 · next please inspect"));
    assert!(notice.ends_with("..."));
    Ok(())
}

#[test]
fn model_spawned_agent_events_keep_live_phase_on_agent_wait() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    let spawn_call = ToolCall {
        id: "call-spawn".to_owned(),
        name: "spawn_agent".to_owned(),
        args_json: json!({
            "profile_id": "explore",
            "objective": "inspect kernel",
            "prompt": "inspect crates/sigil-kernel"
        })
        .to_string(),
    };

    app.handle(RunEvent::ToolCallCompleted(spawn_call.clone()))?;
    assert_eq!(app.run_phase(), RunPhase::Agent("explore".to_owned()));
    assert_eq!(app.last_notice(), Some("waiting for agent @explore"));

    app.handle(RunEvent::ToolApprovalRequested {
        call: spawn_call.clone(),
        spec: ToolSpec {
            name: "spawn_agent".to_owned(),
            description: "Spawn an agent".to_owned(),
            input_schema: json!({"type": "object"}),
            category: ToolCategory::Agent,
            access: ToolAccess::Execute,
            preview: ToolPreviewCapability::Required,
        },
        subjects: Vec::new(),
        preview: None,
    })?;
    assert_eq!(app.run_phase(), RunPhase::Tool("spawn_agent".to_owned()));

    app.handle(RunEvent::ToolApprovalResolved {
        call_id: "call-spawn".to_owned(),
        approved: true,
        reason: None,
    })?;
    assert_eq!(app.run_phase(), RunPhase::Agent("explore".to_owned()));
    assert_eq!(app.last_notice(), Some("waiting for agent @explore"));

    app.handle(RunEvent::ToolResult(ToolResult::ok(
        "call-spawn".to_owned(),
        "spawn_agent".to_owned(),
        "{}".to_owned(),
        sigil_kernel::ToolResultMeta::default(),
    )))?;
    assert_eq!(app.run_phase(), RunPhase::Thinking);
    Ok(())
}

#[test]
fn chat_agent_thread_start_control_pushes_agent_card_with_background_hint() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    let snapshot_id = sigil_kernel::AgentProfileSnapshotId::new("snapshot_explore_1")?;
    let profile_id = sigil_kernel::AgentProfileId::new("explore")?;
    let thread_id = sigil_kernel::AgentThreadId::new("agent_chat_1")?;

    app.handle(RunEvent::Control(ControlEntry::AgentProfileCaptured(
        sigil_kernel::AgentProfileCapturedEntry {
            snapshot: sigil_kernel::AgentProfileSnapshot {
                snapshot_id: snapshot_id.clone(),
                profile_id: profile_id.clone(),
                source: sigil_kernel::AgentProfileSource::System,
                source_hash: "sha256:source".to_owned(),
                profile_hash: "sha256:profile".to_owned(),
                resolved_tool_scope_hash: "tools".to_owned(),
                resolved_permission_policy_hash: "permissions".to_owned(),
                resolved_mcp_scope_hash: "mcp".to_owned(),
                resolved_skill_hashes: Vec::new(),
                trust_state: sigil_kernel::AgentTrustState::Trusted,
            },
        },
    )))?;

    app.handle(RunEvent::Control(ControlEntry::AgentThreadStarted(
        sigil_kernel::AgentThreadStartedEntry {
            thread_id: thread_id.clone(),
            parent_thread_id: Some(sigil_kernel::AgentThreadId::new("main")?),
            parent_session_ref: sigil_kernel::SessionRef::new_relative("parent.jsonl")?,
            thread_session_ref: sigil_kernel::SessionRef::new_relative(
                "children/agents/agent_chat_1.jsonl",
            )?,
            profile_id: profile_id.clone(),
            profile_snapshot_id: snapshot_id.clone(),
            run_context: sigil_kernel::AgentRunContextSnapshot {
                profile_snapshot_id: snapshot_id,
                provider: "deepseek".to_owned(),
                model: "deepseek-v4-pro".to_owned(),
                reasoning_effort: None,
                workspace_root: sigil_kernel::WorkspaceRootSnapshot::new(".")?,
                effective_tool_scope_hash: "tools".to_owned(),
                effective_permission_policy_hash: "permissions".to_owned(),
                effective_mcp_scope_hash: "mcp".to_owned(),
                provider_capability_hash: "provider".to_owned(),
                model_visible_agent_index_hash: Some("agent-index".to_owned()),
                budget_policy_hash: "budget".to_owned(),
                provider_background_handle_ref: None,
            },
            objective: "inspect kernel".to_owned(),
            prompt_hash: "sha256:prompt".to_owned(),
            invocation_mode: sigil_kernel::AgentInvocationMode::JoinBeforeFinal,
            invocation_source: sigil_kernel::AgentInvocationSource::Chat,
            display_name: Some("kernel-explorer".to_owned()),
            created_at_ms: Some(42),
        },
    )))?;

    assert_eq!(app.run_phase(), RunPhase::Agent("explore".to_owned()));
    assert_eq!(app.last_notice(), Some("waiting for agent @explore"));
    assert!(app.timeline.iter().any(|entry| {
        entry.role == TimelineRole::Tool
            && entry.text.contains("\"tool_name\":\"spawn_agent\"")
            && entry.text.contains("\"thread_id\":\"agent_chat_1\"")
            && entry.text.contains("\"action_hint\":\"Ctrl-B background\"")
    }));

    app.handle(RunEvent::Control(ControlEntry::AgentThreadStatusChanged(
        sigil_kernel::AgentThreadStatusChangedEntry {
            thread_id: thread_id.clone(),
            status: sigil_kernel::AgentThreadStatus::Running,
            reason: Some("agent moved to background".to_owned()),
            updated_at_ms: Some(43),
        },
    )))?;
    assert!(app.timeline.iter().any(|entry| {
        entry.role == TimelineRole::Tool
            && entry.text.contains("\"tool_name\":\"wait_agent\"")
            && entry.text.contains("\"thread_id\":\"agent_chat_1\"")
            && entry
                .text
                .contains("\"reason\":\"agent moved to background\"")
    }));
    assert!(app.events.iter().any(|event| {
        event.label == "agent:status"
            && event.detail.contains(thread_id.as_str())
            && event.detail.contains("Running")
    }));
    app.handle_worker_message(WorkerMessage::AgentThreadStatusLive {
        entry: sigil_kernel::AgentThreadStatusChangedEntry {
            thread_id: thread_id.clone(),
            status: sigil_kernel::AgentThreadStatus::Completed,
            reason: Some("background finished".to_owned()),
            updated_at_ms: Some(44),
        },
    })?;
    assert!(app.current_session_entries.iter().any(|entry| {
        matches!(
            entry,
            SessionLogEntry::Control(ControlEntry::AgentThreadStatusChanged(status))
                if status.thread_id == thread_id
                    && status.status == sigil_kernel::AgentThreadStatus::Completed
        )
    }));
    let rows = app.agent_sidebar_rows();
    assert!(
        rows.iter().any(|row| {
            row.label.contains("kernel")
                && row.detail.contains("completed")
                && row.detail.contains("explore")
                && row.detail.contains("chat")
        }),
        "expected completed explore chat row, got {rows:?}"
    );

    app.handle(RunEvent::Control(ControlEntry::AgentThreadStarted(
        sigil_kernel::AgentThreadStartedEntry {
            thread_id: sigil_kernel::AgentThreadId::new("agent_task_1")?,
            parent_thread_id: Some(sigil_kernel::AgentThreadId::new("main")?),
            parent_session_ref: sigil_kernel::SessionRef::new_relative("parent.jsonl")?,
            thread_session_ref: sigil_kernel::SessionRef::new_relative(
                "children/agents/agent_task_1.jsonl",
            )?,
            profile_id,
            profile_snapshot_id: sigil_kernel::AgentProfileSnapshotId::new("snapshot_task_1")?,
            run_context: sigil_kernel::AgentRunContextSnapshot {
                profile_snapshot_id: sigil_kernel::AgentProfileSnapshotId::new("snapshot_task_1")?,
                provider: "deepseek".to_owned(),
                model: "deepseek-v4-pro".to_owned(),
                reasoning_effort: None,
                workspace_root: sigil_kernel::WorkspaceRootSnapshot::new(".")?,
                effective_tool_scope_hash: "tools".to_owned(),
                effective_permission_policy_hash: "permissions".to_owned(),
                effective_mcp_scope_hash: "mcp".to_owned(),
                provider_capability_hash: "provider".to_owned(),
                model_visible_agent_index_hash: Some("agent-index".to_owned()),
                budget_policy_hash: "budget".to_owned(),
                provider_background_handle_ref: None,
            },
            objective: "task child".to_owned(),
            prompt_hash: "sha256:task-prompt".to_owned(),
            invocation_mode: sigil_kernel::AgentInvocationMode::Background,
            invocation_source: sigil_kernel::AgentInvocationSource::Task,
            display_name: None,
            created_at_ms: Some(44),
        },
    )))?;
    assert!(
        app.events
            .iter()
            .any(|event| event.label == "control" && event.detail.contains("agent_task_1"))
    );
    Ok(())
}

#[test]
fn repeated_pending_wait_agent_results_replace_previous_tool_card() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());

    let first = ToolResult::ok(
        "call-wait-1".to_owned(),
        "wait_agent".to_owned(),
        serde_json::json!({
            "thread_id": "agent_chat_1",
            "status": "running",
            "terminal": false,
            "result_available": false,
            "retry_after_ms": 5000,
            "coalescing_key": "wait_agent:agent_chat_1",
            "next_action": "continue independent parent work"
        })
        .to_string(),
        sigil_kernel::ToolResultMeta {
            details: serde_json::json!({
                "thread_id": "agent_chat_1",
                "status": "running",
                "retry_after_ms": 5000,
                "coalescing_key": "wait_agent:agent_chat_1"
            }),
            ..sigil_kernel::ToolResultMeta::default()
        },
    );
    let second = ToolResult::ok(
        "call-wait-2".to_owned(),
        "wait_agent".to_owned(),
        serde_json::json!({
            "thread_id": "agent_chat_1",
            "status": "running",
            "terminal": false,
            "result_available": false,
            "retry_after_ms": 4200,
            "coalesced": true,
            "polling_throttled": true,
            "coalescing_key": "wait_agent:agent_chat_1",
            "next_action": "wait_agent was called too soon"
        })
        .to_string(),
        sigil_kernel::ToolResultMeta {
            details: serde_json::json!({
                "thread_id": "agent_chat_1",
                "status": "running",
                "retry_after_ms": 4200,
                "coalesced": true,
                "polling_throttled": true,
                "coalescing_key": "wait_agent:agent_chat_1"
            }),
            ..sigil_kernel::ToolResultMeta::default()
        },
    );

    app.handle(RunEvent::ToolResult(first))?;
    app.handle(RunEvent::ToolResult(second))?;

    let wait_cards = app
        .timeline
        .iter()
        .filter(|entry| entry.role == TimelineRole::Tool && entry.text.contains("wait_agent"))
        .collect::<Vec<_>>();
    assert_eq!(wait_cards.len(), 1);
    assert!(wait_cards[0].text.contains("call-wait-2"));
    assert!(wait_cards[0].text.contains("polling_throttled"));
    assert!(
        app.events
            .iter()
            .filter(|event| event.label == "tool:result" && event.detail == "wait_agent ok")
            .count()
            >= 2
    );
    Ok(())
}

#[test]
fn ctrl_b_during_agent_wait_requests_background() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.handle_worker_message(WorkerMessage::AgentRunStarted {
        profile_id: "explore".to_owned(),
        prompt: "inspect kernel".to_owned(),
    })?;

    let action = app.handle_key_event(KeyEvent::new(KeyCode::Char('b'), KeyModifiers::CONTROL))?;

    assert!(matches!(action, Some(AppAction::BackgroundActiveAgent)));
    assert_eq!(app.last_notice(), Some("agent background requested"));
    assert!(
        app.events
            .iter()
            .any(|event| event.label == "agent" && event.detail == "background requested")
    );
    Ok(())
}

#[test]
fn worker_queue_messages_update_live_rows_and_dispatch_user_prompt() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    let queue_id = sigil_kernel::ConversationInputQueueId::new("queue_1").expect("valid queue id");
    let queued = sigil_kernel::ConversationInputQueuedEntry {
        queue_id: queue_id.clone(),
        target: sigil_kernel::ConversationInputTarget::MainThread,
        kind: sigil_kernel::ConversationInputKind::Chat,
        prompt_hash: "sha256:queue".to_owned(),
        prompt: "follow up after current run".to_owned(),
        reasoning_effort: Some(ReasoningEffort::Max),
        created_at_ms: Some(1),
    };
    let entry = SessionLogEntry::Control(ControlEntry::ConversationInputQueued(queued.clone()));

    app.handle_worker_message(WorkerMessage::ConversationQueueUpdated {
        items: vec![sigil_kernel::ConversationQueueItemProjection {
            queued,
            status: sigil_kernel::ConversationInputStatus::Queued,
            reason: None,
        }],
        paused: false,
        entries: vec![entry],
    })?;

    assert_eq!(
        app.last_notice(),
        Some("queued 1 · next follow up after current run")
    );
    assert_eq!(app.composer_queue_rows().len(), 1);
    assert!(app.events.iter().any(|event| {
        event.label == "queue:update" && event.detail.contains("next follow up after current run")
    }));

    app.handle_worker_message(WorkerMessage::ConversationQueueDispatchStarted {
        queue_id: queue_id.clone(),
        prompt: "follow up after current run".to_owned(),
    })?;
    assert!(app.is_busy);
    assert_eq!(app.run_phase(), RunPhase::Thinking);
    assert_eq!(app.last_notice(), Some("running queued input"));
    assert!(app.timeline.iter().any(|entry| {
        entry.role == TimelineRole::User && entry.text == "follow up after current run"
    }));
    assert!(app.events.iter().any(|event| {
        event.label == "queue:dispatch" && event.detail.contains(queue_id.as_str())
    }));

    app.handle_worker_message(WorkerMessage::ConversationQueueUpdated {
        items: Vec::new(),
        paused: true,
        entries: Vec::new(),
    })?;
    assert_eq!(app.last_notice(), Some("queue empty"));
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
        app.into_worker_command(AppAction::QueueConversationInput {
            prompt: "queued draft".to_owned(),
            kind: sigil_kernel::ConversationInputKind::Chat,
            target: sigil_kernel::ConversationInputTarget::MainThread,
        }),
        WorkerCommand::QueueConversationInput {
            prompt,
            kind: sigil_kernel::ConversationInputKind::Chat,
            target: sigil_kernel::ConversationInputTarget::MainThread,
            ..
        } if prompt == "queued draft"
    ));
    let queue_id = sigil_kernel::ConversationInputQueueId::new("queue_1").expect("valid queue id");
    assert!(matches!(
        app.into_worker_command(AppAction::CancelQueuedConversationInput {
            queue_id: queue_id.clone(),
        }),
        WorkerCommand::CancelQueuedConversationInput { queue_id }
            if queue_id.as_str() == "queue_1"
    ));
    assert!(matches!(
        app.into_worker_command(AppAction::EditQueuedConversationInput {
            queue_id: queue_id.clone(),
            prompt: "edited draft".to_owned(),
        }),
        WorkerCommand::EditQueuedConversationInput { queue_id, prompt, .. }
            if queue_id.as_str() == "queue_1" && prompt == "edited draft"
    ));
    assert!(matches!(
        app.into_worker_command(AppAction::MoveQueuedConversationInput {
            queue_id: queue_id.clone(),
            direction: crate::runner::QueueMoveDirection::Up,
        }),
        WorkerCommand::MoveQueuedConversationInput {
            queue_id,
            direction: crate::runner::QueueMoveDirection::Up,
        } if queue_id.as_str() == "queue_1"
    ));
    assert!(matches!(
        app.into_worker_command(AppAction::PromoteQueuedConversationInput {
            queue_id: queue_id.clone(),
        }),
        WorkerCommand::PromoteQueuedConversationInput { queue_id }
            if queue_id.as_str() == "queue_1"
    ));
    assert!(matches!(
        app.into_worker_command(AppAction::SendQueuedConversationInputNow {
            queue_id: queue_id.clone(),
        }),
        WorkerCommand::SendQueuedConversationInputNow { queue_id }
            if queue_id.as_str() == "queue_1"
    ));
    assert!(matches!(
        app.into_worker_command(AppAction::SetConversationQueuePaused { paused: true }),
        WorkerCommand::SetConversationQueuePaused { paused: true }
    ));
    assert!(matches!(
        app.into_worker_command(AppAction::InvokeInlineSkill {
            skill_id: "repo-review".to_owned(),
            arguments: "crates".to_owned(),
        }),
        WorkerCommand::InvokeInlineSkill {
            skill_id,
            arguments,
            ..
        } if skill_id == "repo-review" && arguments == "crates"
    ));
    assert!(matches!(
        app.into_worker_command(AppAction::InvokeChildSessionSkill {
            skill_id: "repo-audit".to_owned(),
            arguments: "--depth full".to_owned(),
        }),
        WorkerCommand::InvokeChildSessionSkill {
            skill_id,
            arguments,
        } if skill_id == "repo-audit" && arguments == "--depth full"
    ));
    assert!(matches!(
        app.into_worker_command(AppAction::InvokeAgentProfile {
            profile_id: "repo-review".to_owned(),
            prompt: "audit crates".to_owned(),
            parent_prompt: "@repo-review audit crates".to_owned(),
        }),
        WorkerCommand::InvokeAgentProfile { profile_id, prompt, parent_prompt }
            if profile_id == "repo-review"
                && prompt == "audit crates"
                && parent_prompt == "@repo-review audit crates"
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
        app.into_worker_command(AppAction::ApprovalDecisionWithArgs {
            call_id: "call-spawn".to_owned(),
            args_json: r#"{"mode":"background"}"#.to_owned(),
        }),
        WorkerCommand::ApprovalDecisionWithArgs { call_id, args_json }
            if call_id == "call-spawn" && args_json.contains("background")
    ));
    assert!(matches!(
        app.into_worker_command(AppAction::BackgroundActiveAgent),
        WorkerCommand::BackgroundActiveAgent
    ));
    assert!(matches!(
        app.into_worker_command(AppAction::CancelRun),
        WorkerCommand::CancelRun
    ));
    assert!(matches!(
        app.into_worker_command(AppAction::CancelTerminalTask {
            task_id: "terminal-1".to_owned(),
        }),
        WorkerCommand::CancelTerminalTask { task_id } if task_id == "terminal-1"
    ));
    assert!(matches!(
        app.into_worker_command(AppAction::CloseAgent {
            thread_id: sigil_kernel::AgentThreadId::new("thread-1")
                .expect("test thread id should be valid"),
            reason: Some("done".to_owned()),
        }),
        WorkerCommand::CloseAgent {
            thread_id,
            reason: Some(reason),
        } if thread_id.as_str() == "thread-1" && reason == "done"
    ));
    assert!(matches!(
        app.into_worker_command(AppAction::MessageAgent {
            thread_id: sigil_kernel::AgentThreadId::new("thread-1")
                .expect("test thread id should be valid"),
            prompt: "keep going".to_owned(),
        }),
        WorkerCommand::MessageAgent { thread_id, prompt }
            if thread_id.as_str() == "thread-1" && prompt == "keep going"
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
        app.into_worker_command(AppAction::StartNewSession {
            session_log_path: std::path::PathBuf::from("session-new.jsonl"),
        }),
        WorkerCommand::StartNewSession { session_log_path }
            if session_log_path == std::path::Path::new("session-new.jsonl")
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
fn terminal_task_updated_syncs_session_and_pushes_tool_card() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.pending_terminal_cancel_confirmation = Some("terminal-1".to_owned());
    let entry = worker_terminal_entry("terminal-1", sigil_kernel::TerminalTaskStatus::Cancelled)?;
    let entries = vec![SessionLogEntry::Control(ControlEntry::TerminalTask(
        entry.clone(),
    ))];

    app.handle_worker_message(WorkerMessage::TerminalTaskUpdated { entry, entries })?;

    assert!(app.pending_terminal_cancel_confirmation.is_none());
    assert_eq!(
        app.last_notice(),
        Some("terminal task terminal-1 cancelled")
    );
    assert!(app.task_sidebar_lines().is_empty());
    let tool_entry = app
        .timeline
        .iter()
        .find(|entry| entry.role == TimelineRole::Tool)
        .expect("expected terminal task card");
    let payload: serde_json::Value = serde_json::from_str(&tool_entry.text)?;
    assert_eq!(payload["tool_name"], "terminal_task");
    assert_eq!(
        payload["metadata"]["details"]["terminal_task"]["status"],
        "cancelled"
    );
    assert!(app.events.iter().any(|event| {
        event.label == "terminal" && event.detail == "terminal-1 status=cancelled"
    }));
    Ok(())
}

#[test]
fn new_session_started_restores_empty_session_view() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.push_timeline(TimelineRole::Assistant, "old context");
    let new_session_log_path = std::path::PathBuf::from(".sigil/sessions/session-new.jsonl");

    app.handle_worker_message(WorkerMessage::NewSessionStarted {
        session_log_path: new_session_log_path.clone(),
        provider_name: "deepseek".to_owned(),
        model_name: "deepseek-v4-pro".to_owned(),
        entries: vec![SessionLogEntry::Control(ControlEntry::SessionIdentity {
            provider_name: "deepseek".to_owned(),
            model_name: "deepseek-v4-pro".to_owned(),
        })],
    })?;

    assert_eq!(app.session_log_path, new_session_log_path);
    assert_eq!(app.model_name, "deepseek-v4-pro");
    assert_eq!(app.last_notice(), Some("started new session"));
    assert!(
        app.timeline
            .iter()
            .any(|entry| entry.role == TimelineRole::Notice && entry.text == "started new session")
    );
    assert!(
        !app.timeline
            .iter()
            .any(|entry| entry.role == TimelineRole::Assistant && entry.text == "old context")
    );
    Ok(())
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
            final_message_id: None,
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

fn worker_terminal_entry(
    task_id: &str,
    status: sigil_kernel::TerminalTaskStatus,
) -> Result<sigil_kernel::TerminalTaskEntry> {
    Ok(sigil_kernel::TerminalTaskEntry {
        handle: sigil_kernel::TerminalTaskHandle {
            task_id: sigil_kernel::TerminalTaskId::new(task_id)?,
            command: "cargo test".to_owned(),
            cwd: Path::new(".").to_path_buf(),
            shell: "sh".to_owned(),
            log_path: Path::new(".sigil/tasks").join(task_id).join("output.log"),
            created_at_ms: 10,
        },
        status,
        output_preview: Some("cancelled output".to_owned()),
        output_hash: Some("hash".to_owned()),
        output_truncated: false,
        updated_at_ms: 20,
    })
}

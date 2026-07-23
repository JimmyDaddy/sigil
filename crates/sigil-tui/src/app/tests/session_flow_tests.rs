use super::*;

fn plain_transcript(app: &AppState, max_lines: usize) -> String {
    app.transcript_lines(max_lines)
        .into_iter()
        .map(|line| {
            line.spans
                .into_iter()
                .map(|span| span.content.into_owned())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn latest_session_can_be_restored_on_launch() -> Result<()> {
    let temp = tempdir()?;
    let config = RootConfig {
        workspace: WorkspaceConfig {
            root: temp.path().display().to_string(),
        },
        ..test_config()
    };
    let session_dir = resolved_session_log_dir(&config, temp.path());
    let restored_path = session_dir.join("session-restored.jsonl");
    write_session_log(
        &restored_path,
        &restored_entries("restored-provider", "restored-model"),
    )?;

    let mut app = AppState::from_root_config(temp.path().join("sigil.toml").as_path(), &config);
    assert!(app.restore_latest_session_from_disk(&config));
    assert_eq!(app.session_log_path, restored_path);
    assert_eq!(app.runtime.provider_name, "restored-provider");
    assert_eq!(app.runtime.model_name, "restored-model");
    assert!(
        app.timeline
            .iter()
            .any(|entry| entry.text == "restored assistant answer")
    );
    assert_eq!(app.last_notice(), Some("restored latest session"));
    Ok(())
}

#[test]
fn restored_tool_result_uses_execution_audit_for_user_facing_card() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    let session_log_path = app.session_log_path.clone();
    let entries = vec![
        SessionLogEntry::Control(ControlEntry::SessionIdentity {
            provider_name: "deepseek".to_owned(),
            model_name: "deepseek-v4-flash".to_owned(),
        }),
        SessionLogEntry::User(ModelMessage::user("read the file")),
        SessionLogEntry::Control(ControlEntry::ToolExecution(Box::new(ToolExecutionEntry {
            call_id: "call-read-1".to_owned(),
            tool_name: "read_file".to_owned(),
            status: ToolExecutionStatus::Completed,
            duration_ms: Some(4),
            subjects: Vec::new(),
            changed_files: Vec::new(),
            metadata: ToolResultMeta {
                bytes: Some(18),
                details: json!({
                    "call": {
                        "summary": "path=README.md"
                    }
                }),
                ..ToolResultMeta::default()
            },
            error: None,
            model_content_hash: Some("hash".to_owned()),
        }))),
        SessionLogEntry::ToolResult(ModelMessage::tool(
            "call-read-1",
            json!({
                "status": "ok",
                "content": "# Title\nbody",
                "meta": {
                    "bytes": 18,
                    "details": {
                        "call": {
                            "summary": "path=README.md"
                        }
                    }
                }
            })
            .to_string(),
        )),
    ];

    app.handle_worker_message(WorkerMessage::SessionSwitched {
        session_log_path,
        provider_name: "deepseek".to_owned(),
        model_name: "deepseek-v4-flash".to_owned(),
        entries,
    })?;

    let rendered = plain_transcript(&app, 20);
    assert!(rendered.contains("Read README.md"));
    assert!(!rendered.contains("path=README.md"));
    assert!(!rendered.contains("tool_result"));
    assert!(!rendered.contains("\"status\":\"ok\""));
    Ok(())
}

#[test]
fn restored_read_file_tool_result_uses_original_tool_call_for_code_preview() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    let session_log_path = app.session_log_path.clone();
    let entries = vec![
        SessionLogEntry::User(ModelMessage::user("read source")),
        SessionLogEntry::Assistant(ModelMessage::assistant_with_kind(
            None,
            vec![ToolCall {
                id: "call-read-rs".to_owned(),
                name: "read_file".to_owned(),
                args_json: json!({"path":"src/main.rs"}).to_string(),
            }],
            AssistantMessageKind::ToolPreamble,
        )),
        SessionLogEntry::ToolResult(ModelMessage::tool(
            "call-read-rs",
            json!({
                "status": "ok",
                "content": "fn main() {}\n",
                "meta": { "bytes": 13 }
            })
            .to_string(),
        )),
    ];

    app.handle_worker_message(WorkerMessage::SessionSwitched {
        session_log_path,
        provider_name: "deepseek".to_owned(),
        model_name: "deepseek-v4-flash".to_owned(),
        entries,
    })?;

    let tool_entry = app
        .timeline
        .iter()
        .find(|entry| entry.role == TimelineRole::Tool)
        .expect("expected restored tool entry");
    let payload: serde_json::Value = serde_json::from_str(&tool_entry.text)?;
    assert_eq!(payload["tool_name"], "read_file");
    assert_eq!(payload["preview_kind"], "code");
    assert_eq!(payload["preview_language"], "rust");
    assert_eq!(
        payload["metadata"]["details"]["call"]["path"],
        "src/main.rs"
    );
    assert!(
        payload["preview_lines"][0]
            .as_str()
            .is_some_and(|line| line.contains("fn main"))
    );
    Ok(())
}

#[test]
fn restored_prefix_snapshot_keeps_large_materialized_text_out_of_activity() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    let session_log_path = app.session_log_path.clone();
    let sentinel = "VERY_LONG_PREFIX_SENTINEL";
    let large_prefix = format!("{sentinel}{}", "x".repeat(64 * 1024));
    let entries = vec![SessionLogEntry::Control(
        ControlEntry::PrefixSnapshotCaptured(sigil_kernel::PrefixSnapshot {
            materialized_text: large_prefix,
            sha256: "abcdef1234567890abcdef1234567890".to_owned(),
            provider_name: "deepseek".to_owned(),
            model_name: "deepseek-v4-flash".to_owned(),
            memory_fingerprint: "memory-fingerprint".to_owned(),
            tool_schema_fingerprint: "tool-fingerprint".to_owned(),
            skill_index_fingerprint: "skill-fingerprint".to_owned(),
        }),
    )];

    app.handle_worker_message(WorkerMessage::SessionSwitched {
        session_log_path,
        provider_name: "deepseek".to_owned(),
        model_name: "deepseek-v4-flash".to_owned(),
        entries,
    })?;

    let restored = app
        .events
        .iter()
        .find(|event| event.label == "control:restore")
        .expect("expected restored control activity");
    assert!(restored.detail.contains("[ctl] prefix"));
    assert!(!restored.detail.contains(sentinel));
    assert!(restored.detail.len() < 160);
    Ok(())
}

#[test]
fn restored_terminal_task_control_renders_user_facing_card() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    let session_log_path = app.session_log_path.clone();
    let entries = vec![SessionLogEntry::Control(ControlEntry::TerminalTask(
        session_terminal_entry("terminal-1", sigil_kernel::TerminalTaskStatus::Running)?,
    ))];

    app.handle_worker_message(WorkerMessage::SessionSwitched {
        session_log_path,
        provider_name: "deepseek".to_owned(),
        model_name: "deepseek-v4-flash".to_owned(),
        entries,
    })?;

    let tool_entry = app
        .timeline
        .iter()
        .find(|entry| entry.role == TimelineRole::Tool)
        .expect("expected restored terminal task card");
    let payload: serde_json::Value = serde_json::from_str(&tool_entry.text)?;
    assert_eq!(payload["tool_name"], "terminal_task");
    assert_eq!(
        payload["metadata"]["details"]["terminal_task"]["task_id"],
        "terminal-1"
    );
    assert!(app.events.iter().any(|event| {
        event.label == "control:restore" && event.detail == "terminal terminal-1 status=running"
    }));
    Ok(())
}

#[test]
fn restored_agent_thread_controls_render_user_facing_cards() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    let session_log_path = app.session_log_path.clone();
    let snapshot_id = sigil_kernel::AgentProfileSnapshotId::new("snapshot_restore_1")?;
    let thread_id = sigil_kernel::AgentThreadId::new("agent_restore_1")?;
    let entries = vec![
        SessionLogEntry::Control(ControlEntry::AgentThreadStarted(
            sigil_kernel::AgentThreadStartedEntry {
                thread_id: thread_id.clone(),
                parent_thread_id: Some(sigil_kernel::AgentThreadId::new("main")?),
                batch_id: None,
                batch_member_key: None,
                parent_session_ref: sigil_kernel::SessionRef::new_relative("parent.jsonl")?,
                thread_session_ref: sigil_kernel::SessionRef::new_relative(
                    "children/agents/agent_restore_1.jsonl",
                )?,
                profile_id: sigil_kernel::AgentProfileId::new("explore")?,
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
                invocation_source: sigil_kernel::AgentInvocationSource::Mention,
                display_name: Some("kernel-explorer".to_owned()),
                created_at_ms: Some(42),
            },
        )),
        SessionLogEntry::Control(ControlEntry::AgentThreadStatusChanged(
            sigil_kernel::AgentThreadStatusChangedEntry {
                thread_id,
                status: sigil_kernel::AgentThreadStatus::Running,
                reason: Some("waiting for result".to_owned()),
                updated_at_ms: Some(43),
            },
        )),
        SessionLogEntry::Assistant(ModelMessage::assistant(
            Some("parent summary".to_owned()),
            Vec::new(),
        )),
    ];

    app.handle_worker_message(WorkerMessage::SessionSwitched {
        session_log_path,
        provider_name: "deepseek".to_owned(),
        model_name: "deepseek-v4-flash".to_owned(),
        entries,
    })?;

    let tool_cards = app
        .timeline
        .iter()
        .filter(|entry| entry.role == TimelineRole::Tool)
        .map(|entry| entry.text.as_str())
        .collect::<Vec<_>>();
    let agent_cards = tool_cards
        .iter()
        .filter(|text| text.contains("\"thread_id\":\"agent_restore_1\""))
        .collect::<Vec<_>>();
    assert_eq!(agent_cards.len(), 1);
    assert!(agent_cards[0].contains("\"tool_name\":\"wait_agent\""));
    assert!(agent_cards[0].contains("\"reason\":\"waiting for result\""));
    assert!(app.events.iter().any(|event| {
        event.label == "control:restore"
            && event.detail.contains("agent_restore_1")
            && event.detail.contains("started")
    }));
    assert!(app.events.iter().any(|event| {
        event.label == "control:restore"
            && event.detail.contains("agent_restore_1")
            && event.detail.contains("Running")
    }));
    assert!(plain_transcript(&app, 20).contains("parent summary"));
    Ok(())
}

#[test]
fn restored_reasoning_notes_render_thinking_block() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    let session_log_path = app.session_log_path.clone();
    let entries = vec![
        SessionLogEntry::Control(ControlEntry::SessionIdentity {
            provider_name: "deepseek".to_owned(),
            model_name: "deepseek-v4-flash".to_owned(),
        }),
        SessionLogEntry::User(ModelMessage::user("analyze")),
        SessionLogEntry::Control(ControlEntry::Note {
            kind: "reasoning_delta".to_owned(),
            data: json!({"delta": "\n  "}),
        }),
        SessionLogEntry::Control(ControlEntry::Note {
            kind: "reasoning_delta".to_owned(),
            data: json!({"delta": "step 1\n"}),
        }),
        SessionLogEntry::Control(ControlEntry::Note {
            kind: "reasoning_delta".to_owned(),
            data: json!({"delta": "step 2"}),
        }),
    ];

    app.handle_worker_message(WorkerMessage::SessionSwitched {
        session_log_path,
        provider_name: "deepseek".to_owned(),
        model_name: "deepseek-v4-flash".to_owned(),
        entries,
    })?;

    let rendered = plain_transcript(&app, 20);
    let thinking_entries = app
        .timeline
        .iter()
        .filter(|entry| entry.role == TimelineRole::Thinking)
        .collect::<Vec<_>>();
    assert_eq!(thinking_entries.len(), 1);
    assert_eq!(thinking_entries[0].text, "step 1\nstep 2");
    assert!(rendered.contains("thought"));
    assert!(!rendered.contains("thinking"));
    assert!(rendered.contains("2 lines"));
    assert!(!rendered.contains("hidden"));
    assert!(!rendered.contains("Ctrl-T expand"));
    assert!(rendered.contains("step 1"));
    assert!(rendered.contains("step 2"));
    Ok(())
}

#[test]
fn restored_interrupted_tool_execution_renders_user_facing_card() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    let session_log_path = app.session_log_path.clone();
    let entries = vec![
        SessionLogEntry::Control(ControlEntry::SessionIdentity {
            provider_name: "deepseek".to_owned(),
            model_name: "deepseek-v4-flash".to_owned(),
        }),
        SessionLogEntry::User(ModelMessage::user("run tests")),
        SessionLogEntry::Control(ControlEntry::ToolExecution(Box::new(ToolExecutionEntry {
            call_id: "call-bash-1".to_owned(),
            tool_name: "bash".to_owned(),
            status: ToolExecutionStatus::Interrupted,
            duration_ms: None,
            subjects: Vec::new(),
            changed_files: Vec::new(),
            metadata: ToolResultMeta {
                details: json!({
                    "call": {
                        "summary": "command=cargo test --workspace"
                    }
                }),
                ..ToolResultMeta::default()
            },
            error: Some(ToolError {
                kind: ToolErrorKind::Interrupted,
                message: "tool execution was interrupted before completion".to_owned(),
                retryable: true,
                details: serde_json::Value::Null,
            }),
            model_content_hash: None,
        }))),
    ];

    app.handle_worker_message(WorkerMessage::SessionSwitched {
        session_log_path,
        provider_name: "deepseek".to_owned(),
        model_name: "deepseek-v4-flash".to_owned(),
        entries,
    })?;

    let rendered = plain_transcript(&app, 20);
    assert!(rendered.contains("Ran cargo test --workspace"));
    assert!(rendered.contains("INTERRUPTED"));
    assert!(!rendered.contains("tool_result"));
    Ok(())
}

#[test]
fn restored_tool_result_uses_preview_snapshot_for_diff_card() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    let session_log_path = app.session_log_path.clone();
    let preview = sample_approval_preview();
    let snapshot = ToolPreviewSnapshot::from_preview(
        "call-write-1",
        "write_file",
        &preview,
        Default::default(),
        Some("preview-hash".to_owned()),
    );
    let entries = vec![
        SessionLogEntry::Control(ControlEntry::SessionIdentity {
            provider_name: "deepseek".to_owned(),
            model_name: "deepseek-v4-flash".to_owned(),
        }),
        SessionLogEntry::User(ModelMessage::user("write note")),
        SessionLogEntry::Control(ControlEntry::ToolPreviewCaptured(snapshot)),
        SessionLogEntry::Control(ControlEntry::ToolExecution(Box::new(ToolExecutionEntry {
            call_id: "call-write-1".to_owned(),
            tool_name: "write_file".to_owned(),
            status: ToolExecutionStatus::Completed,
            duration_ms: Some(4),
            subjects: Vec::new(),
            changed_files: vec!["note.txt".to_owned()],
            metadata: ToolResultMeta {
                bytes: Some(14),
                changed_files: vec!["note.txt".to_owned()],
                details: json!({
                    "call": {
                        "summary": "path=note.txt"
                    }
                }),
                ..ToolResultMeta::default()
            },
            error: None,
            model_content_hash: Some("hash".to_owned()),
        }))),
        SessionLogEntry::ToolResult(ModelMessage::tool(
            "call-write-1",
            json!({
                "status": "ok",
                "content": "wrote note.txt",
                "meta": {
                    "bytes": 14,
                    "changed_files": ["note.txt"]
                }
            })
            .to_string(),
        )),
    ];

    app.handle_worker_message(WorkerMessage::SessionSwitched {
        session_log_path,
        provider_name: "deepseek".to_owned(),
        model_name: "deepseek-v4-flash".to_owned(),
        entries,
    })?;

    let tool_entry = app
        .timeline
        .iter()
        .find(|entry| entry.role == TimelineRole::Tool)
        .expect("expected restored tool entry");
    let rendered: serde_json::Value = serde_json::from_str(&tool_entry.text)?;
    assert_eq!(rendered["tool_name"], "write_file");
    assert!(
        rendered["summary"]
            .as_str()
            .is_some_and(|summary| summary.contains("diff +1 -1"))
    );
    assert_eq!(
        rendered["metadata"]["details"]["call"]["summary"],
        "path=note.txt"
    );
    assert!(
        rendered["diff"]["files"][0]["lines"]
            .as_array()
            .is_some_and(|lines| {
                lines
                    .iter()
                    .any(|line| line.as_str().is_some_and(|text| text == "-beta"))
                    && lines
                        .iter()
                        .any(|line| line.as_str().is_some_and(|text| text == "+gamma"))
            })
    );
    Ok(())
}

#[test]
fn restored_delete_file_tool_result_uses_preview_snapshot_for_diff_card() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    let session_log_path = app.session_log_path.clone();
    let snapshot = ToolPreviewSnapshot::from_preview(
        "call-delete-1",
        "delete_file",
        &sample_delete_approval_preview(),
        Default::default(),
        Some("delete-preview-hash".to_owned()),
    );
    let entries = vec![
        SessionLogEntry::Control(ControlEntry::SessionIdentity {
            provider_name: "deepseek".to_owned(),
            model_name: "deepseek-v4-flash".to_owned(),
        }),
        SessionLogEntry::User(ModelMessage::user("delete note")),
        SessionLogEntry::Control(ControlEntry::ToolPreviewCaptured(snapshot)),
        SessionLogEntry::Control(ControlEntry::ToolExecution(Box::new(ToolExecutionEntry {
            call_id: "call-delete-1".to_owned(),
            tool_name: "delete_file".to_owned(),
            status: ToolExecutionStatus::Completed,
            duration_ms: Some(4),
            subjects: Vec::new(),
            changed_files: vec!["note.txt".to_owned()],
            metadata: ToolResultMeta {
                bytes: Some(11),
                changed_files: vec!["note.txt".to_owned()],
                details: json!({
                    "action": "delete",
                    "call": {
                        "summary": "path=note.txt"
                    }
                }),
                ..ToolResultMeta::default()
            },
            error: None,
            model_content_hash: Some("hash".to_owned()),
        }))),
        SessionLogEntry::ToolResult(ModelMessage::tool(
            "call-delete-1",
            json!({
                "status": "ok",
                "content": "deleted /workspace/note.txt",
                "meta": {
                    "bytes": 11,
                    "changed_files": ["note.txt"],
                    "details": {
                        "action": "delete"
                    }
                }
            })
            .to_string(),
        )),
    ];

    app.handle_worker_message(WorkerMessage::SessionSwitched {
        session_log_path,
        provider_name: "deepseek".to_owned(),
        model_name: "deepseek-v4-flash".to_owned(),
        entries,
    })?;

    let tool_entry = app
        .timeline
        .iter()
        .find(|entry| entry.role == TimelineRole::Tool)
        .expect("expected restored tool entry");
    let rendered: serde_json::Value = serde_json::from_str(&tool_entry.text)?;
    assert_eq!(rendered["tool_name"], "delete_file");
    assert_eq!(rendered["metadata"]["details"]["action"], "delete");
    assert!(
        rendered["summary"]
            .as_str()
            .is_some_and(|summary| summary.contains("diff +0 -2"))
    );
    assert!(
        rendered["diff"]["files"][0]["lines"]
            .as_array()
            .is_some_and(|lines| {
                lines
                    .iter()
                    .any(|line| line.as_str().is_some_and(|text| text == "-alpha"))
                    && lines
                        .iter()
                        .any(|line| line.as_str().is_some_and(|text| text == "-beta"))
            })
    );
    Ok(())
}

#[test]
fn session_sidebar_lines_include_model_and_phase() {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.runtime.run_phase = RunPhase::Thinking;

    let lines = app.session_sidebar_lines();

    assert!(lines.iter().any(|line| line == "provider: deepseek"));
    assert!(lines.iter().any(|line| line == "model: deepseek-v4-flash"));
    assert!(lines.iter().any(|line| line == "effort: max"));
    assert!(lines.iter().any(|line| line == "phase: thinking"));
}

#[test]
fn session_display_title_uses_first_user_prompt() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.composer.input = "Summarize the codebase architecture".to_owned();

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
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.composer.input = "hello from user".to_owned();

    let action = app.submit_input()?;

    assert!(matches!(action, Some(AppAction::SubmitPrompt(_))));
    assert_eq!(
        app.latest_user_prompt_preview(),
        Some("hello from user".to_owned())
    );
    Ok(())
}

#[test]
fn restored_session_view_shows_v2_compaction_invitation_and_restored_prompt_pressure() -> Result<()>
{
    let mut config = test_config();
    config.compaction.context_window_tokens = Some(100);
    config.compaction.soft_threshold_ratio = 0.5;
    config.compaction.hard_threshold_ratio = 0.8;
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &config);
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
        SessionLogEntry::User(ModelMessage::user("latest prompt")),
    ];

    app.handle_worker_message(WorkerMessage::SessionSwitched {
        session_log_path,
        provider_name: "deepseek".to_owned(),
        model_name: "deepseek-v4-flash".to_owned(),
        entries,
    })?;

    let lines = app.approval_preview_lines();
    assert_eq!(app.runtime.compaction_status, "ready");
    assert!(lines.iter().any(|line| line.contains("prompt=65")));
    assert!(
        lines
            .iter()
            .any(|line| line.contains("/compact: inspect the V2 durable fold plan"))
    );
    assert!(
        lines
            .iter()
            .all(|line| !line.contains("Compacted 2 earlier messages"))
    );
    Ok(())
}

#[test]
fn session_view_mode_toggle_switches_between_provider_and_audit() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.handle_worker_message(WorkerMessage::SessionSwitched {
        session_log_path: app.session_log_path.clone(),
        provider_name: app.runtime.provider_name.clone(),
        model_name: app.runtime.model_name.clone(),
        entries: vec![
            SessionLogEntry::Control(ControlEntry::SessionIdentity {
                provider_name: "deepseek".to_owned(),
                model_name: "deepseek-v4-flash".to_owned(),
            }),
            SessionLogEntry::User(ModelMessage::user("latest prompt")),
        ],
    })?;

    let provider_lines = app.approval_preview_lines().join("\n");
    assert!(provider_lines.contains("provider view"));
    assert!(provider_lines.contains("Provider:"));

    app.session_browser.view_mode = super::SessionViewMode::Audit;
    let audit_lines = app.approval_preview_lines().join("\n");
    assert!(audit_lines.contains("audit view"));
    assert!(audit_lines.contains("Audit:"));
    assert!(audit_lines.contains("[user] latest prompt"));
    Ok(())
}

#[test]
fn session_audit_view_shows_tool_egress_summary() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.handle_worker_message(WorkerMessage::SessionSwitched {
        session_log_path: app.session_log_path.clone(),
        provider_name: "deepseek".to_owned(),
        model_name: "deepseek-v4-flash".to_owned(),
        entries: vec![
            SessionLogEntry::Control(ControlEntry::SessionIdentity {
                provider_name: "deepseek".to_owned(),
                model_name: "deepseek-v4-flash".to_owned(),
            }),
            SessionLogEntry::Control(ControlEntry::ToolEgress(Box::new(ToolEgressEntry {
                call_id: "call-mcp-1".to_owned(),
                tool_name: "mcp__fake__echo".to_owned(),
                destination: "mcp:fake".to_owned(),
                operation: "tools/call".to_owned(),
                subjects: vec![ToolSubjectAudit {
                    kind: ToolSubjectKind::McpTool,
                    original: "mcp__fake__echo".to_owned(),
                    normalized: "mcp__fake__echo".to_owned(),
                    canonical_path: None,
                    scope: ToolSubjectScope::Unknown,
                }],
                payload: json!({
                    "server": "fake",
                    "secret_detected": true,
                    "arguments": {"top_level_keys": ["value"]}
                }),
                redacted: true,
            }))),
        ],
    })?;

    app.session_browser.view_mode = super::SessionViewMode::Audit;
    let audit_lines = app.approval_preview_lines().join("\n");

    assert!(audit_lines.contains(
        "[ctl] egress call-mcp-1 mcp__fake__echo dest=mcp:fake op=tools/call redacted=true"
    ));
    assert!(!audit_lines.contains("secret_detected"));
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
    let session_dir = resolved_session_log_dir(&config, temp.path());
    std::fs::create_dir_all(&session_dir)?;
    std::fs::write(session_dir.join("session-alpha.jsonl"), "")?;
    std::fs::write(session_dir.join("session-beta.jsonl"), "")?;

    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &config);
    app.refresh_session_history();
    app.session_browser.history_filter = "b".to_owned();
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
    let session_dir = resolved_session_log_dir(&config, temp.path());
    std::fs::create_dir_all(&session_dir)?;
    let alpha = session_dir.join("session-alpha.jsonl");
    let beta = session_dir.join("session-beta.jsonl");
    std::fs::write(&alpha, "")?;
    std::fs::write(&beta, "")?;

    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &config);
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
    let session_dir = resolved_session_log_dir(&config, temp.path());
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

    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &config);
    app.refresh_session_history();

    assert_eq!(
        app.session_browser
            .history
            .iter()
            .find(|entry| entry.path == session_path)
            .and_then(|entry| entry.title.as_deref()),
        Some("Investigate selector title display")
    );

    app.composer.input = "/resume".to_owned();
    assert!(
        app.slash_selector_rows()
            .iter()
            .any(|(_, description)| { description.contains("Investigate selector title display") })
    );
    Ok(())
}

#[test]
fn session_history_uses_projection_title_from_v2_stream() -> Result<()> {
    let temp = tempdir()?;
    let config = RootConfig {
        workspace: WorkspaceConfig {
            root: temp.path().display().to_string(),
        },
        ..test_config()
    };
    let session_dir = resolved_session_log_dir(&config, temp.path());
    std::fs::create_dir_all(&session_dir)?;
    let session_path = session_dir.join("session-v2-title.jsonl");
    let store = JsonlSessionStore::new(&session_path)?;
    store.append(&SessionLogEntry::User(ModelMessage::user(
        "Projection-backed selector title",
    )))?;

    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &config);
    app.refresh_session_history();

    assert_eq!(
        app.session_browser
            .history
            .iter()
            .find(|entry| entry.path == session_path)
            .and_then(|entry| entry.title.as_deref()),
        Some("Projection-backed selector title")
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
    let session_dir = resolved_session_log_dir(&config, temp.path());
    std::fs::create_dir_all(&session_dir)?;
    let restored_path = session_dir.join("session-restored.jsonl");
    let restored = restored_entries("restored-provider", "restored-model");
    write_session_log(&restored_path, &restored)?;

    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &config);
    app.composer.input = "/resume".to_owned();

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
    let session_dir = resolved_session_log_dir(&config, temp.path());
    std::fs::create_dir_all(&session_dir)?;
    let restored_path = session_dir.join("session-restored.jsonl");
    let restored = restored_entries("restored-provider", "restored-model");
    write_session_log(&restored_path, &restored)?;

    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &config);
    app.composer.input = "/resume 1".to_owned();
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

    assert_eq!(app.runtime.provider_name, "restored-provider");
    assert_eq!(app.runtime.model_name, "restored-model");
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

#[test]
fn refresh_session_history_reads_titles_and_resolves_resume_targets() -> Result<()> {
    let temp = tempdir()?;
    let config = RootConfig {
        workspace: WorkspaceConfig {
            root: temp.path().display().to_string(),
        },
        ..test_config()
    };
    let session_dir = resolved_session_log_dir(&config, temp.path());
    std::fs::create_dir_all(&session_dir)?;

    let current_path = session_dir.join("session-current.jsonl");
    write_session_log(
        &current_path,
        &restored_entries("current-provider", "current-model"),
    )?;
    std::thread::sleep(std::time::Duration::from_millis(10));

    let alpha_path = session_dir.join("session-alpha.jsonl");
    write_session_log(
        &alpha_path,
        &[
            SessionLogEntry::User(ModelMessage::user("alpha title")),
            SessionLogEntry::Assistant(ModelMessage::assistant(
                Some("done".to_owned()),
                Vec::new(),
            )),
        ],
    )?;
    std::thread::sleep(std::time::Duration::from_millis(10));

    let beta_path = session_dir.join("session-beta.jsonl");
    let oversized = "x".repeat(300_000);
    JsonlSessionStore::new(&beta_path)?
        .append(&SessionLogEntry::User(ModelMessage::user("beta plan")))?;
    let mut beta_file = std::fs::OpenOptions::new().append(true).open(&beta_path)?;
    std::io::Write::write_all(&mut beta_file, format!("{oversized}\n").as_bytes())?;

    let mut app = AppState::from_root_config(temp.path().join("sigil.toml").as_path(), &config);
    app.session_log_path = current_path.clone();
    app.refresh_session_history();

    assert!(
        app.session_browser
            .history
            .iter()
            .any(|entry| entry.path == alpha_path && entry.title.as_deref() == Some("alpha title"))
    );
    assert!(
        app.session_browser
            .history
            .iter()
            .any(|entry| entry.path == beta_path && entry.title.as_deref() == Some("beta plan"))
    );

    assert_eq!(app.resolve_resume_target(""), Some(beta_path.clone()));
    assert_eq!(app.resolve_resume_target("latest"), Some(beta_path.clone()));
    assert_eq!(app.resolve_resume_target("1"), Some(beta_path.clone()));
    assert_eq!(
        app.resolve_resume_target(beta_path.to_str().unwrap_or_default()),
        Some(beta_path.clone())
    );
    assert_eq!(
        app.resolve_resume_target("alpha title"),
        Some(alpha_path.clone())
    );

    app.session_browser.history_filter = "beta".to_owned();
    let rows = app.recent_session_rows();
    assert!(matches!(
        rows.first(),
        Some(SessionHistoryRow::SessionHeader { total, .. }) if *total == 1
    ));
    Ok(())
}

#[test]
fn session_view_audit_renders_control_entries() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    let preview = ToolPreviewSnapshot::from_preview(
        "call-write-1",
        "write_file",
        &sample_approval_preview(),
        sigil_kernel::ToolDiffBudget::default(),
        None,
    );
    app.session_browser.current_entries = vec![
        SessionLogEntry::Control(ControlEntry::SessionIdentity {
            provider_name: "deepseek".to_owned(),
            model_name: "deepseek-v4-flash".to_owned(),
        }),
        SessionLogEntry::Control(ControlEntry::ContinuationStateSaved(
            sigil_kernel::ProviderContinuationState {
                provider_name: "deepseek".to_owned(),
                state_kind: "cursor".to_owned(),
                message_id: Some("msg-1".to_owned()),
                opaque_blob: json!({"cursor": "abc"}),
            },
        )),
        SessionLogEntry::Control(ControlEntry::ResponseHandleTracked(
            sigil_kernel::ResponseHandle {
                provider_name: "deepseek".to_owned(),
                response_id: "response-1234567890".to_owned(),
                continuation_cursor: None,
            },
        )),
        SessionLogEntry::Control(ControlEntry::BackgroundTaskTracked(
            sigil_kernel::BackgroundTaskHandle {
                provider_name: "deepseek".to_owned(),
                task_id: "task-1".to_owned(),
                resumable: true,
            },
        )),
        SessionLogEntry::Control(ControlEntry::PrefixSnapshotCaptured(
            sigil_kernel::PrefixSnapshot {
                materialized_text: "system".to_owned(),
                sha256: "abcdef1234567890".to_owned(),
                provider_name: "deepseek".to_owned(),
                model_name: "deepseek-v4-flash".to_owned(),
                memory_fingerprint: "memory-fingerprint".to_owned(),
                tool_schema_fingerprint: "tool-fingerprint".to_owned(),
                skill_index_fingerprint: "skill-fingerprint".to_owned(),
            },
        )),
        SessionLogEntry::Control(ControlEntry::MemorySnapshotCaptured(
            sigil_kernel::MemorySnapshot {
                messages: Vec::new(),
                report: sigil_kernel::MemoryLoadReport {
                    enabled: true,
                    document_count: 2,
                    fingerprint: "memory-fingerprint".to_owned(),
                },
            },
        )),
        SessionLogEntry::Control(ControlEntry::ContextAssemblySkipped(
            sigil_kernel::ContextAssemblySkippedEntry {
                reason: "context item runtime:bad snippet token cost exceeds declared token cost"
                    .to_owned(),
                candidate_count: 2,
                item_ids: vec!["runtime:bad".to_owned()],
            },
        )),
        SessionLogEntry::Control(ControlEntry::UsageSnapshot(UsageStats {
            prompt_tokens: 10,
            completion_tokens: 5,
            cache_hit_tokens: 3,
            cache_miss_tokens: 7,
            input_cost: 0.0,
            output_cost: 0.0,
            cache_savings: 0.0,
            system_fingerprint: None,
        })),
        SessionLogEntry::Control(ControlEntry::ToolApproval(
            sigil_kernel::ToolApprovalEntry {
                action: sigil_kernel::ToolApprovalAuditAction::Requested,
                call_id: "call-write-1".to_owned(),
                tool_name: "write_file".to_owned(),
                access: ToolAccess::Write,
                network_effect: None,
                local_policy_decision: ApprovalMode::Ask,
                network_policy_decision: ApprovalMode::Allow,
                source_policy_decision: ApprovalMode::Allow,
                subjects: Vec::new(),
                operation: None,
                risk: None,
                subject_zones: Vec::new(),
                confirmation: None,
                snapshot_required: false,
                command_permission_matches: Vec::new(),
                policy_decision: ApprovalMode::Ask,
                external_directory_required: false,
                allow_source: None,
                grant_call_id: None,
                user_decision: None,
                reason: None,
                preview_hash: None,
            },
        )),
        SessionLogEntry::Control(ControlEntry::ToolExecution(Box::new(ToolExecutionEntry {
            call_id: "call-write-1".to_owned(),
            tool_name: "write_file".to_owned(),
            status: ToolExecutionStatus::Completed,
            duration_ms: Some(4),
            subjects: Vec::new(),
            changed_files: vec!["note.txt".to_owned()],
            metadata: ToolResultMeta::default(),
            error: None,
            model_content_hash: None,
        }))),
        SessionLogEntry::Control(ControlEntry::ToolEgress(Box::new(ToolEgressEntry {
            call_id: "call-net-1".to_owned(),
            tool_name: "fetch_url".to_owned(),
            destination: "https://example.com/very/long/path".to_owned(),
            operation: "GET /resource".to_owned(),
            subjects: Vec::new(),
            payload: json!({"ok": true}),
            redacted: true,
        }))),
        SessionLogEntry::Control(ControlEntry::ToolPreviewCaptured(preview)),
        SessionLogEntry::Control(ControlEntry::ChangeSetProposed(sigil_kernel::ChangeSet {
            id: sigil_kernel::ChangeSetId::new("change-1")?,
            title: "Update README".to_owned(),
            summary: "Update project overview".to_owned(),
            risk: sigil_kernel::ChangeSetRisk::Low,
            files: Vec::new(),
            validations: Vec::new(),
        })),
        SessionLogEntry::Control(ControlEntry::ChangeSetApplied(
            sigil_kernel::ChangeSetResult {
                id: sigil_kernel::ChangeSetId::new("change-1")?,
                status: sigil_kernel::ChangeSetResultStatus::Applied,
                file_results: Vec::new(),
                message: None,
            },
        )),
        SessionLogEntry::Control(ControlEntry::TerminalTask(
            sigil_kernel::TerminalTaskEntry {
                handle: sigil_kernel::TerminalTaskHandle {
                    task_id: sigil_kernel::TerminalTaskId::new("terminal-1")?,
                    command: "cargo test".to_owned(),
                    cwd: ".".into(),
                    shell: "zsh".to_owned(),
                    log_path: ".sigil/terminal/terminal-1/output.log".into(),
                    created_at_ms: 100,
                    execution_backend: None,
                    execution_backend_capabilities: None,
                    enforcement_backend: None,
                    enforcement_backend_capabilities: None,
                    sandbox_profile: None,
                },
                status: sigil_kernel::TerminalTaskStatus::Running,
                output_preview: Some("running tests".to_owned()),
                output_hash: Some("sha256:abc".to_owned()),
                output_truncated: false,
                output_total_bytes: 0,
                output_limit_bytes: None,
                output_termination_reason: None,
                cleanup: None,
                updated_at_ms: 120,
            },
        )),
        SessionLogEntry::Control(ControlEntry::Note {
            kind: "checkpoint".to_owned(),
            data: serde_json::Value::Null,
        }),
    ];
    app.session_browser.view_mode = SessionViewMode::Audit;

    let rendered = app.session_view_lines().join("\n");
    assert!(rendered.contains("[ctl] session deepseek/deepseek-v4-flash"));
    assert!(rendered.contains("[ctl] cont cursor msg=msg-1"));
    assert!(rendered.contains("[ctl] response response-1234567890"));
    assert!(rendered.contains("[ctl] task task-1"));
    assert!(rendered.contains("[ctl] prefix sha=abcdef1234567890"));
    assert!(rendered.contains("[ctl] memory docs=2 fp=memory-fingerpri"));
    assert!(rendered.contains("[ctl] context skipped candidates=2 items=1 reason=context item"));
    assert!(rendered.contains("[ctl] usage p=10 c=5 hit=3 miss=7"));
    assert!(rendered.contains("[ctl] approval call-write-1 write_file action=requested"));
    assert!(rendered.contains("[ctl] execution call-write-1 write_file status=completed"));
    assert!(rendered.contains("[ctl] egress call-net-1 fetch_url"));
    assert!(rendered.contains("[ctl] preview call-write-1 write_file"));
    assert!(rendered.contains("[ctl] changeset change-1 proposed risk=low files=0 Update README"));
    assert!(rendered.contains("[ctl] changeset change-1 status=applied files=0"));
    assert!(rendered.contains("[ctl] terminal terminal-1 status=running"));
    assert!(rendered.contains("[ctl] note checkpoint"));
    Ok(())
}

#[test]
fn restored_failed_tool_execution_and_reasoning_trace_render_in_session_view() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    let session_log_path = app.session_log_path.clone();
    let entries = vec![
        SessionLogEntry::Control(ControlEntry::SessionIdentity {
            provider_name: "deepseek".to_owned(),
            model_name: "deepseek-v4-flash".to_owned(),
        }),
        SessionLogEntry::Control(ControlEntry::Note {
            kind: "reasoning_trace".to_owned(),
            data: json!({"text": "trace line"}),
        }),
        SessionLogEntry::Control(ControlEntry::ToolExecution(Box::new(ToolExecutionEntry {
            call_id: "call-bash-1".to_owned(),
            tool_name: "bash".to_owned(),
            status: ToolExecutionStatus::Failed,
            duration_ms: Some(2),
            subjects: Vec::new(),
            changed_files: Vec::new(),
            metadata: ToolResultMeta::default(),
            error: None,
            model_content_hash: None,
        }))),
    ];

    app.handle_worker_message(WorkerMessage::SessionSwitched {
        session_log_path,
        provider_name: "deepseek".to_owned(),
        model_name: "deepseek-v4-flash".to_owned(),
        entries,
    })?;

    let thinking_entry = app
        .timeline
        .iter()
        .find(|entry| entry.role == TimelineRole::Thinking)
        .expect("expected restored reasoning entry");
    assert!(thinking_entry.text.contains("trace line"));
    let tool_entry = app
        .timeline
        .iter()
        .find(|entry| entry.role == TimelineRole::Tool)
        .expect("expected restored tool entry");
    let payload: serde_json::Value = serde_json::from_str(&tool_entry.text)?;
    assert_eq!(payload["status"], "error");
    assert!(
        payload["preview_lines"][0]
            .as_str()
            .is_some_and(|line| line.contains("tool execution ended with status failed"))
    );
    Ok(())
}

#[test]
fn restored_reasoning_trace_before_final_answer_stays_visible_as_thinking() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    let session_log_path = app.session_log_path.clone();
    let entries = vec![
        SessionLogEntry::Control(ControlEntry::SessionIdentity {
            provider_name: "deepseek".to_owned(),
            model_name: "deepseek-v4-flash".to_owned(),
        }),
        SessionLogEntry::Control(ControlEntry::Note {
            kind: "reasoning_trace".to_owned(),
            data: json!({"text": "draft summary that should stay visible"}),
        }),
        SessionLogEntry::Assistant(ModelMessage::assistant_with_kind(
            Some("final answer".to_owned()),
            Vec::new(),
            AssistantMessageKind::FinalAnswer,
        )),
    ];

    app.handle_worker_message(WorkerMessage::SessionSwitched {
        session_log_path,
        provider_name: "deepseek".to_owned(),
        model_name: "deepseek-v4-flash".to_owned(),
        entries,
    })?;

    let rendered = plain_transcript(&app, 20);
    assert!(rendered.contains("final answer"));
    assert!(rendered.contains("draft summary that should stay visible"));
    assert_eq!(
        app.timeline
            .iter()
            .filter(|entry| entry.role == TimelineRole::Assistant)
            .count(),
        1
    );
    assert_eq!(
        app.timeline
            .iter()
            .filter(|entry| entry.role == TimelineRole::Thinking)
            .count(),
        1
    );
    Ok(())
}

#[test]
fn restored_reasoning_traces_between_tools_before_final_stay_visible() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    let session_log_path = app.session_log_path.clone();
    let entries = vec![
        SessionLogEntry::Control(ControlEntry::SessionIdentity {
            provider_name: "deepseek".to_owned(),
            model_name: "deepseek-v4-flash".to_owned(),
        }),
        SessionLogEntry::User(ModelMessage::user("inspect and summarize")),
        SessionLogEntry::Control(ControlEntry::Note {
            kind: "reasoning_trace".to_owned(),
            data: json!({"text": "first draft summary that should stay visible"}),
        }),
        SessionLogEntry::Assistant(ModelMessage::assistant_with_kind(
            None,
            vec![ToolCall {
                id: "call-read".to_owned(),
                name: "read_file".to_owned(),
                args_json: json!({"path":"src/lib.rs"}).to_string(),
            }],
            AssistantMessageKind::ToolPreamble,
        )),
        SessionLogEntry::ToolResult(ModelMessage::tool("call-read", "file content")),
        SessionLogEntry::Control(ControlEntry::Note {
            kind: "reasoning_delta".to_owned(),
            data: json!({"delta": "second draft summary that should stay visible"}),
        }),
        SessionLogEntry::Assistant(ModelMessage::assistant_with_kind(
            Some("final answer".to_owned()),
            Vec::new(),
            AssistantMessageKind::FinalAnswer,
        )),
    ];

    app.handle_worker_message(WorkerMessage::SessionSwitched {
        session_log_path,
        provider_name: "deepseek".to_owned(),
        model_name: "deepseek-v4-flash".to_owned(),
        entries,
    })?;

    let rendered = plain_transcript(&app, 20);
    assert!(rendered.contains("final answer"));
    assert!(rendered.contains("first draft summary that should stay visible"));
    assert!(rendered.contains("second draft summary that should stay visible"));
    assert_eq!(
        app.timeline
            .iter()
            .filter(|entry| entry.role == TimelineRole::Thinking)
            .count(),
        2
    );
    assert!(
        app.timeline
            .iter()
            .any(|entry| entry.role == TimelineRole::Tool)
    );
    Ok(())
}

#[test]
fn restored_reasoning_trace_before_agent_poll_tool_does_not_render() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    let session_log_path = app.session_log_path.clone();
    let entries = vec![
        SessionLogEntry::User(ModelMessage::user("wait for child agent")),
        SessionLogEntry::Control(ControlEntry::Note {
            kind: "reasoning_trace".to_owned(),
            data: json!({"text": "Still running. Let me poll again."}),
        }),
        SessionLogEntry::Assistant(ModelMessage::assistant_with_kind(
            None,
            vec![ToolCall {
                id: "call-wait".to_owned(),
                name: "wait_agent".to_owned(),
                args_json: json!({"thread_id":"agent_chat_1"}).to_string(),
            }],
            AssistantMessageKind::ToolPreamble,
        )),
    ];

    app.handle_worker_message(WorkerMessage::SessionSwitched {
        session_log_path,
        provider_name: "deepseek".to_owned(),
        model_name: "deepseek-v4-flash".to_owned(),
        entries,
    })?;

    let rendered = plain_transcript(&app, 20);
    assert!(!rendered.contains("Still running. Let me poll again."));
    assert!(
        !app.timeline
            .iter()
            .any(|entry| entry.role == TimelineRole::Thinking)
    );
    Ok(())
}

#[test]
fn restored_tool_preamble_before_final_answer_does_not_render_as_second_reply() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    let session_log_path = app.session_log_path.clone();
    let entries = vec![
        SessionLogEntry::Control(ControlEntry::SessionIdentity {
            provider_name: "deepseek".to_owned(),
            model_name: "deepseek-v4-flash".to_owned(),
        }),
        SessionLogEntry::Assistant(ModelMessage::assistant_with_kind(
            Some("I will inspect the files first.".to_owned()),
            vec![ToolCall {
                id: "call-read".to_owned(),
                name: "read_file".to_owned(),
                args_json: json!({"path":"src/lib.rs"}).to_string(),
            }],
            AssistantMessageKind::ToolPreamble,
        )),
        SessionLogEntry::ToolResult(ModelMessage::tool("call-read", "file content")),
        SessionLogEntry::Assistant(ModelMessage::assistant_with_kind(
            Some("final answer".to_owned()),
            Vec::new(),
            AssistantMessageKind::FinalAnswer,
        )),
    ];

    app.handle_worker_message(WorkerMessage::SessionSwitched {
        session_log_path,
        provider_name: "deepseek".to_owned(),
        model_name: "deepseek-v4-flash".to_owned(),
        entries,
    })?;

    let rendered = plain_transcript(&app, 20);
    assert!(rendered.contains("final answer"));
    assert!(!rendered.contains("I will inspect the files first."));
    assert_eq!(
        app.timeline
            .iter()
            .filter(|entry| entry.role == TimelineRole::Assistant)
            .count(),
        1
    );
    Ok(())
}

#[test]
fn resolve_resume_target_returns_none_for_ambiguous_query() -> Result<()> {
    let temp = tempdir()?;
    let config = RootConfig {
        workspace: WorkspaceConfig {
            root: temp.path().display().to_string(),
        },
        ..test_config()
    };
    let session_dir = resolved_session_log_dir(&config, temp.path());
    std::fs::create_dir_all(&session_dir)?;
    let alpha = session_dir.join("session-alpha.jsonl");
    let alpha_copy = session_dir.join("session-alpha-copy.jsonl");
    let current = session_dir.join("session-current.jsonl");
    std::fs::write(&alpha, "")?;
    std::fs::write(&alpha_copy, "")?;
    std::fs::write(&current, "")?;

    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &config);
    app.session_log_path = current;
    app.refresh_session_history();

    assert_eq!(app.resolve_resume_target("alpha"), None);
    assert_eq!(app.resolve_resume_target("latest"), Some(alpha_copy));
    Ok(())
}

#[test]
fn resolve_resume_target_returns_none_for_ambiguous_title_query() {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.session_log_path = Path::new("session-current.jsonl").to_path_buf();
    app.session_browser.history = vec![
        crate::sessions::SessionHistoryEntry {
            path: Path::new("session-alpha.jsonl").to_path_buf(),
            label: "session-alpha.jsonl".to_owned(),
            title: Some("restored alpha prompt".to_owned()),
            modified_epoch_secs: 2,
            bytes: 10,
        },
        crate::sessions::SessionHistoryEntry {
            path: Path::new("session-beta.jsonl").to_path_buf(),
            label: "session-beta.jsonl".to_owned(),
            title: Some("restored beta prompt".to_owned()),
            modified_epoch_secs: 1,
            bytes: 10,
        },
    ];
    assert_eq!(app.resolve_resume_target("prompt"), None);
}

#[test]
fn restore_latest_session_returns_false_when_history_is_empty() {
    let config = test_config();
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &config);

    assert!(!app.restore_latest_session_from_disk(&config));
}

#[test]
fn restore_session_path_ignores_non_session_rows_without_raw_decode() -> Result<()> {
    let temp = tempdir()?;
    let config = RootConfig {
        workspace: WorkspaceConfig {
            root: temp.path().display().to_string(),
        },
        ..test_config()
    };
    let invalid_path = temp.path().join(".sigil/sessions/bad.jsonl");
    std::fs::create_dir_all(
        invalid_path
            .parent()
            .expect("session log path should have a parent"),
    )?;
    std::fs::write(&invalid_path, "{\"not\":\"a session entry\"}\n")?;

    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &config);

    assert!(app.restore_session_path_from_disk(
        invalid_path.clone(),
        "fallback-provider",
        "fallback-model",
        "restore failed"
    ));
    assert_eq!(app.session_log_path, invalid_path);
    assert!(
        app.session_browser
            .current_entries
            .iter()
            .all(|entry| matches!(
                entry,
                SessionLogEntry::Control(ControlEntry::SessionIdentity { .. })
            ))
    );
    assert_eq!(app.last_notice(), Some("restore failed"));
    Ok(())
}

fn session_terminal_entry(
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
            execution_backend: None,
            execution_backend_capabilities: None,
            enforcement_backend: None,
            enforcement_backend_capabilities: None,
            sandbox_profile: None,
        },
        status,
        output_preview: Some("running output".to_owned()),
        output_hash: Some("hash".to_owned()),
        output_truncated: false,
        output_total_bytes: 0,
        output_limit_bytes: None,
        output_termination_reason: None,
        cleanup: None,
        updated_at_ms: 20,
    })
}

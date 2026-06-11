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
    let session_dir = temp.path().join(".sigil/sessions");
    let restored_path = session_dir.join("session-restored.jsonl");
    write_session_log(
        &restored_path,
        &restored_entries("restored-provider", "restored-model"),
    )?;

    let mut app = AppState::from_root_config(temp.path().join("sigil.toml").as_path(), &config);
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
    assert!(rendered.contains("thought"));
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
    app.run_phase = RunPhase::Thinking;

    let lines = app.session_sidebar_lines();

    assert!(lines.iter().any(|line| line == "provider: deepseek"));
    assert!(lines.iter().any(|line| line == "model: deepseek-v4-flash"));
    assert!(lines.iter().any(|line| line == "effort: max"));
    assert!(lines.iter().any(|line| line == "phase: thinking"));
}

#[test]
fn session_display_title_uses_first_user_prompt() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
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
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
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
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
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

    app.session_view_mode = super::SessionViewMode::Audit;
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
    let session_dir = temp.path().join(".sigil/sessions");
    std::fs::create_dir_all(&session_dir)?;
    std::fs::write(session_dir.join("session-alpha.jsonl"), "")?;
    std::fs::write(session_dir.join("session-beta.jsonl"), "")?;

    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &config);
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
    let session_dir = temp.path().join(".sigil/sessions");
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
    let session_dir = temp.path().join(".sigil/sessions");
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
    let session_dir = temp.path().join(".sigil/sessions");
    std::fs::create_dir_all(&session_dir)?;
    let restored_path = session_dir.join("session-restored.jsonl");
    let restored = restored_entries("restored-provider", "restored-model");
    write_session_log(&restored_path, &restored)?;

    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &config);
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
    let session_dir = temp.path().join(".sigil/sessions");
    std::fs::create_dir_all(&session_dir)?;
    let restored_path = session_dir.join("session-restored.jsonl");
    let restored = restored_entries("restored-provider", "restored-model");
    write_session_log(&restored_path, &restored)?;

    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &config);
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

#[test]
fn refresh_session_history_reads_titles_and_resolves_resume_targets() -> Result<()> {
    let temp = tempdir()?;
    let config = RootConfig {
        workspace: WorkspaceConfig {
            root: temp.path().display().to_string(),
        },
        ..test_config()
    };
    let session_dir = temp.path().join(".sigil/sessions");
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
    std::fs::write(
        &beta_path,
        format!(
            "{oversized}\n{}\n",
            serde_json::to_string(&SessionLogEntry::User(ModelMessage::user("beta plan")))?
        ),
    )?;

    let mut app = AppState::from_root_config(temp.path().join("sigil.toml").as_path(), &config);
    app.session_log_path = current_path.clone();
    app.refresh_session_history();

    assert!(
        app.session_history
            .iter()
            .any(|entry| entry.path == alpha_path && entry.title.as_deref() == Some("alpha title"))
    );
    assert!(
        app.session_history
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

    app.session_history_filter = "beta".to_owned();
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
    app.current_session_entries = vec![
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
                subjects: Vec::new(),
                policy_decision: ApprovalMode::Ask,
                external_directory_required: false,
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
        SessionLogEntry::Control(ControlEntry::CompactionApplied(CompactionRecord {
            summary: "summary".to_owned(),
            compacted_message_count: 3,
            retained_tail_message_count: 2,
        })),
        SessionLogEntry::Control(ControlEntry::Note {
            kind: "checkpoint".to_owned(),
            data: serde_json::Value::Null,
        }),
    ];
    app.session_view_mode = SessionViewMode::Audit;

    let rendered = app.session_view_lines().join("\n");
    assert!(rendered.contains("[ctl] session deepseek/deepseek-v4-flash"));
    assert!(rendered.contains("[ctl] cont cursor msg=msg-1"));
    assert!(rendered.contains("[ctl] response response-1234567890"));
    assert!(rendered.contains("[ctl] task task-1"));
    assert!(rendered.contains("[ctl] prefix sha=abcdef1234567890"));
    assert!(rendered.contains("[ctl] usage p=10 c=5 hit=3 miss=7"));
    assert!(rendered.contains("[ctl] approval call-write-1 write_file action=requested"));
    assert!(rendered.contains("[ctl] execution call-write-1 write_file status=completed"));
    assert!(rendered.contains("[ctl] egress call-net-1 fetch_url"));
    assert!(rendered.contains("[ctl] preview call-write-1 write_file"));
    assert!(rendered.contains("[ctl] compacted=3 tail=2"));
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

    let rendered = plain_transcript(&app, 20);
    assert!(rendered.contains("trace line"));
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
fn resolve_resume_target_returns_none_for_ambiguous_query() -> Result<()> {
    let temp = tempdir()?;
    let config = RootConfig {
        workspace: WorkspaceConfig {
            root: temp.path().display().to_string(),
        },
        ..test_config()
    };
    let session_dir = temp.path().join(".sigil/sessions");
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
    app.session_history = vec![
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
fn restore_session_path_returns_false_for_invalid_log() -> Result<()> {
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

    assert!(!app.restore_session_path_from_disk(
        invalid_path,
        "fallback-provider",
        "fallback-model",
        "restore failed"
    ));
    assert_ne!(app.last_notice(), Some("restore failed"));
    Ok(())
}

use std::{collections::BTreeMap, path::Path};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use serde_json::json;
use sigil_kernel::{
    AgentConfig, AgentRole, CodeIntelStartup, CodeIntelligenceConfig, CompactionConfig,
    ControlEntry, EventHandler, MemoryConfig, PermissionConfig, RootConfig, RunEvent,
    SessionConfig, SessionLogEntry, SessionRef, TaskId, TaskPlanEntry, TaskPlanStatus,
    TaskRunEntry, TaskRunStatus, TaskStepEntry, TaskStepId, TaskStepSpec, TaskStepStatus,
    ToolAccess, ToolCall, ToolCategory, ToolPreviewCapability, ToolResult, ToolResultMeta,
    ToolSpec, WorkspaceConfig,
};

use super::*;
use crate::runner::WorkerMessage;

fn test_config() -> RootConfig {
    RootConfig {
        workspace: WorkspaceConfig {
            root: ".".to_owned(),
        },
        session: SessionConfig {
            log_dir: ".sigil/sessions".to_owned(),
        },
        agent: AgentConfig {
            provider: "deepseek".to_owned(),
            model: "deepseek-v4-flash".to_owned(),
            max_turns: None,
            tool_timeout_secs: 30,
        },
        permission: PermissionConfig::default(),
        memory: MemoryConfig { enabled: true },
        skills: Default::default(),
        compaction: CompactionConfig::default(),
        code_intelligence: Default::default(),
        terminal: Default::default(),
        task: Default::default(),
        providers: BTreeMap::new(),
        mcp_servers: Vec::new(),
    }
}

#[test]
fn ui_view_model_projects_info_rail_and_composer_state() {
    let app = AppState::from_root_config(Path::new("/tmp/sigil.toml"), &test_config());
    let view_model = UiViewModel::from_app(&app);

    assert_eq!(view_model.composer.mode_label, "Build · agent: main");
    assert_eq!(view_model.composer.provider_name, "deepseek");
    assert_eq!(view_model.composer.model_name, "deepseek-v4-flash");
    assert_eq!(view_model.composer.input_rows, 1);
    assert_eq!(view_model.footer.run_label, "ready");
    assert!(view_model.footer.hints.contains("Enter send"));
    assert!(!view_model.info_rail.workspace_label.is_empty());
    assert!(
        view_model
            .info_rail
            .session_lines
            .iter()
            .any(|line| line == "provider: deepseek")
    );
    assert!(
        view_model
            .info_rail
            .code_lines
            .iter()
            .any(|line| line == "status: off")
    );
    assert!(
        view_model
            .info_rail
            .controls
            .iter()
            .any(|line| line == "F1: keyboard help")
    );
    assert!(
        view_model
            .info_rail
            .controls
            .iter()
            .any(|line| line == "Ctrl-C: quit")
    );
}

#[test]
fn ui_view_model_projects_task_lines_from_durable_entries() -> anyhow::Result<()> {
    let mut app = AppState::from_root_config(Path::new("/tmp/sigil.toml"), &test_config());
    let task_id = TaskId::new("task_1")?;
    let step_id = TaskStepId::new("step_1")?;
    let entries = vec![
        SessionLogEntry::Control(ControlEntry::TaskRun(TaskRunEntry {
            task_id: task_id.clone(),
            parent_session_ref: SessionRef::new_relative("parent.jsonl")?,
            objective: "ship task".to_owned(),
            status: TaskRunStatus::Started,
            reason: None,
        })),
        SessionLogEntry::Control(ControlEntry::TaskPlan(TaskPlanEntry {
            task_id: task_id.clone(),
            plan_version: 1,
            status: TaskPlanStatus::Accepted,
            steps: vec![TaskStepSpec {
                step_id: step_id.clone(),
                title: "implement".to_owned(),
                display_name: None,
                detail: None,
                role: AgentRole::Executor,
            }],
            reason: None,
        })),
        SessionLogEntry::Control(ControlEntry::TaskStep(TaskStepEntry {
            task_id: task_id.clone(),
            plan_version: 1,
            step_id,
            role: AgentRole::Executor,
            status: TaskStepStatus::Running,
            title: Some("implement".to_owned()),
            summary: None,
            reason: None,
        })),
        SessionLogEntry::Control(ControlEntry::TaskRun(TaskRunEntry {
            task_id: task_id.clone(),
            parent_session_ref: SessionRef::new_relative("parent.jsonl")?,
            objective: "ship task".to_owned(),
            status: TaskRunStatus::Running,
            reason: None,
        })),
    ];

    app.handle_worker_message(WorkerMessage::TaskRunFinished {
        task_id: task_id.as_str().to_owned(),
        status: TaskRunStatus::Running,
        entries,
    })?;
    let view_model = UiViewModel::from_app(&app);

    assert!(
        view_model
            .info_rail
            .task_lines
            .contains(&"task: task_1".to_owned())
    );
    assert!(
        view_model
            .info_rail
            .task_lines
            .contains(&"status: running".to_owned())
    );
    assert!(
        view_model
            .info_rail
            .task_lines
            .contains(&"plan: v1".to_owned())
    );
    assert!(
        view_model
            .info_rail
            .task_lines
            .contains(&"progress: 0/1 done".to_owned())
    );
    assert!(
        view_model
            .info_rail
            .task_lines
            .contains(&"current: v1:step_1 running".to_owned())
    );
    assert!(
        view_model
            .info_rail
            .task_lines
            .contains(&"◐ 1. running step_1 · implement".to_owned())
    );
    Ok(())
}

#[test]
fn task_strip_view_model_preserves_task_strip_rows() {
    let view_model =
        TaskStripViewModel::from_task_strip_view(crate::app::task_sidebar::TaskStripView {
            title: "Task task_1".to_owned(),
            detail: "running · v1 · 1/2 done".to_owned(),
            rows: vec![
                crate::app::task_sidebar::TaskStripRow {
                    kind: crate::ui::StatusKind::Success,
                    label: "1. inspect".to_owned(),
                    detail: "completed · step_1".to_owned(),
                    active: false,
                },
                crate::app::task_sidebar::TaskStripRow {
                    kind: crate::ui::StatusKind::Running,
                    label: "2. implement".to_owned(),
                    detail: "running · step_2".to_owned(),
                    active: true,
                },
            ],
        });

    assert_eq!(view_model.title, "Task task_1");
    assert_eq!(view_model.detail, "running · v1 · 1/2 done");
    assert_eq!(view_model.rows.len(), 2);
    assert_eq!(view_model.rows[0].kind, crate::ui::StatusKind::Success);
    assert_eq!(view_model.rows[0].label, "1. inspect");
    assert!(!view_model.rows[0].active);
    assert_eq!(view_model.rows[1].kind, crate::ui::StatusKind::Running);
    assert_eq!(view_model.rows[1].label, "2. implement");
    assert!(view_model.rows[1].active);
}

#[test]
fn ui_view_model_projects_enabled_code_intelligence_status() {
    let mut config = test_config();
    config.code_intelligence = CodeIntelligenceConfig {
        enabled: true,
        startup: CodeIntelStartup::Lazy,
        ..CodeIntelligenceConfig::default()
    };
    let app = AppState::from_root_config(Path::new("/tmp/sigil.toml"), &config);
    let view_model = UiViewModel::from_app(&app);

    assert!(
        view_model
            .info_rail
            .code_lines
            .iter()
            .any(|line| line == "status: lazy")
    );
}

#[test]
fn code_tool_result_updates_code_intelligence_status() -> anyhow::Result<()> {
    let mut app = AppState::from_root_config(Path::new("/tmp/sigil.toml"), &test_config());
    app.handle(RunEvent::ToolResult(ToolResult::ok(
        "call-code",
        "code_symbols",
        "{}",
        ToolResultMeta {
            details: json!({
                "code_intelligence": {
                    "status_line": "ready rust-analyzer"
                }
            }),
            ..ToolResultMeta::default()
        },
    )))?;

    assert_eq!(app.code_intelligence_status, "ready rust-analyzer");
    Ok(())
}

#[test]
fn code_tool_result_projects_per_server_lsp_status_lines() -> anyhow::Result<()> {
    let mut app = AppState::from_root_config(Path::new("/tmp/sigil.toml"), &test_config());
    app.handle(RunEvent::ToolResult(ToolResult::ok(
        "call-code",
        "code_workspace_symbols",
        "{}",
        ToolResultMeta {
            details: json!({
                "code_intelligence": {
                    "status_line": "ready multiple",
                    "servers": [
                        {
                            "server": "rust-analyzer",
                            "languages": ["rust"],
                            "status": "ready"
                        },
                        {
                            "server": "pyright",
                            "languages": ["python"],
                            "status": "degraded missing binary"
                        },
                        {
                            "server": "typescript-language-server",
                            "languages": ["typescript", "javascript"],
                            "status": "missing"
                        },
                        {
                            "server": "gopls",
                            "languages": ["go"],
                            "status": "installed"
                        }
                    ]
                }
            }),
            ..ToolResultMeta::default()
        },
    )))?;

    let view_model = UiViewModel::from_app(&app);

    assert!(
        view_model
            .info_rail
            .code_lines
            .iter()
            .any(|line| line == "rust: ready rust-analyzer")
    );
    assert!(
        view_model
            .info_rail
            .code_lines
            .iter()
            .any(|line| line == "python: degraded missing binary")
    );
    assert!(
        view_model
            .info_rail
            .code_lines
            .iter()
            .any(|line| line == "typescript/javascript: missing typescript-language-server")
    );
    assert!(
        view_model
            .info_rail
            .code_lines
            .iter()
            .any(|line| line == "go: installed gopls")
    );
    Ok(())
}

#[test]
fn code_diagnostics_tool_result_projects_diagnostic_counts() -> anyhow::Result<()> {
    let mut app = AppState::from_root_config(Path::new("/tmp/sigil.toml"), &test_config());
    app.handle(RunEvent::ToolResult(ToolResult::ok(
        "call-code",
        "code_diagnostics",
        json!({
            "diagnostics": [
                { "severity": "error" },
                { "severity": "warning" },
                { "severity": "warning" }
            ]
        })
        .to_string(),
        ToolResultMeta {
            details: json!({
                "code_intelligence": {
                    "status_line": "ready rust-analyzer",
                    "servers": [{
                        "server": "rust-analyzer",
                        "languages": ["rust"],
                        "status": "ready"
                    }]
                }
            }),
            ..ToolResultMeta::default()
        },
    )))?;

    assert_eq!(
        app.code_intelligence_status,
        "diagnostics 1 errors 2 warnings"
    );
    let view_model = UiViewModel::from_app(&app);
    assert!(
        view_model
            .info_rail
            .code_lines
            .iter()
            .any(|line| line == "rust: ready rust-analyzer")
    );
    assert!(
        view_model
            .info_rail
            .code_lines
            .iter()
            .any(|line| line == "diagnostics: 1 errors 2 warnings")
    );
    Ok(())
}

#[test]
fn code_diagnostics_tool_result_projects_latest_file_summaries() -> anyhow::Result<()> {
    let mut app = AppState::from_root_config(Path::new("/tmp/sigil.toml"), &test_config());
    app.handle(RunEvent::ToolResult(ToolResult::ok(
        "call-code",
        "code_diagnostics",
        json!({
            "query": {
                "paths": [
                    "src/clean.rs",
                    "src/error.rs",
                    "src/warn.rs",
                    "src/both.rs",
                    "src/extra.rs"
                ]
            },
            "diagnostics": [
                { "path": "src/error.rs", "severity": "error" },
                { "path": "src/both.rs", "severity": "error" },
                { "path": "src/both.rs", "severity": "warning" },
                { "path": "src/warn.rs", "severity": "warning" }
            ]
        })
        .to_string(),
        ToolResultMeta {
            details: json!({
                "code_intelligence": {
                    "status_line": "ready rust-analyzer",
                    "servers": [{
                        "server": "rust-analyzer",
                        "languages": ["rust"],
                        "status": "ready"
                    }]
                }
            }),
            ..ToolResultMeta::default()
        },
    )))?;

    let view_model = UiViewModel::from_app(&app);
    let code_lines = view_model.info_rail.code_lines;

    assert!(
        code_lines
            .iter()
            .any(|line| line == "latest diagnostics: 5 files")
    );
    assert!(
        code_lines
            .iter()
            .any(|line| line == "src/both.rs: 1 error 1 warning")
    );
    assert!(
        code_lines
            .iter()
            .any(|line| line == "src/error.rs: 1 error")
    );
    assert!(
        code_lines
            .iter()
            .any(|line| line == "src/warn.rs: 1 warning")
    );
    assert!(code_lines.iter().any(|line| line == "+1 more files"));
    assert!(
        code_lines
            .iter()
            .position(|line| line == "src/both.rs: 1 error 1 warning")
            < code_lines
                .iter()
                .position(|line| line == "src/warn.rs: 1 warning")
    );
    Ok(())
}

#[test]
fn composer_view_model_tracks_input_cursor() -> anyhow::Result<()> {
    let mut app = AppState::from_root_config(Path::new("/tmp/sigil.toml"), &test_config());
    app.handle_key_event(KeyEvent::new(KeyCode::Char('你'), KeyModifiers::NONE))?;
    app.handle_key_event(KeyEvent::new(KeyCode::Char('好'), KeyModifiers::NONE))?;

    let view_model = UiViewModel::from_app(&app);

    assert_eq!(view_model.composer.input, "你好");
    assert_eq!(view_model.composer.cursor_position, (4, 0));
    Ok(())
}

#[test]
fn live_panel_view_model_projects_activity_and_transcript_rows() -> anyhow::Result<()> {
    let mut app = AppState::from_root_config(Path::new("/tmp/sigil.toml"), &test_config());
    app.set_terminal_size(120, 30);
    app.handle_key_event(KeyEvent::new(KeyCode::Char('你'), KeyModifiers::NONE))?;
    app.handle_key_event(KeyEvent::new(KeyCode::Char('好'), KeyModifiers::NONE))?;
    let _ = app.submit_input()?;

    let view_model = LivePanelViewModel::from_app(&app, 2);

    assert_eq!(
        view_model.progress,
        Some(LiveProgressViewModel {
            title: "Thinking".to_owned(),
            detail: "reasoning with deepseek-v4-flash".to_owned(),
        })
    );
    assert!(view_model.transcript_lines.len() <= 2);
    Ok(())
}

#[test]
fn live_panel_view_model_projects_tool_action_titles() {
    assert_eq!(
        LiveProgressViewModel::from_parts("tool", "running bash").title,
        "Bash"
    );
    assert_eq!(
        LiveProgressViewModel::from_parts("tool", "running read_file").title,
        "Read"
    );
    assert_eq!(
        LiveProgressViewModel::from_parts("tool", "running glob").title,
        "Inspect"
    );
}

#[test]
fn footer_hints_track_slash_selector_state() -> anyhow::Result<()> {
    let mut app = AppState::from_root_config(Path::new("/tmp/sigil.toml"), &test_config());
    app.handle_key_event(KeyEvent::new(KeyCode::Char('/'), KeyModifiers::NONE))?;

    let view_model = UiViewModel::from_app(&app);

    assert!(view_model.footer.hints.contains("choose"));
    assert!(view_model.footer.hints.contains("Esc close"));
    Ok(())
}

#[test]
fn activity_controls_live_in_info_rail_not_footer() -> anyhow::Result<()> {
    let mut app = AppState::from_root_config(Path::new("/tmp/sigil.toml"), &test_config());
    app.handle(RunEvent::ToolResult(ToolResult::ok(
        "call-1",
        "ls",
        r#"["src/lib.rs"]"#,
        ToolResultMeta::default(),
    )))?;

    let view_model = UiViewModel::from_app(&app);

    assert!(!view_model.footer.hints.contains("Ctrl-T"));
    assert!(!view_model.footer.hints.contains("Alt-J/K switch"));
    assert!(!view_model.footer.hints.contains("Esc back"));
    assert!(view_model.footer.hints.contains("Enter send"));
    assert!(
        view_model
            .info_rail
            .controls
            .iter()
            .any(|hint| hint == "Ctrl-T: toggle activity")
    );
    assert!(
        view_model
            .info_rail
            .controls
            .iter()
            .any(|hint| hint == "Alt-J: next activity")
    );
    assert!(
        !view_model
            .info_rail
            .controls
            .iter()
            .any(|hint| hint == "Ctrl-T: thinking view")
    );
    Ok(())
}

#[test]
fn footer_hints_track_approval_state() -> anyhow::Result<()> {
    let mut app = AppState::from_root_config(Path::new("/tmp/sigil.toml"), &test_config());
    app.handle(RunEvent::ToolApprovalRequested {
        call: ToolCall {
            id: "call-approval".to_owned(),
            name: "read_file".to_owned(),
            args_json: r#"{"path":"README.md"}"#.to_owned(),
        },
        spec: ToolSpec {
            name: "read_file".to_owned(),
            description: "Read file".to_owned(),
            input_schema: json!({"type":"object"}),
            category: ToolCategory::File,
            access: ToolAccess::Read,
            preview: ToolPreviewCapability::None,
        },
        subjects: Vec::new(),
        preview: None,
    })?;

    let view_model = UiViewModel::from_app(&app);

    assert!(view_model.footer.hints.contains("Y allow"));
    assert!(!view_model.footer.hints.contains("Esc interrupt"));
    assert!(
        !view_model
            .info_rail
            .controls
            .iter()
            .any(|hint| hint == "Esc: interrupt")
    );
    Ok(())
}

#[test]
fn info_rail_projects_memory_off_and_agent_rows() {
    let mut config = test_config();
    config.memory.enabled = false;
    let app = AppState::from_root_config(Path::new("/tmp/sigil.toml"), &config);

    let view_model = UiViewModel::from_app(&app);

    assert!(
        view_model
            .info_rail
            .session_lines
            .iter()
            .any(|line| line == "memory: off")
    );
    assert!(
        view_model
            .info_rail
            .agent_lines
            .iter()
            .any(|line| line == "◉ main: ○ current session")
    );
    assert!(
        view_model
            .info_rail
            .agent_lines
            .iter()
            .any(|line| line == "  agents: ◇ no child agents recorded")
    );
}

#[test]
fn footer_view_model_tracks_busy_without_pending_approval() -> anyhow::Result<()> {
    let mut app = AppState::from_root_config(Path::new("/tmp/sigil.toml"), &test_config());
    app.handle_key_event(KeyEvent::new(KeyCode::Char('r'), KeyModifiers::NONE))?;
    let _ = app.submit_input()?;

    let view_model = UiViewModel::from_app(&app);

    assert_eq!(view_model.footer.phase, RunPhase::Thinking);
    assert!(view_model.footer.is_busy);
    assert_eq!(
        view_model.footer.run_label,
        "thinking · reasoning with deepseek-v4-flash"
    );
    assert_eq!(
        view_model.footer.hints,
        "agent: main · Esc interrupt · Ctrl-T details"
    );
    assert_eq!(view_model.composer.phase, RunPhase::Thinking);
    assert_eq!(view_model.composer.reasoning_effort_label, "max");
    Ok(())
}

#[test]
fn footer_view_model_treats_pending_approval_as_blocking_prompt() -> anyhow::Result<()> {
    let mut app = AppState::from_root_config(Path::new("/tmp/sigil.toml"), &test_config());
    app.is_busy = true;
    app.handle(RunEvent::ToolApprovalRequested {
        call: ToolCall {
            id: "call-approval".to_owned(),
            name: "write_file".to_owned(),
            args_json: r#"{"path":"README.md","content":"hello"}"#.to_owned(),
        },
        spec: ToolSpec {
            name: "write_file".to_owned(),
            description: "Write file".to_owned(),
            input_schema: json!({"type":"object"}),
            category: ToolCategory::File,
            access: ToolAccess::Write,
            preview: ToolPreviewCapability::Required,
        },
        subjects: Vec::new(),
        preview: None,
    })?;

    let view_model = UiViewModel::from_app(&app);

    assert!(!view_model.footer.is_busy);
    assert_eq!(
        view_model.footer.run_label,
        "approval · waiting for decision on write_file"
    );
    assert_eq!(
        view_model.footer.hints,
        "agent: main · Y allow · N deny · V diff"
    );
    Ok(())
}

#[test]
fn live_progress_titles_cover_known_custom_and_phase_labels() {
    for (detail, expected) in [
        ("running write_file", "Write"),
        ("running edit_file", "Edit"),
        ("running delete_file", "Delete"),
        ("running grep", "Search"),
        ("running ls", "Inspect"),
        ("running custom-tool_name", "Custom Tool Name"),
        ("running ___", "Tool"),
    ] {
        assert_eq!(
            LiveProgressViewModel::from_parts("tool", detail).title,
            expected
        );
    }

    assert_eq!(
        LiveProgressViewModel::from_parts("streaming", "writing").title,
        "Replying"
    );
    assert_eq!(
        LiveProgressViewModel::from_parts("mcp", "filesystem: Scanning").title,
        "MCP"
    );
    assert_eq!(
        LiveProgressViewModel::from_parts("approval", "waiting").title,
        "Approval"
    );
    assert_eq!(
        LiveProgressViewModel::from_parts("other", "working").title,
        "Working"
    );
}

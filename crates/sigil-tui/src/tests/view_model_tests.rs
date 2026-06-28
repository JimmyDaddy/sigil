use std::{collections::BTreeMap, path::Path};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use serde_json::json;
use sigil_kernel::{
    AgentConfig, AgentRole, CodeIntelStartup, CodeIntelligenceConfig, CompactionConfig,
    ContextBodyRef, ContextInclusionReason, ContextItem, ContextSensitivity, ContextSource,
    ContextTrustLevel, ControlEntry, EventHandler, MemoryConfig, PackedContext, PermissionConfig,
    RootConfig, RunEvent, SessionConfig, SessionLogEntry, SessionRef, TaskId, TaskPlanEntry,
    TaskPlanStatus, TaskRunEntry, TaskRunStatus, TaskStepEntry, TaskStepId, TaskStepSpec,
    TaskStepStatus, ToolAccess, ToolCall, ToolCategory, ToolPreviewCapability, ToolResult,
    ToolResultMeta, ToolSpec, WorkspaceConfig,
};

use super::*;
use crate::runner::WorkerMessage;

fn test_config() -> RootConfig {
    RootConfig {
        workspace: WorkspaceConfig {
            root: ".".to_owned(),
        },
        storage: Default::default(),
        session: SessionConfig {
            log_dir: Some(".sigil/sessions".to_owned()),
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
        execution: Default::default(),
        verification: Default::default(),
        appearance: Default::default(),
        task: Default::default(),
        providers: BTreeMap::new(),
        mcp_servers: Vec::new(),
    }
}

fn context_item(
    id: &str,
    source: ContextSource,
    token_cost: usize,
    inclusion_reason: ContextInclusionReason,
) -> ContextItem {
    ContextItem {
        id: id.to_owned(),
        source,
        source_event_id: None,
        trust_level: ContextTrustLevel::UntrustedRepositoryData,
        sensitivity: ContextSensitivity::Repository,
        egress_decision: None,
        repo_revision: None,
        token_cost,
        score: None,
        inclusion_reason,
        body_ref: ContextBodyRef::inline("context"),
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
fn context_provenance_summary_reports_budget_sources_and_exclusions() {
    let packed = PackedContext {
        max_tokens: 20,
        used_tokens: 13,
        stable_prefix: vec![context_item(
            "system",
            ContextSource::SystemPrompt,
            4,
            ContextInclusionReason::StablePrompt,
        )],
        dynamic_suffix: vec![
            context_item(
                "symbol",
                ContextSource::LspSymbol,
                6,
                ContextInclusionReason::RetrievalHit,
            ),
            context_item(
                "archive",
                ContextSource::SessionArchive,
                3,
                ContextInclusionReason::RetrievalHit,
            ),
        ],
        excluded: vec![
            context_item(
                "secret",
                ContextSource::RepositoryFile,
                2,
                ContextInclusionReason::ExcludedSecret,
            ),
            context_item(
                "overflow",
                ContextSource::SessionArchive,
                8,
                ContextInclusionReason::ExcludedTokenBudget,
            ),
        ],
    };

    let summary = ContextProvenanceSummaryViewModel::from_packed_context(&packed, 2);

    assert_eq!(
        summary.budget_line,
        "context: 13 / 20 tokens · 3 included · 2 excluded"
    );
    assert_eq!(
        summary.top_sources,
        vec![
            "symbol · 1 item(s) · 6 tokens".to_owned(),
            "system · 1 item(s) · 4 tokens".to_owned()
        ]
    );
    assert!(
        summary
            .excluded_summary
            .contains(&"secret · 1 item(s)".to_owned())
    );
    assert!(
        summary
            .excluded_summary
            .contains(&"token budget · 1 item(s)".to_owned())
    );
    assert_eq!(
        summary.warning.as_deref(),
        Some("secret-like context was blocked")
    );
    assert_eq!(summary.recommended_action.as_deref(), Some("review egress"));
    let lines = summary.lines();
    assert!(lines.iter().any(|line| line == "action: review egress"));
}

#[test]
fn context_provenance_summary_keeps_one_recommended_action() {
    let packed = PackedContext {
        max_tokens: 5,
        used_tokens: 0,
        stable_prefix: Vec::new(),
        dynamic_suffix: Vec::new(),
        excluded: vec![
            context_item(
                "untrusted",
                ContextSource::WorkspaceInstruction,
                2,
                ContextInclusionReason::ExcludedUntrustedWorkspace,
            ),
            context_item(
                "overflow",
                ContextSource::RepositoryFile,
                3,
                ContextInclusionReason::ExcludedTokenBudget,
            ),
        ],
    };

    let summary = ContextProvenanceSummaryViewModel::from_packed_context(&packed, 5);

    assert_eq!(
        summary.warning.as_deref(),
        Some("untrusted workspace context was not promoted")
    );
    assert_eq!(summary.recommended_action.as_deref(), Some("review trust"));
    assert_eq!(
        summary
            .lines()
            .into_iter()
            .filter(|line| line.starts_with("action:"))
            .count(),
        1
    );
}

#[test]
fn context_provenance_summary_covers_remaining_source_and_exclusion_labels() {
    let included_sources = [
        ContextSource::UserMessage,
        ContextSource::WorkspaceInstruction,
        ContextSource::ToolObservation,
        ContextSource::DurableEvent,
        ContextSource::EvidenceReceipt,
        ContextSource::MutationEvidence,
        ContextSource::VerificationEvidence,
        ContextSource::LspDiagnostic,
        ContextSource::LspReference,
        ContextSource::CurrentDiff,
        ContextSource::TaskDigest,
        ContextSource::ExtensionProvided,
    ];
    let packed = PackedContext {
        max_tokens: 200,
        used_tokens: included_sources.len(),
        stable_prefix: Vec::new(),
        dynamic_suffix: included_sources
            .iter()
            .enumerate()
            .map(|(index, source)| {
                context_item(
                    &format!("source-{index}"),
                    source.clone(),
                    1,
                    ContextInclusionReason::RetrievalHit,
                )
            })
            .collect(),
        excluded: vec![
            context_item(
                "egress-denied",
                ContextSource::RepositoryFile,
                1,
                ContextInclusionReason::ExcludedEgressDenied,
            ),
            context_item(
                "unsupported",
                ContextSource::RepositoryFile,
                1,
                ContextInclusionReason::ExcludedUnsupported,
            ),
            context_item(
                "other-exclusion",
                ContextSource::RepositoryFile,
                1,
                ContextInclusionReason::StablePrompt,
            ),
        ],
    };

    let summary = ContextProvenanceSummaryViewModel::from_packed_context(&packed, 20);

    for label in [
        "user",
        "workspace instruction",
        "tool",
        "event",
        "evidence",
        "mutation",
        "verification",
        "diagnostic",
        "reference",
        "diff",
        "task digest",
        "extension",
    ] {
        assert!(
            summary
                .top_sources
                .iter()
                .any(|source| source.starts_with(label)),
            "missing context source label {label}"
        );
    }
    for label in ["egress denied", "unsupported", "other"] {
        assert!(
            summary
                .excluded_summary
                .iter()
                .any(|source| source.starts_with(label)),
            "missing exclusion label {label}"
        );
    }
    assert!(summary.warning.is_none());
    assert!(summary.recommended_action.is_none());
}

#[test]
fn context_provenance_summary_recommends_budget_adjustment_for_budget_only_exclusion() {
    let packed = PackedContext {
        max_tokens: 5,
        used_tokens: 5,
        stable_prefix: Vec::new(),
        dynamic_suffix: Vec::new(),
        excluded: vec![context_item(
            "overflow",
            ContextSource::SessionArchive,
            8,
            ContextInclusionReason::ExcludedTokenBudget,
        )],
    };

    let summary = ContextProvenanceSummaryViewModel::from_packed_context(&packed, 5);

    assert!(summary.warning.is_none());
    assert_eq!(
        summary.recommended_action.as_deref(),
        Some("adjust context budget")
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
                depends_on: Vec::new(),
                mode: None,
                isolation: None,
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
fn footer_hints_track_plan_agent_mention_and_agent_panel_states() -> anyhow::Result<()> {
    let mut plan_app = AppState::from_root_config(Path::new("/tmp/sigil.toml"), &test_config());
    plan_app.set_pending_plan_approval_from_text("1. inspect\n2. implement");
    let plan_view = UiViewModel::from_app(&plan_app);
    assert!(plan_view.footer.hints.contains("A ask"));
    assert!(plan_view.footer.hints.contains("W workspace edits"));
    let live_view = LivePanelViewModel::from_app(&plan_app, 4);
    let approval = live_view
        .plan_approval
        .expect("pending plan approval should project");
    assert!(approval.hash.starts_with("sha256:"));
    assert!(approval.hash.len() <= 19);
    assert_eq!(approval.scope_summary, "1. inspect");

    let mut mention_app = AppState::from_root_config(Path::new("/tmp/sigil.toml"), &test_config());
    mention_app.handle_key_event(KeyEvent::new(KeyCode::Char('@'), KeyModifiers::NONE))?;
    let mention_view = UiViewModel::from_app(&mention_app);
    assert!(mention_view.footer.hints.contains("Tab/Enter insert"));

    let mut panel_app = AppState::from_root_config(Path::new("/tmp/sigil.toml"), &test_config());
    let task_id = TaskId::new("task_1")?;
    let step_id = TaskStepId::new("step_1")?;
    let entries = vec![
        SessionLogEntry::Control(ControlEntry::TaskRun(TaskRunEntry {
            task_id: task_id.clone(),
            parent_session_ref: SessionRef::new_relative("parent.jsonl")?,
            objective: "review workspace".to_owned(),
            status: TaskRunStatus::Running,
            reason: None,
        })),
        SessionLogEntry::Control(ControlEntry::TaskPlan(TaskPlanEntry {
            task_id: task_id.clone(),
            plan_version: 1,
            status: TaskPlanStatus::Accepted,
            steps: vec![TaskStepSpec {
                step_id: step_id.clone(),
                title: "inspect".to_owned(),
                display_name: Some("Repo Audit".to_owned()),
                detail: None,
                role: AgentRole::SubagentRead,
                depends_on: Vec::new(),
                mode: None,
                isolation: None,
            }],
            reason: None,
        })),
        SessionLogEntry::Control(ControlEntry::TaskChildSession(
            sigil_kernel::TaskChildSessionEntry {
                task_id: task_id.clone(),
                plan_version: 1,
                step_id,
                child_task_id: TaskId::new("child_1")?,
                child_session_ref: SessionRef::new_relative("children/child_1.jsonl")?,
                role: AgentRole::SubagentRead,
                status: sigil_kernel::TaskChildSessionStatus::Started,
                summary_hash: None,
            },
        )),
    ];
    panel_app.handle_worker_message(WorkerMessage::TaskRunFinished {
        task_id: task_id.as_str().to_owned(),
        status: TaskRunStatus::Running,
        entries,
    })?;
    panel_app.handle_key_event(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE))?;
    let panel_view = UiViewModel::from_app(&panel_app);
    assert!(panel_view.composer.agent_panel_focused);
    assert!(panel_view.footer.hints.contains("Enter switch"));

    let mut queue_app = AppState::from_root_config(Path::new("/tmp/sigil.toml"), &test_config());
    let queue_id = sigil_kernel::ConversationInputQueueId::new("queue_1")?;
    let queued = sigil_kernel::ConversationInputQueuedEntry {
        queue_id: queue_id.clone(),
        target: sigil_kernel::ConversationInputTarget::MainThread,
        kind: sigil_kernel::ConversationInputKind::Chat,
        prompt_hash: "sha256:queue".to_owned(),
        prompt: "queued prompt".to_owned(),
        reasoning_effort: None,
        created_at_ms: Some(1),
    };
    queue_app.handle_worker_message(WorkerMessage::ConversationQueueUpdated {
        items: vec![sigil_kernel::ConversationQueueItemProjection {
            queued: queued.clone(),
            status: sigil_kernel::ConversationInputStatus::Queued,
            reason: None,
        }],
        paused: false,
        entries: vec![SessionLogEntry::Control(
            ControlEntry::ConversationInputQueued(queued),
        )],
    })?;
    queue_app.handle_key_event(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE))?;
    let queue_view = UiViewModel::from_app(&queue_app);
    assert!(queue_view.footer.hints.contains("Queue"));
    assert!(queue_view.footer.hints.contains("Tab action"));
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
fn footer_hints_show_agent_background_shortcut_while_waiting() -> anyhow::Result<()> {
    let mut app = AppState::from_root_config(Path::new("/tmp/sigil.toml"), &test_config());
    app.handle_worker_message(WorkerMessage::AgentRunStarted {
        profile_id: "explore".to_owned(),
        prompt: "inspect kernel".to_owned(),
    })?;

    let view_model = UiViewModel::from_app(&app);

    assert!(view_model.footer.hints.contains("Ctrl-B background"));
    assert!(view_model.footer.hints.contains("Esc interrupt"));
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
        operation: sigil_kernel::ToolOperation::Read,
        risk: sigil_kernel::PermissionRisk::Low,
        subject_zones: Vec::new(),
        confirmation: None,
        snapshot_required: false,
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
        "agent: main · Enter queue next turn · Esc interrupt · Ctrl-T details"
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
        operation: sigil_kernel::ToolOperation::OverwriteFile,
        risk: sigil_kernel::PermissionRisk::Medium,
        subject_zones: Vec::new(),
        confirmation: None,
        snapshot_required: false,
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

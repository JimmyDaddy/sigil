use std::{collections::BTreeMap, path::Path};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use serde_json::json;
use termquill_kernel::{
    AgentConfig, CompactionConfig, EventHandler, MemoryConfig, PermissionConfig, RootConfig,
    RunEvent, SessionConfig, ToolAccess, ToolCall, ToolCategory, ToolPreviewCapability, ToolResult,
    ToolResultMeta, ToolSpec, WorkspaceConfig,
};

use super::*;

fn test_config() -> RootConfig {
    RootConfig {
        workspace: WorkspaceConfig {
            root: ".".to_owned(),
        },
        session: SessionConfig {
            log_dir: ".termquill/sessions".to_owned(),
        },
        agent: AgentConfig {
            provider: "deepseek".to_owned(),
            model: "deepseek-v4-flash".to_owned(),
            max_turns: None,
            tool_timeout_secs: 30,
        },
        permission: PermissionConfig::default(),
        memory: MemoryConfig { enabled: true },
        compaction: CompactionConfig::default(),
        providers: BTreeMap::new(),
        mcp_servers: Vec::new(),
    }
}

#[test]
fn ui_view_model_projects_info_rail_and_composer_state() {
    let app = AppState::from_root_config(Path::new("/tmp/termquill.toml"), &test_config());
    let view_model = UiViewModel::from_app(&app);

    assert_eq!(view_model.composer.mode_label, "Build");
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
fn composer_view_model_tracks_input_cursor() -> anyhow::Result<()> {
    let mut app = AppState::from_root_config(Path::new("/tmp/termquill.toml"), &test_config());
    app.handle_key_event(KeyEvent::new(KeyCode::Char('你'), KeyModifiers::NONE))?;
    app.handle_key_event(KeyEvent::new(KeyCode::Char('好'), KeyModifiers::NONE))?;

    let view_model = UiViewModel::from_app(&app);

    assert_eq!(view_model.composer.input, "你好");
    assert_eq!(view_model.composer.cursor_position, (4, 0));
    Ok(())
}

#[test]
fn live_panel_view_model_projects_activity_and_transcript_rows() -> anyhow::Result<()> {
    let mut app = AppState::from_root_config(Path::new("/tmp/termquill.toml"), &test_config());
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
    let mut app = AppState::from_root_config(Path::new("/tmp/termquill.toml"), &test_config());
    app.handle_key_event(KeyEvent::new(KeyCode::Char('/'), KeyModifiers::NONE))?;

    let view_model = UiViewModel::from_app(&app);

    assert!(view_model.footer.hints.contains("choose"));
    assert!(view_model.footer.hints.contains("Esc close"));
    Ok(())
}

#[test]
fn activity_controls_live_in_info_rail_not_footer() -> anyhow::Result<()> {
    let mut app = AppState::from_root_config(Path::new("/tmp/termquill.toml"), &test_config());
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
    let mut app = AppState::from_root_config(Path::new("/tmp/termquill.toml"), &test_config());
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

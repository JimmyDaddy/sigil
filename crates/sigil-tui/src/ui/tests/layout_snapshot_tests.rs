use std::{collections::BTreeMap, path::Path};

use ratatui::layout::Rect;
use serde_json::json;
use sigil_kernel::{
    AgentConfig, CompactionConfig, MemoryConfig, PermissionConfig, RootConfig, RunEvent,
    SessionConfig, ToolAccess, ToolCall, ToolCategory, ToolPreviewCapability, ToolResult,
    ToolResultMeta, ToolSpec, WorkspaceConfig,
};

use crate::{
    app::AppState,
    approval::{
        ApprovalAction, ApprovalDiffLine, ApprovalDiffLineKind, ApprovalFileRow, ApprovalModalView,
        PendingApproval,
    },
    mouse::HitTarget,
    runner::WorkerMessage,
};

use super::*;

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
        compaction: CompactionConfig::default(),
        code_intelligence: Default::default(),
        providers: BTreeMap::new(),
        mcp_servers: Vec::new(),
    }
}

fn sample_tool_result(call_id: &str, path: &str) -> ToolResult {
    ToolResult::ok(
        call_id,
        "ls",
        json!([path]).to_string(),
        ToolResultMeta {
            returned_entries: Some(1),
            total_entries: Some(1),
            ..ToolResultMeta::default()
        },
    )
}

#[test]
fn layout_snapshot_handles_single_modes_and_approval_modal() -> anyhow::Result<()> {
    let setup_app = AppState::from_setup(
        Path::new("sigil.toml").to_path_buf(),
        Path::new(".").to_path_buf(),
        None,
    );
    let setup = LayoutSnapshot::from_app(Rect::new(0, 0, 120, 20), &setup_app);
    assert_eq!(setup.mode, LayoutMode::Setup);
    assert_eq!(setup.live_panel, Rect::default());
    assert_eq!(setup.hit_target(1, 1), HitTarget::Background);

    let mut config_app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    config_app.input = "/config".to_owned();
    let _ = config_app.submit_input()?;
    let config = LayoutSnapshot::from_app(Rect::new(0, 0, 120, 20), &config_app);
    assert_eq!(config.mode, LayoutMode::Config);
    assert_eq!(config.composer, Rect::default());

    let mut approval_app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    approval_app.pending_approval = Some(PendingApproval {
        call: ToolCall {
            id: "call-approval".to_owned(),
            name: "write_file".to_owned(),
            args_json: "{}".to_owned(),
        },
        spec: ToolSpec {
            name: "write_file".to_owned(),
            description: "write file".to_owned(),
            input_schema: json!({}),
            category: ToolCategory::File,
            access: ToolAccess::Write,
            preview: ToolPreviewCapability::Optional,
        },
        subjects: Vec::new(),
        preview: None,
    });
    let approval = LayoutSnapshot::from_app(Rect::new(0, 0, 120, 20), &approval_app);
    let modal = approval
        .approval_modal
        .expect("approval modal should render");
    assert_eq!(
        approval.hit_target(modal.x, modal.y),
        HitTarget::ApprovalModal
    );
    Ok(())
}

#[test]
fn layout_snapshot_exposes_live_text_rows() {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.set_terminal_size(120, 20);
    app.handle_worker_message(WorkerMessage::Event(Box::new(RunEvent::AssistantMessage(
        sigil_kernel::ModelMessage::assistant(Some("copy me".to_owned()), Vec::new()),
    ))))
    .expect("message should be handled");
    let layout = LayoutSnapshot::from_app(Rect::new(0, 0, 120, 20), &app);
    let row = layout
        .live_text_rows
        .first()
        .expect("expected visible text row");

    assert_eq!(
        layout.live_text_line_at(row.area.x, row.area.y),
        Some(row.line_index)
    );
}

#[test]
fn layout_snapshot_hits_approval_file_rows_and_actions() {
    let mut approval_app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    approval_app.pending_approval = Some(PendingApproval {
        call: ToolCall {
            id: "call-approval".to_owned(),
            name: "write_file".to_owned(),
            args_json: "{}".to_owned(),
        },
        spec: ToolSpec {
            name: "write_file".to_owned(),
            description: "write file".to_owned(),
            input_schema: json!({}),
            category: ToolCategory::File,
            access: ToolAccess::Write,
            preview: ToolPreviewCapability::Optional,
        },
        subjects: Vec::new(),
        preview: Some(sigil_kernel::ToolPreview {
            title: "Update files".to_owned(),
            summary: "summary".to_owned(),
            body: String::new(),
            changed_files: vec!["a.txt".to_owned(), "b.txt".to_owned()],
            file_diffs: vec![
                sigil_kernel::ToolPreviewFile {
                    path: "a.txt".to_owned(),
                    diff: "@@ -1 +1 @@\n-a\n+b".to_owned(),
                },
                sigil_kernel::ToolPreviewFile {
                    path: "b.txt".to_owned(),
                    diff: "@@ -1 +1 @@\n-x\n+y".to_owned(),
                },
            ],
        }),
    });
    let approval = LayoutSnapshot::from_app(Rect::new(0, 0, 120, 24), &approval_app);
    let hit_areas = approval
        .approval_modal_hit_areas
        .as_ref()
        .expect("expected approval hit areas");
    let second_file = hit_areas.file_rows[1];

    assert_eq!(
        approval.hit_target(second_file.area.x, second_file.area.y),
        HitTarget::ApprovalFileRow { index: 1 }
    );
    assert_eq!(
        approval.hit_target(hit_areas.allow_action.x, hit_areas.allow_action.y),
        HitTarget::ApprovalAction { approved: true }
    );
    assert_eq!(
        approval.hit_target(hit_areas.deny_action.x, hit_areas.deny_action.y),
        HitTarget::ApprovalAction { approved: false }
    );
}

#[test]
fn approval_modal_area_uses_widest_content_with_screen_cap() {
    let view = ApprovalModalView {
        tool_name: "write_file".to_owned(),
        call_id: "call-1".to_owned(),
        access_label: "write".to_owned(),
        preview_title: "Extremely wide preview title for approval layout sizing".to_owned(),
        preview_summary: "summary".to_owned(),
        metadata_collapsed: false,
        file_rows: Vec::new(),
        changed_files: Vec::new(),
        diff_mode_label: "full",
        active_hunk_index: 0,
        hunk_total: 0,
        diff_label: "preview".to_owned(),
        diff_lines: vec![ApprovalDiffLine {
            text: "a very long diff line that should force the dialog width to expand".to_owned(),
            kind: ApprovalDiffLineKind::Context,
            active_hunk: false,
        }],
        selected_action: ApprovalAction::Deny,
    };

    let area = approval_modal_area(Rect::new(0, 0, 90, 24), &view);
    assert!(area.width <= 84);
    assert!(area.width >= 74);
    assert!(area.x > 0);
    assert!(area.y > 0);
}

#[test]
fn slash_overlay_helpers_cover_zero_width_resume_title_and_candidates() -> anyhow::Result<()> {
    assert_eq!(
        slash_selector_overlay_rect(Rect::new(0, 0, 10, 4), Rect::new(9, 2, 1, 2), 3),
        None
    );
    assert_eq!(
        slash_selector_overlay_rect(Rect::new(0, 2, 20, 4), Rect::new(0, 3, 20, 3), 4),
        Some(Rect::new(1, 2, 18, 4))
    );

    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.set_terminal_size(120, 20);
    app.input = "/resume".to_owned();

    let layout = LayoutSnapshot::from_app(Rect::new(0, 0, 120, 20), &app);
    let slash = layout.slash_overlay.expect("slash overlay should exist");

    assert_eq!(slash.title_rows, 1);
    assert_eq!((slash.window_start, slash.window_end), (0, 0));
    assert_eq!(slash.candidate_at(slash.content.x, slash.content.y), None);
    assert_eq!(
        layout.hit_target(slash.overlay.x, slash.overlay.y),
        HitTarget::SlashOverlay
    );
    Ok(())
}

#[test]
fn tool_card_hit_areas_cover_zero_area_and_busy_progress_rows() -> anyhow::Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.set_terminal_size(96, 18);
    app.handle_worker_message(WorkerMessage::Event(Box::new(RunEvent::ToolResult(
        sample_tool_result("call-first", "src/lib.rs"),
    ))))?;
    app.handle_worker_message(WorkerMessage::Event(Box::new(RunEvent::ToolResult(
        sample_tool_result("call-second", "src/main.rs"),
    ))))?;

    assert!(tool_card_hit_areas(Rect::new(0, 0, 0, 0), &app).is_empty());

    let live_area = Rect::new(0, 0, 72, 10);
    let idle = tool_card_hit_areas(live_area, &app);
    assert!(!idle.is_empty());
    assert!(
        idle.iter()
            .all(|hit| hit.area.width > 0 && hit.area.height > 0)
    );

    app.is_busy = true;
    assert!(app.live_activity_summary().is_some());
    let busy = tool_card_hit_areas(live_area, &app);
    assert_eq!(busy.len(), idle.len());
    assert!(
        busy.iter()
            .all(|hit| hit.area.width > 0 && hit.area.height > 0)
    );
    Ok(())
}

#[test]
fn live_text_row_hit_areas_cover_empty_and_tiny_regions() {
    let empty_app = AppState::from_setup(
        Path::new("sigil.toml").to_path_buf(),
        Path::new(".").to_path_buf(),
        None,
    );
    assert!(live_text_row_hit_areas(Rect::new(0, 0, 1, 10), &empty_app).is_empty());
}

#[test]
fn visible_timeline_rows_returns_none_when_progress_consumes_capacity() -> anyhow::Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.set_terminal_size(120, 20);
    app.handle_worker_message(WorkerMessage::Event(Box::new(RunEvent::AssistantMessage(
        sigil_kernel::ModelMessage::assistant(Some("visible".to_owned()), Vec::new()),
    ))))?;
    app.pending_approval = Some(PendingApproval {
        call: ToolCall {
            id: "call-approval".to_owned(),
            name: "write_file".to_owned(),
            args_json: "{}".to_owned(),
        },
        spec: ToolSpec {
            name: "write_file".to_owned(),
            description: "write file".to_owned(),
            input_schema: json!({}),
            category: ToolCategory::File,
            access: ToolAccess::Write,
            preview: ToolPreviewCapability::Optional,
        },
        subjects: Vec::new(),
        preview: None,
    });

    assert_eq!(visible_timeline_rows(Rect::new(0, 0, 80, 2), &app), None);
    Ok(())
}

#[test]
fn approval_hit_area_helpers_cover_compact_empty_and_selected_variants() {
    let mut view = ApprovalModalView {
        tool_name: "write_file".to_owned(),
        call_id: "call-1".to_owned(),
        access_label: "write".to_owned(),
        preview_title: "Title".to_owned(),
        preview_summary: "summary".to_owned(),
        metadata_collapsed: true,
        file_rows: vec![ApprovalFileRow {
            path: "note.txt".to_owned(),
            selected: true,
            diagnostics: None,
        }],
        changed_files: Vec::new(),
        diff_mode_label: "full",
        active_hunk_index: 0,
        hunk_total: 0,
        diff_label: "preview".to_owned(),
        diff_lines: vec![ApprovalDiffLine {
            text: "diff".to_owned(),
            kind: ApprovalDiffLineKind::Context,
            active_hunk: false,
        }],
        selected_action: ApprovalAction::Allow,
    };

    assert_eq!(approval_header_line_count(&view), 4);
    assert!(approval_file_row_hit_areas(Rect::new(0, 0, 20, 1), &view).is_empty());
    assert_eq!(
        approval_action_hit_areas(Rect::new(0, 0, 0, 0), ApprovalAction::Allow),
        (Rect::default(), Rect::default())
    );

    view.metadata_collapsed = false;
    view.preview_summary = "line one\nline two\nline three".to_owned();
    assert_eq!(approval_header_line_count(&view), 5);
    assert!(approval_modal_hit_areas(Rect::new(0, 0, 4, 4), &view).is_some());
}

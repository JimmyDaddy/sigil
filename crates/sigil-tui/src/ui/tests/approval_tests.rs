use std::{collections::BTreeMap, path::Path};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{Terminal, backend::TestBackend, style::Color};
use serde_json::json;
use sigil_kernel::{
    AgentConfig, CompactionConfig, EventHandler, MemoryConfig, PermissionConfig, RootConfig,
    RunEvent, SessionConfig, ToolAccess, ToolCall, ToolCategory, ToolPreview,
    ToolPreviewCapability, ToolPreviewFile, ToolSpec, WorkspaceConfig,
};

use crate::app::AppState;

use super::*;

#[test]
fn render_approval_file_row_includes_diagnostic_summary() {
    let row = ApprovalFileRow {
        path: "src/lib.rs".to_owned(),
        selected: true,
        diagnostics: Some(ApprovalDiagnosticSummary {
            errors: 1,
            warnings: 2,
        }),
    };

    let line = render_approval_file_row(0, &row);
    let text = plain_line_text(&line);

    assert!(text.contains("src/lib.rs"));
    assert!(text.contains("1 error 2 warnings"));
}

#[test]
fn approval_diff_status_line_includes_selected_file_diagnostics() {
    let view = ApprovalModalView {
        tool_name: "edit_file".to_owned(),
        call_id: "call-1".to_owned(),
        access_label: "file write".to_owned(),
        preview_title: "Edit src/lib.rs".to_owned(),
        preview_summary: "summary".to_owned(),
        metadata_collapsed: false,
        file_rows: vec![
            ApprovalFileRow {
                path: "src/other.rs".to_owned(),
                selected: false,
                diagnostics: Some(ApprovalDiagnosticSummary {
                    errors: 3,
                    warnings: 0,
                }),
            },
            ApprovalFileRow {
                path: "src/lib.rs".to_owned(),
                selected: true,
                diagnostics: Some(ApprovalDiagnosticSummary {
                    errors: 0,
                    warnings: 1,
                }),
            },
        ],
        changed_files: vec!["src/lib.rs".to_owned()],
        diff_mode_label: "full",
        active_hunk_index: 1,
        hunk_total: 2,
        diff_label: "src/lib.rs".to_owned(),
        diff_lines: Vec::new(),
        selected_action: ApprovalAction::Deny,
    };

    let text = plain_line_text(&approval_diff_status_line(&view));

    assert!(text.contains("diagnostics 1 warning"));
    assert!(!text.contains("3 errors"));
}

#[test]
fn approval_header_lines_cover_hidden_empty_and_markdown_summary_states() {
    let base = ApprovalModalView {
        tool_name: "edit_file".to_owned(),
        call_id: "call-1".to_owned(),
        access_label: "file write".to_owned(),
        preview_title: "Edit src/lib.rs".to_owned(),
        preview_summary: "summary".to_owned(),
        metadata_collapsed: false,
        file_rows: Vec::new(),
        changed_files: vec!["src/lib.rs".to_owned()],
        diff_mode_label: "full",
        active_hunk_index: 0,
        hunk_total: 0,
        diff_label: "src/lib.rs".to_owned(),
        diff_lines: Vec::new(),
        selected_action: ApprovalAction::Allow,
    };

    let hidden = approval_header_lines(
        &ApprovalModalView {
            metadata_collapsed: true,
            ..base.clone()
        },
        40,
    );
    let empty = approval_header_lines(
        &ApprovalModalView {
            preview_summary: "   ".to_owned(),
            ..base.clone()
        },
        40,
    );
    let markdown = approval_header_lines(
        &ApprovalModalView {
            preview_summary: "**bold** line\n`code` line\nthird line".to_owned(),
            ..base
        },
        24,
    );

    let hidden_text = plain_lines_text(&hidden);
    let empty_text = plain_lines_text(&empty);
    let markdown_text = plain_lines_text(&markdown);

    assert!(hidden_text.contains("meta hidden"));
    assert!(hidden_text.contains("press M to expand"));
    assert!(empty_text.contains("No preview summary provided."));
    assert!(markdown_text.contains("bold line"));
    assert!(markdown_text.contains("code line"));
    assert!(!markdown_text.contains("third line"));
}

#[test]
fn approval_footer_lines_include_file_navigation_hint_only_for_multiple_files() {
    let single = ApprovalModalView {
        tool_name: "edit_file".to_owned(),
        call_id: "call-1".to_owned(),
        access_label: "file write".to_owned(),
        preview_title: "Edit src/lib.rs".to_owned(),
        preview_summary: String::new(),
        metadata_collapsed: false,
        file_rows: vec![ApprovalFileRow {
            path: "src/lib.rs".to_owned(),
            selected: true,
            diagnostics: None,
        }],
        changed_files: vec!["src/lib.rs".to_owned()],
        diff_mode_label: "full",
        active_hunk_index: 0,
        hunk_total: 0,
        diff_label: "src/lib.rs".to_owned(),
        diff_lines: Vec::new(),
        selected_action: ApprovalAction::Allow,
    };
    let multiple = ApprovalModalView {
        file_rows: vec![
            ApprovalFileRow {
                path: "src/lib.rs".to_owned(),
                selected: true,
                diagnostics: None,
            },
            ApprovalFileRow {
                path: "src/main.rs".to_owned(),
                selected: false,
                diagnostics: None,
            },
        ],
        ..single.clone()
    };

    let single_text = plain_lines_text(&approval_footer_lines(&single));
    let multiple_text = plain_lines_text(&approval_footer_lines(&multiple));

    assert!(!single_text.contains(",/. file"));
    assert!(multiple_text.contains(",/. file"));
}

#[test]
fn approval_diff_line_and_diagnostics_helpers_cover_edge_states() {
    assert_eq!(
        approval_diagnostics_label(ApprovalDiagnosticSummary {
            errors: 0,
            warnings: 0,
        }),
        "clean"
    );
    assert_eq!(
        approval_diagnostics_style(ApprovalDiagnosticSummary {
            errors: 0,
            warnings: 1,
        })
        .fg,
        Some(Color::Yellow)
    );
    assert_eq!(
        approval_diagnostics_style(ApprovalDiagnosticSummary {
            errors: 1,
            warnings: 0,
        })
        .fg,
        Some(Color::LightRed)
    );

    let active = render_approval_diff_line(
        ApprovalDiffLine {
            kind: ApprovalDiffLineKind::Added,
            text: String::new(),
            active_hunk: true,
        },
        None,
        Some(7),
        2,
    );
    let inactive = render_approval_diff_line(
        ApprovalDiffLine {
            kind: ApprovalDiffLineKind::Removed,
            text: "- old".to_owned(),
            active_hunk: false,
        },
        Some(4),
        None,
        2,
    );

    assert_eq!(active.spans[0].content.as_ref(), ">");
    assert_eq!(active.spans[0].style.bg, Some(Color::Yellow));
    assert_eq!(active.spans[5].content.as_ref(), " ");
    assert_eq!(inactive.spans[0].content.as_ref(), "│");
    assert_eq!(inactive.spans[5].content.as_ref(), "- old");
}

fn plain_line_text(line: &Line<'static>) -> String {
    line.spans
        .iter()
        .map(|span| span.content.as_ref())
        .collect::<String>()
}

fn plain_lines_text(lines: &[Line<'static>]) -> String {
    lines
        .iter()
        .map(plain_line_text)
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn approval_header_lines_use_access_badges_and_hidden_metadata_hint() {
    let write_view = modal_view("file write");
    let read_view = modal_view("file read");

    let write_lines = approval_header_lines(&write_view, 80);
    let read_lines = approval_header_lines(&read_view, 80);
    let hidden_text = plain_line_text(
        &approval_header_lines(
            &ApprovalModalView {
                metadata_collapsed: true,
                changed_files: vec!["src/lib.rs".to_owned(), "src/main.rs".to_owned()],
                ..modal_view("mcp network")
            },
            80,
        )[2],
    );

    assert_eq!(write_lines[0].spans[0].style.bg, Some(Color::Yellow));
    assert_eq!(read_lines[0].spans[0].style.bg, Some(Color::Green));
    assert!(hidden_text.contains("meta hidden"));
    assert!(hidden_text.contains("press M to expand"));
}

#[test]
fn approval_header_lines_handle_empty_and_multiline_summaries() {
    let empty = approval_header_lines(
        &ApprovalModalView {
            preview_summary: "  \n".to_owned(),
            ..modal_view("file read")
        },
        80,
    );
    let multiline = approval_header_lines(
        &ApprovalModalView {
            preview_summary: "line one\nline two\nline three".to_owned(),
            ..modal_view("file read")
        },
        80,
    );

    assert_eq!(plain_line_text(&empty[2]), "No preview summary provided.");
    assert_eq!(multiline.len(), 5);
    assert!(plain_line_text(&multiline[2]).contains("line one"));
    assert!(plain_line_text(&multiline[3]).contains("line two"));
    assert!(
        !multiline
            .iter()
            .any(|line| plain_line_text(line).contains("line three"))
    );
}

#[test]
fn approval_footer_lines_only_show_file_navigation_for_multiple_files() {
    let single = approval_footer_lines(&modal_view("file read"));
    let multi = approval_footer_lines(&ApprovalModalView {
        file_rows: vec![
            ApprovalFileRow {
                path: "src/lib.rs".to_owned(),
                selected: true,
                diagnostics: None,
            },
            ApprovalFileRow {
                path: "src/main.rs".to_owned(),
                selected: false,
                diagnostics: None,
            },
        ],
        ..modal_view("file read")
    });

    assert!(!plain_line_text(&single[1]).contains(",/. file"));
    assert!(plain_line_text(&multi[1]).contains(",/. file"));
}

#[test]
fn approval_action_badge_marks_only_selected_action() {
    let selected = approval_action_badge("Allow", Color::Green, true);
    let unselected = approval_action_badge("Deny", Color::Red, false);

    assert!(selected.content.contains("▶ Allow"));
    assert_eq!(selected.style.bg, Some(Color::Green));
    assert_eq!(unselected.style.bg, Some(Color::Red));
    assert!(!unselected.content.contains('▶'));
}

#[test]
fn approval_diff_status_line_handles_empty_hunks_without_diagnostics() {
    let text = plain_line_text(&approval_diff_status_line(&ApprovalModalView {
        diff_label: "remote_tool".to_owned(),
        file_rows: vec![ApprovalFileRow {
            path: "remote_tool".to_owned(),
            selected: false,
            diagnostics: Some(ApprovalDiagnosticSummary {
                errors: 2,
                warnings: 0,
            }),
        }],
        hunk_total: 0,
        active_hunk_index: 9,
        ..modal_view("mcp network")
    }));

    assert!(text.contains("hunk 0/0"));
    assert!(!text.contains("diagnostics"));
}

#[test]
fn approval_diagnostics_helpers_cover_clean_warning_and_error_states() {
    let clean = ApprovalDiagnosticSummary::default();
    let warnings = ApprovalDiagnosticSummary {
        errors: 0,
        warnings: 2,
    };
    let errors = ApprovalDiagnosticSummary {
        errors: 1,
        warnings: 1,
    };

    assert_eq!(approval_diagnostics_label(clean), "clean");
    assert_eq!(approval_diagnostics_label(warnings), "2 warnings");
    assert_eq!(approval_diagnostics_label(errors), "1 error 1 warning");
    assert_eq!(approval_diagnostics_style(clean).fg, Some(Color::Green));
    assert_eq!(approval_diagnostics_style(warnings).fg, Some(Color::Yellow));
    assert_eq!(approval_diagnostics_style(errors).fg, Some(Color::LightRed));
}

#[test]
fn approval_count_label_uses_singular_and_plural_forms() {
    assert_eq!(count_label(1, "warning", "warnings"), "1 warning");
    assert_eq!(count_label(3, "warning", "warnings"), "3 warnings");
}

#[test]
fn render_approval_diff_line_highlights_active_hunks_and_blank_text() {
    let active = render_approval_diff_line(
        ApprovalDiffLine {
            text: String::new(),
            kind: ApprovalDiffLineKind::Removed,
            active_hunk: true,
        },
        Some(7),
        None,
        2,
    );
    let inactive = render_approval_diff_line(
        ApprovalDiffLine {
            text: "+added".to_owned(),
            kind: ApprovalDiffLineKind::Added,
            active_hunk: false,
        },
        None,
        Some(8),
        2,
    );

    assert_eq!(active.spans[0].content.as_ref(), ">");
    assert_eq!(active.spans[0].style.bg, Some(Color::Yellow));
    assert_eq!(active.spans[1].style.fg, Some(Color::Rgb(226, 103, 110)));
    assert!(
        active.spans[1]
            .style
            .add_modifier
            .contains(ratatui::style::Modifier::BOLD)
    );
    assert_eq!(active.spans[3].style.fg, Some(Color::DarkGray));
    assert_eq!(active.spans[5].content.as_ref(), " ");
    assert_eq!(active.spans[5].style.bg, Some(Color::Rgb(58, 52, 18)));

    assert_eq!(inactive.spans[0].content.as_ref(), "│");
    assert_eq!(inactive.spans[1].style.fg, Some(Color::DarkGray));
    assert_eq!(inactive.spans[3].style.fg, Some(Color::Rgb(80, 200, 132)));
    assert!(
        inactive.spans[3]
            .style
            .add_modifier
            .contains(ratatui::style::Modifier::BOLD)
    );
    assert_eq!(inactive.spans[5].content.as_ref(), "+added");
}

#[test]
fn approval_diff_line_kind_maps_every_variant() {
    assert_eq!(
        approval_diff_line_kind(ApprovalDiffLineKind::Header),
        DiffLineKind::Header
    );
    assert_eq!(
        approval_diff_line_kind(ApprovalDiffLineKind::Hunk),
        DiffLineKind::Hunk
    );
    assert_eq!(
        approval_diff_line_kind(ApprovalDiffLineKind::Added),
        DiffLineKind::Added
    );
    assert_eq!(
        approval_diff_line_kind(ApprovalDiffLineKind::Removed),
        DiffLineKind::Removed
    );
    assert_eq!(
        approval_diff_line_kind(ApprovalDiffLineKind::Context),
        DiffLineKind::Context
    );
}

#[test]
fn render_approval_modal_renders_file_list_diff_and_actions() -> anyhow::Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.handle(RunEvent::ToolApprovalRequested {
        call: ToolCall {
            id: "call-approval".to_owned(),
            name: "write_file".to_owned(),
            args_json: r#"{"path":"src/lib.rs"}"#.to_owned(),
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
        preview: Some(multi_file_preview()),
    })?;
    app.code_intelligence_diagnostics_by_path.insert(
        "src/lib.rs".to_owned(),
        ApprovalDiagnosticSummary {
            errors: 0,
            warnings: 1,
        },
    );
    let backend = TestBackend::new(140, 32);
    let mut terminal = Terminal::new(backend)?;

    terminal.draw(|frame| render_approval_modal(frame, &app))?;

    let rendered = rendered_content(&terminal);
    assert!(rendered.contains("Review Tool Call"));
    assert!(rendered.contains("Files 1/2"));
    assert!(rendered.contains("src/lib.rs"));
    assert!(rendered.contains("Allow"));
    assert!(rendered.contains("Deny"));
    Ok(())
}

#[test]
fn render_approval_modal_uses_hidden_metadata_and_preview_fallback() -> anyhow::Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.handle(RunEvent::ToolApprovalRequested {
        call: ToolCall {
            id: "call-remote".to_owned(),
            name: "remote_tool".to_owned(),
            args_json: r#"{"query":"status"}"#.to_owned(),
        },
        spec: ToolSpec {
            name: "remote_tool".to_owned(),
            description: "Remote tool".to_owned(),
            input_schema: json!({"type":"object"}),
            category: ToolCategory::Mcp,
            access: ToolAccess::Network,
            preview: ToolPreviewCapability::None,
        },
        subjects: Vec::new(),
        preview: None,
    })?;
    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Char('m'), KeyModifiers::NONE))?;
    let backend = TestBackend::new(100, 24);
    let mut terminal = Terminal::new(backend)?;

    terminal.draw(|frame| render_approval_modal(frame, &app))?;

    let rendered = rendered_content(&terminal);
    assert!(rendered.contains("Run remote_tool"));
    assert!(rendered.contains("meta hidden"));
    assert!(rendered.contains("No structured diff preview available."));
    assert!(!rendered.contains("Files 1/"));
    Ok(())
}

fn modal_view(access_label: &str) -> ApprovalModalView {
    ApprovalModalView {
        tool_name: "write_file".to_owned(),
        call_id: "call-1".to_owned(),
        access_label: access_label.to_owned(),
        preview_title: "Edit src/lib.rs".to_owned(),
        preview_summary: "summary".to_owned(),
        metadata_collapsed: false,
        file_rows: vec![ApprovalFileRow {
            path: "src/lib.rs".to_owned(),
            selected: true,
            diagnostics: None,
        }],
        changed_files: vec!["src/lib.rs".to_owned()],
        diff_mode_label: "full",
        active_hunk_index: 1,
        hunk_total: 2,
        diff_label: "src/lib.rs".to_owned(),
        diff_lines: vec![ApprovalDiffLine {
            text: "@@ -1 +1 @@".to_owned(),
            kind: ApprovalDiffLineKind::Hunk,
            active_hunk: true,
        }],
        selected_action: ApprovalAction::Deny,
    }
}

fn multi_file_preview() -> ToolPreview {
    ToolPreview {
        title: "Update src/lib.rs".to_owned(),
        summary: "summary line one\nsummary line two".to_owned(),
        body: [
            "--- src/lib.rs",
            "+++ src/lib.rs",
            "@@ -1 +1 @@",
            "-old",
            "+new",
        ]
        .join("\n"),
        changed_files: vec!["src/lib.rs".to_owned(), "src/main.rs".to_owned()],
        file_diffs: vec![
            ToolPreviewFile {
                path: "src/lib.rs".to_owned(),
                diff: [
                    "--- src/lib.rs",
                    "+++ src/lib.rs",
                    "@@ -1 +1 @@",
                    "-old",
                    "+new",
                ]
                .join("\n"),
            },
            ToolPreviewFile {
                path: "src/main.rs".to_owned(),
                diff: [
                    "--- src/main.rs",
                    "+++ src/main.rs",
                    "@@ -2 +2 @@",
                    "-before",
                    "+after",
                ]
                .join("\n"),
            },
        ],
    }
}

fn rendered_content(terminal: &Terminal<TestBackend>) -> String {
    terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .map(|cell| cell.symbol())
        .collect::<String>()
}

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
        terminal: Default::default(),
        task: Default::default(),
        providers: BTreeMap::new(),
        mcp_servers: Vec::new(),
    }
}

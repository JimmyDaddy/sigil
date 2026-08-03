use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{Terminal, backend::TestBackend, style::Color};
use serde_json::json;
use sigil_kernel::{
    AgentConfig, CompactionConfig, EventHandler, MemoryConfig, PermissionConfig, RootConfig,
    RunEvent, SessionConfig, SyntaxThemeId, ToolAccess, ToolCall, ToolCategory, ToolPreview,
    ToolPreviewCapability, ToolPreviewFile, ToolSpec, WorkspaceConfig,
};

use crate::app::AppState;

use super::*;

fn test_approval_identity(call_id: &str) -> sigil_kernel::ApprovalRequestIdentityV2 {
    sigil_kernel::ApprovalRequestIdentityV2 {
        session_id: "session-approval-ui".to_owned(),
        run_id: "run-approval-ui".to_owned(),
        call_id: call_id.to_owned(),
        approval_request_id: format!("approval-{call_id}"),
        plan_hash: "plan-approval-ui".to_owned(),
        policy_version: "policy-approval-ui".to_owned(),
        execution_binding_hash: "binding-approval-ui".to_owned(),
        expires_at_ms: u64::MAX,
    }
}

#[test]
fn render_approval_file_row_includes_diagnostic_summary() {
    let row = ApprovalFileRow {
        path: "src/lib.rs".to_owned(),
        selected: true,
        diagnostics: Some(ApprovalDiagnosticSummary {
            errors: 1,
            warnings: 2,
        }),
        action: None,
        risk: None,
    };

    let line = render_approval_file_row(0, &row);
    let text = plain_line_text(&line);

    assert!(text.contains("src/lib.rs"));
    assert!(text.contains("1 error 2 warnings"));
}

#[test]
fn render_approval_file_row_includes_changeset_action_and_risk() {
    let row = ApprovalFileRow {
        path: "src/lib.rs".to_owned(),
        selected: false,
        diagnostics: None,
        action: Some("update".to_owned()),
        risk: Some("high".to_owned()),
    };

    let line = render_approval_file_row(0, &row);
    let text = plain_line_text(&line);

    assert!(text.contains("src/lib.rs"));
    assert!(text.contains("update"));
    assert!(text.contains("risk high"));
}

#[test]
fn approval_diff_status_line_includes_selected_file_diagnostics() {
    let view = ApprovalModalView {
        tool_name: "edit_file".to_owned(),
        source_agent: None,
        access_label: "file write".to_owned(),
        risk: sigil_kernel::PermissionRisk::Medium,
        policy_label: "local:ask network:allow source:allow final:ask".to_owned(),
        preview_title: "Edit src/lib.rs".to_owned(),
        preview_summary: "summary".to_owned(),
        change_set: None,
        metadata_collapsed: false,
        file_rows: vec![
            ApprovalFileRow {
                path: "src/other.rs".to_owned(),
                selected: false,
                diagnostics: Some(ApprovalDiagnosticSummary {
                    errors: 3,
                    warnings: 0,
                }),
                action: None,
                risk: None,
            },
            ApprovalFileRow {
                path: "src/lib.rs".to_owned(),
                selected: true,
                diagnostics: Some(ApprovalDiagnosticSummary {
                    errors: 0,
                    warnings: 1,
                }),
                action: None,
                risk: None,
            },
        ],
        changed_files: vec!["src/lib.rs".to_owned()],
        diff_mode_label: "full",
        active_hunk_index: 1,
        hunk_total: 2,
        diff_label: "src/lib.rs".to_owned(),
        diff_lines: Vec::new(),
        selected_action: ApprovalAction::Deny,
        session_grant_available: false,
        session_grant_unavailable_reason: Some(sigil_kernel::ToolApprovalSessionGrantUnavailableReason {
            code: sigil_kernel::ToolApprovalSessionGrantUnavailableReasonCode::OperationNotGrantable,
        }),
        ..ApprovalModalView::default()
    };

    let text = plain_line_text(&approval_diff_status_line(&view));

    assert!(text.contains("diagnostics 1 warning"));
    assert!(!text.contains("3 errors"));
}

#[test]
fn approval_header_and_review_lines_keep_metadata_secondary() {
    let base = ApprovalModalView {
        tool_name: "edit_file".to_owned(),
        source_agent: None,
        access_label: "file write".to_owned(),
        risk: sigil_kernel::PermissionRisk::Medium,
        policy_label: "local:ask network:allow source:allow final:ask".to_owned(),
        preview_title: "Edit src/lib.rs".to_owned(),
        preview_summary: "summary".to_owned(),
        change_set: None,
        metadata_collapsed: false,
        file_rows: Vec::new(),
        changed_files: vec!["src/lib.rs".to_owned()],
        diff_mode_label: "full",
        active_hunk_index: 0,
        hunk_total: 0,
        diff_label: "src/lib.rs".to_owned(),
        diff_lines: Vec::new(),
        selected_action: ApprovalAction::AllowOnce,
        session_grant_available: false,
        session_grant_unavailable_reason: Some(sigil_kernel::ToolApprovalSessionGrantUnavailableReason {
            code: sigil_kernel::ToolApprovalSessionGrantUnavailableReasonCode::OperationNotGrantable,
        }),
        ..ApprovalModalView::default()
    };

    let hidden = approval_header_lines(
        &ApprovalModalView {
            metadata_collapsed: true,
            ..base.clone()
        },
        40,
    );
    let empty = approval_review_lines(
        &ApprovalModalView {
            preview_title: String::new(),
            preview_summary: "   ".to_owned(),
            ..base.clone()
        },
        40,
    );
    let markdown = approval_review_lines(
        &ApprovalModalView {
            preview_summary: "**bold** line\n`code` line\nthird line".to_owned(),
            ..base
        },
        24,
    );

    let hidden_text = plain_lines_text(&hidden);
    let empty_text = plain_lines_text(&empty);
    let markdown_text = plain_lines_text(&markdown);

    assert_eq!(hidden.len(), 1);
    assert!(hidden_text.contains("risk medium"));
    assert!(!hidden_text.contains("summary"));
    assert!(!hidden_text.contains("policy local:ask network:allow source:allow final:ask"));
    assert!(empty_text.contains("No review content was provided."));
    assert!(markdown_text.contains("bold line"));
    assert!(markdown_text.contains("code line"));
    assert!(markdown_text.contains("third line"));
}

#[test]
fn approval_details_explain_why_session_grant_is_unavailable() {
    let view = ApprovalModalView {
        session_grant_available: false,
        session_grant_unavailable_reason: Some(
            sigil_kernel::ToolApprovalSessionGrantUnavailableReason {
                code: sigil_kernel::ToolApprovalSessionGrantUnavailableReasonCode::RiskNotGrantable,
            },
        ),
        ..ApprovalModalView::default()
    };

    let text = approval_permission_metadata_lines(&view, 120, &theme::default_palette())
        .iter()
        .map(plain_line_text)
        .collect::<Vec<_>>()
        .join("\n");

    assert!(text.contains("session grant"));
    assert!(text.contains("risk level does not allow reusable authority"));
}

#[test]
fn approval_details_expose_bounded_permission_plan() {
    let mut view = modal_view("shell execute");
    view.effects = BTreeSet::from([sigil_kernel::ToolPermissionEffect::ExecuteWorkspaceCode]);
    view.subjects = vec![sigil_kernel::ToolSubject::command(
        "cargo test",
        "cargo test",
    )];
    view.containment.network = sigil_kernel::NetworkContainment::Deny;
    view.safe_summary = sigil_kernel::ToolPermissionSummary {
        title: "Run workspace tests".to_owned(),
        detail: "Execute one workspace command without network access".to_owned(),
        step_count: 1,
        workspace_code_steps: 1,
    };
    view.decision_reasons = vec![sigil_kernel::PermissionDecisionReason {
        source: sigil_kernel::PermissionDecisionSource::PermissionModeDefault,
        code: "execute_workspace_code_requires_approval".to_owned(),
        detail: "Workspace code must be reviewed".to_owned(),
    }];

    let text = approval_permission_metadata_lines(&view, 180, &theme::default_palette())
        .iter()
        .map(plain_line_text)
        .collect::<Vec<_>>()
        .join("\n");

    assert!(text.contains("execute_workspace_code"));
    assert!(text.contains("network deny"));
    assert!(text.contains("execute_workspace_code_requires_approval"));
    assert!(text.contains("Run workspace tests"));
}

#[test]
fn approval_details_expose_recovery_for_unsupported_shell_syntax() {
    let view = ApprovalModalView {
        analysis: sigil_kernel::ToolAnalysisStatus::Unsupported {
            reason: sigil_kernel::ToolAnalysisReason::new(
                sigil_kernel::ToolAnalysisReasonCode::UnsupportedSyntax,
                Some("PowerShell syntax is not supported by the active analyzer".to_owned()),
            ),
        },
        ..modal_view("shell execute")
    };

    let text = approval_permission_metadata_lines(&view, 180, &theme::default_palette())
        .iter()
        .map(plain_line_text)
        .collect::<Vec<_>>()
        .join("\n");

    assert!(text.contains("unsupported shell dialect"));
    assert!(text.contains("/doctor"));
    assert!(text.contains("/config"));
}

#[test]
fn approval_review_lines_with_palette_use_configured_markdown_colors() {
    let palette = crate::ui::theme::Theme::builtin(sigil_kernel::ThemeId::SolarizedLight).palette;
    let view = ApprovalModalView {
        preview_summary: "`code` summary".to_owned(),
        ..modal_view("file write")
    };

    let lines = approval_review_lines_with_palette(&view, 40, SyntaxThemeId::default(), &palette);
    let code_span = lines
        .iter()
        .flat_map(|line| line.spans.iter())
        .find(|span| span.content.as_ref() == "code")
        .expect("approval summary inline code should render");

    assert_eq!(code_span.style.fg, Some(palette.markdown_code_fg));
    assert_eq!(code_span.style.bg, Some(palette.markdown_code_bg));
}

#[test]
fn approval_details_render_changeset_risk_format_and_agent() {
    let view = ApprovalModalView {
        change_set: Some(ApprovalChangeSetSummary {
            id: "change-123".to_owned(),
            risk: "high".to_owned(),
            format_hint: "cargo fmt --all".to_owned(),
        }),
        ..modal_view("file write")
    };
    let lines = approval_permission_metadata_lines(&view, 80, &theme::default_palette());
    let text = plain_lines_text(&lines);

    assert!(text.contains("change set"));
    assert!(text.contains("change-123"));
    assert!(text.contains("risk high"));
    assert!(text.contains("format cargo fmt --all"));

    let view = ApprovalModalView {
        source_agent: Some("Kernel Mapper · thread_1".to_owned()),
        ..modal_view("file write")
    };
    let lines = approval_permission_metadata_lines(&view, 80, &theme::default_palette());
    let text = plain_lines_text(&lines);

    assert!(text.contains("agent "));
    assert!(text.contains("Kernel Mapper · thread_1"));
}

#[test]
fn approval_footer_lines_include_file_navigation_hint_only_for_multiple_files() {
    let single = ApprovalModalView {
        tool_name: "edit_file".to_owned(),
        source_agent: None,
        access_label: "file write".to_owned(),
        risk: sigil_kernel::PermissionRisk::Medium,
        policy_label: "local:ask network:allow source:allow final:ask".to_owned(),
        preview_title: "Edit src/lib.rs".to_owned(),
        preview_summary: String::new(),
        change_set: None,
        metadata_collapsed: false,
        has_diff_preview: true,
        file_rows: vec![ApprovalFileRow {
            path: "src/lib.rs".to_owned(),
            selected: true,
            diagnostics: None,
            action: None,
            risk: None,
        }],
        changed_files: vec!["src/lib.rs".to_owned()],
        diff_mode_label: "full",
        active_hunk_index: 0,
        hunk_total: 0,
        diff_label: "src/lib.rs".to_owned(),
        diff_lines: Vec::new(),
        selected_action: ApprovalAction::AllowOnce,
        session_grant_available: false,
        session_grant_unavailable_reason: Some(sigil_kernel::ToolApprovalSessionGrantUnavailableReason {
            code: sigil_kernel::ToolApprovalSessionGrantUnavailableReasonCode::OperationNotGrantable,
        }),
        ..ApprovalModalView::default()
    };
    let multiple = ApprovalModalView {
        file_rows: vec![
            ApprovalFileRow {
                path: "src/lib.rs".to_owned(),
                selected: true,
                diagnostics: None,
                action: None,
                risk: None,
            },
            ApprovalFileRow {
                path: "src/main.rs".to_owned(),
                selected: false,
                diagnostics: None,
                action: None,
                risk: None,
            },
        ],
        ..single.clone()
    };

    let single_text = plain_lines_text(&approval_footer_lines(&single));
    let multiple_text = plain_lines_text(&approval_footer_lines(&multiple));
    let single_lines = approval_footer_lines(&single);

    assert!(!single_text.contains(",/. file"));
    assert!(multiple_text.contains(",/. file"));
    assert!(plain_line_text(&single_lines[0]).contains("Allow once"));
    assert!(!plain_line_text(&single_lines[0]).contains("Enter"));
    assert!(plain_line_text(&single_lines[1]).contains("Enter select"));
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
fn approval_header_lines_use_access_risk_and_compact_details_badges() {
    let write_view = modal_view("file write");
    let read_view = modal_view("file read");
    let high_view = ApprovalModalView {
        risk: sigil_kernel::PermissionRisk::High,
        ..modal_view("mcp read · network unknown")
    };

    let write_lines = approval_header_lines(&write_view, 80);
    let read_lines = approval_header_lines(&read_view, 80);
    let high_lines = approval_header_lines(&high_view, 80);
    let hidden_text = plain_lines_text(&approval_header_lines(
        &ApprovalModalView {
            metadata_collapsed: true,
            has_diff_preview: false,
            file_rows: Vec::new(),
            changed_files: vec!["src/lib.rs".to_owned(), "src/main.rs".to_owned()],
            ..modal_view("mcp read · network unknown")
        },
        80,
    ));

    assert_eq!(write_lines[0].spans[0].style.bg, Some(Color::Yellow));
    assert_eq!(read_lines[0].spans[0].style.bg, Some(Color::Green));
    let risk_high = theme::default_palette().risk_high;
    assert_eq!(high_lines[0].spans[0].style.bg, Some(risk_high));
    assert_eq!(
        high_lines[0].spans.last().and_then(|span| span.style.bg),
        Some(risk_high)
    );
    assert!(hidden_text.contains("Details"));
    assert!(!hidden_text.contains("policy"));
}

#[test]
fn approval_review_lines_handle_empty_and_multiline_content() {
    let empty = approval_review_lines(
        &ApprovalModalView {
            preview_title: String::new(),
            preview_summary: "  \n".to_owned(),
            ..modal_view("file read")
        },
        80,
    );
    let multiline = approval_review_lines(
        &ApprovalModalView {
            preview_summary: "line one\nline two\nline three".to_owned(),
            ..modal_view("file read")
        },
        80,
    );

    assert!(plain_lines_text(&empty).contains("No review content was provided."));
    assert_eq!(multiline.len(), 4);
    assert!(plain_lines_text(&multiline).contains("line one"));
    assert!(plain_lines_text(&multiline).contains("line two"));
    assert!(plain_lines_text(&multiline).contains("line three"));
}

#[test]
fn approval_review_lines_omit_generic_shell_copy_around_the_command() {
    let lines = approval_review_lines(
        &ApprovalModalView {
            tool_name: "bash".to_owned(),
            preview_title: "Run shell command".to_owned(),
            preview_summary: "cargo test -p sigil-tui".to_owned(),
            ..ApprovalModalView::default()
        },
        80,
    );

    assert_eq!(plain_lines_text(&lines), "cargo test -p sigil-tui");
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
                action: None,
                risk: None,
            },
            ApprovalFileRow {
                path: "src/main.rs".to_owned(),
                selected: false,
                diagnostics: None,
                action: None,
                risk: None,
            },
        ],
        ..modal_view("file read")
    });

    assert!(!plain_line_text(&single[1]).contains(",/. file"));
    assert!(plain_line_text(&multi[1]).contains(",/. file"));
    assert!(!plain_line_text(&single[0]).contains("Enter"));
    assert!(plain_line_text(&single[1]).contains("Enter select"));
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
            action: None,
            risk: None,
        }],
        hunk_total: 0,
        active_hunk_index: 9,
        ..modal_view("mcp read · network unknown")
    }));

    assert!(text.contains("hunk 0/0"));
    assert!(!text.contains("diagnostics"));
}

#[test]
fn approval_diff_status_line_uses_target_for_non_file_approval() {
    let text = plain_line_text(&approval_diff_status_line(&ApprovalModalView {
        file_rows: Vec::new(),
        changed_files: Vec::new(),
        diff_label: "terminal task terminal-1".to_owned(),
        hunk_total: 0,
        active_hunk_index: 0,
        ..modal_view("terminal input")
    }));

    assert!(text.contains("target terminal task terminal-1"));
    assert!(!text.contains("path terminal task terminal-1"));
}

#[test]
fn approval_diagnostics_helpers_cover_clean_warning_and_error_states() {
    let palette = crate::ui::theme::default_palette();
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
    assert_eq!(approval_risk_color("low"), Color::Green);
    assert_eq!(approval_risk_color("destructive"), Color::LightRed);
    assert_eq!(approval_risk_color("protected"), Color::LightRed);
    assert_eq!(approval_risk_color("unknown"), palette.text_muted);
    let selected = approval_file_meta_style("create", true);
    assert_eq!(selected.fg, Some(Color::Black));
    assert_eq!(selected.bg, Some(Color::Green));
}

#[test]
fn approval_count_label_uses_singular_and_plural_forms() {
    assert_eq!(count_label(1, "warning", "warnings"), "1 warning");
    assert_eq!(count_label(3, "warning", "warnings"), "3 warnings");
}

#[test]
fn render_approval_diff_line_highlights_active_hunks_and_blank_text() {
    let palette = crate::ui::theme::default_palette();
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
    assert_eq!(active.spans[1].style.fg, Some(palette.diff_removed_fg));
    assert!(
        active.spans[1]
            .style
            .add_modifier
            .contains(ratatui::style::Modifier::BOLD)
    );
    assert_eq!(active.spans[3].style.fg, Some(palette.diff_gutter_fg));
    assert_eq!(active.spans[5].content.as_ref(), " ");
    assert_eq!(active.spans[5].style.bg, Some(palette.diff_current_hunk_bg));

    assert_eq!(inactive.spans[0].content.as_ref(), "│");
    assert_eq!(inactive.spans[1].style.fg, Some(palette.diff_gutter_fg));
    assert_eq!(inactive.spans[3].style.fg, Some(palette.diff_added_fg));
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
        approval_identity: test_approval_identity("call-approval"),
        effects: std::collections::BTreeSet::new(),
        analysis: sigil_kernel::ToolAnalysisStatus::Complete,
        containment: sigil_kernel::ExecutionContainmentRequest::default(),
        safe_summary: sigil_kernel::ToolPermissionSummary::default(),
        decision_reasons: Vec::new(),
        session_grant_available: false,
        session_grant_unavailable_reason: Some(sigil_kernel::ToolApprovalSessionGrantUnavailableReason {
            code: sigil_kernel::ToolApprovalSessionGrantUnavailableReasonCode::OperationNotGrantable,
        }),
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
            network_effect: None,
            preview: ToolPreviewCapability::Required,
        },
        subjects: Vec::new(),
        network_effect: None,
        local_policy_decision: sigil_kernel::ApprovalMode::Ask,
        network_policy_decision: sigil_kernel::ApprovalMode::Allow,
        source_policy_decision: sigil_kernel::ApprovalMode::Allow,
        operation: sigil_kernel::ToolOperation::OverwriteFile,
        risk: sigil_kernel::PermissionRisk::Medium,
        subject_zones: Vec::new(),
        confirmation: None,
        snapshot_required: false,
        command_permission_matches: Vec::new(),
        preview: Some(multi_file_preview()),
    })?;
    app.runtime.code_intelligence_diagnostics_by_path.insert(
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
    assert!(rendered.contains("Review file changes"));
    assert!(rendered.contains("Files 1/2"));
    assert!(rendered.contains("src/lib.rs"));
    assert!(rendered.contains("Allow"));
    assert!(rendered.contains("Deny"));
    Ok(())
}

#[test]
fn render_approval_modal_uses_configured_theme_colors() -> anyhow::Result<()> {
    let mut config = test_config();
    let mut colors = BTreeMap::new();
    colors.insert("approval_bg".to_owned(), "#010203".to_owned());
    colors.insert("approval_selected_bg".to_owned(), "#112233".to_owned());
    colors.insert("approval_allow_bg".to_owned(), "#214365".to_owned());
    colors.insert("approval_deny_bg".to_owned(), "#654321".to_owned());
    colors.insert("text_inverse".to_owned(), "#F1F2F3".to_owned());
    colors.insert("markdown_code_fg".to_owned(), "#D0E0F0".to_owned());
    colors.insert("markdown_code_bg".to_owned(), "#203040".to_owned());
    config.appearance.colors = sigil_kernel::ThemeColorOverrides::new(colors);
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &config);
    app.handle(RunEvent::ToolApprovalRequested {
        approval_identity: test_approval_identity("call-themed-approval"),
        effects: std::collections::BTreeSet::new(),
        analysis: sigil_kernel::ToolAnalysisStatus::Complete,
        containment: sigil_kernel::ExecutionContainmentRequest::default(),
        safe_summary: sigil_kernel::ToolPermissionSummary::default(),
        decision_reasons: Vec::new(),
        session_grant_available: false,
        session_grant_unavailable_reason: Some(sigil_kernel::ToolApprovalSessionGrantUnavailableReason {
            code: sigil_kernel::ToolApprovalSessionGrantUnavailableReasonCode::OperationNotGrantable,
        }),
        call: ToolCall {
            id: "call-themed-approval".to_owned(),
            name: "write_file".to_owned(),
            args_json: r#"{"path":"src/lib.rs"}"#.to_owned(),
        },
        spec: ToolSpec {
            name: "write_file".to_owned(),
            description: "Write file".to_owned(),
            input_schema: json!({"type":"object"}),
            category: ToolCategory::File,
            access: ToolAccess::Write,
            network_effect: None,
            preview: ToolPreviewCapability::Required,
        },
        subjects: Vec::new(),
        network_effect: None,
        local_policy_decision: sigil_kernel::ApprovalMode::Ask,
        network_policy_decision: sigil_kernel::ApprovalMode::Allow,
        source_policy_decision: sigil_kernel::ApprovalMode::Allow,
        operation: sigil_kernel::ToolOperation::OverwriteFile,
        risk: sigil_kernel::PermissionRisk::Medium,
        subject_zones: Vec::new(),
        confirmation: None,
        snapshot_required: false,
        command_permission_matches: Vec::new(),
        preview: Some(ToolPreview {
            title: "Edit src/lib.rs".to_owned(),
            summary: "`approval-code` summary".to_owned(),
            body: "--- src/lib.rs\n+++ src/lib.rs\n@@ -1 +1 @@\n-old\n+new".to_owned(),
            changed_files: vec!["src/lib.rs".to_owned()],
            file_diffs: vec![ToolPreviewFile {
                path: "src/lib.rs".to_owned(),
                diff: "--- src/lib.rs\n+++ src/lib.rs\n@@ -1 +1 @@\n-old\n+new".to_owned(),
            }],
        }),
    })?;
    let backend = TestBackend::new(140, 32);
    let mut terminal = Terminal::new(backend)?;

    terminal.draw(|frame| render_approval_modal(frame, &app))?;

    assert_eq!(
        cell_colors_at_text(&terminal, "Review file changes", "Review file changes"),
        (Color::Rgb(241, 242, 243), Color::Rgb(17, 34, 51))
    );
    assert_eq!(
        cell_colors_at_text(&terminal, "approval-code", "approval-code"),
        (Color::Rgb(208, 224, 240), Color::Rgb(32, 48, 64))
    );
    assert_eq!(
        cell_colors_at_text(&terminal, "Allow", "Allow"),
        (Color::Rgb(241, 242, 243), Color::Rgb(33, 67, 101))
    );
    Ok(())
}

#[test]
fn render_approval_modal_uses_hidden_metadata_and_preview_fallback() -> anyhow::Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.handle(RunEvent::ToolApprovalRequested {
        approval_identity: test_approval_identity("call-remote"),
        effects: std::collections::BTreeSet::new(),
        analysis: sigil_kernel::ToolAnalysisStatus::Complete,
        containment: sigil_kernel::ExecutionContainmentRequest::default(),
        safe_summary: sigil_kernel::ToolPermissionSummary::default(),
        decision_reasons: Vec::new(),
        session_grant_available: false,
        session_grant_unavailable_reason: Some(sigil_kernel::ToolApprovalSessionGrantUnavailableReason {
            code: sigil_kernel::ToolApprovalSessionGrantUnavailableReasonCode::OperationNotGrantable,
        }),
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
            access: ToolAccess::Read,
            network_effect: Some(sigil_kernel::NetworkEffect::Unknown),
            preview: ToolPreviewCapability::None,
        },
        subjects: Vec::new(),
        network_effect: Some(sigil_kernel::NetworkEffect::Unknown),
        local_policy_decision: sigil_kernel::ApprovalMode::Allow,
        network_policy_decision: sigil_kernel::ApprovalMode::Allow,
        source_policy_decision: sigil_kernel::ApprovalMode::Ask,
        operation: sigil_kernel::ToolOperation::NetworkRequest,
        risk: sigil_kernel::PermissionRisk::High,
        subject_zones: Vec::new(),
        confirmation: None,
        snapshot_required: false,
        command_permission_matches: Vec::new(),
        preview: None,
    })?;
    let backend = TestBackend::new(100, 24);
    let mut terminal = Terminal::new(backend)?;

    terminal.draw(|frame| render_approval_modal(frame, &app))?;

    let rendered = rendered_content(&terminal);
    assert!(rendered.contains("Approve action?"));
    assert!(rendered.contains("Content to approve"));
    assert!(rendered.contains("Run remote_tool"));
    assert!(rendered.contains("Details"));
    assert!(rendered.contains("Decision"));
    assert!(!rendered.contains("No file changes to review."));
    assert!(!rendered.contains("Summary"));
    assert!(!rendered.contains("No structured diff preview available."));
    assert!(!rendered.contains("call-remote"));
    assert!(!rendered.contains("Files 1/"));
    Ok(())
}

#[test]
fn render_shell_approval_prioritizes_command_without_internal_identity_or_empty_diff()
-> anyhow::Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.handle(RunEvent::ToolApprovalRequested {
        approval_identity: test_approval_identity("call-shell-internal"),
        effects: std::collections::BTreeSet::new(),
        analysis: sigil_kernel::ToolAnalysisStatus::Complete,
        containment: sigil_kernel::ExecutionContainmentRequest::default(),
        safe_summary: sigil_kernel::ToolPermissionSummary::default(),
        decision_reasons: Vec::new(),
        session_grant_available: false,
        session_grant_unavailable_reason: Some(
            sigil_kernel::ToolApprovalSessionGrantUnavailableReason {
                code: sigil_kernel::ToolApprovalSessionGrantUnavailableReasonCode::OperationNotGrantable,
            },
        ),
        call: ToolCall {
            id: "call-shell-internal".to_owned(),
            name: "bash".to_owned(),
            args_json: json!({
                "command": "cargo clippy --workspace --all-targets -- -D warnings 2>&1 | tail -15"
            })
            .to_string(),
        },
        spec: ToolSpec {
            name: "bash".to_owned(),
            description: "Run bash".to_owned(),
            input_schema: json!({"type":"object"}),
            category: ToolCategory::Shell,
            access: ToolAccess::Execute,
            network_effect: None,
            preview: ToolPreviewCapability::None,
        },
        subjects: vec![sigil_kernel::ToolSubject::command(
            "family:cargo_check",
            "family:cargo_check",
        )],
        network_effect: None,
        local_policy_decision: sigil_kernel::ApprovalMode::Ask,
        network_policy_decision: sigil_kernel::ApprovalMode::Allow,
        source_policy_decision: sigil_kernel::ApprovalMode::Allow,
        operation: sigil_kernel::ToolOperation::ExecuteWorkspaceCheckCommand,
        risk: sigil_kernel::PermissionRisk::Medium,
        subject_zones: Vec::new(),
        confirmation: None,
        snapshot_required: false,
        command_permission_matches: Vec::new(),
        preview: None,
    })?;
    let backend = TestBackend::new(110, 24);
    let mut terminal = Terminal::new(backend)?;

    terminal.draw(|frame| render_approval_modal(frame, &app))?;

    let rendered = rendered_content(&terminal);
    assert!(rendered.contains("Approve command?"));
    assert!(rendered.contains("Command to run"));
    assert!(rendered.contains("cargo clippy --workspace --all-targets"));
    assert!(rendered.contains("-D warnings"));
    assert!(rendered.contains("Details"));
    assert!(rendered.contains("Decision"));
    assert!(!rendered.contains("No file changes to review."));
    assert!(!rendered.contains("Summary"));
    assert!(!rendered.contains("call-shell-internal"));
    assert!(!rendered.contains("local:ask network:allow"));
    assert!(!rendered.contains("No structured diff preview available."));
    assert_eq!(
        cell_colors_at_text(&terminal, "cargo clippy --workspace", "cargo"),
        (
            theme::default_palette().text_primary,
            theme::default_palette().surface_code
        )
    );

    app.handle_key_event(KeyEvent::new(KeyCode::Char('m'), KeyModifiers::NONE))?;
    terminal.draw(|frame| render_approval_modal(frame, &app))?;
    let expanded = rendered_content(&terminal);
    assert!(expanded.contains("local:ask network:allow"));
    assert!(expanded.contains("Command to run"));
    assert!(expanded.contains("cargo clippy --workspace --all-targets"));
    Ok(())
}

fn modal_view(access_label: &str) -> ApprovalModalView {
    ApprovalModalView {
        tool_name: "write_file".to_owned(),
        source_agent: None,
        access_label: access_label.to_owned(),
        risk: if access_label.contains("read") {
            sigil_kernel::PermissionRisk::Low
        } else {
            sigil_kernel::PermissionRisk::Medium
        },
        policy_label: "local:ask network:allow source:allow final:ask".to_owned(),
        preview_title: "Edit src/lib.rs".to_owned(),
        preview_summary: "summary".to_owned(),
        change_set: None,
        metadata_collapsed: false,
        has_diff_preview: true,
        file_rows: vec![ApprovalFileRow {
            path: "src/lib.rs".to_owned(),
            selected: true,
            diagnostics: None,
            action: None,
            risk: None,
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
        session_grant_available: false,
        session_grant_unavailable_reason: Some(sigil_kernel::ToolApprovalSessionGrantUnavailableReason {
            code: sigil_kernel::ToolApprovalSessionGrantUnavailableReasonCode::OperationNotGrantable,
        }),
        ..ApprovalModalView::default()
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

fn rendered_rows(terminal: &Terminal<TestBackend>) -> Vec<String> {
    let buffer = terminal.backend().buffer();
    (0..buffer.area.height)
        .map(|y| {
            (0..buffer.area.width)
                .map(|x| buffer.cell((x, y)).expect("cell in bounds").symbol())
                .collect::<String>()
        })
        .collect()
}

fn char_index_of(row: &str, needle: &str) -> Option<usize> {
    row.find(needle)
        .map(|byte_index| row[..byte_index].chars().count())
}

fn cell_colors_at_text(
    terminal: &Terminal<TestBackend>,
    row_needle: &str,
    text: &str,
) -> (Color, Color) {
    let rows = rendered_rows(terminal);
    let row_index = rows
        .iter()
        .position(|row| row.contains(row_needle))
        .expect("row should render");
    let column_index = char_index_of(&rows[row_index], text).expect("text should render in row");
    let cell = terminal
        .backend()
        .buffer()
        .cell((column_index as u16, row_index as u16))
        .expect("cell in bounds");
    (cell.fg, cell.bg)
}

fn test_config() -> RootConfig {
    RootConfig {
        config_version: 2,
        workspace: WorkspaceConfig {
            root: ".".to_owned(),
        },
        storage: Default::default(),
        session: SessionConfig {
            log_dir: Some(".sigil/sessions".to_owned()),
            retention: Default::default(),
        },
        agent: AgentConfig {
            runtime_provider: "deepseek".to_owned(),
            connection: Some(
                sigil_kernel::ConnectionId::new("deepseek-default").expect("valid test connection"),
            ),
            model: "deepseek-v4-flash".to_owned(),
            max_turns: None,
            tool_timeout_secs: 30,
        },
        model_request: Default::default(),
        permission: PermissionConfig::default(),
        memory: MemoryConfig::with_enabled(true),
        skills: Default::default(),
        compaction: CompactionConfig::default(),
        code_intelligence: Default::default(),
        terminal: Default::default(),
        execution: Default::default(),
        verification: Default::default(),
        appearance: Default::default(),
        task: Default::default(),
        connections: BTreeMap::from([(
            "deepseek-default".to_owned(),
            serde_json::json!({
                "label": "DeepSeek",
                "provider": "deepseek",
                "protocol": "deepseek",
                "base_url": "https://api.deepseek.com",
                "credential": {"source": "environment", "name": "SIGIL_API_KEY"}
            }),
        )]),
        web: Default::default(),
        mcp_servers: Vec::new(),
    }
}

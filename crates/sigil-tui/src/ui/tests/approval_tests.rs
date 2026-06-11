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

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
fn approval_header_lines_show_meta_hidden_hint_and_counts() {
    let view = ApprovalModalView {
        tool_name: "write_file".to_owned(),
        call_id: "call-2".to_owned(),
        access_label: "file write".to_owned(),
        preview_title: "Write docs/notes.md".to_owned(),
        preview_summary: "ignored while collapsed".to_owned(),
        metadata_collapsed: true,
        file_rows: vec![ApprovalFileRow {
            path: "docs/notes.md".to_owned(),
            selected: true,
            diagnostics: None,
        }],
        changed_files: vec!["docs/notes.md".to_owned()],
        diff_mode_label: "split",
        active_hunk_index: 0,
        hunk_total: 3,
        diff_label: "docs/notes.md".to_owned(),
        diff_lines: Vec::new(),
        selected_action: ApprovalAction::Allow,
    };

    let plain = approval_header_lines(&view, 48)
        .iter()
        .map(plain_line_text)
        .collect::<Vec<_>>()
        .join("\n");

    assert!(plain.contains("meta hidden"));
    assert!(plain.contains("press M to expand"));
    assert!(plain.contains("files 1"));
    assert!(plain.contains("hunks 3"));
    assert!(plain.contains("mode split"));
}

#[test]
fn approval_diagnostics_label_reports_clean_state() {
    assert_eq!(
        approval_diagnostics_label(ApprovalDiagnosticSummary {
            errors: 0,
            warnings: 0,
        }),
        "clean"
    );
}

#[test]
fn render_approval_diff_line_marks_active_hunk_and_preserves_empty_rows() {
    let line = render_approval_diff_line(
        ApprovalDiffLine {
            text: String::new(),
            kind: ApprovalDiffLineKind::Added,
            active_hunk: true,
        },
        None,
        Some(7),
        2,
    );
    let text = plain_line_text(&line);

    assert!(text.starts_with(">"));
    assert!(text.contains(" 7│  "));
    assert_eq!(line.spans.last().expect("body span").content.as_ref(), " ");
}

fn plain_line_text(line: &Line<'static>) -> String {
    line.spans
        .iter()
        .map(|span| span.content.as_ref())
        .collect::<String>()
}

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

fn plain_line_text(line: &Line<'static>) -> String {
    line.spans
        .iter()
        .map(|span| span.content.as_ref())
        .collect::<String>()
}

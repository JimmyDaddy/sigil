use super::*;
use crate::app::tests::common::{
    inject_write_file_approval, multi_file_approval_preview, test_config,
};
use sigil_kernel::{ToolAccess, ToolCategory, ToolPreviewCapability, ToolSubjectScope};

#[test]
fn approval_helper_functions_format_subjects_and_diff_lines() {
    let subject = sigil_kernel::ToolSubject::path_with_scope(
        "./src/main.rs",
        "src/main.rs",
        Some(std::path::PathBuf::from("/workspace/src/main.rs")),
        ToolSubjectScope::Workspace,
    );
    let spec = sigil_kernel::ToolSpec {
        name: "write_file".to_owned(),
        description: "Write".to_owned(),
        input_schema: serde_json::json!({}),
        category: ToolCategory::File,
        access: ToolAccess::Write,
        preview: ToolPreviewCapability::Required,
    };

    assert_eq!(approval_access_label(&spec), "file write");
    assert_eq!(
        approval_subject_lines(std::slice::from_ref(&subject)),
        vec!["subject=workspace:path:/workspace/src/main.rs".to_owned()]
    );
    assert_eq!(
        approval_subject_summary(std::slice::from_ref(&subject)),
        Some("workspace:path:/workspace/src/main.rs".to_owned())
    );
    assert_eq!(
        approval_diff_line_kind("--- current/file"),
        ApprovalDiffLineKind::Header
    );
    assert_eq!(
        approval_diff_line_kind("@@ -1 +1 @@"),
        ApprovalDiffLineKind::Hunk
    );
    assert_eq!(
        approval_diff_line_kind("+added"),
        ApprovalDiffLineKind::Added
    );
    assert_eq!(
        approval_diff_line_kind("-removed"),
        ApprovalDiffLineKind::Removed
    );
    assert_eq!(
        approval_diff_line_kind(" context"),
        ApprovalDiffLineKind::Context
    );
    assert_eq!(
        normalize_approval_diagnostic_path(".\\src\\main.rs"),
        "src/main.rs"
    );
}

#[test]
fn approval_diff_transformers_cover_hunks_and_changed_only() -> anyhow::Result<()> {
    let mut app = AppState::from_root_config(std::path::Path::new("sigil.toml"), &test_config());
    inject_write_file_approval(&mut app, multi_file_approval_preview())?;

    let diff = app
        .selected_approval_diff()
        .expect("selected diff should exist")
        .to_owned();
    assert_eq!(app.approval_hunk_positions().len(), 2);

    app.approval_selected_hunk_index = 1;
    let current = app.extract_current_hunk(&diff);
    assert!(current.contains("..."));
    assert!(current.contains("@@ -5,2 +5,2 @@"));

    let changed = app.extract_changed_only(&diff);
    assert!(!changed.contains(" alpha"));
    assert!(changed.contains("-beta"));
    assert!(changed.contains("+gamma"));

    app.approval_diff_mode = ApprovalDiffMode::CurrentHunk;
    assert!(
        app.transform_approval_diff(&diff)
            .contains("@@ -5,2 +5,2 @@")
    );

    app.approval_diff_mode = ApprovalDiffMode::ChangedOnly;
    assert_eq!(app.selected_approval_diff(), Some(diff.as_str()));
    Ok(())
}

#[test]
fn approval_hunkless_and_file_switch_guards_cover_private_paths() -> anyhow::Result<()> {
    let mut app = AppState::from_root_config(std::path::Path::new("sigil.toml"), &test_config());
    inject_write_file_approval(
        &mut app,
        sigil_kernel::ToolPreview {
            title: "Plain preview".to_owned(),
            summary: String::new(),
            body: "plain body".to_owned(),
            changed_files: vec!["note.txt".to_owned()],
            file_diffs: Vec::new(),
        },
    )?;

    assert_eq!(app.extract_current_hunk("plain body"), "plain body");
    app.jump_approval_hunk(false);
    app.switch_approval_file(true);

    app.pending_approval = None;
    app.switch_approval_file(true);

    inject_write_file_approval(&mut app, multi_file_approval_preview())?;
    app.approval_selected_hunk_index = 1;
    app.jump_approval_hunk(false);
    assert_eq!(app.approval_selected_hunk_index, 0);

    app.approval_diff_mode = ApprovalDiffMode::CurrentHunk;
    let view = app
        .approval_modal_view()
        .expect("approval modal view should exist");
    assert_eq!(view.active_hunk_index, 1);
    assert!(
        view.diff_lines
            .iter()
            .any(|line| line.active_hunk && line.text.starts_with("@@"))
    );
    Ok(())
}

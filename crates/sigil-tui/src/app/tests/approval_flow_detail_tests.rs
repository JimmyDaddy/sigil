use super::*;
use crate::app::tests::common::{
    inject_write_file_approval, multi_file_approval_preview, test_config,
};
use sigil_kernel::{
    ControlEntry, SessionLogEntry, ToolAccess, ToolCategory, ToolPreviewCapability,
    ToolSubjectScope,
};

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
    assert!(
        approval_changeset_file_metadata("write_file", "{}").is_empty(),
        "non-change-set tools should not get change set file metadata"
    );
    assert!(
        approval_changeset_file_metadata("apply_changeset", "not json").is_empty(),
        "malformed change set args should be ignored"
    );
    assert!(
        approval_changeset_file_metadata("apply_changeset", r#"{"id":"change-1"}"#).is_empty(),
        "change set args without files should be ignored"
    );
    assert!(approval_format_hint(&["package.json".to_owned()]).contains("JSON"));
    assert!(approval_format_hint(&["ci.yaml".to_owned()]).contains("YAML"));
    assert_eq!(
        approval_format_hint(&["Makefile".to_owned()]),
        "run the relevant formatter before commit"
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

#[test]
fn approval_source_agent_helper_uses_profile_and_thread_fallbacks() -> anyhow::Result<()> {
    let mut app = AppState::from_root_config(std::path::Path::new("sigil.toml"), &test_config());
    let profile_thread_id = sigil_kernel::AgentThreadId::new("profile_thread")?;
    let snapshot_id = sigil_kernel::AgentProfileSnapshotId::new("snapshot_a")?;
    app.sync_current_session_state(vec![
        SessionLogEntry::Control(ControlEntry::AgentThreadStarted(
            sigil_kernel::AgentThreadStartedEntry {
                thread_id: profile_thread_id.clone(),
                parent_thread_id: None,
                parent_session_ref: sigil_kernel::SessionRef::new_relative("parent.jsonl")?,
                thread_session_ref: sigil_kernel::SessionRef::new_relative(
                    "children/profile_thread.jsonl",
                )?,
                profile_id: sigil_kernel::AgentProfileId::new("profile-reader")?,
                profile_snapshot_id: snapshot_id.clone(),
                run_context: sigil_kernel::AgentRunContextSnapshot {
                    profile_snapshot_id: snapshot_id,
                    provider: "deepseek".to_owned(),
                    model: "deepseek-v4-pro".to_owned(),
                    reasoning_effort: None,
                    workspace_root: sigil_kernel::WorkspaceRootSnapshot::new(".")?,
                    effective_tool_scope_hash: "tools".to_owned(),
                    effective_permission_policy_hash: "permissions".to_owned(),
                    effective_mcp_scope_hash: "mcp".to_owned(),
                    provider_capability_hash: "provider".to_owned(),
                    model_visible_agent_index_hash: None,
                    budget_policy_hash: "budget".to_owned(),
                    provider_background_handle_ref: None,
                },
                objective: "read".to_owned(),
                prompt_hash: "prompt".to_owned(),
                invocation_mode: sigil_kernel::AgentInvocationMode::Foreground,
                invocation_source: sigil_kernel::AgentInvocationSource::Task,
                display_name: None,
                created_at_ms: None,
            },
        )),
        SessionLogEntry::Control(ControlEntry::AgentApprovalRoute(
            sigil_kernel::AgentApprovalRouteEntry {
                route_id: sigil_kernel::AgentRouteId::new("route_profile")?,
                source_thread_id: profile_thread_id,
                target_thread_id: None,
                call_id: "call-profile".to_owned(),
                tool_name: "read_file".to_owned(),
                status: sigil_kernel::AgentRouteStatus::Requested,
            },
        )),
        SessionLogEntry::Control(ControlEntry::AgentApprovalRoute(
            sigil_kernel::AgentApprovalRouteEntry {
                route_id: sigil_kernel::AgentRouteId::new("route_missing")?,
                source_thread_id: sigil_kernel::AgentThreadId::new("missing_thread")?,
                target_thread_id: None,
                call_id: "call-missing".to_owned(),
                tool_name: "read_file".to_owned(),
                status: sigil_kernel::AgentRouteStatus::Requested,
            },
        )),
    ]);

    assert_eq!(
        app.pending_approval_source_agent("call-profile").as_deref(),
        Some("profile-reader · profile_thread")
    );
    assert_eq!(
        app.pending_approval_source_agent("call-missing").as_deref(),
        Some("missing_thread")
    );
    assert_eq!(app.pending_approval_source_agent("call-none"), None);
    Ok(())
}

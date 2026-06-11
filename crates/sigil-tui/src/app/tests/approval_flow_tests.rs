use super::*;

#[test]
fn approval_request_stores_preview() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    inject_write_file_approval(&mut app, sample_approval_preview())?;

    let pending = app.pending_approval.expect("expected pending approval");
    let preview = pending.preview.expect("expected preview");
    assert_eq!(preview.changed_files, vec!["note.txt".to_owned()]);
    assert!(preview.body.contains("+++ proposed/note.txt"));
    Ok(())
}

#[test]
fn approval_request_without_preview_uses_visible_fallback() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.handle(RunEvent::ToolApprovalRequested {
        call: ToolCall {
            id: "call-mcp-1".to_owned(),
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

    let lines = app.approval_preview_lines().join("\n");
    assert!(lines.contains("tool=remote_tool"));
    assert!(lines.contains("mode=mcp network"));
    assert!(lines.contains(r#"args={"query":"status"}"#));

    let view = app
        .approval_modal_view()
        .expect("approval modal view should exist");
    assert_eq!(view.preview_title, "Run remote_tool");
    assert_eq!(view.access_label, "mcp network");
    assert!(view.preview_summary.contains("preview unavailable"));
    assert!(
        view.diff_lines
            .iter()
            .any(|line| line.text.contains("No structured diff preview available"))
    );
    Ok(())
}

#[test]
fn approval_modal_view_includes_affected_diagnostics_summary() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.handle(RunEvent::ToolResult(ToolResult::ok(
        "call-code",
        "code_diagnostics",
        json!({
            "query": { "paths": ["note.txt", "clean.rs"] },
            "diagnostics": [
                { "path": "note.txt", "severity": "error" },
                { "path": "note.txt", "severity": "warning" },
                { "path": "other.rs", "severity": "error" }
            ]
        })
        .to_string(),
        ToolResultMeta::default(),
    )))?;
    inject_write_file_approval(&mut app, sample_approval_preview())?;

    let view = app
        .approval_modal_view()
        .expect("approval modal view should exist");
    let row = view
        .file_rows
        .iter()
        .find(|row| row.path == "note.txt")
        .expect("changed file should be listed");

    assert_eq!(
        row.diagnostics,
        Some(ApprovalDiagnosticSummary {
            errors: 1,
            warnings: 1,
        })
    );
    Ok(())
}

#[test]
fn approval_diff_mode_cycles_to_changed_only() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    inject_write_file_approval(&mut app, sample_approval_preview())?;
    app.handle_key_event(KeyEvent::new(KeyCode::Char('v'), KeyModifiers::NONE))?;
    app.handle_key_event(KeyEvent::new(KeyCode::Char('v'), KeyModifiers::NONE))?;
    let lines = app.approval_preview_lines().join("\n");
    assert!(lines.contains("mode=changed-only"));
    assert!(!lines.contains("   alpha"));
    assert!(lines.contains("-beta"));
    assert!(lines.contains("+gamma"));
    Ok(())
}

#[test]
fn approval_request_shows_external_subjects_without_preview() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    let external_path = Path::new("/tmp/sigil-outside.txt").to_path_buf();
    app.handle(RunEvent::ToolApprovalRequested {
        call: ToolCall {
            id: "call-external-1".to_owned(),
            name: "read_file".to_owned(),
            args_json: r#"{"path":"/tmp/sigil-outside.txt"}"#.to_owned(),
        },
        spec: ToolSpec {
            name: "read_file".to_owned(),
            description: "Read file".to_owned(),
            input_schema: json!({"type":"object"}),
            category: ToolCategory::File,
            access: ToolAccess::Read,
            preview: ToolPreviewCapability::None,
        },
        subjects: vec![ToolSubject::path_with_scope(
            "/tmp/sigil-outside.txt",
            "/tmp/sigil-outside.txt",
            Some(external_path.clone()),
            ToolSubjectScope::External,
        )],
        preview: None,
    })?;

    let lines = app.approval_preview_lines().join("\n");
    assert!(lines.contains("subject=external:path:/tmp/sigil-outside.txt"));
    let view = app
        .approval_modal_view()
        .expect("approval modal view should exist");
    assert!(view.preview_summary.contains("external:path"));
    assert!(view.preview_summary.contains("/tmp/sigil-outside.txt"));
    Ok(())
}

#[test]
fn approval_keys_emit_allow_and_deny_actions() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    inject_write_file_approval(&mut app, sample_approval_preview())?;

    let allow = app.handle_key_event(KeyEvent::new(KeyCode::Char('y'), KeyModifiers::NONE))?;
    assert!(matches!(
        allow,
        Some(AppAction::ApprovalDecision { call_id, approved })
            if call_id == "call-1" && approved
    ));

    let deny = app.handle_key_event(KeyEvent::new(KeyCode::Char('n'), KeyModifiers::NONE))?;
    assert!(matches!(
        deny,
        Some(AppAction::ApprovalDecision { call_id, approved })
            if call_id == "call-1" && !approved
    ));
    Ok(())
}

#[test]
fn approval_enter_chooses_selected_action() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    inject_write_file_approval(&mut app, sample_approval_preview())?;

    assert_eq!(app.approval_selected_action, ApprovalAction::Deny);
    assert_eq!(
        app.approval_modal_view()
            .expect("approval modal should exist")
            .selected_action,
        ApprovalAction::Deny
    );
    let deny = app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))?;
    assert!(matches!(
        deny,
        Some(AppAction::ApprovalDecision { call_id, approved })
            if call_id == "call-1" && !approved
    ));

    app.handle_key_event(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE))?;
    assert_eq!(app.approval_selected_action, ApprovalAction::Allow);
    let allow = app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))?;
    assert!(matches!(
        allow,
        Some(AppAction::ApprovalDecision { call_id, approved })
            if call_id == "call-1" && approved
    ));
    assert!(
        app.events
            .iter()
            .any(|event| event.label == "approval:action" && event.detail == "allow")
    );
    Ok(())
}

#[test]
fn approval_metadata_toggle_collapses_and_expands_preview_header() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    inject_write_file_approval(&mut app, sample_approval_preview())?;

    let expanded = app.approval_preview_lines().join("\n");
    assert!(expanded.contains("tool=write_file"));
    assert!(expanded.contains("preview=Update note.txt"));

    assert!(
        app.handle_key_event(KeyEvent::new(KeyCode::Char('m'), KeyModifiers::NONE))?
            .is_none()
    );
    let collapsed = app.approval_preview_lines().join("\n");
    assert!(collapsed.contains("meta hidden"));
    assert!(!collapsed.contains("tool=write_file"));
    assert!(
        app.events.iter().any(|event| {
            event.label == "approval:view" && event.detail == "metadata collapsed"
        })
    );

    assert!(
        app.handle_key_event(KeyEvent::new(KeyCode::Char('m'), KeyModifiers::NONE))?
            .is_none()
    );
    let reexpanded = app.approval_preview_lines().join("\n");
    assert!(reexpanded.contains("tool=write_file"));
    assert!(
        app.events
            .iter()
            .any(|event| { event.label == "approval:view" && event.detail == "metadata expanded" })
    );
    Ok(())
}

#[test]
fn approval_hunk_and_file_navigation_updates_selection() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    inject_write_file_approval(&mut app, multi_file_approval_preview())?;

    assert_eq!(app.approval_selected_file_index, 0);
    assert_eq!(app.approval_selected_hunk_index, 0);

    assert!(
        app.handle_key_event(KeyEvent::new(KeyCode::Char(']'), KeyModifiers::NONE))?
            .is_none()
    );
    assert_eq!(app.approval_selected_hunk_index, 1);
    assert!(app.approval_scroll_back > 0);
    let jumped = app.approval_preview_lines().join("\n");
    assert!(jumped.contains("hunk 2/2"));

    assert!(
        app.handle_key_event(KeyEvent::new(KeyCode::Char('.'), KeyModifiers::NONE))?
            .is_none()
    );
    assert_eq!(app.approval_selected_file_index, 1);
    assert_eq!(app.approval_selected_hunk_index, 0);
    assert_eq!(app.approval_scroll_back, 0);
    let second_file = app.approval_preview_lines().join("\n");
    assert!(second_file.contains("file 2/2"));
    assert!(second_file.contains("> note-b.txt"));

    assert!(
        app.handle_key_event(KeyEvent::new(KeyCode::Char(','), KeyModifiers::NONE))?
            .is_none()
    );
    assert_eq!(app.approval_selected_file_index, 0);
    let first_file = app.approval_preview_lines().join("\n");
    assert!(first_file.contains("file 1/2"));
    assert!(first_file.contains("> note-a.txt"));
    Ok(())
}

#[test]
fn approval_modal_view_tracks_selected_hunk() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    inject_write_file_approval(&mut app, multi_file_approval_preview())?;

    assert!(
        app.handle_key_event(KeyEvent::new(KeyCode::Char(']'), KeyModifiers::NONE))?
            .is_none()
    );

    let view = app
        .approval_modal_view()
        .expect("approval modal view should exist");
    assert_eq!(view.diff_label, "note-a.txt");
    assert_eq!(view.active_hunk_index, 2);
    assert_eq!(view.hunk_total, 2);
    assert!(
        view.file_rows
            .iter()
            .any(|row| row.path == "note-a.txt" && row.selected)
    );
    assert!(view.diff_lines.iter().any(|line| {
        line.active_hunk
            && line.kind == super::ApprovalDiffLineKind::Hunk
            && line.text.contains("@@ -5,2 +5,2 @@")
    }));
    Ok(())
}

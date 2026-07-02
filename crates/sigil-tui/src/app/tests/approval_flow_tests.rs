use super::*;

#[test]
fn approval_request_stores_preview() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    inject_write_file_approval(&mut app, sample_approval_preview())?;

    let pending = app.approval.pending.expect("expected pending approval");
    let preview = pending.preview.expect("expected preview");
    assert_eq!(preview.changed_files, vec!["note.txt".to_owned()]);
    assert!(preview.body.contains("+++ proposed/note.txt"));
    Ok(())
}

#[test]
fn approval_request_projects_source_agent_route() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    let source_thread_id = sigil_kernel::AgentThreadId::new("thread_1")?;
    app.sync_current_session_state(vec![
        SessionLogEntry::Control(ControlEntry::AgentThreadDisplayName(
            sigil_kernel::AgentThreadDisplayNameEntry {
                thread_id: source_thread_id.clone(),
                display_name: "Kernel Mapper".to_owned(),
            },
        )),
        SessionLogEntry::Control(ControlEntry::AgentApprovalRoute(
            sigil_kernel::AgentApprovalRouteEntry {
                route_id: sigil_kernel::AgentRouteId::new("approval_route_1")?,
                source_thread_id,
                target_thread_id: Some(sigil_kernel::AgentThreadId::new("main")?),
                call_id: "call-1".to_owned(),
                tool_name: "write_file".to_owned(),
                status: sigil_kernel::AgentRouteStatus::Requested,
            },
        )),
    ]);
    inject_write_file_approval(&mut app, sample_approval_preview())?;

    let lines = app.approval_preview_lines().join("\n");
    assert!(lines.contains("source_agent=Kernel Mapper · thread_1"));
    let view = app
        .approval_modal_view()
        .expect("approval modal view should exist");
    assert_eq!(
        view.source_agent.as_deref(),
        Some("Kernel Mapper · thread_1")
    );
    Ok(())
}

#[test]
fn approval_request_without_preview_uses_visible_fallback() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    let source_thread_id = sigil_kernel::AgentThreadId::new("thread_mcp")?;
    app.sync_current_session_state(vec![
        SessionLogEntry::Control(ControlEntry::AgentThreadDisplayName(
            sigil_kernel::AgentThreadDisplayNameEntry {
                thread_id: source_thread_id.clone(),
                display_name: "MCP Agent".to_owned(),
            },
        )),
        SessionLogEntry::Control(ControlEntry::AgentApprovalRoute(
            sigil_kernel::AgentApprovalRouteEntry {
                route_id: sigil_kernel::AgentRouteId::new("approval_route_mcp")?,
                source_thread_id,
                target_thread_id: Some(sigil_kernel::AgentThreadId::new("main")?),
                call_id: "call-mcp-1".to_owned(),
                tool_name: "remote_tool".to_owned(),
                status: sigil_kernel::AgentRouteStatus::Requested,
            },
        )),
    ]);
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
        operation: sigil_kernel::ToolOperation::NetworkRequest,
        risk: sigil_kernel::PermissionRisk::High,
        subject_zones: Vec::new(),
        confirmation: None,
        snapshot_required: false,
        preview: None,
    })?;

    let lines = app.approval_preview_lines().join("\n");
    assert!(lines.contains("tool=remote_tool"));
    assert!(lines.contains("source_agent=MCP Agent · thread_mcp"));
    assert!(lines.contains("mode=mcp network"));
    assert!(lines.contains(r#"args={"query":"status"}"#));

    let view = app
        .approval_modal_view()
        .expect("approval modal view should exist");
    assert_eq!(view.preview_title, "Run remote_tool");
    assert_eq!(view.source_agent.as_deref(), Some("MCP Agent · thread_mcp"));
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
fn approval_permission_metadata_lines_cover_label_variants() -> Result<()> {
    let operations = [
        (sigil_kernel::ToolOperation::Read, "operation=read"),
        (sigil_kernel::ToolOperation::Search, "operation=search"),
        (
            sigil_kernel::ToolOperation::CreateFile,
            "operation=create file",
        ),
        (sigil_kernel::ToolOperation::EditFile, "operation=edit file"),
        (
            sigil_kernel::ToolOperation::OverwriteFile,
            "operation=overwrite file",
        ),
        (
            sigil_kernel::ToolOperation::DeleteFile,
            "operation=delete file",
        ),
        (
            sigil_kernel::ToolOperation::RenamePath,
            "operation=rename path",
        ),
        (
            sigil_kernel::ToolOperation::CreateDirectory,
            "operation=create directory",
        ),
        (
            sigil_kernel::ToolOperation::DeleteDirectory,
            "operation=delete directory",
        ),
        (
            sigil_kernel::ToolOperation::RecursiveDelete,
            "operation=recursive delete",
        ),
        (
            sigil_kernel::ToolOperation::ApplyChangeSet,
            "operation=apply change set",
        ),
        (
            sigil_kernel::ToolOperation::ExecuteReadOnlyCommand,
            "operation=run read-only command",
        ),
        (
            sigil_kernel::ToolOperation::ExecuteMutatingCommand,
            "operation=run mutating command",
        ),
        (
            sigil_kernel::ToolOperation::ExecuteUnknownCommand,
            "operation=run command",
        ),
        (
            sigil_kernel::ToolOperation::ExecuteDestructiveCommand,
            "operation=run destructive command",
        ),
        (
            sigil_kernel::ToolOperation::SendTerminalInput,
            "operation=send terminal input",
        ),
        (
            sigil_kernel::ToolOperation::NetworkRequest,
            "operation=network request",
        ),
        (
            sigil_kernel::ToolOperation::SpawnAgent,
            "operation=spawn agent",
        ),
        (
            sigil_kernel::ToolOperation::MessageAgent,
            "operation=message agent",
        ),
        (
            sigil_kernel::ToolOperation::CloseAgent,
            "operation=close agent",
        ),
        (
            sigil_kernel::ToolOperation::LoadSkill,
            "operation=load skill",
        ),
        (
            sigil_kernel::ToolOperation::InvokePlugin,
            "operation=invoke plugin",
        ),
    ];
    for (operation, expected) in operations {
        let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
        app.handle(RunEvent::ToolApprovalRequested {
            call: ToolCall {
                id: "call-meta".to_owned(),
                name: "meta_tool".to_owned(),
                args_json: "{}".to_owned(),
            },
            spec: ToolSpec {
                name: "meta_tool".to_owned(),
                description: "Meta tool".to_owned(),
                input_schema: json!({"type":"object"}),
                category: ToolCategory::Custom,
                access: ToolAccess::Execute,
                preview: ToolPreviewCapability::None,
            },
            subjects: Vec::new(),
            operation,
            risk: sigil_kernel::PermissionRisk::Low,
            subject_zones: Vec::new(),
            confirmation: None,
            snapshot_required: false,
            preview: None,
        })?;
        assert!(app.approval_preview_lines().join("\n").contains(expected));
    }

    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.handle(RunEvent::ToolApprovalRequested {
        call: ToolCall {
            id: "call-risk".to_owned(),
            name: "risk_tool".to_owned(),
            args_json: "{}".to_owned(),
        },
        spec: ToolSpec {
            name: "risk_tool".to_owned(),
            description: "Risk tool".to_owned(),
            input_schema: json!({"type":"object"}),
            category: ToolCategory::Custom,
            access: ToolAccess::Write,
            preview: ToolPreviewCapability::None,
        },
        subjects: Vec::new(),
        operation: sigil_kernel::ToolOperation::DeleteFile,
        risk: sigil_kernel::PermissionRisk::Protected,
        subject_zones: vec![
            sigil_kernel::PathTrustZone::WorkspaceSource,
            sigil_kernel::PathTrustZone::WorkspaceDocs,
            sigil_kernel::PathTrustZone::WorkspaceProjectAsset,
            sigil_kernel::PathTrustZone::WorkspaceRuntimeState,
            sigil_kernel::PathTrustZone::WorkspaceIgnored,
            sigil_kernel::PathTrustZone::WorkspaceGitMetadata,
            sigil_kernel::PathTrustZone::WorkspaceConfigSecret,
            sigil_kernel::PathTrustZone::UserState,
            sigil_kernel::PathTrustZone::UserCache,
            sigil_kernel::PathTrustZone::External,
            sigil_kernel::PathTrustZone::Unknown,
        ],
        confirmation: Some(sigil_kernel::PermissionConfirmation::TypePhrase {
            phrase: "DELETE".to_owned(),
        }),
        snapshot_required: true,
        preview: None,
    })?;
    let lines = app.approval_preview_lines().join("\n");
    assert!(lines.contains("risk=protected"));
    assert!(lines.contains("workspace source"));
    assert!(lines.contains("workspace docs"));
    assert!(lines.contains("project asset"));
    assert!(lines.contains("runtime state"));
    assert!(lines.contains("ignored file"));
    assert!(lines.contains("git metadata"));
    assert!(lines.contains("config or secret"));
    assert!(lines.contains("user state"));
    assert!(lines.contains("user cache"));
    assert!(lines.contains("external path"));
    assert!(lines.contains("unknown"));
    assert!(lines.contains("confirmation=type the requested phrase before approval"));
    assert!(lines.contains("recovery=pre-change snapshot required"));

    for (confirmation, expected) in [
        (
            Some(sigil_kernel::PermissionConfirmation::Standard),
            "confirmation=standard approval",
        ),
        (
            Some(sigil_kernel::PermissionConfirmation::TypePath),
            "confirmation=type the path before approval",
        ),
        (None, "risk=destructive"),
    ] {
        let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
        app.handle(RunEvent::ToolApprovalRequested {
            call: ToolCall {
                id: "call-confirmation".to_owned(),
                name: "confirmation_tool".to_owned(),
                args_json: "{}".to_owned(),
            },
            spec: ToolSpec {
                name: "confirmation_tool".to_owned(),
                description: "Confirmation tool".to_owned(),
                input_schema: json!({"type":"object"}),
                category: ToolCategory::Custom,
                access: ToolAccess::Write,
                preview: ToolPreviewCapability::None,
            },
            subjects: Vec::new(),
            operation: sigil_kernel::ToolOperation::DeleteFile,
            risk: sigil_kernel::PermissionRisk::Destructive,
            subject_zones: Vec::new(),
            confirmation,
            snapshot_required: false,
            preview: None,
        })?;
        assert!(app.approval_preview_lines().join("\n").contains(expected));
    }
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
fn approval_modal_view_projects_apply_changeset_metadata() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.handle(RunEvent::ToolApprovalRequested {
        call: ToolCall {
            id: "call-change-set".to_owned(),
            name: "apply_changeset".to_owned(),
            args_json: json!({
                "id": "change-123",
                "risk": "high",
                "files": [
                    {
                        "path": "src/lib.rs",
                        "action": "update",
                        "risk": "high",
                        "content": "pub fn changed() {}\n"
                    },
                    {
                        "path": "README.md",
                        "action": "create",
                        "content": "# Docs\n"
                    }
                ]
            })
            .to_string(),
        },
        spec: ToolSpec {
            name: "apply_changeset".to_owned(),
            description: "Apply change set".to_owned(),
            input_schema: json!({"type":"object"}),
            category: ToolCategory::File,
            access: ToolAccess::Write,
            preview: ToolPreviewCapability::Required,
        },
        subjects: Vec::new(),
        operation: sigil_kernel::ToolOperation::ApplyChangeSet,
        risk: sigil_kernel::PermissionRisk::Destructive,
        subject_zones: Vec::new(),
        confirmation: None,
        snapshot_required: true,
        preview: Some(ToolPreview {
            title: "Apply change set change-123".to_owned(),
            summary: "2 files, risk=high".to_owned(),
            body: String::new(),
            changed_files: vec!["src/lib.rs".to_owned(), "README.md".to_owned()],
            file_diffs: vec![
                sigil_kernel::ToolPreviewFile {
                    path: "src/lib.rs".to_owned(),
                    diff:
                        "--- current/src/lib.rs\n+++ proposed/src/lib.rs\n@@ -1 +1 @@\n-old\n+new"
                            .to_owned(),
                },
                sigil_kernel::ToolPreviewFile {
                    path: "README.md".to_owned(),
                    diff: "--- current/README.md\n+++ proposed/README.md\n@@ -0,0 +1 @@\n+# Docs"
                        .to_owned(),
                },
            ],
        }),
    })?;

    let view = app
        .approval_modal_view()
        .expect("approval modal view should exist");
    let change_set = view.change_set.expect("change set metadata should exist");

    assert_eq!(change_set.id, "change-123");
    assert_eq!(change_set.risk, "high");
    assert!(change_set.format_hint.contains("cargo fmt --all"));
    assert!(change_set.format_hint.contains("Markdown"));
    assert_eq!(view.file_rows[0].action.as_deref(), Some("update"));
    assert_eq!(view.file_rows[0].risk.as_deref(), Some("high"));
    assert_eq!(view.file_rows[1].action.as_deref(), Some("create"));
    assert_eq!(view.file_rows[1].risk.as_deref(), Some("high"));
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
        operation: sigil_kernel::ToolOperation::Read,
        risk: sigil_kernel::PermissionRisk::Low,
        subject_zones: vec![sigil_kernel::PathTrustZone::External],
        confirmation: None,
        snapshot_required: false,
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
fn spawn_agent_approval_key_can_switch_call_to_background() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.handle(RunEvent::ToolApprovalRequested {
        call: ToolCall {
            id: "call-spawn-agent".to_owned(),
            name: sigil_runtime::SPAWN_AGENT_TOOL_NAME.to_owned(),
            args_json: json!({
                "profile_id": "explore",
                "objective": "inspect kernel",
                "prompt": "inspect kernel",
                "mode": "join_before_final"
            })
            .to_string(),
        },
        spec: ToolSpec {
            name: sigil_runtime::SPAWN_AGENT_TOOL_NAME.to_owned(),
            description: "Spawn agent".to_owned(),
            input_schema: json!({"type":"object"}),
            category: ToolCategory::Agent,
            access: ToolAccess::Execute,
            preview: ToolPreviewCapability::Required,
        },
        subjects: Vec::new(),
        operation: sigil_kernel::ToolOperation::SpawnAgent,
        risk: sigil_kernel::PermissionRisk::High,
        subject_zones: Vec::new(),
        confirmation: None,
        snapshot_required: false,
        preview: None,
    })?;

    let lines = app.approval_preview_lines().join("\n");
    assert!(lines.contains("B background"));
    let action = app.handle_key_event(KeyEvent::new(KeyCode::Char('b'), KeyModifiers::NONE))?;
    let Some(AppAction::ApprovalDecisionWithArgs { call_id, args_json }) = action else {
        panic!("expected approval decision with rewritten args");
    };
    assert_eq!(call_id, "call-spawn-agent");
    let args: serde_json::Value = serde_json::from_str(&args_json)?;
    assert_eq!(args["mode"], "background");
    assert_eq!(args["profile_id"], "explore");
    Ok(())
}

#[test]
fn approval_enter_chooses_selected_action() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    inject_write_file_approval(&mut app, sample_approval_preview())?;

    assert_eq!(app.approval.selected_action, ApprovalAction::AllowOnce);
    assert_eq!(
        app.approval_modal_view()
            .expect("approval modal should exist")
            .selected_action,
        ApprovalAction::AllowOnce
    );
    let allow = app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))?;
    assert!(matches!(
        allow,
        Some(AppAction::ApprovalDecision { call_id, approved })
            if call_id == "call-1" && approved
    ));

    app.handle_key_event(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE))?;
    assert_eq!(app.approval.selected_action, ApprovalAction::Deny);
    let deny = app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))?;
    assert!(matches!(
        deny,
        Some(AppAction::ApprovalDecision { call_id, approved })
            if call_id == "call-1" && !approved
    ));
    assert!(
        app.events
            .iter()
            .any(|event| event.label == "approval:action" && event.detail == "Deny")
    );
    Ok(())
}

#[test]
fn approval_enter_can_choose_session_grant_when_available() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    inject_write_file_approval(&mut app, sample_approval_preview())?;
    app.approval
        .pending
        .as_mut()
        .expect("pending approval")
        .session_grant_available = true;

    app.handle_key_event(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE))?;
    assert_eq!(app.approval.selected_action, ApprovalAction::AllowSession);
    let action = app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))?;

    assert!(matches!(
        action,
        Some(AppAction::ApprovalSessionDecision { call_id }) if call_id == "call-1"
    ));
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

    assert_eq!(app.approval.selected_file_index, 0);
    assert_eq!(app.approval.selected_hunk_index, 0);

    assert!(
        app.handle_key_event(KeyEvent::new(KeyCode::Char(']'), KeyModifiers::NONE))?
            .is_none()
    );
    assert_eq!(app.approval.selected_hunk_index, 1);
    assert!(app.approval.scroll_back > 0);
    let jumped = app.approval_preview_lines().join("\n");
    assert!(jumped.contains("hunk 2/2"));

    assert!(
        app.handle_key_event(KeyEvent::new(KeyCode::Char('.'), KeyModifiers::NONE))?
            .is_none()
    );
    assert_eq!(app.approval.selected_file_index, 1);
    assert_eq!(app.approval.selected_hunk_index, 0);
    assert_eq!(app.approval.scroll_back, 0);
    let second_file = app.approval_preview_lines().join("\n");
    assert!(second_file.contains("file 2/2"));
    assert!(second_file.contains("> note-b.txt"));

    assert!(
        app.handle_key_event(KeyEvent::new(KeyCode::Char(','), KeyModifiers::NONE))?
            .is_none()
    );
    assert_eq!(app.approval.selected_file_index, 0);
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

#[test]
fn approval_resolved_updates_timeline_for_allow_and_deny() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    inject_write_file_approval(&mut app, sample_approval_preview())?;

    app.handle(RunEvent::ToolApprovalResolved {
        call_id: "call-1".to_owned(),
        approved: false,
        reason: Some("policy denied".to_owned()),
    })?;
    assert!(app.approval.pending.is_none());
    assert_eq!(app.active_pane, PaneFocus::Composer);
    assert!(
        app.timeline
            .iter()
            .any(|entry| entry.text.contains("Denied call-1: policy denied"))
    );

    inject_write_file_approval(&mut app, sample_approval_preview())?;
    app.handle(RunEvent::ToolApprovalResolved {
        call_id: "call-1".to_owned(),
        approved: true,
        reason: None,
    })?;
    assert!(
        app.timeline
            .iter()
            .any(|entry| entry.text.contains("Approved call-1."))
    );
    Ok(())
}

#[test]
fn approval_preview_handles_empty_preview_body_and_slash_shortcut() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    inject_write_file_approval(
        &mut app,
        ToolPreview {
            title: "Preview".to_owned(),
            summary: "Summary".to_owned(),
            body: String::new(),
            changed_files: vec!["note.txt".to_owned()],
            file_diffs: Vec::new(),
        },
    )?;

    let view = app
        .approval_modal_view()
        .expect("approval modal should exist");
    assert_eq!(view.diff_lines[0].text, "No preview body available.");

    assert!(
        app.handle_key_event(KeyEvent::new(KeyCode::Char('/'), KeyModifiers::NONE))?
            .is_none()
    );
    assert_eq!(app.active_pane, PaneFocus::Composer);
    assert_eq!(app.composer.input, "/");
    Ok(())
}

#[test]
fn approval_preview_lines_cover_changed_files_scroll_keys_and_escape() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.handle(RunEvent::ToolApprovalRequested {
        call: ToolCall {
            id: "call-plain-1".to_owned(),
            name: "write_file".to_owned(),
            args_json: r#"{"path":"note.txt"}"#.to_owned(),
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
        operation: sigil_kernel::ToolOperation::OverwriteFile,
        risk: sigil_kernel::PermissionRisk::Medium,
        subject_zones: Vec::new(),
        confirmation: None,
        snapshot_required: false,
        preview: Some(ToolPreview {
            title: "Plain preview".to_owned(),
            summary: String::new(),
            body: "plain body".to_owned(),
            changed_files: vec!["note.txt".to_owned()],
            file_diffs: Vec::new(),
        }),
    })?;

    let lines = app.approval_preview_lines().join("\n");
    assert!(lines.contains("changed: note.txt"));
    assert!(lines.contains("hunk 0/0"));

    for code in [
        KeyCode::Char('['),
        KeyCode::Up,
        KeyCode::Down,
        KeyCode::PageUp,
        KeyCode::PageDown,
        KeyCode::Home,
        KeyCode::End,
        KeyCode::Char('x'),
        KeyCode::Backspace,
        KeyCode::Tab,
        KeyCode::BackTab,
    ] {
        assert!(
            app.handle_key_event(KeyEvent::new(code, KeyModifiers::NONE))?
                .is_none()
        );
    }

    app.active_pane = PaneFocus::Composer;
    assert!(
        app.handle_key_event(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE))?
            .is_none()
    );
    assert_eq!(app.active_pane, PaneFocus::Activity);
    Ok(())
}

#[test]
fn escape_in_pending_approval_only_changes_focus() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.active_pane = PaneFocus::Composer;
    inject_write_file_approval(&mut app, sample_approval_preview())?;

    let action = app.handle_key_event(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE))?;

    assert!(action.is_none());
    assert_eq!(app.active_pane, PaneFocus::Activity);
    assert!(app.approval.pending.is_some());
    assert!(app.approval_modal_view().is_some());
    Ok(())
}

#[test]
fn slash_prefix_during_pending_approval_returns_to_composer() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.active_pane = PaneFocus::Activity;
    inject_write_file_approval(&mut app, sample_approval_preview())?;

    let action = app.handle_key_event(KeyEvent::new(KeyCode::Char('/'), KeyModifiers::NONE))?;

    assert!(action.is_none());
    assert_eq!(app.active_pane, PaneFocus::Composer);
    assert_eq!(app.composer.input, "/");
    assert!(app.approval.pending.is_some());
    Ok(())
}

use super::*;

fn sync_smoke_child_agent(app: &mut AppState) -> Result<()> {
    app.sync_current_session_state(child_agent_entries(
        Some("perm-review"),
        sigil_kernel::AgentThreadStatus::Completed,
        sigil_kernel::SessionRef::new_relative("children/task_1/step_1-child_1.jsonl")?,
    )?);
    Ok(())
}

#[test]
fn product_smoke_workspace_check_permission_mode_once_and_can_select_session() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());

    app.handle(RunEvent::ToolApprovalRequested {
        call: ToolCall {
            id: "call-cargo-check".to_owned(),
            name: "bash".to_owned(),
            args_json: r#"{"command":"cargo check 2>&1"}"#.to_owned(),
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
        subjects: vec![ToolSubject::command(
            "family:cargo_check",
            "family:cargo_check",
        )],
        network_effect: None,
        local_policy_decision: sigil_kernel::ApprovalMode::Ask,
        network_policy_decision: sigil_kernel::ApprovalMode::Allow,
        source_policy_decision: sigil_kernel::ApprovalMode::Allow,
        operation: sigil_kernel::ToolOperation::ExecuteWorkspaceCheckCommand,
        risk: sigil_kernel::PermissionRisk::Medium,
        subject_zones: vec![sigil_kernel::PathTrustZone::Unknown],
        confirmation: None,
        snapshot_required: false,
        command_permission_matches: Vec::new(),
        preview: Some(ToolPreview {
            title: "Run workspace check".to_owned(),
            summary: "Runs a workspace build check through bash.".to_owned(),
            body: "cargo check 2>&1".to_owned(),
            changed_files: Vec::new(),
            file_diffs: Vec::new(),
        }),
    })?;

    let view = app
        .approval_modal_view()
        .expect("workspace check approval should be visible");
    assert_eq!(view.preview_title, "Run workspace check");
    assert!(view.preview_summary.contains("cargo check 2>&1"));
    assert!(view.preview_summary.contains("Reason:"));
    assert!(view.preview_summary.contains("Access:"));
    assert!(view.preview_summary.contains("Session grant:"));
    assert_eq!(view.selected_action, ApprovalAction::AllowOnce);
    assert!(view.session_grant_available);

    app.handle_key_event(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE))?;
    assert_eq!(app.approval.selected_action, ApprovalAction::AllowSession);
    let action = app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))?;
    assert!(matches!(
        action,
        Some(AppAction::ApprovalSessionDecision { call_id }) if call_id == "call-cargo-check"
    ));
    Ok(())
}

#[test]
fn product_smoke_down_selects_visible_composer_agent_row() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    sync_smoke_child_agent(&mut app)?;
    app.active_pane = PaneFocus::Composer;
    app.composer.input.clear();
    app.composer.agent_panel_focused = false;
    app.sidebar_agent_selected = 0;

    app.handle_key_event(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE))?;

    assert!(app.is_composer_agent_panel_focused());
    assert_eq!(app.sidebar_agent_selected, 1);
    assert_eq!(app.last_notice.as_deref(), Some("agent list focused"));
    Ok(())
}

#[test]
fn product_smoke_multiline_user_prompt_renders_as_one_user_entry() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.composer.input = "第一段\n\n第二段\n\n第三段".to_owned();
    app.composer.input_cursor = app.composer.input.chars().count();

    let action = app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))?;

    assert!(matches!(action, Some(AppAction::SubmitPrompt(prompt)) if prompt.contains("第二段")));
    let user_entries = app
        .timeline
        .iter()
        .filter(|entry| entry.role == TimelineRole::User)
        .collect::<Vec<_>>();
    assert_eq!(user_entries.len(), 1);
    assert_eq!(user_entries[0].text, "第一段\n\n第二段\n\n第三段");
    Ok(())
}

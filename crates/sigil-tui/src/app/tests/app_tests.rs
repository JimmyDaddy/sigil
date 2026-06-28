use super::super::ComposerMode;
use super::*;

#[test]
fn top_level_plan_agent_and_task_key_paths_cover_edge_states() -> Result<()> {
    assert_eq!(ComposerMode::Build.notice(), "thinking");
    assert_eq!(ComposerMode::Plan.notice(), "planning");
    assert_eq!(ComposerMode::Build.phase_marker(), "thinking");
    assert_eq!(ComposerMode::Plan.phase_marker(), "plan");

    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.composer_mode = ComposerMode::Plan;
    let action = app.handle_key_event(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE))?;
    assert!(action.is_none());
    assert_eq!(app.composer_mode_label(), "Build");
    assert_eq!(app.last_notice(), Some("build mode"));

    app.set_pending_plan_approval_from_text("  ");
    assert!(app.pending_plan_approval().is_none());
    app.set_pending_plan_approval_from_text("1. inspect");
    let ignored = app.handle_key_event(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::CONTROL))?;
    assert!(ignored.is_none());
    assert!(app.pending_plan_approval().is_some());
    let ignored = app.handle_key_event(KeyEvent::new(KeyCode::Char('z'), KeyModifiers::NONE))?;
    assert!(ignored.is_none());
    assert!(app.pending_plan_approval().is_some());
    let approved = app.handle_key_event(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE))?;
    assert!(matches!(
        approved,
        Some(AppAction::ApprovePlan {
            permission: sigil_kernel::PlanApprovalPermission::Ask,
            clear_planning_context: true,
            ..
        })
    ));

    app.is_busy = true;
    app.input = "@review inspect".to_owned();
    let action = app.submit_input()?;
    assert!(matches!(
        action,
        Some(AppAction::QueueConversationInput {
            prompt,
            kind: sigil_kernel::ConversationInputKind::Chat,
            target: sigil_kernel::ConversationInputTarget::MainThread,
        }) if prompt == "@review inspect"
    ));
    assert_eq!(
        app.timeline.last().map(|entry| entry.text.as_str()),
        Some("queued for next turn")
    );

    app.input = "/task implement".to_owned();
    let action = app.submit_input()?;
    assert!(action.is_none());
    assert_eq!(
        app.timeline.last().map(|entry| entry.text.as_str()),
        Some("busy; task later")
    );

    app.is_busy = false;
    app.input = "/task".to_owned();
    let action = app.submit_input()?;
    assert!(action.is_none());
    assert_eq!(app.last_notice(), Some("usage: /task <task|continue>"));
    Ok(())
}

#[test]
fn from_root_config_initializes_mcp_statuses_from_startup_mode() {
    let mut config = test_config();
    config.mcp_servers.push(sigil_kernel::McpServerConfig {
        name: "eager".to_owned(),
        command: "mcp-eager".to_owned(),
        startup: McpServerStartup::Eager,
        ..Default::default()
    });
    config.mcp_servers.push(sigil_kernel::McpServerConfig {
        name: "lazy".to_owned(),
        command: "mcp-lazy".to_owned(),
        startup: McpServerStartup::Lazy,
        required: false,
        ..Default::default()
    });

    let app = AppState::from_root_config(Path::new("sigil.toml"), &config);

    assert_eq!(
        app.mcp_server_runtime_status_label("eager").as_deref(),
        Some("activating")
    );
    assert_eq!(
        app.mcp_server_runtime_status_label("lazy").as_deref(),
        Some("deferred")
    );
    assert_eq!(
        app.mcp_sidebar_lines(),
        vec!["eager: activating".to_owned(), "lazy: deferred".to_owned()]
    );
}

#[test]
fn terminal_capability_helpers_default_on_and_follow_config() {
    let setup_app = AppState::from_setup(
        Path::new("sigil.toml").to_path_buf(),
        Path::new(".").to_path_buf(),
        None,
    );
    assert!(setup_app.terminal_mouse_capture_enabled());
    assert!(setup_app.terminal_osc52_clipboard_enabled());

    let mut config = test_config();
    config.terminal.mouse_capture = false;
    config.terminal.osc52_clipboard = false;
    let app = AppState::from_root_config(Path::new("sigil.toml"), &config);

    assert!(!app.terminal_mouse_capture_enabled());
    assert!(!app.terminal_osc52_clipboard_enabled());
}

#[test]
fn terminal_task_sidebar_lines_project_running_count() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.sync_current_session_state(vec![
        SessionLogEntry::Control(ControlEntry::TerminalTask(test_terminal_entry(
            "terminal-1",
            sigil_kernel::TerminalTaskStatus::Running,
        )?)),
        SessionLogEntry::Control(ControlEntry::TerminalTask(test_terminal_entry(
            "terminal-2",
            sigil_kernel::TerminalTaskStatus::Exited { exit_code: Some(0) },
        )?)),
    ]);

    let lines = app.task_sidebar_lines();

    assert!(lines.contains(&"terminal: 1 running".to_owned()));
    assert!(lines.contains(&"terminal latest: terminal-1 running".to_owned()));
    Ok(())
}

#[test]
fn focused_terminal_task_cancel_requires_confirmation() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.handle(RunEvent::Control(ControlEntry::TerminalTask(
        test_terminal_entry("terminal-1", sigil_kernel::TerminalTaskStatus::Running)?,
    )))?;

    let first = app.handle_key_event(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::ALT))?;
    assert!(first.is_none());
    assert_eq!(
        app.last_notice(),
        Some("Alt-X again to cancel terminal task terminal-1")
    );

    let second = app.handle_key_event(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::ALT))?;
    assert!(matches!(
        second,
        Some(AppAction::CancelTerminalTask { task_id }) if task_id == "terminal-1"
    ));
    Ok(())
}

fn sync_agent_task(
    app: &mut AppState,
    display_name: Option<&str>,
    child_status: sigil_kernel::TaskChildSessionStatus,
    child_session_ref: sigil_kernel::SessionRef,
) -> Result<()> {
    let task_id = sigil_kernel::TaskId::new("task_1")?;
    let step_id = sigil_kernel::TaskStepId::new("step_1")?;
    app.sync_current_session_state(vec![
        SessionLogEntry::Control(ControlEntry::TaskRun(sigil_kernel::TaskRunEntry {
            task_id: task_id.clone(),
            parent_session_ref: sigil_kernel::SessionRef::new_relative("parent.jsonl")?,
            objective: "review workspace".to_owned(),
            status: sigil_kernel::TaskRunStatus::Running,
            reason: None,
        })),
        SessionLogEntry::Control(ControlEntry::TaskPlan(sigil_kernel::TaskPlanEntry {
            task_id: task_id.clone(),
            plan_version: 1,
            status: sigil_kernel::TaskPlanStatus::Accepted,
            steps: vec![sigil_kernel::TaskStepSpec {
                step_id: step_id.clone(),
                title: "Inspect repository".to_owned(),
                display_name: display_name.map(ToOwned::to_owned),
                detail: None,
                role: sigil_kernel::AgentRole::SubagentRead,
                depends_on: Vec::new(),
                mode: None,
                isolation: None,
            }],
            reason: None,
        })),
        SessionLogEntry::Control(ControlEntry::TaskChildSession(
            sigil_kernel::TaskChildSessionEntry {
                task_id,
                plan_version: 1,
                step_id,
                child_task_id: sigil_kernel::TaskId::new("child_1")?,
                child_session_ref,
                role: sigil_kernel::AgentRole::SubagentRead,
                status: child_status,
                summary_hash: None,
            },
        )),
    ]);
    Ok(())
}

#[test]
fn agent_command_edges_cover_unavailable_rows_and_usage() -> Result<()> {
    let task_id = sigil_kernel::TaskId::new("task_1")?;
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());

    app.activate_agent_from_command("")?;
    assert_eq!(
        app.last_notice(),
        Some("usage: /agent <main|next|prev|child-id|rename target name>")
    );
    app.activate_agent_from_command("rename")?;
    assert_eq!(
        app.last_notice(),
        Some("usage: /agent rename <child-id|current> <name>")
    );
    app.activate_agent_from_command("close")?;
    assert_eq!(
        app.last_notice(),
        Some("usage: /agent close <agent|current>")
    );
    app.activate_agent_from_command("cancel")?;
    assert_eq!(
        app.last_notice(),
        Some("usage: /agent cancel <agent|current>")
    );
    app.activate_agent_from_command("close missing")?;
    assert_eq!(app.last_notice(), Some("agent not found: missing"));
    app.activate_agent_from_command("cancel missing")?;
    assert_eq!(app.last_notice(), Some("agent not found: missing"));
    app.activate_agent_from_command("missing")?;
    assert_eq!(app.last_notice(), Some("agent not found: missing"));
    app.activate_agent_from_command("next")?;
    assert_eq!(app.last_notice(), Some("no child agents to switch"));
    app.active_agent_view = super::super::AgentView::Child {
        child_task_id: "orphan".to_owned(),
        child_session_ref: sigil_kernel::SessionRef::new_relative("children/orphan.jsonl")?,
    };
    app.activate_agent_from_command("close current")?;
    assert_eq!(app.last_notice(), Some("agent close unavailable: current"));
    app.activate_agent_from_command("cancel current")?;
    assert_eq!(app.last_notice(), Some("agent cancel unavailable: current"));
    app.active_agent_view = super::super::AgentView::Main;
    app.activate_agent_from_command("rename current Main Agent")?;
    assert_eq!(app.last_notice(), Some("agent not found: current"));
    app.activate_agent_from_command("message")?;
    assert_eq!(
        app.last_notice(),
        Some("usage: /agent message <agent|current> <prompt>")
    );
    app.activate_agent_from_command("steer current keep going")?;
    assert_eq!(app.last_notice(), Some("agent not found: current"));

    app.sync_current_session_state(vec![SessionLogEntry::Control(ControlEntry::TaskRun(
        sigil_kernel::TaskRunEntry {
            task_id,
            parent_session_ref: sigil_kernel::SessionRef::new_relative("parent.jsonl")?,
            objective: "review workspace".to_owned(),
            status: sigil_kernel::TaskRunStatus::Running,
            reason: None,
        },
    ))]);
    let rows = app.agent_sidebar_rows();
    assert!(
        rows.iter()
            .any(|row| row.label == "agents" && row.detail == "no child agents recorded")
    );
    app.active_pane = PaneFocus::Activity;
    app.sidebar_selected_card = SidebarCard::Agents;
    app.sidebar_agent_selected = 1;
    app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))?;
    assert_eq!(
        app.last_notice(),
        Some("agent focus unavailable: no child agents recorded")
    );
    assert!(
        app.timeline
            .iter()
            .any(|entry| entry.text == "agent focus unavailable: no child agents recorded")
    );
    Ok(())
}

#[test]
fn agent_rename_filters_and_persists_display_name() -> Result<()> {
    let temp = tempdir()?;
    let child_ref = sigil_kernel::SessionRef::new_relative("children/task_1/step_1-child_1.jsonl")?;
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.session_log_path = temp.path().join(".sigil/sessions/session-parent.jsonl");
    sync_agent_task(
        &mut app,
        None,
        sigil_kernel::TaskChildSessionStatus::Completed,
        child_ref,
    )?;

    assert!(
        app.agent_slash_entries("rename child")
            .iter()
            .any(|entry| entry.fill == "/agent rename child_1 ")
    );
    assert!(app.agent_slash_entries("rename no-match").is_empty());
    assert!(!app.agent_selector_allows_popup("rename child_1 Repo Audit"));
    assert!(!app.agent_selector_allows_popup("close current"));
    assert!(!app.agent_selector_allows_popup("message child_1 retry with more detail"));
    app.activate_agent_from_command("read 1")?;
    assert_eq!(app.active_agent_label(), "read 1");

    app.activate_agent_from_command("rename current ")?;
    assert_eq!(
        app.last_notice(),
        Some("usage: /agent rename <child-id|current> <name>")
    );
    app.activate_agent_from_command("rename current bad\nname")?;
    assert!(
        app.last_notice()
            .is_some_and(|notice| notice.starts_with("agent rename failed:"))
    );
    app.activate_agent_from_command("rename current Repo Audit")?;
    assert_eq!(
        app.last_notice(),
        Some("agent renamed: child_1 -> Repo Audit")
    );
    assert_eq!(app.active_agent_label(), "Repo Audit");
    assert!(app.current_session_entries.iter().any(|entry| matches!(
        entry,
        SessionLogEntry::Control(ControlEntry::TaskChildSessionDisplayName(rename))
            if rename.display_name == "Repo Audit"
    )));
    let stale_entries = app
        .current_session_entries
        .iter()
        .filter(|entry| {
            !matches!(
                entry,
                SessionLogEntry::Control(ControlEntry::TaskChildSessionDisplayName(_))
            )
        })
        .cloned()
        .collect::<Vec<_>>();
    app.sync_current_session_state(stale_entries);
    assert_eq!(app.active_agent_label(), "Repo Audit");
    assert!(app.current_session_entries.iter().any(|entry| matches!(
        entry,
        SessionLogEntry::Control(ControlEntry::TaskChildSessionDisplayName(rename))
            if rename.display_name == "Repo Audit"
    )));
    Ok(())
}

#[test]
fn agent_flow_selection_refresh_and_rename_edges_cover_private_guards() -> Result<()> {
    let child_ref = sigil_kernel::SessionRef::new_relative("children/task_1/step_1-child_1.jsonl")?;
    let mut empty_app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());

    assert!(empty_app.agent_slash_entries("rename ").is_empty());
    empty_app.activate_agent_from_command("rename main Main Agent")?;
    assert_eq!(empty_app.last_notice(), Some("agent not found: main"));

    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    sync_agent_task(
        &mut app,
        Some("Repo Read"),
        sigil_kernel::TaskChildSessionStatus::Started,
        child_ref.clone(),
    )?;
    app.activate_agent_from_command("child_1")?;
    assert_eq!(app.active_agent_label(), "Repo Read");

    app.sidebar_agent_selected = 99;
    assert!(app.move_composer_agent_selection(false));
    assert_eq!(app.sidebar_agent_selected, 0);

    app.sidebar_agent_selected = 1;
    assert!(app.move_composer_agent_selection(false));
    assert_eq!(app.sidebar_agent_selected, 0);

    app.activate_agent_from_command("prev")?;
    assert_eq!(app.active_agent_label(), "main");
    assert!(
        app.agent_slash_entries("rename child_1 Repo Read")
            .is_empty()
    );
    assert!(!app.agent_selector_allows_popup("rename child_1 Repo Read"));

    app.sidebar_agent_selected = 99;
    app.refresh_active_agent_view_after_parent_sync();
    assert!(app.sidebar_agent_selected < app.agent_sidebar_rows().len());

    app.active_agent_view = super::super::AgentView::Child {
        child_task_id: "missing_child".to_owned(),
        child_session_ref: child_ref,
    };
    app.sync_current_session_state(Vec::new());
    app.refresh_active_agent_view_after_parent_sync();
    assert_eq!(app.active_agent_label(), "main");
    Ok(())
}

#[test]
fn agent_sidebar_rows_show_plan_subagent_availability_and_child_sessions() -> Result<()> {
    let task_id = sigil_kernel::TaskId::new("task_1")?;
    let step_id = sigil_kernel::TaskStepId::new("step_1")?;
    let child_task_id = sigil_kernel::TaskId::new("child_1")?;
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());

    let rows = app.agent_sidebar_rows();
    assert!(rows.iter().any(|row| {
        row.label == "main" && row.detail == "idle in current session" && row.active
    }));
    assert!(rows.iter().any(|row| {
        row.label == "agents"
            && row.detail == "no child agents recorded"
            && !row.active
            && row.muted
    }));
    let temp = tempdir()?;
    let session_dir = temp.path().join(".sigil/sessions");
    app.session_log_path = session_dir.join("session-parent.jsonl");
    let child_ref = sigil_kernel::SessionRef::new_relative("children/task_1/step_1-child_1.jsonl")?;
    let child_store = JsonlSessionStore::new(child_ref.resolve(&session_dir))?;
    child_store.append(&SessionLogEntry::User(ModelMessage::user(
        "child delegated prompt",
    )))?;
    child_store.append(&SessionLogEntry::Assistant(ModelMessage::assistant(
        Some("CHILD_TRANSCRIPT_DONE".to_owned()),
        Vec::new(),
    )))?;
    app.push_timeline(TimelineRole::Assistant, "PARENT_MAIN_TRANSCRIPT");

    app.sync_current_session_state(vec![
        SessionLogEntry::Control(ControlEntry::TaskRun(sigil_kernel::TaskRunEntry {
            task_id: task_id.clone(),
            parent_session_ref: sigil_kernel::SessionRef::new_relative("parent.jsonl")?,
            objective: "review workspace".to_owned(),
            status: sigil_kernel::TaskRunStatus::Running,
            reason: None,
        })),
        SessionLogEntry::Control(ControlEntry::TaskPlan(sigil_kernel::TaskPlanEntry {
            task_id: task_id.clone(),
            plan_version: 1,
            status: sigil_kernel::TaskPlanStatus::Accepted,
            steps: vec![sigil_kernel::TaskStepSpec {
                step_id: step_id.clone(),
                title: "让子 agent 检查仓库".to_owned(),
                display_name: Some("仓库审查".to_owned()),
                detail: None,
                role: sigil_kernel::AgentRole::SubagentRead,
                depends_on: Vec::new(),
                mode: None,
                isolation: None,
            }],
            reason: None,
        })),
        SessionLogEntry::Control(ControlEntry::TaskChildSession(
            sigil_kernel::TaskChildSessionEntry {
                task_id: task_id.clone(),
                plan_version: 1,
                step_id: step_id.clone(),
                child_task_id,
                child_session_ref: child_ref,
                role: sigil_kernel::AgentRole::SubagentRead,
                status: sigil_kernel::TaskChildSessionStatus::Started,
                summary_hash: None,
            },
        )),
    ]);

    let rows = app.agent_sidebar_rows();

    assert!(rows.iter().any(|row| {
        row.label == "agent 仓库审查"
            && row.detail == "started · subagent_read · v1:step_1"
            && !row.muted
    }));
    app.active_pane = PaneFocus::Activity;
    app.sidebar_selected_card = SidebarCard::Agents;
    app.sidebar_agent_selected = 1;
    app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))?;
    let focus_lines = app
        .transcript_lines(8)
        .into_iter()
        .map(|line| {
            line.spans
                .into_iter()
                .map(|span| span.content.into_owned())
                .collect::<String>()
        })
        .collect::<Vec<_>>();
    assert!(
        focus_lines
            .iter()
            .any(|line| line == "agent view: 仓库审查 · child session")
    );
    assert!(
        focus_lines
            .iter()
            .any(|line| line == "status: started · subagent_read · v1:step_1")
    );
    assert!(
        focus_lines
            .iter()
            .any(|line| line.contains("CHILD_TRANSCRIPT_DONE"))
    );
    assert!(
        !focus_lines
            .iter()
            .any(|line| line.contains("PARENT_MAIN_TRANSCRIPT"))
    );
    app.sidebar_agent_selected = 0;
    app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))?;
    let main_lines = app
        .transcript_lines(8)
        .into_iter()
        .map(|line| {
            line.spans
                .into_iter()
                .map(|span| span.content.into_owned())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n");
    assert!(main_lines.contains("PARENT_MAIN_TRANSCRIPT"));
    assert!(!main_lines.contains("CHILD_TRANSCRIPT_DONE"));

    app.sync_current_session_state(vec![
        SessionLogEntry::Control(ControlEntry::TaskRun(sigil_kernel::TaskRunEntry {
            task_id: task_id.clone(),
            parent_session_ref: sigil_kernel::SessionRef::new_relative("parent.jsonl")?,
            objective: "review workspace".to_owned(),
            status: sigil_kernel::TaskRunStatus::Completed,
            reason: None,
        })),
        SessionLogEntry::Control(ControlEntry::TaskPlan(sigil_kernel::TaskPlanEntry {
            task_id: task_id.clone(),
            plan_version: 1,
            status: sigil_kernel::TaskPlanStatus::Accepted,
            steps: vec![sigil_kernel::TaskStepSpec {
                step_id: step_id.clone(),
                title: "让子 agent 检查仓库".to_owned(),
                display_name: Some("仓库审查".to_owned()),
                detail: None,
                role: sigil_kernel::AgentRole::SubagentRead,
                depends_on: Vec::new(),
                mode: None,
                isolation: None,
            }],
            reason: None,
        })),
        SessionLogEntry::Control(ControlEntry::TaskChildSession(
            sigil_kernel::TaskChildSessionEntry {
                task_id,
                plan_version: 1,
                step_id,
                child_task_id: sigil_kernel::TaskId::new("child_2")?,
                child_session_ref: sigil_kernel::SessionRef::new_relative(
                    "children/task_1/step_1-child_2.jsonl",
                )?,
                role: sigil_kernel::AgentRole::SubagentRead,
                status: sigil_kernel::TaskChildSessionStatus::Completed,
                summary_hash: None,
            },
        )),
    ]);

    let rows = app.agent_sidebar_rows();

    assert!(rows.iter().any(|row| {
        row.label == "agent 仓库审查"
            && row.detail == "completed · subagent_read · v1:step_1"
            && !row.muted
    }));
    Ok(())
}

#[test]
fn agent_sidebar_rows_project_agent_thread_entries() -> Result<()> {
    let temp = tempdir()?;
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    let session_dir = temp.path().join(".sigil/sessions");
    app.session_log_path = session_dir.join("parent.jsonl");
    let session_ref = sigil_kernel::SessionRef::new_relative("children/thread_1.jsonl")?;
    let child_store = JsonlSessionStore::new(session_ref.resolve(&session_dir))?;
    child_store.append(&SessionLogEntry::User(ModelMessage::user("inspect kernel")))?;
    child_store.append(&SessionLogEntry::Assistant(ModelMessage::assistant(
        Some("KERNEL_THREAD_DONE".to_owned()),
        Vec::new(),
    )))?;

    let profile_id = sigil_kernel::AgentProfileId::new("explore")?;
    let snapshot_id = sigil_kernel::AgentProfileSnapshotId::new("snapshot_explore_1")?;
    let thread_id = sigil_kernel::AgentThreadId::new("thread_1")?;
    let run_context = sigil_kernel::AgentRunContextSnapshot {
        profile_snapshot_id: snapshot_id.clone(),
        provider: "deepseek".to_owned(),
        model: "deepseek-v4-pro".to_owned(),
        reasoning_effort: None,
        workspace_root: sigil_kernel::WorkspaceRootSnapshot::new(
            temp.path().display().to_string(),
        )?,
        effective_tool_scope_hash: "sha256:tools".to_owned(),
        effective_permission_policy_hash: "sha256:permissions".to_owned(),
        effective_mcp_scope_hash: "sha256:mcp".to_owned(),
        provider_capability_hash: "sha256:provider".to_owned(),
        model_visible_agent_index_hash: Some("sha256:index".to_owned()),
        budget_policy_hash: "sha256:budget".to_owned(),
        provider_background_handle_ref: None,
    };
    app.sync_current_session_state(vec![
        SessionLogEntry::Control(ControlEntry::AgentProfileCaptured(
            sigil_kernel::AgentProfileCapturedEntry {
                snapshot: sigil_kernel::AgentProfileSnapshot {
                    snapshot_id: snapshot_id.clone(),
                    profile_id: profile_id.clone(),
                    source: sigil_kernel::AgentProfileSource::System,
                    source_hash: "sha256:source".to_owned(),
                    profile_hash: "sha256:profile".to_owned(),
                    resolved_tool_scope_hash: "sha256:tools".to_owned(),
                    resolved_permission_policy_hash: "sha256:permissions".to_owned(),
                    resolved_mcp_scope_hash: "sha256:mcp".to_owned(),
                    resolved_skill_hashes: Vec::new(),
                    trust_state: sigil_kernel::AgentTrustState::Trusted,
                },
            },
        )),
        SessionLogEntry::Control(ControlEntry::AgentThreadStarted(
            sigil_kernel::AgentThreadStartedEntry {
                thread_id: thread_id.clone(),
                parent_thread_id: Some(sigil_kernel::AgentThreadId::new("main")?),
                parent_session_ref: sigil_kernel::SessionRef::new_relative("parent.jsonl")?,
                thread_session_ref: session_ref,
                profile_id,
                profile_snapshot_id: snapshot_id,
                run_context,
                objective: "inspect kernel".to_owned(),
                prompt_hash: "sha256:prompt".to_owned(),
                invocation_mode: sigil_kernel::AgentInvocationMode::Foreground,
                invocation_source: sigil_kernel::AgentInvocationSource::Chat,
                display_name: Some("kernel map".to_owned()),
                created_at_ms: Some(42),
            },
        )),
        SessionLogEntry::Control(ControlEntry::AgentThreadStatusChanged(
            sigil_kernel::AgentThreadStatusChangedEntry {
                thread_id: thread_id.clone(),
                status: sigil_kernel::AgentThreadStatus::Running,
                reason: None,
                updated_at_ms: None,
            },
        )),
    ]);

    let rows = app.agent_sidebar_rows();
    assert!(rows.iter().any(|row| {
        row.label == "agent kernel map" && row.detail == "running · explore · chat" && !row.muted
    }));

    app.input = "/agent thread_1".to_owned();
    assert!(app.submit_input()?.is_none());
    assert_eq!(app.active_agent_label(), "kernel map");
    let focus_lines = app
        .transcript_lines(8)
        .into_iter()
        .map(|line| {
            line.spans
                .into_iter()
                .map(|span| span.content.into_owned())
                .collect::<String>()
        })
        .collect::<Vec<_>>();
    assert!(
        focus_lines
            .iter()
            .any(|line| line == "status: running · explore · chat")
    );
    assert!(
        focus_lines
            .iter()
            .any(|line| line.contains("KERNEL_THREAD_DONE"))
    );

    app.input = "/agent rename current Kernel Mapper".to_owned();
    assert!(app.submit_input()?.is_none());
    assert_eq!(app.active_agent_label(), "Kernel Mapper");
    let persisted = JsonlSessionStore::read_entries(&app.session_log_path)?;
    assert!(persisted.iter().any(|entry| {
        matches!(
            entry,
            SessionLogEntry::Control(ControlEntry::AgentThreadDisplayName(rename))
                if rename.thread_id == thread_id && rename.display_name == "Kernel Mapper"
        )
    }));
    let stale_entries = app
        .current_session_entries
        .iter()
        .filter(|entry| {
            !matches!(
                entry,
                SessionLogEntry::Control(ControlEntry::AgentThreadDisplayName(_))
            )
        })
        .cloned()
        .collect::<Vec<_>>();
    app.sync_current_session_state(stale_entries);
    assert_eq!(app.active_agent_label(), "Kernel Mapper");
    assert!(app.current_session_entries.iter().any(|entry| {
        matches!(
            entry,
            SessionLogEntry::Control(ControlEntry::AgentThreadDisplayName(rename))
                if rename.thread_id == thread_id && rename.display_name == "Kernel Mapper"
        )
    }));

    app.input = "/agent message current continue".to_owned();
    let action = app.submit_input()?;
    assert!(matches!(
        action,
        Some(AppAction::MessageAgent {
            ref thread_id,
            ref prompt,
        }) if thread_id.as_str() == "thread_1" && prompt == "continue"
    ));
    assert_eq!(
        app.last_notice.as_deref(),
        Some("agent message requested: thread_1")
    );

    app.input = "/agent cancel current".to_owned();
    assert!(app.submit_input()?.is_none());
    assert_eq!(
        app.last_notice.as_deref(),
        Some("agent cancel unavailable until runtime support: thread_1")
    );
    let persisted = JsonlSessionStore::read_entries(&app.session_log_path)?;
    assert!(!persisted.iter().any(|entry| {
        matches!(
            entry,
            SessionLogEntry::Control(ControlEntry::AgentThreadStatusChanged(status))
                if status.thread_id == thread_id
                    && status.status == sigil_kernel::AgentThreadStatus::Cancelled
        )
    }));

    app.input = "/agent close current".to_owned();
    app.composer_agent_panel_focused = true;
    assert!(app.submit_input()?.is_none());
    assert_eq!(
        app.last_notice.as_deref(),
        Some("agent close unavailable until terminal: thread_1")
    );
    assert!(app.composer_agent_panel_focused);
    assert_eq!(app.active_agent_label(), "Kernel Mapper");
    assert!(
        app.agent_sidebar_rows()
            .iter()
            .any(|row| row.label == "agent Kernel Mapper")
    );

    let mut terminal_entries = app.current_session_entries.clone();
    terminal_entries.push(SessionLogEntry::Control(
        ControlEntry::AgentThreadStatusChanged(sigil_kernel::AgentThreadStatusChangedEntry {
            thread_id: thread_id.clone(),
            status: sigil_kernel::AgentThreadStatus::Completed,
            reason: None,
            updated_at_ms: None,
        }),
    ));
    app.sync_current_session_state(terminal_entries.clone());

    app.input = "/agent close current".to_owned();
    app.composer_agent_panel_focused = true;
    let action = app.submit_input()?;
    assert!(matches!(
        action,
        Some(AppAction::CloseAgent {
            ref thread_id,
            reason: Some(ref reason),
        }) if thread_id.as_str() == "thread_1" && reason == "closed from TUI /agent"
    ));
    assert_eq!(
        app.last_notice.as_deref(),
        Some("agent close requested: thread_1")
    );
    assert!(app.composer_agent_panel_focused);
    assert_eq!(app.active_agent_label(), "Kernel Mapper");
    let persisted = JsonlSessionStore::read_entries(&app.session_log_path)?;
    assert!(!persisted.iter().any(|entry| {
        matches!(
            entry,
            SessionLogEntry::Control(ControlEntry::AgentThreadClosed(closed))
                if closed.thread_id == thread_id
        )
    }));

    let mut closed_entries = terminal_entries.clone();
    closed_entries.push(SessionLogEntry::Control(ControlEntry::AgentThreadClosed(
        sigil_kernel::AgentThreadClosedEntry {
            thread_id: thread_id.clone(),
            reason: Some("closed from TUI /agent".to_owned()),
        },
    )));
    app.handle_worker_message(WorkerMessage::AgentThreadClosed {
        thread_id: thread_id.clone(),
        entries: closed_entries,
    })?;
    assert_eq!(app.active_agent_label(), "main");
    assert!(
        !app.agent_sidebar_rows()
            .iter()
            .any(|row| row.label == "agent Kernel Mapper")
    );
    Ok(())
}

#[test]
fn agent_sidebar_rows_keep_completed_status_when_read_agent_result_fails() -> Result<()> {
    let temp = tempdir()?;
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.session_log_path = temp.path().join(".sigil/sessions/parent.jsonl");

    let profile_id = sigil_kernel::AgentProfileId::new("explore")?;
    let snapshot_id = sigil_kernel::AgentProfileSnapshotId::new("snapshot_explore_1")?;
    let thread_id = sigil_kernel::AgentThreadId::new("thread_1")?;
    let session_ref = sigil_kernel::SessionRef::new_relative("children/thread_1.jsonl")?;
    let run_context = sigil_kernel::AgentRunContextSnapshot {
        profile_snapshot_id: snapshot_id.clone(),
        provider: "deepseek".to_owned(),
        model: "deepseek-v4-pro".to_owned(),
        reasoning_effort: None,
        workspace_root: sigil_kernel::WorkspaceRootSnapshot::new(
            temp.path().display().to_string(),
        )?,
        effective_tool_scope_hash: "sha256:tools".to_owned(),
        effective_permission_policy_hash: "sha256:permissions".to_owned(),
        effective_mcp_scope_hash: "sha256:mcp".to_owned(),
        provider_capability_hash: "sha256:provider".to_owned(),
        model_visible_agent_index_hash: Some("sha256:index".to_owned()),
        budget_policy_hash: "sha256:budget".to_owned(),
        provider_background_handle_ref: None,
    };
    let read_failure = ToolResult::error(
        "call-read-failed",
        "read_agent_result",
        ToolErrorKind::Internal,
        "child agent session has no assistant final answer",
    )
    .to_model_message();

    app.sync_current_session_state(vec![
        SessionLogEntry::Control(ControlEntry::AgentProfileCaptured(
            sigil_kernel::AgentProfileCapturedEntry {
                snapshot: sigil_kernel::AgentProfileSnapshot {
                    snapshot_id: snapshot_id.clone(),
                    profile_id: profile_id.clone(),
                    source: sigil_kernel::AgentProfileSource::System,
                    source_hash: "sha256:source".to_owned(),
                    profile_hash: "sha256:profile".to_owned(),
                    resolved_tool_scope_hash: "sha256:tools".to_owned(),
                    resolved_permission_policy_hash: "sha256:permissions".to_owned(),
                    resolved_mcp_scope_hash: "sha256:mcp".to_owned(),
                    resolved_skill_hashes: Vec::new(),
                    trust_state: sigil_kernel::AgentTrustState::Trusted,
                },
            },
        )),
        SessionLogEntry::Control(ControlEntry::AgentThreadStarted(
            sigil_kernel::AgentThreadStartedEntry {
                thread_id: thread_id.clone(),
                parent_thread_id: Some(sigil_kernel::AgentThreadId::new("main")?),
                parent_session_ref: sigil_kernel::SessionRef::new_relative("parent.jsonl")?,
                thread_session_ref: session_ref.clone(),
                profile_id,
                profile_snapshot_id: snapshot_id,
                run_context,
                objective: "inspect kernel".to_owned(),
                prompt_hash: "sha256:prompt".to_owned(),
                invocation_mode: sigil_kernel::AgentInvocationMode::Foreground,
                invocation_source: sigil_kernel::AgentInvocationSource::Chat,
                display_name: Some("kernel map".to_owned()),
                created_at_ms: Some(42),
            },
        )),
        SessionLogEntry::Control(ControlEntry::AgentThreadStatusChanged(
            sigil_kernel::AgentThreadStatusChangedEntry {
                thread_id: thread_id.clone(),
                status: sigil_kernel::AgentThreadStatus::Completed,
                reason: None,
                updated_at_ms: Some(120),
            },
        )),
        SessionLogEntry::Control(ControlEntry::AgentThreadResultRecorded(
            sigil_kernel::AgentThreadResultRecordedEntry {
                result: sigil_kernel::AgentThreadResult {
                    thread_id,
                    session_ref,
                    status: sigil_kernel::AgentThreadTerminalStatus::Completed,
                    summary: "done".to_owned(),
                    summary_truncated: false,
                    original_summary_chars: None,
                    artifacts: Vec::new(),
                    changed_paths: Vec::new(),
                    risks: Vec::new(),
                    followups: Vec::new(),
                    usage: None,
                    output_hash: "sha256:done".to_owned(),
                    final_answer_ref: None,
                },
            },
        )),
        SessionLogEntry::ToolResult(read_failure),
    ]);

    let rows = app.agent_sidebar_rows();
    let row = rows
        .iter()
        .find(|row| row.label == "agent kernel map")
        .expect("agent row");
    assert_eq!(row.detail, "completed · explore · chat");
    assert_eq!(row.status_symbol(), "✓");
    assert!(!row.detail.contains("failed"));
    Ok(())
}

#[test]
fn alt_a_cycles_agent_view_without_activity_focus() -> Result<()> {
    let task_id = sigil_kernel::TaskId::new("task_1")?;
    let step_id = sigil_kernel::TaskStepId::new("step_1")?;
    let child_ref = sigil_kernel::SessionRef::new_relative("children/task_1/step_1-child_1.jsonl")?;
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.sync_current_session_state(vec![
        SessionLogEntry::Control(ControlEntry::TaskRun(sigil_kernel::TaskRunEntry {
            task_id: task_id.clone(),
            parent_session_ref: sigil_kernel::SessionRef::new_relative("parent.jsonl")?,
            objective: "review workspace".to_owned(),
            status: sigil_kernel::TaskRunStatus::Running,
            reason: None,
        })),
        SessionLogEntry::Control(ControlEntry::TaskPlan(sigil_kernel::TaskPlanEntry {
            task_id: task_id.clone(),
            plan_version: 1,
            status: sigil_kernel::TaskPlanStatus::Accepted,
            steps: vec![sigil_kernel::TaskStepSpec {
                step_id: step_id.clone(),
                title: "让子 agent 检查仓库".to_owned(),
                display_name: Some("仓库审查".to_owned()),
                detail: None,
                role: sigil_kernel::AgentRole::SubagentRead,
                depends_on: Vec::new(),
                mode: None,
                isolation: None,
            }],
            reason: None,
        })),
        SessionLogEntry::Control(ControlEntry::TaskChildSession(
            sigil_kernel::TaskChildSessionEntry {
                task_id,
                plan_version: 1,
                step_id,
                child_task_id: sigil_kernel::TaskId::new("child_1")?,
                child_session_ref: child_ref,
                role: sigil_kernel::AgentRole::SubagentRead,
                status: sigil_kernel::TaskChildSessionStatus::Started,
                summary_hash: None,
            },
        )),
    ]);

    assert_eq!(app.active_pane, PaneFocus::Composer);
    app.handle_key_event(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::ALT))?;

    assert_eq!(app.active_agent_label(), "仓库审查");
    assert!(app.is_composer_agent_panel_focused());
    assert_eq!(
        app.last_notice(),
        Some("agent focus: agent 仓库审查 · started · subagent_read · v1:step_1")
    );

    app.handle_key_event(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::ALT))?;

    assert_eq!(app.active_agent_label(), "main");
    assert!(app.is_composer_agent_panel_focused());
    assert_eq!(
        app.last_notice(),
        Some("agent focus: main · idle in current session")
    );

    app.handle_key_event(KeyEvent::new(
        KeyCode::Char('A'),
        KeyModifiers::ALT | KeyModifiers::SHIFT,
    ))?;

    assert_eq!(app.active_agent_label(), "仓库审查");
    Ok(())
}

#[test]
fn task_sidebar_lines_project_latest_task_flags_and_status_labels() -> Result<()> {
    let task_id = sigil_kernel::TaskId::new("task_1")?;
    let step_id = sigil_kernel::TaskStepId::new("step_1")?;
    let child_ref = sigil_kernel::SessionRef::new_relative("children/task_1/step_1-child_1.jsonl")?;
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());

    for (status, label) in [
        (sigil_kernel::TaskRunStatus::Started, "started"),
        (sigil_kernel::TaskRunStatus::Running, "running"),
        (sigil_kernel::TaskRunStatus::Paused, "paused"),
        (sigil_kernel::TaskRunStatus::Completed, "completed"),
        (sigil_kernel::TaskRunStatus::Failed, "failed"),
        (sigil_kernel::TaskRunStatus::Cancelled, "cancelled"),
        (sigil_kernel::TaskRunStatus::Interrupted, "interrupted"),
    ] {
        app.sync_current_session_state(vec![SessionLogEntry::Control(ControlEntry::TaskRun(
            sigil_kernel::TaskRunEntry {
                task_id: task_id.clone(),
                parent_session_ref: sigil_kernel::SessionRef::new_relative("parent.jsonl")?,
                objective: "ship task".to_owned(),
                status,
                reason: None,
            },
        ))]);
        assert!(
            app.task_sidebar_lines()
                .contains(&format!("status: {label}"))
        );
    }
    assert_eq!(
        super::super::task_sidebar::task_child_session_status_label(
            sigil_kernel::TaskChildSessionStatus::Failed
        ),
        "failed"
    );
    assert_eq!(
        super::super::task_sidebar::task_child_session_status_label(
            sigil_kernel::TaskChildSessionStatus::Cancelled
        ),
        "cancelled"
    );
    assert_eq!(
        super::super::task_sidebar::task_child_session_status_label(
            sigil_kernel::TaskChildSessionStatus::Interrupted
        ),
        "interrupted"
    );

    app.sync_current_session_state(vec![
        SessionLogEntry::Control(ControlEntry::TaskRun(sigil_kernel::TaskRunEntry {
            task_id: task_id.clone(),
            parent_session_ref: sigil_kernel::SessionRef::new_relative("parent.jsonl")?,
            objective: "ship task".to_owned(),
            status: sigil_kernel::TaskRunStatus::Paused,
            reason: None,
        })),
        SessionLogEntry::Control(ControlEntry::TaskPlan(sigil_kernel::TaskPlanEntry {
            task_id: task_id.clone(),
            plan_version: 1,
            status: sigil_kernel::TaskPlanStatus::Accepted,
            steps: vec![sigil_kernel::TaskStepSpec {
                step_id: step_id.clone(),
                title: "inspect".to_owned(),
                display_name: None,
                detail: None,
                role: sigil_kernel::AgentRole::Executor,
                depends_on: Vec::new(),
                mode: None,
                isolation: None,
            }],
            reason: None,
        })),
        SessionLogEntry::Control(ControlEntry::TaskStep(sigil_kernel::TaskStepEntry {
            task_id: task_id.clone(),
            plan_version: 1,
            step_id: step_id.clone(),
            role: sigil_kernel::AgentRole::Executor,
            status: sigil_kernel::TaskStepStatus::Running,
            title: Some("inspect".to_owned()),
            summary: None,
            reason: None,
        })),
        SessionLogEntry::Control(ControlEntry::TaskSubagentApprovalRoute(
            sigil_kernel::TaskSubagentApprovalRouteEntry {
                route_id: sigil_kernel::TaskRouteId::new("route_1")?,
                task_id: task_id.clone(),
                plan_version: 1,
                step_id: step_id.clone(),
                role: sigil_kernel::AgentRole::SubagentWrite,
                child_session_ref: child_ref.clone(),
                call_id: "call-1".to_owned(),
                tool_name: "write_file".to_owned(),
                status: sigil_kernel::TaskRouteStatus::Requested,
            },
        )),
        SessionLogEntry::Control(ControlEntry::TaskChildSession(
            sigil_kernel::TaskChildSessionEntry {
                task_id,
                plan_version: 1,
                step_id,
                child_task_id: sigil_kernel::TaskId::new("child_1")?,
                child_session_ref: child_ref,
                role: sigil_kernel::AgentRole::SubagentWrite,
                status: sigil_kernel::TaskChildSessionStatus::Unavailable,
                summary_hash: None,
            },
        )),
    ]);

    let lines = app.task_sidebar_lines();

    assert!(lines.contains(&"task: task_1".to_owned()));
    assert!(lines.contains(&"status: paused".to_owned()));
    assert!(lines.contains(&"plan: v1".to_owned()));
    assert!(lines.contains(&"progress: 0/1 done".to_owned()));
    assert!(lines.contains(&"current: v1:step_1 running".to_owned()));
    assert!(lines.contains(&"◐ 1. running step_1 · inspect".to_owned()));
    assert!(lines.contains(&"routes: unverified".to_owned()));
    assert!(lines.contains(&"child: unavailable".to_owned()));
    Ok(())
}

#[test]
fn task_sidebar_lines_surface_missing_verification_actions() -> Result<()> {
    let task_id = sigil_kernel::TaskId::new("task_1")?;
    let step_id = sigil_kernel::TaskStepId::new("fix-typo")?;
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());

    app.sync_current_session_state(vec![
        SessionLogEntry::Control(ControlEntry::TaskRun(sigil_kernel::TaskRunEntry {
            task_id: task_id.clone(),
            parent_session_ref: sigil_kernel::SessionRef::new_relative("parent.jsonl")?,
            objective: "fix typo".to_owned(),
            status: sigil_kernel::TaskRunStatus::Paused,
            reason: Some("step fix-typo blocked".to_owned()),
        })),
        SessionLogEntry::Control(ControlEntry::TaskPlan(sigil_kernel::TaskPlanEntry {
            task_id: task_id.clone(),
            plan_version: 1,
            status: sigil_kernel::TaskPlanStatus::Accepted,
            steps: vec![sigil_kernel::TaskStepSpec {
                step_id: step_id.clone(),
                title: "Fix typo".to_owned(),
                display_name: None,
                detail: None,
                role: sigil_kernel::AgentRole::Executor,
                depends_on: Vec::new(),
                mode: None,
                isolation: None,
            }],
            reason: None,
        })),
        SessionLogEntry::Control(ControlEntry::TaskStep(sigil_kernel::TaskStepEntry {
            task_id: task_id.clone(),
            plan_version: 1,
            step_id: step_id.clone(),
            role: sigil_kernel::AgentRole::Executor,
            status: sigil_kernel::TaskStepStatus::Blocked,
            title: Some("Fix typo".to_owned()),
            summary: Some("typo fixed but verification missing".to_owned()),
            reason: Some("missing verification".to_owned()),
        })),
        SessionLogEntry::Control(ControlEntry::ReadinessEvaluated(
            sigil_kernel::ReadinessEvaluatedEntry {
                scope: sigil_kernel::EvidenceScope::Step(format!(
                    "{}:{}",
                    task_id.as_str(),
                    step_id.as_str()
                )),
                evaluation: sigil_kernel::ReadinessEvaluation {
                    run_status: sigil_kernel::RunStatus::Blocked,
                    verification_verdict: sigil_kernel::VerificationVerdict::Missing,
                    visible_state: sigil_kernel::VisibleCompletionState::NeedsUser,
                    reasons: vec![sigil_kernel::ReadinessReason::MissingRequiredCheck {
                        check_spec_id: "kernel-verification".to_owned(),
                    }],
                    required_actions: vec![sigil_kernel::RequiredAction::RunCheck {
                        check_spec_id: "kernel-verification".to_owned(),
                    }],
                },
                policy_hash: Some("policy".to_owned()),
                workspace_snapshot_id: Some("snapshot".to_owned()),
            },
        )),
    ]);

    let lines = app.task_sidebar_lines();

    assert!(lines.contains(&"status: paused".to_owned()));
    assert!(lines.contains(&"last: v1:fix-typo needs check".to_owned()));
    assert!(lines.contains(&"verification: missing".to_owned()));
    assert!(lines.contains(&"action: run check kernel-verification".to_owned()));
    assert!(lines.contains(&"△ 1. needs check fix-typo · Fix typo".to_owned()));

    let strip = app.task_strip_view().expect("task strip should render");
    assert_eq!(strip.detail, "paused · v1 · 0/1 done · missing");
    assert_eq!(strip.rows[0].kind, crate::ui::StatusKind::Warning);
    assert_eq!(strip.rows[0].label, "1. needs check · Fix typo");
    assert_eq!(strip.rows[0].detail, "needs check · fix-typo");
    Ok(())
}

#[test]
fn mcp_sidebar_lines_summarize_failure_without_repeating_server_name() -> Result<()> {
    let mut config = test_config();
    config.mcp_servers.push(sigil_kernel::McpServerConfig {
        name: "filesystem".to_owned(),
        startup: sigil_kernel::McpServerStartup::Eager,
        ..sigil_kernel::McpServerConfig::default()
    });
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &config);
    app.handle_worker_message(WorkerMessage::McpActivationStatus {
        server_name: Some("filesystem".to_owned()),
        status: McpActivationStatus::Failed {
            error: "MCP server filesystem tools/list failed: bad response".to_owned(),
        },
    })?;

    assert_eq!(
        app.mcp_sidebar_lines(),
        vec!["filesystem: failed: tools/list failed: bad response"]
    );
    Ok(())
}

#[test]
fn task_sidebar_lines_show_failed_step_and_remaining_plan() -> Result<()> {
    let task_id = sigil_kernel::TaskId::new("task_1")?;
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());

    app.sync_current_session_state(vec![
        SessionLogEntry::Control(ControlEntry::TaskRun(sigil_kernel::TaskRunEntry {
            task_id: task_id.clone(),
            parent_session_ref: sigil_kernel::SessionRef::new_relative("parent.jsonl")?,
            objective: "review workspace".to_owned(),
            status: sigil_kernel::TaskRunStatus::Failed,
            reason: Some("step gate_check failed".to_owned()),
        })),
        SessionLogEntry::Control(ControlEntry::TaskPlan(sigil_kernel::TaskPlanEntry {
            task_id: task_id.clone(),
            plan_version: 1,
            status: sigil_kernel::TaskPlanStatus::Accepted,
            steps: vec![
                sigil_kernel::TaskStepSpec {
                    step_id: sigil_kernel::TaskStepId::new("gate_check")?,
                    title: "跑门禁".to_owned(),
                    display_name: None,
                    detail: None,
                    role: sigil_kernel::AgentRole::Executor,
                    depends_on: Vec::new(),
                    mode: None,
                    isolation: None,
                },
                sigil_kernel::TaskStepSpec {
                    step_id: sigil_kernel::TaskStepId::new("overview")?,
                    title: "扫描项目整体结构".to_owned(),
                    display_name: None,
                    detail: None,
                    role: sigil_kernel::AgentRole::Executor,
                    depends_on: Vec::new(),
                    mode: None,
                    isolation: None,
                },
            ],
            reason: None,
        })),
        SessionLogEntry::Control(ControlEntry::TaskStep(sigil_kernel::TaskStepEntry {
            task_id,
            plan_version: 1,
            step_id: sigil_kernel::TaskStepId::new("gate_check")?,
            role: sigil_kernel::AgentRole::Executor,
            status: sigil_kernel::TaskStepStatus::Failed,
            title: Some("跑门禁".to_owned()),
            summary: Some("门禁全部通过".to_owned()),
            reason: Some("invalid tool arguments".to_owned()),
        })),
    ]);

    let lines = app.task_sidebar_lines();

    assert!(lines.contains(&"status: failed".to_owned()));
    assert!(lines.contains(&"progress: 0/2 done".to_owned()));
    assert!(lines.contains(&"last: v1:gate_check failed".to_owned()));
    assert!(lines.contains(&"reason: step gate_check failed".to_owned()));
    assert!(lines.contains(&"✕ 1. failed gate_check · 跑门禁".to_owned()));
    assert!(lines.contains(&"◇ 2. pending overview · 扫描项目整体结构".to_owned()));
    Ok(())
}

#[test]
fn task_sidebar_lines_distinguish_cancelled_and_interrupted_steps() -> Result<()> {
    let task_id = sigil_kernel::TaskId::new("task_1")?;
    let cancelled_step = sigil_kernel::TaskStepId::new("cancel_setup")?;
    let interrupted_step = sigil_kernel::TaskStepId::new("interrupt_review")?;
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());

    app.sync_current_session_state(vec![
        SessionLogEntry::Control(ControlEntry::TaskRun(sigil_kernel::TaskRunEntry {
            task_id: task_id.clone(),
            parent_session_ref: sigil_kernel::SessionRef::new_relative("parent.jsonl")?,
            objective: "review workspace".to_owned(),
            status: sigil_kernel::TaskRunStatus::Cancelled,
            reason: Some("user cancelled task".to_owned()),
        })),
        SessionLogEntry::Control(ControlEntry::TaskPlan(sigil_kernel::TaskPlanEntry {
            task_id: task_id.clone(),
            plan_version: 1,
            status: sigil_kernel::TaskPlanStatus::Accepted,
            steps: vec![
                sigil_kernel::TaskStepSpec {
                    step_id: cancelled_step.clone(),
                    title: "cancel setup".to_owned(),
                    display_name: None,
                    detail: None,
                    role: sigil_kernel::AgentRole::Executor,
                    depends_on: Vec::new(),
                    mode: None,
                    isolation: None,
                },
                sigil_kernel::TaskStepSpec {
                    step_id: interrupted_step.clone(),
                    title: "review interrupted".to_owned(),
                    display_name: None,
                    detail: None,
                    role: sigil_kernel::AgentRole::Executor,
                    depends_on: Vec::new(),
                    mode: None,
                    isolation: None,
                },
            ],
            reason: None,
        })),
        SessionLogEntry::Control(ControlEntry::TaskStep(sigil_kernel::TaskStepEntry {
            task_id: task_id.clone(),
            plan_version: 1,
            step_id: cancelled_step,
            role: sigil_kernel::AgentRole::Executor,
            status: sigil_kernel::TaskStepStatus::Cancelled,
            title: Some("cancel setup".to_owned()),
            summary: None,
            reason: Some("user cancelled task".to_owned()),
        })),
        SessionLogEntry::Control(ControlEntry::TaskStep(sigil_kernel::TaskStepEntry {
            task_id,
            plan_version: 1,
            step_id: interrupted_step,
            role: sigil_kernel::AgentRole::Executor,
            status: sigil_kernel::TaskStepStatus::Interrupted,
            title: Some("review interrupted".to_owned()),
            summary: None,
            reason: Some("tool interrupted".to_owned()),
        })),
    ]);

    let lines = app.task_sidebar_lines();

    assert!(lines.contains(&"✕ 1. cancelled cancel_setup · cancel setup".to_owned()));
    assert!(lines.contains(&"✕ 2. interrupted interrupt_review · review interrupted".to_owned()));
    Ok(())
}

#[test]
fn task_sidebar_lines_keeps_hidden_current_step_visible() -> Result<()> {
    let task_id = sigil_kernel::TaskId::new("task_1")?;
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    let steps = (1..=8)
        .map(|index| {
            Ok(sigil_kernel::TaskStepSpec {
                step_id: sigil_kernel::TaskStepId::new(format!("step_{index}"))?,
                title: format!("step {index}"),
                display_name: None,
                detail: None,
                role: sigil_kernel::AgentRole::Executor,
                depends_on: Vec::new(),
                mode: None,
                isolation: None,
            })
        })
        .collect::<Result<Vec<_>>>()?;

    app.sync_current_session_state(vec![
        SessionLogEntry::Control(ControlEntry::TaskRun(sigil_kernel::TaskRunEntry {
            task_id: task_id.clone(),
            parent_session_ref: sigil_kernel::SessionRef::new_relative("parent.jsonl")?,
            objective: "review workspace".to_owned(),
            status: sigil_kernel::TaskRunStatus::Running,
            reason: None,
        })),
        SessionLogEntry::Control(ControlEntry::TaskPlan(sigil_kernel::TaskPlanEntry {
            task_id: task_id.clone(),
            plan_version: 1,
            status: sigil_kernel::TaskPlanStatus::Accepted,
            steps,
            reason: None,
        })),
        SessionLogEntry::Control(ControlEntry::TaskStep(sigil_kernel::TaskStepEntry {
            task_id: task_id.clone(),
            plan_version: 1,
            step_id: sigil_kernel::TaskStepId::new("step_1")?,
            role: sigil_kernel::AgentRole::Executor,
            status: sigil_kernel::TaskStepStatus::Completed,
            title: Some("step 1".to_owned()),
            summary: None,
            reason: None,
        })),
        SessionLogEntry::Control(ControlEntry::TaskStep(sigil_kernel::TaskStepEntry {
            task_id: task_id.clone(),
            plan_version: 1,
            step_id: sigil_kernel::TaskStepId::new("step_2")?,
            role: sigil_kernel::AgentRole::Executor,
            status: sigil_kernel::TaskStepStatus::Blocked,
            title: Some("step 2".to_owned()),
            summary: None,
            reason: None,
        })),
        SessionLogEntry::Control(ControlEntry::TaskStep(sigil_kernel::TaskStepEntry {
            task_id,
            plan_version: 1,
            step_id: sigil_kernel::TaskStepId::new("step_8")?,
            role: sigil_kernel::AgentRole::Executor,
            status: sigil_kernel::TaskStepStatus::Running,
            title: Some("step 8".to_owned()),
            summary: None,
            reason: None,
        })),
    ]);

    let lines = app.task_sidebar_lines();

    assert!(lines.contains(&"progress: 1/8 done".to_owned()));
    assert!(lines.contains(&"✓ 1. completed step_1 · step 1".to_owned()));
    assert!(lines.contains(&"✕ 2. blocked step_2 · step 2".to_owned()));
    assert!(lines.contains(&"◐ 8. running step_8 · step 8".to_owned()));
    assert!(lines.contains(&"+2 more steps · 2 pending".to_owned()));
    Ok(())
}

#[test]
fn task_sidebar_lines_completed_long_plan_shows_final_step_and_hidden_summary() -> Result<()> {
    let task_id = sigil_kernel::TaskId::new("task_1")?;
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    let steps = (1..=10)
        .map(|index| {
            Ok(sigil_kernel::TaskStepSpec {
                step_id: sigil_kernel::TaskStepId::new(format!("step_{index}"))?,
                title: format!("step {index}"),
                display_name: None,
                detail: None,
                role: sigil_kernel::AgentRole::Executor,
                depends_on: Vec::new(),
                mode: None,
                isolation: None,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    let mut entries = vec![
        SessionLogEntry::Control(ControlEntry::TaskRun(sigil_kernel::TaskRunEntry {
            task_id: task_id.clone(),
            parent_session_ref: sigil_kernel::SessionRef::new_relative("parent.jsonl")?,
            objective: "review workspace".to_owned(),
            status: sigil_kernel::TaskRunStatus::Running,
            reason: None,
        })),
        SessionLogEntry::Control(ControlEntry::TaskPlan(sigil_kernel::TaskPlanEntry {
            task_id: task_id.clone(),
            plan_version: 1,
            status: sigil_kernel::TaskPlanStatus::Accepted,
            steps,
            reason: None,
        })),
    ];
    for index in 1..=10 {
        entries.push(SessionLogEntry::Control(ControlEntry::TaskStep(
            sigil_kernel::TaskStepEntry {
                task_id: task_id.clone(),
                plan_version: 1,
                step_id: sigil_kernel::TaskStepId::new(format!("step_{index}"))?,
                role: sigil_kernel::AgentRole::Executor,
                status: sigil_kernel::TaskStepStatus::Completed,
                title: Some(format!("step {index}")),
                summary: None,
                reason: None,
            },
        )));
    }
    entries.push(SessionLogEntry::Control(ControlEntry::TaskRun(
        sigil_kernel::TaskRunEntry {
            task_id,
            parent_session_ref: sigil_kernel::SessionRef::new_relative("parent.jsonl")?,
            objective: "review workspace".to_owned(),
            status: sigil_kernel::TaskRunStatus::Completed,
            reason: Some("completed plan v1".to_owned()),
        },
    )));

    app.sync_current_session_state(entries);
    let lines = app.task_sidebar_lines();

    assert!(lines.contains(&"status: completed".to_owned()));
    assert!(lines.contains(&"progress: 10/10 done".to_owned()));
    assert!(lines.contains(&"last: v1:step_10 completed".to_owned()));
    assert!(lines.contains(&"✓ 10. completed step_10 · step 10".to_owned()));
    assert!(lines.contains(&"+4 more steps · 4 completed".to_owned()));
    Ok(())
}

#[test]
fn task_sidebar_lines_summarizes_hidden_non_pending_statuses() -> Result<()> {
    let task_id = sigil_kernel::TaskId::new("task_1")?;
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    let steps = (1..=12)
        .map(|index| {
            Ok(sigil_kernel::TaskStepSpec {
                step_id: sigil_kernel::TaskStepId::new(format!("step_{index}"))?,
                title: format!("step {index}"),
                display_name: None,
                detail: None,
                role: sigil_kernel::AgentRole::Executor,
                depends_on: Vec::new(),
                mode: None,
                isolation: None,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    let mut entries = vec![
        SessionLogEntry::Control(ControlEntry::TaskRun(sigil_kernel::TaskRunEntry {
            task_id: task_id.clone(),
            parent_session_ref: sigil_kernel::SessionRef::new_relative("parent.jsonl")?,
            objective: "review workspace".to_owned(),
            status: sigil_kernel::TaskRunStatus::Running,
            reason: None,
        })),
        SessionLogEntry::Control(ControlEntry::TaskPlan(sigil_kernel::TaskPlanEntry {
            task_id: task_id.clone(),
            plan_version: 1,
            status: sigil_kernel::TaskPlanStatus::Accepted,
            steps,
            reason: None,
        })),
    ];
    for (index, status) in [
        (7, sigil_kernel::TaskStepStatus::Running),
        (8, sigil_kernel::TaskStepStatus::Failed),
        (9, sigil_kernel::TaskStepStatus::Blocked),
        (10, sigil_kernel::TaskStepStatus::Cancelled),
        (11, sigil_kernel::TaskStepStatus::Interrupted),
        (12, sigil_kernel::TaskStepStatus::Completed),
        (1, sigil_kernel::TaskStepStatus::Running),
    ] {
        entries.push(SessionLogEntry::Control(ControlEntry::TaskStep(
            sigil_kernel::TaskStepEntry {
                task_id: task_id.clone(),
                plan_version: 1,
                step_id: sigil_kernel::TaskStepId::new(format!("step_{index}"))?,
                role: sigil_kernel::AgentRole::Executor,
                status,
                title: Some(format!("step {index}")),
                summary: None,
                reason: None,
            },
        )));
    }

    app.sync_current_session_state(entries);
    let lines = app.task_sidebar_lines();

    assert!(lines.contains(&"◐ 1. running step_1 · step 1".to_owned()));
    assert!(lines.contains(
        &"+6 more steps · 1 running, 1 failed, 1 blocked, 1 cancelled, 1 interrupted, 1 completed"
            .to_owned()
    ));
    Ok(())
}

#[test]
fn task_sidebar_lines_focuses_first_pending_without_problem_step() -> Result<()> {
    let task_id = sigil_kernel::TaskId::new("task_1")?;
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());

    app.sync_current_session_state(vec![
        SessionLogEntry::Control(ControlEntry::TaskRun(sigil_kernel::TaskRunEntry {
            task_id: task_id.clone(),
            parent_session_ref: sigil_kernel::SessionRef::new_relative("parent.jsonl")?,
            objective: "review workspace".to_owned(),
            status: sigil_kernel::TaskRunStatus::Running,
            reason: None,
        })),
        SessionLogEntry::Control(ControlEntry::TaskPlan(sigil_kernel::TaskPlanEntry {
            task_id: task_id.clone(),
            plan_version: 1,
            status: sigil_kernel::TaskPlanStatus::Accepted,
            steps: vec![
                sigil_kernel::TaskStepSpec {
                    step_id: sigil_kernel::TaskStepId::new("step_1")?,
                    title: "step 1".to_owned(),
                    display_name: None,
                    detail: None,
                    role: sigil_kernel::AgentRole::Executor,
                    depends_on: Vec::new(),
                    mode: None,
                    isolation: None,
                },
                sigil_kernel::TaskStepSpec {
                    step_id: sigil_kernel::TaskStepId::new("step_2")?,
                    title: "step 2".to_owned(),
                    display_name: None,
                    detail: None,
                    role: sigil_kernel::AgentRole::Executor,
                    depends_on: Vec::new(),
                    mode: None,
                    isolation: None,
                },
            ],
            reason: None,
        })),
        SessionLogEntry::Control(ControlEntry::TaskStep(sigil_kernel::TaskStepEntry {
            task_id,
            plan_version: 1,
            step_id: sigil_kernel::TaskStepId::new("step_1")?,
            role: sigil_kernel::AgentRole::Executor,
            status: sigil_kernel::TaskStepStatus::Completed,
            title: Some("step 1".to_owned()),
            summary: None,
            reason: None,
        })),
    ]);

    let lines = app.task_sidebar_lines();

    assert!(lines.contains(&"✓ 1. completed step_1 · step 1".to_owned()));
    assert!(lines.contains(&"◇ 2. pending step_2 · step 2".to_owned()));
    assert!(!lines.iter().any(|line| line.starts_with("last: ")));
    Ok(())
}

#[test]
fn task_strip_view_projects_focus_hidden_summary_and_fallback_row() -> Result<()> {
    let task_id = sigil_kernel::TaskId::new("task_1")?;
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    let steps = (1..=6)
        .map(|index| {
            Ok(sigil_kernel::TaskStepSpec {
                step_id: sigil_kernel::TaskStepId::new(format!("step_{index}"))?,
                title: format!("step {index}"),
                display_name: None,
                detail: None,
                role: sigil_kernel::AgentRole::Executor,
                depends_on: Vec::new(),
                mode: None,
                isolation: None,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    let mut entries = vec![
        SessionLogEntry::Control(ControlEntry::TaskRun(sigil_kernel::TaskRunEntry {
            task_id: task_id.clone(),
            parent_session_ref: sigil_kernel::SessionRef::new_relative("parent.jsonl")?,
            objective: "review workspace".to_owned(),
            status: sigil_kernel::TaskRunStatus::Running,
            reason: None,
        })),
        SessionLogEntry::Control(ControlEntry::TaskPlan(sigil_kernel::TaskPlanEntry {
            task_id: task_id.clone(),
            plan_version: 1,
            status: sigil_kernel::TaskPlanStatus::Accepted,
            steps,
            reason: None,
        })),
    ];
    for (index, status) in [
        (1, sigil_kernel::TaskStepStatus::Completed),
        (2, sigil_kernel::TaskStepStatus::Completed),
        (3, sigil_kernel::TaskStepStatus::Blocked),
        (6, sigil_kernel::TaskStepStatus::Running),
    ] {
        entries.push(SessionLogEntry::Control(ControlEntry::TaskStep(
            sigil_kernel::TaskStepEntry {
                task_id: task_id.clone(),
                plan_version: 1,
                step_id: sigil_kernel::TaskStepId::new(format!("step_{index}"))?,
                role: sigil_kernel::AgentRole::Executor,
                status,
                title: Some(format!("step {index}")),
                summary: None,
                reason: None,
            },
        )));
    }

    app.sync_current_session_state(entries);

    let strip = app.task_strip_view().expect("task strip should render");
    assert_eq!(strip.title, "Task task_1");
    assert_eq!(strip.detail, "running · v1 · 2/6 done");
    assert_eq!(strip.rows.len(), 5);
    assert_eq!(strip.rows[0].label, "1. step 1");
    assert_eq!(strip.rows[0].kind, crate::ui::StatusKind::Success);
    assert_eq!(strip.rows[2].label, "3. step 3");
    assert_eq!(strip.rows[2].kind, crate::ui::StatusKind::Error);
    assert_eq!(strip.rows[3].label, "6. step 6");
    assert!(strip.rows[3].active);
    assert_eq!(strip.rows[4].label, "+2 more steps");
    assert_eq!(strip.rows[4].detail, "2 pending");

    app.sync_current_session_state(vec![SessionLogEntry::Control(ControlEntry::TaskRun(
        sigil_kernel::TaskRunEntry {
            task_id,
            parent_session_ref: sigil_kernel::SessionRef::new_relative("parent.jsonl")?,
            objective: "fallback task".to_owned(),
            status: sigil_kernel::TaskRunStatus::Paused,
            reason: None,
        },
    ))]);
    let fallback = app
        .task_strip_view()
        .expect("task strip should render run-only task");
    assert_eq!(fallback.rows[0].label, "fallback task");
    assert_eq!(fallback.rows[0].kind, crate::ui::StatusKind::Warning);
    assert!(fallback.rows[0].active);

    app.sync_current_session_state(vec![SessionLogEntry::Control(ControlEntry::TaskRun(
        sigil_kernel::TaskRunEntry {
            task_id: sigil_kernel::TaskId::new("task_completed")?,
            parent_session_ref: sigil_kernel::SessionRef::new_relative("parent.jsonl")?,
            objective: "completed fallback".to_owned(),
            status: sigil_kernel::TaskRunStatus::Completed,
            reason: None,
        },
    ))]);
    let completed = app
        .task_strip_view()
        .expect("task strip should render completed run-only task");
    assert_eq!(completed.rows[0].kind, crate::ui::StatusKind::Success);
    assert!(!completed.rows[0].active);

    app.sync_current_session_state(vec![SessionLogEntry::Control(ControlEntry::TaskRun(
        sigil_kernel::TaskRunEntry {
            task_id: sigil_kernel::TaskId::new("task_failed")?,
            parent_session_ref: sigil_kernel::SessionRef::new_relative("parent.jsonl")?,
            objective: "failed fallback".to_owned(),
            status: sigil_kernel::TaskRunStatus::Failed,
            reason: None,
        },
    ))]);
    let failed = app
        .task_strip_view()
        .expect("task strip should render failed run-only task");
    assert_eq!(failed.rows[0].kind, crate::ui::StatusKind::Error);
    assert!(!failed.rows[0].active);
    Ok(())
}

#[test]
fn mcp_sidebar_lines_are_empty_before_runtime_config_loads() -> Result<()> {
    let temp = tempdir()?;
    let app = AppState::from_setup(
        temp.path().join("sigil.toml"),
        temp.path().to_path_buf(),
        Some("missing config".to_owned()),
    );

    assert!(app.mcp_sidebar_lines().is_empty());
    Ok(())
}

#[test]
fn code_intelligence_sidebar_sorts_diagnostics_and_collapses_overflow() {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.code_intelligence_server_lines.insert(
        "rust-analyzer".to_owned(),
        "rust-analyzer: ready".to_owned(),
    );
    app.code_intelligence_diagnostics_line = Some("diagnostics: 8".to_owned());
    app.code_intelligence_diagnostics_by_path = std::collections::BTreeMap::from([
        (
            "src/a.rs".to_owned(),
            ApprovalDiagnosticSummary {
                errors: 1,
                warnings: 0,
            },
        ),
        (
            "src/b.rs".to_owned(),
            ApprovalDiagnosticSummary {
                errors: 3,
                warnings: 0,
            },
        ),
        (
            "src/c.rs".to_owned(),
            ApprovalDiagnosticSummary {
                errors: 3,
                warnings: 2,
            },
        ),
        (
            "src/d.rs".to_owned(),
            ApprovalDiagnosticSummary {
                errors: 1,
                warnings: 5,
            },
        ),
        (
            "src/e.rs".to_owned(),
            ApprovalDiagnosticSummary {
                errors: 0,
                warnings: 1,
            },
        ),
    ]);

    let lines = app.code_intelligence_sidebar_lines();
    let diagnostics_index = lines
        .iter()
        .position(|line| line == "latest diagnostics: 5 files")
        .expect("diagnostics header should be present");

    assert_eq!(
        lines.first().map(String::as_str),
        Some("rust-analyzer: ready")
    );
    assert_eq!(lines.get(1).map(String::as_str), Some("diagnostics: 8"));
    assert_eq!(
        lines.get(diagnostics_index + 1).map(String::as_str),
        Some("src/c.rs: 3 errors 2 warnings")
    );
    assert_eq!(
        lines.get(diagnostics_index + 2).map(String::as_str),
        Some("src/b.rs: 3 errors")
    );
    assert_eq!(
        lines.get(diagnostics_index + 3).map(String::as_str),
        Some("src/d.rs: 1 error 5 warnings")
    );
    assert_eq!(
        lines.get(diagnostics_index + 4).map(String::as_str),
        Some("src/a.rs: 1 error")
    );
    assert_eq!(lines.last().map(String::as_str), Some("+1 more files"));
}

#[test]
fn activity_pane_sidebar_keys_cover_permission_agents_usage_and_noop_paths() -> Result<()> {
    let temp = tempdir()?;
    let config_path = temp.path().join("sigil.toml");
    let config = test_config();
    config.save(&config_path)?;
    let mut app = AppState::from_root_config(&config_path, &config);
    app.active_pane = PaneFocus::Activity;
    app.sidebar_selected_card = SidebarCard::Permission;

    let action = app.handle_key_event(KeyEvent::new(KeyCode::BackTab, KeyModifiers::NONE))?;
    assert!(matches!(
        action,
        Some(AppAction::RuntimeConfigUpdated { .. })
    ));
    assert_eq!(app.permission_default_mode, "deny");

    app.active_pane = PaneFocus::Activity;
    app.sidebar_selected_card = SidebarCard::Permission;
    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE))?;
    assert_eq!(app.sidebar_selected_card, SidebarCard::Usage);
    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE))?;
    assert_eq!(app.sidebar_selected_card, SidebarCard::Agents);
    assert_eq!(app.sidebar_agent_selected, 0);
    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE))?;
    assert_eq!(app.sidebar_agent_selected, 1);
    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE))?;
    assert_eq!(app.sidebar_selected_card, SidebarCard::Usage);

    for index in 0..12 {
        app.push_timeline(TimelineRole::Assistant, format!("activity message {index}"));
    }
    app.set_terminal_size(80, 10);
    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Home, KeyModifiers::NONE))?;
    assert!(app.timeline_scroll_back > 0);
    assert_eq!(app.sidebar_selected_card, SidebarCard::Usage);
    let _ = app.handle_key_event(KeyEvent::new(KeyCode::End, KeyModifiers::NONE))?;
    assert_eq!(app.timeline_scroll_back, 0);
    assert_eq!(app.sidebar_selected_card, SidebarCard::Usage);

    app.sidebar_selected_card = SidebarCard::Agents;
    app.sidebar_agent_selected = 99;
    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))?;
    assert_eq!(app.last_notice(), Some("no agent selected"));
    assert!(
        app.timeline
            .iter()
            .any(|entry| entry.role == TimelineRole::Notice && entry.text == "no agent selected")
    );

    let before_input = app.input.clone();
    for key in [
        KeyCode::Char('x'),
        KeyCode::Backspace,
        KeyCode::Left,
        KeyCode::Right,
    ] {
        let _ = app.handle_key_event(KeyEvent::new(key, KeyModifiers::NONE))?;
        assert_eq!(app.input, before_input);
        assert_eq!(app.active_pane, PaneFocus::Activity);
    }

    app.is_busy = true;
    app.sidebar_selected_card = SidebarCard::Permission;
    let action = app.handle_key_event(KeyEvent::new(KeyCode::BackTab, KeyModifiers::NONE))?;
    assert!(action.is_none());
    assert_eq!(app.last_notice(), Some("busy; permission locked"));
    assert!(app.timeline.iter().any(|entry| {
        entry.role == TimelineRole::Notice && entry.text == "busy; permission mode stays unchanged"
    }));
    Ok(())
}

#[test]
fn composer_top_level_keys_cover_empty_submit_cursor_scroll_and_escape_paths() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());

    assert!(app.submit_input()?.is_none());

    app.input = "/".to_owned();
    let row_count = app.slash_selector_rows().len();
    assert!(row_count > 1);
    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE))?;
    let selected_after_down = app
        .slash_selector_selected_index()
        .expect("slash selector should have selected row");
    let _ = app.handle_key_event(KeyEvent::new(KeyCode::BackTab, KeyModifiers::SHIFT))?;
    assert_eq!(
        app.slash_selector_selected_index(),
        Some((selected_after_down + row_count - 1) % row_count)
    );
    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE))?;
    assert!(app.slash_selector_selected_index().is_some());

    app.input = "line one\nline two".to_owned();
    let first_line_cursor = "line".chars().count();
    app.input_cursor = first_line_cursor;
    app.active_pane = PaneFocus::Composer;
    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE))?;
    assert!(app.input_cursor > first_line_cursor);
    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Home, KeyModifiers::NONE))?;
    assert_eq!(app.input_cursor, 0);
    let _ = app.handle_key_event(KeyEvent::new(KeyCode::End, KeyModifiers::NONE))?;
    assert_eq!(app.input_cursor, app.input.chars().count());

    for index in 0..12 {
        app.push_timeline(TimelineRole::Assistant, format!("message {index}"));
    }
    app.set_terminal_size(80, 12);
    let _ = app.handle_key_event(KeyEvent::new(KeyCode::PageUp, KeyModifiers::NONE))?;
    assert!(app.timeline_scroll_back > 0);
    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Home, KeyModifiers::NONE))?;
    assert!(app.timeline_scroll_back > 0);
    let _ = app.handle_key_event(KeyEvent::new(KeyCode::PageDown, KeyModifiers::NONE))?;
    let _ = app.handle_key_event(KeyEvent::new(KeyCode::End, KeyModifiers::NONE))?;
    assert_eq!(app.timeline_scroll_back, 0);

    app.input = "abc".to_owned();
    app.input_cursor = 2;
    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Left, KeyModifiers::NONE))?;
    assert_eq!(app.input_cursor, 1);
    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE))?;
    assert_eq!(app.input_cursor, 2);
    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE))?;
    assert_eq!(app.input, "ac");
    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE))?;
    assert!(app.input.is_empty());
    assert_eq!(app.input_cursor, 0);

    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Char('\n'), KeyModifiers::SHIFT))?;
    assert_eq!(app.input, "\n");
    Ok(())
}

#[test]
fn slash_and_status_helpers_cover_usage_no_match_and_no_config_guards() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.run_phase = RunPhase::Streaming;
    assert_eq!(app.run_phase_label(), "streaming");

    app.provider_name = "custom".to_owned();
    app.model_name = "unknown".to_owned();
    app.compaction_config.context_window_tokens = None;
    assert_eq!(
        app.context_usage_line(),
        "ctx: n/a · prompt 0 · set fallback_context_window_tokens"
    );
    assert_eq!(app.context_usage_hint(100), "threshold n/a");
    assert_eq!(
        crate::app::context_window_source_label(sigil_runtime::ContextWindowSource::None),
        "n/a"
    );
    assert!(app.compaction_policy_line().starts_with("policy: soft"));
    assert!(app.footer_status_line().contains("ctx n/a"));

    app.input = "/resume definitely-missing".to_owned();
    assert!(app.submit_input()?.is_none());
    assert!(
        app.timeline
            .iter()
            .any(|entry| entry.role == TimelineRole::Notice && entry.text == "no matching session")
    );

    let action = app.execute_slash_command(
        crate::slash::ResolvedSlashCommand {
            canonical: "/model".to_owned(),
            arg: String::new(),
        },
        "/model".to_owned(),
    )?;
    assert!(action.is_none());
    assert_eq!(app.last_notice(), Some("usage: /model <flash|pro|id>"));

    let action = app.execute_slash_command(
        crate::slash::ResolvedSlashCommand {
            canonical: "/bogus".to_owned(),
            arg: String::new(),
        },
        "/bogus".to_owned(),
    )?;
    assert!(action.is_none());
    assert!(
        app.timeline.iter().any(
            |entry| entry.role == TimelineRole::Notice && entry.text == "unknown slash command"
        )
    );

    let mut setup_app = AppState::from_setup(
        Path::new("sigil.toml").to_path_buf(),
        Path::new(".").to_path_buf(),
        None,
    );
    let action = setup_app.execute_slash_command(
        crate::slash::ResolvedSlashCommand {
            canonical: "/model".to_owned(),
            arg: "pro".to_owned(),
        },
        "/model pro".to_owned(),
    )?;
    assert!(action.is_none());
    assert!(setup_app.is_setup_mode());

    setup_app.active_pane = PaneFocus::Composer;
    let action = setup_app.handle_key_event(KeyEvent::new(KeyCode::BackTab, KeyModifiers::NONE))?;
    assert!(action.is_none());
    Ok(())
}

#[test]
fn model_command_updates_openai_compat_provider_block() -> Result<()> {
    let mut config = test_config();
    config.agent.provider = "openai_compat".to_owned();
    config.agent.model = "gpt-old".to_owned();
    config.providers.insert(
        "openai_compat".to_owned(),
        json!({
            "base_url": "https://openai.example.com/v1",
            "model": "gpt-old",
            "api_key": "openai-key",
            "request_timeout_secs": 20
        }),
    );
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &config);

    let action = app.execute_slash_command(
        crate::slash::ResolvedSlashCommand {
            canonical: "/model".to_owned(),
            arg: "gpt-new".to_owned(),
        },
        "/model gpt-new".to_owned(),
    )?;

    let Some(AppAction::RuntimeConfigUpdated { root_config }) = action else {
        panic!("expected runtime config update");
    };
    assert_eq!(root_config.agent.provider, "openai_compat");
    assert_eq!(root_config.agent.model, "gpt-new");
    assert_eq!(
        root_config.providers["openai_compat"]["model"],
        serde_json::Value::String("gpt-new".to_owned())
    );
    assert_eq!(
        root_config.providers["openai_compat"]["api_key"],
        serde_json::Value::String("openai-key".to_owned())
    );
    Ok(())
}

fn test_terminal_entry(
    task_id: &str,
    status: sigil_kernel::TerminalTaskStatus,
) -> Result<sigil_kernel::TerminalTaskEntry> {
    Ok(sigil_kernel::TerminalTaskEntry {
        handle: sigil_kernel::TerminalTaskHandle {
            task_id: sigil_kernel::TerminalTaskId::new(task_id)?,
            command: "cargo test".to_owned(),
            cwd: Path::new(".").to_path_buf(),
            shell: "sh".to_owned(),
            log_path: Path::new(".sigil/tasks").join(task_id).join("output.log"),
            created_at_ms: 10,
            execution_backend: None,
            execution_backend_capabilities: None,
        },
        status,
        output_preview: Some("running output".to_owned()),
        output_hash: Some("hash".to_owned()),
        output_truncated: false,
        updated_at_ms: 20,
    })
}

use super::*;

#[test]
fn alt_d_requests_changed_file_diagnostics_when_code_intelligence_is_enabled() -> Result<()> {
    let temp = tempdir()?;
    let mut config = test_config();
    config.workspace.root = temp.path().display().to_string();
    config.code_intelligence.enabled = true;
    let mut app = AppState::from_root_config(&temp.path().join("sigil.toml"), &config);

    let action = app.handle_key_event(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::ALT))?;

    assert!(matches!(
        action,
        Some(AppAction::CheckChangedFilesDiagnostics)
    ));
    assert!(
        app.events
            .iter()
            .any(|event| event.label == "code:check" && event.detail == "changed files")
    );
    Ok(())
}

#[test]
fn alt_d_does_not_request_diagnostics_when_code_intelligence_is_disabled() -> Result<()> {
    let temp = tempdir()?;
    let mut config = test_config();
    config.workspace.root = temp.path().display().to_string();
    let mut app = AppState::from_root_config(&temp.path().join("sigil.toml"), &config);

    let action = app.handle_key_event(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::ALT))?;

    assert!(action.is_none());
    assert_eq!(app.last_notice.as_deref(), Some("code intelligence is off"));
    Ok(())
}

#[test]
fn alt_d_does_not_request_diagnostics_while_busy() -> Result<()> {
    let temp = tempdir()?;
    let mut config = test_config();
    config.workspace.root = temp.path().display().to_string();
    config.code_intelligence.enabled = true;
    let mut app = AppState::from_root_config(&temp.path().join("sigil.toml"), &config);
    app.runtime.is_busy = true;

    let action = app.handle_key_event(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::ALT))?;

    assert!(action.is_none());
    assert_eq!(
        app.last_notice.as_deref(),
        Some("wait for the active run before checking changes")
    );
    Ok(())
}

#[test]
fn alt_d_does_not_request_diagnostics_with_pending_approval() -> Result<()> {
    let temp = tempdir()?;
    let mut config = test_config();
    config.workspace.root = temp.path().display().to_string();
    config.code_intelligence.enabled = true;
    let mut app = AppState::from_root_config(&temp.path().join("sigil.toml"), &config);
    inject_write_file_approval(&mut app, sample_approval_preview())?;

    let action = app.request_changed_files_diagnostics();

    assert!(action.is_none());
    assert_eq!(
        app.last_notice.as_deref(),
        Some("finish the pending approval before checking changes")
    );
    assert!(!app.events.iter().any(|event| event.label == "code:check"));
    Ok(())
}

#[test]
fn alt_p_pauses_only_the_exact_running_task_plan() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    let task_id = sigil_kernel::TaskId::new("task_1")?;
    app.sync_current_session_state(vec![
        SessionLogEntry::Control(ControlEntry::TaskRun(sigil_kernel::TaskRunEntry {
            task_id: task_id.clone(),
            parent_session_ref: sigil_kernel::SessionRef::new_relative("parent.jsonl")?,
            objective: "pause safely".to_owned(),
            title: None,
            status: sigil_kernel::TaskRunStatus::Running,
            reason: None,
        })),
        SessionLogEntry::Control(ControlEntry::TaskPlan(sigil_kernel::TaskPlanEntry {
            task_id,
            plan_version: 2,
            status: sigil_kernel::TaskPlanStatus::Accepted,
            steps: vec![sigil_kernel::TaskStepSpec {
                step_id: sigil_kernel::TaskStepId::new("step_1")?,
                title: "Inspect".to_owned(),
                display_name: None,
                detail: None,
                role: sigil_kernel::AgentRole::SubagentRead,
                depends_on: Vec::new(),
                intent_refs: Vec::new(),
                mode: Some(sigil_kernel::TaskStepMode::Read),
                isolation: Some(sigil_kernel::TaskIsolationMode::SharedReadOnly),
            }],
            reason: None,
        })),
    ]);
    app.runtime.is_busy = true;

    let action = app.handle_key_event(KeyEvent::new(KeyCode::Char('p'), KeyModifiers::ALT))?;
    let Some(AppAction::PauseTask { request }) = action else {
        panic!("Alt-P should create an exact pause action");
    };
    assert!(request.has_exact_identity());
    assert_eq!(request.task_id.as_str(), "task_1");
    assert_eq!(request.plan_version, 2);
    assert_eq!(app.last_notice(), Some("pausing task task_1"));

    app.runtime.is_busy = false;
    assert!(
        app.handle_key_event(KeyEvent::new(KeyCode::Char('p'), KeyModifiers::ALT))?
            .is_none()
    );
    assert_eq!(app.last_notice(), Some("no active task to pause"));
    Ok(())
}

#[test]
fn pending_approval_blocks_agent_cycle_commands() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    inject_write_file_approval(&mut app, sample_approval_preview())?;

    assert!(!app.handle_ui_command(crate::commands::UiCommand::CycleAgentView));
    assert_eq!(
        app.last_notice(),
        Some("finish the pending approval before switching agents")
    );
    assert!(!app.handle_ui_command(crate::commands::UiCommand::CycleAgentViewPrevious));
    assert_eq!(
        app.last_notice(),
        Some("finish the pending approval before switching agents")
    );
    Ok(())
}

#[test]
fn enter_plan_mode_ui_command_is_not_handled_by_global_dispatch() {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());

    assert!(!app.handle_ui_command(crate::commands::UiCommand::EnterPlanMode));
}

#[test]
fn focused_terminal_task_cancel_rejects_missing_owner_and_inactive_tasks_while_busy() -> Result<()>
{
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());

    let missing = app.handle_key_event(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::ALT))?;
    assert!(missing.is_none());
    assert_eq!(app.last_notice(), Some("focus a terminal task first"));

    app.sync_current_session_state(vec![SessionLogEntry::Control(ControlEntry::TerminalTask(
        dispatch_terminal_entry("terminal-1", sigil_kernel::TerminalTaskStatus::Running)?,
    ))]);
    app.timeline_state.selected_tool_activity_key = Some("terminal_task:terminal-1".to_owned());
    app.runtime.is_busy = true;
    app.terminal_task_control_identities.insert(
        "terminal-1".to_owned(),
        crate::runner::TerminalTaskControlIdentity {
            session_scope_id: "session-scope-1".to_owned(),
            run_id: "foreground-run-1".to_owned(),
            task_id: "terminal-1".to_owned(),
            expected_generation: 1,
        },
    );
    let busy = app.handle_key_event(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::ALT))?;
    assert!(busy.is_none());
    assert_eq!(
        app.last_notice(),
        Some("Alt-X again to cancel terminal task terminal-1")
    );

    app.runtime.is_busy = false;
    app.sync_current_session_state(vec![SessionLogEntry::Control(ControlEntry::TerminalTask(
        dispatch_terminal_entry(
            "terminal-1",
            sigil_kernel::TerminalTaskStatus::Exited { exit_code: Some(0) },
        )?,
    ))]);
    let inactive = app.handle_key_event(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::ALT))?;
    assert!(inactive.is_none());
    assert_eq!(
        app.last_notice(),
        Some("terminal task terminal-1 is not running")
    );
    assert!(!app.handle_ui_command(crate::commands::UiCommand::CancelFocusedTerminalTask));
    Ok(())
}

fn dispatch_terminal_entry(
    task_id: &str,
    status: sigil_kernel::TerminalTaskStatus,
) -> Result<sigil_kernel::TerminalTaskEntry> {
    Ok(sigil_kernel::TerminalTaskEntry {
        schema_version: sigil_kernel::terminal_task::TERMINAL_TASK_SCHEMA_VERSION,
        handle: sigil_kernel::TerminalTaskHandle {
            task_id: sigil_kernel::TerminalTaskId::new(task_id)?,
            command_sha256: "0".repeat(64),
            cwd_label: ".".to_owned(),
            shell_label: "sh".to_owned(),
            shell_sha256: "1".repeat(64),
            log_ref: format!("terminal-log:{task_id}"),
            created_at_ms: 10,
            execution_backend: None,
            execution_backend_capabilities: None,
            enforcement_backend: None,
            enforcement_backend_capabilities: None,
            sandbox_profile: None,
        },
        generation: 1,
        status,
        readiness: sigil_kernel::TerminalReadinessStatus::None,
        output_preview: Some("running output".to_owned()),
        output_hash: Some("hash".to_owned()),
        output_truncated: false,
        output_total_bytes: 0,
        output_limit_bytes: None,
        output_termination_reason: None,
        cleanup: None,
        updated_at_ms: 20,
    })
}

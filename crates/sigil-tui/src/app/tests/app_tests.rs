use super::*;

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

#[test]
fn agent_sidebar_rows_show_plan_subagent_availability_and_child_sessions() -> Result<()> {
    let task_id = sigil_kernel::TaskId::new("task_1")?;
    let step_id = sigil_kernel::TaskStepId::new("step_1")?;
    let child_task_id = sigil_kernel::TaskId::new("child_1")?;
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());

    let rows = app.agent_sidebar_rows();
    assert!(rows.iter().any(|row| {
        row.label == "subagents" && row.detail == "available via /plan" && !row.muted
    }));

    app.sync_current_session_state(vec![
        SessionLogEntry::Control(ControlEntry::TaskRun(sigil_kernel::TaskRunEntry {
            task_id: task_id.clone(),
            parent_session_ref: sigil_kernel::SessionRef::new_relative("parent.jsonl")?,
            objective: "review workspace".to_owned(),
            status: sigil_kernel::TaskRunStatus::Running,
            reason: None,
        })),
        SessionLogEntry::Control(ControlEntry::TaskChildSession(
            sigil_kernel::TaskChildSessionEntry {
                task_id: task_id.clone(),
                plan_version: 1,
                step_id: step_id.clone(),
                child_task_id,
                child_session_ref: sigil_kernel::SessionRef::new_relative(
                    "children/task_1/step_1-child_1.jsonl",
                )?,
                role: sigil_kernel::AgentRole::SubagentRead,
                status: sigil_kernel::TaskChildSessionStatus::Started,
                summary_hash: None,
            },
        )),
    ]);

    let rows = app.agent_sidebar_rows();

    assert!(rows.iter().any(|row| {
        row.label == "subagents" && row.detail == "1 child session active" && !row.muted
    }));

    app.sync_current_session_state(vec![
        SessionLogEntry::Control(ControlEntry::TaskRun(sigil_kernel::TaskRunEntry {
            task_id: task_id.clone(),
            parent_session_ref: sigil_kernel::SessionRef::new_relative("parent.jsonl")?,
            objective: "review workspace".to_owned(),
            status: sigil_kernel::TaskRunStatus::Completed,
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
        row.label == "subagents" && row.detail == "1 child session recorded" && !row.muted
    }));
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
                detail: None,
                role: sigil_kernel::AgentRole::Executor,
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
    assert!(lines.contains(&"▶ 1. running step_1 · inspect".to_owned()));
    assert!(lines.contains(&"routes: unverified".to_owned()));
    assert!(lines.contains(&"child: unavailable".to_owned()));
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
                    detail: None,
                    role: sigil_kernel::AgentRole::Executor,
                },
                sigil_kernel::TaskStepSpec {
                    step_id: sigil_kernel::TaskStepId::new("overview")?,
                    title: "扫描项目整体结构".to_owned(),
                    detail: None,
                    role: sigil_kernel::AgentRole::Executor,
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
    assert!(lines.contains(&"! 1. failed gate_check · 跑门禁".to_owned()));
    assert!(lines.contains(&"· 2. pending overview · 扫描项目整体结构".to_owned()));
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
                    detail: None,
                    role: sigil_kernel::AgentRole::Executor,
                },
                sigil_kernel::TaskStepSpec {
                    step_id: interrupted_step.clone(),
                    title: "review interrupted".to_owned(),
                    detail: None,
                    role: sigil_kernel::AgentRole::Executor,
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

    assert!(lines.contains(&"× 1. cancelled cancel_setup · cancel setup".to_owned()));
    assert!(lines.contains(&"⏸ 2. interrupted interrupt_review · review interrupted".to_owned()));
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
                detail: None,
                role: sigil_kernel::AgentRole::Executor,
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
    assert!(lines.contains(&"! 2. blocked step_2 · step 2".to_owned()));
    assert!(lines.contains(&"▶ 8. running step_8 · step 8".to_owned()));
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
                detail: None,
                role: sigil_kernel::AgentRole::Executor,
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
                detail: None,
                role: sigil_kernel::AgentRole::Executor,
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

    assert!(lines.contains(&"▶ 1. running step_1 · step 1".to_owned()));
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
                    detail: None,
                    role: sigil_kernel::AgentRole::Executor,
                },
                sigil_kernel::TaskStepSpec {
                    step_id: sigil_kernel::TaskStepId::new("step_2")?,
                    title: "step 2".to_owned(),
                    detail: None,
                    role: sigil_kernel::AgentRole::Executor,
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
    assert!(lines.contains(&"· 2. pending step_2 · step 2".to_owned()));
    assert!(!lines.iter().any(|line| line.starts_with("last: ")));
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
    assert_eq!(app.context_usage_line(), "ctx: n/a · 0 tok");
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
            canonical: "/model",
            arg: String::new(),
        },
        "/model".to_owned(),
    )?;
    assert!(action.is_none());
    assert_eq!(app.last_notice(), Some("usage: /model <flash|pro|id>"));

    let action = app.execute_slash_command(
        crate::slash::ResolvedSlashCommand {
            canonical: "/bogus",
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
            canonical: "/model",
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
            canonical: "/model",
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
        },
        status,
        output_preview: Some("running output".to_owned()),
        output_hash: Some("hash".to_owned()),
        output_truncated: false,
        updated_at_ms: 20,
    })
}

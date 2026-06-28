use super::*;
use crate::slash::ResolvedSlashCommand;

fn config_for_workspace(workspace_root: &Path) -> RootConfig {
    let mut config = test_config();
    config.workspace.root = workspace_root.display().to_string();
    config
}

fn write_workspace_skill(workspace_root: &Path, id: &str, body: &str) -> Result<()> {
    let path = workspace_root
        .join(".sigil")
        .join("skills")
        .join(id)
        .join("SKILL.md");
    std::fs::create_dir_all(path.parent().expect("skill path should have parent"))?;
    std::fs::write(path, body)?;
    Ok(())
}

fn write_workspace_agent(workspace_root: &Path, id: &str, body: &str) -> Result<()> {
    let path = workspace_root
        .join(".sigil")
        .join("agents")
        .join(id)
        .join("agent.toml");
    std::fs::create_dir_all(path.parent().expect("agent path should have parent"))?;
    std::fs::write(path, body)?;
    Ok(())
}

#[test]
fn compact_command_dispatches_worker_action_when_idle() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.input = "/compact".to_owned();

    let action = app.submit_input()?;

    assert!(matches!(action, Some(AppAction::CompactNow)));
    Ok(())
}

#[test]
fn compact_command_prefix_is_resolved_to_exact_command() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.input = "/comp".to_owned();

    let action = app.submit_input()?;

    assert!(matches!(action, Some(AppAction::CompactNow)));
    Ok(())
}

#[test]
fn plan_command_dispatches_plan_prompt_when_idle() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.input = "/plan implement task mode".to_owned();

    let action = app.submit_input()?;

    assert!(matches!(
        action,
        Some(AppAction::SubmitPlanPrompt(prompt)) if prompt == "implement task mode"
    ));
    assert!(app.is_busy);
    assert_eq!(app.last_notice(), Some("planning"));
    assert_eq!(app.input, "");
    assert!(!app.has_slash_selector());
    assert!(app.timeline.iter().any(|entry| {
        entry.role == TimelineRole::User && entry.text == "/plan implement task mode"
    }));
    Ok(())
}

#[test]
fn task_command_dispatches_durable_task_when_idle() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.input = "/task implement task mode".to_owned();

    let action = app.submit_input()?;

    assert!(matches!(
        action,
        Some(AppAction::SubmitTask(prompt)) if prompt == "implement task mode"
    ));
    assert!(app.is_busy);
    assert_eq!(app.last_notice(), Some("planning task"));
    assert!(app.timeline.iter().any(|entry| {
        entry.role == TimelineRole::User && entry.text == "/task implement task mode"
    }));
    Ok(())
}

#[test]
fn empty_plan_command_enters_one_shot_plan_mode() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.input = "/plan".to_owned();

    assert!(app.submit_input()?.is_none());
    assert_eq!(app.composer_mode_label(), "Plan");
    assert_eq!(app.last_notice(), Some("plan mode"));
    assert_eq!(app.input, "");
    assert!(!app.has_slash_selector());

    app.input = "inspect crates/sigil-tui".to_owned();
    let action = app.submit_input()?;

    assert!(matches!(
        action,
        Some(AppAction::SubmitPlanPrompt(prompt)) if prompt == "inspect crates/sigil-tui"
    ));
    assert_eq!(app.composer_mode_label(), "Build");
    assert_eq!(app.last_notice(), Some("planning"));
    Ok(())
}

#[test]
fn plan_command_body_hides_slash_selector_after_command_boundary() {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.input = "/plan implement task mode".to_owned();
    app.reset_slash_selector();

    assert!(!app.has_slash_selector());
    assert!(app.slash_selector_rows().is_empty());
    assert!(app.slash_selector_empty_message().is_none());

    app.input = "/agent ".to_owned();
    app.reset_slash_selector();

    assert!(app.has_slash_selector());
    assert_eq!(app.slash_selector_title(), Some("Agent"));
    assert!(!app.slash_selector_rows().is_empty());
}

#[test]
fn plan_continue_command_points_to_task_continue() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.input = "/plan continue".to_owned();

    let action = app.submit_input()?;

    assert!(action.is_none());
    assert!(!app.is_busy);
    assert_eq!(app.last_notice(), Some("use /task continue"));
    assert!(app.timeline.iter().any(|entry| {
        entry.role == TimelineRole::Notice
            && entry
                .text
                .contains("plan mode cannot continue durable tasks")
    }));
    Ok(())
}

#[test]
fn agent_command_switches_visible_agent_view() -> Result<()> {
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
                status: sigil_kernel::TaskChildSessionStatus::Completed,
                summary_hash: None,
            },
        )),
    ]);

    app.input = "/agent child_1".to_owned();
    assert!(app.submit_input()?.is_none());
    assert_eq!(app.active_agent_label(), "仓库审查");

    app.input = "/agent main".to_owned();
    assert!(app.submit_input()?.is_none());
    assert_eq!(app.active_agent_label(), "main");

    app.input = "/agent 仓库审查".to_owned();
    assert!(app.submit_input()?.is_none());
    assert_eq!(app.active_agent_label(), "仓库审查");

    app.input = "/agent main".to_owned();
    assert!(app.submit_input()?.is_none());
    assert_eq!(app.active_agent_label(), "main");

    app.input = "/agent next".to_owned();
    assert!(app.submit_input()?.is_none());
    assert_eq!(app.active_agent_label(), "仓库审查");
    Ok(())
}

#[test]
fn agent_rename_command_persists_child_display_name() -> Result<()> {
    let temp = tempdir()?;
    let task_id = sigil_kernel::TaskId::new("task_1")?;
    let step_id = sigil_kernel::TaskStepId::new("step_1")?;
    let mut app = AppState::from_root_config(
        temp.path().join("sigil.toml").as_path(),
        &config_for_workspace(temp.path()),
    );
    app.session_log_path = temp.path().join(".sigil/sessions/current.jsonl");
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
            }],
            reason: None,
        })),
        SessionLogEntry::Control(ControlEntry::TaskChildSession(
            sigil_kernel::TaskChildSessionEntry {
                task_id,
                plan_version: 1,
                step_id,
                child_task_id: sigil_kernel::TaskId::new("child_1")?,
                child_session_ref: sigil_kernel::SessionRef::new_relative(
                    "children/task_1/step_1-child_1.jsonl",
                )?,
                role: sigil_kernel::AgentRole::SubagentRead,
                status: sigil_kernel::TaskChildSessionStatus::Completed,
                summary_hash: None,
            },
        )),
    ]);

    app.input = "/agent rename child_1 德语译员".to_owned();
    assert!(app.submit_input()?.is_none());
    app.input = "/agent 德语译员".to_owned();
    assert!(app.submit_input()?.is_none());

    assert_eq!(app.active_agent_label(), "德语译员");
    let persisted = JsonlSessionStore::read_entries(&app.session_log_path)?;
    assert!(persisted.iter().any(|entry| {
        matches!(
            entry,
            SessionLogEntry::Control(ControlEntry::TaskChildSessionDisplayName(rename))
                if rename.child_task_id.as_str() == "child_1"
                    && rename.display_name == "德语译员"
        )
    }));
    Ok(())
}

#[test]
fn agent_label_falls_back_to_role_ordinal_without_display_name() -> Result<()> {
    let task_id = sigil_kernel::TaskId::new("task_1")?;
    let step_id = sigil_kernel::TaskStepId::new("translate_german")?;
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.sync_current_session_state(vec![
        SessionLogEntry::Control(ControlEntry::TaskRun(sigil_kernel::TaskRunEntry {
            task_id: task_id.clone(),
            parent_session_ref: sigil_kernel::SessionRef::new_relative("parent.jsonl")?,
            objective: "translate".to_owned(),
            status: sigil_kernel::TaskRunStatus::Running,
            reason: None,
        })),
        SessionLogEntry::Control(ControlEntry::TaskPlan(sigil_kernel::TaskPlanEntry {
            task_id: task_id.clone(),
            plan_version: 1,
            status: sigil_kernel::TaskPlanStatus::Accepted,
            steps: vec![sigil_kernel::TaskStepSpec {
                step_id: step_id.clone(),
                title: "让子 agent 翻译为德语".to_owned(),
                display_name: None,
                detail: None,
                role: sigil_kernel::AgentRole::SubagentRead,
            }],
            reason: None,
        })),
        SessionLogEntry::Control(ControlEntry::TaskChildSession(
            sigil_kernel::TaskChildSessionEntry {
                task_id,
                plan_version: 1,
                step_id,
                child_task_id: sigil_kernel::TaskId::new("child_1")?,
                child_session_ref: sigil_kernel::SessionRef::new_relative(
                    "children/task_1/translate_german-child_1.jsonl",
                )?,
                role: sigil_kernel::AgentRole::SubagentRead,
                status: sigil_kernel::TaskChildSessionStatus::Completed,
                summary_hash: None,
            },
        )),
    ]);

    app.input = "/agent ".to_owned();
    let rows = app.slash_selector_rows();
    assert!(rows.iter().any(|(label, _)| label == "read 1"));
    assert!(!rows.iter().any(|(label, _)| label == "德语译员"));

    app.input = "/agent read 1".to_owned();
    assert!(app.submit_input()?.is_none());
    assert_eq!(app.active_agent_label(), "read 1");
    Ok(())
}

#[test]
fn agent_command_selector_lists_main_child_and_navigation() -> Result<()> {
    let task_id = sigil_kernel::TaskId::new("task_1")?;
    let step_id = sigil_kernel::TaskStepId::new("step_1")?;
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
            }],
            reason: None,
        })),
        SessionLogEntry::Control(ControlEntry::TaskChildSession(
            sigil_kernel::TaskChildSessionEntry {
                task_id,
                plan_version: 1,
                step_id,
                child_task_id: sigil_kernel::TaskId::new("child_1")?,
                child_session_ref: sigil_kernel::SessionRef::new_relative(
                    "children/task_1/step_1-child_1.jsonl",
                )?,
                role: sigil_kernel::AgentRole::SubagentRead,
                status: sigil_kernel::TaskChildSessionStatus::Started,
                summary_hash: None,
            },
        )),
    ]);
    app.input = "/agent ".to_owned();

    let rows = app.slash_selector_rows();

    assert!(
        rows.iter()
            .any(|(label, description)| label == "main" && description == "◉ current session")
    );
    assert!(
        rows.iter().any(|(label, description)| label == "仓库审查"
            && description == "◐ subagent_read · v1:step_1")
    );
    assert!(!rows.iter().any(|(label, _)| label == "next"));
    assert!(!rows.iter().any(|(label, _)| label == "prev"));
    assert_eq!(app.slash_selector_title(), Some("Agent"));

    app.input = "/agent rename ".to_owned();
    app.reset_slash_selector();
    let rename_rows = app.slash_selector_rows();
    assert!(
        rename_rows
            .iter()
            .any(|(label, description)| label == "仓库审查" && description.contains("rename"))
    );

    app.input = "/agent rename child_1 德语译员".to_owned();
    app.reset_slash_selector();
    assert!(!app.has_slash_selector());
    Ok(())
}

#[test]
fn agent_mention_selector_lists_only_trusted_enabled_user_invocable_profiles() -> Result<()> {
    let workspace = tempdir()?;
    write_workspace_agent(
        workspace.path(),
        "repo-review",
        r#"
description = "Review repository changes."
instructions = "Use read-only tools."
trust = "trusted"
nickname_candidates = ["Repo Review"]
aliases = ["rr"]
"#,
    )?;
    write_workspace_agent(
        workspace.path(),
        "repo-disabled",
        r#"
description = "Disabled review agent."
instructions = "Use read-only tools."
trust = "trusted"
enabled = false
"#,
    )?;
    write_workspace_agent(
        workspace.path(),
        "repo-private",
        r#"
description = "Private review agent."
instructions = "Use read-only tools."
trust = "trusted"
user_invocable = false
"#,
    )?;
    write_workspace_agent(
        workspace.path(),
        "repo-draft",
        r#"
description = "Needs review agent."
instructions = "Use read-only tools."
"#,
    )?;
    let config = config_for_workspace(workspace.path());
    let mut app = AppState::from_root_config(&workspace.path().join("sigil.toml"), &config);
    app.input = "@repo".to_owned();

    assert!(app.has_slash_selector());
    assert!(app.has_agent_mention_selector());
    assert_eq!(app.slash_selector_title(), Some("Agent"));
    let rows = app.slash_selector_rows();
    assert!(
        rows.iter()
            .any(|(label, description)| label == "@repo-review"
                && description.contains("subagent · workspace · Review repository changes."))
    );
    assert!(!rows.iter().any(|(label, _)| label == "@repo-disabled"));
    assert!(!rows.iter().any(|(label, _)| label == "@repo-private"));
    assert!(!rows.iter().any(|(label, _)| label == "@repo-draft"));

    app.accept_slash_selector();
    assert_eq!(app.input, "@repo-review ");

    app.input = "@rr".to_owned();
    app.reset_slash_selector();
    let alias_rows = app.slash_selector_rows();
    assert!(
        alias_rows
            .iter()
            .any(|(label, description)| { label == "@rr" && description.contains("aliases: rr") })
    );
    app.accept_slash_selector();
    assert_eq!(app.input, "@rr ");

    app.input = "@missing".to_owned();
    app.reset_slash_selector();
    assert!(app.slash_selector_rows().is_empty());
    assert_eq!(
        app.slash_selector_empty_message(),
        Some("no matching agent")
    );
    Ok(())
}

#[test]
fn agent_mention_submit_dispatches_agent_invocation() -> Result<()> {
    let workspace = tempdir()?;
    write_workspace_agent(
        workspace.path(),
        "repo-review",
        r#"
description = "Review repository changes."
instructions = "Use read-only tools."
trust = "trusted"
aliases = ["rr"]
"#,
    )?;
    let config = config_for_workspace(workspace.path());
    let mut app = AppState::from_root_config(&workspace.path().join("sigil.toml"), &config);
    app.input = "@repo-review audit crates/sigil-tui".to_owned();

    let action = app.submit_input()?;

    assert!(matches!(
        action,
        Some(AppAction::InvokeAgentProfile { profile_id, prompt, parent_prompt })
            if profile_id == "repo-review"
                && prompt == "audit crates/sigil-tui"
                && parent_prompt == "@repo-review audit crates/sigil-tui"
    ));
    assert!(app.is_busy);
    assert_eq!(app.last_notice(), Some("waiting for agent @repo-review"));
    assert_eq!(app.input, "");
    assert!(app.timeline.iter().any(|entry| {
        entry.role == TimelineRole::User && entry.text == "@repo-review audit crates/sigil-tui"
    }));
    Ok(())
}

#[test]
fn agent_mention_alias_submit_dispatches_canonical_profile() -> Result<()> {
    let workspace = tempdir()?;
    write_workspace_agent(
        workspace.path(),
        "repo-review",
        r#"
description = "Review repository changes."
instructions = "Use read-only tools."
trust = "trusted"
aliases = ["rr"]
"#,
    )?;
    let config = config_for_workspace(workspace.path());
    let mut app = AppState::from_root_config(&workspace.path().join("sigil.toml"), &config);
    app.input = "@rr audit crates/sigil-tui".to_owned();

    let action = app.submit_input()?;

    assert!(matches!(
        action,
        Some(AppAction::InvokeAgentProfile { profile_id, prompt, parent_prompt })
            if profile_id == "repo-review"
                && prompt == "audit crates/sigil-tui"
                && parent_prompt == "@rr audit crates/sigil-tui"
    ));
    Ok(())
}

#[test]
fn agent_slash_name_submit_dispatches_canonical_profile() -> Result<()> {
    let workspace = tempdir()?;
    write_workspace_agent(
        workspace.path(),
        "repo-review",
        r#"
description = "Review repository changes."
instructions = "Use read-only tools."
trust = "trusted"
slash_names = ["review-agent"]
"#,
    )?;
    let config = config_for_workspace(workspace.path());
    let mut app = AppState::from_root_config(&workspace.path().join("sigil.toml"), &config);
    app.input = "/review-agent audit crates/sigil-tui".to_owned();

    assert!(app.has_slash_selector());
    assert!(
        app.slash_selector_rows()
            .iter()
            .any(|(label, _)| label == "/review-agent")
    );

    let action = app.submit_input()?;

    assert!(matches!(
        action,
        Some(AppAction::InvokeAgentProfile { profile_id, prompt, parent_prompt })
            if profile_id == "repo-review"
                && prompt == "audit crates/sigil-tui"
                && parent_prompt == "/review-agent audit crates/sigil-tui"
    ));
    assert_eq!(app.last_notice(), Some("waiting for agent @repo-review"));
    Ok(())
}

#[test]
fn agent_slash_name_covers_selector_edges_and_fallback_resolution() -> Result<()> {
    let workspace = tempdir()?;
    write_workspace_agent(
        workspace.path(),
        "repo-review",
        r#"
description = "Review repository changes."
instructions = "Use read-only tools."
trust = "trusted"
aliases = ["rr"]
slash_names = ["review-agent"]
"#,
    )?;
    let config = config_for_workspace(workspace.path());
    let mut app = AppState::from_root_config(&workspace.path().join("sigil.toml"), &config);

    assert!(app.slash_agent_entries("review", "").is_empty());
    assert!(app.slash_agent_entries("", "").is_empty());
    let slash_rows = app.slash_agent_entries("/review", "");
    assert_eq!(slash_rows.len(), 1);
    assert_eq!(slash_rows[0].fill, "/review-agent ");

    let command = app
        .resolve_slash_command("/review-agent audit crates/sigil-tui")
        .expect("direct slash alias should resolve without selector state");
    assert_eq!(command.canonical, "@agent");
    assert_eq!(command.arg, "repo-review audit crates/sigil-tui");

    app.input = "/review-agent audit crates/sigil-tui".to_owned();
    app.is_busy = true;
    assert!(app.submit_input()?.is_none());
    assert_eq!(app.last_notice(), Some("busy; invoke agent later"));
    Ok(())
}

#[test]
fn agent_slash_command_reports_usage_for_missing_agent_prompt() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());

    assert!(
        app.execute_slash_command(
            ResolvedSlashCommand {
                canonical: "@agent".to_owned(),
                arg: "repo-review".to_owned(),
            },
            "@repo-review".to_owned(),
        )?
        .is_none()
    );
    assert_eq!(app.last_notice(), Some("usage: /agent-name <prompt>"));

    assert!(
        app.execute_slash_command(
            ResolvedSlashCommand {
                canonical: "@agent".to_owned(),
                arg: "repo-review ".to_owned(),
            },
            "/review-agent".to_owned(),
        )?
        .is_none()
    );
    assert_eq!(app.last_notice(), Some("usage: /agent-name <prompt>"));
    Ok(())
}

#[test]
fn agent_mention_submit_rejects_unknown_agent_without_clearing_input() -> Result<()> {
    let workspace = tempdir()?;
    let config = config_for_workspace(workspace.path());
    let mut app = AppState::from_root_config(&workspace.path().join("sigil.toml"), &config);
    app.input = "@missing audit crates".to_owned();

    let action = app.submit_input()?;

    assert!(action.is_none());
    assert!(!app.is_busy);
    assert_eq!(app.last_notice(), Some("unknown agent missing"));
    assert_eq!(app.input, "@missing audit crates");
    Ok(())
}

#[test]
fn plain_prompt_remains_chat_when_session_has_unfinished_task() -> Result<()> {
    let task_id = sigil_kernel::TaskId::new("task_1")?;
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.sync_current_session_state(vec![SessionLogEntry::Control(ControlEntry::TaskRun(
        sigil_kernel::TaskRunEntry {
            task_id,
            parent_session_ref: sigil_kernel::SessionRef::new_relative("parent.jsonl")?,
            objective: "review workspace".to_owned(),
            status: sigil_kernel::TaskRunStatus::Running,
            reason: Some("task running".to_owned()),
        },
    ))]);
    app.input = "优先看 runtime 状态同步".to_owned();

    let action = app.submit_input()?;

    assert!(matches!(
        action,
        Some(AppAction::SubmitPrompt(ref prompt)) if prompt == "优先看 runtime 状态同步"
    ));
    assert!(app.is_busy);
    assert!(app.timeline.iter().any(|entry| {
        entry.role == TimelineRole::User && entry.text == "优先看 runtime 状态同步"
    }));
    assert_eq!(app.last_notice(), Some("thinking"));
    Ok(())
}

#[test]
fn plain_prompt_remains_chat_when_session_has_no_task() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.input = "优先看 runtime 状态同步".to_owned();

    let action = app.submit_input()?;

    assert!(matches!(
        action,
        Some(AppAction::SubmitPrompt(prompt)) if prompt == "优先看 runtime 状态同步"
    ));
    Ok(())
}

#[test]
fn new_command_dispatches_new_session_action_when_idle() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    let current_session_log_path = app.session_log_path.clone();
    app.push_timeline(TimelineRole::Assistant, "old context");
    app.input = "/new".to_owned();

    let action = app.submit_input()?;

    assert!(matches!(
        action,
        Some(AppAction::StartNewSession { ref session_log_path })
            if session_log_path != &current_session_log_path
                && session_log_path.parent() == Some(app.session_log_dir.as_path())
    ));
    assert!(!app.is_busy);
    assert!(
        !app.timeline
            .iter()
            .any(|entry| entry.role == TimelineRole::User && entry.text == "/new")
    );
    Ok(())
}

#[test]
fn new_command_reports_busy_without_dispatching() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.is_busy = true;
    app.input = "/new".to_owned();

    assert!(app.submit_input()?.is_none());
    assert!(app.timeline.iter().any(|entry| {
        entry.role == TimelineRole::Notice && entry.text == "busy; start new session later"
    }));
    Ok(())
}

#[test]
fn task_continue_command_can_pass_guidance() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.input = "/task continue 优先看 runtime 状态同步".to_owned();

    let action = app.submit_input()?;

    assert!(matches!(
        action,
        Some(AppAction::ContinueTask {
            task_id: None,
            guidance: Some(ref guidance)
        }) if guidance == "优先看 runtime 状态同步"
    ));
    assert!(app.is_busy);
    Ok(())
}

#[test]
fn plan_command_reports_busy_and_usage_without_dispatching() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.is_busy = true;
    app.input = "/plan implement task mode".to_owned();

    assert!(app.submit_input()?.is_none());
    assert!(
        app.timeline.iter().any(|entry| {
            entry.role == TimelineRole::Notice && entry.text == "busy; plan later"
        })
    );

    app.is_busy = false;
    app.input = "/plan   ".to_owned();

    assert!(app.submit_input()?.is_none());
    assert_eq!(app.last_notice(), Some("plan mode"));
    assert_eq!(app.composer_mode_label(), "Plan");
    Ok(())
}

#[test]
fn effort_command_updates_runtime_effort_and_worker_submit_uses_it() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.input = "/effort high".to_owned();

    assert!(app.submit_input()?.is_none());
    assert_eq!(app.reasoning_effort.as_str(), "high");

    let command = app.into_worker_command(AppAction::SubmitPrompt("hello".to_owned()));
    assert!(matches!(
        command,
        WorkerCommand::SubmitPrompt {
            prompt,
            reasoning_effort: ReasoningEffort::High,
        } if prompt == "hello"
    ));
    Ok(())
}

#[test]
fn model_command_switches_runtime_model_and_starts_fresh_session() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    let previous_session_id = app.session_id.clone();
    app.push_timeline(TimelineRole::Assistant, "old context");
    app.input = "/model pro".to_owned();

    let action = app.submit_input()?;

    assert!(matches!(
        action,
        Some(AppAction::RuntimeConfigUpdated { .. })
    ));
    assert_eq!(app.model_name, "deepseek-v4-pro");
    assert_ne!(app.session_id, previous_session_id);
    assert!(
        app.timeline
            .iter()
            .any(|entry| entry.text.contains("model -> deepseek-v4-pro"))
    );
    Ok(())
}

#[test]
fn slash_command_hints_include_prefix_matches() {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.input = "/res".to_owned();
    let hints = app.slash_command_hints();
    assert!(hints.iter().any(|hint| hint.contains("/resume")));
}

#[test]
fn slash_skill_invocation_resolves_inline_skill_after_native_commands() -> Result<()> {
    let workspace = tempdir()?;
    write_workspace_skill(
        workspace.path(),
        "repo-review",
        r#"---
name: repo-review
description: Review repository changes.
trust: trusted
user-invocable: true
run-as: inline
---

# Repo Review
"#,
    )?;
    let config = config_for_workspace(workspace.path());
    let mut app = AppState::from_root_config(&workspace.path().join("sigil.toml"), &config);
    app.input = "/repo-review crates/sigil-tui".to_owned();

    let rows = app.slash_selector_rows();
    assert!(rows.iter().any(|(label, description)| {
        label == "/repo-review" && description.contains("skill · inline")
    }));
    let action = app.submit_input()?;

    assert!(matches!(
        action,
        Some(AppAction::InvokeInlineSkill { skill_id, arguments })
            if skill_id == "repo-review" && arguments == "crates/sigil-tui"
    ));
    assert!(app.is_busy);
    assert!(app.timeline.iter().any(|entry| {
        entry.role == TimelineRole::User && entry.text == "/repo-review crates/sigil-tui"
    }));
    Ok(())
}

#[test]
fn slash_skill_invocation_excludes_child_session_skill() -> Result<()> {
    let workspace = tempdir()?;
    write_workspace_skill(
        workspace.path(),
        "repo-audit",
        r#"---
name: repo-audit
description: Audit repository changes.
trust: trusted
user-invocable: true
run-as: child-session
---

# Repo Audit
"#,
    )?;
    let config = config_for_workspace(workspace.path());
    let mut app = AppState::from_root_config(&workspace.path().join("sigil.toml"), &config);
    app.input = "/repo-audit --depth full".to_owned();

    let rows = app.slash_selector_rows();
    assert!(!rows.iter().any(|(label, _)| label == "/repo-audit"));
    let action = app.submit_input()?;

    assert!(action.is_none());
    assert!(!app.is_busy);
    assert_eq!(app.last_notice(), Some("unknown slash command"));
    Ok(())
}

#[test]
fn native_slash_command_shadows_matching_skill_id() -> Result<()> {
    let workspace = tempdir()?;
    write_workspace_skill(
        workspace.path(),
        "config",
        r#"---
name: config
description: Skill with a native command id.
trust: trusted
user-invocable: true
---

# Config Skill
"#,
    )?;
    let config = config_for_workspace(workspace.path());
    let mut app = AppState::from_root_config(&workspace.path().join("sigil.toml"), &config);
    app.input = "/config".to_owned();

    let action = app.submit_input()?;

    assert!(action.is_none());
    assert!(app.is_config_mode());
    assert!(!app.is_busy);
    Ok(())
}

#[test]
fn slash_skill_invocation_requires_trust() -> Result<()> {
    let workspace = tempdir()?;
    write_workspace_skill(
        workspace.path(),
        "needs-review",
        r#"---
name: needs-review
description: Review required.
user-invocable: true
---

# Needs Review
"#,
    )?;
    let config = config_for_workspace(workspace.path());
    let mut app = AppState::from_root_config(&workspace.path().join("sigil.toml"), &config);
    app.input = "/needs-review target".to_owned();

    let action = app.submit_input()?;

    assert!(action.is_none());
    assert!(!app.is_busy);
    assert_eq!(app.last_notice(), Some("skill needs-review is not trusted"));
    Ok(())
}

#[test]
fn slash_skill_invocation_guard_edges_report_notices() -> Result<()> {
    let workspace = tempdir()?;
    write_workspace_skill(
        workspace.path(),
        "busy-skill",
        r#"---
name: busy-skill
description: Busy skill.
trust: trusted
user-invocable: true
---

# Busy
"#,
    )?;
    write_workspace_skill(
        workspace.path(),
        "disabled-skill",
        r#"---
name: disabled-skill
description: Disabled skill.
trust: trusted
enabled: false
user-invocable: true
---

# Disabled
"#,
    )?;
    write_workspace_skill(
        workspace.path(),
        "hidden-skill",
        r#"---
name: hidden-skill
description: Hidden skill.
trust: trusted
user-invocable: false
---

# Hidden
"#,
    )?;
    let config = config_for_workspace(workspace.path());
    let mut app = AppState::from_root_config(&workspace.path().join("sigil.toml"), &config);

    let no_slash = app.execute_skill_slash_command(
        &crate::slash::ResolvedSlashCommand {
            canonical: "busy-skill".to_owned(),
            arg: String::new(),
        },
        "busy-skill",
    )?;
    assert!(no_slash.is_none());

    app.is_busy = true;
    let busy = app.execute_skill_slash_command(
        &crate::slash::ResolvedSlashCommand {
            canonical: "/busy-skill".to_owned(),
            arg: String::new(),
        },
        "/busy-skill",
    )?;
    assert!(busy.is_none());
    assert_eq!(app.last_notice(), Some("busy; use skill later"));
    app.is_busy = false;

    let disabled = app.execute_skill_slash_command(
        &crate::slash::ResolvedSlashCommand {
            canonical: "/disabled-skill".to_owned(),
            arg: String::new(),
        },
        "/disabled-skill",
    )?;
    assert!(disabled.is_none());
    assert_eq!(app.last_notice(), Some("skill disabled-skill is disabled"));

    let hidden = app.execute_skill_slash_command(
        &crate::slash::ResolvedSlashCommand {
            canonical: "/hidden-skill".to_owned(),
            arg: String::new(),
        },
        "/hidden-skill",
    )?;
    assert!(hidden.is_none());
    assert_eq!(
        app.last_notice(),
        Some("skill hidden-skill is not user-invocable")
    );
    Ok(())
}

#[test]
fn slash_skill_selector_edges_cover_setup_and_private_filters() -> Result<()> {
    let workspace = tempdir()?;
    write_workspace_skill(
        workspace.path(),
        "empty-description",
        r#"---
name: empty-description
description: ""
trust: trusted
user-invocable: true
---
"#,
    )?;
    let mut setup_app = AppState::from_setup(
        workspace.path().join("sigil.toml"),
        workspace.path().to_path_buf(),
        None,
    );
    setup_app.input = "/".to_owned();
    let _ = setup_app.slash_selector_rows();

    let config = config_for_workspace(workspace.path());
    let mut app = AppState::from_root_config(&workspace.path().join("sigil.toml"), &config);
    app.input = "/empty".to_owned();
    let rows = app.slash_selector_rows();
    assert!(rows.iter().any(|(label, description)| {
        label == "/empty-description" && description == "skill · inline"
    }));
    assert_eq!(
        app.selected_slash_entry()
            .expect("empty description skill should be selectable")
            .fill,
        "/empty-description"
    );
    assert!(app.slash_skill_entries("repo-review", "").is_empty());
    assert!(app.slash_skill_entries("", "").is_empty());
    Ok(())
}

#[test]
fn slash_command_hints_handles_leading_space() {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.input = " /compact".to_owned();
    let hints = app.slash_command_hints();
    assert!(hints.iter().any(|hint| hint.contains("/compact")));
}

#[test]
fn slash_command_entries_preserve_typed_argument_during_completion() {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.input = "/c now".to_owned();

    let rows = app.slash_selector_rows();

    assert!(rows.iter().any(|(label, _)| label == "/compact"));
    assert_eq!(
        app.selected_slash_entry()
            .expect("slash entry should resolve")
            .fill,
        "/compact now"
    );
}

#[test]
fn slash_command_input_starts_in_activity_mode() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.active_pane = PaneFocus::Activity;
    app.input.clear();

    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Char('/'), KeyModifiers::NONE))?;
    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::NONE))?;

    assert_eq!(app.active_pane, PaneFocus::Composer);
    assert_eq!(app.input, "/c".to_owned());
    assert!(
        app.slash_command_hints()
            .iter()
            .any(|hint| hint.contains("/compact"))
    );
    Ok(())
}

#[test]
fn ideographic_comma_starts_command_palette() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.input.clear();

    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Char('、'), KeyModifiers::NONE))?;
    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::NONE))?;

    assert_eq!(app.input, "/c");
    assert!(
        app.slash_command_hints()
            .iter()
            .any(|hint| hint.contains("/compact"))
    );
    Ok(())
}

#[test]
fn slash_selector_shows_all_commands_for_root_slash() {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.input = "/".to_owned();

    let rows = app.slash_selector_rows();

    assert_eq!(rows.len(), super::SLASH_COMMANDS.len());
    assert!(rows.iter().any(|(label, _)| label == "/doctor"));
    assert_eq!(app.slash_selector_selected_index(), Some(0));
}

#[test]
fn slash_selector_does_not_register_tool_commands() {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.input = "/".to_owned();

    let rows = app.slash_selector_rows();
    assert!(!rows.iter().any(|(label, _)| label == "/tool"));
    assert!(!rows.iter().any(|(label, _)| label == "/tools"));

    app.input = "/tools".to_owned();
    assert!(app.slash_selector_rows().is_empty());
    assert_eq!(app.slash_selector_empty_message(), Some("no slash match"));

    app.input = "/tool".to_owned();
    assert!(app.slash_selector_rows().is_empty());
    assert_eq!(app.slash_selector_empty_message(), Some("no slash match"));

    assert!(app.resolve_slash_command("/tool latest").is_none());
    assert!(app.resolve_slash_command("/tools full").is_none());
}

#[test]
fn slash_selector_navigation_and_tab_completion_work() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.input = "/".to_owned();

    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE))?;
    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE))?;

    assert_eq!(app.input, "/config".to_owned());
    assert_eq!(app.slash_selector_selected_index(), Some(0));
    Ok(())
}

#[test]
fn slash_selector_empty_navigation_and_visibility_edges_are_noops() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.input = "plain prompt".to_owned();
    assert_eq!(app.slash_selector_visible_rows(), 0);
    assert_eq!(app.slash_selector_empty_message(), None);

    app.input = "/unknown".to_owned();
    assert!(app.slash_selector_rows().is_empty());
    app.move_slash_selector(true);
    assert_eq!(app.slash_selector_selected_index(), None);
    assert!(app.handle_mouse_slash_candidate(0)?.is_none());
    app.accept_slash_selector();
    assert_eq!(app.input, "/unknown");
    assert_eq!(app.slash_command_hints(), vec!["no slash match".to_owned()]);

    app.input = "/".to_owned();
    let row_count = app.slash_selector_rows().len();
    assert!(row_count > 2);
    assert_eq!(app.slash_selector_empty_message(), None);
    app.move_slash_selector(false);
    assert_eq!(app.slash_selector_selected_index(), Some(row_count - 1));
    app.move_slash_selector(false);
    assert_eq!(app.slash_selector_selected_index(), Some(row_count - 2));
    Ok(())
}

#[test]
fn slash_selector_offers_model_candidates_and_completes_argument() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.input = "/model p".to_owned();

    let rows = app.slash_selector_rows();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].0, "pro");

    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE))?;
    assert_eq!(app.input, "/model deepseek-v4-pro");
    Ok(())
}

#[test]
fn slash_selector_includes_custom_current_model_when_query_matches() {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.model_name = "custom-current-model".to_owned();
    app.input = "/model custom".to_owned();

    let rows = app.slash_selector_rows();

    assert_eq!(rows.first().map(|row| row.0.as_str()), Some("current"));
    assert!(
        rows.first()
            .map(|row| row.1.contains("custom-current-model"))
            .unwrap_or(false)
    );
}

#[test]
fn mouse_selecting_slash_command_with_argument_selector_completes_entry() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.input = "/".to_owned();
    let model_index = app
        .slash_selector_rows()
        .iter()
        .position(|(label, _)| label == "/model")
        .expect("model command should be present");

    let action = app.handle_mouse_slash_candidate(model_index)?;

    assert!(action.is_none());
    assert_eq!(app.input, "/model ");
    assert_eq!(app.last_notice(), Some("slash completed to /model"));
    Ok(())
}

#[test]
fn slash_selector_executes_selected_model_candidate() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    let previous_session_id = app.session_id.clone();
    app.input = "/model p".to_owned();

    let action = app.submit_input()?;

    assert!(matches!(
        action,
        Some(AppAction::RuntimeConfigUpdated { .. })
    ));
    assert_eq!(app.model_name, "deepseek-v4-pro");
    assert_ne!(app.session_id, previous_session_id);
    Ok(())
}

#[test]
fn enter_on_root_slash_model_completes_into_second_stage_selector() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.input = "/".to_owned();

    select_root_slash_command(&mut app, "/model")?;
    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))?;

    assert_eq!(app.input, "/model ");
    let rows = app.slash_selector_rows();
    assert!(rows.iter().any(|(label, _)| label == "flash"));
    assert!(rows.iter().any(|(label, _)| label == "pro"));
    Ok(())
}

#[test]
fn enter_on_root_slash_effort_completes_into_second_stage_selector() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.input = "/".to_owned();

    select_root_slash_command(&mut app, "/effort")?;
    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))?;

    assert_eq!(app.input, "/effort ");
    let rows = app.slash_selector_rows();
    assert!(rows.iter().any(|(label, _)| label == "low"));
    assert!(rows.iter().any(|(label, _)| label == "max"));
    Ok(())
}

#[test]
fn model_command_is_noop_when_selected_model_is_already_active() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    let previous_session_id = app.session_id.clone();
    app.input = "/model".to_owned();

    let action = app.submit_input()?;

    assert!(action.is_none());
    assert_eq!(app.model_name, "deepseek-v4-flash");
    assert_eq!(app.session_id, previous_session_id);
    assert_eq!(
        app.last_notice(),
        Some("model already active = deepseek-v4-flash")
    );
    Ok(())
}

#[test]
fn slash_model_and_effort_invalid_or_busy_paths_show_usage_without_state_change() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    let original_model = app.model_name.clone();
    let original_session_id = app.session_id.clone();

    app.input = "/effort impossible".to_owned();
    assert!(app.submit_input()?.is_none());
    assert_eq!(app.reasoning_effort.as_str(), "max");
    assert_eq!(
        app.last_notice(),
        Some("usage: /effort <low|medium|high|max>")
    );

    app.input = "/model".to_owned();
    assert!(app.submit_input()?.is_none());
    assert_eq!(app.model_name, original_model);
    assert_eq!(app.session_id, original_session_id);

    app.is_busy = true;
    app.input = "/model pro".to_owned();
    assert!(app.submit_input()?.is_none());
    assert_eq!(app.model_name, original_model);
    assert_eq!(app.session_id, original_session_id);
    assert_eq!(app.last_notice(), Some("busy; model locked"));
    assert!(
        app.timeline
            .iter()
            .any(|entry| entry.role == TimelineRole::Notice
                && entry.text == "busy; switch model after the run")
    );
    Ok(())
}

#[test]
fn slash_selector_orders_effort_candidates_by_current_value() {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.reasoning_effort = ReasoningEffort::High;
    app.input = "/effort".to_owned();

    let rows = app.slash_selector_rows();

    assert_eq!(rows.first().map(|row| row.0.as_str()), Some("high"));
}

#[test]
fn slash_selector_executes_selected_effort_candidate() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.input = "/effort h".to_owned();

    assert!(app.submit_input()?.is_none());
    assert_eq!(app.reasoning_effort.as_str(), "high");
    Ok(())
}

#[test]
fn slash_selector_preserves_custom_model_ids() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.input = "/model ds-custom".to_owned();

    let rows = app.slash_selector_rows();
    assert_eq!(rows.first().map(|row| row.0.as_str()), Some("custom"));

    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE))?;
    assert_eq!(app.input, "/model ds-custom");
    Ok(())
}

#[test]
fn resume_selector_empty_message_distinguishes_no_match_from_no_sessions() {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.session_history = vec![crate::sessions::SessionHistoryEntry {
        path: Path::new(".sigil/sessions/alpha.jsonl").to_path_buf(),
        label: "alpha".to_owned(),
        title: Some("Alpha task".to_owned()),
        modified_epoch_secs: 1,
        bytes: 128,
    }];
    app.input = "/resume zzz".to_owned();

    assert!(app.slash_selector_rows().is_empty());
    assert_eq!(
        app.slash_selector_empty_message(),
        Some("no matching session")
    );
    assert_eq!(
        app.slash_command_hints(),
        vec!["no matching session".to_owned()]
    );
}

#[test]
fn slash_command_does_not_pollute_timeline_as_user_message() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.input = "/config".to_owned();

    let action = app.submit_input()?;

    assert!(action.is_none());
    assert!(
        !app.timeline
            .iter()
            .any(|entry| entry.role == TimelineRole::User && entry.text == "/config")
    );
    assert!(
        app.events
            .iter()
            .any(|event| event.label == "slash" && event.detail == "/config")
    );
    Ok(())
}

#[test]
fn submit_root_slash_executes_selected_command() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.input = "/".to_owned();

    let action = app.submit_input()?;

    assert!(matches!(action, Some(AppAction::CompactNow)));
    Ok(())
}

#[test]
fn unknown_slash_command_does_not_become_normal_prompt() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.input = "/unknown".to_owned();

    let action = app.submit_input()?;

    assert!(action.is_none());
    assert!(!app.is_busy);
    assert!(
        app.timeline
            .iter()
            .any(|entry| entry.text.contains("unknown slash command"))
    );
    Ok(())
}

#[test]
fn exit_alias_quits_tui() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.input = "/exit".to_owned();

    let action = app.submit_input()?;

    assert!(action.is_none());
    assert!(app.should_quit);
    assert!(
        app.timeline
            .iter()
            .any(|entry| entry.role == TimelineRole::Notice && entry.text == "quitting")
    );
    Ok(())
}

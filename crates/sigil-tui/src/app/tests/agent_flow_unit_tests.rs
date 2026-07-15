use std::{collections::BTreeMap, fs, path::Path};

use super::*;
use sigil_kernel::{
    AgentConfig, CompactionConfig, MemoryConfig, ModelMessage, PermissionConfig, RootConfig,
    SessionConfig, SkillConfig, WorkspaceConfig,
};
use tempfile::tempdir;

fn test_root_config() -> RootConfig {
    RootConfig {
        workspace: WorkspaceConfig {
            root: ".".to_owned(),
        },
        storage: Default::default(),
        session: SessionConfig {
            log_dir: Some(".sigil/sessions".to_owned()),
            retention: Default::default(),
        },
        agent: AgentConfig {
            provider: "deepseek".to_owned(),
            model: "deepseek-v4-flash".to_owned(),
            max_turns: None,
            tool_timeout_secs: 30,
        },
        model_request: Default::default(),
        permission: PermissionConfig::default(),
        memory: MemoryConfig { enabled: true },
        skills: SkillConfig {
            user_skills: false,
            user_agents: false,
            compatibility_sources: Vec::new(),
            ..Default::default()
        },
        compaction: CompactionConfig::default(),
        code_intelligence: Default::default(),
        terminal: Default::default(),
        execution: Default::default(),
        verification: Default::default(),
        appearance: Default::default(),
        task: Default::default(),
        providers: BTreeMap::new(),
        web: Default::default(),
        mcp_servers: Vec::new(),
    }
}

fn test_thread(
    thread_id: &str,
    objective: &str,
    profile_id: Option<&str>,
) -> anyhow::Result<AgentThreadProjection> {
    Ok(AgentThreadProjection {
        thread_id: AgentThreadId::new(thread_id)?,
        parent_thread_id: None,
        parent_session_ref: None,
        thread_session_ref: Some(sigil_kernel::SessionRef::new_relative(format!(
            "children/{thread_id}.jsonl"
        ))?),
        profile_id: profile_id
            .map(sigil_kernel::AgentProfileId::new)
            .transpose()?,
        profile_snapshot_id: None,
        run_context: None,
        objective: objective.to_owned(),
        prompt_hash: "sha256:prompt".to_owned(),
        invocation_mode: None,
        invocation_source: None,
        display_name: None,
        status: AgentThreadStatus::Started,
        reason: None,
        result: None,
        result_delivered: false,
        result_fully_delivered: false,
        result_delivered_chars: 0,
        result_delivery_call_ids: Vec::new(),
        attempts: BTreeMap::new(),
        merge_safe_points: Vec::new(),
        duplicate_terminal_entries: 0,
        closed: false,
        unresolved: false,
        profile_snapshot_missing: false,
        profile_snapshot_mismatch: false,
    })
}

#[test]
fn agent_thread_sidebar_item_uses_objective_profile_and_ordinal_fallbacks() -> anyhow::Result<()> {
    let objective = test_thread("thread_objective", "Review kernel", Some("reader"))?;
    let from_objective = agent_sidebar_item_from_thread(&objective, None, 1, false);
    assert_eq!(from_objective.label, "Review kernel");
    assert_eq!(
        from_objective.detail,
        "started · reader · unknown · result pending"
    );

    let profile = test_thread("thread_profile", "   ", Some("reader-agent"))?;
    let from_profile = agent_sidebar_item_from_thread(&profile, None, 2, false);
    assert_eq!(from_profile.label, "reader agent");
    assert_eq!(
        from_profile.detail,
        "started · reader-agent · unknown · result pending"
    );

    let ordinal = test_thread("thread_ordinal", "   ", None)?;
    let from_ordinal = agent_sidebar_item_from_thread(&ordinal, None, 3, false);
    assert_eq!(from_ordinal.label, "agent 3");
    assert_eq!(
        from_ordinal.detail,
        "started · agent · unknown · result pending"
    );
    Ok(())
}

#[test]
fn active_agent_view_terminal_uses_child_session_status() -> anyhow::Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_root_config());
    let task_id = sigil_kernel::TaskId::new("task_1")?;
    let step_id = sigil_kernel::TaskStepId::new("step_1")?;
    let child_ref = sigil_kernel::SessionRef::new_relative("children/child.jsonl")?;
    app.sync_current_session_state(vec![
        SessionLogEntry::Control(ControlEntry::TaskRun(sigil_kernel::TaskRunEntry {
            task_id: task_id.clone(),
            parent_session_ref: sigil_kernel::SessionRef::new_relative("parent.jsonl")?,
            objective: "review".to_owned(),
            status: sigil_kernel::TaskRunStatus::Running,
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
                child_session_ref: child_ref.clone(),
                role: sigil_kernel::AgentRole::SubagentRead,
                status: sigil_kernel::TaskChildSessionStatus::Failed,
                summary_hash: None,
            },
        )),
    ]);
    app.active_agent_view = AgentView::Child {
        child_task_id: "child_1".to_owned(),
        child_session_ref: child_ref,
    };

    assert!(app.active_agent_view_is_terminal());
    Ok(())
}

#[test]
fn recent_child_session_entries_cover_empty_valid_and_invalid_files() -> anyhow::Result<()> {
    let temp = tempdir()?;
    let missing = temp.path().join("missing.jsonl");
    let missing_signature = child_transcript_file_signature(&missing)?;
    let missing_recent = read_recent_session_entries(&missing, 16, missing_signature)?;
    assert!(missing_recent.entries.is_empty());
    assert!(!missing_recent.truncated);

    let valid = temp.path().join("valid.jsonl");
    let store = JsonlSessionStore::new(&valid)?;
    for index in 0..4 {
        store.append(&SessionLogEntry::User(ModelMessage::user(format!(
            "child prompt {index}"
        ))))?;
    }
    let valid_signature = child_transcript_file_signature(&valid)?;
    let recent = read_recent_session_entries(&valid, 2, valid_signature)?;
    assert_eq!(recent.entries.len(), 2);
    assert!(recent.truncated);

    let invalid_utf8 = temp.path().join("invalid-utf8.jsonl");
    fs::write(&invalid_utf8, [0xff, b'\n'])?;
    let error = read_recent_session_entries(
        &invalid_utf8,
        2,
        child_transcript_file_signature(&invalid_utf8)?,
    )
    .expect_err("invalid utf8 should fail");
    assert!(error.to_string().contains("decode recent entry"));

    let invalid_json = temp.path().join("invalid-json.jsonl");
    fs::write(&invalid_json, "not-json\n")?;
    let error = read_recent_session_entries(
        &invalid_json,
        2,
        child_transcript_file_signature(&invalid_json)?,
    )
    .expect_err("invalid json should fail");
    assert!(error.to_string().contains("parse recent session entry"));
    assert!(recent_session_entry_parse_error(&invalid_json).contains("invalid-json.jsonl"));
    Ok(())
}

#[test]
fn agent_thread_labels_cover_status_and_source_variants() -> anyhow::Result<()> {
    let mut thread = test_thread("thread_labels", "Review", Some("reader"))?;

    thread.invocation_source = Some(sigil_kernel::AgentInvocationSource::Chat);
    assert_eq!(agent_thread_source_label(&thread), "chat");
    thread.invocation_source = Some(sigil_kernel::AgentInvocationSource::Mention);
    assert_eq!(agent_thread_source_label(&thread), "mention");
    thread.invocation_source = Some(sigil_kernel::AgentInvocationSource::Skill);
    assert_eq!(agent_thread_source_label(&thread), "skill");
    thread.invocation_source = Some(sigil_kernel::AgentInvocationSource::Task);
    assert_eq!(agent_thread_source_label(&thread), "task");
    thread.invocation_source = Some(sigil_kernel::AgentInvocationSource::Plugin);
    assert_eq!(agent_thread_source_label(&thread), "plugin");
    thread.invocation_source = Some(sigil_kernel::AgentInvocationSource::System);
    assert_eq!(agent_thread_source_label(&thread), "system");
    thread.invocation_source = Some(sigil_kernel::AgentInvocationSource::Unknown);
    assert_eq!(agent_thread_source_label(&thread), "unknown");
    thread.invocation_source = None;
    assert_eq!(agent_thread_source_label(&thread), "unknown");

    assert_eq!(
        agent_thread_status_label(AgentThreadStatus::Started),
        "started"
    );
    assert_eq!(
        agent_thread_status_label(AgentThreadStatus::Running),
        "running"
    );
    assert_eq!(
        agent_thread_status_label(AgentThreadStatus::Blocked),
        "blocked"
    );
    assert_eq!(
        agent_thread_status_label(AgentThreadStatus::Completed),
        "completed"
    );
    assert_eq!(
        agent_thread_status_label(AgentThreadStatus::Failed),
        "failed"
    );
    assert_eq!(
        agent_thread_status_label(AgentThreadStatus::Cancelled),
        "cancelled"
    );
    assert_eq!(
        agent_thread_status_label(AgentThreadStatus::Interrupted),
        "interrupted"
    );
    assert_eq!(
        agent_thread_status_label(AgentThreadStatus::Closed),
        "closed"
    );
    assert_eq!(
        agent_thread_status_label(AgentThreadStatus::Unavailable),
        "unavailable"
    );
    assert_eq!(
        agent_thread_status_label(AgentThreadStatus::Unknown),
        "unknown"
    );
    Ok(())
}

use super::*;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

static TEST_STORAGE_COUNTER: AtomicU64 = AtomicU64::new(0);

pub(crate) fn test_config() -> RootConfig {
    let storage_id = TEST_STORAGE_COUNTER.fetch_add(1, Ordering::Relaxed);
    let storage_root = std::env::temp_dir().join(format!(
        "sigil-tui-test-storage-{}-{storage_id}",
        std::process::id()
    ));
    let skills = sigil_kernel::SkillConfig {
        user_skills: false,
        user_agents: false,
        compatibility_sources: Vec::new(),
        ..Default::default()
    };

    RootConfig {
        workspace: WorkspaceConfig {
            root: ".".to_owned(),
        },
        storage: sigil_kernel::StorageConfig {
            state_root: sigil_kernel::StorageRoot::Path(
                storage_root.join("state").display().to_string(),
            ),
            cache_root: sigil_kernel::StorageRoot::Path(
                storage_root.join("cache").display().to_string(),
            ),
            project_assets_root: ".sigil".to_owned(),
            ..Default::default()
        },
        session: SessionConfig::default(),
        agent: AgentConfig {
            provider: "deepseek".to_owned(),
            model: "deepseek-v4-flash".to_owned(),
            max_turns: None,
            tool_timeout_secs: 30,
        },
        model_request: Default::default(),
        permission: PermissionConfig::default(),
        memory: MemoryConfig { enabled: true },
        skills,
        compaction: CompactionConfig::default(),
        code_intelligence: Default::default(),
        terminal: Default::default(),
        execution: Default::default(),
        verification: Default::default(),
        appearance: Default::default(),
        task: Default::default(),
        providers: std::collections::BTreeMap::new(),
        mcp_servers: Vec::new(),
    }
}

pub(crate) fn resolved_session_log_dir(config: &RootConfig, workspace_root: &Path) -> PathBuf {
    sigil_runtime::resolve_sigil_paths(&config.storage, &config.session, workspace_root)
        .session_log_dir
}

pub(crate) fn restored_entries(provider_name: &str, model_name: &str) -> Vec<SessionLogEntry> {
    vec![
        SessionLogEntry::Control(ControlEntry::SessionIdentity {
            provider_name: provider_name.to_owned(),
            model_name: model_name.to_owned(),
        }),
        SessionLogEntry::User(ModelMessage::user("restored user prompt")),
        SessionLogEntry::ToolResult(ModelMessage::tool("call-1", "restored tool output")),
        SessionLogEntry::Assistant(ModelMessage::assistant(
            Some("restored assistant answer".to_owned()),
            Vec::new(),
        )),
    ]
}

pub(crate) fn select_root_slash_command(app: &mut AppState, command: &str) -> Result<()> {
    let index = app
        .slash_selector_rows()
        .iter()
        .position(|(label, _)| label == command)
        .ok_or_else(|| anyhow::anyhow!("slash command {command} not found"))?;
    for _ in 0..index {
        let _ = app.handle_key_event(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE))?;
    }
    Ok(())
}

pub(crate) fn write_session_log(path: &Path, entries: &[SessionLogEntry]) -> Result<()> {
    let store = JsonlSessionStore::new(path)?;
    for entry in entries {
        store.append(entry)?;
    }
    Ok(())
}

pub(crate) fn child_agent_entries(
    display_name: Option<&str>,
    thread_status: sigil_kernel::AgentThreadStatus,
    child_session_ref: sigil_kernel::SessionRef,
) -> Result<Vec<SessionLogEntry>> {
    child_agent_entries_with(
        "review workspace",
        "Inspect repository",
        display_name,
        "step_1",
        "child_1",
        child_session_ref,
        "subagent_read",
        thread_status,
    )
}

pub(crate) fn child_agent_entries_with(
    objective: &str,
    step_title: &str,
    display_name: Option<&str>,
    step_id: &str,
    child_id: &str,
    child_session_ref: sigil_kernel::SessionRef,
    profile_id: &str,
    thread_status: sigil_kernel::AgentThreadStatus,
) -> Result<Vec<SessionLogEntry>> {
    let task_id = sigil_kernel::TaskId::new("task_1")?;
    let step_id = sigil_kernel::TaskStepId::new(step_id)?;
    let child_task_id = sigil_kernel::TaskId::new(child_id)?;
    let thread_id = sigil_kernel::AgentThreadId::new(child_id)?;
    let profile_id = sigil_kernel::AgentProfileId::new(profile_id)?;
    let snapshot_id = sigil_kernel::AgentProfileSnapshotId::new(format!("snapshot_{}", child_id))?;
    let task_child_status = match thread_status {
        sigil_kernel::AgentThreadStatus::Completed => {
            sigil_kernel::TaskChildSessionStatus::Completed
        }
        sigil_kernel::AgentThreadStatus::Failed => sigil_kernel::TaskChildSessionStatus::Failed,
        sigil_kernel::AgentThreadStatus::Cancelled => {
            sigil_kernel::TaskChildSessionStatus::Cancelled
        }
        sigil_kernel::AgentThreadStatus::Interrupted => {
            sigil_kernel::TaskChildSessionStatus::Interrupted
        }
        sigil_kernel::AgentThreadStatus::Unavailable => {
            sigil_kernel::TaskChildSessionStatus::Unavailable
        }
        _ => sigil_kernel::TaskChildSessionStatus::Started,
    };

    Ok(vec![
        SessionLogEntry::Control(ControlEntry::TaskRun(sigil_kernel::TaskRunEntry {
            task_id: task_id.clone(),
            parent_session_ref: sigil_kernel::SessionRef::new_relative("parent.jsonl")?,
            objective: objective.to_owned(),
            status: sigil_kernel::TaskRunStatus::Running,
            reason: None,
        })),
        SessionLogEntry::Control(ControlEntry::TaskPlan(sigil_kernel::TaskPlanEntry {
            task_id: task_id.clone(),
            plan_version: 1,
            status: sigil_kernel::TaskPlanStatus::Accepted,
            steps: vec![sigil_kernel::TaskStepSpec {
                step_id: step_id.clone(),
                title: step_title.to_owned(),
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
                child_task_id,
                child_session_ref: child_session_ref.clone(),
                role: sigil_kernel::AgentRole::SubagentRead,
                status: task_child_status,
                summary_hash: None,
            },
        )),
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
                thread_session_ref: child_session_ref,
                profile_id,
                profile_snapshot_id: snapshot_id.clone(),
                run_context: sigil_kernel::AgentRunContextSnapshot {
                    profile_snapshot_id: snapshot_id,
                    provider: "deepseek".to_owned(),
                    model: "deepseek-v4-pro".to_owned(),
                    reasoning_effort: None,
                    workspace_root: sigil_kernel::WorkspaceRootSnapshot::new("/tmp/workspace")?,
                    effective_tool_scope_hash: "sha256:tools".to_owned(),
                    effective_permission_policy_hash: "sha256:permissions".to_owned(),
                    effective_mcp_scope_hash: "sha256:mcp".to_owned(),
                    provider_capability_hash: "sha256:provider".to_owned(),
                    model_visible_agent_index_hash: Some("sha256:index".to_owned()),
                    budget_policy_hash: "sha256:budget".to_owned(),
                    provider_background_handle_ref: None,
                },
                objective: objective.to_owned(),
                prompt_hash: "sha256:prompt".to_owned(),
                invocation_mode: sigil_kernel::AgentInvocationMode::Background,
                invocation_source: sigil_kernel::AgentInvocationSource::Task,
                display_name: display_name.map(ToOwned::to_owned),
                created_at_ms: Some(42),
            },
        )),
        SessionLogEntry::Control(ControlEntry::AgentThreadStatusChanged(
            sigil_kernel::AgentThreadStatusChangedEntry {
                thread_id,
                status: thread_status,
                reason: None,
                updated_at_ms: None,
            },
        )),
    ])
}

pub(crate) fn sample_approval_preview() -> ToolPreview {
    ToolPreview {
        title: "Update note.txt".to_owned(),
        summary: "Preview summary".to_owned(),
        body: "--- current/note.txt\n+++ proposed/note.txt\n@@ -1,2 +1,2 @@\n alpha\n-beta\n+gamma".to_owned(),
        changed_files: vec!["note.txt".to_owned()],
        file_diffs: vec![sigil_kernel::ToolPreviewFile {
            path: "note.txt".to_owned(),
            diff: "--- current/note.txt\n+++ proposed/note.txt\n@@ -1,2 +1,2 @@\n alpha\n-beta\n+gamma".to_owned(),
        }],
    }
}

pub(crate) fn sample_delete_approval_preview() -> ToolPreview {
    ToolPreview {
        title: "Delete note.txt".to_owned(),
        summary: "Delete 2 lines from note.txt".to_owned(),
        body: "--- current/note.txt\n+++ proposed/note.txt\n@@ -1,2 +0,0 @@\n-alpha\n-beta"
            .to_owned(),
        changed_files: vec!["note.txt".to_owned()],
        file_diffs: vec![sigil_kernel::ToolPreviewFile {
            path: "note.txt".to_owned(),
            diff: "--- current/note.txt\n+++ proposed/note.txt\n@@ -1,2 +0,0 @@\n-alpha\n-beta"
                .to_owned(),
        }],
    }
}

pub(crate) fn multi_file_approval_preview() -> ToolPreview {
    ToolPreview {
        title: "Update multiple files".to_owned(),
        summary: "Multi-file preview".to_owned(),
        body: String::new(),
        changed_files: vec!["note-a.txt".to_owned(), "note-b.txt".to_owned()],
        file_diffs: vec![
            sigil_kernel::ToolPreviewFile {
                path: "note-a.txt".to_owned(),
                diff: "--- current/note-a.txt\n+++ proposed/note-a.txt\n@@ -1,2 +1,2 @@\n alpha\n-beta\n+gamma\n@@ -5,2 +5,2 @@\n delta\n-epsilon\n+zeta".to_owned(),
            },
            sigil_kernel::ToolPreviewFile {
                path: "note-b.txt".to_owned(),
                diff: "--- current/note-b.txt\n+++ proposed/note-b.txt\n@@ -1,1 +1,1 @@\n-old\n+new".to_owned(),
            },
        ],
    }
}

pub(crate) fn inject_write_file_approval(app: &mut AppState, preview: ToolPreview) -> Result<()> {
    app.handle(RunEvent::ToolApprovalRequested {
        call: ToolCall {
            id: "call-1".to_owned(),
            name: "write_file".to_owned(),
            args_json: r#"{"path":"note.txt","content":"hello"}"#.to_owned(),
        },
        spec: ToolSpec {
            name: "write_file".to_owned(),
            description: "Write a file".to_owned(),
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
        preview: Some(preview),
    })
}

use super::*;

pub(crate) fn test_config() -> RootConfig {
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
        session: SessionConfig {
            log_dir: ".sigil/sessions".to_owned(),
        },
        agent: AgentConfig {
            provider: "deepseek".to_owned(),
            model: "deepseek-v4-flash".to_owned(),
            max_turns: None,
            tool_timeout_secs: 30,
        },
        permission: PermissionConfig::default(),
        memory: MemoryConfig { enabled: true },
        skills,
        compaction: CompactionConfig::default(),
        code_intelligence: Default::default(),
        terminal: Default::default(),
        task: Default::default(),
        providers: std::collections::BTreeMap::new(),
        mcp_servers: Vec::new(),
    }
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
        preview: Some(preview),
    })
}

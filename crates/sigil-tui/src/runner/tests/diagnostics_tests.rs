use std::{fs, process::Command};

use anyhow::{Result, anyhow};
use sigil_kernel::{
    Agent, ControlEntry, JsonlSessionStore, LanguageServerConfig, Provider, RunEvent,
    SessionLogEntry, ToolExecutionStatus,
};
use tempfile::tempdir;

use super::{
    super::{WorkerCommand, WorkerMessage},
    common::{PlannedProvider, spawn_test_worker, test_root_config},
};

#[test]
fn check_changed_files_runs_real_code_diagnostics_and_audits_control_state() -> Result<()> {
    let temp = tempdir()?;
    let workspace_root = temp.path().to_path_buf();
    init_git_repo(&workspace_root)?;
    fs::write(workspace_root.join("broken.rs"), "fn broken( {\n")?;

    let session_log_path = workspace_root.join(".sigil/sessions/session-diagnostics.jsonl");
    let mut root_config = test_root_config(&workspace_root, "planned", "planned-model");
    root_config.code_intelligence.enabled = true;
    root_config.code_intelligence.discovery.enabled = false;
    root_config.code_intelligence.servers = vec![LanguageServerConfig {
        name: "missing-rust-analyzer".to_owned(),
        languages: vec!["rust".to_owned()],
        command: "definitely-missing-sigil-lsp".to_owned(),
        args: Vec::new(),
        env: Default::default(),
        root_markers: vec!["Cargo.toml".to_owned(), "rust-project.json".to_owned()],
        file_extensions: vec!["rs".to_owned()],
        initialization_options: serde_json::Value::Null,
        trust_required: true,
        startup_timeout_ms: 50,
    }];
    let provider = PlannedProvider::new(vec![]);
    let capabilities = provider.capabilities();
    let registry = test_runtime()?.block_on(sigil_runtime::build_tool_registry(
        &root_config,
        &capabilities,
        workspace_root.clone(),
    ))?;
    let agent = Agent::new(provider, registry);
    let worker = spawn_test_worker(root_config, session_log_path.clone(), agent, workspace_root)?;

    worker.send(WorkerCommand::CheckChangedFilesDiagnostics)?;
    let message = worker.recv_until(|message| {
        matches!(
            message,
            WorkerMessage::Event(event)
                if matches!(event.as_ref(), RunEvent::ToolResult(result) if result.tool_name == "code_diagnostics")
        )
    })?;
    let WorkerMessage::Event(event) = message else {
        return Err(anyhow!("expected diagnostics tool event"));
    };
    let RunEvent::ToolResult(result) = event.as_ref() else {
        return Err(anyhow!("expected diagnostics tool result"));
    };
    assert!(!result.is_error());
    let content: serde_json::Value = serde_json::from_str(&result.content)?;
    assert_eq!(content["tool"], "code_diagnostics");
    assert!(
        content["diagnostics"]
            .as_array()
            .is_some_and(|diagnostics| diagnostics.iter().any(|diagnostic| {
                diagnostic["path"] == "broken.rs" && diagnostic["severity"] == "error"
            }))
    );

    let entries = JsonlSessionStore::read_entries(&session_log_path)?;
    assert!(entries.iter().any(|entry| matches!(
        entry,
        SessionLogEntry::Control(ControlEntry::ToolApproval(approval))
            if approval.tool_name == "code_diagnostics" && approval.reason.is_none()
    )));
    assert!(entries.iter().any(|entry| matches!(
        entry,
        SessionLogEntry::Control(ControlEntry::ToolExecution(execution))
            if execution.tool_name == "code_diagnostics"
                && execution.status == ToolExecutionStatus::Started
    )));
    assert!(entries.iter().any(|entry| matches!(
        entry,
        SessionLogEntry::Control(ControlEntry::ToolExecution(execution))
            if execution.tool_name == "code_diagnostics"
                && execution.status == ToolExecutionStatus::Completed
                && execution.model_content_hash.is_none()
    )));
    assert!(
        !entries
            .iter()
            .any(|entry| matches!(entry, SessionLogEntry::ToolResult(_)))
    );

    worker.shutdown()?;
    Ok(())
}

#[test]
fn check_changed_files_reports_notice_when_no_source_files_changed() -> Result<()> {
    let temp = tempdir()?;
    let workspace_root = temp.path().to_path_buf();
    init_git_repo(&workspace_root)?;
    let session_log_path = workspace_root.join(".sigil/sessions/session-no-changes.jsonl");
    let mut root_config = test_root_config(&workspace_root, "planned", "planned-model");
    root_config.code_intelligence.enabled = true;
    let provider = PlannedProvider::new(vec![]);
    let agent = Agent::new(provider, sigil_kernel::ToolRegistry::new());
    let worker = spawn_test_worker(root_config, session_log_path, agent, workspace_root)?;

    worker.send(WorkerCommand::CheckChangedFilesDiagnostics)?;
    let notice = worker.recv_until(|message| matches!(message, WorkerMessage::Notice(_)))?;

    assert!(matches!(
        notice,
        WorkerMessage::Notice(ref text) if text == "no changed source files to check"
    ));
    worker.shutdown()?;
    Ok(())
}

fn init_git_repo(workspace_root: &std::path::Path) -> Result<()> {
    let status = Command::new("git")
        .arg("-C")
        .arg(workspace_root)
        .args(["init", "-q"])
        .status()?;
    if status.success() {
        Ok(())
    } else {
        Err(anyhow!(
            "failed to initialize git repo under {}",
            workspace_root.display()
        ))
    }
}

fn test_runtime() -> Result<tokio::runtime::Runtime> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| anyhow!("failed to build test runtime: {error}"))
}

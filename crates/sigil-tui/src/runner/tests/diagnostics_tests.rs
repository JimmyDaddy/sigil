use std::{collections::BTreeSet, fs, sync::Arc, time::Instant};

use anyhow::{Result, anyhow};
use async_trait::async_trait;
use sigil_kernel::{
    Agent, ApprovalMode, ControlEntry, JsonlSessionStore, LanguageServerConfig,
    PermissionAccessConfig, PermissionConfig, PermissionDecision, Provider, RunEvent, Session,
    SessionLogEntry, Tool, ToolAccess, ToolCall, ToolCategory, ToolContext, ToolErrorKind,
    ToolExecutionStatus, ToolPreviewCapability, ToolRegistry, ToolResult, ToolResultMeta,
    ToolResultStatus, ToolSpec, ToolSubject, ToolSubjectScope,
};
use tempfile::tempdir;

use super::{
    super::{
        WorkerCommand, WorkerMessage,
        diagnostics::{
            attach_diagnostics_context, changed_source_files, check_changed_files_diagnostics,
            collect_nul_paths, diagnostics_paths_from_call, diagnostics_tool_event, duration_ms,
            ensure_git_workspace, git_command, git_output, has_head, is_supported_source_file,
            permission_block_reason,
        },
    },
    common::{PlannedProvider, StreamPlan, spawn_test_worker, test_root_config},
};

#[derive(Clone)]
enum ExecutePlan {
    Ok(Box<ToolResult>),
    Err(&'static str),
}

#[derive(Clone)]
struct DiagnosticsTestTool {
    access: ToolAccess,
    subjects: std::result::Result<Vec<ToolSubject>, &'static str>,
    dynamic_access: std::result::Result<ToolAccess, &'static str>,
    execute_plan: ExecutePlan,
}

impl DiagnosticsTestTool {
    fn new(access: ToolAccess, execute_plan: ExecutePlan) -> Self {
        Self {
            access,
            subjects: Ok(Vec::new()),
            dynamic_access: Ok(access),
            execute_plan,
        }
    }

    fn with_subjects_error(mut self, error: &'static str) -> Self {
        self.subjects = Err(error);
        self
    }

    fn with_access_error(mut self, error: &'static str) -> Self {
        self.dynamic_access = Err(error);
        self
    }

    fn with_subjects(mut self, subjects: Vec<ToolSubject>) -> Self {
        self.subjects = Ok(subjects);
        self
    }
}

#[async_trait]
impl Tool for DiagnosticsTestTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "code_diagnostics".to_owned(),
            description: "test diagnostics".to_owned(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "paths": { "type": "array", "items": { "type": "string" } },
                    "max_results": { "type": "integer" }
                },
                "required": ["paths", "max_results"]
            }),
            category: ToolCategory::Custom,
            access: self.access,
            preview: ToolPreviewCapability::None,
        }
    }

    fn permission_subjects(
        &self,
        _ctx: &ToolContext,
        _args: &serde_json::Value,
    ) -> anyhow::Result<Vec<ToolSubject>> {
        self.subjects
            .clone()
            .map_err(|error| anyhow!(error.to_owned()))
    }

    fn permission_access(
        &self,
        _ctx: &ToolContext,
        _args: &serde_json::Value,
    ) -> anyhow::Result<ToolAccess> {
        self.dynamic_access
            .map_err(|error| anyhow!(error.to_owned()))
    }

    async fn execute(
        &self,
        _ctx: ToolContext,
        _call_id: String,
        _args: serde_json::Value,
    ) -> anyhow::Result<ToolResult> {
        match self.execute_plan.clone() {
            ExecutePlan::Ok(result) => Ok(*result),
            ExecutePlan::Err(error) => Err(anyhow!(error.to_owned())),
        }
    }
}

fn test_runtime() -> Result<tokio::runtime::Runtime> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| anyhow!("failed to build test runtime: {error}"))
}

fn test_options(
    workspace_root: &std::path::Path,
    default_mode: ApprovalMode,
) -> sigil_kernel::AgentRunOptions {
    let mut root_config = test_root_config(workspace_root, "planned", "planned-model");
    root_config.permission.default_mode = default_mode;
    sigil_runtime::build_run_options(
        &root_config,
        workspace_root.to_path_buf(),
        sigil_kernel::InteractionMode::Interactive,
    )
}

fn tool_error_kind(result: &ToolResult) -> ToolErrorKind {
    match &result.status {
        ToolResultStatus::Error(error) => error.kind,
        ToolResultStatus::Ok => panic!("expected tool error"),
    }
}

fn session_with_directory_store(workspace_root: &std::path::Path, name: &str) -> Result<Session> {
    let store_path = workspace_root.join(name);
    fs::create_dir_all(&store_path)?;
    let store = JsonlSessionStore::new(store_path)?;
    Ok(Session::new("planned", "planned-model").with_store(store))
}

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

#[test]
fn check_changed_files_reports_failure_outside_git_workspace() -> Result<()> {
    let temp = tempdir()?;
    let workspace_root = temp.path().to_path_buf();
    let session_log_path = workspace_root.join(".sigil/sessions/session-no-git.jsonl");
    let mut root_config = test_root_config(&workspace_root, "planned", "planned-model");
    root_config.code_intelligence.enabled = true;
    let provider = PlannedProvider::new(vec![]);
    let agent = Agent::new(provider, sigil_kernel::ToolRegistry::new());
    let worker = spawn_test_worker(root_config, session_log_path, agent, workspace_root)?;

    worker.send(WorkerCommand::CheckChangedFilesDiagnostics)?;
    let failure = worker.recv_until(|message| matches!(message, WorkerMessage::RunFailed(_)))?;

    assert!(matches!(
        failure,
        WorkerMessage::RunFailed(ref error) if error.contains("is not inside a git repository")
    ));

    worker.shutdown()?;
    Ok(())
}

#[test]
fn check_changed_files_is_rejected_while_run_is_active() -> Result<()> {
    let temp = tempdir()?;
    let workspace_root = temp.path().to_path_buf();
    let session_log_path = temp
        .path()
        .join(".sigil/sessions/session-diagnostics-busy.jsonl");
    let root_config = test_root_config(&workspace_root, "planned", "planned-model");
    let provider = PlannedProvider::new(vec![StreamPlan::Pending]);
    let agent = Agent::new(provider, sigil_kernel::ToolRegistry::new());
    let worker = spawn_test_worker(root_config, session_log_path, agent, workspace_root)?;

    worker.send(WorkerCommand::SubmitPrompt {
        prompt: "hold".to_owned(),
        reasoning_effort: sigil_kernel::ReasoningEffort::Max,
    })?;
    let _ = worker.recv_until(|message| matches!(message, WorkerMessage::RunStarted { .. }))?;

    worker.send(WorkerCommand::CheckChangedFilesDiagnostics)?;
    let failure = worker.recv_until(|message| matches!(message, WorkerMessage::RunFailed(_)))?;

    assert!(matches!(
        failure,
        WorkerMessage::RunFailed(ref error)
            if error == "cannot check changes while the agent is running"
    ));

    worker.shutdown()?;
    Ok(())
}

#[test]
fn changed_source_files_requires_git_workspace() {
    let temp = tempdir().expect("tempdir should build");
    let error = changed_source_files(temp.path()).expect_err("expected git workspace failure");

    assert!(error.to_string().contains("is not inside a git repository"));
}

#[test]
fn git_workspace_helpers_track_initialized_and_committed_states() -> Result<()> {
    let temp = tempdir()?;
    let workspace_root = temp.path().to_path_buf();

    assert!(!has_head(&workspace_root));
    init_git_repo(&workspace_root)?;
    ensure_git_workspace(&workspace_root)?;
    assert!(!has_head(&workspace_root));

    fs::write(workspace_root.join("tracked.rs"), "fn main() {}\n")?;
    run_git(&workspace_root, &["add", "tracked.rs"])?;
    run_git(&workspace_root, &["commit", "-qm", "initial"])?;

    assert!(has_head(&workspace_root));
    Ok(())
}

#[test]
fn changed_source_files_uses_cached_and_untracked_files_without_head() -> Result<()> {
    let temp = tempdir()?;
    let workspace_root = temp.path().to_path_buf();
    init_git_repo(&workspace_root)?;
    fs::write(workspace_root.join("tracked.rs"), "fn tracked() {}\n")?;
    fs::write(workspace_root.join("untracked.ts"), "export const x = 1;\n")?;
    fs::write(workspace_root.join("ignored.txt"), "ignore me\n")?;
    let add_status = git_command(&workspace_root)
        .args(["add", "tracked.rs"])
        .status()?;
    assert!(add_status.success());

    let paths = changed_source_files(&workspace_root)?;

    assert_eq!(
        paths,
        vec!["tracked.rs".to_owned(), "untracked.ts".to_owned()]
    );
    Ok(())
}

#[test]
fn collect_nul_paths_discards_empty_segments_and_trims_values() {
    let mut paths = BTreeSet::new();

    collect_nul_paths(&mut paths, b"alpha.rs\0 beta.ts \0\0".to_vec());

    assert_eq!(
        paths.into_iter().collect::<Vec<_>>(),
        vec!["alpha.rs".to_owned(), "beta.ts".to_owned()]
    );
}

#[test]
fn supported_source_file_checks_extension_and_real_file() -> Result<()> {
    let temp = tempdir()?;
    fs::write(temp.path().join("ok.rs"), "fn ok() {}\n")?;
    fs::write(
        temp.path().join("component.TSX"),
        "export const X = () => null;\n",
    )?;
    fs::write(temp.path().join("script.py"), "print('ok')\n")?;
    fs::write(temp.path().join("skip.txt"), "nope\n")?;

    assert!(is_supported_source_file(temp.path(), "ok.rs"));
    assert!(is_supported_source_file(temp.path(), "component.TSX"));
    assert!(is_supported_source_file(temp.path(), "script.py"));
    assert!(!is_supported_source_file(temp.path(), "skip.txt"));
    assert!(!is_supported_source_file(temp.path(), "missing.rs"));
    Ok(())
}

#[test]
fn diagnostics_paths_from_call_rejects_invalid_payloads() {
    let invalid_json = diagnostics_paths_from_call(&ToolCall {
        id: "call-1".to_owned(),
        name: "code_diagnostics".to_owned(),
        args_json: "{".to_owned(),
    })
    .expect_err("expected invalid json error");
    assert!(invalid_json.to_string().contains("invalid tool args"));

    let missing_paths = diagnostics_paths_from_call(&ToolCall {
        id: "call-1".to_owned(),
        name: "code_diagnostics".to_owned(),
        args_json: serde_json::json!({"max_results": 4}).to_string(),
    })
    .expect_err("expected missing paths error");
    assert!(
        missing_paths
            .to_string()
            .contains("missing diagnostics paths")
    );

    let mixed_paths = diagnostics_paths_from_call(&ToolCall {
        id: "call-1".to_owned(),
        name: "code_diagnostics".to_owned(),
        args_json: serde_json::json!({
            "paths": ["src/main.rs", 42, null, "src/lib.rs"],
            "max_results": 4
        })
        .to_string(),
    })
    .expect("mixed path payload should keep string paths");
    assert_eq!(
        mixed_paths,
        vec!["src/main.rs".to_owned(), "src/lib.rs".to_owned()]
    );
}

#[test]
fn permission_block_reason_covers_approval_deny_and_external_directory() {
    let call = ToolCall {
        id: "call-1".to_owned(),
        name: "code_diagnostics".to_owned(),
        args_json: "{}".to_owned(),
    };

    let ask = PermissionDecision {
        mode: ApprovalMode::Ask,
        access: ToolAccess::Execute,
        subjects: Vec::new(),
        external_directory_required: false,
    };
    let deny = PermissionDecision {
        mode: ApprovalMode::Deny,
        access: ToolAccess::Execute,
        subjects: vec![ToolSubject::path("src/main.rs", "src/main.rs")],
        external_directory_required: false,
    };
    let external = PermissionDecision {
        mode: ApprovalMode::Deny,
        access: ToolAccess::Execute,
        subjects: vec![ToolSubject::path_with_scope(
            "/tmp/external.rs",
            "/tmp/external.rs",
            None,
            ToolSubjectScope::External,
        )],
        external_directory_required: true,
    };

    let (ask_kind, ask_reason) = permission_block_reason(&call, &ask);
    let (deny_kind, deny_reason) = permission_block_reason(&call, &deny);
    let (external_kind, external_reason) = permission_block_reason(&call, &external);

    assert_eq!(ask_kind, ToolErrorKind::ApprovalRequired);
    assert!(ask_reason.contains("requires approval"));
    assert_eq!(deny_kind, ToolErrorKind::PermissionDenied);
    assert!(deny_reason.contains("src/main.rs"));
    assert_eq!(external_kind, ToolErrorKind::ExternalDirectoryRequired);
    assert!(external_reason.contains("permission.external_directory.enabled"));
    assert!(external_reason.contains(".sigil/tmp"));
}

#[test]
fn attach_diagnostics_context_merges_with_existing_metadata_shapes() {
    let mut null_details = ToolResult::ok(
        "call-1",
        "code_diagnostics",
        "ok",
        ToolResultMeta::default(),
    );
    attach_diagnostics_context(&mut null_details, &[String::from("a.rs")]);
    assert_eq!(
        null_details.metadata.details["call"]["path_count"],
        serde_json::json!(1)
    );

    let mut object_details = ToolResult::ok(
        "call-2",
        "code_diagnostics",
        "ok",
        ToolResultMeta {
            details: serde_json::json!({"existing": true}),
            ..ToolResultMeta::default()
        },
    );
    attach_diagnostics_context(
        &mut object_details,
        &[String::from("a.rs"), String::from("b.rs")],
    );
    assert_eq!(
        object_details.metadata.details["existing"],
        serde_json::json!(true)
    );
    assert_eq!(
        object_details.metadata.details["call"]["path_count"],
        serde_json::json!(2)
    );

    let mut scalar_details = ToolResult::ok(
        "call-3",
        "code_diagnostics",
        "ok",
        ToolResultMeta {
            details: serde_json::json!("tool-details"),
            ..ToolResultMeta::default()
        },
    );
    attach_diagnostics_context(&mut scalar_details, &[String::from("single.rs")]);
    assert_eq!(
        scalar_details.metadata.details["tool"],
        serde_json::json!("tool-details")
    );
}

#[test]
fn duration_ms_never_underflows() {
    let elapsed = duration_ms(Instant::now());

    assert!(elapsed < 1_000);
}

#[test]
fn check_changed_files_diagnostics_errors_when_tool_is_not_registered() -> Result<()> {
    let temp = tempdir()?;
    let runtime = test_runtime()?;
    let mut session = Session::new("planned", "planned-model");
    let options = test_options(temp.path(), ApprovalMode::Allow);

    let result = check_changed_files_diagnostics(
        &runtime,
        &ToolRegistry::new(),
        &mut session,
        &options,
        4,
        vec!["src/main.rs".to_owned()],
    )?;

    assert_eq!(tool_error_kind(&result), ToolErrorKind::Unsupported);
    assert_eq!(
        result.metadata.details["call"]["summary"],
        serde_json::json!("paths=diagnostics")
    );
    assert!(session.entries().iter().any(|entry| matches!(
        entry,
        SessionLogEntry::Control(ControlEntry::ToolExecution(execution))
            if execution.status == ToolExecutionStatus::Failed
    )));
    Ok(())
}

#[test]
fn check_changed_files_diagnostics_propagates_missing_tool_audit_write_errors() -> Result<()> {
    let temp = tempdir()?;
    let runtime = test_runtime()?;
    let mut session = session_with_directory_store(temp.path(), "session-dir")?;
    let options = test_options(temp.path(), ApprovalMode::Allow);

    let error = check_changed_files_diagnostics(
        &runtime,
        &ToolRegistry::new(),
        &mut session,
        &options,
        4,
        vec!["src/main.rs".to_owned()],
    )
    .expect_err("directory store should make audit append fail");

    assert!(error.to_string().contains("failed to open"));
    Ok(())
}

#[test]
fn check_changed_files_diagnostics_surfaces_subject_resolution_errors() -> Result<()> {
    let temp = tempdir()?;
    let runtime = test_runtime()?;
    let mut session = Session::new("planned", "planned-model");
    let options = test_options(temp.path(), ApprovalMode::Allow);
    let mut registry = ToolRegistry::new();
    registry.register(Arc::new(
        DiagnosticsTestTool::new(
            ToolAccess::Execute,
            ExecutePlan::Ok(Box::new(ToolResult::ok(
                "call-1",
                "code_diagnostics",
                "ok",
                ToolResultMeta::default(),
            ))),
        )
        .with_subjects_error("bad paths"),
    ));

    let result = check_changed_files_diagnostics(
        &runtime,
        &registry,
        &mut session,
        &options,
        4,
        vec!["src/main.rs".to_owned()],
    )?;

    assert_eq!(tool_error_kind(&result), ToolErrorKind::InvalidInput);
    assert!(
        result
            .content
            .contains("invalid code diagnostics arguments: bad paths")
    );
    Ok(())
}

#[test]
fn check_changed_files_diagnostics_propagates_subject_audit_write_errors() -> Result<()> {
    let temp = tempdir()?;
    let runtime = test_runtime()?;
    let mut session = session_with_directory_store(temp.path(), "subject-session-dir")?;
    let options = test_options(temp.path(), ApprovalMode::Allow);
    let mut registry = ToolRegistry::new();
    registry.register(Arc::new(
        DiagnosticsTestTool::new(
            ToolAccess::Execute,
            ExecutePlan::Ok(Box::new(ToolResult::ok(
                "call-1",
                "code_diagnostics",
                "ok",
                ToolResultMeta::default(),
            ))),
        )
        .with_subjects_error("bad paths"),
    ));

    let error = check_changed_files_diagnostics(
        &runtime,
        &registry,
        &mut session,
        &options,
        4,
        vec!["src/main.rs".to_owned()],
    )
    .expect_err("directory store should make subject failure audit append fail");

    assert!(error.to_string().contains("failed to open"));
    Ok(())
}

#[test]
fn check_changed_files_diagnostics_surfaces_access_resolution_errors() -> Result<()> {
    let temp = tempdir()?;
    let runtime = test_runtime()?;
    let mut session = Session::new("planned", "planned-model");
    let options = test_options(temp.path(), ApprovalMode::Allow);
    let mut registry = ToolRegistry::new();
    registry.register(Arc::new(
        DiagnosticsTestTool::new(
            ToolAccess::Execute,
            ExecutePlan::Ok(Box::new(ToolResult::ok(
                "call-1",
                "code_diagnostics",
                "ok",
                ToolResultMeta::default(),
            ))),
        )
        .with_access_error("bad access"),
    ));

    let result = check_changed_files_diagnostics(
        &runtime,
        &registry,
        &mut session,
        &options,
        4,
        vec!["src/main.rs".to_owned()],
    )?;

    assert_eq!(tool_error_kind(&result), ToolErrorKind::InvalidInput);
    assert!(
        result
            .content
            .contains("invalid code diagnostics arguments: bad access")
    );
    Ok(())
}

#[test]
fn check_changed_files_diagnostics_propagates_access_audit_write_errors() -> Result<()> {
    let temp = tempdir()?;
    let runtime = test_runtime()?;
    let mut session = session_with_directory_store(temp.path(), "access-session-dir")?;
    let options = test_options(temp.path(), ApprovalMode::Allow);
    let mut registry = ToolRegistry::new();
    registry.register(Arc::new(
        DiagnosticsTestTool::new(
            ToolAccess::Execute,
            ExecutePlan::Ok(Box::new(ToolResult::ok(
                "call-1",
                "code_diagnostics",
                "ok",
                ToolResultMeta::default(),
            ))),
        )
        .with_access_error("bad access"),
    ));

    let error = check_changed_files_diagnostics(
        &runtime,
        &registry,
        &mut session,
        &options,
        4,
        vec!["src/main.rs".to_owned()],
    )
    .expect_err("directory store should make access failure audit append fail");

    assert!(error.to_string().contains("failed to open"));
    Ok(())
}

#[test]
fn check_changed_files_diagnostics_honors_permission_policy_blocks() -> Result<()> {
    let temp = tempdir()?;
    let runtime = test_runtime()?;
    let mut session = Session::new("planned", "planned-model");
    let options = test_options(temp.path(), ApprovalMode::Ask);
    let mut registry = ToolRegistry::new();
    registry.register(Arc::new(DiagnosticsTestTool::new(
        ToolAccess::Execute,
        ExecutePlan::Ok(Box::new(ToolResult::ok(
            "call-1",
            "code_diagnostics",
            "ok",
            ToolResultMeta::default(),
        ))),
    )));

    let result = check_changed_files_diagnostics(
        &runtime,
        &registry,
        &mut session,
        &options,
        4,
        vec!["src/main.rs".to_owned()],
    )?;

    assert_eq!(tool_error_kind(&result), ToolErrorKind::ApprovalRequired);
    assert!(session.entries().iter().any(|entry| matches!(
        entry,
        SessionLogEntry::Control(ControlEntry::ToolApproval(approval))
            if approval.policy_decision == ApprovalMode::Ask
    )));
    Ok(())
}

#[test]
fn check_changed_files_diagnostics_propagates_policy_audit_write_errors() -> Result<()> {
    let temp = tempdir()?;
    let runtime = test_runtime()?;
    let mut session = session_with_directory_store(temp.path(), "policy-session-dir")?;
    let options = test_options(temp.path(), ApprovalMode::Allow);
    let mut registry = ToolRegistry::new();
    registry.register(Arc::new(DiagnosticsTestTool::new(
        ToolAccess::Read,
        ExecutePlan::Ok(Box::new(ToolResult::ok(
            "call-1",
            "code_diagnostics",
            "ok",
            ToolResultMeta::default(),
        ))),
    )));

    let error = check_changed_files_diagnostics(
        &runtime,
        &registry,
        &mut session,
        &options,
        4,
        vec!["src/main.rs".to_owned()],
    )
    .expect_err("directory store should make policy audit append fail");

    assert!(error.to_string().contains("failed to open"));
    Ok(())
}

#[test]
fn check_changed_files_diagnostics_converts_execute_failures_to_internal_tool_errors() -> Result<()>
{
    let temp = tempdir()?;
    let runtime = test_runtime()?;
    let mut session = Session::new("planned", "planned-model");
    let options = test_options(temp.path(), ApprovalMode::Allow);
    let mut registry = ToolRegistry::new();
    registry.register(Arc::new(
        DiagnosticsTestTool::new(ToolAccess::Execute, ExecutePlan::Err("tool exploded"))
            .with_subjects(vec![ToolSubject::path("src/main.rs", "src/main.rs")]),
    ));

    let result = check_changed_files_diagnostics(
        &runtime,
        &registry,
        &mut session,
        &options,
        4,
        vec!["src/main.rs".to_owned()],
    )?;

    assert_eq!(tool_error_kind(&result), ToolErrorKind::Internal);
    assert_eq!(
        result.metadata.details["call"]["paths"],
        serde_json::json!(["src/main.rs"])
    );
    assert!(session.entries().iter().any(|entry| matches!(
        entry,
        SessionLogEntry::Control(ControlEntry::ToolExecution(execution))
            if execution.status == ToolExecutionStatus::Started
    )));
    assert!(session.entries().iter().any(|entry| matches!(
        entry,
        SessionLogEntry::Control(ControlEntry::ToolExecution(execution))
            if execution.status == ToolExecutionStatus::Failed
    )));
    Ok(())
}

fn init_git_repo(workspace_root: &std::path::Path) -> Result<()> {
    run_git_status(workspace_root, &["init", "-q"])?;
    run_git_status(
        workspace_root,
        &["config", "user.email", "sigil-tests@example.invalid"],
    )?;
    run_git_status(workspace_root, &["config", "user.name", "Sigil Tests"])?;
    Ok(())
}

fn run_git_status(workspace_root: &std::path::Path, args: &[&str]) -> Result<()> {
    let status = git_command(workspace_root).args(args).status()?;
    if !status.success() {
        Err(anyhow!(
            "git {} failed under {}",
            args.join(" "),
            workspace_root.display()
        ))
    } else {
        Ok(())
    }
}

fn run_git(workspace_root: &std::path::Path, args: &[&str]) -> Result<()> {
    run_git_status(workspace_root, args)
}

#[test]
fn changed_source_files_keeps_supported_tracked_and_untracked_paths() -> Result<()> {
    let temp = tempdir()?;
    let workspace_root = temp.path().to_path_buf();
    init_git_repo(&workspace_root)?;
    fs::write(workspace_root.join("tracked.rs"), "fn main() {}\n")?;
    fs::write(workspace_root.join("ignored.txt"), "not source\n")?;
    run_git(&workspace_root, &["add", "tracked.rs", "ignored.txt"])?;
    run_git(&workspace_root, &["commit", "-qm", "initial"])?;

    fs::write(workspace_root.join("tracked.rs"), "fn broken( {\n")?;
    fs::write(workspace_root.join("new.ts"), "const broken = (\n")?;
    fs::write(workspace_root.join("notes.md"), "# docs\n")?;

    let changed = changed_source_files(&workspace_root)?;

    assert_eq!(changed, vec!["new.ts".to_owned(), "tracked.rs".to_owned()]);
    Ok(())
}

#[test]
fn git_output_reports_git_stderr_for_failed_commands() {
    let temp = tempdir().expect("tempdir should build");
    let error = git_output(temp.path(), &["definitely-not-a-command"])
        .expect_err("unknown git command should fail");

    let message = error.to_string();
    assert!(message.contains("git definitely-not-a-command failed under"));
    assert!(message.contains("definitely-not-a-command"));
}

#[test]
fn git_output_reports_failure_even_when_git_stderr_is_empty() -> Result<()> {
    let temp = tempdir()?;
    let workspace_root = temp.path().to_path_buf();
    init_git_repo(&workspace_root)?;
    fs::write(workspace_root.join("tracked.rs"), "fn main() {}\n")?;
    run_git(&workspace_root, &["add", "tracked.rs"])?;
    run_git(&workspace_root, &["commit", "-qm", "initial"])?;
    fs::write(workspace_root.join("tracked.rs"), "fn changed() {}\n")?;

    let error = git_output(
        &workspace_root,
        &["diff", "--quiet", "--exit-code", "HEAD", "--", "tracked.rs"],
    )
    .expect_err("changed tracked file should make git diff --quiet fail");

    let message = error.to_string();
    assert!(message.contains("git diff --quiet --exit-code HEAD -- tracked.rs failed under"));
    assert!(!message.contains(": "));
    Ok(())
}

#[test]
fn diagnostics_tool_event_wraps_tool_result() {
    let event = diagnostics_tool_event(sigil_kernel::ToolResult::ok(
        "call-1",
        "code_diagnostics",
        "{\"status\":\"ok\"}",
        sigil_kernel::ToolResultMeta::default(),
    ));

    assert!(matches!(
        event,
        RunEvent::ToolResult(result)
            if result.tool_name == "code_diagnostics" && !result.is_error()
    ));
}

#[test]
fn check_changed_files_diagnostics_completes_and_preserves_existing_call_metadata() -> Result<()> {
    let temp = tempdir()?;
    let runtime = test_runtime()?;
    let mut session = Session::new("planned", "planned-model");
    let options = test_options(temp.path(), ApprovalMode::Allow);
    let mut registry = ToolRegistry::new();
    registry.register(Arc::new(
        DiagnosticsTestTool::new(
            ToolAccess::Read,
            ExecutePlan::Ok(Box::new(ToolResult::ok(
                "call-1",
                "code_diagnostics",
                "{}",
                ToolResultMeta {
                    details: serde_json::json!({
                        "call": {
                            "summary": "prebuilt diagnostics metadata"
                        }
                    }),
                    ..ToolResultMeta::default()
                },
            ))),
        )
        .with_subjects(vec![ToolSubject::path("src/main.rs", "src/main.rs")]),
    ));

    let result = check_changed_files_diagnostics(
        &runtime,
        &registry,
        &mut session,
        &options,
        4,
        vec!["src/main.rs".to_owned()],
    )?;

    assert!(!result.is_error());
    assert_eq!(
        result.metadata.details["call"]["summary"],
        serde_json::json!("prebuilt diagnostics metadata")
    );
    assert!(session.entries().iter().any(|entry| matches!(
        entry,
        SessionLogEntry::Control(ControlEntry::ToolExecution(execution))
            if execution.status == ToolExecutionStatus::Completed
    )));
    Ok(())
}

#[test]
fn check_changed_files_diagnostics_returns_unsupported_when_tool_missing() -> Result<()> {
    let temp = tempdir()?;
    let workspace_root = temp.path().to_path_buf();
    let session_log_path = workspace_root.join(".sigil/sessions/session-missing-tool.jsonl");
    let root_config = test_root_config(&workspace_root, "planned", "planned-model");
    let runtime = test_runtime()?;
    let store = JsonlSessionStore::new(session_log_path.clone())?;
    let mut session = Session::load_from_store(
        root_config.agent.provider.clone(),
        root_config.agent.model.clone(),
        store,
    )?;
    let options = sigil_runtime::build_run_options(
        &root_config,
        workspace_root.clone(),
        sigil_kernel::InteractionMode::Interactive,
    );

    let result = check_changed_files_diagnostics(
        &runtime,
        &ToolRegistry::new(),
        &mut session,
        &options,
        20,
        vec!["src/main.rs".to_owned()],
    )?;

    assert!(result.is_error());
    assert!(matches!(
        &result.status,
        sigil_kernel::ToolResultStatus::Error(error)
            if error.kind == ToolErrorKind::Unsupported
    ));
    assert_eq!(result.metadata.details["call"]["path_count"], 1);

    let entries = JsonlSessionStore::read_entries(&session_log_path)?;
    assert!(entries.iter().any(|entry| matches!(
        entry,
        SessionLogEntry::Control(ControlEntry::ToolExecution(execution))
            if execution.tool_name == "code_diagnostics"
                && execution.status == ToolExecutionStatus::Failed
    )));
    Ok(())
}

#[test]
fn check_changed_files_diagnostics_honors_permission_denial() -> Result<()> {
    let temp = tempdir()?;
    let workspace_root = temp.path().to_path_buf();
    let session_log_path = workspace_root.join(".sigil/sessions/session-denied-tool.jsonl");
    let mut root_config = test_root_config(&workspace_root, "planned", "planned-model");
    root_config.permission = PermissionConfig {
        access: PermissionAccessConfig {
            read: Some(ApprovalMode::Deny),
            ..PermissionAccessConfig::default()
        },
        ..PermissionConfig::default()
    };
    let provider = PlannedProvider::new(Vec::new());
    let capabilities = provider.capabilities();
    let runtime = test_runtime()?;
    let registry = runtime.block_on(sigil_runtime::build_tool_registry(
        &root_config,
        &capabilities,
        workspace_root.clone(),
    ))?;
    let store = JsonlSessionStore::new(session_log_path.clone())?;
    let mut session = Session::load_from_store(
        root_config.agent.provider.clone(),
        root_config.agent.model.clone(),
        store,
    )?;
    let options = sigil_runtime::build_run_options(
        &root_config,
        workspace_root.clone(),
        sigil_kernel::InteractionMode::Interactive,
    );

    let result = check_changed_files_diagnostics(
        &runtime,
        &registry,
        &mut session,
        &options,
        20,
        vec!["src/main.rs".to_owned()],
    )?;

    assert!(result.is_error());
    let entries = JsonlSessionStore::read_entries(&session_log_path)?;
    assert!(entries.iter().any(|entry| matches!(
        entry,
        SessionLogEntry::Control(ControlEntry::ToolExecution(execution))
            if execution.tool_name == "code_diagnostics"
                && execution.status == ToolExecutionStatus::Failed
    )));
    Ok(())
}

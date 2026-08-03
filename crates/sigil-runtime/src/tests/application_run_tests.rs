use std::{
    path::Path,
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

use anyhow::Result;
use async_trait::async_trait;
use futures::{Stream, stream};
use sigil_kernel::{
    AgentRole, AgentRunDisposition, AgentRunOutcome, AgentRunOutput, AgentRunPurpose,
    AgentRunResult, AgentRunTerminalReason, ApprovalHandler, AssistantMessageKind,
    AutoApproveHandler, CompletionRequest, ControlEntry, ConversationRunLifecycleRecordV1,
    ConversationRunStartedEntryV1, ConversationRunTerminalStatusV1, DisclosurePresentationError,
    DisclosurePresentationReceipt, EgressDisclosurePresenter, IntegrationPlanId, InteractionMode,
    JsonlSessionStore, ModelMessage, NoopEventHandler, PreEgressDisclosure, Provider,
    ProviderCapabilities, ProviderChunk, PublicRunEvent, PublicRunEventKind, ReasoningEffort,
    ReasoningStreamSupport, RootConfig, RunCancellationOwner, RunCancellationRequestedEntry,
    RunCancellationTarget, RunCancellationTerminalOutcome, RunEvent, Session, SessionLogEntry,
    SessionRef, StartDurableTaskAction, TASK_PLAN_UPDATE_TOOL_NAME, TaskHandoffId, TaskId,
    TaskIntegrationReviewRequest, TaskPauseRequest, TaskPlanEntry, TaskPlanStatus,
    TaskRoutingPolicy, TaskRunCancellationScopeBoundEntry, TaskRunEntry, TaskRunStatus,
    TaskStepEntry, TaskStepId, TaskStepStatus, TaskVerificationRerunRequest, Tool, ToolAccess,
    ToolApproval, ToolArtifactSensitivity, ToolArtifactStore, ToolCall, ToolCategory, ToolContext,
    ToolPreviewCapability, ToolRegistry, ToolRegistryScope, ToolResult, ToolResultMeta,
    ToolResultRecordedV2, ToolSpec, UsageStats, conversation_run_lifecycle_record_from_stream,
};

use crate::agent_supervisor::task_role_runtime::TaskRoleProviderBuilder;

use super::{
    ApplicationCancellationTicket, ApplicationRunControl, ApplicationRunEventHandler,
    ApplicationRunEventSequence, ApplicationRunExecutionKind, ApplicationRunInteraction,
    ApplicationRunPrepareError, ApplicationRunPrepareErrorClass, ApplicationRunRequest,
    ApplicationRunServices, ApplicationRunTerminalStatus, ApplicationSessionLeaseManager,
    ApplicationTaskContinuationRequest, ApplicationTaskExecutionRuntime,
    ApplicationTaskPauseTicket, ApplicationTranscriptRole,
    MAX_APPLICATION_TRANSCRIPT_MESSAGE_BYTES, PublicApplicationEventBridge,
    accept_application_task_integration_review, admit_application_agent_binding,
    admit_application_model_selection, admit_application_reasoning_effort,
    admit_application_skill_binding, application_run_context_view, application_run_input,
    application_session_frontier_view, application_session_transcript_page,
    application_task_integration_review_view, application_terminal_projection,
    application_verification_view, attach_application_request_context, bind_application_session,
    bind_application_session_with_model, bind_application_session_with_model_ref,
    bind_application_session_with_model_ref_and_attachment, bind_existing_application_session,
    constrain_application_tool_registry, continue_application_task_handoff,
    default_application_session_path, optional_eager_mcp_warning, prepare_application_run,
    prepare_application_run_blocking, prepare_application_task_continuation,
    record_application_preparation_cancellation, rerun_application_verification,
    validate_execution_contract,
};

fn application_conversation_lifecycle(
    path: &Path,
) -> Result<Vec<ConversationRunLifecycleRecordV1>> {
    JsonlSessionStore::read_event_records(path)?
        .iter()
        .filter_map(|record| conversation_run_lifecycle_record_from_stream(record).transpose())
        .collect()
}

fn append_running_application_task(
    session: &mut Session,
    task_id: &TaskId,
    scope_id: Option<&str>,
    plan_version: u32,
) -> Result<()> {
    let mut controls = vec![
        ControlEntry::TaskRun(TaskRunEntry {
            task_id: task_id.clone(),
            parent_session_ref: SessionRef::new_relative("session.jsonl")?,
            objective: "control an application Task".to_owned(),
            status: TaskRunStatus::Running,
            reason: None,
        }),
        ControlEntry::TaskPlan(TaskPlanEntry {
            task_id: task_id.clone(),
            plan_version,
            status: TaskPlanStatus::Accepted,
            steps: Vec::new(),
            reason: None,
        }),
        ControlEntry::TaskStep(TaskStepEntry {
            task_id: task_id.clone(),
            plan_version,
            step_id: TaskStepId::new("step-active")?,
            role: AgentRole::Executor,
            status: TaskStepStatus::Running,
            title: Some("active application step".to_owned()),
            summary: None,
            reason: None,
        }),
    ];
    if let Some(scope_id) = scope_id {
        controls.push(ControlEntry::TaskRunCancellationScopeBound(
            TaskRunCancellationScopeBoundEntry {
                task_id: task_id.clone(),
                run_scope_id: scope_id.to_owned(),
            },
        ));
    }
    session.append_controls(controls)
}

fn write_application_test_config(path: &Path) -> Result<()> {
    std::fs::write(
        path,
        r#"config_version = 2

[workspace]
root = "."

[agent]
connection = "deepseek-default"
model = "deepseek-v4-flash"

[connections.deepseek-default]
label = "DeepSeek"
provider = "deepseek"
protocol = "deepseek"
base_url = "https://api.deepseek.com"
credential = { source = "environment", name = "SIGIL_API_KEY" }
"#,
    )?;
    Ok(())
}

fn write_unauthenticated_application_test_config(path: &Path) -> Result<()> {
    std::fs::write(
        path,
        r#"config_version = 2

[workspace]
root = "."

[agent]
connection = "local-test"
model = "gpt-test"

[connections.local-test]
label = "Local test"
provider = "custom"
protocol = "chat_completions"
base_url = "http://127.0.0.1:1"
credential = { source = "none" }
"#,
    )?;
    Ok(())
}

#[test]
fn durable_frontier_projection_is_scope_checked_and_read_only() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("session.jsonl");
    let store = JsonlSessionStore::new(&path)?;
    let mut session = Session::new("deepseek", "deepseek-v4-flash").with_store(store);
    session.append_control(ControlEntry::SessionIdentity {
        provider_name: "deepseek".to_owned(),
        model_name: "deepseek-v4-flash".to_owned(),
        resolved_model_route: None,
    })?;
    session.append_user_message(ModelMessage::user("hello"))?;
    let scope = session.session_scope_id().to_owned();

    let before = std::fs::read(&path)?;
    let frontier = application_session_frontier_view(&path, &scope)?;
    let after = std::fs::read(&path)?;

    assert_eq!(frontier.session_scope_id, scope);
    assert_eq!(frontier.through_stream_sequence, 2);
    assert_eq!(
        before, after,
        "frontier reads must not mutate durable truth"
    );
    assert!(application_session_frontier_view(&path, "another-scope").is_err());
    assert!(application_session_frontier_view(&path, "").is_err());
    Ok(())
}

struct RejectingDisclosurePresenter;

#[async_trait]
impl EgressDisclosurePresenter for RejectingDisclosurePresenter {
    async fn present(
        &self,
        _disclosure: PreEgressDisclosure,
    ) -> std::result::Result<DisclosurePresentationReceipt, DisclosurePresentationError> {
        Err(DisclosurePresentationError::SinkClosed)
    }
}

struct ApplicationTaskRoleProviderBuilder;

#[async_trait]
impl TaskRoleProviderBuilder for ApplicationTaskRoleProviderBuilder {
    async fn build(&self, _root_config: &RootConfig, role: AgentRole) -> Result<Box<dyn Provider>> {
        Ok(Box::new(ApplicationTaskRoleProvider { role }))
    }
}

struct ApplicationTaskRoleProvider {
    role: AgentRole,
}

#[async_trait]
impl Provider for ApplicationTaskRoleProvider {
    fn name(&self) -> &str {
        "application-task-test"
    }

    fn capabilities(&self) -> ProviderCapabilities {
        application_task_provider_capabilities()
    }

    async fn stream(
        &self,
        request: CompletionRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<ProviderChunk>> + Send>>> {
        let chunks = if self.role == AgentRole::Planner
            && request
                .tools
                .iter()
                .any(|tool| tool.name == TASK_PLAN_UPDATE_TOOL_NAME)
        {
            let args = r#"{
                "plan_version": 1,
                "status": "accepted",
                "steps": [{
                    "step_id": "inspect_application",
                    "title": "Inspect application runtime",
                    "role": "executor",
                    "mode": "read",
                    "isolation": "shared_read_only"
                }]
            }"#;
            vec![
                Ok(ProviderChunk::ToolCallStart {
                    id: "application-task-plan".to_owned(),
                    name: TASK_PLAN_UPDATE_TOOL_NAME.to_owned(),
                }),
                Ok(ProviderChunk::ToolCallArgsDelta {
                    id: "application-task-plan".to_owned(),
                    delta: args.to_owned(),
                }),
                Ok(ProviderChunk::ToolCallComplete(ToolCall {
                    id: "application-task-plan".to_owned(),
                    name: TASK_PLAN_UPDATE_TOOL_NAME.to_owned(),
                    args_json: args.to_owned(),
                })),
                Ok(ProviderChunk::Done),
            ]
        } else if self.role == AgentRole::Planner {
            vec![
                Ok(ProviderChunk::TextDelta(
                    "application durable task completed".to_owned(),
                )),
                Ok(ProviderChunk::Done),
            ]
        } else {
            vec![
                Ok(ProviderChunk::TextDelta(
                    "application task step completed".to_owned(),
                )),
                Ok(ProviderChunk::Done),
            ]
        };
        Ok(Box::pin(stream::iter(chunks)))
    }
}

fn application_task_provider_capabilities() -> ProviderCapabilities {
    ProviderCapabilities {
        exact_prefix_cache: true,
        reports_cache_tokens: true,
        reasoning_stream: ReasoningStreamSupport::Native,
        supports_reasoning_effort: true,
        supports_tool_stream: true,
        supports_background_tasks: false,
        supports_response_handles: false,
        supports_reasoning_artifacts: false,
        supports_structured_output: true,
        supports_assistant_prefix_seed: false,
        supports_schema_constrained_tools: true,
        supports_agent_background_resume: false,
        supports_agent_thread_usage: false,
        supports_agent_result_replay: false,
        supports_infill_completion: false,
        supports_system_fingerprint: true,
        tool_name_max_chars: 64,
    }
}

struct NamedTool(&'static str);

#[async_trait]
impl Tool for NamedTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: self.0.to_owned(),
            description: "application scope test tool".to_owned(),
            input_schema: serde_json::json!({"type":"object"}),
            category: ToolCategory::File,
            access: ToolAccess::Read,
            network_effect: None,
            preview: ToolPreviewCapability::None,
        }
    }

    async fn execute(
        &self,
        _ctx: ToolContext,
        call_id: String,
        _args: serde_json::Value,
    ) -> Result<ToolResult> {
        Ok(ToolResult::ok(
            call_id,
            self.0,
            "ok",
            ToolResultMeta::default(),
        ))
    }
}

#[test]
fn application_tool_scope_is_exact_and_rejects_unknown_names() {
    let mut registry = ToolRegistry::new();
    registry.register(Arc::new(NamedTool("read_file")));
    registry.register(Arc::new(NamedTool("bash")));
    let scope =
        ToolRegistryScope::from_names_and_prefixes(["read_file"], std::iter::empty::<&str>());
    let scoped = constrain_application_tool_registry(registry.clone(), &scope)
        .expect("known exact scope should apply");
    assert!(scoped.spec_for("read_file").is_some());
    assert!(scoped.spec_for("bash").is_none());

    let unknown =
        ToolRegistryScope::from_names_and_prefixes(["missing_tool"], std::iter::empty::<&str>());
    let error = match constrain_application_tool_registry(registry, &unknown) {
        Ok(_) => panic!("unknown tool scope must fail before dispatch"),
        Err(error) => error,
    };
    assert!(error.to_string().contains("unknown tool"));
}

#[test]
fn session_lease_rejects_overlapping_foreground_runs_and_releases_on_drop() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("sessions/session.jsonl");
    let manager = ApplicationSessionLeaseManager::new();

    let first = manager.acquire(&path)?;
    let error = manager
        .acquire(&path)
        .expect_err("same durable session must have one foreground run");
    assert!(error.to_string().contains("active foreground run"));

    drop(first);
    let reacquired = manager.acquire(&path)?;
    drop(reacquired);
    Ok(())
}

#[test]
fn preparation_recovers_an_orphan_run_after_exclusive_lease_before_next_admission() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let config_path = temp.path().join("sigil.toml");
    std::fs::write(
        &config_path,
        r#"config_version = 2

[workspace]
root = "."

[agent]
connection = "deepseek-default"
model = "deepseek-v4-flash"

[connections.deepseek-default]
label = "DeepSeek"
provider = "deepseek"
protocol = "deepseek"
base_url = "https://api.deepseek.com"
credential = { source = "environment", name = "SIGIL_API_KEY" }
"#,
    )?;
    let session_path = temp.path().join("state/sessions/orphan.jsonl");
    let binding = bind_application_session(&config_path, temp.path(), Some(&session_path))?;
    let session = Session::load_from_store(
        "deepseek",
        "deepseek-v4-flash",
        JsonlSessionStore::new(&binding.session_log_path)?,
    )?;
    session
        .conversation_run_lifecycle_recorder()?
        .append_started(&ConversationRunStartedEntryV1::new("orphan-run", 1)?)?;

    let mut request =
        ApplicationRunRequest::non_interactive(&config_path, temp.path(), "continue", "next-run");
    request.session_path = Some(binding.session_log_path.clone());
    let prepared = prepare_application_run_blocking(
        request,
        Arc::new(ApplicationSessionLeaseManager::new()),
        false,
    )?;

    let lifecycle = application_conversation_lifecycle(&binding.session_log_path)?;
    assert!(matches!(
        lifecycle.as_slice(),
        [
            ConversationRunLifecycleRecordV1::ConversationRunStartedV1(started),
            ConversationRunLifecycleRecordV1::ConversationRunFinalizedV1(finalized),
        ] if started.run_id() == "orphan-run"
            && finalized.run_id() == "orphan-run"
            && finalized.status() == ConversationRunTerminalStatusV1::Interrupted
    ));
    drop(prepared);
    Ok(())
}

#[tokio::test]
async fn verification_view_uses_durable_truth_and_rerun_shares_the_foreground_lease() -> Result<()>
{
    let temp = tempfile::tempdir()?;
    let config_path = temp.path().join("sigil.toml");
    write_application_test_config(&config_path)?;
    let session_path = temp.path().join("state/sessions/verification.jsonl");
    let binding = bind_application_session(&config_path, temp.path(), Some(&session_path))?;
    assert!(application_verification_view(&binding.session_log_path)?.is_none());

    let lease_manager = Arc::new(ApplicationSessionLeaseManager::new());
    let foreground = lease_manager.acquire(&binding.session_log_path)?;
    let services = ApplicationRunServices::with_session_leases(
        Arc::new(RejectingDisclosurePresenter),
        Arc::clone(&lease_manager),
    );
    let request = TaskVerificationRerunRequest::new(
        TaskId::new("task_1")?,
        1,
        TaskStepId::new("verify_1")?,
        "cargo-test".to_owned(),
        "check-hash".to_owned(),
        "policy-hash".to_owned(),
        "snapshot-1".to_owned(),
    );

    let error = rerun_application_verification(
        &config_path,
        temp.path(),
        &binding.session_log_path,
        &binding.session_scope_id,
        &services,
        &request,
    )
    .await
    .expect_err("verification must not overlap another foreground operation");

    assert!(error.to_string().contains("active foreground run"));
    drop(foreground);
    Ok(())
}

#[tokio::test]
async fn integration_review_projection_is_scope_checked_and_acceptance_shares_the_foreground_lease()
-> Result<()> {
    let temp = tempfile::tempdir()?;
    let config_path = temp.path().join("sigil.toml");
    write_application_test_config(&config_path)?;
    let session_path = temp.path().join("state/sessions/integration.jsonl");
    let binding = bind_application_session(&config_path, temp.path(), Some(&session_path))?;
    let before = std::fs::read(&binding.session_log_path)?;

    assert!(
        application_task_integration_review_view(
            &binding.session_log_path,
            &binding.session_scope_id,
        )?
        .is_none()
    );
    assert!(
        application_task_integration_review_view(
            &binding.session_log_path,
            "another-session-scope",
        )
        .is_err()
    );
    assert_eq!(
        std::fs::read(&binding.session_log_path)?,
        before,
        "integration review projection must not mutate durable truth"
    );

    let lease_manager = Arc::new(ApplicationSessionLeaseManager::new());
    let foreground = lease_manager.acquire(&binding.session_log_path)?;
    let services = ApplicationRunServices::with_session_leases(
        Arc::new(RejectingDisclosurePresenter),
        Arc::clone(&lease_manager),
    );
    let request = TaskIntegrationReviewRequest {
        request_id: "review-request".to_owned(),
        task_id: TaskId::new("task-integration")?,
        plan_id: IntegrationPlanId::new("plan-integration")?,
        plan_version: 1,
        preview_digest: "sha256:preview".to_owned(),
    };

    let error = accept_application_task_integration_review(
        &config_path,
        temp.path(),
        &binding.session_log_path,
        &binding.session_scope_id,
        &services,
        &request,
    )
    .await
    .expect_err("integration acceptance must not overlap a foreground operation");
    assert!(error.to_string().contains("active foreground run"));

    drop(foreground);
    let error = accept_application_task_integration_review(
        &config_path,
        temp.path(),
        &binding.session_log_path,
        &binding.session_scope_id,
        &services,
        &request,
    )
    .await
    .expect_err("a session without a current review must reject acceptance");
    assert!(error.to_string().contains("no longer current"));
    assert_eq!(
        std::fs::read(&binding.session_log_path)?,
        before,
        "rejected integration acceptance must not append durable facts"
    );
    Ok(())
}

#[cfg(unix)]
#[test]
fn session_lease_collapses_symlink_aliases_to_one_canonical_path() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let real = temp.path().join("real-session.jsonl");
    let alias = temp.path().join("alias-session.jsonl");
    std::fs::File::create(&real)?;
    std::os::unix::fs::symlink(&real, &alias)?;
    let manager = ApplicationSessionLeaseManager::new();

    let first = manager.acquire(&real)?;
    let error = manager
        .acquire(&alias)
        .expect_err("symlink alias must resolve to the active durable session");
    assert!(error.to_string().contains("active foreground run"));
    drop(first);
    Ok(())
}

#[test]
fn default_session_path_and_repo_context_are_application_owned() -> Result<()> {
    let temp = tempfile::tempdir()?;
    std::fs::write(temp.path().join("README.md"), "Sigil application service")?;

    let path = default_application_session_path(&temp.path().join("sessions"));
    let input = application_run_input(temp.path(), "summarize README.md".to_owned());

    assert!(path.starts_with(temp.path().join("sessions")));
    assert_eq!(
        path.extension().and_then(|value| value.to_str()),
        Some("jsonl")
    );
    assert!(
        input
            .runtime_context
            .items
            .iter()
            .any(|item| item.id == "repo-file:README.md")
    );
    Ok(())
}

#[tokio::test]
async fn application_request_context_uses_runtime_resolver() -> Result<()> {
    let temp = tempfile::tempdir()?;
    std::fs::write(temp.path().join("README.md"), "Sigil application resolver")?;
    let resolver = crate::RequestContextResolver::request_local(temp.path().to_path_buf());

    let input = attach_application_request_context(
        sigil_kernel::AgentRunInput::user("summarize README.md"),
        &resolver,
        "summarize README.md",
    )
    .await;

    assert!(
        input
            .runtime_context
            .items
            .iter()
            .any(|item| item.id == "repo-file:README.md")
    );
    assert!(
        input
            .runtime_context
            .items
            .iter()
            .any(|item| item.id == "lsp-context:unavailable")
    );
    Ok(())
}

#[test]
fn adapter_session_binding_creates_and_reopens_the_same_durable_v2_scope() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let config_path = temp.path().join("sigil.toml");
    std::fs::write(
        &config_path,
        r#"config_version = 2

[workspace]
root = "."

[agent]
connection = "deepseek-default"
model = "deepseek-v4-flash"

[connections.deepseek-default]
label = "DeepSeek"
provider = "deepseek"
protocol = "deepseek"
base_url = "https://api.deepseek.com"
credential = { source = "environment", name = "SIGIL_API_KEY" }
"#,
    )?;
    let requested_path = temp.path().join("state/sessions/http.jsonl");

    let first = bind_application_session(&config_path, temp.path(), Some(&requested_path))?;
    let second = bind_application_session(&config_path, temp.path(), Some(&requested_path))?;

    assert_eq!(first, second);
    assert!(first.session_log_path.is_absolute());
    assert!(first.session_log_path.exists());
    assert!(!first.session_scope_id.is_empty());
    Ok(())
}

#[test]
fn adapter_session_binding_accepts_connection_models_and_rejects_unknown_connections() -> Result<()>
{
    let temp = tempfile::tempdir()?;
    let config_path = temp.path().join("sigil.toml");
    write_application_test_config(&config_path)?;
    let selected_path = temp.path().join("state/sessions/pro.jsonl");

    let binding = bind_application_session_with_model(
        &config_path,
        temp.path(),
        Some(&selected_path),
        Some("deepseek-v4-pro"),
    )?;
    let context = application_run_context_view(
        &config_path,
        temp.path(),
        &binding.session_log_path,
        &binding.session_scope_id,
    )?;
    assert_eq!(context.model_name, "deepseek-v4-pro");
    assert!(
        context
            .model_options
            .iter()
            .any(|option| option.model_name == "deepseek-v4-flash")
    );
    assert!(
        context
            .model_options
            .iter()
            .any(|option| option.model_name == "deepseek-v4-pro")
    );

    let manual = bind_application_session_with_model(
        &config_path,
        temp.path(),
        Some(&temp.path().join("state/sessions/manual.jsonl")),
        Some("unknown-model"),
    )?;
    let manual_context = application_run_context_view(
        &config_path,
        temp.path(),
        &manual.session_log_path,
        &manual.session_scope_id,
    )?;
    assert_eq!(manual_context.model_name, "unknown-model");

    let connection_id = sigil_kernel::ConnectionId::new("deepseek-default")?;
    let rejected_unadmitted_model = bind_application_session_with_model_ref(
        &config_path,
        temp.path(),
        Some(&temp.path().join("state/sessions/unadmitted-model.jsonl")),
        Some(&connection_id),
        Some("unknown-model"),
    );
    assert!(matches!(
        rejected_unadmitted_model,
        Err(ApplicationRunPrepareError::InvalidInvocation { .. })
    ));

    let missing_connection = sigil_kernel::ConnectionId::new("missing-connection")?;
    let rejected = bind_application_session_with_model_ref(
        &config_path,
        temp.path(),
        Some(&temp.path().join("state/sessions/unknown-connection.jsonl")),
        Some(&missing_connection),
        Some("deepseek-v4-pro"),
    );
    assert!(matches!(
        rejected,
        Err(ApplicationRunPrepareError::ConnectionConfigInvalid { .. })
    ));
    Ok(())
}

#[test]
fn run_context_catalog_keeps_same_model_ids_distinct_across_connections() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let config_path = temp.path().join("sigil.toml");
    std::fs::write(
        &config_path,
        r#"config_version = 2

[workspace]
root = "."

[agent]
connection = "deepseek-personal"
model = "deepseek-v4-flash"

[connections.deepseek-personal]
label = "DeepSeek personal"
provider = "deepseek"
protocol = "deepseek"
base_url = "https://api.deepseek.com"
credential = { source = "environment", name = "SIGIL_API_KEY" }

[connections.deepseek-team]
label = "DeepSeek team"
provider = "deepseek"
protocol = "deepseek"
base_url = "https://api.deepseek.com"
credential = { source = "environment", name = "SIGIL_API_KEY" }
"#,
    )?;
    let binding = bind_application_session(&config_path, temp.path(), None)?;

    let context = application_run_context_view(
        &config_path,
        temp.path(),
        &binding.session_log_path,
        &binding.session_scope_id,
    )?;

    let flash_routes = context
        .model_options
        .iter()
        .filter(|option| option.model_ref.model_id == "deepseek-v4-flash")
        .map(|option| option.model_ref.connection_id.as_str())
        .collect::<Vec<_>>();
    assert_eq!(flash_routes, vec!["deepseek-personal", "deepseek-team"]);
    assert_eq!(
        context
            .model_options
            .first()
            .map(|option| &option.model_ref),
        Some(&context.model_ref),
    );
    Ok(())
}

#[test]
fn session_reopen_binding_requires_an_existing_durable_file() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let config_path = temp.path().join("sigil.toml");
    write_application_test_config(&config_path)?;
    let session_path = temp.path().join("state/sessions/existing.jsonl");
    let created = bind_application_session(&config_path, temp.path(), Some(&session_path))?;

    let reopened = bind_existing_application_session(&config_path, &created.session_log_path)?;

    assert_eq!(reopened, created);
    let missing = temp.path().join("state/sessions/missing.jsonl");
    assert!(bind_existing_application_session(&config_path, &missing).is_err());
    assert!(!missing.exists());
    Ok(())
}

#[test]
fn session_reopen_binding_rejects_a_route_less_current_session() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let config_path = temp.path().join("sigil.toml");
    write_application_test_config(&config_path)?;
    let session_path = temp.path().join("state/sessions/route-less-v2.jsonl");
    let store = JsonlSessionStore::new(&session_path)?;
    store.append(&SessionLogEntry::Control(ControlEntry::SessionIdentity {
        provider_name: "deepseek".to_owned(),
        model_name: "deepseek-v4-flash".to_owned(),
        resolved_model_route: None,
    }))?;
    let before = std::fs::read(&session_path)?;

    assert!(bind_existing_application_session(&config_path, &session_path).is_err());
    assert_eq!(std::fs::read(&session_path)?, before);
    Ok(())
}

#[test]
fn run_context_exposes_exact_bound_confirmation_and_application_applies_it() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let config_path = temp.path().join("sigil.toml");
    let write_config = |port: u16| -> Result<()> {
        std::fs::write(
            &config_path,
            format!(
                r#"config_version = 2

[workspace]
root = "."

[agent]
connection = "local-test"
model = "gpt-test"

[connections.local-test]
label = "Local test"
provider = "custom"
protocol = "chat_completions"
base_url = "http://127.0.0.1:{port}/v1"
credential = {{ source = "none" }}
"#
            ),
        )?;
        Ok(())
    };
    write_config(1)?;
    let session_path = temp.path().join("state/sessions/confirmation.jsonl");
    let binding = bind_application_session(&config_path, temp.path(), Some(&session_path))?;
    write_config(2)?;

    let context = application_run_context_view(
        &config_path,
        temp.path(),
        &binding.session_log_path,
        &binding.session_scope_id,
    )?;
    let recovery = context
        .route_recovery
        .expect("changed origin must require confirmation");
    assert_eq!(
        recovery.code,
        super::ApplicationSessionRouteRecoveryCode::SessionRouteConfirmationRequired
    );
    assert!(!recovery.recovery_binding.contains("127.0.0.1"));

    let mut unconfirmed = ApplicationRunRequest::non_interactive(
        &config_path,
        temp.path(),
        "continue",
        "run-unconfirmed",
    );
    unconfirmed.session_path = Some(binding.session_log_path.clone());
    let error = match prepare_application_run_blocking(
        unconfirmed,
        Arc::new(ApplicationSessionLeaseManager::new()),
        false,
    ) {
        Ok(_) => panic!("unconfirmed egress change must not prepare a run"),
        Err(error) => error,
    };
    assert_eq!(
        error.class(),
        ApplicationRunPrepareErrorClass::SessionRouteConfirmationRequired
    );

    JsonlSessionStore::new(&binding.session_log_path)?.append(&SessionLogEntry::User(
        sigil_kernel::ModelMessage::user("durable frontier advanced"),
    ))?;
    let mut stale_confirmation = ApplicationRunRequest::non_interactive(
        &config_path,
        temp.path(),
        "continue",
        "run-stale-confirmation",
    );
    stale_confirmation.session_path = Some(binding.session_log_path.clone());
    stale_confirmation.route_recovery_binding = Some(recovery.recovery_binding.clone());
    let stale_error = match prepare_application_run_blocking(
        stale_confirmation,
        Arc::new(ApplicationSessionLeaseManager::new()),
        false,
    ) {
        Ok(_) => panic!("a recovery binding must stale when the durable frontier advances"),
        Err(error) => error,
    };
    assert_eq!(
        stale_error.class(),
        ApplicationRunPrepareErrorClass::SessionRouteConfirmationRequired
    );
    let refreshed_recovery = application_run_context_view(
        &config_path,
        temp.path(),
        &binding.session_log_path,
        &binding.session_scope_id,
    )?
    .route_recovery
    .expect("route recovery remains required")
    .recovery_binding;

    let mut confirmed = ApplicationRunRequest::non_interactive(
        &config_path,
        temp.path(),
        "continue",
        "run-confirmed",
    );
    confirmed.session_path = Some(binding.session_log_path.clone());
    confirmed.route_recovery_binding = Some(refreshed_recovery);
    let prepared = prepare_application_run_blocking(
        confirmed,
        Arc::new(ApplicationSessionLeaseManager::new()),
        false,
    )?;
    assert_eq!(
        prepared.session.session_scope_id(),
        binding.session_scope_id
    );
    assert!(prepared.session.entries().iter().any(|entry| matches!(
        entry,
        SessionLogEntry::Control(ControlEntry::SessionModelSelected { .. })
    )));
    assert!(!prepared.session.entries().iter().any(|entry| matches!(
        entry,
        SessionLogEntry::Control(ControlEntry::SessionRouteRebound { .. })
    )));
    Ok(())
}

#[test]
fn attached_bind_rejects_external_owner_before_route_recovery_writes() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let config_path = temp.path().join("sigil.toml");
    let write_config = |port: u16| -> Result<()> {
        std::fs::write(
            &config_path,
            format!(
                r#"config_version = 2

[workspace]
root = "."

[agent]
connection = "local-test"
model = "gpt-test"

[connections.local-test]
label = "Local test"
provider = "custom"
protocol = "chat_completions"
base_url = "http://127.0.0.1:{port}/v1"
credential = {{ source = "none" }}
"#
            ),
        )?;
        Ok(())
    };
    write_config(1)?;
    let session_path = temp.path().join("state/sessions/attached-bind.jsonl");
    let binding = bind_application_session(&config_path, temp.path(), Some(&session_path))?;
    write_config(2)?;
    let before = std::fs::read(&binding.session_log_path)?;
    let owner = crate::interactive_session_attachment::InteractiveSessionAttachmentLease::acquire(
        &binding.session_log_path,
    )?;

    let error = bind_application_session_with_model_ref_and_attachment(
        &config_path,
        temp.path(),
        Some(&binding.session_log_path),
        None,
        None,
    )
    .expect_err("external owner must reject before route recovery loads or appends");
    assert_eq!(
        error.class(),
        ApplicationRunPrepareErrorClass::SessionAlreadyActive
    );
    assert!(
        error
            .recovery_binding()
            .is_some_and(|binding| !binding.is_empty())
    );
    assert_eq!(std::fs::read(&binding.session_log_path)?, before);
    drop(owner);
    Ok(())
}

#[test]
fn same_origin_endpoint_correction_rebinds_without_blocking_run_context() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let config_path = temp.path().join("sigil.toml");
    let write_config = |path: &str| -> Result<()> {
        std::fs::write(
            &config_path,
            format!(
                r#"config_version = 2

[workspace]
root = "."

[agent]
connection = "local-test"
model = "gpt-test"

[connections.local-test]
label = "Local test"
provider = "custom"
protocol = "chat_completions"
base_url = "http://127.0.0.1:1{path}"
credential = {{ source = "none" }}
"#
            ),
        )?;
        Ok(())
    };
    write_config("/wrong")?;
    let session_path = temp.path().join("state/sessions/rebind.jsonl");
    let binding = bind_application_session(&config_path, temp.path(), Some(&session_path))?;
    write_config("/v1")?;
    assert_eq!(
        bind_existing_application_session(&config_path, &binding.session_log_path)?,
        binding
    );

    let context = application_run_context_view(
        &config_path,
        temp.path(),
        &binding.session_log_path,
        &binding.session_scope_id,
    )?;
    assert!(context.route_recovery.is_none());
    let mut request = ApplicationRunRequest::non_interactive(
        &config_path,
        temp.path(),
        "continue",
        "run-rebound",
    );
    request.session_path = Some(binding.session_log_path.clone());
    let prepared = prepare_application_run_blocking(
        request,
        Arc::new(ApplicationSessionLeaseManager::new()),
        false,
    )?;
    assert_eq!(
        prepared.session.session_scope_id(),
        binding.session_scope_id
    );
    assert_eq!(
        prepared
            .session
            .entries()
            .iter()
            .filter(|entry| matches!(
                entry,
                SessionLogEntry::Control(ControlEntry::SessionRouteRebound { .. })
            ))
            .count(),
        1
    );
    Ok(())
}

#[test]
fn run_context_uses_durable_identity_and_only_proven_usage() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let config_path = temp.path().join("sigil.toml");
    write_application_test_config(&config_path)?;
    let session_path = temp.path().join("state/sessions/context.jsonl");
    let binding = bind_application_session(&config_path, temp.path(), Some(&session_path))?;

    let empty = application_run_context_view(
        &config_path,
        temp.path(),
        &binding.session_log_path,
        &binding.session_scope_id,
    )?;
    assert_eq!(empty.provider_name, "deepseek");
    assert_eq!(empty.model_name, "deepseek-v4-flash");
    assert_eq!(empty.model_ref.connection_id.as_str(), "deepseek-default");
    assert_eq!(empty.model_ref.model_id, "deepseek-v4-flash");
    assert_eq!(
        empty.default_permission_mode,
        sigil_kernel::PermissionMode::Manual
    );
    assert_eq!(empty.model_options.len(), 2);
    let pro = empty
        .model_options
        .iter()
        .find(|option| option.model_name == "deepseek-v4-pro")
        .expect("pro model option");
    assert_eq!(
        pro.availability,
        crate::provider_connections::ModelAvailability::Unverified
    );
    assert!(
        empty.model_options.iter().all(|option| {
            option.availability != crate::provider_connections::ModelAvailability::Available
        }),
        "bundled and configured-only application models must not claim remote availability"
    );
    assert_eq!(pro.default_reasoning_effort, Some(ReasoningEffort::Max));
    assert_eq!(
        pro.available_reasoning_efforts,
        vec![
            ReasoningEffort::Low,
            ReasoningEffort::Medium,
            ReasoningEffort::High,
            ReasoningEffort::Max,
        ]
    );
    assert!(pro.reasoning_effort_binding.is_some());
    assert!(!empty.model_selection_binding.is_empty());
    assert_eq!(
        empty.available_reasoning_efforts,
        vec![
            ReasoningEffort::Low,
            ReasoningEffort::Medium,
            ReasoningEffort::High,
            ReasoningEffort::Max,
        ]
    );
    assert_eq!(empty.default_reasoning_effort, Some(ReasoningEffort::Max));
    assert!(empty.reasoning_effort_binding.is_some());
    assert_eq!(empty.context_window_tokens, Some(1_000_000));
    assert_eq!(
        empty.context_window_source,
        crate::ContextWindowSource::Provider
    );
    assert_eq!(empty.last_prompt_tokens, None);
    assert_eq!(
        empty.extension_catalog.commands.len(),
        crate::APPLICATION_COMMANDS.len()
    );
    assert!(
        empty
            .extension_catalog
            .commands
            .iter()
            .any(|command| command.canonical == "/new" && command.available)
    );
    assert!(empty.extension_catalog.agents.iter().any(|agent| {
        agent.available
            && agent.unavailable_reason.is_none()
            && agent.binding.as_ref().is_some_and(|binding| {
                binding.profile_id == agent.id
                    && agent.snapshot_id.as_deref() == Some(binding.snapshot_id.as_str())
            })
    }));

    JsonlSessionStore::new(&binding.session_log_path)?.append(&SessionLogEntry::Control(
        ControlEntry::UsageSnapshot(UsageStats {
            prompt_tokens: 42_000,
            completion_tokens: 800,
            cache_hit_tokens: 30_000,
            cache_miss_tokens: 12_000,
            input_cost: 0.0,
            output_cost: 0.0,
            cache_savings: 0.0,
            system_fingerprint: None,
            cache_usage: None,
            pricing_snapshot: None,
        }),
    ))?;
    let used = application_run_context_view(
        &config_path,
        temp.path(),
        &binding.session_log_path,
        &binding.session_scope_id,
    )?;
    assert_eq!(used.last_prompt_tokens, Some(42_000));
    assert!(
        application_run_context_view(
            &config_path,
            temp.path(),
            &binding.session_log_path,
            "wrong-scope",
        )
        .is_err()
    );
    Ok(())
}

#[test]
fn run_model_selection_switches_the_existing_session_and_rejects_stale_capabilities() -> Result<()>
{
    let temp = tempfile::tempdir()?;
    let config_path = temp.path().join("sigil.toml");
    write_application_test_config(&config_path)?;
    let root_config = RootConfig::load(&config_path)?;
    let session_path = temp.path().join("state/sessions/model-switch.jsonl");
    let binding = bind_application_session(&config_path, temp.path(), Some(&session_path))?;
    let context = application_run_context_view(
        &config_path,
        temp.path(),
        &binding.session_log_path,
        &binding.session_scope_id,
    )?;
    let mut request =
        ApplicationRunRequest::non_interactive(&config_path, temp.path(), "hello", "run-model");
    request.session_path = Some(binding.session_log_path.clone());
    request.model_connection_id = Some(sigil_kernel::ConnectionId::new("deepseek-default")?);
    request.model_name = Some("deepseek-v4-pro".to_owned());
    request.model_selection_binding = Some(context.model_selection_binding.clone());
    let pro_option = context
        .model_options
        .iter()
        .find(|option| option.model_name == "deepseek-v4-pro")
        .expect("pro model option");
    request.reasoning_effort = pro_option.default_reasoning_effort.clone();
    request.reasoning_effort_binding = pro_option.reasoning_effort_binding.clone();

    let store = JsonlSessionStore::new(&binding.session_log_path)?;
    let session = Session::load_from_store("deepseek", "deepseek-v4-flash", store)?;
    let (selected_provider, selected_route) = admit_application_model_selection(
        &request,
        &root_config,
        &session,
        &temp.path().join("cache"),
    )?
    .expect("explicit model selection should resolve a route");
    assert_eq!(selected_provider, "deepseek");
    assert_eq!(selected_route.model_ref.model_id, "deepseek-v4-pro");
    admit_application_reasoning_effort(&request, "deepseek", "deepseek-v4-pro")?;

    let mut stale_effort = request.clone();
    stale_effort.reasoning_effort_binding = Some("stale-effort".to_owned());
    assert!(matches!(
        prepare_application_run_blocking(
            stale_effort,
            Arc::new(ApplicationSessionLeaseManager::new()),
            false,
        ),
        Err(ApplicationRunPrepareError::InvalidInvocation { .. })
    ));
    let unchanged = application_run_context_view(
        &config_path,
        temp.path(),
        &binding.session_log_path,
        &binding.session_scope_id,
    )?;
    assert_eq!(unchanged.model_name, "deepseek-v4-flash");

    let prepared = prepare_application_run_blocking(
        request.clone(),
        Arc::new(ApplicationSessionLeaseManager::new()),
        false,
    )?;
    assert_eq!(
        prepared.session.session_scope_id(),
        binding.session_scope_id
    );
    assert_eq!(prepared.session.model_name(), "deepseek-v4-pro");
    drop(prepared);

    let selected_context = application_run_context_view(
        &config_path,
        temp.path(),
        &binding.session_log_path,
        &binding.session_scope_id,
    )?;
    assert_eq!(selected_context.model_name, "deepseek-v4-pro");
    assert_eq!(
        selected_context.model_ref.connection_id.as_str(),
        "deepseek-default"
    );
    assert_eq!(selected_context.model_ref.model_id, "deepseek-v4-pro");
    assert_eq!(
        selected_context.default_reasoning_effort,
        Some(ReasoningEffort::Max)
    );
    let mut stale = request;
    stale.model_name = Some("deepseek-v4-flash".to_owned());
    let pro_store = JsonlSessionStore::new(&binding.session_log_path)?;
    let pro_session = Session::load_from_store("deepseek", "deepseek-v4-pro", pro_store)?;
    assert!(matches!(
        admit_application_model_selection(
            &stale,
            &root_config,
            &pro_session,
            &temp.path().join("cache"),
        ),
        Err(ApplicationRunPrepareError::InvalidInvocation { .. })
    ));
    Ok(())
}

#[test]
fn run_model_selection_switches_provider_connections_without_replacing_the_session() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let config_path = temp.path().join("sigil.toml");
    std::fs::write(
        &config_path,
        r#"config_version = 2

[workspace]
root = "."

[agent]
connection = "local-default"
model = "local-model"

[connections.local-default]
label = "Local default"
provider = "custom"
protocol = "responses"
base_url = "http://127.0.0.1:11434/v1"
credential = { source = "none" }

[connections.deepseek-team]
label = "DeepSeek team"
provider = "deepseek"
protocol = "deepseek"
base_url = "https://api.deepseek.com"
credential = { source = "environment", name = "SIGIL_API_KEY" }
"#,
    )?;
    let session_path = temp.path().join("state/sessions/cross-provider.jsonl");
    let binding = bind_application_session(&config_path, temp.path(), Some(&session_path))?;
    let context = application_run_context_view(
        &config_path,
        temp.path(),
        &binding.session_log_path,
        &binding.session_scope_id,
    )?;
    assert_eq!(context.model_ref.connection_id.as_str(), "local-default");
    assert!(context.model_options.iter().any(|option| {
        option.model_ref.connection_id.as_str() == "deepseek-team"
            && option.model_ref.model_id == "deepseek-v4-flash"
    }));

    let mut request =
        ApplicationRunRequest::non_interactive(&config_path, temp.path(), "hello", "run-cross");
    request.session_path = Some(binding.session_log_path.clone());
    request.model_connection_id = Some(sigil_kernel::ConnectionId::new("deepseek-team")?);
    request.model_name = Some("deepseek-v4-flash".to_owned());
    request.model_selection_binding = Some(context.model_selection_binding);

    let prepared = prepare_application_run_blocking(
        request,
        Arc::new(ApplicationSessionLeaseManager::new()),
        false,
    )?;
    assert_eq!(
        prepared.session.session_scope_id(),
        binding.session_scope_id
    );
    assert_eq!(prepared.session.provider_name(), "deepseek");
    assert_eq!(prepared.session.model_name(), "deepseek-v4-flash");
    drop(prepared);

    let switched = application_run_context_view(
        &config_path,
        temp.path(),
        &binding.session_log_path,
        &binding.session_scope_id,
    )?;
    assert_eq!(switched.model_ref.connection_id.as_str(), "deepseek-team");
    assert_eq!(switched.provider_name, "deepseek");
    assert_eq!(switched.model_name, "deepseek-v4-flash");
    Ok(())
}

#[test]
fn recovery_model_selection_requires_exact_route_and_catalog_bindings() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let config_path = temp.path().join("sigil.toml");
    std::fs::write(
        &config_path,
        r#"config_version = 2

[workspace]
root = "."

[agent]
connection = "original"
model = "gpt-original"

[connections.original]
label = "Original"
provider = "custom"
protocol = "responses"
base_url = "http://127.0.0.1:1/v1"
credential = { source = "none" }
"#,
    )?;
    let session_path = temp.path().join("state/sessions/replacement-binding.jsonl");
    let binding = bind_application_session(&config_path, temp.path(), Some(&session_path))?;
    std::fs::write(
        &config_path,
        r#"config_version = 2

[workspace]
root = "."

[agent]
connection = "replacement"
model = "gpt-replacement"

[connections.replacement]
label = "Replacement"
provider = "custom"
protocol = "chat_completions"
base_url = "http://127.0.0.1:2/v1"
credential = { source = "none" }
"#,
    )?;
    let context = application_run_context_view(
        &config_path,
        temp.path(),
        &binding.session_log_path,
        &binding.session_scope_id,
    )?;
    let recovery = context
        .route_recovery
        .as_ref()
        .expect("missing original connection should require replacement");
    assert_eq!(
        recovery.code,
        super::ApplicationSessionRouteRecoveryCode::SessionRouteSelectionRequired
    );
    let replacement = context
        .model_options
        .iter()
        .find(|option| {
            option.availability
                != crate::provider_connections::ModelAvailability::ConfiguredUnavailable
        })
        .expect("replacement route should be selectable")
        .model_ref
        .clone();
    let before = std::fs::read(&binding.session_log_path)?;
    let request = |route_recovery_binding: Option<String>, model_selection_binding: String| {
        let mut request = ApplicationRunRequest::non_interactive(
            &config_path,
            temp.path(),
            "continue on replacement",
            "run-replacement-binding",
        );
        request.session_path = Some(binding.session_log_path.clone());
        request.model_connection_id = Some(replacement.connection_id.clone());
        request.model_name = Some(replacement.model_id.clone());
        request.model_selection_binding = Some(model_selection_binding);
        request.route_recovery_binding = route_recovery_binding;
        request
    };

    for rejected in [
        request(None, context.model_selection_binding.clone()),
        request(
            Some("stale-route-recovery-binding".to_owned()),
            context.model_selection_binding.clone(),
        ),
    ] {
        let error = match prepare_application_run_blocking(
            rejected,
            Arc::new(ApplicationSessionLeaseManager::new()),
            false,
        ) {
            Ok(_) => panic!("replacement selection must require the exact route binding"),
            Err(error) => error,
        };
        assert_eq!(
            error.class(),
            ApplicationRunPrepareErrorClass::SessionRouteSelectionRequired
        );
        assert_eq!(std::fs::read(&binding.session_log_path)?, before);
    }

    let prepared = prepare_application_run_blocking(
        request(
            Some(recovery.recovery_binding.clone()),
            context.model_selection_binding,
        ),
        Arc::new(ApplicationSessionLeaseManager::new()),
        false,
    )?;
    assert_eq!(
        prepared.session.session_scope_id(),
        binding.session_scope_id
    );
    assert_eq!(
        prepared
            .session
            .resolved_model_route()
            .map(|route| &route.model_ref),
        Some(&replacement)
    );
    Ok(())
}

#[test]
fn run_context_uses_only_the_exact_connection_fresh_catalog_cache() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let config_path = temp.path().join("sigil.toml");
    let cache_root = temp.path().join("cache");
    std::fs::write(
        &config_path,
        format!(
            r#"config_version = 2

[storage]
cache_root = {}

[workspace]
root = "."

[agent]
connection = "local-cache"
model = "local-cache-model"

[connections.local-cache]
label = "Local cached"
provider = "custom"
protocol = "responses"
base_url = "http://127.0.0.1:11434/v1"
credential = {{ source = "none" }}
"#,
            toml::Value::String(cache_root.to_string_lossy().into_owned())
        ),
    )?;
    let root_config = RootConfig::load(&config_path)?;
    let loaded = crate::provider_connections::load_provider_connections(&root_config);
    let connection = &loaded
        .connections
        .get(&sigil_kernel::ConnectionId::new("local-cache").expect("connection id should parse"))
        .expect("exact connection should load")
        .config;
    let cached_ref = sigil_kernel::ModelRef::new(connection.id.clone(), "local-cache-model")?;
    crate::provider_connections::seed_unauthenticated_catalog_cache_for_test(
        &cache_root,
        connection,
        &[crate::provider_connections::ModelCatalogEntry {
            model_ref: cached_ref,
            display_name: "Cached exact model".to_owned(),
            availability: crate::provider_connections::ModelAvailability::Available,
            recommendation: crate::provider_connections::ModelRecommendation::Recommended,
            provenance: crate::provider_connections::ModelCatalogProvenance::Remote,
        }],
    )?;

    let binding = bind_application_session(&config_path, temp.path(), None)?;
    let context = application_run_context_view(
        &config_path,
        temp.path(),
        &binding.session_log_path,
        &binding.session_scope_id,
    )?;

    assert_eq!(context.model_options.len(), 1);
    let option = &context.model_options[0];
    assert_eq!(option.model_ref.connection_id.as_str(), "local-cache");
    assert_eq!(option.model_name, "local-cache-model");
    assert_eq!(option.display_name, "Cached exact model");
    assert_eq!(
        option.availability,
        crate::provider_connections::ModelAvailability::Available
    );
    assert_eq!(
        option.provenance,
        crate::provider_connections::ModelCatalogProvenance::Cache
    );
    Ok(())
}

#[test]
fn exact_inline_skill_binding_loads_transient_context_and_audit_entry() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let config_path = temp.path().join("sigil.toml");
    std::fs::write(
        &config_path,
        r#"config_version = 2

[workspace]
root = "."

[agent]
connection = "deepseek-default"
model = "deepseek-v4-flash"
[connections.deepseek-default]
label = "DeepSeek"
provider = "deepseek"
protocol = "deepseek"
base_url = "https://api.deepseek.com"
credential = { source = "environment", name = "SIGIL_API_KEY" }
"#,
    )?;
    let skill_path = temp.path().join(".sigil/skills/review/SKILL.md");
    std::fs::create_dir_all(skill_path.parent().expect("skill parent"))?;
    std::fs::write(
        &skill_path,
        r#"---
id: review
name: Review
description: Review the selected code.
trust: trusted
run-as: inline
user-invocable: true
---

# Review instructions
Inspect the requested code and report concrete findings.
"#,
    )?;

    let root_config = RootConfig::load(&config_path)?;
    let report = crate::discover_skill_index(temp.path(), &root_config.skills)?;
    let descriptor = report
        .snapshot
        .descriptors
        .iter()
        .find(|descriptor| descriptor.id == "review")
        .expect("review skill should be discovered");
    let mut request = ApplicationRunRequest::non_interactive(
        &config_path,
        temp.path(),
        "review src/lib.rs",
        "run-skill",
    );
    request.skill_binding = Some(crate::ApplicationSkillBinding {
        skill_id: descriptor.id.clone(),
        skill_sha256: descriptor.sha256.clone(),
        index_fingerprint: report.snapshot.fingerprint.clone(),
    });
    let store = JsonlSessionStore::new(temp.path().join("session.jsonl"))?;
    let mut session = Session::load_from_store("deepseek", "deepseek-v4-flash", store)?;

    let loaded =
        admit_application_skill_binding(&request, &root_config, temp.path(), &mut session)?
            .expect("exact binding should load");

    assert_eq!(loaded.descriptor.id, "review");
    assert!(
        loaded
            .transient_context
            .content
            .as_deref()
            .is_some_and(|content| content.contains("Review instructions"))
    );
    assert!(session.entries().iter().any(|entry| matches!(
        entry,
        SessionLogEntry::Control(ControlEntry::SkillLoaded(loaded))
            if loaded.skill_id == "review" && loaded.run_id.as_deref() == Some("run-skill")
    )));

    request
        .skill_binding
        .as_mut()
        .expect("binding should exist")
        .index_fingerprint = "stale".to_owned();
    assert!(matches!(
        admit_application_skill_binding(&request, &root_config, temp.path(), &mut session,),
        Err(ApplicationRunPrepareError::InvalidInvocation { .. })
    ));
    Ok(())
}

#[test]
fn exact_agent_binding_admits_current_snapshot_and_rejects_stale_snapshot() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let config_path = temp.path().join("sigil.toml");
    std::fs::write(
        &config_path,
        r#"config_version = 2

[workspace]
root = "."

[agent]
connection = "deepseek-default"
model = "deepseek-v4-flash"
[connections.deepseek-default]
label = "DeepSeek"
provider = "deepseek"
protocol = "deepseek"
base_url = "https://api.deepseek.com"
credential = { source = "environment", name = "SIGIL_API_KEY" }
"#,
    )?;
    let root_config = RootConfig::load(&config_path)?;
    let catalog = crate::application_extension_catalog_view(&root_config, temp.path(), &[])?;
    let agent = catalog
        .agents
        .iter()
        .find(|agent| agent.available)
        .expect("a trusted built-in agent should be available");
    let mut request = ApplicationRunRequest::non_interactive(
        &config_path,
        temp.path(),
        "inspect the workspace",
        "run-agent",
    );
    request.agent_binding = agent.binding.clone();

    let (registry, profile_id) =
        admit_application_agent_binding(&request, &root_config, temp.path(), &[])?
            .expect("exact binding should admit the agent profile");
    assert_eq!(profile_id.as_str(), agent.id);
    assert!(registry.get(&profile_id).is_some());

    request
        .agent_binding
        .as_mut()
        .expect("binding should exist")
        .snapshot_id = "stale".to_owned();
    assert!(matches!(
        admit_application_agent_binding(&request, &root_config, temp.path(), &[]),
        Err(ApplicationRunPrepareError::InvalidInvocation { .. })
    ));
    Ok(())
}

#[test]
fn explicit_reasoning_effort_requires_exact_current_binding() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let config_path = temp.path().join("sigil.toml");
    std::fs::write(
        &config_path,
        r#"config_version = 2

[workspace]
root = "."

[agent]
connection = "deepseek-default"
model = "deepseek-v4-flash"
[connections.deepseek-default]
label = "DeepSeek"
provider = "deepseek"
protocol = "deepseek"
base_url = "https://api.deepseek.com"
credential = { source = "environment", name = "SIGIL_API_KEY" }
"#,
    )?;
    let config = RootConfig::load(&config_path)?;
    let (provider_name, route) = crate::provider_connections::resolve_default_model_route(&config)?;
    let supported = crate::reasoning_effort::supported_reasoning_efforts(
        &provider_name,
        &route.model_ref.model_id,
    );
    let binding = crate::reasoning_effort::reasoning_effort_binding(
        &provider_name,
        &route.model_ref.model_id,
        &supported,
    )
    .expect("default model supports reasoning effort");
    let mut request =
        ApplicationRunRequest::non_interactive("sigil.toml", ".", "hello", "run-effort");
    request.reasoning_effort = Some(ReasoningEffort::High);
    request.reasoning_effort_binding = Some(binding);
    assert!(
        admit_application_reasoning_effort(&request, &provider_name, &route.model_ref.model_id,)
            .is_ok()
    );

    request.reasoning_effort_binding = Some("stale".to_owned());
    assert!(matches!(
        admit_application_reasoning_effort(&request, &provider_name, &route.model_ref.model_id,),
        Err(ApplicationRunPrepareError::InvalidInvocation { .. })
    ));

    request.reasoning_effort = None;
    assert!(matches!(
        admit_application_reasoning_effort(&request, &provider_name, &route.model_ref.model_id,),
        Err(ApplicationRunPrepareError::InvalidInvocation { .. })
    ));
    Ok(())
}

#[test]
fn transcript_page_is_scope_checked_chronological_bounded_and_argument_free() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let config_path = temp.path().join("sigil.toml");
    write_application_test_config(&config_path)?;
    let session_path = temp.path().join("state/sessions/transcript.jsonl");
    let binding = bind_application_session(&config_path, temp.path(), Some(&session_path))?;
    let store = JsonlSessionStore::new(&binding.session_log_path)?;
    store.append(&SessionLogEntry::User(ModelMessage::user("first")))?;
    store.append(&SessionLogEntry::Assistant(
        ModelMessage::assistant_with_kind(
            Some("checking".to_owned()),
            vec![ToolCall {
                id: "call-1".to_owned(),
                name: "read_file".to_owned(),
                args_json: "{\"token\":\"must-not-project\"}".to_owned(),
            }],
            AssistantMessageKind::ToolPreamble,
        ),
    ))?;
    let artifact_store = ToolArtifactStore::for_session_store(&store);
    let (recorded, _) = ToolResultRecordedV2::capture(
        &ToolResult::ok(
            "call-1",
            "read_file",
            "tool output",
            ToolResultMeta::default(),
        ),
        Some(&artifact_store),
        ToolArtifactSensitivity::Ordinary,
    )?;
    store.append(&SessionLogEntry::ToolResultV2(recorded))?;
    store.append(&SessionLogEntry::Assistant(
        ModelMessage::assistant_with_kind(
            Some("final".to_owned()),
            Vec::new(),
            AssistantMessageKind::FinalAnswer,
        ),
    ))?;

    let latest = application_session_transcript_page(
        &binding.session_log_path,
        &binding.session_scope_id,
        None,
        2,
    )?;
    assert_eq!(latest.total_messages, 4);
    assert_eq!(latest.messages.len(), 2);
    assert_eq!(latest.messages[0].ordinal, 3);
    assert_eq!(latest.messages[0].role, ApplicationTranscriptRole::Tool);
    assert_eq!(latest.messages[0].tool_name.as_deref(), Some("read_file"));
    assert_eq!(latest.messages[1].content.as_deref(), Some("final"));
    assert_eq!(latest.next_before, Some(3));
    assert!(!format!("{latest:?}").contains("must-not-project"));

    let older = application_session_transcript_page(
        &binding.session_log_path,
        &binding.session_scope_id,
        latest.next_before,
        2,
    )?;
    assert_eq!(
        older
            .messages
            .iter()
            .map(|message| message.ordinal)
            .collect::<Vec<_>>(),
        vec![1, 2]
    );
    assert_eq!(older.next_before, None);
    assert!(
        application_session_transcript_page(&binding.session_log_path, "wrong-scope", None, 10)
            .is_err()
    );
    Ok(())
}

#[test]
fn transcript_page_projects_durable_reasoning_notes_without_other_control_data() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let config_path = temp.path().join("sigil.toml");
    write_application_test_config(&config_path)?;
    let session_path = temp
        .path()
        .join("state/sessions/reasoning-transcript.jsonl");
    let binding = bind_application_session(&config_path, temp.path(), Some(&session_path))?;
    let store = JsonlSessionStore::new(&binding.session_log_path)?;
    store.append(&SessionLogEntry::User(ModelMessage::user("inspect")))?;
    store.append(&SessionLogEntry::Control(ControlEntry::Note {
        kind: "reasoning_trace".to_owned(),
        data: serde_json::json!({ "text": "checking the durable path" }),
    }))?;
    store.append(&SessionLogEntry::Control(ControlEntry::Note {
        kind: "internal_only".to_owned(),
        data: serde_json::json!({ "text": "must not project" }),
    }))?;
    store.append(&SessionLogEntry::Assistant(
        ModelMessage::assistant_with_kind(
            Some("done".to_owned()),
            Vec::new(),
            AssistantMessageKind::FinalAnswer,
        ),
    ))?;

    let page = application_session_transcript_page(
        &binding.session_log_path,
        &binding.session_scope_id,
        None,
        10,
    )?;

    assert_eq!(page.total_messages, 3);
    assert_eq!(page.messages[1].role, ApplicationTranscriptRole::Assistant);
    assert_eq!(
        page.messages[1].assistant_kind,
        Some(AssistantMessageKind::ReasoningTrace)
    );
    assert_eq!(
        page.messages[1].content.as_deref(),
        Some("checking the durable path")
    );
    assert!(!format!("{page:?}").contains("must not project"));
    Ok(())
}

#[test]
fn transcript_page_truncates_utf8_content_without_breaking_character_boundaries() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let config_path = temp.path().join("sigil.toml");
    write_application_test_config(&config_path)?;
    let session_path = temp.path().join("state/sessions/large-transcript.jsonl");
    let binding = bind_application_session(&config_path, temp.path(), Some(&session_path))?;
    let store = JsonlSessionStore::new(&binding.session_log_path)?;
    let original = "界".repeat(MAX_APPLICATION_TRANSCRIPT_MESSAGE_BYTES);
    store.append(&SessionLogEntry::User(ModelMessage::user(&original)))?;

    let page = application_session_transcript_page(
        &binding.session_log_path,
        &binding.session_scope_id,
        None,
        1,
    )?;
    let message = &page.messages[0];
    let content = message.content.as_deref().expect("text remains available");
    assert!(message.truncated);
    assert_eq!(message.original_content_bytes, original.len() as u64);
    assert!(content.len() <= MAX_APPLICATION_TRANSCRIPT_MESSAGE_BYTES);
    assert!(content.is_char_boundary(content.len()));
    Ok(())
}

#[test]
fn preparation_cancellation_is_durable_idempotent_and_secret_safe() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let config_path = temp.path().join("sigil.toml");
    write_application_test_config(&config_path)?;
    let session_path = temp.path().join("state/sessions/http.jsonl");
    let binding = bind_application_session(&config_path, temp.path(), Some(&session_path))?;

    let first = record_application_preparation_cancellation(
        &config_path,
        &binding.session_log_path,
        "run-1",
        "stop token=super-secret",
    )?;
    let second = record_application_preparation_cancellation(
        &config_path,
        &binding.session_log_path,
        "run-1",
        "stop token=super-secret",
    )?;

    assert_eq!(first, binding);
    assert_eq!(second, binding);
    let durable = std::fs::read_to_string(&binding.session_log_path)?;
    assert_eq!(durable.matches("cancel-preparation-run-1").count(), 2);
    assert!(durable.contains("\"outcome\":\"cancelled\""));
    assert!(durable.contains("token=[redacted]"));
    assert!(!durable.contains("super-secret"));
    Ok(())
}

#[test]
fn interaction_contract_distinguishes_noninteractive_and_external_surfaces() {
    assert_eq!(
        ApplicationRunInteraction::NonInteractive.kernel_mode(),
        sigil_kernel::InteractionMode::Headless
    );
    assert_eq!(
        ApplicationRunInteraction::AdapterManaged.kernel_mode(),
        sigil_kernel::InteractionMode::Interactive
    );
    assert_eq!(
        ApplicationRunInteraction::ExternallyInteractive.kernel_mode(),
        sigil_kernel::InteractionMode::Interactive
    );
}

#[test]
fn prepare_error_class_is_typed_and_public_message_does_not_expose_source() {
    let error = ApplicationRunPrepareError::configuration(anyhow::anyhow!(
        "secret provider value must remain in the source chain"
    ));

    assert_eq!(
        error.class(),
        ApplicationRunPrepareErrorClass::Configuration
    );
    assert_eq!(error.to_string(), "application configuration is invalid");
    assert!(!error.to_string().contains("secret provider value"));
}

#[test]
fn optional_eager_mcp_warning_redacts_known_and_structural_secret_carriers() {
    let redactor = sigil_kernel::SecretRedactor::from_values(["known-secret-value"]);
    let error =
        anyhow::anyhow!("Authorization: Bearer known-secret-value; api_key=another-secret-value");

    let warning = optional_eager_mcp_warning(&redactor, "optional-server", &error);

    assert!(warning.contains("optional eager MCP server optional-server failed"));
    assert!(!warning.contains("known-secret-value"));
    assert!(!warning.contains("another-secret-value"));
    assert!(warning.contains("[redacted]"));
}

struct ExplicitApprovalHandler;

impl ApprovalHandler for ExplicitApprovalHandler {
    fn approve_tool_call(&mut self, _call: &ToolCall, _spec: &ToolSpec) -> Result<ToolApproval> {
        Ok(ToolApproval::Approve)
    }

    fn approval_is_explicit_user_action(&self) -> bool {
        true
    }
}

#[test]
fn externally_interactive_runs_reject_automated_approval_handlers() {
    assert!(
        validate_execution_contract(
            ApplicationRunInteraction::AdapterManaged,
            &AutoApproveHandler,
            true,
        )
        .is_ok()
    );
    assert!(
        validate_execution_contract(
            ApplicationRunInteraction::AdapterManaged,
            &AutoApproveHandler,
            false,
        )
        .is_err()
    );
    assert!(
        validate_execution_contract(
            ApplicationRunInteraction::ExternallyInteractive,
            &AutoApproveHandler,
            true,
        )
        .is_err()
    );
    assert!(
        validate_execution_contract(
            ApplicationRunInteraction::ExternallyInteractive,
            &ExplicitApprovalHandler,
            false,
        )
        .is_err()
    );
    assert!(
        validate_execution_contract(
            ApplicationRunInteraction::ExternallyInteractive,
            &ExplicitApprovalHandler,
            true,
        )
        .is_ok()
    );
}

#[test]
fn public_event_bridge_sequences_lifecycle_and_kernel_events() -> Result<()> {
    #[derive(Default)]
    struct Recorder(Vec<PublicRunEvent>);

    impl ApplicationRunEventHandler for Recorder {
        fn handle_public_event(&mut self, event: PublicRunEvent) -> Result<()> {
            self.0.push(event);
            Ok(())
        }
    }

    let mut recorder = Recorder::default();
    let events = ApplicationRunEventSequence::new("session-1".to_owned(), "run-1".to_owned());
    let mut bridge = PublicApplicationEventBridge::new(events, &mut recorder);
    bridge.emit(PublicRunEventKind::RunStarted {
        prompt: "hello".to_owned(),
    })?;
    sigil_kernel::EventHandler::handle(&mut bridge, RunEvent::TextDelta("hi".to_owned()))?;
    bridge.emit(PublicRunEventKind::RunFinished {
        final_text: "hi".to_owned(),
    })?;
    drop(bridge);

    assert_eq!(
        recorder
            .0
            .iter()
            .map(|event| event.sequence)
            .collect::<Vec<_>>(),
        vec![1, 2, 3]
    );
    assert!(matches!(
        recorder.0[0].event,
        PublicRunEventKind::RunStarted { .. }
    ));
    assert!(matches!(
        recorder.0[1].event,
        PublicRunEventKind::TextDelta { .. }
    ));
    assert!(matches!(
        recorder.0[2].event,
        PublicRunEventKind::RunFinished { .. }
    ));
    Ok(())
}

#[test]
fn public_event_bridge_projects_task_controls_and_preserves_unknown_controls() -> Result<()> {
    #[derive(Default)]
    struct Recorder(Vec<PublicRunEvent>);

    impl ApplicationRunEventHandler for Recorder {
        fn handle_public_event(&mut self, event: PublicRunEvent) -> Result<()> {
            self.0.push(event);
            Ok(())
        }
    }

    let mut recorder = Recorder::default();
    let events = ApplicationRunEventSequence::new("session-1".to_owned(), "run-1".to_owned());
    let mut bridge = PublicApplicationEventBridge::new(events, &mut recorder);
    sigil_kernel::EventHandler::handle(
        &mut bridge,
        RunEvent::Control(ControlEntry::TaskRun(TaskRunEntry {
            task_id: TaskId::new("task-1")?,
            parent_session_ref: sigil_kernel::SessionRef::new_relative("parent.jsonl")?,
            objective: "private task objective".to_owned(),
            status: TaskRunStatus::Running,
            reason: None,
        })),
    )?;
    sigil_kernel::EventHandler::handle(
        &mut bridge,
        RunEvent::Control(ControlEntry::Note {
            kind: "diagnostic".to_owned(),
            data: serde_json::json!({"value": 1}),
        }),
    )?;
    drop(bridge);

    assert!(matches!(
        recorder.0[0].event,
        PublicRunEventKind::TaskPhaseChanged {
            task_id: Some(ref task_id),
            ref status,
            ..
        } if task_id == "task-1" && status == "running"
    ));
    assert!(matches!(
        recorder.0[1].event,
        PublicRunEventKind::Control { .. }
    ));
    assert_eq!(
        recorder
            .0
            .iter()
            .map(|event| event.sequence)
            .collect::<Vec<_>>(),
        vec![1, 2]
    );
    Ok(())
}

#[test]
fn public_event_sequence_seals_after_root_terminal() -> Result<()> {
    #[derive(Default)]
    struct Recorder(Vec<PublicRunEvent>);

    impl ApplicationRunEventHandler for Recorder {
        fn handle_public_event(&mut self, event: PublicRunEvent) -> Result<()> {
            self.0.push(event);
            Ok(())
        }
    }

    let sequence = ApplicationRunEventSequence::new("session-1".to_owned(), "run-1".to_owned());
    let mut recorder = Recorder::default();
    sequence.emit(
        &mut recorder,
        PublicRunEventKind::RunStarted {
            prompt: "hello".to_owned(),
        },
    )?;
    sequence.emit(
        &mut recorder,
        PublicRunEventKind::RunFailed {
            error: "interrupted".to_owned(),
        },
    )?;
    assert!(
        sequence
            .emit(
                &mut recorder,
                PublicRunEventKind::TextDelta {
                    text: "late".to_owned(),
                },
            )
            .is_err()
    );
    assert_eq!(recorder.0.len(), 2);
    Ok(())
}

#[test]
fn failed_terminal_delivery_does_not_seal_the_public_event_sequence() -> Result<()> {
    struct FailFirstTerminal {
        failed: bool,
        events: Vec<PublicRunEvent>,
    }

    impl ApplicationRunEventHandler for FailFirstTerminal {
        fn handle_public_event(&mut self, event: PublicRunEvent) -> Result<()> {
            if !self.failed && matches!(event.event, PublicRunEventKind::RunFailed { .. }) {
                self.failed = true;
                anyhow::bail!("durable publication failed");
            }
            self.events.push(event);
            Ok(())
        }
    }

    let sequence = ApplicationRunEventSequence::new("session-1".to_owned(), "run-1".to_owned());
    let mut handler = FailFirstTerminal {
        failed: false,
        events: Vec::new(),
    };
    assert!(
        sequence
            .emit(
                &mut handler,
                PublicRunEventKind::RunFailed {
                    error: "first terminal".to_owned(),
                },
            )
            .is_err()
    );
    sequence.emit(
        &mut handler,
        PublicRunEventKind::RunFailed {
            error: "retry terminal".to_owned(),
        },
    )?;

    assert_eq!(handler.events.len(), 1);
    assert_eq!(handler.events[0].sequence, 1);
    Ok(())
}

#[test]
fn non_final_kernel_terminals_do_not_project_as_run_finished() {
    for (terminal_reason, expected_status) in [
        (
            AgentRunTerminalReason::MaxTurns,
            ApplicationRunTerminalStatus::Interrupted,
        ),
        (
            AgentRunTerminalReason::DelegationUnsatisfied,
            ApplicationRunTerminalStatus::Blocked,
        ),
    ] {
        let output = AgentRunOutput {
            disposition: match terminal_reason {
                AgentRunTerminalReason::MaxTurns => AgentRunDisposition::Interrupted,
                AgentRunTerminalReason::DelegationUnsatisfied => AgentRunDisposition::Blocked,
                other => panic!("unexpected non-final terminal reason: {other:?}"),
            },
            result: AgentRunResult {
                final_text: String::new(),
                tool_calls: 0,
                final_message_id: None,
            },
            outcome: AgentRunOutcome {
                terminal_reason,
                ..AgentRunOutcome::default()
            },
        };
        let (status, event) = application_terminal_projection(&output);

        assert_eq!(status, expected_status);
        assert!(matches!(event, PublicRunEventKind::RunFailed { .. }));
    }
}

#[test]
fn durable_task_handoff_never_projects_as_application_success() -> Result<()> {
    let task_id = TaskId::new("task-application-handoff")?;
    let output = AgentRunOutput {
        disposition: AgentRunDisposition::StartDurableTask(StartDurableTaskAction {
            handoff_id: TaskHandoffId::new("handoff-application")?,
            task_id: task_id.clone(),
            source_turn: sigil_kernel::ConversationTurnRef::new(
                "session-application",
                "message-application",
                "run-application",
            )?,
        }),
        result: AgentRunResult {
            final_text: String::new(),
            tool_calls: 1,
            final_message_id: None,
        },
        outcome: AgentRunOutcome {
            terminal_reason: AgentRunTerminalReason::TaskHandoff,
            ..AgentRunOutcome::default()
        },
    };

    let (status, event) = application_terminal_projection(&output);
    assert_eq!(status, ApplicationRunTerminalStatus::Blocked);
    assert!(matches!(event, PublicRunEventKind::RunFailed { .. }));
    Ok(())
}

#[tokio::test]
async fn application_auto_routing_stays_manual_without_attached_task_executor() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let config_path = temp.path().join("sigil.toml");
    std::fs::write(
        &config_path,
        r#"config_version = 2

[workspace]
root = "."

[agent]
connection = "deepseek-default"
model = "deepseek-v4-flash"

[task]
routing_policy = "auto"

[connections.deepseek-default]
label = "DeepSeek"
provider = "deepseek"
protocol = "deepseek"
base_url = "https://api.deepseek.com"
credential = { source = "environment", name = "SIGIL_API_KEY" }
"#,
    )?;
    let request = ApplicationRunRequest::non_interactive(
        &config_path,
        temp.path(),
        "implement the cross-layer feature",
        "run-application-without-task-executor",
    );
    let services = ApplicationRunServices::new(Arc::new(RejectingDisclosurePresenter));

    let prepared = prepare_application_run(request, &services).await?;

    assert!(!services.task_executor_attached());
    assert!(prepared.execution.task_execution.is_none());
    let ApplicationRunExecutionKind::Main { input, .. } = &prepared.execution.kind else {
        panic!("ordinary application request must prepare the main agent");
    };
    let Some(AgentRunPurpose::Conversation(context)) = input.purpose.as_ref() else {
        panic!("ordinary application request must carry conversation purpose");
    };
    assert_eq!(context.routing_policy, TaskRoutingPolicy::Manual);
    assert!(context.task_handoff.is_none());
    Ok(())
}

#[tokio::test]
async fn application_preparation_enables_model_owned_auto_handoff_without_host_classification()
-> Result<()> {
    let temp = tempfile::tempdir()?;
    let config_path = temp.path().join("sigil.toml");
    std::fs::write(
        &config_path,
        r#"config_version = 2

[workspace]
root = "."

[agent]
connection = "deepseek-default"
model = "deepseek-v4-flash"

[task]
routing_policy = "auto"

[connections.deepseek-default]
label = "DeepSeek"
provider = "deepseek"
protocol = "deepseek"
base_url = "https://api.deepseek.com"
credential = { source = "environment", name = "SIGIL_API_KEY" }
"#,
    )?;
    let request = ApplicationRunRequest::non_interactive(
        &config_path,
        temp.path(),
        "implement the cross-layer feature",
        "run-application-auto-handoff",
    );
    let services = ApplicationRunServices::new(Arc::new(RejectingDisclosurePresenter))
        .with_task_role_provider_builder(Arc::new(ApplicationTaskRoleProviderBuilder));

    let prepared = prepare_application_run(request, &services).await?;

    assert!(services.task_executor_attached());
    assert!(prepared.execution.task_execution.is_some());
    let ApplicationRunExecutionKind::Main { input, .. } = &prepared.execution.kind else {
        panic!("ordinary application request must prepare the main agent");
    };
    let Some(AgentRunPurpose::Conversation(context)) = input.purpose.as_ref() else {
        panic!("ordinary application request must carry conversation purpose");
    };
    assert_eq!(context.routing_policy, TaskRoutingPolicy::Auto);
    assert!(
        context.task_handoff.is_some(),
        "auto routing exposes typed handoff authority to the model"
    );
    assert!(
        prepared
            .execution
            .session
            .task_state_projection()
            .tasks
            .is_empty(),
        "host preparation must not infer prompt intent or create a task before the model tool call"
    );
    Ok(())
}

#[tokio::test]
async fn application_task_handoff_runs_shared_executor_and_returns_synthesis() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let config_path = temp.path().join("sigil.toml");
    std::fs::write(
        &config_path,
        r#"config_version = 2

[workspace]
root = "."

[agent]
connection = "application-task-test"
model = "application-task-model"

[connections.application-task-test]
label = "Application task test"
provider = "custom"
protocol = "chat_completions"
base_url = "http://127.0.0.1:11434/v1"
credential = { source = "none" }
"#,
    )?;
    let mut root_config = RootConfig::load(&config_path)?;
    root_config.task.enabled = true;
    root_config.task.routing_policy = TaskRoutingPolicy::Auto;
    let session_path = temp.path().join("session.jsonl");
    let store = JsonlSessionStore::new(&session_path)?;
    let mut session =
        Session::load_from_store("application-task-test", "application-task-model", store)?;
    let task_id = TaskId::new("task-application-execution")?;
    let parent_session_ref = SessionRef::new_relative("session.jsonl")?;
    session.append_control(ControlEntry::TaskRun(TaskRunEntry {
        task_id: task_id.clone(),
        parent_session_ref: parent_session_ref.clone(),
        objective: "inspect the application runtime".to_owned(),
        status: TaskRunStatus::Started,
        reason: Some("accepted by the application conversation coordinator".to_owned()),
    }))?;
    let profile_registry =
        crate::AgentProfileRegistry::from_root_config_with_workspace_and_entries(
            &root_config,
            temp.path(),
            session.entries(),
        )?;
    let task_execution = ApplicationTaskExecutionRuntime {
        root_config: root_config.clone(),
        options: crate::build_run_options(
            &root_config,
            temp.path().to_path_buf(),
            InteractionMode::Headless,
        ),
        base_registry: ToolRegistry::new(),
        agent_supervisor: crate::AgentSupervisor::new(
            profile_registry,
            crate::AgentBudgetPolicy::from_root_config(&root_config),
            application_task_provider_capabilities(),
        ),
        role_provider_builder: Arc::new(ApplicationTaskRoleProviderBuilder),
    };
    let cancellation_owner = RunCancellationOwner::new();
    let cancellation_handle = cancellation_owner.handle();
    let action = StartDurableTaskAction {
        handoff_id: TaskHandoffId::new("handoff-application-execution")?,
        task_id: task_id.clone(),
        source_turn: sigil_kernel::ConversationTurnRef::new(
            session.session_scope_id(),
            "message-application-execution",
            "run-application-execution",
        )?,
    };
    let root_output = AgentRunOutput {
        disposition: AgentRunDisposition::StartDurableTask(action),
        result: AgentRunResult {
            final_text: String::new(),
            tool_calls: 1,
            final_message_id: None,
        },
        outcome: AgentRunOutcome {
            terminal_reason: AgentRunTerminalReason::TaskHandoff,
            tool_calls: 1,
            ..AgentRunOutcome::default()
        },
    };
    let mut handler = NoopEventHandler;
    let mut approval_handler = AutoApproveHandler;

    let output = continue_application_task_handoff(
        &mut session,
        root_output,
        Some(task_execution),
        &mut handler,
        &mut approval_handler,
        &cancellation_handle,
    )
    .await?;

    assert_eq!(output.disposition, AgentRunDisposition::FinalAnswer);
    assert_eq!(
        output.result.final_text,
        "application durable task completed"
    );
    assert!(output.result.final_message_id.is_some());
    assert!(cancellation_handle.is_naturally_finalized());
    let projection = session.task_state_projection();
    let task = projection.tasks.get(&task_id).expect("task should exist");
    assert_eq!(task.status, TaskRunStatus::Completed);
    assert_eq!(
        task.final_answer
            .as_ref()
            .map(|answer| answer.message_id.as_str()),
        output.result.final_message_id.as_deref()
    );
    Ok(())
}

#[tokio::test]
async fn application_task_continuation_reopens_exact_task_and_returns_synthesis() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let config_path = temp.path().join("sigil.toml");
    write_unauthenticated_application_test_config(&config_path)?;
    let session_path = temp.path().join("session.jsonl");
    let store = JsonlSessionStore::new(&session_path)?;
    let root_config = RootConfig::load(&config_path)?;
    let (provider_name, route) =
        crate::provider_connections::resolve_default_model_route(&root_config)
            .map_err(anyhow::Error::new)?;
    let mut session = Session::load_from_store_with_route(
        provider_name,
        route.model_ref.model_id.clone(),
        Some(route),
        store,
    )?;
    let task_id = TaskId::new("task-application-continuation")?;
    session.append_control(ControlEntry::TaskRun(TaskRunEntry {
        task_id: task_id.clone(),
        parent_session_ref: SessionRef::new_relative("session.jsonl")?,
        objective: "continue the application task".to_owned(),
        status: TaskRunStatus::Paused,
        reason: Some("application restart".to_owned()),
    }))?;
    let session_scope_id = session.session_scope_id().to_owned();
    drop(session);
    let services = ApplicationRunServices::new(Arc::new(RejectingDisclosurePresenter))
        .with_task_role_provider_builder(Arc::new(ApplicationTaskRoleProviderBuilder));
    let prepared = prepare_application_task_continuation(
        ApplicationTaskContinuationRequest {
            config_path,
            launch_cwd: temp.path().to_path_buf(),
            session_path: session_path.clone(),
            session_attachment: None,
            expected_session_scope_id: session_scope_id.clone(),
            run_id: "run-application-task-continuation".to_owned(),
            task_id: task_id.clone(),
            guidance: None,
            interaction: ApplicationRunInteraction::NonInteractive,
            permission_mode: None,
        },
        &services,
    )
    .await?;
    assert_eq!(prepared.task_id(), &task_id);
    assert_eq!(prepared.session_id(), session_scope_id);
    assert_eq!(
        prepared.session_log_path(),
        std::fs::canonicalize(&session_path)?.as_path()
    );
    let (execution, control) = prepared.into_parts();
    assert_eq!(
        control.cancellation_target,
        RunCancellationTarget::Task {
            task_id: task_id.as_str().to_owned()
        }
    );
    #[derive(Default)]
    struct Recorder(Vec<PublicRunEvent>);

    impl ApplicationRunEventHandler for Recorder {
        fn handle_public_event(&mut self, event: PublicRunEvent) -> Result<()> {
            self.0.push(event);
            Ok(())
        }
    }
    let mut events = Recorder::default();
    let mut approval_handler = AutoApproveHandler;

    let output = execution
        .execute(&mut events, &mut approval_handler)
        .await?;

    assert_eq!(output.session_id, session_scope_id);
    assert_eq!(output.task_id, task_id);
    assert_eq!(output.task_status, TaskRunStatus::Completed);
    assert_eq!(
        output.terminal_status,
        ApplicationRunTerminalStatus::Succeeded
    );
    assert_eq!(
        output.final_text.as_deref(),
        Some("application durable task completed")
    );
    assert!(matches!(
        events.0.first().map(|event| &event.event),
        Some(PublicRunEventKind::RunStarted { .. })
    ));
    assert!(
        events
            .0
            .iter()
            .any(|event| matches!(event.event, PublicRunEventKind::TaskPlanUpdated { .. }))
    );
    assert!(matches!(
        events.0.last().map(|event| &event.event),
        Some(PublicRunEventKind::RunFinished { final_text })
            if final_text == "application durable task completed"
    ));
    assert!(control.handle().is_naturally_finalized());
    let lifecycle = application_conversation_lifecycle(&session_path)?;
    assert!(matches!(
        lifecycle.as_slice(),
        [
            ConversationRunLifecycleRecordV1::ConversationRunStartedV1(_),
            ConversationRunLifecycleRecordV1::ConversationRunFinalizedV1(finalized),
        ] if finalized.status() == ConversationRunTerminalStatusV1::Succeeded
    ));
    let reopened = Session::load_from_store(
        "deepseek",
        "deepseek-v4-flash",
        JsonlSessionStore::new(&session_path)?,
    )?;
    assert!(
        reopened
            .entries()
            .iter()
            .all(|entry| !matches!(entry, SessionLogEntry::User(_))),
        "Task continuation must not synthesize a user conversation prompt"
    );
    Ok(())
}

#[tokio::test]
async fn application_task_continuation_rejects_stale_scope_without_mutation() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let config_path = temp.path().join("sigil.toml");
    std::fs::write(
        &config_path,
        r#"config_version = 2

[workspace]
root = "."

[agent]
connection = "deepseek-default"
model = "deepseek-v4-flash"

[task]
enabled = true

[connections.deepseek-default]
label = "DeepSeek"
provider = "deepseek"
protocol = "deepseek"
base_url = "https://api.deepseek.com"
credential = { source = "environment", name = "SIGIL_API_KEY" }
"#,
    )?;
    let session_path = temp.path().join("session.jsonl");
    let store = JsonlSessionStore::new(&session_path)?;
    let mut session = Session::load_from_store("deepseek", "deepseek-v4-flash", store)?;
    let task_id = TaskId::new("task-stale-application-continuation")?;
    session.append_control(ControlEntry::TaskRun(TaskRunEntry {
        task_id: task_id.clone(),
        parent_session_ref: SessionRef::new_relative("session.jsonl")?,
        objective: "reject stale continuation".to_owned(),
        status: TaskRunStatus::Paused,
        reason: None,
    }))?;
    drop(session);
    let before = std::fs::read(&session_path)?;
    let services = ApplicationRunServices::new(Arc::new(RejectingDisclosurePresenter))
        .with_task_role_provider_builder(Arc::new(ApplicationTaskRoleProviderBuilder));

    let error = match prepare_application_task_continuation(
        ApplicationTaskContinuationRequest {
            config_path,
            launch_cwd: temp.path().to_path_buf(),
            session_path: session_path.clone(),
            session_attachment: None,
            expected_session_scope_id: "stale-session-scope".to_owned(),
            run_id: "run-stale-task-continuation".to_owned(),
            task_id,
            guidance: None,
            interaction: ApplicationRunInteraction::NonInteractive,
            permission_mode: None,
        },
        &services,
    )
    .await
    {
        Ok(_) => panic!("stale session scope must reject Task continuation"),
        Err(error) => error,
    };

    assert_eq!(error.class(), ApplicationRunPrepareErrorClass::Execution);
    assert_eq!(std::fs::read(&session_path)?, before);
    Ok(())
}

#[tokio::test]
async fn application_task_pause_writes_paused_only_after_quiescence() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let store_path = temp.path().join("session.jsonl");
    let store = JsonlSessionStore::new(&store_path)?;
    let mut session = Session::load_from_store("deepseek", "model", store)?;
    let recorder = session.run_cancellation_recorder()?;
    let owner = RunCancellationOwner::new();
    let handle = owner.handle();
    let root_task_guard = handle.register_task()?;
    let task_id = TaskId::new("task-application-pause")?;
    append_running_application_task(&mut session, &task_id, Some(handle.scope_id()), 1)?;
    let control = ApplicationRunControl {
        owner,
        recorder,
        cancellation_target: RunCancellationTarget::Task {
            task_id: task_id.as_str().to_owned(),
        },
        conversation_lifecycle: session.conversation_run_lifecycle_recorder()?,
        conversation_start: ConversationRunStartedEntryV1::new("run-task-pause", 1)?,
        events: ApplicationRunEventSequence::new(
            session.session_scope_id().to_owned(),
            "run-task-pause".to_owned(),
        ),
        _session_lease: Arc::new(ApplicationSessionLeaseManager::new().acquire(&store_path)?),
    };
    #[derive(Default)]
    struct Recorder(Vec<PublicRunEvent>);

    impl ApplicationRunEventHandler for Recorder {
        fn handle_public_event(&mut self, event: PublicRunEvent) -> Result<()> {
            self.0.push(event);
            Ok(())
        }
    }
    let mut events = Recorder::default();
    let pause_request = TaskPauseRequest::new(task_id.clone(), 1);
    let pause_action_id = pause_request.request_id.clone();
    let ticket = control.request_task_pause(pause_request, None, || {})?;
    assert_ne!(ticket.cancellation.request.request_id, pause_action_id);
    assert!(
        ticket
            .cancellation
            .request
            .request_id
            .starts_with(&pause_action_id)
    );
    assert!(
        ticket
            .cancellation
            .request
            .request_id
            .ends_with(handle.scope_id())
    );
    assert!(control.handle().is_cancel_requested());
    drop(root_task_guard);

    let outcome = control
        .finalize_task_pause(ticket, true, &mut events)
        .await?;

    assert_eq!(outcome.task_id, task_id);
    assert_eq!(outcome.task_status, TaskRunStatus::Paused);
    assert_eq!(
        outcome.cancellation_outcome,
        RunCancellationTerminalOutcome::Cancelled
    );
    assert!(events.0.iter().any(|event| {
        matches!(
            &event.event,
            PublicRunEventKind::TaskRunFinished { task_id, status }
                if task_id == "task-application-pause" && status == "paused"
        )
    }));
    assert!(matches!(
        events.0.last().map(|event| &event.event),
        Some(PublicRunEventKind::RunCancelled)
    ));
    let reopened =
        Session::load_from_store("deepseek", "model", JsonlSessionStore::new(&store_path)?)?;
    let task = reopened
        .task_state_projection()
        .tasks
        .get(&outcome.task_id)
        .cloned()
        .expect("paused Task projection");
    assert_eq!(task.status, TaskRunStatus::Paused);
    assert!(
        task.active_steps.is_empty(),
        "paused Task must close active steps"
    );
    let lifecycle = application_conversation_lifecycle(&store_path)?;
    assert!(matches!(
        lifecycle.as_slice(),
        [
            ConversationRunLifecycleRecordV1::ConversationRunStartedV1(_),
            ConversationRunLifecycleRecordV1::ConversationRunFinalizedV1(finalized),
        ] if finalized.status() == ConversationRunTerminalStatusV1::Blocked
    ));
    Ok(())
}

#[tokio::test]
async fn stale_application_task_pause_does_not_activate_cancellation() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let store_path = temp.path().join("session.jsonl");
    let store = JsonlSessionStore::new(&store_path)?;
    let mut session = Session::load_from_store("deepseek", "model", store)?;
    let recorder = session.run_cancellation_recorder()?;
    let owner = RunCancellationOwner::new();
    let handle = owner.handle();
    let _root_task_guard = handle.register_task()?;
    let task_id = TaskId::new("task-application-stale-pause")?;
    append_running_application_task(&mut session, &task_id, Some(handle.scope_id()), 1)?;
    session.append_control(ControlEntry::TaskPlan(TaskPlanEntry {
        task_id: task_id.clone(),
        plan_version: 2,
        status: TaskPlanStatus::Accepted,
        steps: Vec::new(),
        reason: Some("replanned before pause".to_owned()),
    }))?;
    let control = ApplicationRunControl {
        owner,
        recorder,
        cancellation_target: RunCancellationTarget::Task {
            task_id: task_id.as_str().to_owned(),
        },
        conversation_lifecycle: session.conversation_run_lifecycle_recorder()?,
        conversation_start: ConversationRunStartedEntryV1::new("run-task-stale-pause", 1)?,
        events: ApplicationRunEventSequence::new(
            session.session_scope_id().to_owned(),
            "run-task-stale-pause".to_owned(),
        ),
        _session_lease: Arc::new(ApplicationSessionLeaseManager::new().acquire(&store_path)?),
    };

    let error = control
        .request_task_pause(TaskPauseRequest::new(task_id, 1), None, || {})
        .expect_err("stale rendered pause action must fail closed");

    assert!(error.to_string().contains("binding is stale"));
    assert!(error.into_ticket().is_none());
    assert!(
        !control.handle().is_cancel_requested(),
        "stale pause must not reserve or activate cancellation"
    );
    Ok(())
}

#[tokio::test]
async fn unaudited_application_task_pause_records_interrupted_before_failing() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let store_path = temp.path().join("session.jsonl");
    let store = JsonlSessionStore::new(&store_path)?;
    let mut session = Session::load_from_store("deepseek", "model", store)?;
    let recorder = session.run_cancellation_recorder()?;
    let owner = RunCancellationOwner::new();
    let handle = owner.handle();
    let root_task_guard = handle.register_task()?;
    let task_id = TaskId::new("task-application-unaudited-pause")?;
    append_running_application_task(&mut session, &task_id, Some(handle.scope_id()), 1)?;
    let pause_request = TaskPauseRequest::new(task_id.clone(), 1);
    let cancellation_request = RunCancellationRequestedEntry {
        request_id: pause_request.request_id.clone(),
        run_scope_id: handle.scope_id().to_owned(),
        target: RunCancellationTarget::Task {
            task_id: task_id.as_str().to_owned(),
        },
        reason: "pause audit append failed".to_owned(),
        requested_at_ms: 1,
        quiescence_deadline_ms: 2,
    };
    assert!(owner.reserve_cancel());
    assert!(owner.activate_reserved_cancel());
    let control = ApplicationRunControl {
        owner,
        recorder,
        cancellation_target: cancellation_request.target.clone(),
        conversation_lifecycle: session.conversation_run_lifecycle_recorder()?,
        conversation_start: ConversationRunStartedEntryV1::new("run-task-unaudited-pause", 1)?,
        events: ApplicationRunEventSequence::new(
            session.session_scope_id().to_owned(),
            "run-task-unaudited-pause".to_owned(),
        ),
        _session_lease: Arc::new(ApplicationSessionLeaseManager::new().acquire(&store_path)?),
    };
    let ticket = ApplicationTaskPauseTicket {
        request: pause_request,
        cancellation: ApplicationCancellationTicket {
            request: cancellation_request,
            deadline: std::time::Instant::now() + std::time::Duration::from_secs(1),
            request_recorded: false,
            conversation_start_recorded: false,
        },
    };
    drop(root_task_guard);
    #[derive(Default)]
    struct Recorder(Vec<PublicRunEvent>);

    impl ApplicationRunEventHandler for Recorder {
        fn handle_public_event(&mut self, event: PublicRunEvent) -> Result<()> {
            self.0.push(event);
            Ok(())
        }
    }
    let mut events = Recorder::default();

    assert!(
        control
            .finalize_task_pause(ticket, true, &mut events)
            .await
            .is_err()
    );

    assert!(events.0.iter().any(|event| {
        matches!(
            &event.event,
            PublicRunEventKind::TaskRunFinished { task_id, status }
                if task_id == "task-application-unaudited-pause" && status == "interrupted"
        )
    }));
    assert!(matches!(
        events.0.last().map(|event| &event.event),
        Some(PublicRunEventKind::RunFailed { .. })
    ));
    let reopened =
        Session::load_from_store("deepseek", "model", JsonlSessionStore::new(&store_path)?)?;
    assert_eq!(
        reopened
            .task_state_projection()
            .tasks
            .get(&task_id)
            .expect("unaudited paused Task")
            .status,
        TaskRunStatus::Interrupted
    );
    Ok(())
}

#[tokio::test]
async fn application_task_pause_records_interrupted_when_execution_did_not_join() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let store_path = temp.path().join("session.jsonl");
    let store = JsonlSessionStore::new(&store_path)?;
    let mut session = Session::load_from_store("deepseek", "model", store)?;
    let recorder = session.run_cancellation_recorder()?;
    let owner = RunCancellationOwner::new();
    let handle = owner.handle();
    let root_task_guard = handle.register_task()?;
    let task_id = TaskId::new("task-application-pause-interrupted")?;
    append_running_application_task(&mut session, &task_id, Some(handle.scope_id()), 1)?;
    let control = ApplicationRunControl {
        owner,
        recorder,
        cancellation_target: RunCancellationTarget::Task {
            task_id: task_id.as_str().to_owned(),
        },
        conversation_lifecycle: session.conversation_run_lifecycle_recorder()?,
        conversation_start: ConversationRunStartedEntryV1::new("run-task-pause-interrupted", 1)?,
        events: ApplicationRunEventSequence::new(
            session.session_scope_id().to_owned(),
            "run-task-pause-interrupted".to_owned(),
        ),
        _session_lease: Arc::new(ApplicationSessionLeaseManager::new().acquire(&store_path)?),
    };
    let ticket = control.request_task_pause(
        TaskPauseRequest::new(task_id.clone(), 1),
        Some(std::time::Duration::from_millis(10)),
        || {},
    )?;
    drop(root_task_guard);
    #[derive(Default)]
    struct Recorder(Vec<PublicRunEvent>);

    impl ApplicationRunEventHandler for Recorder {
        fn handle_public_event(&mut self, event: PublicRunEvent) -> Result<()> {
            self.0.push(event);
            Ok(())
        }
    }
    let mut events = Recorder::default();

    let outcome = control
        .finalize_task_pause(ticket, false, &mut events)
        .await?;

    assert_eq!(outcome.task_status, TaskRunStatus::Interrupted);
    assert_eq!(
        outcome.cancellation_outcome,
        RunCancellationTerminalOutcome::Interrupted
    );
    assert!(matches!(
        events.0.last().map(|event| &event.event),
        Some(PublicRunEventKind::RunFailed { .. })
    ));
    let reopened =
        Session::load_from_store("deepseek", "model", JsonlSessionStore::new(&store_path)?)?;
    assert_eq!(
        reopened
            .task_state_projection()
            .tasks
            .get(&task_id)
            .expect("interrupted Task")
            .status,
        TaskRunStatus::Interrupted
    );
    Ok(())
}

#[tokio::test]
async fn cancellation_control_persists_request_then_terminal_after_quiescence() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let store_path = temp.path().join("session.jsonl");
    let store = JsonlSessionStore::new(&store_path)?;
    let session = Session::load_from_store("deepseek", "model", store)?;
    let recorder = session.run_cancellation_recorder()?;
    let owner = RunCancellationOwner::new();
    let handle = owner.handle();
    let root_task_guard = handle.register_task()?;
    let control = ApplicationRunControl {
        owner,
        recorder,
        cancellation_target: RunCancellationTarget::Run,
        conversation_lifecycle: session.conversation_run_lifecycle_recorder()?,
        conversation_start: ConversationRunStartedEntryV1::new("run-1", 1)?,
        events: ApplicationRunEventSequence::new(
            session.session_scope_id().to_owned(),
            "run-1".to_owned(),
        ),
        _session_lease: Arc::new(
            ApplicationSessionLeaseManager::new().acquire(&temp.path().join("session.jsonl"))?,
        ),
    };
    let unblocked = Arc::new(AtomicBool::new(false));
    let signal = Arc::clone(&unblocked);
    #[derive(Default)]
    struct Recorder(Vec<PublicRunEvent>);

    impl ApplicationRunEventHandler for Recorder {
        fn handle_public_event(&mut self, event: PublicRunEvent) -> Result<()> {
            self.0.push(event);
            Ok(())
        }
    }
    let mut events = Recorder::default();

    let ticket = control.request_cancellation("test cancel", None, move || {
        signal.store(true, Ordering::SeqCst);
    })?;
    assert!(unblocked.load(Ordering::SeqCst));
    assert!(control.handle().is_cancel_requested());
    drop(root_task_guard);

    let outcome = control
        .finalize_cancellation(ticket, true, &mut events)
        .await?;
    assert_eq!(outcome, RunCancellationTerminalOutcome::Cancelled);
    assert!(matches!(
        events.0.last().map(|event| &event.event),
        Some(PublicRunEventKind::RunCancelled)
    ));
    let lifecycle = application_conversation_lifecycle(&store_path)?;
    assert!(matches!(
        lifecycle.as_slice(),
        [
            ConversationRunLifecycleRecordV1::ConversationRunStartedV1(_),
            ConversationRunLifecycleRecordV1::ConversationRunFinalizedV1(finalized),
        ] if finalized.status() == ConversationRunTerminalStatusV1::Cancelled
    ));
    Ok(())
}

#[tokio::test]
async fn cancellation_control_closes_only_the_task_bound_to_its_scope() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let store_path = temp.path().join("session.jsonl");
    let store = JsonlSessionStore::new(&store_path)?;
    let mut session = Session::load_from_store("deepseek", "model", store)?;
    let recorder = session.run_cancellation_recorder()?;
    let owner = RunCancellationOwner::new();
    let handle = owner.handle();
    let root_task_guard = handle.register_task()?;
    let task_id = TaskId::new("task-application-cancel")?;
    append_running_application_task(&mut session, &task_id, Some(handle.scope_id()), 1)?;
    let control = ApplicationRunControl {
        owner,
        recorder,
        cancellation_target: RunCancellationTarget::Run,
        conversation_lifecycle: session.conversation_run_lifecycle_recorder()?,
        conversation_start: ConversationRunStartedEntryV1::new("run-task-cancel", 1)?,
        events: ApplicationRunEventSequence::new(
            session.session_scope_id().to_owned(),
            "run-task-cancel".to_owned(),
        ),
        _session_lease: Arc::new(ApplicationSessionLeaseManager::new().acquire(&store_path)?),
    };
    let ticket = control.request_cancellation("cancel exact application Task", None, || {})?;
    drop(root_task_guard);
    #[derive(Default)]
    struct Recorder(Vec<PublicRunEvent>);

    impl ApplicationRunEventHandler for Recorder {
        fn handle_public_event(&mut self, event: PublicRunEvent) -> Result<()> {
            self.0.push(event);
            Ok(())
        }
    }
    let mut events = Recorder::default();

    let outcome = control
        .finalize_cancellation(ticket, true, &mut events)
        .await?;

    assert_eq!(outcome, RunCancellationTerminalOutcome::Cancelled);
    assert!(events.0.iter().any(|event| {
        matches!(
            &event.event,
            PublicRunEventKind::TaskRunFinished { task_id, status }
                if task_id == "task-application-cancel" && status == "cancelled"
        )
    }));
    let reopened =
        Session::load_from_store("deepseek", "model", JsonlSessionStore::new(&store_path)?)?;
    assert_eq!(
        reopened
            .task_state_projection()
            .tasks
            .get(&task_id)
            .expect("cancelled Task")
            .status,
        TaskRunStatus::Cancelled
    );
    Ok(())
}

#[tokio::test]
async fn cancellation_control_does_not_guess_an_unbound_latest_task() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let store_path = temp.path().join("session.jsonl");
    let store = JsonlSessionStore::new(&store_path)?;
    let mut session = Session::load_from_store("deepseek", "model", store)?;
    let recorder = session.run_cancellation_recorder()?;
    let owner = RunCancellationOwner::new();
    let handle = owner.handle();
    let root_task_guard = handle.register_task()?;
    let task_id = TaskId::new("task-unrelated-to-chat-cancel")?;
    append_running_application_task(&mut session, &task_id, None, 1)?;
    let control = ApplicationRunControl {
        owner,
        recorder,
        cancellation_target: RunCancellationTarget::Run,
        conversation_lifecycle: session.conversation_run_lifecycle_recorder()?,
        conversation_start: ConversationRunStartedEntryV1::new("run-chat-cancel", 1)?,
        events: ApplicationRunEventSequence::new(
            session.session_scope_id().to_owned(),
            "run-chat-cancel".to_owned(),
        ),
        _session_lease: Arc::new(ApplicationSessionLeaseManager::new().acquire(&store_path)?),
    };
    let ticket = control.request_cancellation("cancel ordinary chat", None, || {})?;
    drop(root_task_guard);
    #[derive(Default)]
    struct Recorder(Vec<PublicRunEvent>);

    impl ApplicationRunEventHandler for Recorder {
        fn handle_public_event(&mut self, event: PublicRunEvent) -> Result<()> {
            self.0.push(event);
            Ok(())
        }
    }
    let mut events = Recorder::default();

    let outcome = control
        .finalize_cancellation(ticket, true, &mut events)
        .await?;

    assert_eq!(outcome, RunCancellationTerminalOutcome::Cancelled);
    assert!(
        events
            .0
            .iter()
            .all(|event| !matches!(event.event, PublicRunEventKind::TaskRunFinished { .. })),
        "ordinary chat cancellation must not synthesize a Task terminal"
    );
    let reopened =
        Session::load_from_store("deepseek", "model", JsonlSessionStore::new(&store_path)?)?;
    assert_eq!(
        reopened
            .task_state_projection()
            .tasks
            .get(&task_id)
            .expect("unrelated Task remains visible")
            .status,
        TaskRunStatus::Running
    );
    Ok(())
}

#[tokio::test]
async fn cancellation_without_execution_join_persists_interrupted_and_failed_event() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let store_path = temp.path().join("session.jsonl");
    let store = JsonlSessionStore::new(&store_path)?;
    let session = Session::load_from_store("deepseek", "model", store)?;
    let recorder = session.run_cancellation_recorder()?;
    let owner = RunCancellationOwner::new();
    let handle = owner.handle();
    let root_task_guard = handle.register_task()?;
    let control = ApplicationRunControl {
        owner,
        recorder,
        cancellation_target: RunCancellationTarget::Run,
        conversation_lifecycle: session.conversation_run_lifecycle_recorder()?,
        conversation_start: ConversationRunStartedEntryV1::new("run-1", 1)?,
        events: ApplicationRunEventSequence::new(
            session.session_scope_id().to_owned(),
            "run-1".to_owned(),
        ),
        _session_lease: Arc::new(ApplicationSessionLeaseManager::new().acquire(&store_path)?),
    };
    let ticket = control.request_cancellation(
        "test interrupted terminal",
        Some(std::time::Duration::from_millis(10)),
        || {},
    )?;
    drop(root_task_guard);
    #[derive(Default)]
    struct Recorder(Vec<PublicRunEvent>);

    impl ApplicationRunEventHandler for Recorder {
        fn handle_public_event(&mut self, event: PublicRunEvent) -> Result<()> {
            self.0.push(event);
            Ok(())
        }
    }
    let mut events = Recorder::default();

    let outcome = control
        .finalize_cancellation(ticket, false, &mut events)
        .await?;

    assert_eq!(outcome, RunCancellationTerminalOutcome::Interrupted);
    assert!(matches!(
        events.0.last().map(|event| &event.event),
        Some(PublicRunEventKind::RunFailed { .. })
    ));
    let lifecycle = application_conversation_lifecycle(&store_path)?;
    assert!(matches!(
        lifecycle.as_slice(),
        [
            ConversationRunLifecycleRecordV1::ConversationRunStartedV1(_),
            ConversationRunLifecycleRecordV1::ConversationRunFinalizedV1(finalized),
        ] if finalized.status() == ConversationRunTerminalStatusV1::Interrupted
    ));
    let durable = std::fs::read_to_string(store_path)?;
    assert!(durable.contains("\"outcome\":\"interrupted\""));
    Ok(())
}

#[tokio::test]
async fn cancellation_audit_failure_still_unblocks_and_requires_failed_terminal() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let store_path = temp.path().join("session.jsonl");
    let store = JsonlSessionStore::new(&store_path)?;
    let session = Session::load_from_store("deepseek", "model", store)?;
    let recorder = session.run_cancellation_recorder()?;
    let owner = RunCancellationOwner::new();
    let handle = owner.handle();
    let root_task_guard = handle.register_task()?;
    let control = ApplicationRunControl {
        owner,
        recorder,
        cancellation_target: RunCancellationTarget::Run,
        conversation_lifecycle: session.conversation_run_lifecycle_recorder()?,
        conversation_start: ConversationRunStartedEntryV1::new("run-1", 1)?,
        events: ApplicationRunEventSequence::new(
            session.session_scope_id().to_owned(),
            "run-1".to_owned(),
        ),
        _session_lease: Arc::new(ApplicationSessionLeaseManager::new().acquire(&store_path)?),
    };
    temp.close()?;
    let unblocked = Arc::new(AtomicBool::new(false));
    let signal = Arc::clone(&unblocked);

    let error = control
        .request_cancellation("test audit failure", None, move || {
            signal.store(true, Ordering::SeqCst);
        })
        .expect_err("removed session parent must reject the durable append");
    assert!(unblocked.load(Ordering::SeqCst));
    assert!(control.handle().is_cancel_requested());
    let ticket = error
        .into_ticket()
        .expect("activated cancellation must return a cleanup ticket");
    drop(root_task_guard);

    #[derive(Default)]
    struct Recorder(Vec<PublicRunEvent>);

    impl ApplicationRunEventHandler for Recorder {
        fn handle_public_event(&mut self, event: PublicRunEvent) -> Result<()> {
            self.0.push(event);
            Ok(())
        }
    }
    let mut events = Recorder::default();
    assert!(
        control
            .finalize_cancellation(ticket, true, &mut events)
            .await
            .is_err()
    );
    assert!(matches!(
        events.0.last().map(|event| &event.event),
        Some(PublicRunEventKind::RunFailed { .. })
    ));
    Ok(())
}
